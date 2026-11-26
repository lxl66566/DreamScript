//! The build identity the script-cache key folds: a blake3 digest over
//! the source trees of every workspace crate the source pipeline runs
//! through, so two builds whose bytecode output can differ never share
//! an artifact. Computed at build time; the run path pays nothing.

mod build_inputs;

use std::{env, path::PathBuf};

use build_inputs::{fold_build_id, input_roots};

fn main() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("cargo sets the manifest"));

    // The identity inputs and their watches come from one list: every
    // root the digest folds is a root cargo watches, spelled so it
    // resolves from the package root. The watches are emitted whether
    // or not the fold completes — an absent watch path is the packaged
    // build, and cargo records it without rerunning.
    let id = fold_build_id(&manifest);
    println!(
        "cargo:rustc-env=DS_BUILD_ID={}",
        id.as_deref().unwrap_or("")
    );
    for root in input_roots(&manifest) {
        println!("cargo:rerun-if-changed={}", root.watch);
    }
}
