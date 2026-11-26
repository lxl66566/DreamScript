//! The `ds test` runner through the binary: discovery in ascending
//! order, `--list` without running, the pinned per-test lines and the
//! trailing summary, the failing tests' captured output under its
//! fence (a passing test's output stays swallowed), the exit
//! discipline (0 all-pass, 1 any failure), and the bare `no tests`
//! answer on a root without the directory. The script-mode caches ride
//! the re-entry: a warm run answers the cold summaries verbatim, the
//! root and dep-member artifacts serve a package's tests from disk
//! (the debug observable proves which path each run took), an edit to
//! the test file, a dep, or the package's own source re-keys, the
//! re-entry's keys are the run path's own (a manifest-free probe
//! composes the same compile both ways, each command serving the
//! other's artifacts), `--no-cache` runs the same tests and writes
//! nothing.

use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
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
    support::shared_dir("ds-cli-test-cmd-cache-")
}

/// One private cache root per tag: the cache-facing tests never share
/// a root and never touch a real user cache directory.
fn cache_dir(tag: &str) -> PathBuf {
    support::guarded_dir(&format!("ds-cli-test-cmd-cache-{tag}-"))
}

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn out(command: &mut Command) -> String {
    let produced = command.output().expect("the binary runs");
    String::from_utf8_lossy(&produced.stdout).into_owned()
}

/// Runs the binary against one cache root; answers (ok, stdout,
/// stderr). The debug observable rides every spawn that inspects the
/// cache paths.
fn ds_at(cache: &Path, args: &[&str]) -> (bool, String, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_ds"))
        .env("DS_CACHE", cache)
        .env("DS_RUN_CACHE_DEBUG", "1")
        .args(args)
        .output()
        .expect("the ds binary runs");
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

/// The parent runner's stdout with the per-test wall times normalized
/// away: the summary lines' cache-invariant shape.
fn normalized(stdout: &str) -> String {
    let mut lines = Vec::new();
    for line in stdout.lines() {
        let trimmed = line.trim_end();
        if let Some(at) = trimmed.rfind(" (")
            && trimmed.ends_with(')')
            && trimmed[..at].contains(" ... ")
        {
            lines.push(trimmed[..at].to_owned());
            continue;
        }
        lines.push(trimmed.to_owned());
    }
    lines.join("\n")
}

/// Every `.dsmod` under the root, recursively: the write observable.
fn artifacts(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|ext| ext == "dsmod") {
                found.push(path);
            }
        }
    }
    found
}

/// One package root beside a path dep, both programmatic: the dep's
/// one member the root's own member imports, one test file asserting
/// through the pair. Answers (root, dep, test file).
fn dep_fixture(tag: &str, dep_value: u32) -> (PathBuf, PathBuf, PathBuf) {
    let base = support::guarded_dir(&format!("ds-cli-test-cmd-pkg-{tag}-"));
    let dep = base.join("helper");
    let root = base.join("app");
    fs::create_dir_all(dep.join("tests")).expect("the dep tree creates");
    fs::create_dir_all(root.join("tests")).expect("the root tree creates");
    fs::write(
        dep.join("ds.pkg"),
        "name = \"@helper\"\nversion = \"1.0.0\"\n",
    )
    .expect("the dep manifest writes");
    fs::write(
        dep.join("lib.ds"),
        format!("pub fn val() -> int {{\n  {dep_value}\n}}\n"),
    )
    .expect("the dep member writes");
    fs::write(
        root.join("ds.pkg"),
        "name = \"cache_app\"\nversion = \"0.1.0\"\n\n[deps.@helper]\npath = \"../helper\"\n",
    )
    .expect("the root manifest writes");
    fs::write(
        root.join("lib.ds"),
        "import @helper (val)\n\npub fn seven() -> int {\n  val()\n}\n",
    )
    .expect("the root member writes");
    let test = root.join("tests").join("pass.ds");
    fs::write(
        &test,
        "fn main() -> () {\n  if seven() != 7 {\n    die(\"seven drifted\")\n    \
         println(\"unreached\")\n  }\n}\n",
    )
    .expect("the test writes");
    (root, dep, test)
}

