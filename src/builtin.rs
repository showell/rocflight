//! The vendored builtin module.
//!
//! `src/roc/Builtin.roc` is a verbatim copy of the roc compiler's own
//! `src/build/roc/Builtin.roc` — the Roc source that defines `Str`, `List`, `Dict`,
//! `Set`, `Num`, `Iter` and the `Json` encoding. Re-sync it when the pinned nightly
//! moves:
//!
//! ```text
//! cp roc-compiler/src/build/roc/Builtin.roc src/roc/Builtin.roc && tests/check_builtin.sh
//! ```
//!
//! The boundary between what rocflight must write in Rust and what it gets for free is
//! read off that source rather than maintained by hand: **a member with a body is Roc,
//! a member with only an annotation is an intrinsic.** That is the same contract the
//! real compiler honours — `canonicalize/BuiltinLowLevel.zig` rewrites exactly the
//! annotation-only members into lambdas running a `LowLevel` op.

use crate::ast::Expr;
use crate::desugaring::Desugarer;
use crate::parser::Parser;

/// The vendored source, compiled in so a run needs no file beside the binary.
pub const SOURCE: &str = include_str!("roc/Builtin.roc");

/// One member of `Builtin :: [].{ … }`, lifted out to stand on its own.
pub struct Member {
    pub name: &'static str,
    /// The member's source, de-indented by one tab so it is a top-level declaration.
    pub source: String,
    pub lines: usize,
}

/// Split the vendored source into its top-level members.
///
/// Parsing the file whole reports ONE error and hides the other 23,000 lines, so it is
/// read a member at a time. They are exactly the declarations at one tab of indent.
pub fn members() -> Vec<Member> {
    members_where(|_| true)
}

/// The members `wanted` accepts, keeping the source of only those.
///
/// Loading one member should not cost building the text of the other eleven: the file
/// is 23,555 lines, and that showed up as two milliseconds and 700kB on every run of
/// every program.
pub fn members_where(wanted: impl Fn(&str) -> bool) -> Vec<Member> {
    MEMBERS
        .iter()
        .filter(|slice| wanted(slice.name))
        .map(|slice| {
            let mut member = Member { name: slice.name, source: String::new(), lines: 0 };
            for line in SOURCE[slice.start..slice.end].lines() {
                let text = if slice.in_nominal { line.strip_prefix('\t').unwrap_or(line) } else { line };
                member.source.push_str(text);
                member.source.push('\n');
                member.lines += 1;
            }
            member
        })
        .collect()
}

/// Where one member's lines sit in `SOURCE`.
struct Slice {
    name: &'static str,
    start: usize,
    end: usize,
    /// Inside `Builtin :: [].{ … }`, so its lines carry one tab to strip.
    in_nominal: bool,
}

// Every member's byte range — `MEMBERS`, a table computed by `build.rs`.
//
// This was a scan of all 23,555 lines behind a `OnceLock`, and once per process was
// still 0.9ms of every program that names a `Dict`. `Builtin.roc` is a constant
// compiled into the binary, so where its members begin and end is a constant too:
// `build.rs::member_index` writes the table out and this includes it. The scanner, and
// `member_name` with it, moved there. `tests/check_builtin.sh --strict` is the gate,
// because a skewed offset makes a member fail to parse.
include!(concat!(env!("OUT_DIR"), "/builtin_index.rs"));

/// What reading one member told us.
pub struct Read {
    pub name: &'static str,
    pub lines: usize,
    /// The parse error, if it did not parse. This reports PARSING only; whether a
    /// member also type-checks is `check_builtin.sh`'s second number.
    pub error: Option<String>,
    /// Members with a body: ordinary Roc, which rocflight can run once it loads them.
    pub defined: Vec<&'static str>,
    /// Members with an annotation and no body: what Rust has to supply.
    pub intrinsics: Vec<&'static str>,
}

