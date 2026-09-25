//! Reading a real platform's Roc sources.
//!
//! This reads what a platform's header declares — the `requires` entry-point type, the
//! `exposes` module list, the `hosted` symbol map, the `provides` entry, the link
//! recipe — and each exposed module's declared members with their signatures. It
//! implements none of the platform's effects: those are the host's compiled code,
//! reached by linking the interpreter into that host (`platform::driver`) and calling
//! through `platform::hosted`. Nothing a platform provides is re-implemented here.
//!
//! Shapes verified against basic-cli 0.22.0 on nightly-2026-09-03:
//!
//! ```text
//! platform ""
//!     requires { main! : ... => Try({}, [Exit(I32), ..]) }
//!     exposes [Cmd, Env, ..., Stdout, ...]
//!     packages { http: "https://..." }
//!     provides { "roc_main": main_for_host! }
//!     hosted { "hosted_stdout_line": Host.stdout_line!, ... }
//! ```
//!
//! An exposed module declares its members inside an opaque type's method block:
//!
//! ```text
//! Stdout :: [].{
//!     line! : Str => Try({}, [StdoutErr(IOErr), ..])
//!     line! = |message| ...
//! }
//! ```

use std::path::PathBuf;

/// A platform read from its cached sources.
#[derive(Debug, Clone)]
pub struct RealPlatform {
    /// The dependency alias the app gave it (`cli` in `cli: platform "..."`).
    pub alias: String,
    /// Directory holding the extracted `.roc` modules.
    pub sources: PathBuf,
    /// Module names from the root's `exposes [...]`.
    pub exposes: Vec<String>,
    /// The entry-point signature from `requires { main! : ... }`, verbatim.
    pub requires: Option<String>,
    /// The root's `hosted { "symbol": Host.member!, … }` block, in its order — which
    /// is the host's dispatch order — as `(symbol, member)`.
    pub hosted: Vec<(String, String)>,
    /// The function `provides { "roc_main": main_for_host! }` names: what the host
    /// calls, defined in Roc at the bottom of the platform's `main.roc`.
    pub provides: Option<String>,
}

/// One member an exposed module declares, with its signature as written.
#[derive(Debug, Clone, PartialEq)]
pub struct Member {
    pub name: String,
    pub signature: String,
}

impl RealPlatform {
    /// Read the platform rooted at `sources`.
    pub fn read(alias: &str, sources: PathBuf) -> Result<Self, String> {
        let root = sources.join("main.roc");
        let text = std::fs::read_to_string(&root)
            .map_err(|e| format!("Cannot read platform root {}: {}", root.display(), e))?;

        Ok(RealPlatform {
            alias: alias.to_string(),
            sources,
            exposes: parse_exposes(&text),
            requires: parse_requires(&text),
            hosted: parse_hosted(&text),
            provides: parse_provides(&text),
        })
    }

    /// Does the platform expose this module?
    pub fn exposes_module(&self, name: &str) -> bool {
        self.exposes.iter().any(|m| m == name)
    }

    /// Read an exposed module's declared members.
    ///
    /// Fails when the module is not exposed, so `import cli.Nope` is reported against
    /// the platform's own list rather than as a missing file.
    pub fn read_module(&self, name: &str) -> Result<Vec<Member>, String> {
        if !self.exposes_module(name) {
            return Err(format!(
                "The platform `{}` does not expose `{}`. It exposes: {}",
                self.alias,
                name,
                self.exposes.join(", ")
            ));
        }
        let path = self.sources.join(format!("{}.roc", name));
        let text = std::fs::read_to_string(&path)
            .map_err(|e| format!("Cannot read {}: {}", path.display(), e))?;
        Ok(parse_members(&text))
    }
}

/// Extract `exposes [A, B, C]` from a platform root.
fn parse_exposes(text: &str) -> Vec<String> {
    let Some(start) = text.find("exposes") else {
        return Vec::new();
    };
    let after = &text[start + "exposes".len()..];
    let Some(open) = after.find('[') else {
        return Vec::new();
    };
    let Some(close) = after[open..].find(']') else {
        return Vec::new();
    };
    after[open + 1..open + close]
        .split(',')
        .map(|m| m.trim())
        .filter(|m| !m.is_empty())
        .map(str::to_string)
        .collect()
}

