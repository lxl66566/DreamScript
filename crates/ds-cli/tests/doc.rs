//! End-to-end `ds doc` through the binary: the checked-in reference and
//! error table are the dumper's own bytes, and a fixture pins the dump's
//! shape — fixed family order, sorted entries, comments attached, private
//! declarations out.

use std::{fs, process::Command};

mod support;

/// Runs `ds doc` with the arguments and answers its stdout.
fn ds_doc(args: &[&str]) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_ds"))
        .arg("doc")
        .args(args)
        .output()
        .expect("the ds binary builds with the test");
    assert!(
        output.status.success(),
        "ds doc {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// The checked-in reference is the bare dumper's own output: the drift
/// pin that keeps `docs/en/reference.md` regenerating with the surface.
#[test]
fn the_reference_is_the_dumpers_own_bytes() {
    let dumped = ds_doc(&[]);
    let checked = fs::read_to_string("../../docs/en/reference.md")
        .expect("docs/en/reference.md is checked in");
    assert_eq!(
        dumped, checked,
        "docs/en/reference.md drifted from `ds doc`; regenerate it"
    );
}

/// The checked-in error table is `ds doc --errors`'s own output: the
/// generation pin against the diagnostic registry, so a registry change
/// without a regenerated table fails here.
#[test]
fn the_error_table_is_the_registrys_own_bytes() {
    let dumped = ds_doc(&["--errors"]);
    let checked =
        fs::read_to_string("../../docs/en/errors.md").expect("docs/en/errors.md is checked in");
    assert_eq!(
        dumped, checked,
        "docs/en/errors.md drifted from the registry; regenerate with `ds doc --errors`"
    );
}

/// The dumper is deterministic: two runs answer byte-identical output.
#[test]
fn two_runs_answer_identical_bytes() {
    assert_eq!(ds_doc(&[]), ds_doc(&[]), "no clock, no map order");
    assert_eq!(
        ds_doc(&["--errors"]),
        ds_doc(&["--errors"]),
        "no clock, no map order"
    );
}

/// A fixture file pins the dump's shape: the fixed family order over
/// source order, entries sorted inside a family, the comment run
/// attached as-is, and the private declarations out.
#[test]
fn a_fixture_pins_the_shape() {
    let dir = support::guarded_dir("ds-cli-doc-fixture-");
    let path = dir.join("fixture.ds");
    fs::write(
        &path,
        concat!(
            "// The pair struct, spelled first but sorted mid-list.\n",
            "pub struct alpha { x: int, y: int }\n",
            "\n",
            "tag hidden\n",
            "\n",
            "pub fn mid() -> int\n",
            "\n",
            "// The smaller fn, spelled after mid on purpose.\n",
            "pub fn beta() -> int\n",
            "\n",
            "// A value.\n",
            "pub const omega = 1\n",
            "\n",
            "pub trait show {\n",
            "  fn show(self) -> string\n",
            "}\n",
            "\n",
            "impl show for int {\n",
            "  fn show(self) -> string\n",
            "}\n",
        ),
    )
    .expect("the fixture writes");
    let target = path.to_string_lossy().into_owned();
    let out = ds_doc(&[&target]);
    // Family sections in the reference's fixed order: the trait spelled
    // last in the source renders first, the impl last.
    let trait_at = out.find("### Traits").expect("the trait section renders");
    let struct_at = out.find("### Structs").expect("the struct section renders");
    let functions_at = out.find("### Functions").expect("the fn section renders");
    let values_at = out.find("### Values").expect("the value section renders");
    let impls_at = out.find("### Impls").expect("the impl section renders");
    assert!(
        trait_at < struct_at
            && struct_at < functions_at
            && functions_at < values_at
            && values_at < impls_at,
        "families render in the fixed order: {out}"
    );
    // Sorted entries inside a family: beta before mid despite the source
    // order.
    let beta_at = out.find("#### `beta`").expect("beta renders");
    let mid_at = out.find("#### `mid`").expect("mid renders");
    assert!(beta_at < mid_at, "functions sort by name: {out}");
    // The comment run rides its declaration, bytes as the source carries
    // them.
    assert!(
        out.contains("#### `beta`\n\nThe smaller fn, spelled after mid on purpose.\n\n"),
        "the comment run attaches: {out}"
    );
    // The private tag stays out; the impl face renders member signatures.
    assert!(
        !out.contains("hidden"),
        "non-pub declarations stay out: {out}"
    );
    assert!(
        out.contains("impl show for int { fn show(self) -> string }"),
        "the impl face renders member signatures: {out}"
    );
    assert_eq!(out, ds_doc(&[&target]), "deterministic");
}
