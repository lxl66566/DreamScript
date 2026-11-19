//! The `ds save` subcommand: install one working-tree package into the
//! content-addressed store — the manifest below the root plus every .ds
//! source under it, framed as the canonical bundle and installed under
//! its own identity, which the install reports so a dependent's manifest
//! can pin it. Saving never compiles and compiling never installs: the
//! two CLI verbs stay orthogonal.

use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, anyhow, bail};
use clap::Args;
use ds_pkg::{Package, Store};

use crate::package::{self, MANIFEST_FILE};

/// The `ds save` surface: one package root.
#[derive(Debug, Args)]
pub struct SaveArgs {
    /// Path of the package root: the directory holding ds.pkg
    dir: PathBuf,
}

/// Installs one package root's bundle into the store.
///
/// # Errors
///
/// An unreadable or drifting manifest, a root with no .ds source, a
/// bundle the framing refuses, and a store the platform will not write.
pub fn save(args: &SaveArgs) -> Result<()> {
    let manifest = package::read_manifest(&args.dir.join(MANIFEST_FILE))?;
    if manifest.name == ds_stdlib::STD_PACKAGE_NAME {
        bail!(
            "`{}` is the reserved standard-library package name; the toolchain's default install \
             owns it and a user package cannot be saved under it",
            ds_stdlib::STD_PACKAGE_NAME
        );
    }
    let sources = collect_members(&args.dir)?;
    let bundle = Package::new(&manifest, sources).map_err(|error| anyhow!("{error}"))?;
    let Some(root) = ds_pkg::store::cache_root() else {
        bail!(
            "no cache root resolves ($DS_CACHE unset, no user cache directory); ds save installs \
             into the package store below it"
        );
    };
    let store = Store::open(&root);
    let identity = store.install(&bundle).map_err(|error| anyhow!("{error}"))?;
    println!(
        "saved {} {} as blake3:{identity}",
        manifest.name, manifest.version
    );
    Ok(())
}

/// Every .ds source below the root, keyed by its '/'-separated path
/// relative to the root (the bundle's member grammar); other files stay
/// out of the bundle. The walk itself never follows a directory link
/// and admits a .ds link only inside the root.
///
/// # Errors
///
/// An unreadable directory or member, and a root holding no .ds source.
fn collect_members(root: &Path) -> Result<Vec<(String, Vec<u8>)>> {
    let mut members = Vec::new();
    for path in package::walk_sources(root)? {
        let bytes = fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
        let relative = path
            .strip_prefix(root)
            .expect("the walk started at the root");
        let member = relative
            .components()
            .map(|component| component.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/");
        members.push((member, bytes));
    }
    if members.is_empty() {
        bail!("{} holds no .ds source member", root.display());
    }
    Ok(members)
}
