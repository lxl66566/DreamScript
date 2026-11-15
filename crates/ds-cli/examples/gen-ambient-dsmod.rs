//! Regenerates the checked-in ambient dsmod artifact: runs the ambient
//! member alone through the pipeline (parse, the fresh analyze, the
//! declaration-only lowering whose `main` waiver the artifact's
//! generation needs) and writes `../ds-stdlib/src/ambient.dsmod` — the
//! 16-byte build-identity header followed by the module bytes the run
//! path's serving seam embeds. The identity is the same build digest
//! the script-cache key folds (`run_cache.rs`'s `BUILD_ID`), through
//! the same fold; the drift gate in `src/ambient_dsmod.rs` pins the two
//! spellings equal by comparing this file's output against its own
//! fresh generation. Run it after any change to `ambient.ds`, the
//! checker, the lowerer, or the dsmod format; the drift gates fail
//! until this runs, and the ambient must check and lower clean on its
//! own for the artifact to write.

use ds_mod::Hash128;

/// The build identity, the `run_cache` fold's own chain: the build-time
/// digest when the sibling sources are present, the package version in
/// packaged builds.
const BUILD_ID: &str =
    if env!("DS_BUILD_ID").is_empty() { env!("CARGO_PKG_VERSION") } else { env!("DS_BUILD_ID") };

fn main() {
    let mut files = ds_diag::Files::new();
    let id = files.add_source(ds_stdlib::AMBIENT_FILE, ds_stdlib::AMBIENT_SOURCE);
    let ast = ds_parse::parse(&files, id).expect("the ambient parses");
    let analyzed = ds_meta::analyze_owned(&files, vec![ast], &[false]);
    assert!(
        analyzed.checked.diags.is_empty(),
        "the ambient does not check clean: {:?}",
        analyzed.checked.diags
    );
    let lowered = ds_lower::lower_ambient(
        "@std",
        Hash128::compute(ds_stdlib::AMBIENT_SOURCE.as_bytes()),
        &analyzed.asts,
        &analyzed,
    )
    .expect("the ambient lowers");
    let module = lowered.module();
    assert!(
        module.chains().is_empty() && module.tags().is_empty(),
        "the ambient's lowering mints a chain or tag table: the splice installs function records \
         only, so the serving seam needs widening first"
    );
    let mut bytes = Hash128::compute(BUILD_ID.as_bytes()).as_bytes().to_vec();
    bytes.extend_from_slice(&ds_mod::write(module));
    let out = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("ds-stdlib")
        .join("src")
        .join("ambient.dsmod");
    std::fs::write(&out, &bytes).expect("writing the ambient dsmod artifact");
    println!(
        "wrote {} bytes to {} ({} functions)",
        bytes.len(),
        out.display(),
        module.functions().len()
    );
}
