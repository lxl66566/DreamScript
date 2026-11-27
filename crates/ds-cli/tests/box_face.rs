//! End-to-end through the `ds` binary over the real ambient: the owning
//! container face and its refusals. The tree fixture builds a
//! two-child tree declared through `box`, walks it, instantiates a
//! generic fn per held shape, unboxes at the last share (the container
//! freeing) and over a surviving holder (a copy), compares and shows
//! through the held value, and dies with the wrapper's deinit firing
//! while the branch cascade frees the tree. The refusal legs pin the
//! classes the round keeps: the cyclic-field refusal unchanged for the
//! spellings without `box` (the check stays clean, the run refuses at
//! the depth budget), the `~Copyable` value refusing the constructor at
//! the check, and a callable held value refusing the structural
//! equality at the plan.

use std::{
    env, fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

mod support;

fn workdir(name: &str) -> PathBuf {
    support::guarded_dir(&format!("ds-cli-box-face-{name}-"))
}

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// Runs the command, removes the working directory, answers the process
/// output.
fn run(command: &mut Command, dir: &Path) -> Output {
    let out = command.output().expect("the binary runs");
    let _ = fs::remove_dir_all(dir);
    out
}

fn ds_in(dir: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ds"));
    command.env("DS_CACHE", dir.join("cache"));
    command
}

/// The tree fixture's golden: the two-child tree walks to three, the
/// held values cross the generic instantiations, and the wrapper's
/// deinit fires at the unique death after every read.
#[test]
fn the_box_face_builds_walks_and_dies() {
    let dir = workdir("tree");
    let out = run(ds_in(&dir).arg("run").arg(fixture("box_face.ds")), &dir);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        concat!(
            "tree 3 boxes 5 6\n",
            "root 2\n",
            "held hey\n",
            "unbox 42\n",
            "taken move me\n",
            "kept 3 still 3\n",
            "eq true true\n",
            "eqstr true\n",
            "eqnest true\n",
            "eqlist true true\n",
            "eqwrap true\n",
            "show box(3) box(box(7))\n",
            "derived wrap { n: 1, b: box(x) }\n",
            "tracked 4 dies\n",
        ),
        "the box tree's golden stdout drifted"
    );
}

/// The cyclic-field refusal stays for the spelling without `box`: the
/// declaration checks clean (the refusal is the lowering's own) and the
/// run refuses at the depth budget naming the finite-span class.
#[test]
fn the_cyclic_field_refusal_stays_without_box() {
    let dir = workdir("cyclic");
    let checked = run(ds_in(&dir).arg("check").arg(fixture("box_cyclic.ds")), &dir);
    assert!(
        checked.status.success(),
        "the cyclic declaration checks clean: {}",
        String::from_utf8_lossy(&checked.stderr)
    );
    let executed = run(ds_in(&dir).arg("run").arg(fixture("box_cyclic.ds")), &dir);
    assert!(!executed.status.success(), "{executed:?}");
    let stderr = String::from_utf8_lossy(&executed.stderr);
    assert!(
        stderr.contains("nests past the depth budget"),
        "the run refuses at the depth budget: {stderr}"
    );
}

/// A `~Copyable` value refuses the constructor at the check: the
/// container's death owns counts, never claims, so the value's deinit
/// has no firing position.
#[test]
fn the_linear_held_value_refuses_the_box() {
    let dir = workdir("linear");
    let out = run(
        ds_in(&dir)
            .arg("check")
            .arg(fixture("box_linear_refusal.ds")),
        &dir,
    );
    assert!(!out.status.success(), "{out:?}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("refuses the box"),
        "the check refuses the linear held value: {stderr}"
    );
}

/// The structural equality descends into the held value: the container's
/// word never compares, a callable held value (no content equality)
/// refusing the chain at the plan.
#[test]
fn the_box_equality_descends_and_gates_the_held_value() {
    let dir = workdir("equality");
    let out = run(ds_in(&dir).arg("run").arg(fixture("box_eq_gate.ds")), &dir);
    assert!(!out.status.success(), "{out:?}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("descends into the held value"),
        "the run refuses the callable held value: {stderr}"
    );
}
