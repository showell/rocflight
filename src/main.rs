#![forbid(unsafe_code)]

//! The CLI. `rocflight <file.roc>`; the pipeline itself is `run::run_file`, so a
//! platform's host can run the same thing from `roc_main`.

use std::env;
use std::error::Error;
use std::process;
use std::time::Duration;

/// No thread with a giant stack any more.
///
/// The tree-walker spent many Rust frames per Roc call and needed 256 MB reserved to
/// recurse a few hundred levels. The VM's call frames are a `Vec`, so Roc recursion
/// costs heap rather than stack and the default main-thread stack is plenty.
fn main() {
    cli();
}

/// The command surface, which is `roc`'s minus everything an interpreter has no
/// business doing: no `build`/`bundle`/`install`/`glue` (nothing is compiled to a
/// binary), no `fmt`/`docs`/`repl`/`lsp` (not a toolchain), and none of `roc`'s
/// options, which all configure codegen, caching or parallelism that rocflight
/// does not have.
const USAGE: &str = "\
Run the given .roc file

Usage: rocflight [ROC_FILE] [ARGS_FOR_APP]...
       rocflight <COMMAND>

Commands:
  test     Run all top-level `expect`s in a module
  eval     Print `Str.inspect` of a module's value, for roc's eval test runner (`--raw` for the Str itself)
  version  Print rocflight's version
  help     Print this message

Arguments:
  [ROC_FILE]         The .roc file to run [default: main.roc]
  [ARGS_FOR_APP]...  Arguments to pass into the app being run
";

/// What the debug build is additionally allowed to print.
///
/// These are development aids — they exist to inspect the pipeline, never to change
/// what a program does — so the release binary has no way to ask for them: see
/// `debug_flag`, which only recognises anything under `debug_assertions`.
#[derive(Default, Clone, Copy)]
struct Debug {
    show_desugared: bool,
    show_ast: bool,
    ast_only: bool,
    show_platforms: bool,
}

/// Recognise one development flag, reporting whether it was consumed.
///
/// The release build recognises none of them, so `rocflight --show-ast` there is an
/// unexpected argument like any other typo.
#[cfg(debug_assertions)]
fn debug_flag(arg: &str, dbg: &mut Debug) -> bool {
    match arg {
        "--show-desugared" => dbg.show_desugared = true,
        "--show-ast" => dbg.show_ast = true,
        "--ast-only" => {
            dbg.show_ast = true;
            dbg.ast_only = true;
        }
        "--show-platforms" => dbg.show_platforms = true,
        // A report about the vendored module rather than a switch on a run, so it
        // prints and leaves without ever opening a .roc file.
        "--builtins" | "--builtins=names" => {
            report_builtins(arg.ends_with("names"));
            process::exit(0);
        }
        _ => return false,
    }
    true
}

#[cfg(not(debug_assertions))]
fn debug_flag(_arg: &str, _dbg: &mut Debug) -> bool {
    false
}

/// The flags `debug_flag` accepts, listed under the usage text — in the debug build
/// only, so `rocflight help` describes the binary the reader is actually holding.
#[cfg(debug_assertions)]
const DEBUG_USAGE: &str = "
Development flags (debug builds only):
  --show-desugared  Print the desugared source before running
  --show-ast        Print the AST and its inferred type, then run
  --ast-only        Print the AST and its inferred type, do not run
  --show-platforms  Report each real platform the app resolves
  --builtins        Report the vendored Builtin.roc, member by member
  --builtins=names  ... and list every intrinsic it declares
";

#[cfg(not(debug_assertions))]
const DEBUG_USAGE: &str = "";

fn usage() {
    print!("{}{}", USAGE, DEBUG_USAGE);
}

