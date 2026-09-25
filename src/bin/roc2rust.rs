//! `roc2rust app.roc out.rs`: the checked Roc program as one Rust file, which
//! `rustc out.rs` compiles. See `rocflight::rust`.
//!
//! Exit 0: written. Exit 2: refused, with the reason on stderr.

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let [_, app, out] = args.as_slice() else {
        eprintln!("usage: roc2rust <app.roc> <out.rs>");
        std::process::exit(64);
    };
    let options = rocflight::run::Options { emit_rust: Some(out.into()), ..Default::default() };
    if let Err(e) = rocflight::run::run_file(app, options) {
        eprintln!("REFUSED: {}", e);
        std::process::exit(2);
    }
}
