//! The `ds bind` subcommand: H5's binding face over the packages store.
//! Each `[artifacts]` library the manifest graph resolves installs
//! content-addressed on the run path's own rule (a missing artifact
//! refusing before anything binds), then binds: the host loader takes
//! the installed file by its path, and the export reader enumerates the
//! library's C symbols — the report naming what a source-level extern
//! round will address. Binding today reports and never calls: the
//! consumption seam still refuses at lowering until the C-ABI
//! marshaling round, whose H5 contract clauses (a bound extern means
//! panic=abort, a blocking foreign call pins its fiber) hold from the
//! first bound library.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, anyhow, bail};
use clap::Args;
use ds_pkg::{ArtifactBound, Store};

/// The `ds bind` surface: one package-shaped target, optionally one
/// named artifact of it.
#[derive(Debug, Args)]
pub struct BindArgs {
    /// Path of a .ds source with a ds.pkg beside it, or of the package
    /// root holding the ds.pkg
    target: PathBuf,
    /// Bind only the named [artifacts] entry (default: every artifact
    /// the graph resolves)
    #[arg(long)]
    artifact: Option<String>,
}

/// Binds the target's installed artifact libraries and reports each
/// one's exported C symbols.
///
/// # Errors
///
/// No package beside the target, every resolution and install refusal
/// the run path reports, an artifact the manifest does not name, a
/// library the host loader refuses, and bytes whose export tables do
/// not read.
pub fn bind(args: &BindArgs) -> Result<()> {
    let entry = entry_of(&args.target)?;
    let Some(graph) = crate::package::open(&entry, false)? else {
        bail!(
            "{} declares no package: no ds.pkg sits beside it, so nothing declares an artifact",
            args.target.display()
        );
    };
    let artifacts = graph.artifacts();
    if artifacts.is_empty() {
        bail!(
            "the resolved graph of {} declares no [artifacts] entry: no native library is pinned",
            args.target.display()
        );
    }
    let selected = select(artifacts, args.artifact.as_deref())?;
    // the same store the install landed in: the run path's open already
    // resolved the cache root, so the root answers here too
    let root = ds_pkg::store::cache_root().ok_or_else(|| {
        anyhow!("no cache root resolves ($DS_CACHE unset, no user cache directory)")
    })?;
    let store = Store::open(&root);
    for (name, artifact) in selected {
        bind_one(&store, name, artifact)?;
    }
    Ok(())
}

/// The entry path whose parent holds the manifest: a file target stands
/// as itself (the run/build rule); a directory target names the package
/// root, the manifest inside it serving the same discovery.
///
/// # Errors
///
/// A missing target, and a directory holding no ds.pkg.
fn entry_of(target: &Path) -> Result<PathBuf> {
    anyhow::ensure!(target.exists(), "{} does not exist", target.display());
    if target.is_dir() {
        anyhow::ensure!(
            target.join(crate::package::MANIFEST_FILE).is_file(),
            "{} holds no ds.pkg",
            target.display()
        );
        // the parent rule reads the manifest beside a file: any child
        // path of the root serves, and nothing reads the child itself
        return Ok(target.join("_.ds"));
    }
    Ok(target.to_path_buf())
}

/// The artifacts this bind reports: the whole graph's, or the one the
/// flag names — an unknown name refusing with the declared spellings.
fn select<'a>(
    artifacts: &'a BTreeMap<String, ArtifactBound>,
    only: Option<&'a str>,
) -> Result<Vec<(&'a str, &'a ArtifactBound)>> {
    match only {
        None => Ok(artifacts
            .iter()
            .map(|(name, bound)| (name.as_str(), bound))
            .collect()),
        Some(name) => {
            let Some(bound) = artifacts.get(name) else {
                let declared = artifacts
                    .keys()
                    .map(String::as_str)
                    .collect::<Vec<_>>()
                    .join(", ");
                bail!(
                    "the manifest declares no artifact `{name}`{}",
                    if declared.is_empty() {
                        String::new()
                    } else {
                        format!(" (declared: {declared})")
                    }
                );
            };
            Ok(vec![(name, bound)])
        },
    }
}

/// Binds and reports one installed artifact: the loader takes the
/// library by its content-addressed path and the export reader
/// enumerates its C symbols, the report naming the bound identity.
///
/// # Errors
///
/// A library the host loader refuses (a foreign target triple among the
/// causes, the declared target named beside the refusal), bytes that
/// vanished from the store, and export tables that do not read.
fn bind_one(store: &Store, name: &str, artifact: &ArtifactBound) -> Result<()> {
    let path = store.artifact_path(artifact.hash);
    // the loader taking the file is the bind; the handle stays live
    // through the report and releases after it
    let _bound = ds_ffi::Library::open(&path).map_err(|error| {
        anyhow!(
            "{error}; the artifact declares the target `{}`, and a library built for another \
             triple is among the refusals",
            artifact.target
        )
    })?;
    let bytes = store
        .load_artifact(artifact.hash)
        .with_context(|| format!("artifact `{name}` reads back from the store"))?
        .with_context(|| format!("artifact `{name}` {} is not installed", artifact.ver))?;
    let symbols = ds_ffi::enumerate(&bytes)
        .with_context(|| format!("artifact `{name}`'s export tables refuse to read"))?;
    println!(
        "bound `{name}` {} for {} (blake3:{})",
        artifact.ver, artifact.target, artifact.hash
    );
    println!("  {} exported C symbol(s)", symbols.len());
    for symbol in &symbols {
        println!("  {symbol}");
    }
    Ok(())
}