/// Read every member, reporting what parsed and where its builtin boundary falls.
pub fn read() -> Vec<Read> {
    members()
        .into_iter()
        .map(|member| {
            let mut read = Read {
                name: member.name,
                lines: member.lines,
                error: None,
                defined: Vec::new(),
                intrinsics: Vec::new(),
            };
            match Desugarer::new(member.source).desugar() {
                Err(e) => read.error = Some(e.to_string()),
                Ok(desugared) => {
                    let mut parser = Parser::new(&desugared);
                    match parser.parse_expr() {
                        Err(e) => read.error = Some(e.to_string()),
                        Ok(ast) => {
                            read.defined = definitions(&ast);
                            read.intrinsics =
                                parser.intrinsics().to_vec();
                        }
                    }
                }
            }
            read
        })
        .collect()
}

/// The names a parsed member BINDS, walking the `Let` spine its top level is.
///
/// That covers both shapes Builtin.roc uses: a nominal's methods, which `parse_expr`
/// wraps around the program as `Type.method` bindings, and the plain top-level
/// functions of the low-level section. `_` is a statement, not a definition.
fn definitions(ast: &crate::ast::Expr) -> Vec<&'static str> {
    let mut names = Vec::new();
    let mut cursor = ast;
    while let crate::ast::Expr::Let { name, body, .. } = cursor {
        if *name != "_" {
            names.push(*name);
        }
        cursor = body;
    }
    names
}

/// A member parsed and ready to compile into a program.
pub struct Loaded {
    pub name: &'static str,
    pub ast: crate::ast::Expr,
    /// The BARE names this member declares with a type and no body — the low-level ops.
    /// A qualified intrinsic (`Str.concat`) already routes through the builtin path on
    /// its module name; these have no module, so the compiler has to be told.
    pub intrinsics: Vec<&'static str>,
    /// `Type.method` to its declared type, for every annotation the member carries.
    pub signatures: Vec<(&'static str, crate::types::Type)>,
    /// The nominals this member declares, as `(name, backing)`.
    pub nominals: Vec<(&'static str, crate::types::Type)>,
}

/// Parse the named members so they can be compiled ahead of the user's file.
///
/// The names are the ones `read` reports. An unknown name, or one whose member does not
/// parse, is an error rather than a silent omission — loading half a module would leave
/// its definitions resolving to whatever Rust happens to answer, which is exactly the
/// drift this whole exercise is meant to stop.
///
/// The result is NOT type-checked. `roc` already checked this source, rocflight's
/// checker is weaker than the language Builtin.roc is written in, and the app's own
/// checking is unaffected either way because these names reach it as builtins already.
/// `ponytail: trusted input, checked upstream; revisit when P3 makes the annotations
/// the type table.`
/// `Builtin.roc`, parsed at build time. See `crate::artifact` and `build.rs`.
static ARTIFACT_BYTES: &[u8] = include_bytes!("roc/Builtin.artifact");

/// While GENERATING an artifact, there is no artifact.
///
/// Every reader below goes through `artifact()`, so one switch covers all of them —
/// the trees, the signatures and the bytecode. Without it, generation reads the
/// previous artifact and the next one is built from the last: circular, and not
/// reproducible, because a table read back installs no nodes where parsing it would
/// have, so every node id after it shifts. `tests/check_artifact.sh` caught that twice,
/// once per reader that was added.
static GENERATING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn generating(yes: bool) {
    GENERATING.store(yes, std::sync::atomic::Ordering::Relaxed);
}

fn artifact() -> Option<&'static crate::artifact::Artifact> {
    if GENERATING.load(std::sync::atomic::Ordering::Relaxed) {
        return None;
    }
    static OPENED: std::sync::OnceLock<Option<crate::artifact::Artifact>> =
        std::sync::OnceLock::new();
    OPENED.get_or_init(|| crate::artifact::Artifact::open(ARTIFACT_BYTES)).as_ref()
}

/// How a member is named in the artifact.
///
/// Every member but one parses the same however it was reached. The low-level section
/// does not: it is cut down to what the OTHER selected members use, so `Dict` alone and
/// `Dict` beside `Stream` are two different parses. There are only four ways it can be
/// reached — `needed_by` adds it with `Dict` and `Set`, and `Box` and `Stream` may join
/// them — so it is stored once per way, under a key that names them.
pub fn artifact_key_for(member: &str, selected: &[&str]) -> String {
    artifact_key(member, selected)
}

fn artifact_key(member: &str, selected: &[&str]) -> String {
    if member != "(low level)" {
        return member.to_string();
    }
    let mut key = String::from(member);
    for other in ["Box", "Dict", "Set", "Stream"] {
        if selected.contains(&other) {
            key.push('|');
            key.push_str(other);
        }
    }
    key
}

/// Every selection `needed_by` can produce that includes the low-level section, so the
/// generator knows which cut-downs to store.
pub fn artifact_selections() -> Vec<Vec<&'static str>> {
    let mut all = Vec::new();
    for box_ in [false, true] {
        for stream in [false, true] {
            let mut one = vec!["(low level)", "Dict", "Set"];
            if box_ {
                one.push("Box");
            }
            if stream {
                one.push("Stream");
            }
            all.push(one);
        }
    }
    // The members that can be reached without the low-level section at all.
    all.push(vec!["Box"]);
    all.push(vec!["Stream"]);
    all
}

