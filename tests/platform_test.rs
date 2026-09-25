//! The outside world: platform loading, the host ABI, and the app entry point.

mod common;
use crate::common::*;

// ==========================================================================
// Real platform loading — phase 19.
//
// Verified against `roc` nightly-2026-09-03 and basic-cli 0.22.0:
//   * `roc` caches dependencies content-addressed at
//     `~/.cache/roc/packages/<HASH>/`, where `<HASH>` is the URL's filename with its
//     archive extension removed — so a URL maps to a directory with no network access
//   * the archive is `.tar.zst` now; the Rust-era compiler used `.tar.br`
//   * a platform root declares `requires`, `exposes`, `packages`, `provides`, `hosted`
//   * an exposed module declares its members inside `Name :: [].{ ... }`; anything
//     after that block is private
//
// The architectural limit: a platform's `hosted` functions live in its COMPILED HOST,
// which a tree-walking interpreter cannot call. Effects run only where this
// interpreter supplies its own implementation; the rest are reported as a gap.

use rocflight::platform::{real, resolve};

/// basic-cli 0.22.0 — the platform the roc-lang example uses.
const CLI: &str = "https://github.com/roc-lang/basic-cli/releases/download/0.22.0/F1JVZPYfWP71s8vk6tHcV1Qx1Ef6CZkwswGoCn8VHZmL.tar.zst";

fn cached() -> bool {
    resolve::is_cached(CLI)
}

// --- URL resolution (no cache needed) -------------------------------------

#[test]
fn a_url_maps_to_a_cache_directory_by_its_hash() {
    assert_eq!(resolve::hash_from_url(CLI), Some("F1JVZPYfWP71s8vk6tHcV1Qx1Ef6CZkwswGoCn8VHZmL"));
}

#[test]
fn both_archive_formats_are_recognised() {
    // `.tar.zst` is current; `.tar.br` is what the Rust-era compiler produced.
    assert_eq!(resolve::hash_from_url("https://x/y/AAA.tar.zst"), Some("AAA"));
    assert_eq!(resolve::hash_from_url("https://x/y/BBB.tar.br"), Some("BBB"));
}

#[test]
fn a_compiler_version_pin_is_not_a_fetchable_dependency() {
    // `roc: "nightly-2026-09-03-62fcb65"` names no archive.
    assert_eq!(resolve::hash_from_url("nightly-2026-09-03-62fcb65"), None);
}

// --- app header parsing ---------------------------------------------------

#[test]
fn the_app_header_records_platform_and_package_dependencies() {
    let src = "app [main!] {\n\tcli: platform \"https://x/A.tar.zst\",\n\tunicode: \"https://y/B.tar.zst\",\n\troc: \"nightly-2026-09-03\",\n}\n\nmain! = |_a| Ok({})\n";
    let mut parser = Parser::new(src);
    let _ = parser.parse_expr();

    let deps = parser.dependencies();
    assert_eq!(deps.len(), 3);
    assert_eq!(deps[0], ("cli".into(), "https://x/A.tar.zst".into(), true));
    assert_eq!(deps[1], ("unicode".into(), "https://y/B.tar.zst".into(), false));
    // The compiler pin is recorded like any other and filtered out by the loader.
    assert_eq!(deps[2], ("roc".into(), "nightly-2026-09-03".into(), false));
}

#[test]
fn imports_are_recorded_with_their_dependency_alias() {
    let src = "app [main!] { cli: platform \"https://x/A.tar.zst\" }\n\nimport cli.Stdout\nimport cli.Env exposing [var]\nimport Local\n\nmain! = |_a| Ok({})\n";
    let mut parser = Parser::new(src);
    let _ = parser.parse_expr();

    // A bare `import Local` has no alias: it is a file beside the app.
    assert_eq!(
        parser.imports(),
        &[
            ("cli".to_string(), "Stdout".to_string()),
            ("cli".to_string(), "Env".to_string()),
            (String::new(), "Local".to_string()),
        ]
    );
}

