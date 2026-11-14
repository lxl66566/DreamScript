//! Regenerates the precompiled ambient AST artifact: parses
//! `AMBIENT_SOURCE`, runs the ds-check derive expansion over the tree,
//! and writes `src/ambient.ast` — the 16-byte source-hash header followed
//! by the postcard-encoded expanded tree the `ambient_blob` module
//! embeds. Expansion at artifact build time is what lets the run path
//! move the served tree through the expansion pass untouched. Run it
//! after any change to `ambient.ds`, the grammar, the AST shape, or the
//! derive expansion; the drift gate fails until this runs, and the
//! ambient's own derives must expand clean for the artifact to write.

use ds_diag::Files;
use ds_mod::Hash128;
use ds_stdlib::{AMBIENT_FILE, AMBIENT_SOURCE};

fn main() {
    let mut files = Files::new();
    let file = files.add_source(AMBIENT_FILE, AMBIENT_SOURCE);
    let ast = ds_parse::parse(&files, file).expect("the ambient parses");
    let (expanded, diags) = ds_check::expand_derives(&[&ast]);
    assert!(
        diags.is_empty(),
        "the ambient's derives refuse to expand: {diags:?}"
    );
    let mut bytes = Hash128::compute(AMBIENT_SOURCE.as_bytes())
        .as_bytes()
        .to_vec();
    bytes.extend(postcard::to_allocvec(&expanded[0]).expect("the expanded AST serializes"));
    let out = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/ambient.ast");
    std::fs::write(&out, &bytes).expect("writing the ambient AST artifact");
    println!("wrote {} bytes to {}", bytes.len(), out.display());
}
