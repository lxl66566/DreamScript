//! Run-time resolution of the `conformance/` corpus for test binaries.
//!
//! The workspace shares one `CARGO_TARGET_DIR` across git worktrees, so a
//! test binary can outlive the worktree it was built in and then be served
//! fresh from another lane, its compile-time `env!("CARGO_MANIFEST_DIR")`
//! path dangling into the deleted checkout. Callers resolve the corpus at
//! run time instead: the compile-time checkout first (a live lane judges
//! its own corpus), then a walk up from the test binary (the shared target
//! dir sits under a checkout root, so the walk reaches that root's corpus).

use std::path::{Path, PathBuf};

/// The repository's `conformance/` directory, resolved at run time.
///
/// Panics when neither the compile-time checkout nor any ancestor directory
/// of the running binary holds the corpus (a `CARGO_TARGET_DIR` placed
/// outside every checkout).
#[must_use]
pub fn dir() -> PathBuf {
    resolve("../../conformance")
}

/// One file in the corpus: `file("types_hybrid.ds")`.
///
/// Panics like [`dir`] when nothing holds the file: the compile-time
/// checkout must carry the named file, not only the directory — a stale
/// cross-lane bake of another worktree's corpus lacks files this lane
/// added, so the file-level check keeps the fallback honest.
#[must_use]
pub fn file(name: &str) -> PathBuf {
    resolve(&format!("../../conformance/{name}"))
}

/// Resolves `rel`, a manifest-relative path in the tests'
/// `"../../conformance/<file>.ds"` spelling, at run time.
///
/// The walk-up fallback drops `rel`'s leading parent-directory climb (the
/// climb out of `crates/<crate>` to the repo root) and joins the remainder
/// against each ancestor of the binary, so the contract covers paths that
/// climb to the repo root. Panics like [`dir`] when nothing resolves.
#[must_use]
pub fn resolve(rel: &str) -> PathBuf {
    let baked = Path::new(env!("CARGO_MANIFEST_DIR")).join(rel);
    if baked.exists() {
        return baked;
    }
    let mut rest = Path::new(rel);
    while let Ok(tail) = rest.strip_prefix("..") {
        rest = tail;
    }
    let exe = std::env::current_exe().expect("the test binary knows its own path");
    for ancestor in exe.ancestors().skip(1) {
        let candidate = ancestor.join(rest);
        if candidate.exists() {
            return candidate;
        }
    }
    panic!(
        "conformance corpus unresolved: {} is gone and no ancestor of {} holds {}",
        baked.display(),
        exe.display(),
        rest.display(),
    );
}
