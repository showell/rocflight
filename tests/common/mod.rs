//! The helpers every integration test file was defining for itself.
//!
//! Each of them is the same three steps — desugar, parse, type check — stopping at a
//! different one. They were copy-pasted into a dozen files with small pointless
//! differences (`ty` vs `type_of`, `eval_err` vs `eval_error`, panic vs `to_string` on
//! a non-Str); this is the union, and the permissive branch wins every time.

#![allow(dead_code)]

pub use rocflight::desugaring::Desugarer;
pub use rocflight::eval::Value;
pub use rocflight::parser::Parser;
pub use rocflight::types::TypeChecker;

/// Desugar and parse, surfacing a parse error.
pub fn parse(src: &str) -> Result<rocflight::ast::Expr, rocflight::error::ParseError> {
    let desugared = Desugarer::new(src.to_string()).desugar().unwrap();
    Parser::new(&desugared).parse_expr()
}

/// Same, but a parse failure is a test failure.
pub fn build(src: &str) -> rocflight::ast::Expr {
    parse(src).unwrap_or_else(|e| panic!("parse failed on:\n{}\n{}", src, e))
}

/// Type check and evaluate. `where` clauses are honoured, which is a no-op for a
/// program that has none.
pub fn eval(src: &str) -> Value {
    let desugared = Desugarer::new(src.to_string()).desugar().unwrap();
    let mut parser = Parser::new(&desugared);
    let ast = parser
        .parse_expr()
        .unwrap_or_else(|e| panic!("parse failed on:\n{}\n{}", src, e));
    let mut checker = TypeChecker::new();
    checker.allow_dispatch(parser.where_methods());
    checker
        .synth(&ast)
        .unwrap_or_else(|e| panic!("type check failed on:\n{}\n{}", src, e));
    rocflight::vm::eval(&ast).unwrap_or_else(|e| panic!("eval failed on:\n{}\n{}", src, e))
}

/// The evaluated value as roc would print it.
pub fn value(src: &str) -> String {
    eval(src).to_string()
}

pub fn as_str(src: &str) -> String {
    match eval(src) {
        Value::Str(s) => s.to_string(),
        other => other.to_string(),
    }
}

pub fn as_bool(src: &str) -> bool {
    match eval(src) {
        Value::Bool(b) => b,
        other => panic!("expected Bool, got {:?}", other),
    }
}

/// The inferred type, as written.
pub fn type_of(src: &str) -> String {
    TypeChecker::new()
        .synth(&build(src))
        .unwrap_or_else(|e| panic!("type check failed on:\n{}\n{}", src, e))
        .to_string()
}

/// The inferred type with its free variables defaulted — what roc would report.
pub fn defaulted_type_of(src: &str) -> String {
    let mut checker = TypeChecker::new();
    let ty = checker
        .synth(&build(src))
        .unwrap_or_else(|e| panic!("type check failed on:\n{}\n{}", src, e));
    checker.defaulted(&ty).to_string()
}

pub fn accepts(src: &str) -> bool {
    TypeChecker::new().synth(&build(src)).is_ok()
}

pub fn type_error(src: &str) -> String {
    match TypeChecker::new().synth(&build(src)) {
        Err(e) => e.message,
        Ok(t) => panic!("expected a type error on:\n{}\ngot {}", src, t),
    }
}

/// An error from evaluation, with the type checker's verdict ignored.
pub fn eval_error(src: &str) -> String {
    let ast = build(src);
    let _ = TypeChecker::new().synth(&ast);
    rocflight::vm::eval(&ast)
        .expect_err("expected an eval error")
        .message
}

/// Run a whole program through `run_file`, the pipeline `rocflight file.roc` uses:
/// the program's own type declarations reach the checker, which `accepts` and
/// `type_error` never give it. `Err` is what the run reported.
pub fn run_program(src: &str) -> Result<(), String> {
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!("rocflight-test-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join("main.roc");
    std::fs::write(&path, src).expect("write the program");
    let verdict = rocflight::run::run_file(path.to_str().expect("utf-8 path"), Default::default());
    let _ = std::fs::remove_dir_all(&dir);
    verdict.map(|_| ()).map_err(|e| e.to_string())
}
