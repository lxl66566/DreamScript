//! The build identity's watch contract: every input the digest folds
//! must be watched by a path cargo can actually resolve. A
//! rerun-if-changed spelling is package-root-relative, so a sibling
//! crate spelled from the crates dir (`ds-check/src`) watches a path
//! under ds-cli that never exists, cargo records the watch as absent,
//! and a warm target dir never reruns the build script when the
//! sibling changes — the identity silently goes stale, the dsmod drift
//! gate passes only in the checkout that regenerated the artifact, and
//! the script cache can share an artifact between builds whose
//! bytecode differs.

#[path = "../build_inputs.rs"]
mod build_inputs;

use std::path::Path;

/// Every watch spelling resolves, from the package root, to exactly
/// the file or tree its root folds: the spelling and the input share
/// one derivation, and this pins the join against the real disk.
#[test]
fn every_input_is_watched_where_it_folds() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    for root in build_inputs::input_roots(manifest) {
        let resolved = manifest.join(&root.watch);
        let watched = resolved
            .canonicalize()
            .unwrap_or_else(|error| panic!("resolving `{}`: {error}", root.watch));
        let folded = root
            .path
            .canonicalize()
            .unwrap_or_else(|error| panic!("resolving `{}`: {error}", root.path.display()));
        assert_eq!(
            watched, folded,
            "watch `{}` must point at the input it guards",
            root.watch
        );
    }
}

/// The spelling scheme itself: siblings and the lock escape the
/// package root through `..`, the run path stays inside it, and no
/// spelling carries a backslash (cargo takes forward slashes on every
/// platform). The digest keys and fold kinds are pinned alongside so
/// a refactor cannot silently re-key every checked-in artifact or
/// relax how a missing root lands.
#[test]
fn watch_spellings_and_keys_stay_pinned() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let roots = build_inputs::input_roots(manifest);
    assert_eq!(
        roots.len(),
        build_inputs::BYTECODE_CRATES.len() * 2 + 1 + build_inputs::RUN_PATH_FILES.len()
    );
    for crate_name in build_inputs::BYTECODE_CRATES {
        for tail in ["src", "Cargo.toml"] {
            let root = roots
                .iter()
                .find(|root| root.key == format!("{crate_name}/{tail}"))
                .unwrap_or_else(|| panic!("missing root {crate_name}/{tail}"));
            assert_eq!(root.watch, format!("../{crate_name}/{tail}"));
            assert!(
                matches!(
                    (&root.kind, tail),
                    (build_inputs::RootKind::Tree, "src")
                        | (build_inputs::RootKind::File, "Cargo.toml")
                ),
                "{crate_name}/{tail} must keep the fold kind its absence contract rides"
            );
        }
    }
    let lock = roots
        .iter()
        .find(|root| root.key == "Cargo.lock")
        .expect("the lock root");
    assert_eq!(lock.watch, "../../Cargo.lock");
    assert!(matches!(lock.kind, build_inputs::RootKind::File));
    for file in build_inputs::RUN_PATH_FILES {
        let root = roots
            .iter()
            .find(|root| root.key == format!("ds-cli/src/{file}"))
            .unwrap_or_else(|| panic!("missing root ds-cli/src/{file}"));
        assert_eq!(root.watch, format!("src/{file}"));
        assert!(
            matches!(root.kind, build_inputs::RootKind::Required),
            "a missing run-path source must panic, never thin the identity"
        );
    }
    assert!(
        roots
            .iter()
            .all(|root| !root.watch.contains('\\') && !root.watch.is_empty()),
        "watch spellings are forward-slash and non-empty"
    );
    // the generated dsmod rides inside the ds-stdlib tree the walk
    // folds and the fold removes its key afterward, so the removal
    // only hits while the spelling stays the tree's key plus the
    // relative path — and the artifact never becomes a root of its
    // own, which would chase the moving fixpoint directly
    let stdlib_tree = roots
        .iter()
        .find(|root| root.key == "ds-stdlib/src")
        .expect("the ds-stdlib src tree root");
    assert!(
        build_inputs::GENERATED_ARTIFACT.starts_with(&format!("{}/", stdlib_tree.key)),
        "the generated artifact's key must stay inside the tree the walk folds"
    );
    assert!(
        roots
            .iter()
            .all(|root| root.key != build_inputs::GENERATED_ARTIFACT),
        "the generated artifact never becomes its own identity root"
    );
}