fn run_path(path: &Path) -> String {
    path.to_str().expect("the temp path is UTF-8").to_owned()
}

#[test]
fn list_prints_the_discovered_tests_in_ascending_order() {
    let stdout = out(ds().arg("test").arg(fixture("test_pkg")).arg("--list"));
    assert_eq!(
        stdout,
        "tests/fail_exit.ds\ntests/fail_trap.ds\ntests/pass.ds\n"
    );
}

#[test]
fn a_root_without_tests_answers_bare_no_tests_clean() {
    let mut command = ds();
    command.arg("test").arg(fixture("test_pkg_notests"));
    let produced = command.output().expect("the binary runs");
    assert!(produced.status.success(), "no tests is a clean answer");
    assert_eq!(
        String::from_utf8_lossy(&produced.stdout),
        "no tests\n",
        "one line, nothing else"
    );
}

#[test]
fn the_runner_pins_the_lines_the_fence_and_the_summary() {
    let mut command = ds();
    command.arg("test").arg(fixture("test_pkg"));
    let produced = command.output().expect("the binary runs");
    assert_eq!(
        produced.status.code(),
        Some(1),
        "any failure exits 1 (a trap and a deliberate exit(2) failed)"
    );
    let stdout = String::from_utf8_lossy(&produced.stdout).into_owned();
    // ascending order, one line per test, the wall time trailing
    assert!(
        stdout.contains("test tests/fail_exit.ds ... FAILED ("),
        "{stdout}"
    );
    assert!(
        stdout.contains("test tests/fail_trap.ds ... FAILED ("),
        "{stdout}"
    );
    assert!(stdout.contains("test tests/pass.ds ... ok ("), "{stdout}");
    // the failing runs' captured output shows under its fence, the
    // passing run's output stays swallowed
    assert!(stdout.contains("---- tests/fail_trap.ds ----"), "{stdout}");
    assert!(stdout.contains("trap: die: boom"), "{stdout}");
    assert!(stdout.contains("---- tests/fail_exit.ds ----"), "{stdout}");
    assert!(!stdout.contains("unreached"), "{stdout}");
    // the trailing summary closes the run
    assert!(
        stdout.contains("test result: failed. 1 passed; 2 failed; 3 total"),
        "{stdout}"
    );
    // the fence and the summary follow every per-test line
    let last_line = stdout.find("test tests/pass.ds ... ok").expect("pass line");
    let fence = stdout
        .find("---- tests/fail_trap.ds ----")
        .expect("the fence");
    let summary = stdout.find("test result:").expect("the summary");
    assert!(last_line < fence && fence < summary, "{stdout}");
}

#[test]
fn an_all_pass_root_exits_clean_and_swallows_the_output() {
    let mut command = ds();
    command.arg("test").arg(fixture("test_pkg_pass"));
    let produced = command.output().expect("the binary runs");
    assert!(produced.status.success(), "all-pass exits 0");
    let stdout = String::from_utf8_lossy(&produced.stdout).into_owned();
    assert!(stdout.contains("test tests/pass.ds ... ok ("), "{stdout}");
    assert!(
        stdout.contains("test result: ok. 1 passed; 0 failed; 1 total"),
        "{stdout}"
    );
    // a passing test's stdout never reaches the runner's output
    assert!(!stdout.contains("hidden"), "{stdout}");
}