#[test]
fn the_first_import_survives_the_dependency_map() {
    // Regression: after parsing the header's `{ ... }` the cursor already sat on the
    // next line, and skipping to the next line again swallowed the first `import`.
    let src = "app [main!] { cli: platform \"https://x/A.tar.zst\" }\n\nimport cli.Stdout\n\nmain! = |_a| Ok({})\n";
    let mut parser = Parser::new(src);
    let _ = parser.parse_expr();
    assert_eq!(parser.imports().len(), 1, "the first import was dropped");
}

// --- reading a real platform (needs the cache) ----------------------------

#[test]
fn a_real_platform_root_is_read() {
    if !cached() {
        eprintln!("skipped: run `roc check` on tests/roc/19_platform/basic_cli.roc once");
        return;
    }
    let platform = real::load("cli", CLI, std::path::Path::new(".")).expect("should load");

    assert!(platform.exposes_module("Stdout"), "basic-cli exposes Stdout");
    assert!(platform.exposes.len() > 10, "exposes many modules");
    assert!(!platform.hosted.is_empty(), "declares hosted effects");

    // The `requires` signature contains `{}`, so a scan to the first `}` truncates it.
    let requires = platform.requires.as_deref().expect("requires clause");
    assert!(requires.starts_with("main! :"), "got {}", requires);
    assert!(requires.contains("Try("), "got {}", requires);
}

#[test]
fn an_exposed_modules_members_are_read_with_their_signatures() {
    if !cached() {
        return;
    }
    let platform = real::load("cli", CLI, std::path::Path::new(".")).expect("should load");
    let members = platform.read_module("Stdout").expect("Stdout is exposed");

    let line = members.iter().find(|m| m.name == "line!").expect("line! is declared");
    assert!(
        line.signature.starts_with("Str =>"),
        "the signature comes from the platform's own source, got {}",
        line.signature
    );
}

#[test]
fn a_private_helper_is_not_a_member() {
    if !cached() {
        return;
    }
    // Stdout.roc closes its method block and then defines `widen_stdout_err`.
    let platform = real::load("cli", CLI, std::path::Path::new(".")).expect("should load");
    let members = platform.read_module("Stdout").expect("Stdout is exposed");
    assert!(
        !members.iter().any(|m| m.name == "widen_stdout_err"),
        "a definition after the method block is private, got {:?}",
        members.iter().map(|m| &m.name).collect::<Vec<_>>()
    );
}

#[test]
fn importing_an_unexposed_module_names_what_is_available() {
    if !cached() {
        return;
    }
    let platform = real::load("cli", CLI, std::path::Path::new(".")).expect("should load");
    let err = platform.read_module("Nope").expect_err("Nope is not exposed");
    assert!(err.contains("does not expose"), "got {}", err);
    assert!(err.contains("Stdout"), "the message should list what IS exposed: {}", err);
}

// --- failure modes --------------------------------------------------------

#[test]
fn an_unfetched_platform_says_how_to_fetch_it() {
    let err = real::load(
        "cli",
        "https://github.com/roc-lang/basic-cli/releases/download/9.9.9/NOTFETCHED.tar.zst",
        std::path::Path::new("."),
    )
    .expect_err("not cached");
    assert!(err.contains("roc check"), "the error should name the fix: {}", err);
}

#[test]
fn an_import_naming_no_declared_dependency_is_rejected() {
    let deps = vec![("cli".to_string(), CLI.to_string(), true)];
    let imports = vec![("other".to_string(), "Thing".to_string())];
    let err = real::verify_app(&deps, &imports, std::path::Path::new(".")).expect_err("`other` was never declared");
    assert!(err.contains("no declared dependency"), "got {}", err);
}

#[test]
fn a_compiler_pin_is_skipped_rather_than_fetched() {
    // `roc: "nightly-..."` must not be treated as a platform to load.
    let deps = vec![("roc".to_string(), "nightly-2026-09-03".to_string(), false)];
    assert!(real::verify_app(&deps, &[], std::path::Path::new(".")).is_ok());
}

