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

/// Compiles one source and writes its module.
///
/// The compiled module's header hash comes from the one read of the source the
/// pipeline performed, so a checked read here would re-read the file and re-run
/// blake3 over bytes the module already commits to. The written bytes are
/// verified against the module value in debug builds instead.
pub fn build(args: &BuildArgs) -> Result<()> {
    let compiled = source::compile(&args.file, "main", args.no_ambient)?;
    let module = compiled.lowered.module();
    let bytes = ds_mod::write(module);
    verify_written(&bytes, module);
    std::fs::write(&args.out, &bytes).with_context(|| format!("writing {}", args.out.display()))?;
    Ok(())
}

/// Debug-build check that the writer emitted bytes its own reader returns as
/// the same module: a writer/reader drift fails every debug build and test
/// run rather than landing a corrupt artifact.
fn verify_written(bytes: &[u8], module: &ds_mod::Module) {
    if cfg!(debug_assertions) {
        let read_back = ds_mod::read(bytes).expect("the written module must read back");
        assert_eq!(
            &read_back, module,
            "the written module must read back unchanged"
        );
    }
}
