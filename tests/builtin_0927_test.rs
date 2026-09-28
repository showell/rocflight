//! Builtin.roc re-synced to roc `nightly-2026-09-27-a3ce7f1`: the numeric members it
//! added, which rocflight answers in Rust because the `Num` member is not loaded.
//!
//! Every expected output here is what the 09-27 nightly printed for the same program.
//! Float values roc prints in exponent form (`1e-18`) are left out: rocflight writes
//! those out in full, a display difference older than this re-sync.

use std::process::Command;

fn run(test: &str, program: &str) -> String {
    let dir = std::env::temp_dir().join(format!("rocflight-builtin-0927-{}-{}", test, std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.roc");
    std::fs::write(&file, program).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_rocflight")).arg(&file).output().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(out.status.success(), "{} failed ({}):\n{}", test, out.status, String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).unwrap()
}

/// `T.from_str_prefix` and `T.from_utf8_prefix`: the longest token, no backtracking, and the
/// `rest` after it. Out of range is the token not fitting, not a shorter token that would.
#[test]
fn numeric_prefix_parsers_match_roc() {
    let program = r####"app [main!] {}

main! = |_args| {
	echo!("${Str.inspect(U8.from_str_prefix("42,rest"))} ${Str.inspect(U8.from_str_prefix("300,"))} ${Str.inspect(U8.from_str_prefix("abc"))}\n")
	echo!("${Str.inspect(I8.from_str_prefix("-7abc"))} ${Str.inspect(I8.from_str_prefix("0b12"))} ${Str.inspect(U16.from_str_prefix("-5"))}\n")
	echo!("${Str.inspect(I64.from_str_prefix("2e-1"))} ${Str.inspect(U32.from_str_prefix("1e-0x"))} ${Str.inspect(U64.from_str_prefix("0x"))} ${Str.inspect(I32.from_str_prefix("-0x10;"))}\n")
	echo!("${Str.inspect(I128.from_str_prefix("-170141183460469231731687303715884105728,"))} ${Str.inspect(U128.from_str_prefix("340282366920938463463374607431768211456"))}\n")
	echo!("${Str.inspect(Dec.from_str_prefix("1_000.5_0,"))} ${Str.inspect(Dec.from_str_prefix("0.1234567890123456789,"))} ${Str.inspect(Dec.from_str_prefix("-.5,"))} ${Str.inspect(Dec.from_str_prefix("inf"))}\n")
	echo!("${Str.inspect(F64.from_str_prefix("1.5x"))} ${Str.inspect(F64.from_str_prefix("-Infinity!"))} ${Str.inspect(F64.from_str_prefix("0x1.8p1q"))} ${Str.inspect(F32.from_str_prefix("nanx"))}\n")
	echo!("${Str.inspect(U8.from_utf8_prefix([0x35, 0x0D, 0xFF]))} ${Str.inspect(U8.from_utf8_prefix([0x33, 0x30, 0x30]))} ${Str.inspect(F64.from_utf8_prefix([0x31, 0x2E, 0x35, 0xFF]))}\n")
	Ok({})
}
"####;
    let expected = r####"Ok({ rest: ",rest", value: 42 }) Err(OutOfRange) Err(NotANumber)
Ok({ rest: "abc", value: -7 }) Ok({ rest: "2", value: 1 }) Err(OutOfRange)
Err(OutOfRange) Ok({ rest: "x", value: 1 }) Ok({ rest: "x", value: 0 }) Ok({ rest: ";", value: -16 })
Ok({ rest: ",", value: -170141183460469231731687303715884105728 }) Err(OutOfRange)
Ok({ rest: ",", value: 1000.5 }) Err(OutOfRange) Ok({ rest: ",", value: -0.5 }) Err(NotANumber)
Ok({ rest: "x", value: 1.5 }) Ok({ rest: "!", value: -inf }) Ok({ rest: "q", value: 3 }) Ok({ rest: "x", value: nan })
Ok({ rest: [13, 255], value: 5 }) Err(OutOfRange) Ok({ rest: [255], value: 1.5 })
"####;
    assert_eq!(run("prefix", program), expected);
}

/// `T.from_str` is the same grammar over the whole input: no whitespace, radix prefixes,
/// underscores between digits, `inf`/`nan`/hex floats, and no `1.0` for an integer.
#[test]
fn numeric_from_str_follows_the_prefix_grammar() {
    let program = r####"app [main!] {}

main! = |_args| {
	echo!("${Str.inspect(U8.from_str(" 42"))} ${Str.inspect(I64.from_str("42 "))} ${Str.inspect(U8.from_str("0x1F"))} ${Str.inspect(I64.from_str("-0x10"))} ${Str.inspect(U32.from_str("0o17"))}\n")
	echo!("${Str.inspect(I64.from_str("1_000"))} ${Str.inspect(I64.from_str("1__0"))} ${Str.inspect(U8.from_str("1.0"))} ${Str.inspect(U128.from_str("2e3"))} ${Str.inspect(U128.from_str("-0"))}\n")
	echo!("${Str.inspect(F64.from_str(" 1.5"))} ${Str.inspect(F64.from_str("1_000.5"))} ${Str.inspect(F64.from_str("inf"))} ${Str.inspect(F32.from_str("NaN"))} ${Str.inspect(F64.from_str("0x1p3"))} ${Str.inspect(Dec.from_str("1_000.5"))}\n")
	Ok({})
}
"####;
    let expected = r####"Err(BadNumStr) Err(BadNumStr) Ok(31) Ok(-16) Ok(15)
Ok(1000) Err(BadNumStr) Err(BadNumStr) Ok(2000) Ok(0)
Err(BadNumStr) Ok(1000.5) Ok(inf) Ok(nan) Ok(8) Ok(1000.5)
"####;
    assert_eq!(run("from_str", program), expected);
}

/// `atan2` (Dec by roc's own CORDIC, bit for bit), `Dec`'s rounding family in whole attos, and
/// `is_approx_eq`.
#[test]
fn atan2_dec_rounding_and_is_approx_eq_match_roc() {
    let program = r####"app [main!] {}

main! = |_args| {
	echo!("${Str.inspect(F64.atan2({ x: -1.0, y: 1.0 }))} ${Str.inspect(F32.atan2({ x: 4.0, y: 3.0 }))} ${Str.inspect(Dec.atan2({ x: 4.0, y: 3.0 }))} ${Str.inspect(Dec.atan2({ x: -1.0, y: -1.0 }))}\n")
	ds : List(Dec)
	ds = [2.5, -2.5, 1.49, -1.51, 12.345]
	for d in ds {
		echo!("${Str.inspect(d.round())} ${Str.inspect(d.floor())} ${Str.inspect(d.ceiling())} ${Str.inspect(d.trunc())} ${Str.inspect(d.round_to({ step: 0.01, ties: AwayFromZero }))} ${Str.inspect(d.round_to({ step: 0.1, ties: ToEven }))}\n")
	}
	echo!("${Str.inspect(Dec.lowest.round_try())} ${Str.inspect(Dec.highest.ceiling_try())} ${Str.inspect(Dec.highest.floor_try())}\n")
	echo!("${Str.inspect(F64.is_approx_eq(0.1 + 0.2, 0.3, { rel: 0.000001, abs: 0.0 }))} ${Str.inspect(Dec.is_approx_eq(1.0, 1.01, { rel: 0.01, abs: 0.0 }))} ${Str.inspect(Dec.is_approx_eq(Dec.highest, Dec.lowest, { rel: 1.0, abs: 0.0 }))} ${Str.inspect(F64.is_approx_eq(F64.nan, F64.nan, { rel: 0.5, abs: 1.0 }))}\n")
	Ok({})
}
"####;
    let expected = r####"2.356194490192345 0.6435011 0.643501108793284386 -2.356194490192344929
3.0 2.0 3.0 2.0 2.5 2.5
-3.0 -3.0 -2.0 -2.0 -2.5 -2.5
1.0 1.0 2.0 1.0 1.49 1.5
-2.0 -2.0 -1.0 -1.0 -1.51 -1.5
12.0 12.0 13.0 12.0 12.35 12.3
Err(Overflow) Err(Overflow) Ok(170141183460469231731.0)
True True False False
"####;
    assert_eq!(run("math", program), expected);
}