// ==========================================================================
// Every hosted function basic-cli declares has a call shape rocflight can make.
//
// The platform's `Host.roc` is the signatures, `main.roc`'s `hosted { … }` block is
// the symbols, and `IOErr.roc` and the `Internal*.roc` modules are the types those
// signatures name. All vendored under `tests/fixtures/basic-cli-0.22.0/`, so this
// runs without roc's cache. The gate is `abi::Plan::of` accepting all sixty.

use std::collections::HashMap;
use std::path::Path;

use rocflight::platform::abi::{Plan, Signature};
use rocflight::platform::layout::Declarations;
use rocflight::types::Type;

const FIXTURES: &str = "tests/fixtures/basic-cli-0.22.0";

/// Parse one platform module for what it declares: its signatures and its types.
fn declarations(file: &str) -> (Vec<(&'static str, Type)>, Vec<(&'static str, Type)>) {
    let path = Path::new(FIXTURES).join(file);
    let source = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {}", path.display(), e));
    let desugared = Desugarer::new(source).desugar().expect("desugars");
    let desugared: &'static str = Box::leak(desugared.into_boxed_str());
    let mut parser = Parser::named(&path.display().to_string(), desugared);
    parser.parse_expr().unwrap_or_else(|e| panic!("{}: {}", file, e));
    (parser.signatures().to_vec(), parser.nominals().to_vec())
}

#[test]
fn every_basic_cli_hosted_function_has_a_call_shape() {
    let (host_sigs, _host_types) = declarations("Host.roc");
    let mut decls: HashMap<String, Type> = HashMap::new();
    for file in ["IOErr.roc", "InternalHttp.roc", "InternalSqlite.roc", "InternalDateTime.roc", "Host.roc"] {
        let (_, types) = declarations(file);
        for (name, ty) in types {
            // `Host.NativeOsStr` and bare `NativeOsStr` are the same declaration.
            let bare = name.rsplit('.').next().unwrap_or(name);
            decls.insert(bare.to_string(), ty.clone());
            decls.insert(name.to_string(), ty);
        }
    }
    let decls = Declarations::new(decls);

    let main = std::fs::read_to_string(Path::new(FIXTURES).join("main.roc")).expect("main.roc");
    let hosted: Vec<(&str, &str)> = main
        .lines()
        .filter_map(|l| {
            // `"hosted_stdin_bytes": Host.stdin_bytes!,`
            let l = l.trim();
            let (symbol, member) = l.strip_prefix('"')?.split_once("\": ")?;
            Some((symbol, member.trim_end_matches(',')))
        })
        .collect();
    assert_eq!(hosted.len(), 60, "basic-cli 0.22.0 declares 60 hosted functions");

    let mut failures = Vec::new();
    for (symbol, member) in &hosted {
        let Some((_, ty)) = host_sigs.iter().find(|(n, _)| n == member) else {
            failures.push(format!("{}: `{}` has no signature in Host.roc", symbol, member));
            continue;
        };
        match Signature::of(ty, &decls).and_then(|sig| Plan::of(&sig)) {
            Ok(_) => {}
            Err(e) => failures.push(format!("{}: {} — {}", symbol, ty, e)),
        }
    }
    assert!(failures.is_empty(), "{} of {} cannot be called:\n  {}", failures.len(), hosted.len(), failures.join("\n  "));
}

// ==========================================================================
// The platformless-app entry-point model.
//
// Facts locked in here were verified against `roc` nightly-2026-09-03 and
// `roc experimental-lsp`, not assumed:
//   * `main! = |_args| { ... }` with no header is a valid app; the header
//     `app [main!] {}` is implied.
//   * `echo!` comes from the default host, and is `Str => {}`.
//   * The `!` is part of the identifier, so names are looked up verbatim.

