//! The build identity the script-cache key folds: a blake3 digest over
//! the source trees of every workspace crate the source pipeline runs
//! through, so two builds whose bytecode output can differ never share
//! an artifact. Computed at build time; the run path pays nothing.

use std::{
    collections::BTreeMap,
    env, fs,
    path::{Path, PathBuf},
};

/// The workspace crates whose code turns source text into module bytes,
/// ds-vm deliberately absent (it executes the artifact but never shapes
/// it) alongside the lsp/repl/fmt subcommands.
const BYTECODE_CRATES: [&str; 9] = [
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
const RUN_PATH_FILES: [&str; 7] = [
    "main.rs",
    "run.rs",
    "run_cache.rs",
    "dsi_cache.rs",
    "source.rs",
    "ambient_dsmod.rs",
    "package.rs",
];

/// The generated artifact inside a digested tree: its own bytes fold the
/// identity, so hashing it here would chase a moving fixpoint. The
/// drift gate pins it against a fresh generation instead.
const GENERATED_ARTIFACT: &str = "ds-stdlib/src/ambient.dsmod";

fn main() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("cargo sets the manifest"));
    let crates_dir = manifest
        .parent()
        .expect("the package sits inside crates/")
        .to_path_buf();

    // The identity inputs, keyed by workspace-relative path with forward
    // slashes so the same tree hashes identically on every platform. The
    // repo carries no gitattributes, so working-tree bytes are the
    // identity as-is.
    let mut files: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    let mut watched: Vec<String> = Vec::new();
    let mut complete = true;
    for crate_name in BYTECODE_CRATES {
        let package = crates_dir.join(crate_name);
        let src = package.join("src");
        if !collect_tree(&src, &format!("{crate_name}/src"), &mut files) {
            complete = false;
            break;
        }
        // a feature flip recompiles different code without touching any
        // source file, so each manifest is an identity input
        if let Ok(bytes) = fs::read(package.join("Cargo.toml")) {
            files.insert(format!("{crate_name}/Cargo.toml"), bytes);
        } else {
            complete = false;
            break;
        }
        watched.push(relativize(&src, &manifest));
    }
    // The lock pins every dependency's version, and a dependency upgrade
    // (the parser's grammar engine included) can change the produced
    // bytecode without any workspace source changing.
    if complete {
        match fs::read(
            crates_dir
                .parent()
                .expect("the crates dir has a parent")
                .join("Cargo.lock"),
        ) {
            Ok(bytes) => {
                files.insert("Cargo.lock".to_owned(), bytes);
            },
            Err(_) => complete = false,
        }
    }
    files.remove(GENERATED_ARTIFACT);
    if complete {
        for file in RUN_PATH_FILES {
            // the run path's own sources; a missing one is a broken
            // package build, and the identity must never be partial
            let bytes = fs::read(manifest.join("src").join(file)).unwrap_or_else(|error| {
                panic!("reading the run path's {file}: {error}");
            });
            files.insert(format!("ds-cli/src/{file}"), bytes);
            watched.push(format!("src/{file}"));
        }
    }

    if complete {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"ds-build-id-v1");
        for (path, bytes) in &files {
            hasher.update(&(path.len() as u64).to_le_bytes());
            hasher.update(path.as_bytes());
            hasher.update(&(bytes.len() as u64).to_le_bytes());
            hasher.update(bytes);
        }
        println!("cargo:rustc-env=DS_BUILD_ID={}", hasher.finalize());
    } else {
        // a packaged build has no sibling sources: the version alone
        println!("cargo:rustc-env=DS_BUILD_ID=");
    }
    for path in watched {
        println!("cargo:rerun-if-changed={path}");
    }
}

/// Collects every file under `dir` into `files` keyed by
/// `prefix`-plus-relative path; false when the tree is absent or
/// unreadable (the packaged-build case).
fn collect_tree(dir: &Path, prefix: &str, files: &mut BTreeMap<String, Vec<u8>>) -> bool {
    let Ok(entries) = fs::read_dir(dir) else {
        return false;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if !collect_tree(&path, &format!("{prefix}/{name}"), files) {
                return false;
            }
        } else {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            match fs::read(&path) {
                Ok(bytes) => {
                    files.insert(format!("{prefix}/{name}"), bytes);
                },
                Err(_) => return false,
            }
        }
    }
    true
}

/// The build-script path spelling of `dir` relative to the package root.
fn relativize(dir: &Path, manifest: &Path) -> String {
    dir.strip_prefix(manifest.parent().expect("the crates dir is the parent"))
        .unwrap_or(dir)
        .components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<String>>()
        .join("/")
}
