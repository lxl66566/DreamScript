//! The `ds build` subcommand: compile one .ds source to a dsmod module
//! (parse, check, lower, write) — the artifact loads through the existing
//! `ds run <file.dsmod>` path and the cache's mmap read, and its header
//! carries the compile's content identity (the source's hash for a
//! one-member root, the composed member fold otherwise), which
//! [`ds_mod::read_checked`] verifies. A source with a `ds.pkg` beside it
//! builds as a package: the manifest resolves over the store, the root
//! composes every member of its namespace (G3) — one module, exactly
//! what the same package serves as a dep — and the artifact's link
//! table records the resolved graph's identities.

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Args;

use crate::{package, source};

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
/// The compiled module's header hash comes from the pipeline's own
/// member reads (the source's hash for a one-member root, the composed
/// fold over a manifest root's members otherwise), so a checked read
/// here would re-read the file and re-run blake3 over bytes the module
/// already commits to. The written bytes are verified against the
/// module value in debug builds instead.
pub fn build(args: &BuildArgs) -> Result<()> {
    let compiled = match package::open(&args.file, args.no_ambient)? {
        Some(graph) => build_root(&args.file, &graph, args.no_ambient, false),
        // the bare-specifier switch: a manifest-free source builds over
        // the implicit `@std` graph, the same resolution its run takes
        None if !args.no_ambient => {
            build_root(&args.file, &package::implicit_std_graph(), false, true)
        },
        None => {
            let prefix = crate::dsi_cache::PrefixCache::discover(true, args.no_ambient);
            source::compile(
                &args.file,
                args.no_ambient,
                Some(&prefix),
                source::BundleAfter::Keep,
            )
        },
    }?;
    let module = compiled.lowered.module();
    let bytes = ds_mod::write(module);
    verify_written(&bytes, module);
    std::fs::write(&args.out, &bytes).with_context(|| format!("writing {}", args.out.display()))?;
    Ok(())
}

/// One root compile over a resolved graph: the same dep identities,
/// artifact pins, and session shape key the prefix tier, and `script`
/// picks the root's parse (the manifest-free source folds its top-level
/// statements, a manifest root keeps the member grammar).
fn build_root(
    file: &std::path::Path,
    graph: &package::Graph,
    no_ambient: bool,
    script: bool,
) -> Result<source::CompiledSource> {
    let pins = graph.artifact_pins();
    let linked = crate::dsi_cache::PrefixCache::discover_linked(
        true,
        no_ambient,
        graph.identities(),
        &pins,
        script,
    );
    package::compile_root(file, graph, script, Some(&linked), None)
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
