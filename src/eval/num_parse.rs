//! Reading a number out of text, as roc 2026-09-27 reads it.
//!
//! One grammar serves both `T.from_str` and the prefix parsers
//! `T.from_str_prefix`/`T.from_utf8_prefix`, the way roc's own builtins do it
//! (`num.zig`, `decimal_parse.zig`, `dec.zig`): scan the LONGEST token of the type's
//! grammar at the start of the input, then read that token. `from_str` is the same
//! scan with the extra demand that the token is the whole input.
//!
//! The scan depends only on syntax, never on range, and never backtracks: `"300,"` as
//! a `U8` is a three-byte token that does not fit, so `OutOfRange`, even though `"30"`
//! would have. No whitespace is skipped anywhere.
//!
//! The grammars:
//! - integer: `sign? 0x|0o|0b` then digits of that radix, or `sign? D (_? D)*` with an
//!   optional `e sign? D (_? D)*` exponent. A radix prefix with no digit of its radix
//!   is not a radix token, so `"0x"` is the decimal token `"0"`.
//! - Dec: `sign? (D+ | D+ . D* | . D+)` with the same optional exponent.
//! - float: the Dec mantissa (or a hex one, `0x` with a `p` exponent) with
//!   `_` between digits, or `inf`, `infinity`, `nan` in any case.

use super::Value;

/// The numeric type being read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Int { signed: bool, bits: u32 },
    F32,
    F64,
    Dec,
}

impl Kind {
    /// The type a module name (`U8`, `Dec`, ...) or a low-level prefix (`u8`, `dec`)
    /// names, in either case.
    pub fn named(name: &str) -> Option<Kind> {
        // Without allocating: this is asked on the call path.
        Some(match name {
            "U8" | "u8" => Kind::Int { signed: false, bits: 8 },
            "U16" | "u16" => Kind::Int { signed: false, bits: 16 },
            "U32" | "u32" => Kind::Int { signed: false, bits: 32 },
            "U64" | "u64" => Kind::Int { signed: false, bits: 64 },
            "U128" | "u128" => Kind::Int { signed: false, bits: 128 },
            "I8" | "i8" => Kind::Int { signed: true, bits: 8 },
            "I16" | "i16" => Kind::Int { signed: true, bits: 16 },
            "I32" | "i32" => Kind::Int { signed: true, bits: 32 },
            "I64" | "i64" => Kind::Int { signed: true, bits: 64 },
            "I128" | "i128" => Kind::Int { signed: true, bits: 128 },
            "F32" | "f32" => Kind::F32,
            "F64" | "f64" => Kind::F64,
            "Dec" | "dec" => Kind::Dec,
            _ => return None,
        })
    }

    /// The type's zero, which is what roc's prefix parsers put in `value` on failure.
    pub fn zero(self) -> Value {
        match self {
            Kind::Int { signed: false, bits: 128 } => Value::U128(0),
            Kind::Int { .. } => Value::Int(0),
            Kind::F32 => Value::F32(0.0),
            Kind::F64 => Value::Float(0.0),
            Kind::Dec => Value::Dec(0),
        }
    }
}

/// `Some` when the whole of `bytes` is one token of the type and the token denotes a
/// value of it: `T.from_str`.
pub fn parse_whole(kind: Kind, bytes: &[u8]) -> Option<Value> {
    let (consumed, value) = parse_prefix(kind, bytes);
    if consumed == 0 || consumed != bytes.len() {
        return None;
    }
    value
}

