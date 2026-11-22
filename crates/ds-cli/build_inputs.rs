//! The build identity's input roots, shared between the build script
//! and the watch test: one list drives both what the digest folds and
//! what cargo watches, so the two sets can never drift apart.

use std::path::{Path, PathBuf};

/// The workspace crates whose code turns source text into module
/// bytes, ds-vm deliberately absent (it executes the artifact but
/// never shapes it) alongside the lsp/repl/fmt subcommands.
pub const BYTECODE_CRATES: [&str; 9] = [
    "ds-abi",
    "ds-check",
    "ds-meta",
    "ds-diag",
    "ds-lower",
    "ds-mod",
    "ds-parse",
    "ds-query",
    "ds-stdlib",
];

/// The ds-cli sources on the run path itself.
pub const RUN_PATH_FILES: [&str; 7] = [
    "main.rs",
    "run.rs",
    "run_cache.rs",
    "dsi_cache.rs",
    "source.rs",
    "ambient_dsmod.rs",
    "package.rs",
];

/// The generated artifact inside a digested tree: its own bytes fold
/// the identity, so hashing it here would chase a moving fixpoint.
/// The drift gate pins it against a fresh generation instead.
pub const GENERATED_ARTIFACT: &str = "ds-stdlib/src/ambient.dsmod";

/// One identity input: where it sits on disk, the workspace-relative
/// key the digest folds it under, the spelling cargo watches, and how
/// the reader treats it.
pub struct InputRoot {
    /// The file or tree whose bytes fold into the identity.
    pub path: PathBuf,
    /// The digest key: a tree folds every file under it as
    /// `key`-plus-relative path, a file folds its bytes as `key`.
    pub key: String,
    /// The `rerun-if-changed` spelling: package-root-relative, forward
    /// slashes, `..` segments where the input escapes the package.
    pub watch: String,
    /// How the root's bytes reach the digest.
    pub kind: RootKind,
}

/// How a root folds, which also decides how an absent root lands.
pub enum RootKind {
    /// A directory folded recursively; absence means the packaged
    /// build, where the identity degrades to the version alone.
    Tree,
    /// A single file folded under its key; absence means the packaged
    /// build too.
    File,
    /// A single file folded under its key; absence is a broken package
    /// build and must panic, never thin the identity.
    Required,
}

/// Every identity input root, in fold order: each bytecode crate's
/// source tree then its manifest (a feature flip recompiles different
/// code without touching any source file), the lock (a dependency
/// upgrade, the parser's grammar engine included, can change the
/// produced bytecode without any workspace source changing), then the
/// run path's own sources.
pub fn input_roots(manifest_dir: &Path) -> Vec<InputRoot> {
    let crates_dir = manifest_dir
        .parent()
        .expect("the package sits inside crates/");
    let workspace_dir = crates_dir.parent().expect("the crates dir has a parent");
    let mut roots = Vec::new();
    for crate_name in BYTECODE_CRATES {
        let package = crates_dir.join(crate_name);
        for (tail, kind) in [("src", RootKind::Tree), ("Cargo.toml", RootKind::File)] {
            let path = package.join(tail);
            roots.push(InputRoot {
                watch: relative(&path, manifest_dir),
                key: format!("{crate_name}/{tail}"),
                path,
                kind,
            });
        }
    }
    let lock = workspace_dir.join("Cargo.lock");
    roots.push(InputRoot {
        watch: relative(&lock, manifest_dir),
        key: "Cargo.lock".to_owned(),
        path: lock,
        kind: RootKind::File,
    });
    for file in RUN_PATH_FILES {
        let path = manifest_dir.join("src").join(file);
        roots.push(InputRoot {
            watch: relative(&path, manifest_dir),
            key: format!("ds-cli/src/{file}"),
            path,
            kind: RootKind::Required,
        });
    }
    roots
}

/// `path` spelled relative to `base`, forward slashes throughout.
/// Cargo resolves a `rerun-if-changed` path against the package root,
/// so a sibling crate's tree must be spelled `../ds-check/src` from
/// there; the crates-dir spelling `ds-check/src` watches a path under
/// ds-cli that never exists, cargo records the watch as absent, and a
/// warm target dir never reruns the build script when the sibling
/// changes — the identity silently goes stale.
pub fn relative(path: &Path, base: &Path) -> String {
    let mut path = path.components().peekable();
    let mut from = base.components().peekable();
    while path.peek().is_some_and(|part| from.peek() == Some(part)) {
        path.next();
        from.next();
    }
    let mut parts: Vec<String> = std::iter::repeat_n("..", from.count())
        .map(str::to_owned)
        .collect();
    parts.extend(path.map(|part| part.as_os_str().to_string_lossy().into_owned()));
    if parts.is_empty() { ".".to_owned() } else { parts.join("/") }
}