/// Extract the entry-point signature from `requires { main! : ... }`.
fn parse_requires(text: &str) -> Option<String> {
    let start = text.find("requires")?;
    let after = &text[start..];
    let open = after.find('{')?;
    // Brace-matched, because the signature itself contains `{}`.
    let mut depth = 0;
    let mut end = None;
    for (i, c) in after[open..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    end = Some(open + i);
                    break;
                }
            }
            _ => {}
        }
    }
    let inner = after[open + 1..end?].trim();
    Some(inner.split_whitespace().collect::<Vec<_>>().join(" "))
}

/// The Roc after the platform header in `main.roc`: its imports and the function
/// `provides` names. Parsed as a module of its own, it is what the host's `roc_main`
/// runs — the exit-code mapping is the platform's Roc, not rocflight's.
///
/// The header is `platform ""` and its indented entries; the tail starts at the first
/// unindented line after it.
pub fn entry_source(text: &str) -> String {
    let mut lines = text.lines();
    for line in lines.by_ref() {
        if line.starts_with("platform") {
            break;
        }
    }
    let tail: Vec<&str> = lines
        .skip_while(|l| l.is_empty() || l.starts_with(['\t', ' ']) || l.starts_with('#'))
        .collect();
    tail.join("\n")
}

/// The link recipe for `target` from `targets: { x64musl: { inputs: [...] } }`, with
/// the platform's files as names and the `app` slot as `"app"`.
pub fn link_inputs(text: &str, target: &str) -> Option<Vec<String>> {
    let line = text.lines().find(|l| l.trim_start().starts_with(&format!("{}:", target)))?;
    let list = &line[line.find('[')? + 1..line.rfind(']')?];
    Some(
        list.split(',')
            .map(|item| item.trim().trim_matches('"').to_string())
            .filter(|item| !item.is_empty())
            .collect(),
    )
}

/// The entry point `provides { "roc_main": main_for_host! }` names.
fn parse_provides(text: &str) -> Option<String> {
    let line = text.lines().find(|l| l.trim_start().starts_with("provides"))?;
    let after = &line[line.find(':')? + 1..];
    let name = after.trim().trim_end_matches('}').trim().trim_end_matches(',');
    (!name.is_empty()).then(|| name.to_string())
}

/// Extract `(symbol, member)` pairs from `hosted { "symbol": Host.member!, }`.
fn parse_hosted(text: &str) -> Vec<(String, String)> {
    let Some(start) = text.find("hosted") else {
        return Vec::new();
    };
    let after = &text[start..];
    let Some(open) = after.find('{') else {
        return Vec::new();
    };

    let mut depth = 0;
    let mut end = after.len();
    for (i, c) in after[open..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    end = open + i;
                    break;
                }
            }
            _ => {}
        }
    }

    after[open..end]
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            let (symbol, member) = line.strip_prefix('"')?.split_once("\": ")?;
            Some((symbol.to_string(), member.trim().trim_end_matches(',').to_string()))
        })
        .collect()
}

/// Extract `name : signature` declarations from a module.
///
/// Only annotations INSIDE the module's method block count. `Stdout.roc` closes its
/// `Stdout :: [].{ ... }` block and then defines `widen_stdout_err` below it — a
/// private helper, not part of the module's surface. Scanning the whole file would
/// export it.
///
/// Only the annotation lines are read: the definitions are Roc code this interpreter
/// would have to run, and for a `hosted` effect there is nothing to run anyway.
fn parse_members(text: &str) -> Vec<Member> {
    let body = method_block(text).unwrap_or(text);
    let mut members = Vec::new();
    for line in body.lines() {
        let trimmed = line.trim();
        // Skip comments and anything that is not `name : type`.
        if trimmed.starts_with('#') {
            continue;
        }
        let Some(colon) = trimmed.find(" : ") else {
            continue;
        };
        let name = trimmed[..colon].trim();
        if name.is_empty()
            || !name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '!')
            || name.starts_with(|c: char| c.is_uppercase())
        {
            continue;
        }
        let signature = trimmed[colon + 3..].trim();
        if signature.is_empty() {
            continue;
        }
        members.push(Member { name: name.to_string(), signature: signature.to_string() });
    }
    members
}

