//! The run-level golden gate over the conformance corpus: every corpus
//! file whose items are all run-stage (no reject item, nothing staged
//! for a later milestone) executes end to end through the `ds` binary in
//! source mode, and the golden stdout — pinned from live runs — carries
//! the observable behavior the M8 differential corpus must keep
//! identical across tiers: the defaulted-parameter spellings, the
//! list[record] checked reads, the transitive dispatch chain, the E5
//! teardown deinit prints (their order deterministic across runs), and
//! the tutorial's show-interpolation and receiverless-default answers.

use std::{env, fs, path::PathBuf, process::Command};

fn ds() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ds"));
    command.current_dir(env!("CARGO_MANIFEST_DIR"));
    command.env("DS_CACHE", test_cache());
    command
}

/// A private per-process script-cache root: spawned runs never read or
/// write a real user cache directory.
fn test_cache() -> PathBuf {
    static CACHE: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    CACHE
        .get_or_init(|| {
            let dir = env::temp_dir().join(format!("ds-cli-test-cache-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            dir
        })
        .clone()
}

fn corpus(name: &str) -> PathBuf {
    ds_corpus_path::file(name)
}

fn run_golden(name: &str, golden: &str) {
    let out = ds()
        .arg("run")
        .arg(corpus(name))
        .output()
        .expect("the binary runs");
    assert!(out.status.success(), "{name} failed: {out:?}");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        golden,
        "{name}: the golden stdout drifted"
    );
}

#[test]
fn trait_defaults_stages_one_authority_across_every_spelling() {
    // the omitted default and the written argument answer identically
    // through the dot and the qualified spelling, and the inherent
    // member keeps its own body default
    run_golden(
        "trait_defaults.ds",
        "int 12 12 13 13\nword 6 6 5\ngauge 7 11\n0\n",
    );
}

#[test]
fn records_lists_read_whole_elements_through_the_checked_span() {
    // list_push stages whole record spans, the missing index answers on
    // the none side of the `.ok`/`.err` chain, and the field projections
    // sum over the explicit-index walk (4*9 + 2*25 + 8*3)
    run_golden(
        "records_lists.ds",
        "count 3 miss absent total 110\nfirst nut 36\n0\n",
    );
}

#[test]
fn transitive_dispatch_rides_the_staged_evidence_area() {
    // the entering concrete site stages the serving body's whole
    // evidence area: the tells-for-list member dispatches through the
    // generic shows-for-list body at int, string, and one nested hop
    run_golden(
        "transitive_dispatch.ds",
        "got [n=6,n=7,]\ngot [x!,y!,]\ngot [[n=5,],]\n0\n",
    );
}

#[test]
fn teardown_deinit_fires_at_the_own_exit_and_the_drain() {
    // the completing child's hook fires at its own frame exit ("freed
    // clean" before the boundary), and the cancel-cut child's kept frame
    // fires at the teardown drain ("freed cut" after "start cut", before
    // the timeout unions out) — the "never" line stays unreached
    run_golden(
        "teardown_deinit.ds",
        "ran clean\nfreed clean\nclean kept 0\nstart cut\nfreed cut\nt\n0\n",
    );
}

#[test]
fn the_nursery_surface_joins_cuts_and_unions_payloads_out() {
    // the spawn-order join, the first-failure cut at safe points, the
    // completing child's deinit claim, and the payloaded cancel shapes:
    // a struct member's flat span read through the catch arm's fields,
    // and a generic application's one Ref word read through the list
    // natives
    run_golden(
        "nursery_surface.ds",
        "guarded kept 3\nt\nr\nran 1\nfreed 1\nclaim kept 0\ncode 3 payload\nlist 3\n0\n",
    );
}

#[test]
fn a_recursive_nursery_block_drains_the_interlock_iteratively() {
    // a spawned child's body joining its own fresh nursery while an
    // enclosing drive owns the child stands one interlock frame per
    // recursion level; depth 2000 lands where the recursive driver
    // overflowed the process stack near depth 50
    run_golden("nursery_deep.ds", "three 0\ntwo-thousand 0\n0\n");
}

#[test]
fn the_tutorial_surface_interpolates_and_defaults_end_to_end() {
    // the show-carrying holes over a derived struct, a payloaded enum
    // through the union, and an inherent member; the receiverless make
    // and default spellings; the enum case constructor's named payload
    // fields
    run_golden(
        "tutorial_surface.ds",
        "derived shape { label: nut, weight: 3 } / shape { label: nut, weight: 3 }\nunion loud { \
         volume: 7 }\ninherent gauge 4\ndefault 0 0 0\ncase true\n0\n",
    );
}
