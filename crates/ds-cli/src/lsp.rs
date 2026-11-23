//! The `ds lsp` subcommand: the language-server protocol on stdin and
//! stdout, the editor embedding of the same query surface the REPL and
//! `ds check` drive. The engine runs on a dedicated worker thread; this
//! subcommand only builds the runtime the protocol loop needs — and the
//! linked-surface provider the hover's linked tier reads, serving the
//! `.dsi` artifacts a run of the edited package's graph cached.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::ExitCode,
    sync::{Arc, Mutex},
};

use anyhow::Result;
use clap::Args;
use ds_mod::Hash128;

/// The `ds lsp` surface: no options in the first slice (the client's
/// `initialize` carries the protocol configuration).
#[derive(Debug, Args)]
pub struct LspArgs {}

/// Serves the protocol until the client shuts the server down or
/// disconnects.
///
/// # Errors
///
/// Runtime-construction and stream failures only; language-level
/// problems travel as published diagnostics, never as exits.
pub fn lsp(_args: &LspArgs) -> Result<ExitCode> {
    // The protocol loop is async while the CLI stays sync, so the
    // runtime lives and dies inside the subcommand.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(ds_lsp::serve_with(
        tokio::io::stdin(),
        tokio::io::stdout(),
        linked_surface_provider(),
    ))?;
    Ok(ExitCode::SUCCESS)
}

/// The LSP's linked-surface provider: a document whose directory holds
/// a `ds.pkg` resolves its package graph's key inputs once per process
/// and serves each linked name's `.dsi` surface from the artifact a run
/// of the same graph cached beside the dep's compiled module — the same
/// key fold, so a package run and an editor session over the same
/// worktree read one artifact. The memo's invalidation key is the
/// content fingerprint of the input set the root's resolution consumed
/// (the manifest's bytes plus each path dep directory's content
/// hash): each hover compares it once — a cheap read, never a
/// resolution — and a mismatch re-resolves the root and drops its
/// served surfaces, so a dep repin or a manifest edit mid-session
/// lands with no restart while an unchanged input set stands on the
/// memo. The maps' lock guards reads and writes alone: the fingerprint
/// read, graph resolution, and artifact decode run outside it, so a
/// slow read or resolve never holds the maps against the engine's
/// next demand. A root that answers nothing (no manifest beside the
/// document, an unresolvable graph, no cached artifact) records no
/// entry, so the next hover re-probes — the common miss being one
/// manifest stat — and a `ds.pkg` that appears mid-session serves;
/// every miss answers nothing and the hover falls back to the session's
/// own tiers. Recomputing negatives is the smaller shape against a
/// timestamped expiry: it carries no constant and no clock branch, and
/// a graph that refuses resolution re-probes exactly what each
/// `ds run` invocation pays.
fn linked_surface_provider() -> ds_lsp::SurfaceProvider {
    provider_over(
        |doc| crate::package::resolve_identities(doc).ok().flatten(),
        crate::package::resolution_fingerprint,
    )
}

/// The provider over caller-named resolve and fingerprint seams: each
/// document's graph keys come from `resolve` (the production seam the
/// package light entry answers) and the consumed inputs' content
/// fingerprint from `fingerprint` (the production seam the manifest
/// and path dep fold answers), so the memo policy — the fingerprint
/// compared once per hover ahead of every map read, resolution and
/// decode outside the lock, positives re-resolving exactly when the
/// inputs changed, negatives re-probed — is testable without a store
/// or a manifest.
fn provider_over(
    resolve: impl Fn(&Path) -> Option<crate::package::GraphKeys> + Send + Sync + 'static,
    fingerprint: impl Fn(&Path) -> Option<Hash128> + Send + Sync + 'static,
) -> ds_lsp::SurfaceProvider {
    #[derive(Default)]
    struct Memo {
        /// Each package root's consumed-input fingerprint over its
        /// resolved graph keys: the pair the cheap compare re-resolves
        /// behind. A root that answers nothing records no entry, so
        /// the next hover re-probes it.
        roots: HashMap<PathBuf, (Option<Hash128>, crate::package::GraphKeys)>,
        /// Each served (root, link name) surface, decoded once under
        /// the root's current keys — a re-resolve landing other keys
        /// drops the root's entries with them.
        surfaces: HashMap<(PathBuf, String), Arc<ds_lsp::LinkedSurface>>,
    }
    let memo = Mutex::new(Memo::default());
    Arc::new(move |doc: &Path, link: &str| {
        let root = doc.parent()?.to_path_buf();
        let key = (root.clone(), link.to_owned());
        // the cheap compare first: the consumed inputs' fingerprint,
        // one read the memo hit pays instead of a resolution, so a
        // memoized surface never survives a changed input set
        let print = fingerprint(doc);
        let hit = {
            let memo = memo.lock().ok()?;
            match memo.roots.get(&root) {
                Some((memoed, keys)) if *memoed == print => Some(keys.clone()),
                _ => None,
            }
        };
        let keys = if let Some(keys) = hit {
            keys
        } else {
            // resolution runs outside the maps' lock; a racing
            // second resolution only lands the same keys, the
            // double-check keeping one incumbent
            let Some(resolved) = resolve(doc) else {
                let mut memo = memo.lock().ok()?;
                memo.roots.remove(&root);
                memo.surfaces.retain(|(held, _), _| held != &root);
                return None;
            };
            let mut memo = memo.lock().ok()?;
            let changed = memo
                .roots
                .get(&root)
                .is_none_or(|(_, keys)| *keys != resolved);
            memo.roots.insert(root.clone(), (print, resolved.clone()));
            if changed {
                memo.surfaces.retain(|(held, _), _| held != &root);
            }
            resolved
        };
        if let Some(surface) = memo.lock().ok()?.surfaces.get(&key) {
            return Some(Arc::clone(surface));
        }
        let surface = Arc::new(dep_surface(&keys, &key.1)?);
        let mut memo = memo.lock().ok()?;
        memo.surfaces.entry(key).or_insert(Arc::clone(&surface));
        Some(surface)
    })
}