/// The contents of a module's `Name :: ....{ ... }` method block.
///
/// Returns `None` when the file has no such block, in which case the caller falls back
/// to the whole file — older modules used a `module [...]` header and top-level
/// definitions instead.
fn method_block(text: &str) -> Option<&str> {
    let open_marker = text.find(".{")?;
    // Must be introduced by `Name ::`, not some unrelated `.{`.
    if !text[..open_marker].contains("::") {
        return None;
    }
    let start = open_marker + 2;

    let mut depth = 1;
    for (i, c) in text[start..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&text[start..start + i]);
                }
            }
            _ => {}
        }
    }
    None
}

/// Read a platform from a dependency URL, if `roc` has fetched it, or from a path.
pub fn load(alias: &str, url: &str, app_dir: &std::path::Path) -> Result<RealPlatform, String> {
    let sources = super::resolve::dependency_dir(url, app_dir).ok_or_else(|| {
        format!(
            "Platform `{}` is not in roc's cache. Run `roc check` on this app once to \
             fetch it: {}",
            alias, url
        )
    })?;
    RealPlatform::read(alias, sources)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exposes_is_read_as_a_module_list() {
        let text = "platform \"\"\n\trequires { main! : A => B }\n\texposes [Cmd, Env, Stdout]\n";
        assert_eq!(parse_exposes(text), vec!["Cmd", "Env", "Stdout"]);
    }

    #[test]
    fn requires_survives_braces_in_the_signature() {
        // The entry-point type contains `{}`, so a naive scan to the first `}` truncates.
        let text = "platform \"\"\n\trequires {\n\t\tmain! : List(Str) => Try({}, [Exit(I32), ..])\n\t}\n";
        assert_eq!(
            parse_requires(text).as_deref(),
            Some("main! : List(Str) => Try({}, [Exit(I32), ..])")
        );
    }

    #[test]
    fn hosted_symbol_names_are_collected() {
        let text = "\thosted {\n\t\t\"hosted_stdout_line\": Host.stdout_line!,\n\t\t\"hosted_dir_list\": Host.dir_list!,\n\t}\n";
        assert_eq!(
            parse_hosted(text),
            vec![
                ("hosted_stdout_line".to_string(), "Host.stdout_line!".to_string()),
                ("hosted_dir_list".to_string(), "Host.dir_list!".to_string())
            ]
        );
    }

    #[test]
    fn the_header_tail_and_link_recipe_are_read() {
        let text = "platform \"\"\n\trequires { main! : A }\n\tprovides { \"roc_main\": main_for_host! }\n\ttargets: {\n\t\tx64musl: { inputs: [\"crt1.o\", \"libhost.a\", app, \"libc.a\"] },\n\t}\n\nimport Stderr\n\nmain_for_host! = |args| 0\n";
        assert_eq!(parse_provides(text).as_deref(), Some("main_for_host!"));
        assert_eq!(entry_source(text), "import Stderr\n\nmain_for_host! = |args| 0");
        assert_eq!(link_inputs(text, "x64musl").unwrap(), ["crt1.o", "libhost.a", "app", "libc.a"]);
        assert!(link_inputs(text, "x64mac").is_none());
    }

    #[test]
    fn members_come_from_annotation_lines() {
        let text = "Stdout :: [].{\n\t## doc\n\tline! : Str => Try({}, [StdoutErr(IOErr), ..])\n\tline! = |m| m\n}\n";
        let members = parse_members(text);
        assert_eq!(members.len(), 1);
        assert_eq!(members[0].name, "line!");
        assert_eq!(members[0].signature, "Str => Try({}, [StdoutErr(IOErr), ..])");
    }

    #[test]
    fn a_helper_outside_the_method_block_is_not_a_member() {
        // Stdout.roc really does this: the block closes, then a private helper follows.
        let text = "Stdout :: [].{\n\tline! : Str => Try({}, [])\n\tline! = |m| m\n}\n\nwiden : Try(a, []) -> Try(a, [])\nwiden = |r| r\n";
        let members = parse_members(text);
        assert_eq!(
            members.iter().map(|m| m.name.as_str()).collect::<Vec<_>>(),
            vec!["line!"],
            "a definition after the block is private"
        );
    }

    #[test]
    fn a_type_declaration_is_not_a_member() {
        // `IOErr : [...]` is a type, not a callable member.
        let text = "IOErr : [NotFound, Other(Str)]\nline! : Str => Try({}, [])\n";
        let members = parse_members(text);
        assert_eq!(members.iter().map(|m| m.name.as_str()).collect::<Vec<_>>(), vec!["line!"]);
    }
}

