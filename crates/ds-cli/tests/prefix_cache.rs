//! The checked-prefix `.dsi` cache through the `ds` binary: a cold run
//! persists both script-mode artifacts (the dsmod module and the prefix
//! under `$DS_CACHE/dsi`), a run whose dsmod artifact was evicted takes
//! the prefix hit and reproduces the cold output byte for byte, an edit
//! re-keys both, a corrupt prefix falls back and is repaired, the
//! `--no-cache` switch turns the tier off, a failing program writes
//! nothing, and `ds build` persists a prefix a later run serves. Every
//! spawn points `DS_CACHE` at a private temp directory with both debug
//! observables on, so the tests prove which path a run took without
//! timing it.

use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

mod support;

/// A written zero-parameter entry: the shape the caches serve.
const WRITTEN_MAIN: &str = "fn main() -> () {\n  println(\"warm as toast\")\n}\n";

/// One private cache root per tag: tests never share a root and never
/// touch a real user cache directory.
fn cache_dir(tag: &str) -> PathBuf {
    support::guarded_dir(&format!("ds-cli-prefix-cache-{tag}-"))
}

/// One source file per tag: the cache key folds both the file's stem
/// (the module name) and the content.
fn source_file(tag: &str, text: &str) -> PathBuf {
    let dir = support::guarded_dir("ds-cli-prefix-cache-src-");
    let path = dir.join(format!("{tag}.ds"));
    fs::write(&path, text).expect("the source writes");
    path
}

/// Runs the binary with the private cache root and both debug
/// observables on; the `ds run cache:` and `ds prefix cache:` lines on
/// stderr name the paths each tier took.
fn ds(cache: &Path, args: &[&str]) -> (bool, String, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_ds"))
        .env("DS_CACHE", cache)
        .env("DS_RUN_CACHE_DEBUG", "1")
        .env("DS_PREFIX_CACHE_DEBUG", "1")
        .args(args)
        .output()
        .expect("the ds binary runs");
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

/// Every artifact under the root with the given extension, recursively.
fn artifacts(root: &Path, extension: &str) -> Vec<PathBuf> {
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
            } else if path.extension().is_some_and(|ext| ext == extension) {
                found.push(path);
            }
        }
    }
    found
}

fn run_path(source: &Path) -> String {
    source
        .to_str()
        .expect("the temp source path is UTF-8")
        .to_owned()
}

/// A cold run persists one dsmod and one prefix, an evicted dsmod makes
/// the next run take the prefix hit and reproduce the cold output byte
/// for byte, and the hit run rewrites the evicted dsmod.
#[test]
fn a_cold_run_persists_a_prefix_an_evicted_dsmod_serves() {
    let cache = cache_dir("round-trip");
    let source = source_file("roundtrip", WRITTEN_MAIN);
    let (ok, cold_out, cold_err) = ds(&cache, &["run", &run_path(&source)]);
    assert!(ok, "the cold run succeeds: {cold_err}");
    assert!(
        cold_err.contains("ds prefix cache: miss"),
        "the cold run misses the prefix: {cold_err}"
    );
    assert_eq!(artifacts(&cache, "dsmod").len(), 1, "one dsmod persists");
    assert_eq!(artifacts(&cache, "dsi").len(), 1, "one prefix persists");

    // the eviction: only the dsmod artifact goes away, the run below must
    // re-run the front end over the persisted prefix
    fs::remove_file(&artifacts(&cache, "dsmod")[0]).expect("the dsmod evicts");
    let (ok, warm_out, warm_err) = ds(&cache, &["run", &run_path(&source)]);
    assert!(ok, "the warm run succeeds: {warm_err}");
    assert!(
        warm_err.contains("ds run cache: miss"),
        "the evicted dsmod misses: {warm_err}"
    );
    assert!(
        warm_err.contains("ds prefix cache: hit"),
        "the persisted prefix serves: {warm_err}"
    );
    assert_eq!(cold_out, warm_out, "the runs print identically");
    assert_eq!(artifacts(&cache, "dsmod").len(), 1, "the hit run rewrites");
}

