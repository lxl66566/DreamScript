//! The build identity the script-cache key folds: a blake3 digest over
//! the source trees of every workspace crate the source pipeline runs
//! through, so two builds whose bytecode output can differ never share
//! an artifact. Computed at build time; the run path pays nothing.

mod build_inputs;

use std::{
    collections::BTreeMap,
    env, fs,
    path::{Path, PathBuf},
};

use build_inputs::{GENERATED_ARTIFACT, RootKind, input_roots};

fn main() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("cargo sets the manifest"));

    // The identity inputs and their watches come from one list: every
    // root the digest folds is a root cargo watches, spelled so it
    // resolves from the package root. The keys are workspace-relative
    // with forward slashes so the same tree hashes identically on
    // every platform; the repo carries no gitattributes, so
    // working-tree bytes are the identity as-is.
    let mut files: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    let mut watched: Vec<String> = Vec::new();
    let mut complete = true;
    'roots: for root in input_roots(&manifest) {
        let bytes = match root.kind {
            RootKind::Tree => {
                if !collect_tree(&root.path, &root.key, &mut files) {
                    complete = false;
                    break 'roots;
                }
                watched.push(root.watch);
                continue;
            },
            RootKind::File => fs::read(&root.path),
            // the run path's own sources; a missing one is a broken
            // package build, and the identity must never be partial
            RootKind::Required => Ok(fs::read(&root.path)
                .unwrap_or_else(|error| panic!("reading {}: {error}", root.key))),
        };
        // a packaged build has no sibling sources
        if let Ok(bytes) = bytes {
            files.insert(root.key, bytes);
            watched.push(root.watch);
        } else {
            complete = false;
            break 'roots;
        }
    }
    files.remove(GENERATED_ARTIFACT);

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
        // the version alone
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
