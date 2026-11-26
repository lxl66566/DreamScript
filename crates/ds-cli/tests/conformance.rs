//! The run-level golden gate over the conformance corpus: every corpus
//! file whose items are all run-stage (no reject item, nothing staged
//! for a later milestone) executes end to end through the `ds` binary in
//! source mode, and the golden stdout — pinned from live runs — carries
//! the observable behavior the M8 differential corpus must keep
//! identical across tiers: the defaulted-parameter spellings, the
//! list[record] checked reads, the transitive dispatch chain, the E5
//! teardown deinit prints (their order deterministic across runs), and
//! the tutorial's show-interpolation and receiverless-default answers.

use std::{env, path::PathBuf, process::Command};

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
fn the_union_default_annotation_answers_the_member_construction() {
    // the `#[default]`-annotated case answers the union's default() — a
    // payloaded struct case through per-part defaults, a tuple case the
    // call — and a defaulting struct field typing the union answers the
    // same construction
    run_golden(
        "derive_default_union.ds",
        "tone=loud { volume: 0 } level=big(0) pick=loud { volume: 0 }\n0\n",
    );
}

#[test]
fn the_comptime_fence_admits_pure_row_helpers() {
    // an ordinary fn admits by its own solved pure row, its body joining
    // the fenced evaluation (a two-hop chain riding the transitive
    // closure), the answer embedding at the call site, and the helper
    // still running on the runtime path
    run_golden("comptime_admission.ds", "kind=point flat=true\n0\n");
}

#[test]
fn the_comptime_fence_admits_trap_row_helpers() {
    // a callee whose solved row is `trap` alone admits (the 2026-11-17
    // amendment): the int-arithmetic helper and the string-concat helper
    // (its row the effects pass's unsolved-type over-approximation) join
    // the fenced evaluation with their answers embedding, and both still
    // run on the runtime path
    run_golden("comptime_trap_row.ds", "page!\n42\n0\n");
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

#[test]
fn the_json_derive_families_round_trip_through_the_parser_and_writer() {
    // one declaration deriving both families: the optional list field's
    // presence, presence-empty, and absence survive the encode-decode
    // walk (the none-side observation beside the re-encoded document),
    // the enum's two unit cases and one struct-payload case answer both
    // directions, and every wrong shape answers parse_err — a
    // non-object, a missing required member, a wrong-typed member, an
    // unknown case name, a non-string non-object enum shape
    run_golden(
        "json_derive_roundtrip.ds",
        concat!(
            "{\"name\":\"ada\",\"rows\":[4,5]}\n",
            "{\"name\":\"ada\"}\n",
            "{\"name\":\"ada\",\"rows\":[4,5]} false\n",
            "{\"name\":\"ada\"} true\n",
            "{\"name\":\"ada\",\"rows\":[]} false\n",
            "parse_err\n",
            "parse_err\n",
            "parse_err\n",
            "\"left\"\n",
            "{\"times\":3}\n",
            "\"left\"\n",
            "\"right\"\n",
            "{\"times\":3}\n",
            "parse_err\n",
            "parse_err\n",
            "0\n",
        ),
    );
}

#[test]
fn the_bytes_surface_renders_orders_and_crosses_the_bridges() {
    // the byte-list rendering (literal low bytes beside a high byte read
    // through the checked byte_at, empty printing bytes[]), the derive
    // families composing over a bytes part (the lexicographic
    // shorter-prefix-first order, the content hash), the utf8 bridges
    // round-tripping a multi-byte string with a lone lead byte answering
    // utf8_err, and list[bytes] answering content equality and in
    // membership
    run_golden(
        "bytes_surface.ds",
        concat!(
            "bytes[1, 2, 195]\n",
            "bytes[]\n",
            "chunk { label: lead, data: bytes[97, 98] }\n",
            "-1 1 0\n",
            "true false\n",
            "6\n",
            "héllo\n",
            "utf8_err\n",
            "true false\n",
            "true false\n",
            "0\n",
        ),
    );
}

#[test]
fn collection_equality_compares_nested_content_and_membership() {
    // the content-compare natives over the collections' one Ref word:
    // nested collections compose (a list of lists, a map over list
    // values, a struct field of list and map type), and in membership
    // compares the element by its type (a list element over a list of
    // lists, a struct element whose fields are collections); the float
    // judge rides the one bit-structural word equality everywhere — a
    // NaN reflexive, the two zeros distinct keys and unequal elements
    run_golden(
        "collection_equality.ds",
        "nested true\nmatrix true\nfields true\nmember true\nfloat true\n0\n",
    );
}

#[test]
fn the_json_width_vocabulary_round_trips_both_derive_families() {
    // byte/char/u32/bytes fields with their edge values through both
    // json derive families: byte and u32 as json numbers (the u32 words
    // built through the ambient's u32.of(x) conversion), char as its
    // one-character string, bytes as its number array (the empty payload
    // beside a spelled high byte), and every wrong-width or wrong-shape
    // decode answering parse_err
    run_golden(
        "json_width_roundtrip.ds",
        concat!(
            "{\"kind\":255,\"serial\":4294967295,\"mark\":\"é\",\"payload\":[0,255]}\n",
            "{\"kind\":0,\"serial\":0,\"mark\":\"q\",\"payload\":[]}\n",
            "{\"kind\":0,\"serial\":4294967295,\"mark\":\"é\",\"payload\":[0,255]}\n",
            "true\n",
            "true\n",
            "true\n",
            "0\n",
        ),
    );
}

#[test]
fn the_arena_surface_builds_traverses_and_reuses() {
    // the read-side aliasing container: the handle-built cycle walks
    // home through the at answers' mut windows, the removal retires the
    // slot the reuse reanswers under the bumped generation, the handle
    // crosses calls as a plain int word, and the dangling probe stays
    // uncalled — its trap is the ds-cli arena_face suite's pin
    run_golden("arena_surface.ds", "cycle 9\nreuse 0\ncrossed 7\n0\n");
}

#[test]
fn the_string_view_surface_holds_cursors_into_a_shared_base() {
    // the zero-copy parsing face: the validated construction, the
    // checked char read, the composing slice, the content equality, and
    // the materializing text — every refusal answering the none member
    run_golden("string_view_surface.ds", "roundtrip dream\nrefused 3\n0\n");
}

#[test]
fn the_place_chains_store_and_the_compound_operator_folds_through_them() {
    // the assignment sugar's whole surface through the binary: the field
    // paths, the subscript stores, the nestings, the map's key/value
    // staging, and the compound operator over every operator class, a
    // nested chain, the string `+=`, and a probe evaluated once (the
    // miss traps are the ds-lower place_assign suite's pins)
    run_golden(
        "place_stores.ds",
        "fields 30 359\nsubscripts 3201 150 304 221\ncompound 7 8606721 5084630 ax bax vw 211\n0\n",
    );
}
/// The non-handle io spellings answer the io_err union: a failed write,
/// list, and size over an absent parent each land on the not_found class
/// and extract through the `.ok` handler, `?`, and match — no error line
/// prints and the run exits clean where the total spelling halted.
#[test]
fn the_fallible_io_spellings_answer_the_io_err_union() {
    run_golden(
        "io_fallible.ds",
        "write refused\nlist refused\nsize not_found\n0\n",
    );
}
