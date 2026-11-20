//! The `ds info` subcommand: render the `.dsi` — one package's
//! reviewable API surface (G4). The `pub` declarations of the target's
//! members print as their signature spellings, `pub(pkg)` and private
//! declarations stay out, and the rendering is re-parsable DreamScript:
//! each line is the declaration a dependent's author reads, never a
//! body. A file target renders its own surface alone; a directory target
//! renders every .ds member below it (the package's members, the same
//! walk `ds save` collects), one surface in member order.
//!
//! A file target whose compile a cache holds serves the surface from the
//! persisted `.dsi` artifact — the checked-prefix tier under `$DS_CACHE`,
//! keyed exactly as the run path keys it — and a miss (no cache root, no
//! artifact, a drifted source) falls back to the source rendering below,
//! the artifacts being a pure cache. The served rendering is the same
//! document the source path prints over the kinds the artifact carries.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use clap::Args;
use ds_diag::{Files, render};
use ds_mod::Hash128;
use ds_parse::parse;

/// The `ds info` surface: one package root or one .ds source file.
#[derive(Debug, Args)]
pub struct InfoArgs {
    /// Path of the package root directory or one .ds source file
    target: PathBuf,
}

/// Renders the target's `.dsi` to stdout.
///
/// # Errors
///
/// An unreadable target, a member that fails to parse, and a root holding
/// no .ds source.
pub fn info(args: &InfoArgs) -> Result<()> {
    if args.target.is_file()
        && let Some(served) = cached_surface(&args.target)
    {
        print!("{served}");
        return Ok(());
    }
    let members = members(&args.target)?;
    let mut files = Files::new();
    let mut asts = Vec::new();
    for path in &members {
        let source =
            fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let file = files.add_source(path.to_string_lossy().into_owned(), &source);
        let ast = parse(&files, file).map_err(|diags| plain(&diags, &files))?;
        asts.push((ast, file));
    }
    let mut out = String::new();
    for (ast, file) in &asts {
        ds_fmt::signature::member(&mut out, files.source(*file), ast);
    }
    print!("{out}");
    Ok(())
}

/// The served surface of one file target, when a cache holds its
/// compile's checked prefix: the same registration the run path pins
/// (the ambient, the member, the entry session's host provision) keys
/// the artifact, and the package's own band — the member between the
/// ambient and the host provision — renders through the package-surface
/// conversion. A manifest beside the member keys the resolved graph's
/// dep identities and native-artifact pins, both answered by the light
/// resolution entry — a cache probe pays no artifact install and no
/// graph assembly; a manifest-free target keys
/// the implicit `@std` identity its run resolves (the bare-specifier
/// switch), both exactly the run path's fold. A multi-member package
/// root registers a wider member set than this one-member registration,
/// so it stays a miss and the source rendering answers. Any miss is
/// `None`, the source rendering's cue.
fn cached_surface(target: &Path) -> Option<String> {
    let source = fs::read_to_string(target).ok()?;
    let (identities, artifacts) = match crate::package::resolve_identities(target) {
        Ok(Some(keys)) => (keys.identities, keys.artifact_pins),
        // a manifest-free target keys the implicit @std identity the
        // run path's script resolution folds (the bare-specifier
        // switch), so the artifact the same source's run wrote serves
        _ => (crate::package::implicit_std_identities(), BTreeMap::new()),
    };
    let cache =
        crate::dsi_cache::PrefixCache::discover_linked(true, false, &identities, &artifacts);
    let mut files = Files::new();
    // the registration mirrors the run's: a graph that resolved `@std`
    // (declared or implicit) registers the shrunk core, every other
    // registration the merged source
    let ambient_source = if identities.contains_key(ds_stdlib::STD_PACKAGE_NAME) {
        ds_stdlib::AMBIENT_CORE_SOURCE
    } else {
        ds_stdlib::AMBIENT_SOURCE
    };
    let ambient = files.add_source(ds_stdlib::AMBIENT_FILE, ambient_source);
    let member = files.add_source(target.display().to_string(), &source);
    let host = files.add_source(crate::source::HOST_FILE, crate::source::HOST_SOURCE);
    let name = crate::source::script_module_name(target).ok()?;
    let prefix = cache.lookup(&name, Hash128::compute(source.as_bytes()), &[
        ambient, member, host,
    ])?;
    let band = ds_dsi::surface::SurfaceBand {
        leading: 1,
        trailing: 1,
    };
    Some(ds_dsi::surface::member_surface(&prefix, band))
}

/// Every diagnostic rendered as plain text, the run's error to print.
fn plain(diags: &[ds_diag::Diagnostic], files: &Files) -> anyhow::Error {
    anyhow::anyhow!(
        "{}",
        diags
            .iter()
            .map(|diag| render::human(diag, files))
            .collect::<String>()
    )
}

/// The target's member files: the file itself, or every .ds below a root
/// in ascending path order — the same link-safe walk `ds save` collects
/// (a directory link never expands, a .ds link stays only inside the
/// root), so both verbs read one member set off the worktree.
///
/// # Errors
///
/// A missing target and a root holding no .ds source.
fn members(target: &Path) -> Result<Vec<PathBuf>> {
    if target.is_file() {
        return Ok(vec![target.to_path_buf()]);
    }
    anyhow::ensure!(target.exists(), "{} does not exist", target.display());
    let mut members = crate::package::walk_sources(target)?;
    if members.is_empty() {
        bail!("{} holds no .ds source member", target.display());
    }
    members.sort();
    Ok(members)
}
