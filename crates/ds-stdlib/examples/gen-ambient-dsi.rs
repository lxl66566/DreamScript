//! Regenerates the checked-ambient artifact: runs the ambient member
//! alone through the pipeline (parse, derive expansion, collect and
//! resolve, the declaration phase for the mint/set logs, the full
//! analyze for the clean gate, the effects pass, the method rows) and
//! writes `src/ambient.dsi` — the 16-byte source-hash header followed by
//! the postcard-encoded tables the `ambient_dsi` module embeds. Run it
//! after any change to `ambient.ds`, the checker's tables, resolution,
//! effects, or the artifact format; the drift gates fail until this
//! runs, and the ambient must check and resolve clean on its own for the
//! artifact to write.

use ds_stdlib::ambient_dsi;

fn main() {
    let artifact = ambient_dsi::generate();
    let bytes = ambient_dsi::artifact_bytes(&artifact);
    let out = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/ambient.dsi");
    std::fs::write(&out, &bytes).expect("writing the checked-ambient artifact");
    println!(
        "wrote {} bytes to {} ({} defs, {} methods, {} atoms, {} instances, {} sets, {} rows)",
        bytes.len(),
        out.display(),
        artifact.package.defs.len(),
        artifact.package.methods.len(),
        artifact.universe.atoms.len(),
        artifact.instances.len(),
        artifact.universe.sets.len(),
        artifact.effects.len(),
    );
}
