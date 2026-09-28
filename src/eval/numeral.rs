//! `Numeral`: what a custom `from_numeral` receives.
//!
//! `Builtin.roc` declares it as `Literal({ is_negative, digits_before_pt,
//! digits_after_pt, digits_after_pt_count })` with the digits in base 256, so a custom
//! number type can read a literal of any length exactly. One is built from a literal's
//! text where the compiler still has it, and from the value where it does not — a
//! literal that reached a generic body is an ordinary number by then.

use super::{call_function, str_value, EvalError, Value, DEC_SCALE};

/// `-?digits(.digits)?` with underscores allowed; anything else (hex, an exponent) is
/// built from its value instead.
pub fn numeral_from_text(text: &str) -> Option<Value> {
    let clean: String = text.chars().filter(|c| *c != '_').collect();
    let (negative, body) = match clean.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, clean.as_str()),
    };
    let (whole, fraction) = body.split_once('.').unwrap_or((body, ""));
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    if !digits(whole) || !(fraction.is_empty() || digits(fraction)) {
        return None;
    }
    Some(numeral(negative, whole, fraction))
}

pub fn numeral_from_value(value: &Value) -> Option<Value> {
    let text = match value {
        Value::Int(n) => n.to_string(),
        Value::Dec(attos) => {
            let magnitude = attos.unsigned_abs();
            let fraction = format!("{:018}", magnitude % DEC_SCALE as u128);
            let fraction = fraction.trim_end_matches('0');
            let sign = if *attos < 0 { "-" } else { "" };
            if fraction.is_empty() {
                format!("{}{}", sign, magnitude / DEC_SCALE as u128)
            } else {
                format!("{}{}.{}", sign, magnitude / DEC_SCALE as u128, fraction)
            }
        }
        Value::Float(f) if f.is_finite() => format!("{}", f),
        Value::F32(f) if f.is_finite() => format!("{}", f),
        _ => return None,
    };
    numeral_from_text(&text)
}

fn numeral(negative: bool, whole: &str, fraction: &str) -> Value {
    let bytes = |digits: &str| {
        Value::list(base256(digits).into_iter().map(|b| Value::Int(i128::from(b))).collect())
    };
    Value::tag(
        "Literal",
        vec![Value::record(vec![
            ("is_negative", Value::Bool(negative)),
            ("digits_before_pt", bytes(whole)),
            ("digits_after_pt", bytes(fraction)),
            ("digits_after_pt_count", Value::Int(fraction.len() as i128)),
        ])],
    )
}

/// A decimal digit string as big-endian base-256 digits; zero is no digits at all.
fn base256(decimal: &str) -> Vec<u8> {
    let mut digits: Vec<u8> = decimal.bytes().map(|b| b - b'0').skip_while(|d| *d == 0).collect();
    let mut out = Vec::new();
    while !digits.is_empty() {
        let mut rem = 0u32;
        let mut next = Vec::with_capacity(digits.len());
        for d in digits {
            let cur = rem * 10 + u32::from(d);
            let q = cur / 256;
            rem = cur % 256;
            if !(next.is_empty() && q == 0) {
                next.push(q as u8);
            }
        }
        out.push(rem as u8);
        digits = next;
    }
    out.reverse();
    out
}

/// Big-endian base-256 digits as a decimal string; no digits is `0`.
fn decimal(bytes: &[Value]) -> String {
    let mut digits: Vec<u8> = vec![0];
    for byte in bytes {
        let Value::Int(b) = byte else { continue };
        let mut carry = (*b as u32) & 0xff;
        for d in digits.iter_mut().rev() {
            let cur = u32::from(*d) * 256 + carry;
            *d = (cur % 10) as u8;
            carry = cur / 10;
        }
        while carry > 0 {
            digits.insert(0, (carry % 10) as u8);
            carry /= 10;
        }
    }
    digits.into_iter().map(|d| char::from(b'0' + d)).collect()
}

fn field<'a>(numeral: &'a Value, name: &str) -> Option<&'a Value> {
    let Value::Tag("Literal", payload) = numeral else { return None };
    let Value::Record(fields) = payload.first()? else { return None };
    fields.iter().find(|(n, _)| *n == name).map(|(_, v)| v)
}

/// `Numeral.is_negative(n)`, `Numeral.digits_before_pt(n)`, ….
pub fn call_numeral(method: &str, args: &[Value]) -> Result<Value, EvalError> {
    args.first()
        .and_then(|n| field(n, method))
        .cloned()
        .ok_or_else(|| EvalError { message: format!("Numeral.{} needs a Numeral", method) })
}

/// The literal's text back, so a width's `from_str` can read it.
pub fn numeral_text(numeral: &Value) -> Option<String> {
    let negative = matches!(field(numeral, "is_negative")?, Value::Bool(true));
    let whole = decimal(field(numeral, "digits_before_pt")?.sequence()?);
    let count = match field(numeral, "digits_after_pt_count")? {
        Value::Int(n) => *n as usize,
        _ => 0,
    };
    let mut text = String::new();
    if negative {
        text.push('-');
    }
    text.push_str(&whole);
    if count > 0 {
        let fraction = decimal(field(numeral, "digits_after_pt")?.sequence()?);
        text.push('.');
        text.push_str(&"0".repeat(count.saturating_sub(fraction.len())));
        text.push_str(&fraction);
    }
    Some(text)
}

