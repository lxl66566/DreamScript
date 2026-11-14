//! The script-mode dsmod cache through the `ds` binary: a cold run
//! writes exactly one artifact, a warm run takes the hit path and
//! reproduces the cold output byte for byte, an edit re-keys, a corrupt
//! artifact falls back and is repaired, a failing program writes
//! nothing, an unusable cache root still runs, the off switch forces
//! the cold path, and a parameterized entry — staging from the
//! artifact's recorded entry plan and member-tag table — serves warm
//! with arguments staged identically to the cold path, while an entry
//! spelling the artifact records no plan for (a synthesized member)
//! stays cold. Every spawn points `DS_CACHE` at a private temp directory
//! with the debug observable on, so the tests prove which path a run
//! took without timing it.

use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

/// A written zero-parameter entry: the shape the cache serves.
const WRITTEN_MAIN: &str = "fn main() -> () {\n  println(\"warm as toast\")\n}\n";

/// A parameterized entry: stages from the artifact's recorded plan (the
/// optional's two-word union span and its member tags).
const PARAM_MAIN: &str = "fn main(port: int?) -> () {\n  if port is none {\n    println(\"no \
                          port\")\n  } else {\n    println(\"port\")\n  }\n}\n";

/// One private cache root per tag: tests never share a root and never
/// touch a real user cache directory.
fn cache_dir(tag: &str) -> PathBuf {
    let dir = env::temp_dir().join(format!("ds-cli-run-cache-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("the private cache root creates");
    dir
}

/// One source file per tag: the cache key folds both the file's stem
/// (the module name) and the content.
fn source_file(tag: &str, text: &str) -> PathBuf {
    let dir = env::temp_dir().join(format!("ds-cli-run-cache-src-{}", std::process::id()));
    fs::create_dir_all(&dir).expect("the source directory creates");
    let path = dir.join(format!("{tag}.ds"));
    fs::write(&path, text).expect("the source writes");
    path
}

/// Runs the binary with the private cache root and the debug observable
/// on; the `ds run cache:` line on stderr names the path the run took.
fn ds_run(cache: &Path, args: &[&str]) -> (bool, String, String) {
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

/// Every `.dsmod` under the root, recursively: the artifact count is
/// the write observable.
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

fn run_path(source: &Path) -> String {
    source
        .to_str()
        .expect("the temp source path is UTF-8")
        .to_owned()
}

#[test]
fn cold_writes_one_artifact_and_warm_hits_identically() {
    let cache = cache_dir("warm");
    let source = source_file("warm", WRITTEN_MAIN);

    let (ok, cold_out, cold_err) = ds_run(&cache, &["run", &run_path(&source)]);
    assert!(ok, "the cold run succeeds (stderr: {cold_err})");
    assert_eq!(cold_out, "warm as toast\n0\n");
    assert!(
        cold_err.contains("cache: miss"),
        "cold reports miss: {cold_err}"
    );
    assert_eq!(artifacts(&cache).len(), 1, "exactly one artifact lands");

    let (ok, warm_out, warm_err) = ds_run(&cache, &["run", &run_path(&source)]);
    assert!(ok, "the warm run succeeds (stderr: {warm_err})");
    assert_eq!(warm_out, cold_out, "the warm run prints the cold output");
    assert!(
        warm_err.contains("cache: hit"),
        "the warm run hits: {warm_err}"
    );
    assert_eq!(artifacts(&cache).len(), 1, "the hit path writes nothing");

    // the top-level-statement script (the synthesized main) serves the
    // same way: it is a zero-parameter entry too
    let stmt = source_file("stmt", "println(\"statement script\")\n");
    let (ok, out, err) = ds_run(&cache, &["run", &run_path(&stmt)]);
    assert!(ok, "the statement script succeeds (stderr: {err})");
    assert_eq!(out, "statement script\n0\n");
    assert!(
        err.contains("cache: miss"),
        "the statement script misses first: {err}"
    );
    let (ok, out, err) = ds_run(&cache, &["run", &run_path(&stmt)]);
    assert!(ok, "the warm statement script succeeds (stderr: {err})");
    assert_eq!(out, "statement script\n0\n");
    assert!(
        err.contains("cache: hit"),
        "the warm statement script hits: {err}"
    );

    let _ = fs::remove_dir_all(&cache);
}

#[test]
fn staged_args_on_a_served_entry_refuse_identically() {
    // a zero-parameter entry with a staged argument list refuses at the
    // staging plan's count diagnostic on both paths
    let cache = cache_dir("args");
    let source = source_file("args", WRITTEN_MAIN);
    let (ok, _, err) = ds_run(&cache, &["run", &run_path(&source)]);
    assert!(ok, "the cold run succeeds (stderr: {err})");

    let (ok, out, err) = ds_run(&cache, &["run", &run_path(&source), "--args", "7"]);
    assert!(!ok, "a staged list against a zero-parameter entry fails");
    assert_eq!(out, "", "stdout stays clean");
    assert!(
        err.contains("cache: hit"),
        "the artifact still serves: {err}"
    );
    assert!(
        err.contains("--args stages 1 value(s) but entry `args` takes 0 parameter(s)"),
        "the hit path answers the compile path's count refusal: {err}"
    );
    let _ = fs::remove_dir_all(&cache);
}

#[test]
fn editing_the_source_rekeys() {
    let cache = cache_dir("rekey");
    let source = source_file("rekey", WRITTEN_MAIN);
    let (ok, _, err) = ds_run(&cache, &["run", &run_path(&source)]);
    assert!(ok, "the first run succeeds (stderr: {err})");
    assert_eq!(artifacts(&cache).len(), 1);

    let edited = WRITTEN_MAIN.replace("warm as toast", "edited");
    fs::write(&source, &edited).expect("the edit writes");
    let (ok, out, err) = ds_run(&cache, &["run", &run_path(&source)]);
    assert!(ok, "the edited run succeeds (stderr: {err})");
    assert_eq!(out, "edited\n0\n");
    assert!(err.contains("cache: miss"), "an edit re-keys: {err}");
    assert_eq!(
        artifacts(&cache).len(),
        2,
        "the edit lands a second artifact"
    );

    // the first artifact still serves the first content
    fs::write(&source, WRITTEN_MAIN).expect("the revert writes");
    let (ok, out, err) = ds_run(&cache, &["run", &run_path(&source)]);
    assert!(ok, "the reverted run succeeds (stderr: {err})");
    assert_eq!(out, "warm as toast\n0\n");
    assert!(
        err.contains("cache: hit"),
        "the first artifact still serves: {err}"
    );
    let _ = fs::remove_dir_all(&cache);
}

#[test]
fn a_corrupt_artifact_falls_back_and_is_repaired() {
    let cache = cache_dir("corrupt");
    let source = source_file("corrupt", WRITTEN_MAIN);
    let (ok, _, err) = ds_run(&cache, &["run", &run_path(&source)]);
    assert!(ok, "the cold run succeeds (stderr: {err})");
    let artifact = artifacts(&cache)[0].clone();

    let bytes = fs::read(&artifact).expect("the artifact reads");
    fs::write(&artifact, &bytes[..bytes.len() / 2]).expect("the truncation writes");
    let (ok, out, err) = ds_run(&cache, &["run", &run_path(&source)]);
    assert!(
        ok,
        "the run over a truncated artifact succeeds (stderr: {err})"
    );
    assert_eq!(out, "warm as toast\n0\n");
    assert!(
        err.contains("cache: miss"),
        "a truncated artifact is a miss: {err}"
    );

    let (ok, out, err) = ds_run(&cache, &["run", &run_path(&source)]);
    assert!(ok, "the repaired-cache run succeeds (stderr: {err})");
    assert_eq!(out, "warm as toast\n0\n");
    assert!(
        err.contains("cache: hit"),
        "the miss repaired the artifact: {err}"
    );
    assert_eq!(artifacts(&cache).len(), 1, "the repair overwrites in place");
    let _ = fs::remove_dir_all(&cache);
}

#[test]
fn a_checking_failure_writes_nothing() {
    let cache = cache_dir("fails");
    let source = source_file(
        "fails",
        "fn main() -> () {\n  println(there_is_no_such_name)\n}\n",
    );
    let (ok, out, err) = ds_run(&cache, &["run", &run_path(&source)]);
    assert!(!ok, "a program that does not check clean fails");
    assert_eq!(out, "");
    assert!(
        err.contains("the source does not check clean"),
        "the diagnostics render as before: {err}"
    );
    assert!(
        artifacts(&cache).is_empty(),
        "only a successful compile is cached"
    );
    let _ = fs::remove_dir_all(&cache);
}

#[test]
fn an_unusable_cache_root_still_runs() {
    // a cache root below an ordinary file: no directory can be created
    // there and no artifact can be read — the run must not notice
    let blocker = env::temp_dir().join(format!("ds-cli-run-cache-blocker-{}", std::process::id()));
    fs::write(&blocker, b"").expect("the blocker writes");
    let source = source_file("unusable", WRITTEN_MAIN);
    let (ok, out, err) = ds_run(&blocker, &["run", &run_path(&source)]);
    assert!(ok, "the run over an unusable root succeeds (stderr: {err})");
    assert_eq!(out, "warm as toast\n0\n");
    assert!(
        err.contains("cache: miss"),
        "the unusable root reads as a miss: {err}"
    );
    let _ = fs::remove_file(&blocker);
}

#[test]
fn no_cache_disables_the_lookup_and_the_store() {
    let cache = cache_dir("off");
    let source = source_file("off", WRITTEN_MAIN);
    let (ok, out, _) = ds_run(&cache, &["run", &run_path(&source)]);
    assert!(ok, "the cold run succeeds");
    assert_eq!(artifacts(&cache).len(), 1);

    let (ok, out2, err) = ds_run(&cache, &["run", &run_path(&source), "--no-cache"]);
    assert!(ok, "the off run succeeds (stderr: {err})");
    assert_eq!(out2, out, "the off run prints the same output");
    assert!(
        err.contains("cache: off"),
        "the off switch reports itself: {err}"
    );
    assert_eq!(artifacts(&cache).len(), 1, "the off switch writes nothing");
    let _ = fs::remove_dir_all(&cache);
}

#[test]
fn a_parameterized_entry_stages_identically_warm_and_cold() {
    // the staged optional's cold output, from a fresh root, is the
    // byte-identical reference the warm run must reproduce
    let cold = cache_dir("param-cold");
    let cold_source = source_file("paramcold", PARAM_MAIN);
    let (ok, cold_out, err) = ds_run(&cold, &["run", &run_path(&cold_source), "--args", "8080"]);
    assert!(ok, "the cold staged optional runs (stderr: {err})");
    assert_eq!(cold_out, "port\n0\n");
    let _ = fs::remove_dir_all(&cold);

    let cache = cache_dir("param");
    let source = source_file("param", PARAM_MAIN);
    let (ok, out, err) = ds_run(&cache, &["run", &run_path(&source)]);
    assert!(ok, "the parameterized entry runs (stderr: {err})");
    assert_eq!(out, "no port\n0\n");
    assert!(
        err.contains("cache: miss"),
        "the first run compiles in-process: {err}"
    );
    assert_eq!(artifacts(&cache).len(), 1, "the parameterized entry caches");

    let (ok, warm_out, warm_err) = ds_run(&cache, &["run", &run_path(&source)]);
    assert!(ok, "the warm parameterized entry runs (stderr: {warm_err})");
    assert_eq!(warm_out, out, "the warm run prints the cold output");
    assert!(
        warm_err.contains("cache: hit"),
        "the parameterized entry serves warm: {warm_err}"
    );

    let (ok, staged_out, staged_err) =
        ds_run(&cache, &["run", &run_path(&source), "--args", "8080"]);
    assert!(ok, "the warm staged optional runs (stderr: {staged_err})");
    assert_eq!(
        staged_out, cold_out,
        "the warm staged run prints the cold staged output"
    );
    assert!(
        staged_err.contains("cache: hit"),
        "the staged optional serves warm: {staged_err}"
    );
    assert_eq!(artifacts(&cache).len(), 1, "the hit path writes nothing");
    let _ = fs::remove_dir_all(&cache);
}

#[test]
fn an_entry_flag_onto_a_parameterized_lambda_serves_warm() {
    let cache = cache_dir("entryflag");
    let source = source_file(
        "entryflag",
        "fn worker(n: int) -> int {\n  n +% 1\n}\nfn main() -> () {\n  println(\"string \
         ${worker(1)}\")\n}\n",
    );
    let (ok, out, err) = ds_run(&cache, &[
        "run",
        &run_path(&source),
        "--entry",
        "worker",
        "--args",
        "41",
    ]);
    assert!(ok, "the flagged entry runs cold (stderr: {err})");
    assert_eq!(out, "42\n");
    assert!(
        err.contains("cache: miss"),
        "the flagged entry caches: {err}"
    );
    assert_eq!(artifacts(&cache).len(), 1);

    let (ok, warm_out, warm_err) = ds_run(&cache, &[
        "run",
        &run_path(&source),
        "--entry",
        "worker",
        "--args",
        "41",
    ]);
    assert!(ok, "the flagged entry runs warm (stderr: {warm_err})");
    assert_eq!(warm_out, out, "the warm flagged run prints the cold output");
    assert!(
        warm_err.contains("cache: hit"),
        "the flagged parameterized lambda serves warm: {warm_err}"
    );
    let _ = fs::remove_dir_all(&cache);
}

#[test]
fn an_entry_flag_on_a_synthesized_member_stays_cold() {
    // `unit#make` is a parameterized function the artifact records no
    // entry plan for (synthesized member spelling), so every run
    // bypasses the artifact and compiles — staging raw, exactly as the
    // compile path does for a name that is no source binding
    let cache = cache_dir("member");
    let source = source_file(
        "member",
        "struct unit {}\nimpl unit {\n  fn make(w: int) -> int {\n    w +% w\n  }\n}\nfn main() \
         -> () {\n  println(\"string ${unit.make(21)}\")\n}\n",
    );
    let (ok, _, err) = ds_run(&cache, &["run", &run_path(&source)]);
    assert!(ok, "the plain run succeeds (stderr: {err})");
    assert_eq!(artifacts(&cache).len(), 1);

    for _ in 0..2 {
        let (ok, out, err) = ds_run(&cache, &[
            "run",
            &run_path(&source),
            "--entry",
            "unit#make",
            "--args",
            "21",
        ]);
        assert!(ok, "the synthesized-member entry runs (stderr: {err})");
        assert_eq!(out, "42\n");
        assert!(
            err.contains("cache: bypassed"),
            "a synthesized member spelling never serves: {err}"
        );
    }
    assert_eq!(artifacts(&cache).len(), 1, "the bypass path writes nothing");
    let _ = fs::remove_dir_all(&cache);
}

#[test]
fn a_foreign_parameter_refuses_identically_warm_and_cold() {
    // a Foreign plan rides the artifact as a refusal fact, so the warm
    // run answers the compile path's staging refusal verbatim
    let cache = cache_dir("foreign");
    let source = source_file(
        "foreign",
        "fn main(name: string) -> () {\n  println(name)\n}\n",
    );
    let (_, cold_out, cold_err) = ds_run(&cache, &["run", &run_path(&source), "--args", "7"]);
    assert!(
        cold_err.contains("--args cannot stage entry `foreign`'s parameter 1"),
        "the cold run refuses the string parameter: {cold_err}"
    );
    let (_, warm_out, warm_err) = ds_run(&cache, &["run", &run_path(&source), "--args", "7"]);
    assert!(
        warm_err.contains("cache: hit"),
        "the foreign-parameter entry still serves: {warm_err}"
    );
    assert_eq!(warm_out, cold_out, "stdout stays clean on both paths");
    assert!(
        warm_err.contains("--args cannot stage entry `foreign`'s parameter 1"),
        "the warm run answers the same refusal: {warm_err}"
    );
    let _ = fs::remove_dir_all(&cache);
}

#[test]
fn an_absent_entry_names_the_module_from_the_hit_path() {
    let cache = cache_dir("absent");
    let source = source_file("absent", WRITTEN_MAIN);
    let (ok, _, err) = ds_run(&cache, &["run", &run_path(&source)]);
    assert!(ok, "the cold run succeeds (stderr: {err})");

    let (ok, out, err) = ds_run(&cache, &["run", &run_path(&source), "--entry", "nope"]);
    assert!(!ok, "an absent entry fails");
    assert_eq!(out, "");
    assert!(
        err.contains("cache: hit"),
        "an absent entry still serves from the cache: {err}"
    );
    assert!(
        err.contains("module `absent` has no function `nope`"),
        "the hit path reports the missing entry as before: {err}"
    );
    let _ = fs::remove_dir_all(&cache);
}