/// The length of the longest token at the start of `bytes`, and its value when it
/// denotes one. `(0, None)` is "no number here"; `(n, None)` with `n > 0` is a token
/// that is out of the type's range (or, for `Dec`, more precise than it).
pub fn parse_prefix(kind: Kind, bytes: &[u8]) -> (usize, Option<Value>) {
    let consumed = match kind {
        Kind::Int { .. } => int_prefix_len(bytes),
        Kind::F32 | Kind::F64 => float_prefix_len(bytes),
        Kind::Dec => scan_decimal(bytes, Grammar::Dec).map_or(0, |p| p.token_len),
    };
    if consumed == 0 {
        return (0, None);
    }
    let token = &bytes[..consumed];
    let value = match kind {
        Kind::Int { signed, bits } => parse_int_token(signed, bits, token).map(|n| match n {
            IntValue::Signed(n) => Value::Int(n),
            IntValue::Unsigned(n) if bits == 128 => Value::U128(n),
            IntValue::Unsigned(n) => Value::Int(n as i128),
        }),
        Kind::F32 => parse_float_token(token, true).map(|x| Value::F32(x as f32)),
        Kind::F64 => parse_float_token(token, false).map(Value::Float),
        Kind::Dec => parse_dec_token(token).map(Value::Dec),
    };
    (consumed, value)
}

// ── Integers ──

enum IntValue {
    Signed(i128),
    Unsigned(u128),
}

fn is_digit(byte: u8) -> bool {
    byte.is_ascii_digit()
}

fn digit_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'z' => Some(byte - b'a' + 10),
        b'A'..=b'Z' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn is_radix_digit(byte: u8, radix: u8) -> bool {
    digit_value(byte).is_some_and(|d| d < radix)
}

fn has_sign(bytes: &[u8]) -> bool {
    matches!(bytes.first(), Some(b'-' | b'+'))
}

fn has_explicit_radix(bytes: &[u8]) -> bool {
    let start = usize::from(has_sign(bytes));
    bytes.len() >= start + 2
        && bytes[start] == b'0'
        && matches!(bytes[start + 1], b'b' | b'B' | b'o' | b'O' | b'x' | b'X')
}

fn radix_of(marker: u8) -> u8 {
    match marker {
        b'b' | b'B' => 2,
        b'o' | b'O' => 8,
        _ => 16,
    }
}

fn int_prefix_len(bytes: &[u8]) -> usize {
    let radix_len = radix_int_prefix_len(bytes);
    if radix_len != 0 {
        return radix_len;
    }
    scan_decimal(bytes, Grammar::Int).map_or(0, |p| p.token_len)
}

fn radix_int_prefix_len(bytes: &[u8]) -> usize {
    if !has_explicit_radix(bytes) {
        return 0;
    }
    let digits_start = usize::from(has_sign(bytes)) + 2;
    let radix = radix_of(bytes[digits_start - 1]);
    let mut end = digits_start;
    let mut index = digits_start;
    while index < bytes.len() {
        let byte = bytes[index];
        if byte == b'_' {
            if index == digits_start || index + 1 == bytes.len() || !is_radix_digit(bytes[index + 1], radix) {
                break;
            }
            index += 1;
            continue;
        }
        if !is_radix_digit(byte, radix) {
            break;
        }
        end = index + 1;
        index += 1;
    }
    if end == digits_start {
        0
    } else {
        end
    }
}

/// The largest magnitude of the type: its maximum, or for a negative signed value
/// one more than that.
fn positive_limit(signed: bool, bits: u32) -> u128 {
    match (signed, bits) {
        (false, 128) => u128::MAX,
        (false, bits) => (1u128 << bits) - 1,
        (true, bits) => (1u128 << (bits - 1)) - 1,
    }
}

fn signed_result(signed: bool, bits: u32, negative: bool, magnitude: u128) -> Option<IntValue> {
    if !signed {
        return (!negative || magnitude == 0).then_some(IntValue::Unsigned(magnitude));
    }
    let limit = positive_limit(true, bits);
    if !negative {
        return (magnitude <= limit).then(|| IntValue::Signed(magnitude as i128));
    }
    if magnitude == limit + 1 {
        // The type's minimum. For I128 that is `i128::MIN`, which has no positive twin.
        return Some(IntValue::Signed(if bits == 128 { i128::MIN } else { -(magnitude as i128) }));
    }
    (magnitude <= limit).then(|| IntValue::Signed(-(magnitude as i128)))
}