/// A value where a nominal with a literal conversion was declared: a literal that
/// arrived raw — through a generic body, which roc would have specialised — is
/// converted now, and anything else is already what it should be.
///
/// The three callables are the nominal's `from_quote`, `from_numeral` and
/// `from_interpolation`, or `{}` where it has none.
pub fn coerce(args: &mut [Value]) -> Result<Value, EvalError> {
    let mut args = args.into_iter();
    let (Some(value), Some(quote), Some(numeral), Some(interp)) =
        (args.next(), args.next(), args.next(), args.next())
    else {
        return Err(EvalError { message: "Lit.coerce takes a value and three converters".to_string() });
    };
    coerce_value(std::mem::replace(value, Value::Unit), &quote, &numeral, &interp)
}

fn callable(f: &Value) -> bool {
    matches!(f, Value::Closure(_) | Value::Builtin(..))
}

fn unwrap_ok(converted: Value) -> Result<Value, EvalError> {
    match converted {
        Value::Tag("Ok", payload) if payload.len() == 1 => Ok(payload[0].clone()),
        other => Err(EvalError { message: format!("No match arm matched {}", other) }),
    }
}

fn coerce_value(value: Value, quote: &Value, numeral: &Value, interp: &Value) -> Result<Value, EvalError> {
    match value {
        Value::Str(_) if callable(quote) => unwrap_ok(call_function(quote.clone(), vec![value])?),
        Value::Str(_) if callable(interp) => {
            call_function(interp.clone(), vec![value, Value::list(Vec::new())])
        }
        Value::Int(_) | Value::Float(_) | Value::F32(_) | Value::Dec(_) if callable(numeral) => {
            let literal = numeral_from_value(&value).ok_or_else(|| EvalError {
                message: format!("{} cannot be read as a numeral", value),
            })?;
            unwrap_ok(call_function(numeral.clone(), vec![literal])?)
        }
        // A Roc list is homogeneous, so its first element says whether any element is a
        // literal a converter applies to. When none is, the list is handed back as it
        // came: rebuilding it anyway copied the whole list at every coerced boundary,
        // and a nominal over `List(U8)` that only declares `from_quote` paid that on
        // every write (B-Teague/rocflight#6).
        Value::List(items) if items.first().is_some_and(|first| converts(first, quote, numeral, interp)) => {
            let converted: Result<Vec<Value>, EvalError> = items
                .iter()
                .cloned()
                .map(|v| coerce_value(v, quote, numeral, interp))
                .collect();
            Ok(Value::list(converted?))
        }
        Value::Tag("Ok", payload) if payload.len() == 1 => {
            let inner = coerce_value(payload[0].clone(), quote, numeral, interp)?;
            Ok(Value::tag("Ok", [inner]))
        }
        other => Ok(other),
    }
}

/// Does `coerce_value` change this scalar? A string when a quote or interpolation
/// converter exists, a number when `from_numeral` does; nothing else.
fn converts(value: &Value, quote: &Value, numeral: &Value, interp: &Value) -> bool {
    match value {
        Value::Str(_) => callable(quote) || callable(interp),
        Value::Int(_) | Value::Float(_) | Value::F32(_) | Value::Dec(_) => callable(numeral),
        _ => false,
    }
}

/// `Str.from_interpolation(first, rest)`: the segments back together.
pub fn from_interpolation(args: &[Value]) -> Option<Value> {
    let Value::Str(first) = args.first()? else { return None };
    let mut text = first.to_string();
    for pair in args.get(1)?.sequence()? {
        if let Value::Tuple(items) = pair {
            for item in items.iter() {
                if let Value::Str(s) = item {
                    text.push_str(s);
                }
            }
        }
    }
    Some(str_value(text))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// B-Teague/rocflight#6. A `List(U8)` behind a nominal that declares only
    /// `from_quote` has nothing a converter applies to, so the list must come back as
    /// the SAME allocation: a copy here, on every coerced call boundary, made each
    /// write through the nominal cost the whole list.
    #[test]
    fn a_list_nothing_converts_is_returned_as_is() {
        let bytes = Value::list((0..1024).map(|b| Value::Int(b % 256)).collect());
        let Value::List(before) = &bytes else { unreachable!() };
        let before = std::rc::Rc::clone(before);
        let from_quote = Value::Builtin("Str.to_utf8", 1);
        let mut args = [bytes, from_quote, Value::Unit, Value::Unit];
        let Value::List(after) = coerce(&mut args).unwrap() else { panic!("not a list") };
        assert!(std::rc::Rc::ptr_eq(&before, &after), "the list was copied");
    }
}
