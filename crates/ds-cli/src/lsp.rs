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
/// worktree read one artifact. The maps' lock guards reads and writes
/// alone: graph resolution and artifact decode run outside it, so a
/// slow resolve or read never holds the maps against the engine's next
/// demand. A root that answers nothing (no manifest beside the
/// document, an unresolvable graph, no cached artifact) records no
/// entry, so the next hover re-probes — the common miss being one
/// manifest stat — and a `ds.pkg` that appears mid-session serves;
/// every miss answers nothing and the hover falls back to the session's
/// own tiers. Recomputing negatives is the smaller shape against a
/// timestamped expiry: it carries no constant and no clock branch, and
/// a graph that refuses resolution re-probes exactly what each
/// `ds run` invocation pays.
fn linked_surface_provider() -> ds_lsp::SurfaceProvider {
    provider_over(|doc| crate::package::resolve_identities(doc).ok().flatten())
}

/// The provider over a caller-named resolve seam: each document's
/// graph keys come from `resolve` (the production seam the package
/// light entry answers), so the memo policy — resolution and decode
/// outside the lock, positives memoized once per process, negatives
/// re-probed — is testable without a store or a manifest.
fn provider_over(
    resolve: impl Fn(&Path) -> Option<crate::package::GraphKeys> + Send + Sync + 'static,
) -> ds_lsp::SurfaceProvider {
    #[derive(Default)]
    struct Memo {
        /// Each package root's resolved graph keys, once per process;
        /// a root that answers nothing records no entry, so the next
        /// hover re-probes it.
        roots: HashMap<PathBuf, crate::package::GraphKeys>,
        /// Each served (root, link name) surface, decoded once.
        surfaces: HashMap<(PathBuf, String), Arc<ds_lsp::LinkedSurface>>,
    }
    let memo = Mutex::new(Memo::default());
    Arc::new(move |doc: &Path, link: &str| {
        let root = doc.parent()?.to_path_buf();
        let key = (root.clone(), link.to_owned());
        if let Some(surface) = memo.lock().ok()?.surfaces.get(&key) {
            return Some(Arc::clone(surface));
        }
        let keys = if let Some(keys) = memo.lock().ok()?.roots.get(&root) {
            keys.clone()
        } else {
            // resolution runs outside the maps' lock; a racing
            // second resolution only lands the same keys, the
            // double-check keeping one incumbent
            let resolved = resolve(doc)?;
            let mut memo = memo.lock().ok()?;
            memo.roots.entry(root).or_insert(resolved).clone()
        };
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
    let cache = crate::run_cache::RunCache::discover_linked(
        true,
        false,
        &keys.identities,
        &keys.artifact_pins,
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

    use super::provider_over;
    use crate::package::GraphKeys;

    fn empty_keys() -> GraphKeys {
        GraphKeys {
            identities: BTreeMap::new(),
            artifact_pins: BTreeMap::new(),
        }
    }

    /// The roots memo's negative policy: a root that answers nothing
    /// records no entry, so every hover re-probes and a manifest that
    /// appears mid-session serves on the next hover; once a root
    /// answers, its keys stay memoized for the process.
    #[test]
    fn negative_roots_reprobe_until_the_manifest_answers() {
        let probes = Arc::new(AtomicUsize::new(0));
        let answered = Arc::new(Mutex::new(false));
        let provider = provider_over({
            let probes = Arc::clone(&probes);
            let answered = Arc::clone(&answered);
            move |doc: &Path| {
                assert!(doc.ends_with("main.ds"), "the seam sees the document");
                probes.fetch_add(1, Ordering::SeqCst);
                answered.lock().expect("uncontended").then(empty_keys)
            }
        });
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
            "a resolved root memoizes once per process"
        );
    }

    /// Graph resolution runs outside the maps' lock: a resolve seam
    /// that re-enters the provider for another document's link answers
    /// instead of deadlocking on the lock the old provider held across
    /// its resolution.
    #[test]
    fn resolution_runs_outside_the_maps_lock() {
        let nested: Arc<OnceLock<ds_lsp::SurfaceProvider>> = Arc::new(OnceLock::new());
        let provider = provider_over({
            let nested = Arc::clone(&nested);
            move |doc: &Path| {
                if doc.ends_with("inner.ds") {
                    return Some(empty_keys());
                }
                let provider = nested.get().expect("the provider registers first");
                assert!(provider(Path::new("nested/inner.ds"), "@lib").is_none());
                Some(empty_keys())
            }
        });
        assert!(nested.set(provider.clone()).is_ok(), "set once");
        assert!(provider(Path::new("outer/main.ds"), "@lib").is_none());
    }
}
