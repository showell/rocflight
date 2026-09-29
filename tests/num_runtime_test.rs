//! The numeric functions `Builtin.roc` declares, answered by rocflight's runtime.
//!
//! rocflight does not load `Builtin.roc`'s `Num` member at run time: every numeric
//! method is Rust. So a function the file declares but the runtime lacks only shows up
//! when a program calls it ("Unknown function U16.from_le_bytes"). The first test here
//! asks the question of every declared function at once, from the checker's own
//! signature tables, so the next re-sync finds its gaps at `cargo test`.

use rocflight::eval::Value;
use rocflight::types::Type;
use std::process::Command;

fn run(test: &str, program: &str) -> String {
    let dir = std::env::temp_dir().join(format!("rocflight-num-runtime-{}-{}", test, std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.roc");
    std::fs::write(&file, program).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_rocflight")).arg(&file).output().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(out.status.success(), "{} failed:\n{}", test, String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).unwrap()
}

/// A value of `ty` to call with, or `None` when there is no sensible one to make
/// (a function, a `Numeral`, a `Hasher`): those are skipped.
fn sample(ty: &Type) -> Option<Value> {
    Some(match ty {
        Type::U8 | Type::U16 | Type::U32 | Type::U64 | Type::I8 | Type::I16 | Type::I32 | Type::I64 | Type::I128 => {
            Value::Int(1)
        }
        Type::U128 => Value::U128(1),
        Type::F64 => Value::Float(1.5),
        Type::F32 => Value::F32(1.5),
        Type::Dec => Value::Dec(1_500_000_000_000_000_000),
        Type::Bool => Value::Bool(true),
        Type::Str => rocflight::eval::str_value("1"),
        Type::List(_) => Value::list(vec![Value::Int(1), Value::Int(0)]),
        Type::Tuple(items) => Value::tuple(items.iter().map(sample).collect::<Option<_>>()?),
        Type::Record { fields, .. } => {
            Value::record(fields.iter().map(|(n, t)| Some((*n, sample(t)?))).collect::<Option<_>>()?)
        }
        Type::TagUnion { tags, .. } => {
            let (name, payload) = tags.first()?;
            Value::tag(name, payload.iter().map(sample).collect::<Option<Vec<_>>>()?)
        }
        Type::TypeVar(_) => Value::Int(1),
        _ => return None,
    })
}

/// Declared, and deliberately not answered by `call_builtin_values`.
const ANSWERED_ELSEWHERE: &[(&str, &str)] = &[
    // The `Encoding` protocol: dispatched through `dispatch_builtin` and the Json code.
    ("parser_for", "Encoding protocol"),
    ("encoder_for", "Encoding protocol"),
    ("encode", "Encoding protocol"),
    ("decode", "Encoding protocol"),
    // A custom `Range`'s element hooks, reached through `best_method` (`call_range`).
    ("range_iter", "Range protocol"),
    ("range_len_if_known", "Range protocol"),
];

#[test]
fn every_declared_numeric_function_has_a_runtime_answer() {
    let mut missing = Vec::new();
    for module in ["U8", "I8", "U16", "I16", "U32", "I32", "U64", "I64", "U128", "I128", "F32", "F64", "Dec"] {
        let table = rocflight::builtin::signatures_for(module);
        assert!(!table.is_empty(), "no signatures for {}", module);
        for (qualified, ty) in table {
            let name = qualified.rsplit('.').next().unwrap_or(qualified);
            // roc refuses `==` on floats (`is_float_eq`), and so does the checker.
            if ANSWERED_ELSEWHERE.iter().any(|(n, _)| *n == name) || (name == "is_eq" && module.starts_with('F')) {
                continue;
            }
            let mut args = Vec::new();
            let mut at = ty.clone();
            let mut makeable = true;
            while let Type::Function(param, result) = at {
                match sample(&param) {
                    Some(value) => args.push(value),
                    None => makeable = false,
                }
                at = *result;
            }
            if !makeable {
                continue;
            }
            // An error is fine (a crash for a bad argument is an answer); only "there is
            // no such function" is not.
            if let Err(e) = rocflight::eval::call_builtin_values(module, name, &mut args) {
                if e.message.contains("Unknown function") || e.message.contains("not implemented") {
                    missing.push(format!("{}.{} : {}", module, name, ty));
                }
            }
        }
    }
    assert!(missing.is_empty(), "declared in Builtin.roc, unknown to the runtime:\n{}", missing.join("\n"));
}

/// `from_le_bytes`, `append_le_bytes_to`, `from_int_digits`, `from_dec_digits`, `is_multiple_of`,
/// `to`/`until`, `to_dec_try` and `to_f64_*`, across the widths. Expected output: roc 09-27's.
#[test]
fn the_numeric_functions_the_runtime_lacked_match_roc() {
    assert_eq!(run("sweep", r####"app [main!] {}

main! = |_args| {
	bytes : List(U8)
	bytes = [0x34, 0x12, 0xFF, 0x7F, 0x01, 0x00, 0x00, 0x80, 0x10, 0x20, 0x30, 0x40, 0x50, 0x60, 0x70, 0x80, 0x90]
	echo!("le: ${Str.inspect(U16.from_le_bytes(bytes, 0))} ${Str.inspect(U16.from_le_bytes(bytes, 16))} ${Str.inspect(I16.from_le_bytes(bytes, 2))} ${Str.inspect(U32.from_le_bytes(bytes, 4))} ${Str.inspect(I32.from_le_bytes(bytes, 4))} ${Str.inspect(U64.from_le_bytes(bytes, 1))} ${Str.inspect(I64.from_le_bytes(bytes, 8))} ${Str.inspect(U128.from_le_bytes(bytes, 0))} ${Str.inspect(I128.from_le_bytes(bytes, 1))} ${Str.inspect(I128.from_le_bytes(bytes, 2))}\n")
	echo!("append: ${Str.inspect(0x1234.U64.append_le_bytes_to([], 2))} ${Str.inspect(U64.append_le_bytes_to(0xFF, [9], 1))} ${Str.inspect(1.U64.append_le_bytes_to([], 9))} ${Str.inspect(U64.append_le_bytes_to(0x0102030405060708, [], 8))}\n")
	d : List(U8)
	d = [1, 2, 3]
	echo!("digits: ${Str.inspect(U8.from_int_digits(d))} ${Str.inspect(U8.from_int_digits([2, 5, 6]))} ${Str.inspect(I8.from_int_digits([1, 2, 7]))} ${Str.inspect(I8.from_int_digits([1, 2, 8]))} ${Str.inspect(U64.from_int_digits([]))} ${Str.inspect(U16.from_int_digits([1, 10]))} ${Str.inspect(I128.from_int_digits([9, 9]))} ${Str.inspect(U128.from_int_digits([3, 4, 0, 2, 8, 2, 3, 6, 6, 9, 2, 0, 9, 3, 8, 4, 6, 3, 4, 6, 3, 3, 7, 4, 6, 0, 7, 4, 3, 1, 7, 6, 8, 2, 1, 1, 4, 5, 5]))}\n")
	echo!("fdigits: ${Str.inspect(F64.from_int_digits([4, 2]))} ${Str.inspect(F32.from_int_digits([1, 6]))} ${Str.inspect(Dec.from_int_digits([1, 2]))} ${Str.inspect(Dec.from_dec_digits(([1, 2], [5])))} ${Str.inspect(F64.from_dec_digits(([0], [2, 5])))} ${Str.inspect(F32.from_dec_digits(([3], [])))} ${Str.inspect(Dec.from_dec_digits(([1], [1, 2, 3, 4, 5, 6, 7, 8, 9, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9])))}\n")
	echo!("mult: ${Str.inspect(U8.is_multiple_of(12, 4))} ${Str.inspect(U8.is_multiple_of(12, 5))} ${Str.inspect(U8.is_multiple_of(0, 0))} ${Str.inspect(U8.is_multiple_of(3, 0))} ${Str.inspect(I8.is_multiple_of(-128, -1))} ${Str.inspect(I64.is_multiple_of(-12, 4))} ${Str.inspect(12.U32.is_multiple_of(6))} ${Str.inspect(U128.is_multiple_of(340282366920938463463374607431768211455, 5))}\n")
	echo!("iter: ${Str.inspect(Iter.fold(U8.to(1, 4), [], |acc, item| acc.append(item)))} ${Str.inspect(Iter.fold(U8.until(1, 4), [], |acc, item| acc.append(item)))} ${Str.inspect(Iter.fold(U8.to(250, 255), [], |acc, item| acc.append(item)))} ${Str.inspect(Iter.fold(I8.until(-2, 2), [], |acc, item| acc.append(item)))} ${Str.inspect(Iter.fold(U64.to(5, 2), [], |acc, item| acc.append(item)))} ${Str.inspect(Iter.fold(3.I32.until(5), [], |acc, item| acc.append(item)))}\n")
	echo!("conv: ${Str.inspect(U128.to_dec_try(5))} ${Str.inspect(U128.to_dec_try(340282366920938463463374607431768211455))} ${Str.inspect(I128.to_dec_try(-7))} ${Str.inspect(1.5.F64.to_f64_wrap())} ${Str.inspect(F64.to_f64_try(2.5))} ${Str.inspect(Dec.to_dec_try(3.25))}\n")
	Ok({})
}
"####), r####"le: Ok(4660) Err(OutOfBounds) Ok(32767) Ok(2147483649) Ok(-2147483647) Ok(1188950301650976530) Ok(-9191740941672636400) Ok(170724674178065304615596741374464954932) Ok(-148206642269402510169071715980404588782) Err(OutOfBounds)
append: Ok([52, 18]) Ok([9, 255]) Err(OutOfBounds) Ok([8, 7, 6, 5, 4, 3, 2, 1])
digits: Ok(123) Err(OutOfRange) Ok(127) Err(OutOfRange) Err(OutOfRange) Err(OutOfRange) Ok(99) Ok(340282366920938463463374607431768211455)
fdigits: Ok(42) Ok(16) Ok(12.0) Ok(12.5) Ok(0.25) Ok(3) Err(OutOfRange)
mult: True False True False True True True True
iter: [1, 2, 3, 4] [1, 2, 3] [250, 251, 252, 253, 254, 255] [-2, -1, 0, 1] [] [3, 4]
conv: Ok(5.0) Err(OutOfRange) Ok(-7.0) 1.5 Ok(2.5) Ok(3.25)
"####);
}

/// `Dec.to`/`Dec.until` step by 1.0 from the start, whatever its fraction. Expected output: roc 09-27's.
#[test]
fn dec_to_and_until_step_by_one_as_roc_does() {
    assert_eq!(run("dec_to", r####"app [main!] {}

main! = |_args| {
	a : Dec
	a = 1.5
	echo!("${Str.inspect(Iter.fold(Dec.to(1.0, 4.0), [], |acc, item| acc.append(item)))} ${Str.inspect(Iter.fold(Dec.to(a, 4.0), [], |acc, item| acc.append(item)))} ${Str.inspect(Iter.fold(Dec.until(-2.0, 1.0), [], |acc, item| acc.append(item)))} ${Str.inspect(Iter.fold(Dec.until(5.0, 2.0), [], |acc, item| acc.append(item)))} ${Str.inspect(Iter.fold(Dec.to(2.0, 2.0), [], |acc, item| acc.append(item)))}\n")
	Ok({})
}
"####), r####"[1.0, 2.0, 3.0, 4.0] [1.5, 2.5, 3.5] [-2.0, -1.0, 0.0] [] [2.0]
"####);
}

/// Leading zeros, a non-digit item, an empty part, both parts empty, and floats that
/// overflow. Expected output: roc 09-27's.
#[test]
fn digit_functions_agree_with_roc_at_the_edges() {
    assert_eq!(run("edges", r####"app [main!] {}

main! = |_args| {
	z : List(U8)
	z = []
	echo!("${Str.inspect(U8.from_int_digits([0, 0, 7]))} ${Str.inspect(I32.from_int_digits([0]))} ${Str.inspect(U8.from_int_digits([1, 255]))} ${Str.inspect(F64.from_int_digits(z))} ${Str.inspect(Dec.from_int_digits([1, 7, 0, 1, 4, 1, 1, 8, 3, 4, 6, 0, 4, 6, 9, 2, 3, 1, 7, 3, 2]))}\n")
	echo!("${Str.inspect(Dec.from_dec_digits((z, [5])))} ${Str.inspect(Dec.from_dec_digits((z, z)))} ${Str.inspect(F64.from_dec_digits(([1], z)))} ${Str.inspect(F64.from_dec_digits(([0, 0], [0, 1])))} ${Str.inspect(F32.from_dec_digits(([9], [1, 10])))} ${Str.inspect(Dec.from_dec_digits(([0], [1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0])))}\n")
	big = List.repeat(9.U8, 400)
	echo!("${Str.inspect(F64.from_int_digits(big))} ${Str.inspect(F32.from_int_digits(List.repeat(9.U8, 40)))}\n")
	Ok({})
}
"####), r####"Ok(7) Ok(0) Err(OutOfRange) Err(OutOfRange) Err(OutOfRange)
Ok(0.5) Err(OutOfRange) Ok(1) Ok(0.01) Err(OutOfRange) Ok(0.1)
Err(OutOfRange) Err(OutOfRange)
"####);
}