fn entry_point_of(src: &str) -> Option<String> {
    let desugared = Desugarer::new(src.to_string()).desugar().unwrap();
    let mut parser = Parser::new(&desugared);
    parser.parse_expr().unwrap();
    parser.app_entry_point()
}

#[test]
fn headerless_app_implies_main_bang() {
    let entry = entry_point_of("main! = |_args| {\n    echo!(\"hi\")\n    Ok({})\n}\n");
    assert_eq!(entry.as_deref(), Some("main!"));
}

#[test]
fn explicit_empty_header_is_accepted() {
    // `app [main!] {}` has no platform string. The header parser must not go
    // looking for one.
    let entry = entry_point_of("app [main!] {}\n\nmain! = |_| {\n    echo!(\"hi\")\n    Ok({})\n}\n");
    assert_eq!(entry.as_deref(), Some("main!"));
}

#[test]
fn annotations_do_not_truncate_the_binding_chain() {
    // Regression: an unskipped `main! : ...` line ended the top-level chain, so
    // `main!` was never bound and lookup failed at run time.
    let src = "app [main!] {}\n\
               \n\
               greeting : Str\n\
               greeting = \"hello\"\n\
               \n\
               main! : List(Str) => Try({}, [Exit(I8), ..])\n\
               main! = |_args| {\n    echo!(greeting)\n    Ok({})\n}\n";
    let desugared = Desugarer::new(src.to_string()).desugar().unwrap();

    // The annotations survive desugaring...
    assert!(desugared.contains("greeting : Str"), "stripped annotation");
    assert!(desugared.contains("main! : List(Str)"), "stripped annotation");

    // ...and the parser still reaches the `main!` binding past them.
    let mut parser = Parser::new(&desugared);
    let ast = parser.parse_expr().unwrap();
    assert!(
        format!("{}", ast).contains("main!"),
        "binding chain truncated at the annotation: {}",
        ast
    );
}

#[test]
fn record_fields_are_not_mistaken_for_annotations() {
    // `name: value` (no space) is a record field; `name : Type` is an annotation.
    // Conflating them would silently delete record fields.
    let src = "app [main!] {}\n\nmain! = |_| {\n    r = 1\n    r\n}\n";
    assert_eq!(entry_point_of(src).as_deref(), Some("main!"));
}

#[test]
fn echo_is_a_host_effect_not_a_builtin() {
    use rocflight::platform::host;

    // Confirmed by LSP hover: `echo! : Str => {}`.
    assert_eq!(host::lookup("echo!"), Some((&["Str"][..], "{}")));
    // Dropping the `!` gives a different, unknown name.
    assert!(host::lookup("echo").is_none());
}

// ---------------------------------------------------------------------------
// Expressions must be parsed at full precedence wherever they can appear.
// Three places used a lower rung of the ladder and silently truncated:
// argument lists, top-level binding values, and block statements.
// ---------------------------------------------------------------------------


#[test]
fn call_can_be_an_argument_to_a_call() {
    // Regression: arguments were parsed with `parse_primary_expr`, a single atom,
    // so `inc(41)` inside `I64.to_str(...)` stopped after `inc` and the stray `(`
    // failed the closing-paren check.
    assert_eq!(value("inc = |x| x + 1\nI64.to_str(inc(41))"), "\"42\"");
}

#[test]
fn operator_can_be_an_argument_to_a_call() {
    assert_eq!(value("f = |x| x * 2\nI64.to_str(f(20 + 1))"), "\"42\"");
}

#[test]
fn top_level_binding_value_can_be_an_operator_expression() {
    // Regression: top-level binding values used `parse_call_expr`, which has no
    // operator handling, so `a = 2 + (3 * 4)` failed — while the same binding
    // inside a block worked. Only the desugared test files exposed this, because
    // they lift bindings to the top level.
    assert_eq!(value("a = 2 + (3 * 4)\na"), "14");
}

