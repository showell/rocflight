//! The pipeline: file in, value out.
//!
//! What `rocflight file.roc` does, as a library call, so a platform's host can run
//! the same thing from `roc_main` (see `host/` and Learning.md §13). Printing,
//! the test tally and exit codes stay with the caller.

use std::error::Error;
use std::time::{Duration, Instant};

use crate::desugaring::Desugarer;
use crate::eval::value::Value;
use crate::parser::Parser;
use crate::types::{Type, TypeChecker};

/// How to run a file. The four `show_*` are debugging aids and default off.
#[derive(Default)]
pub struct Options {
    pub show_desugared: bool,
    pub show_ast: bool,
    pub ast_only: bool,
    pub show_platforms: bool,
    /// Compile the top-level `expect`s and do not run the entry point: `roc test`.
    pub test_mode: bool,
    /// What the entry point is called with — the host's `args`, when there is a host.
    pub args: Option<Value>,
    /// Run the platform's own entry point (`provides { "roc_main": main_for_host! }`)
    /// instead of the app's `main!`. Set by `roc_main`, inside the host, where the
    /// platform's Roc is what maps `main!`'s answer to an exit code.
    pub host_entry: bool,
    /// Also run the top-level `expect`s, and report a failing one the way roc's
    /// compile-time evaluation does. `rocflight eval` sets this.
    pub check_expects: bool,
    /// Render `Str.inspect` of the result while the program is still installed, so a
    /// nominal's own `to_inspect` steers it. `rocflight eval` (non-raw) sets this.
    pub inspect_result: bool,
    /// Compile, hand the precompiled GROUP to `PREFIX_OUT` and stop. `gen-artifact`
    /// sets this: it needs `Builtin.roc` compiled through the real front end, and the
    /// front end lives here.
    pub emit_prefix: bool,
}

thread_local! {
    /// Where `emit_prefix` leaves what it compiled. A thread-local rather than a return
    /// value because it is a generator-only path and `run_file` answers what a RUN
    /// answers; threading it through every caller would be worse than this.
    pub static PREFIX_OUT: std::cell::RefCell<
        Option<(crate::vm::Program, crate::vm::compile::Group, u32, u32)>,
    > = const { std::cell::RefCell::new(None) };
}

/// What a run produced.
pub struct Ran {
    /// The top level's value for a module; the entry point's result for an app.
    pub value: Value,
    /// Whether the file declared an entry point (an app) or is a module.
    pub is_app: bool,
    /// `Str.inspect` of the result, rendered inside the run when `inspect_result` was
    /// asked — the only place a top-level nominal `to_inspect` can be dispatched.
    pub inspected: Option<String>,
    pub elapsed: Duration,
}

