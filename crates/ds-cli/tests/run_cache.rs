//! The script-mode dsmod cache through the `ds` binary: a cold run
//! writes exactly one artifact, a warm run takes the hit path and
//! reproduces the cold output byte for byte, an edit re-keys, a corrupt
//! artifact falls back and is repaired, a failing program writes
//! nothing, an unusable cache root still runs, the off switch forces
//! the cold path, an `args()`-reading program answers the verbatim
//! process arguments identically warm and cold, an entry flag onto a
//! zero-parameter lambda or a synthesized member serves warm, and both
//! artifact writers record the dep list a deps-first load walk resolves
//! over the cached artifact. Every spawn points `DS_CACHE` at
//! a private temp directory with the debug observable on, so the tests
//! prove which path a run took without timing it.

use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

use ds_mod::Hash128;

/// A written zero-parameter entry: the shape the cache serves.
const WRITTEN_MAIN: &str = "fn main() -> () {\n  println(\"warm as toast\")\n}\n";

/// An `args()`-reading entry: the verbatim process arguments, the CLI
/// process surface's argv (the `get` member is the subset's checked read).
const ARGS_MAIN: &str = "fn main() -> () {\n  const argv = args()\n  if len(argv) == 0 {\n    \
                         println(\"no port\")\n  } else {\n    println(\"port \
                         ${argv.get(0).ok((w: string) => w).err((_: none) => \"?\")}\")\n  }\n}\n";

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
fn a_parameterized_entry_reports_the_vm_refusal() {
    // entries bind no parameters: a parameterized `main` refuses at the
    // VM's argument-count gate, cold and warm alike
    let cache = cache_dir("refusal");
    let source = source_file(
        "refusal",
        "fn main(name: string) -> () {\n  println(name)\n}\n",
    );
    let (ok, out, err) = ds_run(&cache, &["run", &run_path(&source)]);
    assert!(!ok, "the cold parameterized entry fails");
    assert_eq!(out, "", "stdout stays clean");
    assert!(
        err.contains("callee expects 1 argument(s)"),
        "the cold run answers the VM's refusal: {err}"
    );

    let (ok, out, err) = ds_run(&cache, &["run", &run_path(&source)]);
    assert!(!ok, "the warm parameterized entry fails");
    assert_eq!(out, "", "stdout stays clean");
    assert!(
        err.contains("cache: hit"),
        "the artifact still serves: {err}"
    );
    assert!(
        err.contains("callee expects 1 argument(s)"),
        "the warm run answers the same refusal: {err}"
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
fn a_linearity_violation_refuses_and_writes_nothing() {
    // The affine gate refuses before the compile that would cache, so a
    // moved-then-used script never produces an artifact and the next run
    // refuses through the same gate instead of serving one.
    let cache = cache_dir("linear");
    let source = source_file(
        "linear",
        concat!(
            "struct res: ~Copyable { n: int }\n",
            "fn take (move r: res) -> int { r.n }\n",
            "fn main() -> () {\n",
            "  const r = res { n: 5 }\n",
            "  const a = take(r)\n",
            "  const b = take(r)\n",
            "}\n",
        ),
    );
    let (ok, out, err) = ds_run(&cache, &["run", &run_path(&source)]);
    assert!(!ok, "the moved-then-used run refuses");
    assert_eq!(out, "", "nothing executes");
    assert!(err.contains("DS3001"), "the move renders: {err}");
    assert!(
        err.contains("the source fails the linearity check"),
        "the refusal names the stage: {err}"
    );
    assert!(
        artifacts(&cache).is_empty(),
        "a refused compile never writes an artifact"
    );
    let (ok, out, err) = ds_run(&cache, &["run", &run_path(&source)]);
    assert!(!ok, "the repeat run refuses, never a served artifact");
    assert_eq!(out, "", "nothing executes");
    assert!(err.contains("DS3001"), "{err}");
    assert!(
        artifacts(&cache).is_empty(),
        "the refused script never writes an artifact"
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
fn argv_reaches_the_program_identically_warm_and_cold() {
    // the argv-bearing cold output, from a fresh root, is the
    // byte-identical reference the warm run must reproduce
    let cold = cache_dir("argv-cold");
    let cold_source = source_file("argvcold", ARGS_MAIN);
    let (ok, cold_out, err) = ds_run(&cold, &["run", &run_path(&cold_source), "--args", "8080"]);
    assert!(ok, "the cold argv run succeeds (stderr: {err})");
    assert_eq!(cold_out, "port 8080\n0\n");
    let _ = fs::remove_dir_all(&cold);

    let cache = cache_dir("argv");
    let source = source_file("argv", ARGS_MAIN);
    let (ok, out, err) = ds_run(&cache, &["run", &run_path(&source)]);
    assert!(ok, "the args-reading entry runs (stderr: {err})");
    assert_eq!(out, "no port\n0\n");
    assert!(
        err.contains("cache: miss"),
        "the first run compiles in-process: {err}"
    );
    assert_eq!(artifacts(&cache).len(), 1, "the args-reading entry caches");

    let (ok, warm_out, warm_err) = ds_run(&cache, &["run", &run_path(&source)]);
    assert!(ok, "the warm args-reading entry runs (stderr: {warm_err})");
    assert_eq!(warm_out, out, "the warm run prints the cold output");
    assert!(
        warm_err.contains("cache: hit"),
        "the args-reading entry serves warm: {warm_err}"
    );

    let (ok, argv_out, argv_err) = ds_run(&cache, &["run", &run_path(&source), "--args", "8080"]);
    assert!(ok, "the warm argv run succeeds (stderr: {argv_err})");
    assert_eq!(
        argv_out, cold_out,
        "the warm argv run prints the cold argv output"
    );
    assert!(
        argv_err.contains("cache: hit"),
        "the argv run serves warm: {argv_err}"
    );
    assert_eq!(artifacts(&cache).len(), 1, "the hit path writes nothing");
    let _ = fs::remove_dir_all(&cache);
}

#[test]
fn an_entry_flag_onto_a_lambda_serves_warm() {
    let cache = cache_dir("entryflag");
    let source = source_file(
        "entryflag",
        "fn worker() -> int {\n  41 +% 1\n}\nfn main() -> () {\n  println(\"string \
         ${worker()}\")\n}\n",
    );
    let (ok, out, err) = ds_run(&cache, &["run", &run_path(&source), "--entry", "worker"]);
    assert!(ok, "the flagged entry runs cold (stderr: {err})");
    assert_eq!(out, "42\n");
    assert!(
        err.contains("cache: miss"),
        "the flagged entry caches: {err}"
    );
    assert_eq!(artifacts(&cache).len(), 1);

    let (ok, warm_out, warm_err) =
        ds_run(&cache, &["run", &run_path(&source), "--entry", "worker"]);
    assert!(ok, "the flagged entry runs warm (stderr: {warm_err})");
    assert_eq!(warm_out, out, "the warm flagged run prints the cold output");
    assert!(
        warm_err.contains("cache: hit"),
        "the flagged lambda serves warm: {warm_err}"
    );
    let _ = fs::remove_dir_all(&cache);
}

#[test]
fn an_entry_flag_on_a_synthesized_member_serves_warm() {
    // `unit#make` is a zero-parameter synthesized member spelling; like
    // every zero-parameter entry it serves warm from the artifact
    let cache = cache_dir("member");
    let source = source_file(
        "member",
        "struct unit {}\nimpl unit {\n  fn make() -> int {\n    21 +% 21\n  }\n}\nfn main() -> () \
         {\n  println(\"string ${unit.make()}\")\n}\n",
    );
    let (ok, _, err) = ds_run(&cache, &["run", &run_path(&source)]);
    assert!(ok, "the plain run succeeds (stderr: {err})");
    assert_eq!(artifacts(&cache).len(), 1);

    for _ in 0..2 {
        let (ok, out, err) = ds_run(&cache, &["run", &run_path(&source), "--entry", "unit#make"]);
        assert!(ok, "the synthesized-member entry runs (stderr: {err})");
        assert_eq!(out, "42\n");
        assert!(
            err.contains("cache: hit"),
            "the flagged synthesized member serves warm: {err}"
        );
    }
    assert_eq!(artifacts(&cache).len(), 1, "the hit path writes nothing");
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

/// The default @std bundle's member identity — the ascending-path fold
/// over path-and-text pairs the implicit resolution records for the
/// manifest-free run's `@std` dep entry, computed over the tree's own
/// member texts (the ambient core member beside the bodied and migrated
/// members' sources).
fn std_member_identity() -> Hash128 {
    let members = [
        (ds_stdlib::AMBIENT_FILE, ds_stdlib::BUNDLE_CORE_SOURCE),
        (ds_stdlib::CODEC_FILE, ds_stdlib::CODEC_SOURCE),
        (ds_stdlib::BODIED_FILE, ds_stdlib::BODIED_SOURCE),
        (ds_stdlib::STD_MEMBERS_FILE, ds_stdlib::STD_MEMBERS_SOURCE),
    ];
    let mut ordered: Vec<&(&str, &str)> = members.iter().collect();
    ordered.sort_by(|left, right| left.0.cmp(right.0));
    let mut buffer = Vec::new();
    for (path, text) in ordered {
        buffer.extend_from_slice(&(path.len() as u64).to_le_bytes());
        buffer.extend_from_slice(path.as_bytes());
        buffer.extend_from_slice(&(text.len() as u64).to_le_bytes());
        buffer.extend_from_slice(text.as_bytes());
    }
    Hash128::compute(&buffer)
}

/// Both artifact writers record the dep list the bare-specifier switch
/// pins: a manifest-free script importing `@std` through two rows records
/// one entry — the package stem at the bundle's member identity, the
/// composed fold the implicit resolution resolves — in both the
/// script-cache artifact and `ds build`'s output, the cold run writes
/// the `@std` dep member's dsmod beside the root's, the warm run
/// serves both families from disk (the debug markers proving the walk
/// never compiled), and the checked-in merged ambient dsmod no longer
/// answers the bundle's identity (the un-merge split the two: the walk
/// refuses the stale artifact naming both hashes).
#[test]
fn both_writers_record_the_dep_list_a_walk_resolves() {
    const IMPORT_MAIN: &str =
        "import @std (concat)\nimport @std (urls)\n\nfn main() -> int {\n  7\n}\n";
    let cache = cache_dir("deps");
    let source = source_file("deps", IMPORT_MAIN);

    let (ok, out, err) = ds_run(&cache, &["run", &run_path(&source)]);
    assert!(ok, "the importing run succeeds (stderr: {err})");
    assert_eq!(out, "7\n");
    let written = artifacts(&cache);
    assert_eq!(
        written.len(),
        2,
        "the root's and the @std dep member's artifacts land: {written:?}"
    );
    let module = written
        .iter()
        .map(|path| {
            ds_mod::read(&fs::read(path).expect("the artifact reads"))
                .expect("the artifact decodes")
        })
        .find(|module| module.name() == "deps")
        .expect("the root artifact lands among the family");
    let recorded = module.deps().to_vec();
    assert_eq!(
        recorded,
        vec![ds_mod::Dep {
            name: "@std".to_owned(),
            content_hash: std_member_identity(),
        }],
        "both rows link the package stem once at the bundle identity"
    );

    // the warm run serves the root and the dep member from disk: the
    // debug markers name both families' hits, so the walk compiled
    // nothing in-process
    let (ok, warm_out, warm_err) = ds_run(&cache, &["run", &run_path(&source)]);
    assert!(ok, "the warm run succeeds (stderr: {warm_err})");
    assert_eq!(warm_out, out, "the warm run prints the cold output");
    assert!(
        warm_err.contains("ds run cache: hit"),
        "the root artifact serves warm: {warm_err}"
    );
    assert!(
        warm_err.contains("ds dep cache: hit"),
        "the @std dep member serves warm: {warm_err}"
    );

    // `ds build` of the same source records the same dep list
    let built = source
        .parent()
        .expect("the source dir")
        .join("deps-built.dsmod");
    let (ok, _, err) = ds_run(&cache, &[
        "build",
        &run_path(&source),
        "--out",
        built.to_str().expect("UTF-8"),
    ]);
    assert!(ok, "ds build succeeds (stderr: {err})");
    assert_eq!(
        ds_mod::read(&fs::read(&built).expect("the built artifact reads"))
            .expect("the built artifact decodes")
            .deps(),
        recorded.as_slice(),
        "ds build records the same dep list"
    );

    // the merged ambient dsmod no longer answers the bundle's identity:
    // the walk over the cached artifact refuses the stale served bytes
    // naming both hashes — the un-merge split the merged artifact's
    // identity from the bundle the implicit resolution records
    let mut registry = ds_vm::ModuleRegistry::new();
    registry.install_platform(ds_vm::Platform::std());
    let mut vm = ds_vm::Vm::new(registry);
    let merged =
        ds_mod::read(&ds_stdlib::AMBIENT_DSMOD[16..]).expect("the embedded @std artifact reads");
    let walk = vm.load(module, |name, _| (name == "@std").then(|| merged.clone()));
    assert_eq!(
        walk.unwrap_err(),
        ds_vm::LoadError::HashMismatch {
            dep: "@std".to_owned(),
            dependent: "deps".to_owned(),
            linked: std_member_identity(),
            found: Some(Hash128::compute(ds_stdlib::AMBIENT_SOURCE.as_bytes())),
        },
        "the merged artifact refuses the bundle identity naming both hashes"
    );

    let _ = fs::remove_dir_all(&cache);
}