fn parse_int_token(signed: bool, bits: u32, token: &[u8]) -> Option<IntValue> {
    if has_explicit_radix(token) {
        return parse_radix_int(signed, bits, token);
    }
    let parsed = scan_whole(token, Grammar::Int)?;
    let limit = if signed && parsed.negative { positive_limit(true, bits) + 1 } else { positive_limit(signed, bits) };
    let magnitude = match positive_exponent(&parsed) {
        Some(zeros) => {
            if parsed.coefficient_overflow {
                return None;
            }
            append_decimal_zeros(limit, parsed.coefficient, zeros)?
        }
        None if !parsed.exponent_negative && parsed.is_zero() => 0,
        None => return None,
    };
    signed_result(signed, bits, parsed.negative, magnitude)
}

fn parse_radix_int(signed: bool, bits: u32, token: &[u8]) -> Option<IntValue> {
    let negative = token[0] == b'-';
    let mut index = usize::from(has_sign(token));
    let radix = radix_of(token[index + 1]);
    index += 2;
    let limit = if signed && negative { positive_limit(true, bits) + 1 } else { positive_limit(signed, bits) };
    let (max_before_mul, max_digit) = (limit / u128::from(radix), limit % u128::from(radix));
    let mut value: u128 = 0;
    let mut saw_digit = false;
    let mut previous_underscore = false;
    for &byte in &token[index..] {
        if byte == b'_' {
            if !saw_digit || previous_underscore {
                return None;
            }
            previous_underscore = true;
            continue;
        }
        let digit = u128::from(digit_value(byte).filter(|d| *d < radix)?);
        if value > max_before_mul || (value == max_before_mul && digit > max_digit) {
            return None;
        }
        value = value * u128::from(radix) + digit;
        saw_digit = true;
        previous_underscore = false;
    }
    if !saw_digit || previous_underscore {
        return None;
    }
    signed_result(signed, bits, negative, value)
}

// ── Decimal tokens: integers without a radix, and Dec ──

#[derive(Clone, Copy, PartialEq, Eq)]
enum Grammar {
    Int,
    Dec,
}

/// What a decimal token says, exactly: `decimal_parse.zig`'s `ParsedDecimal`.
struct ParsedDecimal {
    negative: bool,
    mantissa_end: usize,
    token_len: usize,
    coefficient_digits: usize,
    fractional_digits: usize,
    leading_zero_digits: usize,
    trailing_zero_digits: usize,
    coefficient: u128,
    coefficient_overflow: bool,
    exponent_negative: bool,
    exponent_magnitude: u64,
    exponent_overflow: bool,
}

impl ParsedDecimal {
    fn is_zero(&self) -> bool {
        self.leading_zero_digits == self.coefficient_digits
    }
}

