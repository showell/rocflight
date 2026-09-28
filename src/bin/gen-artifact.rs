//! Write `src/roc/Builtin.artifact`: `Builtin.roc`, parsed AND compiled.
//!
//! A build script cannot call the crate it is building, so this is a binary that runs
//! against the built library and its output is checked in. `build.rs` compares the hash
//! the artifact records against `src/roc/Builtin.roc` and refuses to build if they have
//! drifted, so a stale one is a compile error and never a wrong answer.
//!
//!     cargo run --release --bin gen-artifact
//!     tests/check_artifact.sh          # regenerates and diffs, for CI
//!
//! The COMPILED half goes through the real front end — a tiny program is written out
//! per selection and run with `emit_prefix` — because the checker's per-node facts are
//! what the compiler reads, and reproducing that pipeline here would be a second
//! implementation of it to keep in step.
use rocflight::artifact::{put_member, put_prefix, put_signature_table, source_hash, Writer};

fn main() {
    // Nothing below reads the artifact it is about to replace.
    rocflight::builtin::generating(true);
    let mut writer = Writer::default();
    let mut written = 0usize;
    let mut seen: Vec<String> = Vec::new();
    let selections = rocflight::builtin::artifact_selections();

    // The trees, one per member, under the key its cut-down is reached by.
    for selection in &selections {
        let parsed = rocflight::builtin::parse_members(selection)
            .unwrap_or_else(|e| panic!("parsing {:?}: {}", selection, e));
        for (loaded, base, offsets) in &parsed {
            let key = rocflight::builtin::artifact_key_for(loaded.name, selection);
            if seen.contains(&key) {
                continue;
            }
            seen.push(key.clone());
            put_member(
                &mut writer,
                &key,
                *base,
                offsets,
                &loaded.ast,
                &loaded.intrinsics,
                &loaded.signatures,
                &loaded.nominals,
            );
            written += 1;
        }
    }

    // The bytecode, one per selection.
    let dir = std::env::temp_dir().join("rocflight-gen-artifact");
    std::fs::create_dir_all(&dir).expect("a scratch directory");
    let mut prefixes = Vec::new();
    for selection in &selections {
        let key = rocflight::builtin::prefix_key(selection);
        if prefixes.contains(&key) {
            continue;
        }
        prefixes.push(key);
    }
    writer.count(prefixes.len());
    for (key, selection) in prefixes.iter().zip(&selections) {
        let path = dir.join("force.roc");
        std::fs::write(&path, forcing_program(selection)).expect("writing the forcing program");
        let options = rocflight::run::Options { emit_prefix: true, ..Default::default() };
        rocflight::run::run_file(path.to_str().expect("utf-8 path"), options)
            .unwrap_or_else(|e| panic!("compiling for {:?}: {}", selection, e));
        let (program, group, nodes_from, nodes_to) = rocflight::run::PREFIX_OUT
            .with(|out| out.borrow_mut().take())
            .unwrap_or_else(|| panic!("nothing was emitted for {:?}", selection));
        put_prefix(
            &mut writer,
            key,
            nodes_from,
            nodes_to,
            &rocflight::ast::offsets_between(nodes_from, nodes_to),
            &program,
            group.chunks,
            &group.globals,
            &group.fns,
        );
    }

    // The signature tables, for the modules that live inside another member.
    let tables = rocflight::builtin::signature_tables();
    writer.count(tables.len());
    for (module, signatures) in &tables {
        put_signature_table(&mut writer, module, signatures);
    }

    let blob = writer.finish(source_hash(rocflight::builtin::SOURCE), written);
    let path = std::path::Path::new("src/roc/Builtin.artifact");
    std::fs::write(path, &blob).unwrap_or_else(|e| panic!("writing {}: {}", path.display(), e));
    eprintln!(
        "wrote {} members and {} compiled prefixes, {} bytes to {}",
        written,
        prefixes.len(),
        blob.len(),
        path.display()
    );
}

/// The smallest program that makes `needed_by` ask for exactly this selection.
fn forcing_program(selection: &[&str]) -> String {
    let mut body = String::from("app [main!] {}\n\nmain! = |_a| {\n");
    if selection.contains(&"Dict") {
        body.push_str("\t_d = Dict.empty()\n");
    }
    if selection.contains(&"Set") && !selection.contains(&"Dict") {
        body.push_str("\t_s = Set.empty()\n");
    }
    if selection.contains(&"Box") {
        body.push_str("\t_b = Box.box(1)\n");
    }
    if selection.contains(&"Stream") {
        body.push_str("\t_t = \"Stream\"\n");
    }
    body.push_str("\tOk({})\n}\n");
    body
}
