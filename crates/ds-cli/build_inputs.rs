//! The build identity's input roots and fold, shared between the
//! build script and the watch test: one list drives both what the
//! digest folds and what cargo watches, and the test re-runs the same
//! fold over the working tree, so the two sets can never drift apart
//! and a build script the watches never rerun cannot keep a stale
//! identity silently.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

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

/// The digest over every input root's bytes: one blake3 fold in root
/// order, each file framed by its key's length and bytes, the domain
/// tag first so the fold can never collide with a bare file digest.
/// Returns the hex digest, or `None` when a Tree or File root is
/// absent or unreadable — the packaged build, where the identity
/// degrades to the version alone. A missing Required root panics: the
/// identity must never go partial. The build script folds once and
/// bakes the result into `DS_BUILD_ID`; the watch test re-runs the
/// same fold over the working tree, so the baked value can only stay
/// green while it matches what a fresh fold of the current tree
/// produces — the comparison a clean checkout's byte gate reaches by
/// rebuilding from scratch.
pub fn fold_build_id(manifest_dir: &Path) -> Option<String> {
    let mut files: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    for root in input_roots(manifest_dir) {
        match root.kind {
            RootKind::Tree => collect_tree(&root.path, &root.key, &mut files)?,
            RootKind::File => {
                files.insert(root.key, fs::read(&root.path).ok()?);
            },
            // the run path's own sources; a missing one is a broken
            // package build, and the identity must never be partial
            RootKind::Required => {
                let key = root.key;
                let bytes =
                    fs::read(&root.path).unwrap_or_else(|error| panic!("reading {key}: {error}"));
                files.insert(key, bytes);
            },
        }
    }
    files.remove(GENERATED_ARTIFACT);
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"ds-build-id-v1");
    for (path, bytes) in &files {
        hasher.update(&(path.len() as u64).to_le_bytes());
        hasher.update(path.as_bytes());
        hasher.update(&(bytes.len() as u64).to_le_bytes());
        hasher.update(bytes);
    }
    Some(hasher.finalize().to_string())
}

/// Collects every file under `dir` into `files` keyed by
/// `prefix`-plus-relative path; `None` when the tree is absent or
/// unreadable (the packaged-build case).
fn collect_tree(dir: &Path, prefix: &str, files: &mut BTreeMap<String, Vec<u8>>) -> Option<()> {
    let entries = fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if path.is_dir() {
            collect_tree(&path, &format!("{prefix}/{name}"), files)?;
        } else {
            files.insert(format!("{prefix}/{name}"), fs::read(&path).ok()?);
        }
    }
    Some(())
}