/// A warm run composes from the cache and answers the cold summaries
/// verbatim: the dep member and the composed root land on the cold
/// re-entry and serve warm from disk (the debug observable rides the
/// re-entry child — the parent swallows a passing test's stderr), the
/// artifact count stands still on the hit path, and the wall-clock-
/// normalized parent output is byte-identical across the two runs.
#[test]
fn a_warm_run_serves_the_cache_and_answers_the_cold_summaries() {
    let cache = cache_dir("warm");
    let (root, _dep, test) = dep_fixture("warm", 7);
    let root_arg = run_path(&root);
    let test_arg = run_path(&test);

    // the cold re-entry: both artifacts land, the paths name misses
    let (ok, _, cold_err) = ds_at(&cache, &["test", &root_arg, "--run-one", &test_arg]);
    assert!(ok, "the cold re-entry passes (stderr: {cold_err})");
    assert!(cold_err.contains("cache: miss"), "cold misses: {cold_err}");
    assert!(
        !cold_err.contains("cache: hit"),
        "the cold run never hits: {cold_err}"
    );
    assert_eq!(
        artifacts(&cache).len(),
        2,
        "the composed root and the dep member both land"
    );

    // the full parent run over the cold cache: the pinned summary
    let (ok, cold_out, _) = ds_at(&cache, &["test", &root_arg]);
    assert!(ok, "the parent run passes");
    assert!(
        cold_out.contains("test result: ok. 1 passed; 0 failed; 1 total"),
        "{cold_out}"
    );

    // the warm re-entry: both tiers serve from disk, nothing writes
    let (ok, _, warm_err) = ds_at(&cache, &["test", &root_arg, "--run-one", &test_arg]);
    assert!(ok, "the warm re-entry passes");
    assert!(
        warm_err.contains("ds dep cache: hit"),
        "the dep member serves from disk: {warm_err}"
    );
    assert!(
        warm_err.contains("ds run cache: hit"),
        "the composed root serves from disk: {warm_err}"
    );
    assert_eq!(artifacts(&cache).len(), 2, "the hit path writes nothing");

    // the full parent run over the warm cache: the summaries match the
    // cold run's, wall times aside
    let (ok, warm_out, warm_err) = ds_at(&cache, &["test", &root_arg]);
    assert!(ok, "the warm parent run passes (stderr: {warm_err})");
    assert_eq!(
        normalized(&cold_out),
        normalized(&warm_out),
        "the warm summaries answer the cold ones"
    );
    let _ = fs::remove_dir_all(&cache);
}

/// An edit between runs re-keys: a moved dep's content fails the
/// asserting test (no stale artifact serves the old graph), the
/// content-addressed originals still serve after the restore, and the
/// package's own member's benign edit re-keys the composed root.
#[test]
fn an_edit_between_runs_rekeys_the_verdict() {
    let cache = cache_dir("rekey");
    let (root, dep, test) = dep_fixture("rekey", 7);
    let root_arg = run_path(&root);

    let (ok, _, err) = ds_at(&cache, &["test", &root_arg]);
    assert!(ok, "the first run passes (stderr: {err})");
    assert_eq!(
        artifacts(&cache).len(),
        2,
        "the composed root and the dep member land"
    );

    // the dep's content moves: the composed test asserts the old
    // value, so a stale serve would keep it green — it must fail
    fs::write(dep.join("lib.ds"), "pub fn val() -> int {\n  8\n}\n").expect("the dep edit writes");
    let (ok, out, err) = ds_at(&cache, &["test", &root_arg]);
    assert!(!ok, "the moved dep fails the run (stderr: {err})");
    assert!(
        out.contains("test tests/pass.ds ... FAILED"),
        "the stale assertion surfaces: {out}"
    );
    assert!(
        out.contains("seven drifted"),
        "the trap rides the fence: {out}"
    );
    assert_eq!(
        artifacts(&cache).len(),
        4,
        "the moved graph compiles and lands its own keys"
    );

    // the dep's content restores: the original keys' artifacts are
    // still current, the run passes again with no new artifacts
    fs::write(dep.join("lib.ds"), "pub fn val() -> int {\n  7\n}\n")
        .expect("the dep restore writes");
    let (ok, _, err) = ds_at(&cache, &["test", &root_arg]);
    assert!(ok, "the restored dep passes (stderr: {err})");
    assert_eq!(
        artifacts(&cache).len(),
        4,
        "the restored keys serve the standing artifacts"
    );

    // the package's own member edits: the composed identity moves, the
    // root re-keys (a miss), and the run stays green
    fs::write(
        root.join("lib.ds"),
        "import @helper (val)\n\npub fn seven() -> int {\n  val()\n}\n\npub fn spare() -> int {\n  \
         0\n}\n",
    )
    .expect("the root edit writes");
    // the re-entry child shows the miss (the parent swallows a passing
    // test stderr)
    let (ok, _, err) = ds_at(&cache, &["test", &root_arg, "--run-one", &run_path(&test)]);
    assert!(ok, "the edited root passes (stderr: {err})");
    assert!(
        err.contains("ds run cache: miss"),
        "the sibling edit re-keys the composed root: {err}"
    );
    assert_eq!(artifacts(&cache).len(), 5, "only the re-keyed root lands");
    let _ = fs::remove_dir_all(&cache);
}