/// Parse the named members from source, keeping each one's node range so an artifact
/// can be written. `gen-artifact` and the round-trip test are the only callers;
/// `load` reads the artifact and only falls back to this.
pub fn parse_members(selected: &[&str]) -> Result<Vec<(Loaded, u32, Vec<u32>)>, String> {
    parse_from_source(selected)
}

/// The compiled builtins for this selection, if the artifact carries them.
///
/// When it does, nothing needs the builtin TREES: their bytecode is already made, and
/// the checker wants only the tables. See `artifact::Prefix`.
pub fn compiled_prefix(selected: &[&str]) -> Option<crate::artifact::Prefix> {
    if selected.is_empty() {
        return None;
    }
    let mut step = std::time::Instant::now();
    let artifact = artifact()?;
    let key = prefix_key(selected);
    let found = artifact.has_prefix(&key).then(|| artifact.prefix(&key))?;
    crate::tick("builtin bytecode", &mut step);
    found
}

/// A selection names one compiled prefix, whatever order it arrived in.
pub fn prefix_key(selected: &[&str]) -> String {
    let mut key = String::new();
    for member in ["(low level)", "Box", "Dict", "Set", "Stream"] {
        if selected.contains(&member) {
            if !key.is_empty() {
                key.push('|');
            }
            key.push_str(member);
        }
    }
    key
}

/// The members' TABLES — declared types and intrinsic names — without their trees.
///
/// What the checker needs when `compiled_prefix` answered, which is every run that has
/// an artifact. Decoding the trees as well is 0.33ms on a `Dict` program spent on
/// something nothing will read.
pub fn load_tables(selected: &[&str]) -> Option<Vec<Loaded>> {
    let artifact = artifact()?;
    let mut out = Vec::with_capacity(selected.len());
    for name in selected {
        let member = artifact.member_tables(&artifact_key(name, selected))?;
        out.push(Loaded {
            name: interned_member_name(name),
            ast: Expr::Unit(crate::ast::fresh_node_unlocated()),
            intrinsics: member.intrinsics,
            signatures: member.signatures,
            nominals: member.nominals,
        });
    }
    Some(out)
}

