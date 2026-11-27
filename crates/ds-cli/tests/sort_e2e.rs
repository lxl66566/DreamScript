//! The ord-routed list sort end to end through the `ds` binary:
//! the ambient `sort[t: ord]` extern's call sites route the
//! comparator out of the solved element type's serving ord instance —
//! the checker's per-call bound record answers the instance, the
//! lowering mints the instance's `compare` body as the callable word
//! staged beside the list `Ref` (the receiver's list-instance evidence
//! word trailing them like filter's), and the VM's stable merge sort
//! reenters the dispatcher per comparison through the reduce seam's
//! CALLG discipline. A sort over a generic body's own type variable
//! re-solves the bound answer through the lens — the entering site's
//! substitution concretizes the variable, the serving instance's
//! compare body stages, and the instance key carries the baked
//! comparator so two same-shaped structs keep their own orders. The
//! refusals pin the honest boundary left: a sort whose body only
//! lowers as the ONE Raw-word body a variable-naming site enters
//! carries no instance to stage. A std-declaring graph keeps the
//! spelling alive too: the shrunk core declares the extern and the ord
//! impls ride the core's own ambient band.

use std::{
    env, fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

mod support;

/// The binary with the manifest root's cwd and a private script-cache
/// root: spawned runs never read or write a real user cache directory.
fn ds() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ds"));
    command.current_dir(env!("CARGO_MANIFEST_DIR"));
    command.env("DS_CACHE", test_cache());
    command
}

/// A private per-thread script-cache root: the thread's exit removes it.
fn test_cache() -> PathBuf {
    support::shared_dir("ds-cli-sort-cache-")
}

/// Writes one package root (a `ds.pkg` manifest beside the members)
/// under a fresh temp dir and answers the root's path.
fn write_root(tag: &str, manifest: &str, members: &[(&str, &str)]) -> PathBuf {
    let root = support::guarded_dir(&format!("ds-cli-sort-{tag}-"));
    fs::write(root.join("ds.pkg"), manifest).expect("the manifest writes");
    for (path, text) in members {
        fs::write(root.join(path), text).expect("the member writes");
    }
    root
}