/// Check an app's dependencies and imports against the platforms they name.
///
/// Run before evaluation so a mistake is reported against the platform's own sources —
/// "the platform does not expose that" — rather than surfacing later as an unknown
/// name.
///
/// Returns the platforms that were loaded, so a caller can report what it found.
pub fn verify_app(
    dependencies: &[(String, String, bool)],
    imports: &[(String, String)],
    app_dir: &std::path::Path,
) -> Result<Vec<RealPlatform>, String> {
    let mut platforms = Vec::new();

    for (alias, spec, is_platform) in dependencies {
        // `roc: "nightly-..."` pins the compiler; it names nothing to fetch.
        if super::resolve::hash_from_url(spec).is_none() && !super::resolve::is_local(spec) {
            continue;
        }
        if !*is_platform {
            // A package supplies ordinary Roc code, which this interpreter would have
            // to evaluate. Not refused, just not loaded.
            continue;
        }
        platforms.push(load(alias, spec, app_dir)?);
    }

    for (alias, module) in imports {
        if alias.is_empty() {
            // `import Module` — a file beside the app, not a dependency.
            continue;
        }
        let Some(platform) = platforms.iter().find(|p| p.alias == *alias) else {
            // A package alias, or one the app never declared. The second is an error
            // worth naming; the first is simply not loaded.
            if !dependencies.iter().any(|(a, _, _)| a == alias) {
                return Err(format!("`import {}.{}` names no declared dependency", alias, module));
            }
            continue;
        };
        // Reading it validates that the platform exposes it.
        platform.read_module(module)?;
    }

    Ok(platforms)
}

/// Is `Module.member` declared by any loaded platform?
pub fn declares(platforms: &[RealPlatform], module: &str, member: &str) -> bool {
    platforms.iter().any(|p| {
        p.read_module(module)
            .map(|ms| ms.iter().any(|m| m.name == member))
            .unwrap_or(false)
    })
}

/// Effects the app's platforms declare, as `Module.member`.
///
/// Set once from `verify_app`'s result before evaluation, then only read. A declared
/// effect this interpreter cannot run is reported as a gap rather than as an unknown
/// name — the difference matters, because the platform really does provide it.
static DECLARED: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

/// Record every member the given platforms expose.
pub fn record_declared(platforms: &[RealPlatform]) {
    let mut names = Vec::new();
    for platform in platforms {
        for module in &platform.exposes {
            if let Ok(members) = platform.read_module(module) {
                for member in members {
                    names.push(format!("{}.{}", module, member.name));
                }
            }
        }
    }
    if let Ok(mut declared) = DECLARED.lock() {
        *declared = names;
    }
}

/// Does a loaded platform declare `Module.member`?
pub fn is_declared(module: &str, member: &str) -> bool {
    let key = format!("{}.{}", module, member);
    DECLARED.lock().map(|d| d.contains(&key)).unwrap_or(false)
}