pub fn load(selected: &[&str]) -> Result<Vec<Loaded>, String> {
    // Loading nothing must touch nothing: `SOURCE` is 700kB of the binary, and merely
    // scanning it faults those pages in on every run of every program.
    if selected.is_empty() {
        return Ok(Vec::new());
    }
    let mut step = std::time::Instant::now();
    // The artifact holds these trees already. Every name in them is a slice of the
    // binary, so this reads the tree and allocates no text at all.
    if let Some(artifact) = artifact() {
        let keys: Vec<String> =
            selected.iter().map(|name| artifact_key(name, selected)).collect();
        if keys.iter().all(|key| artifact.has(key)) {
            let mut loaded = Vec::with_capacity(selected.len());
            for (name, key) in selected.iter().zip(&keys) {
                let member = artifact.member(key).ok_or_else(|| {
                    format!("builtin artifact: `{}` vanished between checks", key)
                })?;
                loaded.push(Loaded {
                    // The KEY names the cut-down; the member keeps its own name.
                    name: interned_member_name(name),
                    ast: member.ast.expect("`member` always decodes the tree"),
                    intrinsics: member.intrinsics,
                    signatures: member.signatures,
                    nominals: member.nominals,
                });
                crate::tick(format_args!("  {} from artifact", name), &mut step);
            }
            return Ok(loaded);
        }
    }
    let parsed = parse_from_source(selected)?;
    Ok(parsed.into_iter().map(|(loaded, _, _)| loaded).collect())
}

/// The `&'static str` the compiled tables key a member by.
fn interned_member_name(name: &str) -> &'static str {
    MEMBERS
        .iter()
        .find(|slice| slice.name == name)
        .map_or_else(|| crate::memory::string_pool::intern(name), |slice| slice.name)
}

fn parse_from_source(selected: &[&str]) -> Result<Vec<(Loaded, u32, Vec<u32>)>, String> {
    // Per-member timing, when `ROCFLIGHT_TIME` asks for it.
    let mut step = std::time::Instant::now();
    let mut sliced = members_where(|name| selected.contains(&name));
    crate::tick("slice + index", &mut step);
    // Whether any OTHER member is loaded, which is all the low-level section is cut
    // down for: on its own, every declaration of it is wanted.
    let alone = sliced.iter().all(|m| m.name == "(low level)");
    let mut loaded = Vec::with_capacity(selected.len());
    for name in selected {
        let at = sliced
            .iter()
            .position(|m| m.name == *name)
            .ok_or_else(|| format!("no builtin member named `{}`", name))?;
        let mut member = sliced.remove(at);
        if member.name == "(low level)" && !alone {
            member.source = reachable(&member.source, selected);
            crate::tick("  (low level) reachable", &mut step);
        }
        let desugared = Desugarer::new(member.source)
            .desugar()
            .map_err(|e| format!("builtin `{}`: {}", name, e))?;
        let first = crate::ast::node_count() as u32;
        let mut parser = Parser::new(&desugared);
        let ast = parser.parse_expr().map_err(|e| format!("builtin `{}`: {}", name, e))?;
        let last = crate::ast::node_count() as u32;
        crate::tick(format_args!("  {} parse", name), &mut step);
        let intrinsics = parser
            .intrinsics()
            .iter()
            .copied()
            .filter(|name| !name.contains('.'))
            .collect();
        let signatures = parser.signatures().to_vec();
        let nominals = parser.nominals().to_vec();
        loaded.push((
            Loaded { name: member.name, ast, intrinsics, signatures, nominals },
            first,
            crate::ast::offsets_between(first, last),
        ));
    }
    Ok(loaded)
}

