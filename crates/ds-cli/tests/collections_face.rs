//! The collection face end to end through the `ds` binary (the
//! member-tier surface): the taught spellings present over the ambient —
//! the `list[t]()`/`map[k, v]()` empty constructors, the inherent member
//! spellings over the native seams (`xs.push(x)` and `m.set(k, v)`
//! mutating in place and answering the collection, `xs.get(i)` and
//! `m.get(k)` the checked reads, `xs.concat(ys)` the joined list),
//! `xs.len()` through the free generic's
//! List instance, `m.keys()` through the ambient's inherent member over
//! the map's ordered key face, and split/join over the one char
//! separator — while a ds-bodied member over the collection's own type
//! variables keeps its lowering refusal: an element-carrying generic
//! member body has no static word kind (the seam pinned generically in
//! `generic_impl_refusals.rs`).

use std::{
    env, fs,
    path::PathBuf,
    process::{Command, Output},
};

mod support;

fn ds() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ds"));
    command.current_dir(env!("CARGO_MANIFEST_DIR"));
    command.env("DS_CACHE", test_cache());
    command
}

/// A private per-thread script-cache root: spawned runs never read or
/// write a real user cache directory, and the thread's exit removes it.
fn test_cache() -> PathBuf {
    support::shared_dir("ds-cli-test-cache-")
}

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn run(command: &mut Command) -> Output {
    command.output().expect("the binary runs")
}

/// The acceptance: every taught spelling answers through the ambient in
/// source mode — the unit entry's trailing `0` line is the script-mode
/// convention.
#[test]
fn the_member_spellings_run_over_the_ambient() {
    let out = run(ds().arg("run").arg(fixture("collections_face.ds")));
    assert!(out.status.success(), "the run failed: {out:?}");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        concat!(
            "len 2 3 2\n",
            "keys 2\n",
            "csv alpha,beta,gamma\n",
            "empty 0 0\n",
            "concat 2\n",
            "get 2 2\n",
            "set 9 1 20\n",
            "clear 1 7\n",
            "map 1 true false\n",
            "entries 2\n",
            "0\n",
        )
    );
}

/// The mutation and membership members answer through the ambient in
/// source mode: the indexed replacement, splice, and removal over the
/// same mut windows, the stride-resetting emptying, the map's row
/// removal (a missing key a no-op), the membership read, and the
/// entry-order pair list — with the trap an out-of-range indexed store
/// answers carrying its own report line.
#[test]
fn the_mutation_and_membership_members_run_over_the_ambient() {
    let out = run(ds().arg("run").arg(fixture("collections_members.ds")));
    assert!(out.status.success(), "the run failed: {out:?}");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        concat!(
            "spliced 9 1 20\n",
            "cleared 7\n",
            "removed true false 1\n",
            "pairs two=2\n",
            "0\n",
        )
    );
}

/// The `.ok` handler over the receiver's `none` member runs over both
/// word kinds the checked read answers: the checked difference collapses
/// the result to the one flowing atom itself, so the hit arm extracts the
/// payload word out of the operand's layout and the miss arm answers the
/// handler's value, the mirror of `.err` over the payload side.
#[test]
fn the_ok_handler_over_none_runs_over_both_word_kinds() {
    let out = run(ds().arg("run").arg(fixture("ok_handler_over_none.ds")));
    assert!(out.status.success(), "the run failed: {out:?}");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        concat!("hit 1 miss -1\n", "found localhost absent missing\n", "0\n",)
    );
}

/// The handler seam's recorded handled sets run end to end: the annotated
/// `.err` retry handler whose answer re-adds the handled member fires off
/// the checker's record (the site's result type re-derives the set empty,
/// so a lowering-side derivation leaves the handler dead), the un-annotated
/// `.ok` handler over a `t | none` receiver defaults its parameter to the
/// receiver minus `none` when the body walk derives no set, and the
/// un-annotated `.err` handler keeps the difference rule's answer.
#[test]
fn handler_seam_records_run_end_to_end() {
    let out = run(ds().arg("run").arg(fixture("handler_seam_faces.ds")));
    assert!(out.status.success(), "the run failed: {out:?}");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        concat!(
            "retried 21 direct 42\n",
            "hit 6 miss 0\n",
            "caught -7 kept 42\n",
            "0\n",
        )
    );
}

