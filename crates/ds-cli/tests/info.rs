//! End-to-end `ds info` through the binary: the `.dsi` rendering of a
//! package root and of one member file — `pub` declarations alone, in
//! source order, signature-spelled and re-parsable.

use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

mod support;

/// A private guarded scratch root, removed at the test thread's exit.
fn temp(tag: &str) -> PathBuf {
    support::guarded_dir(&format!("ds-cli-info-{tag}-"))
}

fn ds_info(target: &Path) -> (bool, String, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_ds"))
        .arg("info")
        .arg(target)
        .output()
        .expect("the ds binary builds with the test");
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

/// The fixture package: one library member carrying one declaration per
/// visible kind plus the tiers the `.dsi` must omit, and one second
/// member the directory target folds in.
const LIB_MEMBER: &str = concat!(
    "fn hidden() -> int {\n",
    "  1\n",
    "}\n",
    "pub(pkg) fn internal() -> int {\n",
    "  2\n",
    "}\n",
    "pub fn open_api(mut xs: list[int]) -> int | none {\n",
    "  xs.first()\n",
    "}\n",
    "pub extern fn fields(t: type) -> list[field]\n",
    "pub struct rec[t] {\n",
    "  n: t\n",
    "  items: list[t]\n",
    "}\n",
    "pub enum tone {\n",
    "  quiet\n",
    "  loud\n",
    "  payload(int, string)\n",
    "  shaped { code: int }\n",
    "}\n",
    "pub trait speak {\n",
    "  fn speak(self) -> string\n",
    "}\n",
    "pub const answer: int = 42\n",
    "pub type id[t] = t\n",
    "pub tag unit\n",
    "import @std/csv (split)\n",
);

const EXTRA_MEMBER: &str = "pub fn second() -> int {\n  7\n}\nfn secret() -> int {\n  9\n}\n";

/// Writes one package root and returns its path.
fn pkg_root(tag: &str) -> PathBuf {
    let root = temp(tag);
    fs::write(root.join("lib.ds"), LIB_MEMBER).expect("the library member writes");
    fs::write(root.join("extra.ds"), EXTRA_MEMBER).expect("the extra member writes");
    root
}

/// The directory target renders every member's `pub` surface in member
/// order; `pub(pkg)`, private declarations, and imports stay out.
#[test]
fn a_package_root_renders_its_pub_surface() {
    let root = pkg_root("dir");
    let (ok, out, err) = ds_info(&root);
    assert!(ok, "info succeeds (stderr: {err})");
    assert_eq!(
        out,
        concat!(
            "pub fn second() -> int\n",
            "pub fn open_api(mut xs: list[int]) -> int | none\n",
            "pub extern fn fields(t: type) -> list[field]\n",
            "pub struct rec[t] { n: t, items: list[t] }\n",
            "pub enum tone {\n",
            "  quiet\n",
            "  loud\n",
            "  payload(int, string)\n",
            "  shaped { code: int }\n",
            "}\n",
            "pub trait speak { fn speak(self) -> string }\n",
            "pub const answer: int = 42\n",
            "pub type id[t] = t\n",
            "pub tag unit\n",
        ),
        "the golden .dsi rendering: {out}"
    );
    fs::remove_dir_all(&root).expect("the scratch root removes");
}

/// The file target renders the one member's surface alone.
#[test]
fn a_file_target_renders_its_own_surface() {
    let root = pkg_root("file");
    let (ok, out, err) = ds_info(&root.join("lib.ds"));
    assert!(ok, "info succeeds (stderr: {err})");
    assert_eq!(
        out,
        concat!(
            "pub fn open_api(mut xs: list[int]) -> int | none\n",
            "pub extern fn fields(t: type) -> list[field]\n",
            "pub struct rec[t] { n: t, items: list[t] }\n",
            "pub enum tone {\n",
            "  quiet\n",
            "  loud\n",
            "  payload(int, string)\n",
            "  shaped { code: int }\n",
            "}\n",
            "pub trait speak { fn speak(self) -> string }\n",
            "pub const answer: int = 42\n",
            "pub type id[t] = t\n",
            "pub tag unit\n",
        ),
        "the member's own .dsi: {out}"
    );
    fs::remove_dir_all(&root).expect("the scratch root removes");
}

/// A missing target and a root with no .ds member refuse with one clean
/// line.
#[test]
fn a_missing_or_empty_target_refuses() {
    let (ok, _, err) = ds_info(Path::new("/tmp/ds-cli-info-absent-<none>"));
    assert!(!ok);
    assert!(
        err.contains("does not exist"),
        "the refusal names the target: {err}"
    );

    let root = temp("empty");
    let (ok, _, err) = ds_info(&root);
    assert!(!ok);
    assert!(
        err.contains("holds no .ds source member"),
        "the refusal names the emptiness: {err}"
    );
    fs::remove_dir_all(&root).expect("the scratch root removes");
}

/// The `.dsi` over the repo's own stdlib surface — the surface carrying
/// `pub const urls`, whose bare-spelling line a DS1002 refusal would
/// poison with every line behind it — re-parses clean through `ds check`:
/// the document contract checked end to end.
#[test]
fn the_stdlib_surface_reparses_clean() {
    let stdlib = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../crates/ds-stdlib/src");
    let (ok, out, err) = ds_info(&stdlib);
    assert!(ok, "info succeeds (stderr: {err})");
    assert!(
        out.contains("pub const urls = "),
        "the initialized const spells its initializer: {out}"
    );
    let root = temp("reparse");
    let surface = root.join("surface.dsi");
    fs::write(&surface, &out).expect("the surface writes");
    let check = Command::new(env!("CARGO_BIN_EXE_ds"))
        .arg("check")
        .arg("--no-ambient")
        .arg(&surface)
        .output()
        .expect("the ds binary builds with the test");
    assert!(
        check.status.success(),
        "the rendered surface re-parses clean (stdout: {}, stderr: {})",
        String::from_utf8_lossy(&check.stdout),
        String::from_utf8_lossy(&check.stderr)
    );
    fs::remove_dir_all(&root).expect("the scratch root removes");
}