/// The low-level section cut down to what the other loaded members reach.
///
/// `Dict` and `Set` use 48 of its 317 declarations, and parsing the other 269 was three
/// milliseconds of every program that names a `Dict`. WHICH 48 used to be worked out
/// here, by closing over the text word by word with a `String` per word — 0.75ms of
/// every such program, over 2,300 lines that never change. `build.rs::low_level_reach`
/// answers it per member now (`LOW_LEVEL_REACH`) and this unions the lists of whatever
/// is loaded; the union is exact because reaching is monotone, so the closure of two
/// members' words is the closure of each.
///
/// What is left is the block split, one pass over the lines. A block is a column-0
/// declaration and every line under it; an unnamed one — a blank line, a comment, a
/// capitalised type alias — is always kept.
fn reachable(source: &str, selected: &[&str]) -> String {
    let keep: std::collections::HashSet<&str> = LOW_LEVEL_REACH
        .iter()
        .filter(|(member, _)| selected.contains(member))
        .flat_map(|(_, names)| names.iter().copied())
        .collect();
    let mut out = String::with_capacity(source.len() / 4);
    // The name of the block being read, and whether it is being kept.
    let mut keeping = true;
    for line in source.split_inclusive('\n') {
        if line.starts_with(|c: char| c.is_alphabetic() || c == '_') {
            let name = line
                .split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '!'))
                .next()
                .filter(|n| n.starts_with(|c: char| c.is_lowercase() || c == '_'))
                .unwrap_or("");
            keeping = name.is_empty() || keep.contains(name);
        }
        if keeping {
            out.push_str(line);
        }
    }
    out
}

/// The members whose signatures the checker reads. NOT all of them, on a measurement.
///
/// `Dict` and `Set` had no type at all before this — `Dict(Str, U64)` came out of the
/// type parser as a fresh variable, which is why `BasicDict` failed with "Cannot
/// dispatch `insert` on an unresolved type". Reading their signatures costs nothing:
/// a program that never mentions a `Dict` never parses one.
///
/// `Str` and `List` were left out at first for cost, not correctness, and were later
/// let in: the annotation parse is what a program that calls `.map` or `.len` pays for
/// a real type. It is the single largest cost of a program that touches nothing else —
/// 0.57ms for `List`, 0.22ms for `Str` — and `annotations_only` plus the lazy cache
/// below is what keeps it to that. See `Learning.md` §11, phase 1.
///
/// All ten parsing members are verified to seed cleanly, with the golden pairs and the
/// examples green on any combination of them, so widening this is one edit whenever the
/// long tail beyond these four is worth its parse.
const TYPED_MEMBERS: &[&str] = &["Dict", "Set", "Str", "List"];

/// The `Type.method` signatures for one module, parsed on FIRST USE and kept.
///
/// Lazily, because seeding all four up front costs every program a parse it may not
/// need: a program that never mentions a `Dict` never pays for `Dict`, and one that
/// only concatenates strings pays 0.22ms for `Str` and nothing for `List`'s 0.57ms.
///
/// The module name is the member name here, which holds for everything in
/// `TYPED_MEMBERS`. It does not in general — `Json` lives in `Encoding` and `Try` in
/// `Box` — so widening that list means indexing modules to members first.
type SignatureTable =
    std::collections::HashMap<String, &'static [(&'static str, crate::types::Type)]>;
static CACHE: std::sync::OnceLock<std::sync::Mutex<SignatureTable>> = std::sync::OnceLock::new();

/// Remember a module's signatures, if nothing has answered for it yet.
fn cache_signatures(module: &str, table: &'static [(&'static str, crate::types::Type)]) {
    CACHE
        .get_or_init(Default::default)
        .lock()
        .expect("signature cache")
        .entry(module.to_string())
        .or_insert(table);
}

