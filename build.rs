// The release binary carries `librocflight_host.a` — the interpreter built as a
// platform's `app`, for x86_64-unknown-linux-musl — so `rocflight main.roc` on a
// platform app works from a copied binary with nothing beside it. The driver
// extracts it into the cache on first use, the way `roc` extracts its own shim
// libraries (Learning.md §13).
//
// Build order is therefore: the host library first, then this binary:
//     cargo build -p rocflight-host --release --target x86_64-unknown-linux-musl
//     cargo build --release
// A build without the library still succeeds — tests, CI, a first build — and the
// driver then looks beside the binary or at ROCFLIGHT_LIB, and says so.
use std::path::PathBuf;

fn main() {
    check_artifact(include_str!("src/roc/Builtin.roc"));
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let lib = std::env::var_os("ROCFLIGHT_HOST_LIB")
        .map(PathBuf::from)
        .unwrap_or_else(|| manifest.join("target/x86_64-unknown-linux-musl/release/librocflight_host.a"));
    println!("cargo:rerun-if-env-changed=ROCFLIGHT_HOST_LIB");
    println!("cargo:rerun-if-changed={}", lib.display());
    let bytes = std::fs::read(&lib).unwrap_or_default();
    std::fs::write(out.join("librocflight_host.a"), bytes).expect("write embedded host library");

    // The vendored Builtin.roc's member boundaries, computed here rather than in every
    // process. `src/builtin.rs` includes the table.
    let builtin = manifest.join("src/roc/Builtin.roc");
    println!("cargo:rerun-if-changed={}", builtin.display());
    let source = std::fs::read_to_string(&builtin).expect("read src/roc/Builtin.roc");
    let rows = scan(&source);
    let generated = format!("{}\n{}", member_index(&rows), low_level_reach(&source, &rows));
    std::fs::write(out.join("builtin_index.rs"), generated)
        .expect("write the builtin member index");
}

/// `\tName :: …` or `\tName(a, b) :: …`, and nothing deeper.
///
/// Was `builtin::member_name`, run at run time over all 23,555 lines. It reads a
/// constant, so it belongs here: see `member_index`.
fn member_name(line: &str) -> Option<&str> {
    let rest = line.strip_prefix('\t')?;
    if rest.starts_with('\t') {
        return None;
    }
    let declared = rest.split(" :: ").next().filter(|d| *d != rest)?;
    let name = match declared.find('(') {
        Some(i) if declared.ends_with(')') => &declared[..i],
        _ => declared,
    };
    let mut chars = name.chars();
    if !chars.next().is_some_and(char::is_uppercase) || !chars.all(|c| c.is_alphanumeric() || c == '_')
    {
        return None;
    }
    Some(name)
}

/// Every member of `Builtin.roc`, as a byte range, written out as a static table.
///
/// This scan used to happen once per process, inside a `OnceLock` — and "once per
/// process" was still 0.9ms of every program that names a `Dict`, because it walks all
/// 700kB of the file. The file is a constant compiled into the binary, so its member
/// boundaries are a constant too. `Learning.md` §11, phase 1.
///
/// The gate is `tests/check_builtin.sh --strict`: a skewed offset makes a member fail
/// to parse, loudly, member by member.
fn scan(source: &str) -> Vec<(String, usize, usize, bool)> {
    let mut rows: Vec<(String, usize, usize, bool)> = Vec::new();
    let mut in_nominal = false;
    let mut offset = 0;
    for line in source.split_inclusive('\n') {
        let start = offset;
        offset += line.len();
        let line = line.strip_suffix('\n').unwrap_or(line);
        if let Some(name) = member_name(line) {
            in_nominal = true;
            if let Some(last) = rows.last_mut() {
                last.2 = start;
            }
            // The header line is part of the member: it is the declaration.
            rows.push((name.to_string(), start, offset, in_nominal));
        } else if in_nominal && line == "}" {
            // The `Builtin :: [].{ … }` nominal closes here. Everything after it is a
            // TOP-LEVEL section — the declarations with no body are the low-level ops
            // the real compiler injects. They are a member of nothing, so they get
            // their own slice, and they are already at column 0.
            in_nominal = false;
            if let Some(last) = rows.last_mut() {
                last.2 = start;
            }
            rows.push(("(low level)".to_string(), offset, offset, in_nominal));
        } else if let Some(last) = rows.last_mut() {
            last.2 = offset;
        }
    }
    rows
}

/// `scan`'s answer as the static table `src/builtin.rs` includes.
fn member_index(rows: &[(String, usize, usize, bool)]) -> String {
    let mut out = String::from("static MEMBERS: &[Slice] = &[\n");
    for (name, start, end, in_nominal) in rows {
        out.push_str(&format!(
            "    Slice {{ name: {:?}, start: {}, end: {}, in_nominal: {} }},\n",
            name, start, end, in_nominal
        ));
    }
    out.push_str("];\n");
    out
}

/// One member's source, as `builtin::members_where` reconstructs it.
///
/// The low-level section is outside `Builtin :: [].{ … }` so its lines keep their
/// indentation; a member inside it loses one tab.
fn member_source(source: &str, start: usize, end: usize, in_nominal: bool) -> String {
    let mut out = String::new();
    for line in source[start..end].lines() {
        out.push_str(if in_nominal { line.strip_prefix('\t').unwrap_or(line) } else { line });
        out.push('\n');
    }
    out
}

