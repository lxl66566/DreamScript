//! The `ds build` subcommand: compile one .ds source to a dsmod module
//! (parse, check, lower, write) — the artifact loads through the existing
//! `ds run <file.dsmod>` path and `read_mmap`, and its header carries the
//! source's blake3 hash, which [`ds_mod::read_checked`] verifies.

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Args;

use crate::source;

/// The `ds build` surface: one source file, one output module.
#[derive(Debug, Args)]
pub struct BuildArgs {
    /// Path to the .ds source file
    file: PathBuf,
    /// Path of the dsmod module to write
    #[arg(short, long)]
    out: PathBuf,
    /// Compile without the stdlib ambient package member
    #[arg(long)]
    no_ambient: bool,
}

/// Compiles one source and writes its module, verifying the written
/// bytes (a checked read against the very source they claim) before the
/// file lands on disk.
pub fn build(args: &BuildArgs) -> Result<()> {
    let compiled = source::compile(&args.file, "main", args.no_ambient)?;
    let bytes = ds_mod::write(compiled.lowered.module());
    let source_bytes =
        std::fs::read(&args.file).with_context(|| format!("reading {}", args.file.display()))?;
    ds_mod::read_checked(&bytes, &source_bytes)
        .with_context(|| "the written module fails its own checked read")?;
    std::fs::write(&args.out, &bytes).with_context(|| format!("writing {}", args.out.display()))?;
    Ok(())
}
