//! The `ds info` subcommand: render the `.dsi` — one package's
//! reviewable API surface (G4). The `pub` declarations of the target's
//! members print as their signature spellings, `pub(pkg)` and private
//! declarations stay out, and the rendering is re-parsable DreamScript:
//! each line is the declaration a dependent's author reads, never a
//! body. A file target renders its own surface alone; a directory target
//! renders every .ds member below it (the package's members, the same
//! walk `ds save` collects), one surface in member order.

use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use clap::Args;
use ds_diag::{Files, render};
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