fn cli() {
    let args: Vec<String> = env::args().skip(1).collect();

    // `test` is the one subcommand that goes on to run a file; the other two answer
    // and leave.
    let (test_mode, eval_mode, rest) = match args.first().map(String::as_str) {
        Some("help") | Some("-h") | Some("--help") => {
            usage();
            return;
        }
        Some("version") => {
            println!("rocflight {}", env!("CARGO_PKG_VERSION"));
            return;
        }
        Some("test") => (true, false, &args[1..]),
        Some("eval") => (false, true, &args[1..]),
        _ => (false, false, &args[..]),
    };

    let mut dbg = Debug::default();
    let mut filename = None;
    // `eval --raw`: a Str answer is printed as itself, not as `Str.inspect` would.
    let mut raw = false;
    // Everything after the file is the app's, as `roc app.roc -- a b` passes it on;
    // the `--` itself is optional here.
    let mut app_args: Vec<String> = Vec::new();
    for arg in rest {
        if filename.is_some() {
            if arg != "--" || !app_args.is_empty() {
                app_args.push(arg.clone());
            }
            continue;
        }
        if debug_flag(arg, &mut dbg) {
            continue;
        }
        if eval_mode && arg == "--raw" {
            raw = true;
            continue;
        }
        if arg.starts_with('-') {
            eprintln!("unexpected argument '{}'", arg);
            usage();
            process::exit(1);
        }
        filename = Some(arg.clone());
    }

    // `roc` defaults to main.roc when given no file, and so does rocflight.
    let filename = filename.unwrap_or_else(|| "main.roc".to_string());

    // An app on a real platform runs on that platform's compiled host: link it (once
    // per platform) and hand over. `test` and the debugging flags stay in-process —
    // a test never reaches an effect, and the flags are about this pipeline.
    let in_process = test_mode || eval_mode || dbg.show_desugared || dbg.show_ast || dbg.ast_only || dbg.show_platforms;
    if !in_process {
        if let Err(e) = launch_on_host(&filename, &app_args) {
            eprintln!("Error: {}", e);
            process::exit(1);
        }
    }

    if eval_mode {
        eval_for_harness(&filename, raw);
    }

    // Run the interpreter with proper error handling
    if let Err(e) = run(&filename, dbg, test_mode) {
        eprintln!("Error: {}", e);
        process::exit(1);
    }
}

/// `rocflight eval FILE`: one backend of roc's own eval test runner.
///
/// `roc-compiler/src/eval/test/parallel_runner.zig` runs every eval test through the
/// interpreter, the dev backend and wasm, and compares their `Str.inspect` strings.
/// With `--rocflight <binary>` it runs this too, over the same pipe protocol its forked
/// backends use: exit 0 with the inspect string on stdout, or exit 2 with an error NAME
/// on stdout — `Crash` is what a crash test expects, and `CompileError` is what a
/// problem test expects. Nothing else may reach stdout, and stderr is discarded.
///
/// `raw`: the runner's allocation tests compare a plain `Str`, not its inspection,
/// so `"ok"` has to come out as `ok`.
fn eval_for_harness(filename: &str, raw: bool) -> ! {
    let options = rocflight::run::Options { check_expects: true, inspect_result: !raw, ..Default::default() };
    match rocflight::run::run_file(filename, options) {
        // A failing `expect` is a compile-time problem in roc's evaluation of a
        // constant, and the harness expects to hear so.
        Ok(Some(_)) if rocflight::eval::expect_tally().1 > 0 || rocflight::eval::assert_failed() => {
            eprintln!("expect failed");
            println!("CompileError");
            process::exit(2);
        }
        Ok(Some(ran)) => {
            match (&ran.value, raw) {
                (rocflight::eval::value::Value::Str(text), true) => println!("{}", text),
                // Rendered inside the run, where a nominal `to_inspect` could dispatch.
                (_, false) => println!("{}", ran.inspected.as_deref().unwrap_or_default()),
                (value, _) => println!("{}", rocflight::eval::inspect(value)),
            }
            process::exit(0);
        }
        Ok(None) => process::exit(0),
        Err(e) => {
            let message = e.to_string();
            let name = if message.contains("Runtime error: crash") {
                "Crash"
            } else if message.contains("No match arm matched")
                || message.contains("Division by zero")
                || message.contains("rejects the literal")
            {
                // A case a top-level constant reaches that its match lacks: roc
                // finds it evaluating the constant, and reports a problem.
                "CompileError"
            } else if message.starts_with("Runtime error") {
                "RuntimeError"
            } else {
                "CompileError"
            };
            // The runner discards stderr; a person reading by hand gets the reason.
            eprintln!("{}", message);
            println!("{}", name);
            process::exit(2);
        }
    }
}

/// The interpreter as a platform's `app`, carried inside this binary (see `build.rs`);
/// empty when it was built without one.
static EMBEDDED_HOST_LIB: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/librocflight_host.a"));