/// The longest token of `grammar` at the start of `bytes`. It never ends on a
/// dangling sign, `_`, `e` or exponent sign.
fn scan_decimal(bytes: &[u8], grammar: Grammar) -> Option<ParsedDecimal> {
    let mut index = 0;
    let negative = bytes.first() == Some(&b'-');
    if has_sign(bytes) {
        index += 1;
    }
    let mut had_decimal_point = false;
    let mut saw_digit = false;
    let (mut coefficient_digits, mut fractional_digits) = (0usize, 0usize);
    let (mut leading_zero_digits, mut trailing_zero_digits) = (0usize, 0usize);
    let mut saw_nonzero = false;
    let mut coefficient: u128 = 0;
    let mut coefficient_overflow = false;
    while index < bytes.len() {
        let byte = bytes[index];
        match byte {
            b'0'..=b'9' => {
                let digit = byte - b'0';
                saw_digit = true;
                coefficient_digits += 1;
                if had_decimal_point {
                    fractional_digits += 1;
                }
                if !saw_nonzero && digit == 0 {
                    leading_zero_digits += 1;
                } else {
                    saw_nonzero = true;
                }
                trailing_zero_digits = if digit == 0 { trailing_zero_digits + 1 } else { 0 };
                if !coefficient_overflow {
                    match coefficient.checked_mul(10).and_then(|c| c.checked_add(u128::from(digit))) {
                        Some(c) => coefficient = c,
                        None => coefficient_overflow = true,
                    }
                }
            }
            b'_' => {
                if index == 0 || !is_digit(bytes[index - 1]) || index + 1 == bytes.len() || !is_digit(bytes[index + 1]) {
                    break;
                }
            }
            b'.' => {
                if grammar == Grammar::Int || had_decimal_point {
                    break;
                }
                had_decimal_point = true;
            }
            _ => break,
        }
        index += 1;
    }
    if !saw_digit {
        return None;
    }
    let mantissa_end = index;

    let (mut exponent_negative, mut exponent_magnitude, mut exponent_overflow) = (false, 0u64, false);
    if index < bytes.len() && (bytes[index] == b'e' || bytes[index] == b'E') {
        let mut cursor = index + 1;
        let mut candidate_negative = false;
        if cursor < bytes.len() && (bytes[cursor] == b'+' || bytes[cursor] == b'-') {
            candidate_negative = bytes[cursor] == b'-';
            cursor += 1;
        }
        if cursor < bytes.len() && is_digit(bytes[cursor]) {
            let mut magnitude: u64 = 0;
            let mut overflow = false;
            while cursor < bytes.len() {
                let byte = bytes[cursor];
                if is_digit(byte) {
                    if !overflow {
                        match magnitude.checked_mul(10).and_then(|m| m.checked_add(u64::from(byte - b'0'))) {
                            Some(m) => magnitude = m,
                            None => overflow = true,
                        }
                    }
                } else if byte == b'_' && is_digit(bytes[cursor - 1]) && cursor + 1 < bytes.len() && is_digit(bytes[cursor + 1]) {
                } else {
                    break;
                }
                cursor += 1;
            }
            exponent_negative = candidate_negative;
            exponent_magnitude = magnitude;
            exponent_overflow = overflow;
            index = cursor;
        }
    }
    Some(ParsedDecimal {
        negative,
        mantissa_end,
        token_len: index,
        coefficient_digits,
        fractional_digits,
        leading_zero_digits,
        trailing_zero_digits,
        coefficient,
        coefficient_overflow,
        exponent_negative,
        exponent_magnitude,
        exponent_overflow,
    })
}

fn scan_whole(bytes: &[u8], grammar: Grammar) -> Option<ParsedDecimal> {
    scan_decimal(bytes, grammar).filter(|p| p.token_len == bytes.len())
}

/// `initial * 10^count`, or `None` past `limit`.
fn append_decimal_zeros(limit: u128, initial: u128, count: usize) -> Option<u128> {
    if initial > limit {
        return None;
    }
    let mut value = initial;
    for _ in 0..count {
        if value > limit / 10 {
            return None;
        }
        value *= 10;
    }
    Some(value)
}

/// A non-negative exponent small enough to apply; `1e-0` counts.
fn positive_exponent(parsed: &ParsedDecimal) -> Option<usize> {
    if parsed.exponent_negative && (parsed.exponent_overflow || parsed.exponent_magnitude != 0) {
        return None;
    }
    if parsed.exponent_overflow || parsed.exponent_magnitude > 38 {
        return None;
    }
    Some(parsed.exponent_magnitude as usize)
}

/// The first `keep_digits` digits of the mantissa, as a number: the coefficient with
/// its trailing zeros dropped.
fn parse_coefficient_prefix(limit: u128, bytes: &[u8], parsed: &ParsedDecimal, keep_digits: usize) -> Option<u128> {
    let mut consumed = 0;
    let mut value: u128 = 0;
    for &byte in &bytes[usize::from(has_sign(bytes))..parsed.mantissa_end] {
        if consumed == keep_digits {
            break;
        }
        if !is_digit(byte) {
            continue;
        }
        let digit = u128::from(byte - b'0');
        if value > limit / 10 || (value == limit / 10 && digit > limit % 10) {
            return None;
        }
        value = value * 10 + digit;
        consumed += 1;
    }
    Some(value)
}

