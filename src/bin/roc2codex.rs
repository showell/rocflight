//! `roc2codex app.roc out.codex`: the checked Roc program as one resolved Codex
//! unit, which `codexrun out.codex` runs. See `rocflight::codex`.
//!
//! The Foreword chapters every Codex program carries are copied from a Cobblestone
//! checkout: `$COBBLESTONE` (default `~/showell_repos/cobblestone-u62`).
//!
//! Exit 0: written. Exit 2: refused, with the reason on stderr, as rocemit does.

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let [_, app, out] = args.as_slice() else {
        eprintln!("usage: roc2codex <app.roc> <out.codex>");
        std::process::exit(64);
    };
    let checkout = std::env::var_os("COBBLESTONE").map(std::path::PathBuf::from).unwrap_or_else(|| {
        std::path::PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join("showell_repos/cobblestone-u62")
    });
    let options = rocflight::run::Options {
        emit_codex: Some((out.into(), checkout.join("codex/foreword/core"))),
        ..Default::default()
    };
    if let Err(e) = rocflight::run::run_file(app, options) {
        eprintln!("REFUSED: {}", e);
        std::process::exit(2);
    }
}
