// No `unsafe`, anywhere, enforced by the compiler. An interpreter's whole job is
// handing untrusted structure to a runtime, and a wrong opcode or a stale index should
// be a panic with a message — not a silent memory error that surfaces as a wrong
// answer in a golden pair three phases later. The one `unsafe` this crate used to
// contain was a lifetime transmute around the AST; dropping `Expr`'s vestigial
// lifetime parameter removed the need for it.
#![forbid(unsafe_code)]

//! An interpreter for the Roc programming language.
//!
//! The pipeline is `run::run_file`: desugar, parse, type check, compile, run. Each step
//! and what it hands the next is Learning.md §2; `ROCFLIGHT_TIME=1` prints their times.

pub mod ast;
pub mod types;
pub mod parser;
pub mod eval;
pub mod vm;
pub mod memory;
pub mod error;
pub mod desugaring;
pub mod platform;
pub mod builtin;
pub mod artifact;
pub mod run;
pub mod codex;

/// Is phase timing on? `ROCFLIGHT_TIME=1` turns it on.
///
/// The front end is where a short program's time goes — parsing the vendored
/// `Builtin.roc` dwarfs running the program — and no sampler on the development machine
/// could break that down: `perf` is not installed, `gprofng` recorded a tenth of its
/// samples, `valgrind` is absent. So the phases report themselves. Read once, because
/// this is asked per phase and per builtin member.
pub fn timing() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("ROCFLIGHT_TIME").is_some())
}

/// One phase boundary: how long since the last one, and reset the clock.
///
/// The DELTA rather than the cumulative elapsed, because that is the number a reader
/// wants — "which phase is 3ms" — and a caller who wants the total already has one.
/// Goes to stderr, which every harness that reads rocflight's answer discards, so this
/// cannot corrupt `rocflight eval`'s output even left switched on.
///
/// `label` is `impl Display` so a caller with a name to interpolate passes
/// `format_args!`, which allocates nothing when the timing is off.
pub fn tick(label: impl std::fmt::Display, since: &mut std::time::Instant) {
    if timing() {
        let now = std::time::Instant::now();
        // `to_string` because a `Display` that writes straight through — which
        // `format_args!` does — ignores the field width, and the column is the point.
        // Inside the guard, so it costs nothing when the timing is off.
        eprintln!("[time] {:>24} {:>8.3}ms", label.to_string(), (now - *since).as_secs_f64() * 1e3);
        *since = now;
    }
}

pub use ast::{Expr, Pattern};
pub use types::Type;
pub use types::TypeChecker;
pub use parser::Parser;
pub use error::{ParseError, TypeError};
pub use desugaring::Desugarer;
pub use platform::{PlatformLoader, Platform, PlatformRef};