/// The token's magnitude times `10^places`, when that is a whole number within `limit`.
fn scaled_magnitude(limit: u128, bytes: &[u8], parsed: &ParsedDecimal, places: i128) -> Option<u128> {
    if parsed.is_zero() {
        return Some(0);
    }
    if parsed.exponent_overflow {
        return None;
    }
    let exponent = if parsed.exponent_negative {
        -i128::from(parsed.exponent_magnitude)
    } else {
        i128::from(parsed.exponent_magnitude)
    };
    let scale = exponent - parsed.fractional_digits as i128 + places;
    if scale >= 0 {
        if scale > 38 || parsed.coefficient_overflow {
            return None;
        }
        return append_decimal_zeros(limit, parsed.coefficient, scale as usize);
    }
    let drop = -scale;
    if drop > parsed.trailing_zero_digits as i128 {
        return None;
    }
    parse_coefficient_prefix(limit, bytes, parsed, parsed.coefficient_digits - drop as usize)
}

/// A Dec token, exactly: `None` when it is out of range or needs more than Dec's
/// eighteen places.
fn parse_dec_token(token: &[u8]) -> Option<i128> {
    let parsed = scan_whole(token, Grammar::Dec)?;
    let limit = if parsed.negative { i128::MAX as u128 + 1 } else { i128::MAX as u128 };
    let magnitude = scaled_magnitude(limit, token, &parsed, 18)?;
    Some(if !parsed.negative {
        magnitude as i128
    } else if magnitude == i128::MAX as u128 + 1 {
        i128::MIN
    } else {
        -(magnitude as i128)
    })
}

// ── Floats ──

fn float_prefix_len(bytes: &[u8]) -> usize {
    let start = usize::from(has_sign(bytes));
    let body = &bytes[start..];
    if body.len() >= 2 && body[0] == b'0' && (body[1] == b'x' || body[1] == b'X') {
        let hex_len = mantissa_exponent_prefix_len(&body[2..], 16, b'p');
        if hex_len != 0 {
            return start + 2 + hex_len;
        }
    }
    let decimal_len = mantissa_exponent_prefix_len(body, 10, b'e');
    if decimal_len != 0 {
        return start + decimal_len;
    }
    for word in ["infinity", "inf", "nan"] {
        if body.len() >= word.len() && body[..word.len()].eq_ignore_ascii_case(word.as_bytes()) {
            return start + word.len();
        }
    }
    0
}

fn mantissa_exponent_prefix_len(bytes: &[u8], radix: u8, exponent_char: u8) -> usize {
    let mut index = 0;
    let mut digits = 0;
    let mut had_point = false;
    while index < bytes.len() {
        let byte = bytes[index];
        if is_radix_digit(byte, radix) {
            digits += 1;
        } else if byte == b'_'
            && index > 0
            && is_radix_digit(bytes[index - 1], radix)
            && index + 1 < bytes.len()
            && is_radix_digit(bytes[index + 1], radix)
        {
        } else if byte == b'.' && !had_point {
            had_point = true;
        } else {
            break;
        }
        index += 1;
    }
    if digits == 0 {
        return 0;
    }
    if index < bytes.len() && (bytes[index] | 0x20) == exponent_char {
        let mut cursor = index + 1;
        if cursor < bytes.len() && (bytes[cursor] == b'-' || bytes[cursor] == b'+') {
            cursor += 1;
        }
        if cursor < bytes.len() && is_digit(bytes[cursor]) {
            while cursor < bytes.len() {
                let byte = bytes[cursor];
                let joins = byte == b'_' && is_digit(bytes[cursor - 1]) && cursor + 1 < bytes.len() && is_digit(bytes[cursor + 1]);
                if !is_digit(byte) && !joins {
                    break;
                }
                cursor += 1;
            }
            index = cursor;
        }
    }
    index
}