/// Which low-level declarations each member reaches, transitively.
///
/// `Dict` and `Set` use 48 of the low-level section's 317 declarations, and parsing the
/// other 269 was three milliseconds of every program that names a `Dict`. The pruning
/// itself then cost 0.75ms, because it closed over the text word by word and allocated
/// a `String` per word — over 2,300 lines, in every process. The words are a constant,
/// so the answer is: this writes it out per member and `builtin::reachable` unions the
/// lists of whatever is loaded. `Learning.md` §11, phase 1.
///
/// A declaration is reached when its name appears in the member's text or in a reached
/// declaration's own text, closed over. The walk is by word, so a name in a comment
/// reaches its declaration too — that costs a parse and never an answer.
fn low_level_reach(source: &str, rows: &[(String, usize, usize, bool)]) -> String {
    let Some((_, ll_start, ll_end, ll_nominal)) = rows.iter().find(|(n, ..)| n == "(low level)")
    else {
        return "static LOW_LEVEL_REACH: &[(&str, &[&str])] = &[];\n".to_string();
    };
    let low = member_source(source, *ll_start, *ll_end, *ll_nominal);

    // A block is a column-0 declaration and every line under it, up to the next.
    let mut blocks: Vec<(&str, usize, usize)> = Vec::new();
    let mut offset = 0;
    for line in low.split_inclusive('\n') {
        let start = offset;
        offset += line.len();
        let starts_block = line.starts_with(|c: char| c.is_alphabetic() || c == '_');
        if starts_block || blocks.is_empty() {
            let name = line
                .split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '!'))
                .next()
                .filter(|n| n.starts_with(|c: char| c.is_lowercase() || c == '_'))
                .unwrap_or("");
            if blocks.last().is_some_and(|(n, _, _)| *n == name && !name.is_empty()) {
                // The body under its own annotation: one block.
            } else {
                blocks.push((name, start, start));
            }
        }
        blocks.last_mut().expect("a block").2 = offset;
    }
    let words = |text: &str| {
        text.split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '!'))
            .filter(|w| !w.is_empty())
            .map(str::to_string)
            .collect::<Vec<_>>()
    };
    let by_name: std::collections::HashMap<&str, (usize, usize)> = blocks
        .iter()
        .filter(|(name, _, _)| !name.is_empty())
        .map(|&(name, start, end)| (name, (start, end)))
        .collect();

    let mut out = String::from("static LOW_LEVEL_REACH: &[(&str, &[&str])] = &[\n");
    for (name, start, end, in_nominal) in rows {
        if name == "(low level)" {
            continue;
        }
        let text = member_source(source, *start, *end, *in_nominal);
        let mut keep: Vec<&str> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let mut pending = words(&text);
        while let Some(word) = pending.pop() {
            let Some((&found, &(from, to))) = by_name.get_key_value(word.as_str()) else { continue };
            if seen.insert(found) {
                keep.push(found);
                pending.extend(words(&low[from..to]));
            }
        }
        keep.sort_unstable();
        out.push_str(&format!("    ({:?}, &{:?}),\n", name, keep));
    }
    out.push_str("];\n");
    out
}

/// The artifact must match the source it was made from.
///
/// `src/roc/Builtin.artifact` is `Builtin.roc` already parsed (see `src/artifact.rs`),
/// and a build script cannot call the crate it is building — so it cannot be generated
/// here. What CAN be done here is refuse to build a binary whose artifact has drifted,
/// which turns "a stale artifact silently answers wrongly" into a compile error. The
/// alternative, checking at run time, means hashing 700kB on every startup, which is
/// most of what the artifact saves.
fn check_artifact(source: &str) {
    println!("cargo:rerun-if-changed=src/roc/Builtin.artifact");
    let path = std::path::Path::new("src/roc/Builtin.artifact");
    // FNV-1a, the same as `artifact::source_hash`. Duplicated rather than shared
    // because this file cannot call the crate; the round-trip is that both read the
    // same eight lines.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in source.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    let fresh = |why: &str| {
        // A placeholder is SAFE: `load` reads it as "no members" and falls back to
        // parsing. So a missing artifact, or one from an older FORMAT, is a warning and
        // a rebuild away — where a HASH mismatch below is a hard error, because that is
        // the case where the trees no longer describe the source.
        let mut empty = Vec::from(*b"ROCFLT07");
        empty.extend_from_slice(&hash.to_le_bytes());
        empty.extend_from_slice(&0u32.to_le_bytes());
        empty.push(0);
        empty.push(0);
        std::fs::write(path, empty).expect("writing the placeholder artifact");
        println!("cargo:warning={}; run `cargo run --release --bin gen-artifact`", why);
    };
    let blob = match std::fs::read(path) {
        Ok(blob) => blob,
        Err(_) => {
            fresh("no builtin artifact");
            return;
        }
    };
    if blob.len() < 16 || &blob[..8] != b"ROCFLT07" {
        fresh("the builtin artifact is from an older format");
        return;
    }
    if u64::from_le_bytes(blob[8..16].try_into().expect("16 bytes")) != hash {
        panic!(
            "src/roc/Builtin.artifact is stale (or from another format version).\n\
             Regenerate it:  cargo run --release --bin gen-artifact"
        );
    }
}