/// One linked name's served surface from its cached `.dsi`: the dep fold
/// the run path keys (the resolved graph's identities and artifact pins
/// over the member's own), the bytes beside the dsmod, and the
/// package-surface conversion over the decoded artifact's own member
/// band. Any miss is `None` — a cache miss, never an error.
fn dep_surface(keys: &crate::package::GraphKeys, link: &str) -> Option<ds_lsp::LinkedSurface> {
    let identity = *keys.identities.get(link)?;
    // a dep member compiles package-shaped (never a script session), so
    // the fold keys the package reading
    let cache = crate::run_cache::RunCache::discover_linked(
        true,
        false,
        &keys.identities,
        &keys.artifact_pins,
        false,
    );
    let artifact = cache.dep_dsi_path(link, identity)?;
    let bytes = std::fs::read(&artifact).ok()?;
    let decoded = ds_dsi::decode_prefix(&bytes, identity).ok()?;
    let files = decoded.package.files.clone();
    let prefix = ds_dsi::serve_prefix(&decoded, &files).ok()?;
    let band = ds_dsi::surface::SurfaceBand {
        leading: crate::source::package_ambient_files(false, link),
        trailing: 0,
    };
    Some(ds_lsp::LinkedSurface {
        signatures: ds_dsi::surface::surface_lines(&prefix, band)
            .into_iter()
            .collect(),
    })
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeMap,
        path::Path,
        sync::{
            Arc, Mutex, OnceLock,
            atomic::{AtomicUsize, Ordering},
        },
    };

    use ds_mod::Hash128;

    use super::provider_over;
    use crate::package::GraphKeys;

    fn empty_keys() -> GraphKeys {
        GraphKeys {
            identities: BTreeMap::new(),
            artifact_pins: BTreeMap::new(),
        }
    }

    /// One generation's keys: a single dep identity, so a re-resolve
    /// under a different generation answers different keys.
    fn keys_of(identity: Hash128) -> GraphKeys {
        GraphKeys {
            identities: BTreeMap::from([("@lib".to_owned(), identity)]),
            artifact_pins: BTreeMap::new(),
        }
    }

    /// The roots memo's negative policy: a root that answers nothing
    /// records no entry, so every hover re-probes and a manifest that
    /// appears mid-session serves on the next hover; once a root
    /// answers, its keys stay memoized while the consumed inputs'
    /// fingerprint holds.
    #[test]
    fn negative_roots_reprobe_until_the_manifest_answers() {
        let probes = Arc::new(AtomicUsize::new(0));
        let answered = Arc::new(Mutex::new(false));
        let provider = provider_over(
            {
                let probes = Arc::clone(&probes);
                let answered = Arc::clone(&answered);
                move |doc: &Path| {
                    assert!(doc.ends_with("main.ds"), "the seam sees the document");
                    probes.fetch_add(1, Ordering::SeqCst);
                    answered.lock().expect("uncontended").then(empty_keys)
                }
            },
            {
                let answered = Arc::clone(&answered);
                move |doc: &Path| {
                    assert!(doc.ends_with("main.ds"), "the seam sees the document");
                    answered
                        .lock()
                        .expect("uncontended")
                        .then(|| Hash128::compute(b"manifest"))
                }
            },
        );
        let doc = Path::new("worktree/main.ds");
        assert!(provider(doc, "@lib").is_none());
        assert!(provider(doc, "@lib").is_none());
        assert_eq!(
            probes.load(Ordering::SeqCst),
            2,
            "a miss records nothing, so every hover re-probes"
        );
        *answered.lock().expect("uncontended") = true;
        assert!(
            provider(doc, "@lib").is_none(),
            "the empty graph serves no link"
        );
        assert_eq!(
            probes.load(Ordering::SeqCst),
            3,
            "the re-probe sees the manifest that appeared mid-session"
        );
        assert!(provider(doc, "@lib").is_none());
        assert_eq!(
            probes.load(Ordering::SeqCst),
            3,
            "a resolved root memoizes while the fingerprint holds"
        );
    }

    /// A mismatched fingerprint re-resolves the root mid-session: the
    /// manifest edit a dep repin is moves the consumed inputs'
    /// fingerprint, so the memo re-resolves and the new keys land with
    /// no restart — while the unchanged input set after it stands on
    /// the memo, no re-resolve.
    #[test]
    fn a_fingerprint_mismatch_re_resolves_the_root_mid_session() {
        // the manifest's generation: 0 the original pin, 1 the repin
        let generation = Arc::new(AtomicUsize::new(0));
        let probes = Arc::new(AtomicUsize::new(0));
        let provider = provider_over(
            {
                let generation = Arc::clone(&generation);
                let probes = Arc::clone(&probes);
                move |doc: &Path| {
                    assert!(doc.ends_with("main.ds"), "the seam sees the document");
                    probes.fetch_add(1, Ordering::SeqCst);
                    Some(match generation.load(Ordering::SeqCst) {
                        0 => keys_of(Hash128::compute(b"lib-v1")),
                        _ => keys_of(Hash128::compute(b"lib-v2")),
                    })
                }
            },
            {
                let generation = Arc::clone(&generation);
                move |doc: &Path| {
                    assert!(doc.ends_with("main.ds"), "the seam sees the document");
                    Some(match generation.load(Ordering::SeqCst) {
                        0 => Hash128::compute(b"manifest-v1"),
                        _ => Hash128::compute(b"manifest-v2"),
                    })
                }
            },
        );
        let doc = Path::new("worktree/main.ds");
        // the root resolves once and memoizes behind its fingerprint
        assert!(provider(doc, "@lib").is_none());
        assert_eq!(probes.load(Ordering::SeqCst), 1);
        // the unchanged input set stands on the memo
        assert!(provider(doc, "@lib").is_none());
        assert_eq!(
            probes.load(Ordering::SeqCst),
            1,
            "the memo hit re-resolves nothing"
        );
        // the repin: the manifest's bytes changed, so the fingerprint
        // mismatches and the root re-resolves mid-session
        generation.store(1, Ordering::SeqCst);
        assert!(provider(doc, "@lib").is_none());
        assert_eq!(
            probes.load(Ordering::SeqCst),
            2,
            "the changed inputs re-resolve the root"
        );
        // the repinned manifest's fingerprint memoizes the new keys
        assert!(provider(doc, "@lib").is_none());
        assert_eq!(
            probes.load(Ordering::SeqCst),
            2,
            "the memo hit stands on the new fingerprint"
        );
    }

    /// Graph resolution runs outside the maps' lock: a resolve seam
    /// that re-enters the provider for another document's link answers
    /// instead of deadlocking on the lock the old provider held across
    /// its resolution.
    #[test]
    fn resolution_runs_outside_the_maps_lock() {
        let nested: Arc<OnceLock<ds_lsp::SurfaceProvider>> = Arc::new(OnceLock::new());
        let provider = provider_over(
            {
                let nested = Arc::clone(&nested);
                move |doc: &Path| {
                    if doc.ends_with("inner.ds") {
                        return Some(empty_keys());
                    }
                    let provider = nested.get().expect("the provider registers first");
                    assert!(provider(Path::new("nested/inner.ds"), "@lib").is_none());
                    Some(empty_keys())
                }
            },
            |_| Some(Hash128::compute(b"manifest")),
        );
        assert!(nested.set(provider.clone()).is_ok(), "set once");
        assert!(provider(Path::new("outer/main.ds"), "@lib").is_none());
    }
}
