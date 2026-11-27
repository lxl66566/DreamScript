//! End-to-end through the `ds` binary over the real ambient: the
//! read-side aliasing round's two library types. The arena fixture
//! walks the rows the inserts seeded over the at answers' counted
//! value reads, and walks the retire/reuse path; the string_view
//! fixture holds a cursor into a shared base with the bounds-validated
//! construction, the checked char read, the composing slice, the
//! content equality, and the one materializing door. The dangling
//! handle traps through `ds run` with its own report and a failing
//! exit status.

use std::{
    env, fs,
    path::PathBuf,
    process::{Command, Output},
};

mod support;

fn workdir(name: &str) -> PathBuf {
    support::guarded_dir(&format!("ds-cli-arena-face-{name}-"))
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

/// The arena fixture's golden: the at answer reads as a counted value
/// (the push landing on its copy, the stored row keeping its seed),
/// the walk over the seeded rows lands on the third node, and the
/// removal and reuse keep the live count.
#[test]
fn the_arena_face_builds_traverses_and_reuses() {
    let dir = workdir("arena");
    let out = run(
        Command::new(env!("CARGO_BIN_EXE_ds"))
            .arg("run")
            .arg(fixture("arena_face.ds"))
            .env("DS_CACHE", dir.join("cache")),
        &dir,
    );
    assert!(out.status.success(), "{out:?}");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        concat!("copy 2 kept 1\n", "walk 2 home true\n", "len 3 at0 1\n",),
        "the arena's golden stdout drifted"
    );
}

/// The string_view fixture's golden: the validated construction, the
/// checked char read inside and outside the range, the composing slice
/// (refused beyond the view's own bounds), the content equality, and
/// the out-of-range construction answering none.
#[test]
fn the_string_view_face_holds_cursors_without_allocating() {
    let dir = workdir("view");
    let out = run(
        Command::new(env!("CARGO_BIN_EXE_ds"))
            .arg("run")
            .arg(fixture("string_view_face.ds"))
            .env("DS_CACHE", dir.join("cache")),
        &dir,
    );
    assert!(out.status.success(), "{out:?}");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        concat!(
            "len 5 text dream\n",
            "c0 d oob ?\n",
            "slice dream eq true\n",
            "beyond-the-view slice refused\n",
            "eq true false\n",
            "range refused\n",
            "",
        ),
        "the view's golden stdout drifted"
    );
}

/// The dangling handle's trap report through `ds run`: the deterministic
/// trap names the handle's own slot and generation halves, the
/// backtrace points at the trapping call's frame, and the run exits
/// failed.
#[test]
fn a_dangling_handle_traps_deterministically_through_the_run() {
    let dir = workdir("trap");
    let path = dir.join("dangling_main.ds");
    fs::write(
        &path,
        concat!(
            "fn main() -> int {\n",
            "  var a = arena[int]()\n",
            "  const stale = a.insert(41)\n",
            "  a.remove(stale)\n",
            "  a.insert(99)\n",
            "  const v = a.at(stale)\n",
            "  v\n",
            "}\n",
        ),
    )
    .expect("scratch source writes");
    let out = run(
        Command::new(env!("CARGO_BIN_EXE_ds"))
            .arg("run")
            .arg(&path)
            .env("DS_CACHE", dir.join("cache")),
        &dir,
    );
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.starts_with(
            "trap: an arena handle names slot 0 at generation 0, which the arena never held or \
             already retired"
        ),
        "the trap names the handle's halves: {stderr}"
    );
    assert!(
        stderr.contains("dangling_main::main"),
        "the backtrace names the trapping frame: {stderr}"
    );
}
