//! Programs split across files: an app beside the local modules it imports.
//!
//! Each runs the real binary in a directory of its own, because the pipeline's module
//! loading is file-based, and because a failure here can be a stack overflow, which
//! aborts a process rather than panicking a test.

use std::path::PathBuf;
use std::process::Command;

/// Write `files` into a fresh directory and run its `main.roc`, answering stdout.
fn run(test: &str, files: &[(&str, &str)]) -> String {
    let dir: PathBuf = std::env::temp_dir().join(format!("rocflight-module-test-{}-{}", test, std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for (name, text) in files {
        std::fs::write(dir.join(name), text).unwrap();
    }
    let out = Command::new(env!("CARGO_BIN_EXE_rocflight"))
        .arg(dir.join("main.roc"))
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        out.status.success(),
        "{} failed ({}):\n{}",
        test,
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

/// B-Teague/rocflight#9. `Code.(c)` inside an imported module's own methods is a
/// construction, as it is in the app's file. The checker was never told the module's
/// constructions, so each was compiled as a value to convert at run time, and the
/// conversion (`from_numeral`) constructs a `Code` itself: unbounded recursion.
#[test]
fn an_imported_nominal_with_from_numeral_constructs_without_converting() {
    let code = "\
Code :: I64.{
\tof : I64 -> Code
\tof = |c| Code.(c)

\tcode : Code -> I64
\tcode = |Code.(c)| c

\tfrom_numeral : Numeral -> Try(Code, [InvalidNumeral(Str)])
\tfrom_numeral = |n| match I64.from_numeral(n) {
\t\tOk(c) => Ok(Code.(c))
\t\tErr(e) => Err(e)
\t}
}
";
    let main = "\
app [main!] {}

import Code

main! = |_args| {
\techo!(Str.inspect(Code.code(Code.of(15))))
\tOk({})
}
";
    assert_eq!(run("from_numeral", &[("Code.roc", code), ("main.roc", main)]), "15");
}