#[test]
fn block_statements_may_be_annotated() {
    // Regression: `skip_trivia` was not called inside blocks, so an annotation on
    // a block-local binding was parsed as an expression.
    let src = "main! = |_| {\n    n : I64\n    n = 21\n    n * 2\n}\n";
    let desugared = Desugarer::new(src.to_string()).desugar().unwrap();
    assert!(desugared.contains("n : I64"), "annotation stripped");
    let mut parser = Parser::new(&desugared);
    parser.parse_expr().expect("annotation inside block broke parsing");
}

#[test]
fn calling_a_non_function_is_still_an_error() {
    // Loosening the Call arm to unify instead of pattern-matching must not make
    // every callee acceptable. Now that identifiers have types, a named non-function
    // is caught too — `x = 42` then `x(1)` used to slip through.
    for src in ["42(1)", "\"hi\"(1)", "x = 42\nx(1)", "s = \"hi\"\ns(1)"] {
        let desugared = Desugarer::new(src.to_string()).desugar().unwrap();
        let mut parser = Parser::new(&desugared);
        let ast = parser.parse_expr().unwrap();
        let mut tc = rocflight::types::TypeChecker::new();
        assert!(tc.synth(&ast).is_err(), "{} should not type check", src);
    }
}

#[test]
fn identifiers_now_carry_their_type() {
    // This replaces a test that documented the opposite: identifiers used to synth to
    // a fresh type variable, so the checker knew nothing about them. With a type
    // environment, a name's type is available wherever it is used.
    use rocflight::types::TypeChecker;

    let typed = |src: &str| -> String {
        let desugared = Desugarer::new(src.to_string()).desugar().unwrap();
        let ast = Parser::new(&desugared).parse_expr().unwrap();
        let mut checker = TypeChecker::new();
        let ty = checker.synth(&ast);
        // Defaulted, because an unconstrained NUMERAL has no width until something
        // gives it one — and roc then makes it a `Dec`, printing `42.0`.
        ty.map(|t| checker.defaulted(&t).to_string()).unwrap_or_default()
    };

    assert_eq!(typed("x = 42\nx"), "Dec");
    assert_eq!(typed("n : I64\nn = 42\nn"), "I64");
    assert_eq!(typed("s = \"hi\"\ns"), "Str");
    assert_eq!(typed("b = Bool.True\nb"), "Bool");
}

#[test]
fn a_platform_named_by_a_path_is_loaded_from_beside_the_app() {
    // `pf: platform "plat/main.roc"` names a directory relative to the app, as a local
    // package does. Without it being resolved, the platform was skipped: its modules
    // were never read, and an import of one it lacks went unreported.
    let dir = std::env::temp_dir().join(format!("rocflight_local_platform_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("plat")).unwrap();
    std::fs::write(
        dir.join("plat/main.roc"),
        "platform \"\"\n\trequires {} { main! : List(Str) => Try({}, [Exit(I8), ..]) }\n\texposes [Echo]\n\tpackages {}\n\tprovides { \"roc_main\": main_for_host! }\n\thosted {\n\t\t\"roc_echo_line\": Echo.line!,\n\t}\n\nimport Echo\n\nmain_for_host! : List(Str) => I8\nmain_for_host! = |args|\n\tmatch main!(args) {\n\t\tOk(_) => 0\n\t\tErr(Exit(code)) => code\n\t}\n",
    )
    .unwrap();
    std::fs::write(dir.join("plat/Echo.roc"), "Echo := [].{\n\tline! : Str => {}\n}\n").unwrap();
    let deps = vec![("pf".to_string(), "plat/main.roc".to_string(), true)];
    let platforms = real::verify_app(&deps, &[("pf".to_string(), "Echo".to_string())], &dir).expect("loads");
    assert_eq!(platforms.len(), 1);
    assert!(real::declares(&platforms, "Echo", "line!"));
    let err = real::verify_app(&deps, &[("pf".to_string(), "Nope".to_string())], &dir).expect_err("no Nope");
    assert!(err.contains("Nope"), "got {}", err);
    let _ = std::fs::remove_dir_all(&dir);
}
