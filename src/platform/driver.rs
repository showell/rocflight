//! Running an app on its platform's compiled host.
//!
//! A platform's host is native code that cannot be called from this process (see
//! Learning.md §13), so `rocflight main.roc` on an app that names one does
//! what `roc` does: links an executable and runs that instead. The executable is
//! `librocflight_host.a` — this interpreter as the platform's `app` — linked by the
//! platform's own recipe, and it is per PLATFORM, not per app: the app's path goes
//! in as `ROCFLIGHT_APP` at exec time, so editing a `.roc` file never re-links.
//!
//! Only `x64musl` (Linux on x86-64, statically against musl) is linked, with `zig`'s
//! bundled lld. `ponytail:` mac and Windows are the same steps with their own recipe
//! from the `targets:` block and their own linker.

use std::path::{Path, PathBuf};
use std::process::Command;

use super::real::{self, RealPlatform};

/// The platform URL an app's header names, if any: `cli: platform "https://…"`.
pub fn platform_url(source: &str) -> Option<String> {
    let header = source.split("\n\n").next().unwrap_or(source);
    let at = header.find("platform \"")?;
    let rest = &header[at + "platform \"".len()..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// Where the library that becomes the platform's `app` is: named by `ROCFLIGHT_LIB`,
/// beside the binary, or `embedded` in it — the release binary carries its own (see
/// `build.rs`), extracted into the cache once so a copied binary needs nothing else.
fn library(embedded: &[u8]) -> Result<PathBuf, String> {
    if let Some(path) = std::env::var_os("ROCFLIGHT_LIB") {
        return Ok(PathBuf::from(path));
    }
    let beside = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("librocflight_host.a")));
    if let Some(path) = beside.filter(|p| p.exists()) {
        return Ok(path);
    }
    if !embedded.is_empty() {
        let dir = cache_root()?.join(format!("lib-{}-{}", env!("CARGO_PKG_VERSION"), embedded.len()));
        let path = dir.join("librocflight_host.a");
        if !path.exists() {
            std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {}", dir.display(), e))?;
            let tmp = dir.join("librocflight_host.a.tmp");
            std::fs::write(&tmp, embedded).map_err(|e| format!("{}: {}", tmp.display(), e))?;
            std::fs::rename(&tmp, &path).map_err(|e| format!("{}: {}", path.display(), e))?;
        }
        return Ok(path);
    }
    Err("this rocflight was built without the host library, and librocflight_host.a is not \
         beside it or named by ROCFLIGHT_LIB. Build it first, then rocflight: \
         cargo build -p rocflight-host --release --target x86_64-unknown-linux-musl && cargo build --release"
        .to_string())
}

fn zig() -> String {
    std::env::var("ZIG").unwrap_or_else(|_| "zig".to_string())
}

/// The cache directory: `$XDG_CACHE_HOME/rocflight` or `~/.cache/rocflight`.
fn cache_root() -> Result<PathBuf, String> {
    if let Some(xdg) = std::env::var_os("XDG_CACHE_HOME") {
        return Ok(PathBuf::from(xdg).join("rocflight"));
    }
    let home = std::env::var_os("HOME").ok_or("HOME is not set")?;
    Ok(PathBuf::from(home).join(".cache").join("rocflight"))
}

/// The executable for `url`'s platform, linking it first if this platform and this
/// library have not met before. `embedded` is the host library the binary carries,
/// or empty when it was built without one.
pub fn prepare(url: &str, app: &Path, embedded: &[u8]) -> Result<PathBuf, String> {
    if !cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        return Err("running on a platform's host is only linked for x86-64 Linux so far".into());
    }
    let (hash, sources) = if super::resolve::is_local(url) {
        local_platform(url, app)?
    } else {
        let hash = super::resolve::hash_from_url(url).ok_or_else(|| format!("`{}` names no package", url))?;
        let sources = super::resolve::sources_dir(url).ok_or_else(|| {
            format!("platform {} is not in roc's cache; run `roc check` on the app once to fetch it", hash)
        })?;
        (hash.to_string(), sources)
    };
    let lib = library(embedded)?;
    let lib_meta = std::fs::metadata(&lib).map_err(|e| format!("{}: {}", lib.display(), e))?;
    let modified = lib_meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // A new library or a new platform is a new executable; the same pair is reused.
    let key = format!("{}-{}-{}-{}", hash, env!("CARGO_PKG_VERSION"), lib_meta.len(), modified);
    let dir = cache_root()?.join(key);
    let exe = dir.join("app");
    if exe.exists() {
        return Ok(exe);
    }
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {}", dir.display(), e))?;
    link(&sources, &lib, &dir, &exe)?;
    Ok(exe)
}