/// The built round trip reproduces the source run exactly.
#[test]
fn built_module_reproduces_the_source_run() {
    let module = env::temp_dir().join(format!(
        "ds_collections_face_probe-{}.dsmod",
        std::process::id()
    ));
    let _ = fs::remove_file(&module);
    let build = run(ds()
        .arg("build")
        .arg(fixture("collections_face.ds"))
        .arg("-o")
        .arg(&module));
    assert!(build.status.success(), "the build failed: {build:?}");
    let out = run(ds().arg("run").arg(&module));
    assert!(out.status.success(), "the run failed: {out:?}");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        concat!(
            "len 2 3 2\n",
            "keys 2\n",
            "csv alpha,beta,gamma\n",
            "empty 0 0\n",
            "concat 2\n",
            "get 2 2\n",
            "set 9 1 20\n",
            "clear 1 7\n",
            "map 1 true false\n",
            "entries 2\n",
            "0\n",
        )
    );
    let _ = fs::remove_file(&module);
}

/// The retired spellings: every prefix form (the plain call and the
/// UFCS dot) refuses at the check with the private-tier diagnostic
/// naming its member replacement.
#[test]
fn the_retired_prefix_spellings_refuse_naming_the_member() {
    let check = run(ds()
        .arg("check")
        .arg(fixture("collections_prefix_refusal.ds")));
    assert_ne!(check.status.code(), Some(0), "the run failed: {check:?}");
    let stderr = String::from_utf8_lossy(&check.stderr);
    for retired in [
        "`list_push` is private to its defining file [G4]; the member spelling is `xs.push(x)`",
        "`list_get` is private to its defining file [G4]; the member spelling is `xs.get(i)`",
        "`map_set` is private to its defining file [G4]; the member spelling is `m.set(k, v)`",
        "`map_get` is private to its defining file [G4]; the member spelling is `m.get(k)`",
        "`map_keys` is private to its defining file [G4]; the member spelling is `m.keys()`",
        "`list_len` is private to its defining file [G4]; the member spelling is `xs.len()`",
        "`list_concat` is private to its defining file [G4]; the member spelling is \
         `xs.concat(ys)`",
        "`map_len` is private to its defining file [G4]; the member spelling is `m.len()`",
    ] {
        assert!(stderr.contains(retired), "the refusal: {stderr}");
    }
    assert_eq!(
        stderr.matches("is private to its defining file").count(),
        9,
        "every retired use refuses exactly once: {stderr}"
    );
}

/// The refused shape: a ds-bodied member over the collection's own type
/// variables checks clean (exit 0, empty stderr) and refuses only at
/// lowering, naming the member-body seam exactly.
#[test]
fn the_ds_bodied_member_refuses_at_lowering() {
    let check = run(ds()
        .arg("check")
        .arg(fixture("collections_member_refusal.ds")));
    assert_eq!(check.status.code(), Some(0), "{check:?}");
    assert_eq!(check.stdout, [] as [u8; 0]);
    assert_eq!(check.stderr, [] as [u8; 0]);
    let run = run(ds()
        .arg("run")
        .arg(fixture("collections_member_refusal.ds")));
    assert_ne!(run.status.code(), Some(0), "the shape must refuse: {run:?}");
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert!(
        stderr.contains("a member parameter over the impl's type variable"),
        "the refusal names the member-body seam: {stderr}"
    );
    assert!(
        stderr.contains("outside the M3 runtime contract"),
        "the refusal names the runtime contract: {stderr}"
    );
}