pub fn signatures_for(module: &str) -> &'static [(&'static str, crate::types::Type)] {
    // Checked before the cache, because the checker asks this of EVERY qualified name
    // it meets and most of them are not a member at all. Taking a lock to be told so
    // is the sort of cost that only shows up in a benchmark.
    if !TYPED_MEMBERS.contains(&module) {
        return &[];
    }
    let cache = CACHE.get_or_init(Default::default);

    if let Some(found) = cache.lock().expect("signature cache").get(module) {
        return found;
    }
    // The artifact holds these already, parsed at build time with the whole member in
    // scope — which is strictly more than the annotations-only re-parse below sees.
    // Read on DEMAND: only four modules are ever asked, so decoding every loaded
    // member's signatures up front decoded most of them into nothing.
    //
    // `Set` is the exception, and it is the reason this is not simply "read the
    // artifact": `Set(item) :: Dict(item, {})`, and the artifact's `Set` was parsed
    // with its own `Parser`, so its `Dict` is an unparameterised placeholder and `item`
    // is dropped. `parse_signatures` prepends `Dict`'s annotations for exactly that.
    if module != "Set" {
        if let Some(found) = artifact().and_then(|a| a.signatures_of(module)) {
            let table: Box<[(&'static str, crate::types::Type)]> = found
                .into_iter()
                .filter(|(name, _)| {
                    name.len() > module.len()
                        && name.starts_with(module)
                        && name.as_bytes()[module.len()] == b'.'
                })
                .map(|(name, ty)| (name, normalise(&ty)))
                .collect();
            let parsed: &'static [(&'static str, crate::types::Type)] = Box::leak(table);
            cache_signatures(module, parsed);
            return parsed;
        }
    }
    // Timed because this RE-PARSES a member `load` may already have parsed — the
    // measured 0.4ms of a `Dict` program and 1.4ms of a program that merely calls
    // `.map`. See `Learning.md` §11, phase 1.
    let mut step = std::time::Instant::now();
    let parsed: &'static [(&'static str, crate::types::Type)] =
        Box::leak(parse_signatures(module));
    crate::tick(format_args!("signatures_for({})", module), &mut step);
    cache_signatures(module, parsed);
    parsed
}