/// A platform named by a path, `platform "cli/platform/main.roc"`: its directory,
/// beside the app, and a name for its linked executable. Nothing content-addresses a
/// local platform, so the name is its directory and the time its host library was
/// built: rebuilding the host links a new executable.
fn local_platform(url: &str, app: &Path) -> Result<(String, PathBuf), String> {
    use std::hash::{Hash, Hasher};
    let app_dir = app.parent().unwrap_or_else(|| Path::new("."));
    let dir = super::resolve::dependency_dir(url, app_dir).ok_or_else(|| format!("`{}` names no directory", url))?;
    let dir = std::fs::canonicalize(&dir).map_err(|e| format!("platform `{}`: {}", url, e))?;
    let built = std::fs::metadata(dir.join("targets").join("x64musl").join("libhost.a"))
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    dir.hash(&mut hasher);
    Ok((format!("local-{:016x}-{}", hasher.finish(), built), dir))
}

/// The platform's link recipe, `app` filled with the hosted table and the library.
fn link(sources: &Path, lib: &Path, dir: &Path, exe: &Path) -> Result<(), String> {
    let main = std::fs::read_to_string(sources.join("main.roc"))
        .map_err(|e| format!("{}: {}", sources.join("main.roc").display(), e))?;
    let inputs = real::link_inputs(&main, "x64musl")
        .ok_or("the platform's `targets:` block has no x64musl recipe")?;
    let platform = RealPlatform::read("platform", sources.to_path_buf())?;
    let target_dir = sources.join("targets").join("x64musl");

    // The dispatch table, from the platform's `hosted` block, in its order.
    let mut table = String::new();
    for (symbol, _) in &platform.hosted {
        table.push_str(&format!("void {}(void);\n", symbol));
    }
    table.push_str("static const char *const names[] = {\n");
    for (symbol, _) in &platform.hosted {
        table.push_str(&format!("  \"{}\",\n", symbol));
    }
    table.push_str("};\nstatic const void *const fns[] = {\n");
    for (symbol, _) in &platform.hosted {
        table.push_str(&format!("  (const void *){},\n", symbol));
    }
    table.push_str(
        "};\nunsigned rocflight_hosted_count(void) { return sizeof(names) / sizeof(names[0]); }\n\
         const char *rocflight_hosted_name(unsigned i) { return names[i]; }\n\
         const void *rocflight_hosted_fn(unsigned i) { return fns[i]; }\n",
    );
    let table_c = dir.join("hosted.c");
    let table_o = dir.join("hosted.o");
    std::fs::write(&table_c, table).map_err(|e| format!("{}: {}", table_c.display(), e))?;
    run(Command::new(zig())
        .args(["cc", "-target", "x86_64-linux-musl", "-O2", "-c", "-o"])
        .arg(&table_o)
        .arg(&table_c))?;

    // Linked with lld directly, as `roc` links with its embedded lld: the host's and
    // this library's Rust runtimes both define `rust_eh_personality`, and only the
    // linker itself takes --allow-multiple-definition.
    let tmp = dir.join("app.tmp");
    let mut ld = Command::new(zig());
    ld.args(["ld.lld", "-static", "--gc-sections", "--allow-multiple-definition", "-o"]).arg(&tmp);
    for input in &inputs {
        if input == "app" {
            ld.arg(&table_o).arg(lib);
        } else {
            ld.arg(target_dir.join(input));
        }
    }
    run(&mut ld)?;
    std::fs::rename(&tmp, exe).map_err(|e| format!("{}: {}", exe.display(), e))
}

fn run(command: &mut Command) -> Result<(), String> {
    let output = command
        .output()
        .map_err(|e| format!("cannot run `{}`: {}", command.get_program().to_string_lossy(), e))?;
    if output.status.success() {
        return Ok(());
    }
    Err(format!(
        "`{} {}` failed:\n{}",
        command.get_program().to_string_lossy(),
        command.get_args().map(|a| a.to_string_lossy()).collect::<Vec<_>>().join(" "),
        String::from_utf8_lossy(&output.stderr)
    ))
}

/// Replace this process with the platform executable running `app`.
#[cfg(unix)]
pub fn exec(exe: &Path, app: &Path, args: &[String]) -> Result<std::convert::Infallible, String> {
    use std::os::unix::process::CommandExt;
    let app = std::fs::canonicalize(app).map_err(|e| format!("{}: {}", app.display(), e))?;
    let error = Command::new(exe).args(args).env("ROCFLIGHT_APP", &app).exec();
    Err(format!("cannot run {}: {}", exe.display(), error))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_platform_url_is_read_off_the_header() {
        let src = "app [main!] {\n\tcli: platform \"https://x/y/HASH.tar.zst\",\n\troc: \"nightly\",\n}\n\nimport cli.Stdout\n";
        assert_eq!(platform_url(src).as_deref(), Some("https://x/y/HASH.tar.zst"));
        assert_eq!(platform_url("module []\n\nx = 1\n"), None);
        assert_eq!(platform_url("app [main!] {}\n\nmain! = |_| echo!(\"platform \\\"no\\\"\")\n"), None);
    }
}