/// The re-entry's keys are the run path's own: a manifest-free probe
/// composes the same compile both ways — the implicit `@std` graph,
/// the session shape, the lone member under the same bundle-relative
/// path — so `ds run` and the re-entry land and serve the same
/// artifacts in either order. Any divergence between the two key
/// constructions (a component moved on one path alone, a session
/// shape picked differently) turns the second command into a miss
/// that recompiles and rewrites.
#[test]
fn the_reentry_serves_the_keys_a_run_composes() {
    let root = support::guarded_dir("ds-cli-test-cmd-keyeq-");
    let probe = root.join("probe.ds");
    fs::write(
        &probe,
        concat!(
            "import @std (max, min)\n",
            "\n",
            "fn main() -> int {\n",
            "  println(\"${max(3, 9)} ${min(3, 9)}\")\n",
            "  0\n",
            "}\n",
        ),
    )
    .expect("the probe writes");
    let root_arg = run_path(&root);
    let probe_arg = run_path(&probe);

    // the cold run lands the probe's module and the walk's @std member
    let cache = cache_dir("keyeq-run-first");
    let (ok, _, err) = ds_at(&cache, &["run", &probe_arg]);
    assert!(ok, "the probe run passes (stderr: {err})");
    assert!(
        err.contains("ds run cache: miss"),
        "the run compiles: {err}"
    );
    assert!(
        err.contains("ds dep cache: miss"),
        "the walk compiles @std: {err}"
    );
    assert_eq!(artifacts(&cache).len(), 2, "the probe and @std land");

    // the re-entry serves both tiers from the run's keys and writes
    // nothing
    let (ok, _, err) = ds_at(&cache, &["test", &root_arg, "--run-one", &probe_arg]);
    assert!(ok, "the re-entry passes (stderr: {err})");
    assert!(
        err.contains("ds run cache: hit"),
        "the re-entry serves the run's root: {err}"
    );
    assert!(
        err.contains("ds dep cache: hit"),
        "the re-entry serves the run's @std: {err}"
    );
    assert_eq!(artifacts(&cache).len(), 2, "the served path writes nothing");
    let _ = fs::remove_dir_all(&cache);

    // the reverse order answers the same: the re-entry lands, the run
    // serves
    let cache = cache_dir("keyeq-test-first");
    let (ok, _, err) = ds_at(&cache, &["test", &root_arg, "--run-one", &probe_arg]);
    assert!(ok, "the cold re-entry passes (stderr: {err})");
    assert!(
        err.contains("ds run cache: miss"),
        "the re-entry compiles: {err}"
    );
    assert_eq!(artifacts(&cache).len(), 2, "the re-entry lands both tiers");
    let (ok, _, err) = ds_at(&cache, &["run", &probe_arg]);
    assert!(ok, "the probe run passes (stderr: {err})");
    assert!(
        err.contains("ds run cache: hit"),
        "the run serves the re-entry's root: {err}"
    );
    assert!(
        err.contains("ds dep cache: hit"),
        "the run serves the re-entry's @std: {err}"
    );
    assert_eq!(artifacts(&cache).len(), 2, "the served path writes nothing");
    let _ = fs::remove_dir_all(&cache);
}

