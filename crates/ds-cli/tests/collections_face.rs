//! The collection face end to end through the `ds` binary (the H5.5
//! member-tier ruling): the taught spellings present over the ambient —
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
