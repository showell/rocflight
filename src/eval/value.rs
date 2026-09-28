//! Runtime values for the evaluator

use std::fmt;

/// Runtime value
#[derive(Clone)]
pub enum Value {
    /// String value
    /// A string.
    ///
    /// `Rc<str>` rather than `&'static str`: the only way to get a `&'static str` at
    /// runtime is to leak, and every concatenation, interpolation and `to_str` made one.
    /// Building a 40,000-character string that way leaked 710 MB and never gave any of
    /// it back. Cloning stays cheap — a refcount bump instead of a pointer copy.
    Str(std::rc::Rc<str>),
    /// An integer. `i128`, because Roc has `U64`, `I128` and a `Dec` that is a
    /// 128-bit fixed-point value — an i64 cannot hold `U64.highest`, which
    /// `Builtin.roc` writes out in full.
    Int(i128),
    /// A 128-bit SIMD vector. `kind` is the element width in bits (8/16/32/64) with
    /// `0x80` set for signed lanes; `bits` holds the lanes packed little-endian, so
    /// lane `i` of an 8-wide vector is byte `i`. One `u128` plus a byte, so `Value`
    /// stays small.
    Simd { kind: u8, bits: u128 },
    /// An unsigned 128-bit integer. `U128` values above `i128::MAX` cannot be held as
    /// an `i128` without reading back negative, so they get their own variant; smaller
    /// `U128`s may still arrive as `Int` and both compare and print the same.
    U128(u128),
    /// An `F64`.
    Float(f64),
    /// An `F32`, kept as one so that `0.1.F32 + 0.2.F32` rounds where roc rounds and
    /// prints `0.3`, and `to_bits` has thirty-two of them to give back.
    F32(f32),
    /// A fixed-point decimal: the value times `Dec::SCALE`, exactly as roc stores it
    /// (`roc-compiler/src/builtins/dec.zig`, `decimal_places: u5 = 18`).
    ///
    /// NOT a float. `147.666666666666666666` is representable here and is not in an
    /// f64, which is the whole reason the type exists.
    Dec(i128),
    /// Builtin function marker: name + arity
    /// One of the interpreter's own functions, passed as a value — `xs.map(Str.inspect)`.
    ///
    /// `&'static str`, not `String`: the name is `Module.name` from the compiler's own
    /// tables, so it already lives as long as the program, and an owned `String` here
    /// made this the widest arm of the enum — which every other `Value` paid for.
    Builtin(&'static str, usize),
    /// A `var` some closure captures: one cell shared by the scope that declared it
    /// and every closure over it, so an assignment is seen by all of them, as roc
    /// does it. Never a first-class value — the compiler reads and writes through it.
    Cell(std::rc::Rc<std::cell::RefCell<Value>>),
    /// A function value: a chunk to run, and the values it captured.
    ///
    /// Boxed, because this is otherwise the variant that would decide
    /// `size_of::<Value>()` — and `Value` is moved on every binding, argument, list
    /// element and return, so its width is a tax on the whole interpreter.
    Closure(std::rc::Rc<crate::vm::Closure>),
    /// Empty record `{}` — Roc's unit value.
    Unit,
    /// An optional record field that was not written: `{ b: 2 }` as `{ a ?: U8, b : U8 }`
    /// holds one of these at `a`. Reads as `Err(MissingField)`, inspects as `<missing>`.
    Missing,
    /// Record value. Fields keep insertion order; `Str.inspect` sorts a copy.
    ///
    /// Behind an `Rc` for the reason `List` is, and it took until Phase 7 to notice
    /// that the reason applied here too: a `Value` is cloned on every register move,
    /// every argument and every return, and a `Vec` clone copies every field. Passing
    /// a record to a function was therefore linear in its WIDTH — a 16-field record
    /// cost 87% more than a 2-field one to do identical work.
    ///
    /// Build one with `Value::record`. `UpdateRecord` mutates through `Rc::make_mut`,
    /// so a record nothing else holds would be updated in place — which does not
    /// happen yet, for the reason written at that opcode.
    Record(std::rc::Rc<Vec<(&'static str, Value)>>),
    /// Boolean.
    Bool(bool),
    /// A list.
    ///
    /// Behind an `Rc`, because a `Value` is cloned on every register move, every
    /// argument and every global read, and a `Vec` clone is a copy of every element.
    /// Passing a list to a function was therefore linear in its length, and a loop
    /// that passed one on each iteration was quadratic: 8,000 elements took four
    /// seconds. roc itself refcounts lists for the same reason.
    ///
    /// Build one with `Value::list`; take the `Vec` back out with `into_items`, which
    /// copies only if something else still holds the list.
    List(std::rc::Rc<Vec<Value>>),
    /// Tuple value. Fixed length, elements may differ in type.
    ///
    /// Behind an `Rc` for the same reason `Record` is — see there. Build one with
    /// `Value::tuple`.
    Tuple(std::rc::Rc<Vec<Value>>),
    /// A range of integers, `start` to `end`, `end` included only if `inclusive`.
    ///
    /// Deliberately NOT a list: roc keeps ranges opaque, so building one as a list
    /// would show `[0, 1, 2]` where roc shows `<opaque>` and would wrongly satisfy a
    /// `List` parameter.
    /// `start..<end` or `start..=end`, walked `step` at a time: `step_by` on a range
    /// sets the step outright, as roc's does, rather than compounding.
    /// The step is an `i64` so the variant stays within the 48 bytes `Value` has.
    Range { start: i128, end: i128, inclusive: bool, step: i64 },
    /// A lazy iterator: `Iter.custom`, a non-integer range, or a filtered/mapped
    /// source. One pointer, so `Value` stays 48 bytes. The eager `List` and integer
    /// `Range` paths are untouched — this is only what must observe laziness.
    Iter(std::rc::Rc<crate::eval::lazy::Lazy>),
    /// A tag value: `Ok(x)`, `Err(e)`, `Red`.
    ///
    /// The payload is behind an `Rc` for the same reason `Lambda` is: with a `Vec`
    /// inline this was the widest arm of the enum, so every `Value` in the program
    /// was as big as a tag. It also makes cloning a tag a refcount bump instead of a
    /// fresh allocation and a copy of every element, which is what passing one to a
    /// function does.
    ///
    /// `Rc<[Value]>` and not `Rc<Vec<Value>>`: a payload is never changed after it is
    /// built, and the `Vec` cost a second heap allocation and a second indirection for
    /// nothing. Build one with `Value::tag` from an ARRAY or an exact-size iterator,
    /// which allocates once — `Rc::from(a_vec)` does not save anything, it allocates
    /// the box and memcpies into it. A tag with no payload is `Value::bare`, which
    /// allocates nothing at all.
    Tag(&'static str, std::rc::Rc<[Value]>),
}

/// The payload every bare tag shares.
///
/// `Rc::new(Vec::new())` allocated: the `Vec` does not, but the `Rc` box does, so
/// `None`, `Dot` and `MissingField` each cost a `malloc` to build. One of these,
/// cloned, costs a refcount bump. Thread-local because `Value` is not `Send`.
fn empty_payload() -> std::rc::Rc<[Value]> {
    thread_local! {
        static EMPTY: std::rc::Rc<[Value]> = std::rc::Rc::from([]);
    }
    EMPTY.with(std::rc::Rc::clone)
}

impl Value {
    /// A tag value. Takes an array or anything else that becomes an `Rc<[Value]>` in
    /// one allocation — NOT a `Vec`, which costs two; see the variant's own note.
    pub fn tag(name: &'static str, payload: impl Into<std::rc::Rc<[Value]>>) -> Value {
        Value::Tag(name, payload.into())
    }

    /// A tag with no payload: `None`, `Dot`, `MissingField`. Allocates nothing.
    pub fn bare(name: &'static str) -> Value {
        Value::Tag(name, empty_payload())
    }

    /// A list value. Wraps the elements so call sites stay readable.
    pub fn list(items: Vec<Value>) -> Value {
        Value::List(std::rc::Rc::new(items))
    }

    /// A record value. Wraps the fields so call sites stay readable.
    pub fn record(fields: Vec<(&'static str, Value)>) -> Value {
        Value::Record(std::rc::Rc::new(fields))
    }

    /// A tuple value. Wraps the elements so call sites stay readable.
    pub fn tuple(items: Vec<Value>) -> Value {
        Value::Tuple(std::rc::Rc::new(items))
    }

    /// The elements of `List` or a `Tuple`, for the operations that treat them alike.
    pub fn sequence(&self) -> Option<&[Value]> {
        match self {
            Value::List(items) => Some(items),
            Value::Tuple(items) => Some(items),
            _ => None,
        }
    }
}

/// A list's elements as an owned `Vec`: moved out when nothing else holds the list,
/// copied otherwise. This is the same rule roc uses to mutate a unique list in place.
pub fn into_items(items: std::rc::Rc<Vec<Value>>) -> Vec<Value> {
    std::rc::Rc::try_unwrap(items).unwrap_or_else(|shared| (*shared).clone())
}

impl fmt::Debug for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Cell(c) => write!(f, "Cell({:?})", c.borrow()),
            Value::Str(s) => write!(f, "Str({})", s),
            Value::Int(n) => write!(f, "Int({})", n),
            Value::Simd { kind, bits } => write!(f, "Simd({}, {})", kind, bits),
            Value::U128(n) => write!(f, "U128({})", n),
            Value::Float(n) => write!(f, "Float({})", n),
            Value::F32(n) => write!(f, "F32({})", n),
            Value::Missing => write!(f, "Missing"),
            Value::Dec(n) => write!(f, "Dec({})", crate::eval::dec_to_string(*n)),
            Value::Builtin(name, arity) => write!(f, "Builtin({}, {})", name, arity),
            Value::Closure(c) => write!(f, "Closure(|{}| ...)", c.params.join(", ")),
            Value::Unit => write!(f, "Unit"),
            Value::Bool(b) => write!(f, "Bool({})", b),
            Value::List(items) => write!(f, "List({:?})", items),
            Value::Tuple(items) => write!(f, "Tuple({:?})", items),
            Value::Range { start, end, inclusive, .. } => {
                write!(f, "Range({}..{}{})", start, if *inclusive { "=" } else { "<" }, end)
            }
            Value::Iter(_) => write!(f, "Iter(<opaque>)"),
            Value::Record(fields) => write!(f, "Record({:?})", fields),
            Value::Tag(name, args) => write!(f, "Tag({}, {:?})", name, args),
        }
    }
}

/// Build a `Value::Str` from anything string-shaped.
pub fn str_value(text: impl Into<std::rc::Rc<str>>) -> Value {
    Value::Str(text.into())
}

/// A string as roc shows it inside a container or an inspect: quoted, with `\\` and
/// `"` escaped. A tab or newline is left as itself — roc does not escape those.
pub fn quoted(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Cell(c) => write!(f, "{}", c.borrow()),
            Value::Str(s) => write!(f, "{}", quoted(s)),
            Value::Int(n) => write!(f, "{}", n),
            Value::U128(n) => write!(f, "{}", n),
            Value::Simd { kind, bits } => write!(f, "{}", crate::eval::simd_inspect(*kind, *bits)),
            // roc prints a whole float WITHOUT a trailing `.0` — `1500.0` shows as
            // `1500` and `0.0` as `0` — and switches to exponent form far from 1; see
            // `float_text`.
            //
            // (An unconstrained integer literal still differs: roc defaults it to a
            // fractional type and shows `42.0`, while this interpreter keeps it an
            // integer. That is the documented numeric-default divergence, not this.)
            Value::Float(n) => float_text(f, *n),
            // The shortest digits that read back as the same f32, which is what roc
            // prints for an `F32`: `0.1.F32` is `0.1`, not its f64 expansion.
            Value::F32(n) => float_text(f, *n),
            Value::Dec(n) => write!(f, "{}", crate::eval::dec_to_string(*n)),
            Value::Builtin(name, arity) => write!(f, "<builtin {}/{}>", name, arity),
            Value::Closure(c) => write!(f, "<lambda |{}|>", c.params.join(", ")),
            Value::Unit => write!(f, "{{}}"),
            Value::Missing => write!(f, "<missing>"),
            Value::Bool(b) => write!(f, "{}", if *b { "True" } else { "False" }),
            Value::List(items) => {
                let rendered: Vec<String> = items.iter().map(|i| i.to_string()).collect();
                write!(f, "[{}]", rendered.join(", "))
            }
            Value::Tuple(items) => {
                let rendered: Vec<String> = items.iter().map(|i| i.to_string()).collect();
                write!(f, "({})", rendered.join(", "))
            }
            // roc renders a range and an iterator as `<opaque>`.
            Value::Range { .. } | Value::Iter(_) => write!(f, "<opaque>"),
            Value::Record(fields) => {
                if fields.is_empty() {
                    return write!(f, "{{}}");
                }
                let rendered: Vec<String> =
                    fields.iter().map(|(k, v)| format!("{}: {}", k, v)).collect();
                write!(f, "{{ {} }}", rendered.join(", "))
            }
            Value::Tag(name, args) => {
                if args.is_empty() {
                    write!(f, "{}", name)
                } else {
                    let rendered: Vec<String> = args.iter().map(|a| a.to_string()).collect();
                    write!(f, "{}({})", name, rendered.join(", "))
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Value;

    /// `Value` is moved on every binding, argument, list element and return, so its
    /// width is a tax on the whole interpreter — and a register VM's main job is moving
    /// them. It was 64 bytes until `Lambda` and `Tag` were boxed. This is the guard
    /// against a new inline field quietly putting it back.
    ///
    /// 48 rather than 32 since `Int` became an `i128`: Roc has `U64`, `I128` and a
    /// fixed-point `Dec`, and `Builtin.roc` writes `U128.highest` out in full, so an
    /// i64 could not hold the language's own numbers. `i128` aligns to 16, which is
    /// what takes the enum from 32 to 48. Measured across the whole benchmark suite
    /// before it was accepted: nothing moved, and `records` got faster.
    #[test]
    fn value_stays_narrow() {
        assert_eq!(
            std::mem::size_of::<Value>(),
            48,
            "Value grew — box the new variant's payload instead"
        );
    }
}

/// A float as roc writes it (`formatFloatDecimal`, compiler_rt_128.zig): the shortest
/// digits that read back as the same value of ITS width, then exponent form (`1e-5`,
/// `1.2345678901234568e16`) when the decimal point is more than 16 places right of the
/// first digit or 4 or more places left of it, and plain decimal (`0.0001`,
/// `9999999999999998`) otherwise. Those are Python's `repr` thresholds.
///
/// Rust's `{:e}` and `{}` already give the same shortest digits in exactly those two
/// layouts, so only the choice between them is roc's. `nan` is roc's spelling.
fn float_text<T: std::fmt::Display + std::fmt::LowerExp + Copy>(f: &mut fmt::Formatter<'_>, n: T) -> fmt::Result
where
    f64: From<T>,
{
    let x = f64::from(n);
    if x.is_nan() {
        return write!(f, "nan");
    }
    // Well inside both thresholds the answer is plain decimal whatever the digits, so
    // the common case formats once. Only near or past them does the layout depend on
    // where the shortest digits put the point.
    if x == 0.0 || x.is_infinite() || (1e-3..1e15).contains(&x.abs()) {
        return write!(f, "{}", n);
    }
    // On the stack: the longest `{:e}` of an f64 is 24 bytes (`-2.2250738585072014e-308`).
    let mut buffer = StackText { bytes: [0; 32], len: 0 };
    fmt::Write::write_fmt(&mut buffer, format_args!("{:e}", n))?;
    let scientific = std::str::from_utf8(&buffer.bytes[..buffer.len]).map_err(|_| fmt::Error)?;
    // `d.ddde<exp>`: the first digit sits at 10^exp, so the point is exp + 1 digits in.
    let exponent: i32 = scientific.rsplit('e').next().and_then(|e| e.parse().ok()).unwrap_or(0);
    let point = exponent + 1;
    if point > 16 || point <= -4 {
        f.write_str(scientific)
    } else {
        write!(f, "{}", n)
    }
}

/// A fixed buffer to format into without allocating; see `float_text`.
struct StackText {
    bytes: [u8; 32],
    len: usize,
}

impl fmt::Write for StackText {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let end = self.len + s.len();
        self.bytes.get_mut(self.len..end).ok_or(fmt::Error)?.copy_from_slice(s.as_bytes());
        self.len = end;
        Ok(())
    }
}