/// A float token's value, rounded to nearest (to `F32`'s precision when `narrow`).
/// An infinity only when the token spells one: `1e400` is not a number the type holds.
fn parse_float_token(token: &[u8], narrow: bool) -> Option<f64> {
    let negative = token[0] == b'-';
    let body = &token[usize::from(has_sign(token))..];
    let magnitude = if body.len() > 2 && body[0] == b'0' && (body[1] | 0x20) == b'x' {
        hex_float(&body[2..], narrow)?
    } else {
        let text: String = body.iter().filter(|b| **b != b'_').map(|b| char::from(*b)).collect();
        if narrow {
            f64::from(text.parse::<f32>().ok()?)
        } else {
            text.parse::<f64>().ok()?
        }
    };
    let explicit_infinity = body.eq_ignore_ascii_case(b"inf") || body.eq_ignore_ascii_case(b"infinity");
    if magnitude.is_infinite() && !explicit_infinity {
        return None;
    }
    Some(if negative { -magnitude } else { magnitude })
}

/// `H+ (. H*)? (p sign? D+)?`, after the `0x`, rounded half to even.
fn hex_float(bytes: &[u8], narrow: bool) -> Option<f64> {
    // Every significant bit up to 124 of them, then whether anything nonzero follows.
    let mut mantissa: u128 = 0;
    let mut exponent: i64 = 0;
    let mut sticky = false;
    let mut index = 0;
    let mut after_point = false;
    while index < bytes.len() {
        let byte = bytes[index];
        if byte == b'.' {
            after_point = true;
        } else if byte == b'_' {
        } else if let Some(d) = digit_value(byte).filter(|d| *d < 16) {
            if mantissa >> 120 == 0 {
                mantissa = (mantissa << 4) | u128::from(d);
                if after_point {
                    exponent -= 4;
                }
            } else {
                sticky |= d != 0;
                if !after_point {
                    exponent += 4;
                }
            }
        } else {
            break;
        }
        index += 1;
    }
    if index < bytes.len() && (bytes[index] | 0x20) == b'p' {
        let text: String = bytes[index + 1..].iter().filter(|b| **b != b'_').map(|b| char::from(*b)).collect();
        // A binary exponent too large for an i64 is an infinity or a zero either way.
        let e = text.parse::<i64>().unwrap_or(if text.starts_with('-') { -1 << 40 } else { 1 << 40 });
        exponent = exponent.saturating_add(e);
    }
    if mantissa == 0 {
        return Some(0.0);
    }
    let (precision, min_low_bit, max_lead) = if narrow { (24i64, -149i64, 127i64) } else { (53, -1074, 1023) };
    let bits = 128 - i64::from(mantissa.leading_zeros());
    let lead = exponent + bits - 1;
    if lead > max_lead {
        return Some(f64::INFINITY);
    }
    let low_bit = (lead - (precision - 1)).max(min_low_bit);
    let drop = low_bit - exponent;
    let kept = if drop <= 0 {
        mantissa << (-drop) as u32
    } else if drop >= 128 {
        // Everything is below the lowest representable bit: zero, or the smallest
        // subnormal when it is more than half of one.
        let half = 1u128 << 127;
        u128::from(drop == 128 && (mantissa > half || (mantissa == half && sticky)))
    } else {
        let shift = drop as u32;
        let kept = mantissa >> shift;
        let rest = mantissa & ((1u128 << shift) - 1);
        let half = 1u128 << (shift - 1);
        let round_up = rest > half || (rest == half && (sticky || kept & 1 == 1));
        kept + u128::from(round_up)
    };
    // Exact: `kept` fits the precision (or is one past it, a power of two), and the
    // scaling is by a power of two into range.
    let value = (kept as f64) * pow2(low_bit.max(-1022)) * pow2((low_bit + 1022).min(0));
    Some(value)
}