/// An edit re-keys both tiers.
#[test]
fn editing_the_source_rekeys_both_tiers() {
    let cache = cache_dir("rekey");
    let source = source_file("rekey", WRITTEN_MAIN);
    let (ok, ..) = ds(&cache, &["run", &run_path(&source)]);
    assert!(ok);

    fs::write(&source, "fn main() -> () {\n  println(\"edited\")\n}\n").expect("the edit writes");
    let (ok, _, err) = ds(&cache, &["run", &run_path(&source)]);
    assert!(ok, "the edited run succeeds: {err}");
    assert!(
        err.contains("ds run cache: miss") && err.contains("ds prefix cache: miss"),
        "both tiers miss the edit: {err}"
    );
    assert_eq!(
        artifacts(&cache, "dsi").len(),
        2,
        "the edit persists its own prefix"
    );
}

/// A corrupt prefix artifact is a miss the run falls back from and the
/// store repairs.
#[test]
fn a_corrupt_prefix_falls_back_and_is_repaired() {
    let cache = cache_dir("repair");
    let source = source_file("repair", WRITTEN_MAIN);
    let (ok, cold_out, _) = ds(&cache, &["run", &run_path(&source)]);
    assert!(ok);

    // flip one byte in the payload's middle: a digest the serve must
    // refuse, whatever the flip decodes to
    let artifact = artifacts(&cache, "dsi")[0].clone();
    let mut bytes = fs::read(&artifact).expect("the artifact reads");
    bytes[40] ^= 0xff;
    fs::write(&artifact, &bytes).expect("the corrupt bytes write");

    fs::remove_file(&artifacts(&cache, "dsmod")[0]).expect("the dsmod evicts");
    let (ok, warm_out, err) = ds(&cache, &["run", &run_path(&source)]);
    assert!(ok, "the run over the corrupt prefix succeeds: {err}");
    assert!(
        err.contains("ds prefix cache: miss"),
        "the corrupt prefix misses: {err}"
    );
    assert_eq!(cold_out, warm_out, "the fallback answers the cold output");

    // the store repaired the artifact
    let (ok, _, err) = ds(&cache, &["run", &run_path(&source)]);
    assert!(ok, "the repaired run succeeds: {err}");
    fs::remove_file(&artifacts(&cache, "dsmod")[0]).expect("the dsmod evicts");
    let (ok, _, err) = ds(&cache, &["run", &run_path(&source)]);
    assert!(ok, "the run after repair succeeds: {err}");
    assert!(
        err.contains("ds prefix cache: hit"),
        "the repaired prefix serves: {err}"
    );
}

/// The off switch disables both tiers: no consult, no write.
#[test]
fn no_cache_disables_the_prefix_tier() {
    let cache = cache_dir("off");
    let source = source_file("off", WRITTEN_MAIN);
    let (ok, _, err) = ds(&cache, &["run", "--no-cache", &run_path(&source)]);
    assert!(ok, "the uncached run succeeds: {err}");
    assert!(
        !err.contains("ds prefix cache: miss") && !err.contains("ds prefix cache: hit"),
        "no prefix lookup ran: {err}"
    );
    assert!(artifacts(&cache, "dsi").is_empty(), "nothing persists");
    assert!(artifacts(&cache, "dsmod").is_empty(), "nothing persists");
}

/// A program that fails to check writes no prefix.
#[test]
fn a_checking_failure_writes_nothing() {
    let cache = cache_dir("failure");
    let source = source_file(
        "failure",
        "fn main() -> () {\n  println(there_is_no_such_name)\n}\n",
    );
    let (ok, ..) = ds(&cache, &["run", &run_path(&source)]);
    assert!(!ok, "the failing run refuses");
    assert!(artifacts(&cache, "dsi").is_empty(), "no prefix persists");
    assert!(artifacts(&cache, "dsmod").is_empty(), "no dsmod persists");
}

/// `ds build` persists the prefix a later `ds run` of the same source
/// serves.
#[test]
fn a_build_persists_what_a_run_serves() {
    let cache = cache_dir("build");
    let source = source_file("built", WRITTEN_MAIN);
    let out = cache.join("built.dsmod");
    let (ok, _, err) = ds(&cache, &[
        "build",
        &run_path(&source),
        "--out",
        &out.to_string_lossy(),
    ]);
    assert!(ok, "the build succeeds: {err}");
    assert_eq!(
        artifacts(&cache, "dsi").len(),
        1,
        "the build persists a prefix"
    );

    fs::remove_file(&out).expect("the built dsmod evicts");
    let (ok, run_out, err) = ds(&cache, &["run", &run_path(&source)]);
    assert!(ok, "the run succeeds: {err}");
    assert!(
        err.contains("ds prefix cache: hit"),
        "the build's prefix serves the run: {err}"
    );
    assert_eq!(run_out, "warm as toast\n0\n", "the run prints the program");
}