/// Hand `signatures_for` what `load` has already parsed, so it does not parse it again.
///
/// `signatures_for` re-parses a member's annotation lines to answer the checker, and on
/// a `Dict` program that is 0.35ms of a 3.1ms run — a second parse of a member `load`
/// parsed moments earlier. `load` sees strictly MORE than the re-parse does: the whole
/// member, with its own declarations in scope.
///
/// Strictly more for every member but one. `Set(item) :: Dict(item, {})`, and `load`
/// gives each member its own `Parser`, so `Set`'s `Dict` is an unparameterised
/// placeholder and `item` is dropped — `Set.from_list([...U64]).to_list()` came back a
/// list of unconstrained numbers when that was got wrong. That is exactly why
/// `parse_signatures` prepends `Dict`'s annotations for `Set`, so `Set` is left to it
/// and everything else is taken from here.
///
/// Seeding only, never overwriting: whatever asked first wins, as it did before.
pub fn seed_signatures(loaded: &[Loaded]) {
    for member in loaded {
        // Nothing to seed from a member read by `load_tables`, which does not decode
        // signatures — `signatures_for` reads that member's from the artifact on
        // demand. Seeding an empty table here would WIN, because the cache is
        // first-writer, and the checker would then believe `Dict` declares nothing.
        if member.signatures.is_empty() {
            continue;
        }
        if member.name == "Set" || !TYPED_MEMBERS.contains(&member.name) {
            continue;
        }
        let prefix = format!("{}.", member.name);
        let table: Box<[(&'static str, crate::types::Type)]> = member
            .signatures
            .iter()
            .filter(|(name, _)| name.starts_with(&prefix))
            .map(|(name, ty)| (*name, normalise(ty)))
            .collect();
        cache_signatures(member.name, Box::leak(table));
    }
}

fn parse_signatures(module: &str) -> Box<[(&'static str, crate::types::Type)]> {
    let Some(slice) = MEMBERS.iter().find(|s| s.name == module) else { return Box::new([]) };
    // `Set(item) :: Dict(item, {})` — so `Set`'s own signatures only carry their
    // element type if `Dict` is a known parameterised nominal while they are parsed.
    // Alone, `Dict(item, {})` degrades to a placeholder and `item` is dropped, which is
    // why `Set.from_list([...U64]).to_list()` came back a list of unconstrained numbers.
    // Parse `Dict`'s declaration first, then keep only `Set`'s own signatures.
    let source = if module == "Set" {
        let dict = MEMBERS
            .iter()
            .find(|s| s.name == "Dict")
            .map(|d| annotations_only(member_lines(d)))
            .unwrap_or_default();
        format!("{}\n{}", dict, annotations_only(member_lines(slice)))
    } else {
        annotations_only(member_lines(slice))
    };
    let Ok(desugared) = Desugarer::new(source).desugar() else {
        return Box::new([]);
    };
    let mut parser = Parser::new(&desugared);
    if parser.parse_expr().is_err() {
        return Box::new([]);
    }
    // One `Module.` prefix, not one per signature: `Num` declares 828 of them.
    let prefix = format!("{}.", module);
    parser
        .signatures()
        .iter()
        .filter(|(name, _)| name.starts_with(&prefix))
        .map(|(name, ty)| (*name, normalise(ty)))
        .collect()
}

/// One member's lines, as a top-level declaration: de-indented, straight off `SOURCE`.
///
/// `members_where` would do this too, but it builds the whole member as a `String`
/// first, and `annotations_only` then throws 92% of it away — 0.25ms to slice `List`'s
/// 1,676 lines so that 137 of them could be kept. Same lines, no copy.
fn member_lines(slice: &Slice) -> impl Iterator<Item = &'static str> + '_ {
    SOURCE[slice.start..slice.end]
        .lines()
        .map(move |line| if slice.in_nominal { line.strip_prefix('\t').unwrap_or(line) } else { line })
}

/// The member with its function BODIES removed.
///
/// Only the annotations are wanted, and `List` is 1,676 lines of which the signatures
/// are a small fraction — parsing the rest cost five milliseconds of every run that
/// touched a list. A body starts at `name = ` and runs until something at its own
/// indent or shallower appears.
fn annotations_only<'a>(lines: impl Iterator<Item = &'a str>) -> String {
    let mut kept = String::new();
    let mut body_indent: Option<usize> = None;
    for line in lines {
        let indent = line.len() - line.trim_start().len();
        if let Some(started) = body_indent {
            // A blank line does not end a body, nor does the closer that ends the
            // body's own block — dropping the opening brace and keeping the closing one
            // is what unbalanced eight golden pairs.
            let closes = matches!(line.trim_start().chars().next(), Some('}' | ')' | ']'));
            if line.trim().is_empty() || indent > started || (indent == started && closes) {
                continue;
            }
            body_indent = None;
        }
        let trimmed = line.trim_start();
        // Doc comments are most of the file and say nothing about a type.
        if trimmed.starts_with('#') {
            continue;
        }
        let is_body = trimmed
            .split_once(" = ")
            .is_some_and(|(name, _)| {
                !name.is_empty()
                    && name.starts_with(|c: char| c.is_lowercase() || c == '_')
                    && name.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '!')
            })
            || trimmed.ends_with(" =");
        if is_body {
            body_indent = Some(indent);
            continue;
        }
        kept.push_str(line);
        kept.push('\n');
    }
    kept
}

/// Bring a signature's types back to the ones rocflight actually uses.
///
/// Inside `Builtin.roc` a reference to `Str` resolves through that file's own
/// `Str :: [ProvidedByCompiler]` declaration, so it arrives as a nominal over an opaque
/// tag rather than as the string type. The same goes for `Bool`, the numbers and
/// `List`. Unifying one of those against a real `Type::Str` fails on the backing.
fn normalise(ty: &crate::types::Type) -> crate::types::Type {
    use crate::types::Type;
    match ty {
        Type::Nominal { name, backing, args } => match *name {
            "Str" => Type::Str,
            "Bool" => Type::Bool,
            "U8" => Type::U8, "U16" => Type::U16, "U32" => Type::U32,
            "U64" => Type::U64, "U128" => Type::U128,
            "I8" => Type::I8, "I16" => Type::I16, "I32" => Type::I32,
            "I64" => Type::I64, "I128" => Type::I128,
            "F32" => Type::F32, "F64" => Type::F64, "Dec" => Type::Dec,
            // `List(_item) :: [ProvidedByCompiler]` erases the element, so the most
            // that can be said is "a list of something".
            "List" => Type::List(Box::new(Type::TypeVar(u32::MAX))),
            _ => Type::Nominal { name: *name, backing: Box::new(normalise(backing)), args: args.iter().map(normalise).collect() },
        },
        Type::Function(a, b) => {
            Type::Function(Box::new(normalise(a)), Box::new(normalise(b)))
        }
        Type::List(inner) => Type::List(Box::new(normalise(inner))),
        Type::Optional(inner) => Type::Optional(Box::new(normalise(inner))),
        Type::Tuple(items) => Type::Tuple(items.iter().map(normalise).collect()),
        Type::Record { fields, open } => Type::Record {
            fields: fields.iter().map(|(f, t)| (*f, normalise(t))).collect(),
            open: *open,
        },
        Type::TagUnion { tags, open } => Type::TagUnion {
            tags: tags
                .iter()
                .map(|(t, args)| (*t, args.iter().map(normalise).collect()))
                .collect(),
            open: *open,
        },
        other => other.clone(),
    }
}

/// The members a program needs, from the names it mentions.
///
/// Loading `Dict` means parsing it, `Set` and the 2,305-line low-level section with it:
/// seventeen milliseconds against a three-millisecond baseline. A program that never
/// mentions a `Dict` should not pay that, and cannot need it — the only way to make one
/// is to name `Dict` or `Set`.
///
/// Over-loading is a cost, never a wrong answer: the word in a comment buys a parse
/// nobody reads. Under-loading is impossible for the same reason it is cheap to detect.
/// Every method name `Builtin.roc` declares, anywhere in it.
///
/// A name roc has NO method for is a name no program can call — `list.reverse()` is
/// `rev` spelled wrong, and roc reports it rather than running it. Scanned once from
/// the source's annotation lines (`name : type`) and kept.
pub fn declared_names() -> &'static std::collections::HashSet<&'static str> {
    static NAMES: std::sync::OnceLock<std::collections::HashSet<&'static str>> =
        std::sync::OnceLock::new();
    NAMES.get_or_init(|| {
        SOURCE
            .lines()
            .filter_map(|line| {
                let trimmed = line.trim_start();
                // `name : type` — a declaration, not a `name = value` binding, and not
                // a `::` type declaration.
                let (name, rest) = trimmed.split_once(':')?;
                if rest.starts_with(':') {
                    return None;
                }
                let name = name.trim_end();
                let ok = !name.is_empty()
                    && name.starts_with(|c: char| c.is_lowercase() || c == '_')
                    && name.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '!')
                    && line.len() - trimmed.len() > 0;
                // Borrowed from `SOURCE`, which is already `'static`: leaking a copy
                // of every one of them cost half a megabyte of peak memory.
                ok.then_some(name)
            })
            .collect()
    })
}

pub fn needed_by(source: &str) -> Vec<&'static str> {
    let mut wanted = Vec::new();
    // `Dict` and `Set` are inseparable — `Set(item) :: Dict(item, {})` — and both are
    // built out of the low-level section.
    //
    // Loading `Set` only when the source names it was tried, for the 0.35ms it costs a
    // `Dict` program, and REVERTED: it reads 1949 of 1953, failing the four
    // `issue 9725: … as a Dict key round-trips` tests. `Dict`'s own source never writes
    // the word `Set`, so the coupling is not textual — a structural key reaches `Set`'s
    // declarations some other way. Whatever that way is has to be understood before
    // this is narrowed, and understanding it is worth more than the 0.35ms.
    if source.contains("Dict") || source.contains("Set") {
        wanted.extend(["(low level)", "Dict", "Set"]);
    }
    // `Box` carries `Try`, so anything writing `Ok`, `Err` or `?` may reach it.
    if source.contains("Box") || source.contains("Try") {
        wanted.push("Box");
    }
    if source.contains("Stream") {
        wanted.push("Stream");
    }
    wanted
}