/// If `filename` names a platform, exec the platform's executable on it. Returns
/// only when there is no platform to launch on.
fn launch_on_host(filename: &str, app_args: &[String]) -> Result<(), Box<dyn Error>> {
    let source = std::fs::read_to_string(filename)
        .map_err(|e| format!("cannot read `{}`: {}", filename, e))?;
    let Some(url) = rocflight::platform::driver::platform_url(&source) else {
        return Ok(());
    };
    let exe = rocflight::platform::driver::prepare(&url, EMBEDDED_HOST_LIB)?;
    rocflight::platform::driver::exec(&exe, std::path::Path::new(filename), app_args)?;
    Ok(())
}

/// Main interpreter pipeline with Result-based error handling
fn run(filename: &str, dbg: Debug, test_mode: bool) -> Result<(), Box<dyn Error>> {
    let options = rocflight::run::Options {
        emit_prefix: false,
        emit_codex: None,
        show_desugared: dbg.show_desugared,
        show_ast: dbg.show_ast,
        ast_only: dbg.ast_only,
        show_platforms: dbg.show_platforms,
        test_mode,
        args: None,
        host_entry: false,
        check_expects: false,
        inspect_result: false,
    };
    let Some(ran) = rocflight::run::run_file(filename, options)? else {
        return Ok(());
    };

    // `rocflight test` reports the `expect` tally the way `roc test` does.
    if test_mode {
        return report_tests(ran.elapsed);
    }
    // A module's own value is its output. An app's output comes from its effects, so
    // there is nothing to print.
    if !ran.is_app {
        println!("{}", ran.value);
    }
    // A failed `expect` inside a function body does not stop the program, but it does
    // make the run fail, as it does under `roc run`.
    if rocflight::eval::assert_failed() {
        process::exit(1);
    }
    Ok(())
}

/// Report the vendored `Builtin.roc`: what parses, and where its builtin boundary is.
///
/// A member with a BODY is ordinary Roc that rocflight will run once it loads the
/// module; a member with only an annotation is an intrinsic that Rust has to supply,
/// exactly as the real compiler's `BuiltinLowLevel.zig` supplies it from Zig. The split
/// is read off the source, so this list is generated rather than maintained.
#[cfg(debug_assertions)]
fn report_builtins(list_names: bool) {
    let (mut parsed, mut defined, mut intrinsics) = (0, 0, 0);
    let read = rocflight::builtin::read();
    for member in &read {
        match &member.error {
            Some(error) => {
                println!("  FAIL {:<10} {:>6} lines  {}", member.name, member.lines, error)
            }
            None => {
                parsed += 1;
                defined += member.defined.len();
                intrinsics += member.intrinsics.len();
                println!(
                    "  ok   {:<10} {:>6} lines  {:>4} defined  {:>4} intrinsic",
                    member.name,
                    member.lines,
                    member.defined.len(),
                    member.intrinsics.len()
                );
            }
        }
    }
    println!(
        "\n{} of {} members parse: {} definitions in Roc, {} intrinsics for Rust",
        parsed,
        read.len(),
        defined,
        intrinsics
    );
    if list_names {
        println!();
        for member in &read {
            for name in &member.intrinsics {
                println!("{}", name);
            }
        }
    }
}

/// Report the `expect` tally, the way `roc test` does.
///
/// Only top-level `expect`s are counted, and roc splits the two outcomes across the two
/// streams: the pass line goes to stdout, the failure report to stderr.
///
/// roc closes the line with ` in <d.d> ms.`, and appends ` (cached)` when every module
/// came from its cache. rocflight caches nothing between runs, so the run is never
/// cached and that suffix never applies.
fn report_tests(elapsed: Duration) -> Result<(), Box<dyn Error>> {
    let ms = elapsed.as_secs_f64() * 1000.0;
    let (ran, failed) = rocflight::eval::expect_tally();
    if failed == 0 {
        println!("All ({}) tests passed in {:.1} ms.", ran, ms);
    } else {
        eprintln!("Ran {} tests in {:.1} ms.:", ran, ms);
        eprintln!("    {} passed", ran - failed);
        eprintln!("    {} failed", failed);
        // Anything that would raise this count is a parse or type error, which
        // rocflight reports and exits on before it gets here.
        eprintln!("    0 compiler errors");
        process::exit(1);
    }
    Ok(())
}