/// Run one file through the whole pipeline.
///
/// `None` when `ast_only` stopped it before running. Otherwise what came out, for the
/// caller to print, tally or turn into an exit code — this decides none of that, so
/// the same pipeline serves `rocflight file.roc`, `rocflight test`, and `roc_main`
/// inside a platform's host.
pub fn run_file(filename: &str, options: Options) -> Result<Option<Ran>, Box<dyn Error>> {
    let Options { show_desugared, show_ast, ast_only, show_platforms, test_mode, args, host_entry, check_expects, inspect_result: _, emit_prefix } = options;
    // `roc test` times the whole invocation, compile included, not just the expects.
    let started = Instant::now();

    // Phase timing, when `ROCFLIGHT_TIME` asks for it: see `crate::tick`.
    let mut phase = Instant::now();
    // Step 1: Load and desugar file.
    //
    // The source is read and parsed on every run. Nothing is cached between runs, so
    // editing a .roc file always takes effect immediately.
    // Named, because the file may be the `main.roc` default that nobody typed.
    let source = std::fs::read_to_string(filename)
        .map_err(|e| format!("cannot read `{}`: {}", filename, e))?;
    // Which builtin members this file needs, read off the source before it is consumed.
    let needed = crate::builtin::needed_by(&source);
    let desugarer = Desugarer::new(source);
    let desugared = desugarer.desugar()?;

    // Optionally display desugared code
    if show_desugared {
        eprintln!("\n=== DESUGARED CODE ===");
        eprintln!("{}", desugared);
        eprintln!("=== END DESUGARED CODE ===\n");
    }

    crate::tick("desugar", &mut phase);
    // Step 2: Parse desugared code (includes AST building)
    let mut parser = Parser::named(filename, &desugared);
    let (ast, app_entry_point) = {
        let expr = parser.parse_expr()?;
        (expr, parser.app_entry_point())
    };
    crate::tick("parse", &mut phase);
    // Every node so far is the app's; modules parsed from here on are not.
    let app_nodes = crate::ast::node_count();

    // Paths in a roc file are relative to the file itself.
    let source_dir = std::path::Path::new(filename)
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."))
        .to_path_buf();

    // Step 2b: Resolve any real platform the app names.
    //
    // Done before evaluation so a bad import is reported against the platform's own
    // sources rather than surfacing later as an unknown name.
    let platforms = crate::platform::real::verify_app(
        parser.dependencies(),
        parser.imports(),
        &source_dir,
    )?;
    crate::platform::real::record_declared(&platforms);
    if show_platforms {
        for platform in &platforms {
            eprintln!(
                "[Platform] {} from {}",
                platform.alias,
                platform.sources.display()
            );
            eprintln!("[Platform]   exposes {} modules", platform.exposes.len());
            eprintln!("[Platform]   declares {} hosted effects", platform.hosted.len());
            if let Some(requires) = &platform.requires {
                eprintln!("[Platform]   requires {}", requires);
            }
        }
    }

    // Step 2b2: the vendored builtin module.
    //
    // Its members are compiled into the SAME program, ahead of everything else, so a
    // definition Builtin.roc writes in Roc — `Dict.insert`, `List.join` — is an
    // ordinary top-level function by the time the app runs, and dispatch finds it the
    // way it finds any `Type.method`. Its annotation-only members stay builtins and
    // land in Rust, which is the same split `BuiltinLowLevel.zig` makes.
    //
    // Which members load is read off the source, never chosen from the command line:
    // the module is part of the interpreter, so asking for a different set of it would
    // only be a way to run a program against a runtime that is not the real one.
    // `Builtin.roc` already COMPILED, when the artifact carries it for this selection.
    // Then nothing needs the builtin trees — only their declared types — and their
    // chunks are not rebuilt at all. See `artifact::Prefix`.
    let prefix = crate::builtin::compiled_prefix(&needed);
    let nodes_before = crate::ast::node_count() as u32;
    let builtins = match prefix.as_ref().and(crate::builtin::load_tables(&needed)) {
        Some(tables) => tables,
        None => crate::builtin::load(&needed)?,
    };
    // The checker will ask for these modules' declared types; `load` has just parsed
    // them with more context than a re-parse would have. See `seed_signatures`.
    crate::builtin::seed_signatures(&builtins);

    let builtin_nodes = (nodes_before, crate::ast::node_count() as u32);
    crate::tick("builtin::load", &mut phase);
    // Step 2c: modules — `import Hello exposing [hello]`, `import pkg.Text`, and every
    // module those import in turn.
    //
    // Each is an ordinary .roc file, found beside its importer, or in the directory of
    // the package its alias names. Its top level is compiled into the shared global
    // scope BEFORE the app's, dependencies before their dependents, so `Hello.hello`
    // is bound by the time the app runs; `exposing` then aliases the named ones so
    // they can be used bare.
    let packages = package_dirs(parser.dependencies(), &source_dir);
    let mut wanted: Vec<(std::path::PathBuf, Vec<String>)> = parser
        .local_modules()
        .iter()
        .map(|(path, exposed)| (source_dir.join(format!("{}.roc", path)), exposed.clone()))
        .collect();
    for (alias, module) in parser.imports() {
        if let Some(dir) = packages.get(alias) {
            wanted.push((dir.join(format!("{}.roc", module.replace('.', "/"))), Vec::new()));
        }
    }
    let mut loaded_modules = ModuleLoader { packages: &packages, seen: Default::default(), order: Vec::new() };
    for (file, _) in &wanted {
        loaded_modules.load(file)?;
    }
    // The app, parsed again knowing what its imports declare (see `ModuleLoader`).
    let imports: Vec<std::path::PathBuf> = wanted.iter().map(|(f, _)| f.clone()).collect();
    let (imported, imported_params) = loaded_modules.declared_by(&imports);
    let (ast, app_entry_point) = if imported.is_empty() {
        (ast, app_entry_point)
    } else {
        parser = Parser::named(filename, &desugared);
        parser.declare_imported(&imported, &imported_params);
        let expr = parser.parse_expr()?;
        let entry = parser.app_entry_point();
        (expr, entry)
    };
    let mut module_asts = Vec::new();
    let mut module_nominals: Vec<(&'static str, Type)> = Vec::new();
    let mut module_params: Vec<(String, Vec<u32>)> = Vec::new();
    let mut module_defaults: Vec<(String, Vec<(String, crate::ast::Expr)>)> = Vec::new();
    let mut module_where_methods: Vec<String> = Vec::new();
    let mut enclosing_owners: Vec<(String, String)> = parser.enclosing_owners().to_vec();
    for (file, module_ast, module_parser) in loaded_modules.order {
        enclosing_owners.extend(module_parser.enclosing_owners().iter().cloned());
        // The last segment is the type the module's method block hangs its names on:
        // `Dir/Hello` exposes them as `Hello.hello`.
        let type_name = file.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string();
        let exposed = wanted.iter().find(|(f, _)| *f == file).map(|(_, e)| e.clone()).unwrap_or_default();
        module_nominals.extend(module_parser.nominals().iter().cloned());
        module_params.extend(module_parser.nominal_params().iter().cloned());
        // A module's own `where` clauses promise dispatch on a generic parameter, the
        // same as the app's: `read : item -> U64 where [item.get : item -> U64]`.
        module_where_methods.extend(module_parser.where_methods());
        module_defaults.extend(module_parser.field_default_exprs().iter().cloned());
        module_asts.push((module_ast, type_name, exposed));
    }

    // Step 2d: the platform's own modules, as the Roc they are.
    //
    // `Stdout.line!` is Roc in the platform's `Stdout.roc`, calling `Host.stdout_line!`,
    // which `Host.roc` declares with a type and no body — `Builtin.roc`'s split, and
    // the same treatment: bodied members compile ahead of the app, and a bodiless one
    // dispatches to the host through `platform::hosted`. Only what the app imports
    // is loaded, with everything those modules import in turn.
    let mut platform_loaded = Vec::new();
    for platform in &platforms {
        let imported: Vec<String> = parser
            .imports()
            .iter()
            .filter(|(alias, _)| *alias == platform.alias)
            .map(|(_, module)| module.clone())
            .collect();
        let loaded = crate::platform::modules::load(platform, &imported, host_entry)?;
        let skipped = crate::platform::modules::register_hosted(platform, &loaded);
        if show_platforms {
            for module in &loaded.modules {
                eprintln!("[Platform] {} loaded module {}", platform.alias, module.name);
            }
            for gap in &skipped {
                eprintln!("[Platform] {} cannot lay out {}", platform.alias, gap);
            }
        }
        platform_loaded.push(loaded);
    }

    crate::tick("modules + platform", &mut phase);
    // Step 3: Type check
    let mut type_checker = TypeChecker::new();
    // Declarations first, so a name used before it is declared — or declared in a
    // module — resolves rather than standing as a placeholder.
    type_checker.declare_types(parser.nominals().iter().map(|(n, t)| (*n, t.clone())));
    type_checker.declare_types(module_nominals.iter().map(|(n, t)| (*n, t.clone())));
    type_checker.declare_nominal_params(parser.nominal_params().iter().cloned().chain(module_params.iter().cloned()));
    for loaded in &platform_loaded {
        type_checker.declare_types(loaded.nominals.iter().map(|(n, t)| (*n, t.clone())));
    }
    type_checker.allow_dispatch(parser.where_methods());
    type_checker.allow_dispatch(module_where_methods.clone());
    type_checker.declare_nominal_literals(parser.nominal_literals());
    type_checker.declare_defaults(parser.field_default_exprs());
    type_checker.declare_defaults(&module_defaults);
    type_checker.declare_enclosing_owners(enclosing_owners.iter().cloned());
    type_checker.declare_suffixed_literals(&parser.suffixed_literals());
    type_checker.declare_suffixed_nominals(parser.nominal_suffixes());
    type_checker.declare_overflowed_literals(parser.overflowed_literals());
    for loaded in &platform_loaded {
        type_checker.declare_signatures(loaded.signatures.iter().cloned());
        type_checker.declare_nominal_literals(&loaded.nominal_literals);
        for module in &loaded.modules {
            type_checker.predeclare(&module.ast);
            type_checker.synth(&module.ast)?;
        }
    }
    for (module_ast, type_name, exposed) in &module_asts {
        // Checked first so the app sees the module's names with their real types.
        type_checker.predeclare(module_ast);
        type_checker.synth(module_ast)?;
        // `import Foo exposing [bar]` — the bare name is `Foo.bar`, as the compiler
        // already treats it.
        type_checker.expose(type_name, exposed);
    }
    // Problems only the type parser can see — an extension alias applied to something
    // it cannot extend. `parse_type`'s own errors are swallowed on purpose.
    if let Some(problem) = parser.type_problems().first() {
        return Err(format!("Type error: {}", problem).into());
    }
    // A top-level constant roc folds at compile time and finds crashing — in the app
    // or in any module it imports.
    for (tree, is_program) in std::iter::once((&ast, true))
        .chain(module_asts.iter().map(|(m, ..)| (m, false)))
    {
        if let Some(problem) = type_checker.comptime_crash_problems(tree, is_program) {
            return Err(format!("Type error: {}", problem).into());
        }
    }
    if let Some(problem) = type_checker.declaration_problems() {
        return Err(format!("Type error: {}", problem).into());
    }
    type_checker.predeclare(&ast);
    let inferred = type_checker.synth(&ast)?;
    // A literal that does not fit the type it was given: refused, as roc refuses it.
    if let Some(problem) = type_checker.method_problems() {
        return Err(format!("Type error: {}", problem).into());
    }
    if let Some(problem) = type_checker.literal_problems() {
        return Err(format!("Type error: {}", problem).into());
    }
    if let Some(problem) = type_checker.polymorphic_problems() {
        return Err(format!("Type error: {}", problem).into());
    }
    // The platform's entry references the app's `main!`, so it comes after the app.
    for loaded in &platform_loaded {
        if let Some((module, _)) = &loaded.entry {
            type_checker.predeclare(&module.ast);
            type_checker.synth(&module.ast)?;
        }
    }

    // Both files of a golden pair should build the same AST and infer the same type:
    // they differ only in sugar. `tests/golden_ast_test.rs` is the gate on that;
    // `--ast-only` makes one pair comparable by eye.
    if show_ast {
        println!("{}", ast);
        println!(":: {}", inferred);
        // Which numerals nothing pinned, so a `Dec` that should have been an `I64`
        // can be traced to the literal that became it.
        let mut floating: Vec<usize> = type_checker
            .fractional_literals()
            .into_iter()
            .filter(|id| id.index() < app_nodes)
            .filter_map(crate::ast::offset_of)
            .collect();
        floating.sort_unstable();
        for offset in floating {
            let line = desugared[..offset.min(desugared.len())].matches('\n').count() + 1;
            let col = offset - desugared[..offset.min(desugared.len())].rfind('\n').map_or(0, |i| i + 1) + 1;
            eprintln!(":: fractional literal at {}:{}", line, col);
        }
    }
    if ast_only {
        return Ok(None);
    }

    crate::tick("type check", &mut phase);
    let ingested: Vec<(&'static str, String)> = parser
        .ingests()
        .iter()
        .map(|(name, path)| {
            let text = std::fs::read_to_string(source_dir.join(path))
                .map_err(|e| format!("cannot ingest `{}`: {}", path, e))?;
            Ok::<_, String>((
                crate::memory::string_pool::intern(name),
                text,
            ))
        })
        .collect::<Result<_, _>>()?;

    // Step 4: compile to bytecode and run it.
    //
    // Anything the compiler cannot lower is an error naming the construct. There is no
    // fallback interpreter to quietly take over, which is the point of there being one
    // engine: a program either compiles or says why.
    let unit = crate::vm::compile::Unit {
        // The builtins lead, and they are the group that gets its own chunk block —
        // the block a compiled prefix replaces. Zero when one was loaded: those
        // modules are not here at all.
        prefix_modules: if prefix.is_some() { 0 } else { builtins.len() },
        // A module's top level is compiled into the SAME program, ahead of the app's,
        // which is how `hello` from `import Hello exposing [hello]` ends up in scope.
        modules: builtins
            .iter()
            // A member's own method blocks already qualified its names — the AST binds
            // `Str.is_empty`, not `is_empty` — so there is nothing to hang them on and
            // nothing to expose bare.
            // Skipped entirely when the builtins arrived compiled: their chunks are
            // the program's prefix, so there is nothing here to compile.
            .filter(|_| prefix.is_none())
            .map(|loaded| crate::vm::compile::Module {
                ast: &loaded.ast,
                type_name: loaded.name,
                exposed: Vec::new(),
            })
            .chain(platform_loaded.iter().flat_map(|loaded| {
                // A platform module's `exposing [IOErr]` names a type, not a value;
                // its functions are always called qualified.
                loaded.modules.iter().map(|module| crate::vm::compile::Module {
                    ast: &module.ast,
                    type_name: module.name,
                    exposed: Vec::new(),
                })
            }))
            .chain(module_asts.iter().map(|(module_ast, type_name, exposed)| {
                crate::vm::compile::Module {
                    ast: module_ast,
                    // Through the pool, not `Box::leak`: a name that appears twice is
                    // one allocation, and a module imported twice leaks nothing.
                    type_name: crate::memory::string_pool::intern(type_name),
                    exposed: exposed
                        .iter()
                        .map(|name| crate::memory::string_pool::intern(name))
                        .collect(),
                }
            }))
            .chain(platform_loaded.iter().filter_map(|loaded| {
                loaded.entry.as_ref().map(|(module, _)| crate::vm::compile::Module {
                    ast: &module.ast,
                    type_name: module.name,
                    exposed: Vec::new(),
                })
            }))
            .collect(),
        app: &ast,
        // `roc test` runs the top-level `expect`s and NOTHING else: the entry point
        // does not run at all, which is why the test program has no entry.
        entry: if test_mode {
            None
        } else if host_entry {
            platform_loaded.iter().find_map(|l| l.entry.as_ref().map(|(_, name)| *name))
        } else {
            app_entry_point.as_deref()
        },
        ingested,
        // What the checker learned about each operator's operands, which is what lets
        // the compiler emit an integer-only opcode where it applies.
        integer_binops: type_checker.integer_binops(),
        dispatch_modules: type_checker.dispatch_modules(),
        binop_modules: type_checker.binop_modules(),
        dec_literals: type_checker.dec_literals(),
        f32_literals: type_checker.f32_literals(),
        u128_literals: type_checker.u128_literals(),
        conversions: type_checker.literal_conversions(),
        numeral_texts: parser.numeral_texts(),
        coerce_values: type_checker.coerce_values(),
        coerce_params: type_checker.coerce_params(),
        zero_sized_capacity: type_checker.zero_sized_capacity(),
        match_types: type_checker.match_types(),
        for_iter_calls: type_checker.for_iter_calls(),
        default_sites: type_checker.default_sites(),
        nominal_defaults: parser
            .field_default_exprs()
            .iter()
            .cloned()
            .chain(module_defaults.iter().cloned())
            .collect(),
        missing_fields: type_checker.missing_fields(),
        run_expects: check_expects,
        fractional_literals: type_checker.fractional_literals(),
        parse_targets: type_checker.json_parse_targets(),
        collect_targets: type_checker.collect_targets(),
        // Every nominal in scope, the app's and each loaded builtin member's: the VM
        // needs their shapes to tell whose method a value can have meant.
        nominals: builtins
            .iter()
            .flat_map(|b| b.nominals.iter().cloned())
            .chain(platform_loaded.iter().flat_map(|l| l.nominals.iter().cloned()))
            .chain(module_nominals.iter().cloned())
            .chain(parser.nominals().iter().cloned())
            .collect(),
        opaque_nominals: parser.opaque_nominals().to_vec(),
        intrinsics: builtins.iter().flat_map(|b| b.intrinsics.iter().copied()).collect(),
        enclosing_owners: enclosing_owners
            .iter()
            .map(|(inner, outer)| {
                let leak = |s: &String| -> &'static str { Box::leak(s.clone().into_boxed_str()) };
                (leak(inner), leak(outer))
            })
            .collect(),
        test_mode,
    };
    crate::tick("build the unit", &mut phase);
    // Node ids the builtins' own parse made, bracketing what a prefix must carry.
    let nodes_before = builtin_nodes.0;
    let nodes_after = builtin_nodes.1;
    let (compiled, group) = crate::vm::compile::compile_reporting(&unit, prefix)?;
    if emit_prefix {
        let group = group.ok_or("nothing to emit: this program loaded no builtins")?;
        PREFIX_OUT.with(|out| {
            *out.borrow_mut() = Some((compiled, group, nodes_before, nodes_after));
        });
        return Ok(None);
    }
    let program = std::rc::Rc::new(compiled);

    crate::tick("compile", &mut phase);
    if std::env::var_os("ROCFLIGHT_CODE").is_some() {
        for (i, chunk) in program.chunks.iter().enumerate() {
            eprintln!("--- chunk {} `{}` arity {} regs {}", i, chunk.name, chunk.arity, chunk.n_regs);
            for (pc, op) in chunk.code.iter().enumerate() {
                eprintln!("  {:>3}  {:?}", pc, op);
            }
            if !chunk.consts.is_empty() { eprintln!("  consts {:?}", chunk.consts); }
        }
    }
    // Step 5: run the top level, then the app's entry point if it declared one.
    //
    // Top-level `expect`s are compiled in only under `test`; an ordinary run skips
    // them the way `roc run` does, so this runs the declarations and the program.
    let (value, inspected) = if options.inspect_result {
        let (value, shown) = crate::vm::run_and_inspect(&program, args)?;
        (value, Some(shown))
    } else {
        (crate::vm::run_with_args(&program, args)?, None)
    };
    crate::tick("run", &mut phase);
    Ok(Some(Ran { value, is_app: app_entry_point.is_some(), inspected, elapsed: started.elapsed() }))
}

/// The directory of each package the app header names, by alias: `cdx: "./codex/main.roc"`
/// is `./codex`, relative to the app; a URL is the directory `roc` extracted it to, when
/// it has. Platforms are not packages here: `platform::real` loads those.
fn package_dirs(
    dependencies: &[(String, String, bool)],
    source_dir: &std::path::Path,
) -> std::collections::HashMap<String, std::path::PathBuf> {
    let mut dirs = std::collections::HashMap::new();
    for (alias, spec, is_platform) in dependencies {
        if *is_platform {
            continue;
        }
        let dir = if spec.contains("://") {
            crate::platform::resolve::sources_dir(spec)
        } else {
            source_dir.join(spec).parent().map(|d| d.to_path_buf())
        };
        if let Some(dir) = dir {
            dirs.insert(alias.clone(), dir);
        }
    }
    dirs
}

/// Loads a module and, first, every module it imports: `order` ends up dependencies
/// first, each file once.
struct ModuleLoader<'a> {
    packages: &'a std::collections::HashMap<String, std::path::PathBuf>,
    seen: std::collections::HashSet<std::path::PathBuf>,
    order: Vec<(std::path::PathBuf, crate::ast::Expr, Parser)>,
}

impl ModuleLoader<'_> {
    fn load(&mut self, file: &std::path::Path) -> Result<(), Box<dyn Error>> {
        if !self.seen.insert(file.to_path_buf()) {
            return Ok(());
        }
        let text = std::fs::read_to_string(file)
            .map_err(|e| format!("cannot read module `{}`: {}", file.display(), e))?;
        let module_source = Desugarer::new(text).desugar()?;
        let module_source: &'static str = Box::leak(module_source.into_boxed_str());
        let mut module_parser = Parser::named(&file.display().to_string(), module_source);
        let mut module_ast = module_parser.parse_expr()?;
        // A module's own imports: a bare one is beside it, a qualified one is in the
        // package its alias names.
        let dir = file.parent().unwrap_or_else(|| std::path::Path::new("."));
        let mut deps: Vec<std::path::PathBuf> = module_parser
            .local_modules()
            .iter()
            .map(|(path, _)| dir.join(format!("{}.roc", path)))
            .collect();
        for (alias, module) in module_parser.imports() {
            if let Some(pkg) = self.packages.get(alias) {
                deps.push(pkg.join(format!("{}.roc", module.replace('.', "/"))));
            }
        }
        for dep in &deps {
            self.load(dep)?;
        }
        // Parsed again knowing what its imports declare, so an imported type
        // written with arguments keeps them.
        let (types, params) = self.declared_by(&deps);
        if !types.is_empty() {
            module_parser = Parser::named(&file.display().to_string(), module_source);
            module_parser.declare_imported(&types, &params);
            module_ast = module_parser.parse_expr()?;
        }
        self.order.push((file.to_path_buf(), module_ast, module_parser));
        Ok(())
    }

    /// The types these loaded modules declare, and their parameters.
    fn declared_by(&self, files: &[std::path::PathBuf]) -> (Vec<(&'static str, Type)>, Vec<(String, Vec<u32>)>) {
        let mut types = Vec::new();
        let mut params = Vec::new();
        for (file, _, parser) in &self.order {
            if files.contains(file) {
                types.extend(parser.nominals().iter().cloned());
                params.extend(parser.nominal_params().iter().cloned());
            }
        }
        (types, params)
    }
}
