//! Regenerates the precompiled ambient AST artifact: parses
//! `AMBIENT_SOURCE` and writes `src/ambient.ast` — the 16-byte source-hash
//! header followed by the postcard-encoded tree the `ambient_blob` module
//! embeds. Run it after any change to `ambient.ds`, the grammar, or the
//! AST shape; the drift gate fails until this runs.

use ds_diag::Files;
use ds_mod::Hash128;
use ds_stdlib::{AMBIENT_FILE, AMBIENT_SOURCE};

fn main() {
    let mut files = Files::new();
    let file = files.add_source(AMBIENT_FILE, AMBIENT_SOURCE);
    let ast = ds_parse::parse(&files, file).expect("the ambient parses");
    let mut bytes = Hash128::compute(AMBIENT_SOURCE.as_bytes())
        .as_bytes()
        .to_vec();
    bytes.extend(postcard::to_allocvec(&ast).expect("the AST serializes"));
    let out = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/ambient.ast");
    std::fs::write(&out, &bytes).expect("writing the ambient AST artifact");
    println!("wrote {} bytes to {}", bytes.len(), out.display());
}
