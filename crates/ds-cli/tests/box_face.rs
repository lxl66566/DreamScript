//! End-to-end through the `ds` binary over the real ambient: the owning
//! container round. The tree fixture builds a two-child tree declared
//! through `box`, walks it, instantiates a generic fn per held shape,
//! and dies with the wrapper's deinit firing while the branch cascade
//! frees the tree. The refusal legs pin the classes the round keeps:
//! the cyclic-field refusal unchanged for the spellings without `box`
//! (the check stays clean, the run refuses at the depth budget), the
//! `~Copyable` value refusing the constructor at the check, and the
//! structural equality refusing at the chain.

use std::{
    env, fs,
    path::PathBuf,
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
fn run(command: &mut Command, dir: &PathBuf) -> Output {
    let out = command.output().expect("the binary runs");
    let _ = fs::remove_dir_all(dir);
    out
}

fn ds_in(dir: &PathBuf) -> Command {
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

/// The structural equality refuses at the chain: the held values
/// compare through their own reads, never the container's word.
#[test]
fn the_box_equality_refuses_at_the_chain() {
    let dir = workdir("equality");
    let out = run(
        ds_in(&dir).arg("run").arg(fixture("box_eq_refusal.ds")),
        &dir,
    );
    assert!(!out.status.success(), "{out:?}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("compares held values through their own reads"),
        "the run refuses the box equality: {stderr}"
    );
}