/// `2^k` for `-1022 <= k <= 1023`, exactly.
fn pow2(k: i64) -> f64 {
    f64::from_bits(((k + 1023) as u64) << 52)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prefix(kind: &str, text: &str) -> (usize, Option<String>) {
        let (n, v) = parse_prefix(Kind::named(kind).unwrap(), text.as_bytes());
        (n, v.map(|v| v.to_string()))
    }

    // The examples in roc's own `num.zig` and `dec.zig` tests, and in Builtin.roc's docs.
    #[test]
    fn integer_prefixes() {
        assert_eq!(prefix("u8", "42,rest"), (2, Some("42".into())));
        assert_eq!(prefix("u8", "300,"), (3, None));
        assert_eq!(prefix("u8", "abc"), (0, None));
        assert_eq!(prefix("i8", "-7abc"), (2, Some("-7".into())));
        assert_eq!(prefix("i8", "0b12"), (3, Some("1".into())));
        assert_eq!(prefix("u16", "-5"), (2, None));
        assert_eq!(prefix("u8", "1.2.3"), (1, Some("1".into())));
        // A negative exponent is part of an integer token, and a token that is not a
        // whole number is out of range.
        assert_eq!(prefix("u8", "2e-1"), (4, None));
        assert_eq!(prefix("u8", "1e-0x"), (4, Some("1".into())));
        assert_eq!(prefix("i128", "-170141183460469231731687303715884105728,"), (40, Some("-170141183460469231731687303715884105728".into())));
        assert_eq!(prefix("u128", "340282366920938463463374607431768211455 "), (39, Some("340282366920938463463374607431768211455".into())));
        assert_eq!(prefix("u128", "340282366920938463463374607431768211456"), (39, None));
        assert_eq!(prefix("u8", "0x"), (1, Some("0".into())));
        assert_eq!(prefix("u8", "1_"), (1, Some("1".into())));
    }

    #[test]
    fn dec_prefixes() {
        let dec = |t: &str| prefix("dec", t);
        assert_eq!(dec("1.x").0, 2);
        assert_eq!(dec("-.5,").0, 3);
        assert_eq!(dec("1.5e3]").0, 5);
        assert_eq!(dec("2e-1 ").0, 4);
        assert_eq!(dec("2e+").0, 1);
        assert_eq!(dec("1_000.5_0,").0, 9);
        assert_eq!(dec("1._5").0, 2);
        assert_eq!(dec("1.2.3").0, 3);
        assert_eq!(dec("0x1").0, 1);
        assert_eq!(dec("0.1234567890123456789,"), (21, None));
        assert_eq!(dec("1e-19,"), (5, None));
        assert_eq!(dec("170141183460469231731.687303715884105728,"), (40, None));
        assert_eq!(dec("1e40"), (4, None));
        for none in ["", "-", ".", "inf", "nan", " 1"] {
            assert_eq!(dec(none), (0, None), "{:?}", none);
        }
    }

    #[test]
    fn hex_floats_round_to_nearest_even() {
        let f = |t: &str| parse_whole(Kind::F64, t.as_bytes()).map(|v| v.to_string());
        assert_eq!(parse_float_token(b"0x1p3", false), Some(8.0));
        assert_eq!(parse_float_token(b"0x1.8p1", false), Some(3.0));
        assert_eq!(parse_float_token(b"0x1p-1074", false), Some(f64::from_bits(1)));
        assert_eq!(parse_float_token(b"0x1p-1075", false), Some(0.0));
        assert_eq!(parse_float_token(b"0x1.8p-1075", false), Some(f64::from_bits(1)));
        assert_eq!(parse_float_token(b"0x1.fffffffffffff8p0", false), Some(2.0));
        assert_eq!(parse_float_token(b"0x1p1024", false), None);
        assert_eq!(f("0x"), None);
    }
}
