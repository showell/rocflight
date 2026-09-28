//! How a float is written, by `Str.inspect` and `to_str` alike: roc's
//! `formatFloatDecimal` (compiler_rt_128.zig). The shortest digits that read back as
//! the same value, in exponent form when the decimal point is more than 16 places right
//! of the first digit or 4 or more places left of it (Python's `repr` thresholds), and
//! plain decimal otherwise.
//!
//! The expected output is what roc `nightly-2026-09-27-a3ce7f1` printed for the program.

use std::process::Command;

#[test]
fn floats_are_written_as_roc_writes_them() {
    let program = r####"app [main!] {}

main! = |_args| {
	fs : List(F64)
	fs = [0.0, -0.0, 1.5, 0.1, 0.1 + 0.2, 0.001, 0.0001, 0.00012345, 0.00001, 0.000012345, -0.00001, -0.0001, 123.456, 1e15, 9999999999999998.0, 1e16, 12345678901234567.0, 1e17, -1e16, 1e21, 1e22, 1e-300, 1.7976931348623157e308, 5e-324, 2.5e-5, 100.0, 1234567.0]
	for f in fs {
		echo!("${Str.inspect(f)} ${f.to_str()}\n")
	}
	gs : List(F32)
	gs = [0.0, 1.5, 0.1, 0.0001, 0.00001, 0.000012345, 1e15, 1e16, 12345678.0, 1e17, 3.4028235e38, 1e-45, -2.5e-5, 16777216.0, 1e-38]
	for g in gs {
		echo!("${Str.inspect(g)} ${g.to_str()}\n")
	}
	hs : List(F64)
	hs = [1e-7, 2e20, 3e30]
	r : { x : F64 }
	r = { x: 1e-10 }
	echo!("${Str.inspect(F64.infinity)} ${Str.inspect(-F64.infinity)} ${Str.inspect(F64.nan)} ${Str.inspect(hs)} ${Str.inspect(r)}\n")
	Ok({})
}
"####;
    let expected = r####"0 0
-0 -0
1.5 1.5
0.1 0.1
0.30000000000000004 0.30000000000000004
0.001 0.001
0.0001 0.0001
0.00012345 0.00012345
1e-5 1e-5
1.2345e-5 1.2345e-5
-1e-5 -1e-5
-0.0001 -0.0001
123.456 123.456
1000000000000000 1000000000000000
9999999999999998 9999999999999998
1e16 1e16
1.2345678901234568e16 1.2345678901234568e16
1e17 1e17
-1e16 -1e16
1e21 1e21
1e22 1e22
1e-300 1e-300
1.7976931348623157e308 1.7976931348623157e308
5e-324 5e-324
2.5e-5 2.5e-5
100 100
1234567 1234567
0 0
1.5 1.5
0.1 0.1
0.0001 0.0001
1e-5 1e-5
1.2345e-5 1.2345e-5
1000000000000000 1000000000000000
1e16 1e16
12345678 12345678
1e17 1e17
3.4028235e38 3.4028235e38
1e-45 1e-45
-2.5e-5 -2.5e-5
16777216 16777216
1e-38 1e-38
inf -inf nan [1e-7, 2e20, 3e30] { x: 1e-10 }
"####;
    let dir = std::env::temp_dir().join(format!("rocflight-float-display-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.roc");
    std::fs::write(&file, program).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_rocflight")).arg(&file).output().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(String::from_utf8(out.stdout).unwrap(), expected);
}