fn run(args: &[&str]) -> Output {
    ds().args(args).output().expect("the binary runs")
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn entry_of(root: &Path) -> String {
    root.join("app.ds")
        .to_str()
        .expect("temp path is UTF-8")
        .to_owned()
}

fn fixture(name: &str) -> String {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
        .to_str()
        .expect("fixture path is UTF-8")
        .to_owned()
}

fn std_manifest() -> String {
    let package = ds_stdlib::std_package();
    let identity = package.content_hash();
    let version = package
        .manifest(ds_diag::FileId::new(0))
        .expect("the default bundle's manifest parses")
        .version
        .to_string();
    format!(
        "name = \"app\"\nversion = \"0.1.0\"\n\n[deps]\n@std = {{ ver = \"{version}\", hash = \
         \"blake3:{identity}\" }}\n"
    )
}

/// The acceptance run's expected output.
const FACE_OUT: &str = concat!(
    "picks 9 3 pear apple\n",
    "picked pt { x: 2, y: 1 }\n",
    "ints 1 3\n",
    "ufcs 1 3\n",
    "words apple pear\n",
    "pts pt { x: 1, y: 5 } pt { x: 2, y: 1 }\n",
    "stable 1:x 1:y 2:a 2:b\n",
    "empty 0\n",
    "",
);

/// The acceptance: both spellings sort int and string elements, the
/// derive-ord struct sorts over its multi-word element spans, the
/// custom partial compare keeps equal keys in input order (stability),
/// and the empty list answers the empty list — cold and warm under the
/// same answer.
#[test]
fn the_ord_routed_sort_runs_through_the_bare_script() {
    let entry = fixture("sort_face.ds");
    let cold = run(&["run", &entry]);
    assert_eq!(
        cold.status.code(),
        Some(0),
        "the cold run exits 0: {}",
        text(&cold.stderr)
    );
    assert_eq!(text(&cold.stdout), FACE_OUT, "the cold run answers");
    let warm = run(&["run", &entry]);
    assert_eq!(warm.status.code(), Some(0), "the warm run exits 0");
    assert_eq!(
        text(&warm.stdout),
        FACE_OUT,
        "the warm run answers identically"
    );
}

/// A non-ord element type refuses at the declared bound's own checker
/// seam, before any lowering runs.
#[test]
fn the_sort_refuses_the_non_ord_element() {
    let entry = fixture("sort_non_ord.ds");
    let out = run(&["run", &entry]);
    assert_eq!(out.status.code(), Some(1), "the non-ord element refuses");
    assert!(
        text(&out.stderr).contains("no instance of `ord`"),
        "the refusal names the unsatisfied bound: {}",
        text(&out.stderr)
    );
}

/// A sort over a generic body's own type variable sorts per shape: the
/// bound answer re-solves through the lens at the entering site's
/// substitution, the serving instance's compare body stages as the
/// callable, and the sorted run answers.
#[test]
fn a_sort_over_a_generic_body_s_own_variable_sorts_per_shape() {
    let entry = fixture("sort_variable.ds");
    let out = run(&["run", &entry]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "the variable element sorts: {}",
        text(&out.stderr)
    );
    assert_eq!(text(&out.stdout), "1\n2\n3\n", "the generic body sorts");
}

/// The instance key carries the baked comparator: two same-shaped
/// structs enter distinct instances, each sorting under its own ord
/// instance's compare body — a shape key alone would bake the first
/// entering shape's comparator into the twin's calls.
#[test]
fn the_instance_key_carries_the_baked_comparator() {
    let entry = fixture("sort_share.ds");
    let out = run(&["run", &entry]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "the twin shapes sort: {}",
        text(&out.stderr)
    );
    assert_eq!(
        text(&out.stdout),
        "a 3 2 1\nb 1 2 3\n",
        "each struct's own order answers"
    );
}

/// A sort whose body only lowers as the ONE Raw-word body a
/// variable-naming site enters refuses at lowering: the bound answer
/// carries no instance and the substitution never binds, so no compare
/// body exists to stage.
#[test]
fn the_sort_refuses_inside_a_one_body() {
    let entry = fixture("sort_one_body.ds");
    let out = run(&["run", &entry]);
    assert_eq!(out.status.code(), Some(1), "the ONE-body sort refuses");
    assert!(
        text(&out.stderr).contains("in-scope assumption"),
        "the refusal names the ONE body's seam: {}",
        text(&out.stderr)
    );
}

/// The ord-routed picks over a generic body's own type variables take
/// the same lens re-solve: the primitive widths keep their inherent
/// dispatch, the struct shape stages the solved instance's compare
/// body, and the tail-position pick lowers in place.
#[test]
fn an_ord_pick_over_a_generic_body_s_own_variables_picks() {
    let entry = fixture("pick_variable.ds");
    let out = run(&["run", &entry]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "the variable picks run: {}",
        text(&out.stderr)
    );
    assert_eq!(
        text(&out.stdout),
        "ints 7 3\npt 9\n",
        "the generic picks answer"
    );
}

/// The std-declaring graph keeps the spelling alive: the shrunk core
/// declares the extern itself and the ord impls ride the core's own
/// ambient band, so the package compile sorts over the same seam.
#[test]
fn a_std_declared_root_sorts_over_the_core_band() {
    let face = fs::read_to_string(fixture("sort_face.ds")).expect("the fixture reads");
    let root = write_root("core", &std_manifest(), &[("app.ds", &face)]);
    let out = run(&["run", &entry_of(&root)]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "the std-declared run exits 0: {}",
        text(&out.stderr)
    );
    assert_eq!(text(&out.stdout), FACE_OUT, "the std-declared run answers");
    fs::remove_dir_all(&root).expect("the temp tree removes");
}