/// An edit to the test file itself re-keys the entry: the composed
/// member identity moves, the re-entry misses, and the fresh compile
/// answers the edited verdict — a stale artifact serving the old
/// module would stay green where the edited test dies, and the dep
/// member keeps serving from its standing key.
#[test]
fn an_edit_to_the_test_file_rekeys_the_entry() {
    let cache = cache_dir("entry");
    let (root, _dep, test) = dep_fixture("entry", 7);
    let root_arg = run_path(&root);
    let test_arg = run_path(&test);

    let (ok, _, err) = ds_at(&cache, &["test", &root_arg, "--run-one", &test_arg]);
    assert!(ok, "the cold re-entry passes (stderr: {err})");
    assert_eq!(
        artifacts(&cache).len(),
        2,
        "the composed root and the dep member land"
    );

    // the edited test dies where the old one passed, so a stale serve
    // answers green and the fresh compile fails
    fs::write(
        &test,
        "fn main() -> () {\n  if seven() == 7 {\n    die(\"the stale artifact served\")\n    \
         println(\"unreached\")\n  }\n}\n",
    )
    .expect("the test edit writes");
    let (ok, out, err) = ds_at(&cache, &["test", &root_arg, "--run-one", &test_arg]);
    assert!(!ok, "the edited test fails its run (stderr: {err})");
    assert!(
        err.contains("ds run cache: miss"),
        "the entry edit re-keys the composed root: {err}"
    );
    assert!(
        err.contains("ds dep cache: hit"),
        "the dep member keeps its standing key: {err}"
    );
    let combined = format!("{out}{err}");
    assert!(
        combined.contains("the stale artifact served"),
        "the fresh compile answers the edited die: {combined}"
    );
    assert_eq!(artifacts(&cache).len(), 3, "only the re-keyed root lands");
    let _ = fs::remove_dir_all(&cache);
}

/// `--no-cache` runs the same tests and writes nothing: the parent
/// summary answers the cached spelling's, the re-entry reports the
/// cache off and consults nothing, and no artifact lands anywhere
/// under the root.
#[test]
fn no_cache_runs_the_tests_and_writes_nothing() {
    let cache = cache_dir("nocache");
    let (root, _dep, test) = dep_fixture("nocache", 7);
    let root_arg = run_path(&root);
    let test_arg = run_path(&test);

    let (ok, out, err) = ds_at(&cache, &["test", &root_arg, "--no-cache"]);
    assert!(ok, "the bypassed run passes (stderr: {err})");
    assert!(
        artifacts(&cache).is_empty(),
        "the bypass writes no artifact"
    );
    assert!(
        out.contains("test result: ok. 1 passed; 0 failed; 1 total"),
        "{out}"
    );

    // the cached run fills the root; the bypassed re-entry over the
    // warm cache consults and writes nothing
    let (ok, warm_out, _) = ds_at(&cache, &["test", &root_arg]);
    assert!(ok, "the cached run passes");
    assert_eq!(artifacts(&cache).len(), 2, "the cached run fills");
    let (ok, _, bypass_err) = ds_at(&cache, &[
        "test",
        &root_arg,
        "--run-one",
        &test_arg,
        "--no-cache",
    ]);
    assert!(ok, "the bypassed re-entry passes");
    assert!(
        bypass_err.contains("ds run cache: off"),
        "the re-entry reports the cache off: {bypass_err}"
    );
    assert!(
        !bypass_err.contains("cache: hit") && !bypass_err.contains("cache: miss"),
        "the bypass consults nothing: {bypass_err}"
    );
    assert_eq!(
        artifacts(&cache).len(),
        2,
        "the bypass writes nothing over a warm cache"
    );

    // the flag leaves the parent summaries alone
    let (ok, bypass_out, bypass_err) = ds_at(&cache, &["test", &root_arg, "--no-cache"]);
    assert!(ok, "the bypassed parent run passes (stderr: {bypass_err})");
    assert_eq!(
        normalized(&warm_out),
        normalized(&bypass_out),
        "the flag leaves the summaries alone"
    );
    assert_eq!(artifacts(&cache).len(), 2, "the bypass writes nothing");
    let _ = fs::remove_dir_all(&cache);
}
