//! Resolving a platform or package URL to its on-disk sources.
//!
//! `roc` already downloads, verifies and extracts dependencies, so none of that is
//! reimplemented here — the interpreter reads what `roc` left behind. That is not
//! laziness for its own sake: `roc` is the authority on cache layout, hashing and
//! archive integrity, and duplicating it would mean a second implementation to keep in
//! step.
//!
//! Layout verified on nightly-2026-09-03:
//!
//! ```text
//! ~/.cache/roc/packages/<HASH>/            extracted .roc modules + the .tar.zst
//! ~/.cache/roc/packages/<HASH>.deps.json   kind, byte counts, transitive deps
//! ```
//!
//! `<HASH>` is the URL's filename with its archive extension removed, so a URL maps to
//! a directory with no network access and no hashing of our own.
//!
//! An older layout also exists (`packages/<host>/<org>/<repo>/releases/download/...`),
//! written by the Rust-era compiler. Both are checked, newest first.

use std::path::{Path, PathBuf};

/// Where `roc` keeps downloaded dependencies.
///
/// ponytail: `$HOME` only. `roc` also honours `XDG_CACHE_HOME`; add that if anyone
/// runs with it set.
pub fn cache_root() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache/roc/packages"))
}

/// The content hash a dependency URL points at.
///
/// `.../download/0.22.0/F1JVZ....tar.zst` gives `F1JVZ...`. Both `.tar.zst` (current)
/// and `.tar.br` (Rust-era) are recognised.
pub fn hash_from_url(url: &str) -> Option<&str> {
    let file = url.rsplit('/').next()?;
    for suffix in [".tar.zst", ".tar.br", ".tar.gz"] {
        if let Some(stem) = file.strip_suffix(suffix) {
            return Some(stem);
        }
    }
    None
}

/// The directory holding a dependency's extracted `.roc` sources.
///
/// Returns `None` when the URL is not a recognised archive, or when `roc` has not
/// fetched it yet — the caller reports that, rather than downloading, so the failure
/// names a fix the user can act on (`roc check` the app once).
pub fn sources_dir(url: &str) -> Option<PathBuf> {
    let root = cache_root()?;
    let hash = hash_from_url(url)?;

    // Current layout: content-addressed, flat.
    let flat = root.join(hash);
    if flat.is_dir() {
        return Some(flat);
    }

    // Rust-era layout: the URL's path mirrored under the cache, with the hash last.
    let path = url.strip_prefix("https://").or_else(|| url.strip_prefix("http://"))?;
    let mirrored = root.join(Path::new(path).parent()?).join(hash);
    if mirrored.is_dir() {
        return Some(mirrored);
    }

    None
}

/// A dependency named by a path rather than a URL: `cli: platform "cli/platform/main.roc"`.
pub fn is_local(spec: &str) -> bool {
    !spec.contains("://") && spec.ends_with(".roc")
}

/// The directory of a dependency's sources: roc's cache for a URL, and for a local one
/// the directory of its `main.roc`, relative to the app.
pub fn dependency_dir(spec: &str, app_dir: &Path) -> Option<PathBuf> {
    if is_local(spec) {
        app_dir.join(spec).parent().map(Path::to_path_buf)
    } else {
        sources_dir(spec)
    }
}

/// Does this URL name a dependency `roc` has already fetched?
pub fn is_cached(url: &str) -> bool {
    sources_dir(url).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_local_dependency_is_a_path_ending_in_roc() {
        assert!(is_local("cli/platform/main.roc"));
        assert!(is_local("../pkg/main.roc"));
        assert!(!is_local("nightly-2026-09-03-62fcb65"));
        assert!(!is_local("https://example.com/a/HASH.tar.zst"));
        assert!(!is_local("https://example.com/a/main.roc"));
        assert_eq!(dependency_dir("cli/platform/main.roc", Path::new("/app")), Some(PathBuf::from("/app/cli/platform")));
        assert_eq!(dependency_dir("/abs/plat/main.roc", Path::new("/app")), Some(PathBuf::from("/abs/plat")));
        assert_eq!(dependency_dir("nightly-2026-09-03-62fcb65", Path::new("/app")), None);
    }

    #[test]
    fn hash_comes_from_the_url_filename() {
        assert_eq!(
            hash_from_url(
                "https://github.com/roc-lang/basic-cli/releases/download/0.22.0/F1JVZ.tar.zst"
            ),
            Some("F1JVZ")
        );
        // The Rust-era compiler used brotli.
        assert_eq!(hash_from_url("https://example.com/a/b/XYZ.tar.br"), Some("XYZ"));
    }

    #[test]
    fn a_non_archive_url_has_no_hash() {
        assert_eq!(hash_from_url("https://example.com/not-an-archive"), None);
        assert_eq!(hash_from_url("nightly-2026-09-03-62fcb65"), None);
    }

    #[test]
    fn an_unfetched_dependency_resolves_to_nothing() {
        assert!(!is_cached(
            "https://github.com/roc-lang/basic-cli/releases/download/9.9.9/NOPE.tar.zst"
        ));
    }
}
