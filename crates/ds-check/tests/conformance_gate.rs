//! The M2 conformance gate over the corpus's stage expectations: every
//! `stage: typecheck` item typechecks clean under `type_check`, `reject`
//! items produce exactly their described diagnostic, and the remaining
//! diagnostics land only in items staged for later milestones (their
//! constructs — comptime, derives, stdlib execution — do not check at M2
//! per the corpus README's cumulation rules). The affine pass [C1, C5]
//! joins the gate: `typecheck`/`run` items must also linearity-check
//! clean, the linearity negative suite must produce exactly its described
//! DS3xxx diagnostics, and no DS3xxx diagnostic may land outside
//! `reject`/later-milestone items in any corpus file.

use std::path::PathBuf;

use ds_check::{
    corpus::{Stage, stages},
    type_check,
};
use ds_diag::{Files, Span};
use ds_parse::{Ast, Item, ItemKind, parse};

/// Ambient stdlib stubs per conformance/README.md's types_hybrid list:
/// signatures only (extern), the M3 stdlib owns the bodies. The failure
/// taxonomy (io_err) mirrors the shared ambient's ownership of it; the
/// config type and the log/default_config/get externs are the corpus
/// file's own (the ambient shed its dead declarations and its demo
/// residue).
const TYPES_AMBIENT: &str = concat!(
    "pub tag int\n",
    "pub tag float\n",
    "pub tag string\n",
    "pub tag bool\n",
    "pub tag io_err\n",
    "pub trait Copyable { }\n",
    "pub struct list[t]\n",
    "pub struct map[k, v]\n",
    "pub struct field { name: string, type: type, tag: string }\n",
    "pub extern fn fields (t: type) -> list[field]\n",
    "pub extern fn type_name (t: type) -> string\n",
    "pub extern fn read_file (path: string) -> string | io_err\n",
    "pub tag parse_err { pos: int }\n",
    "pub enum json {\n",
    "  json_null\n",
    "  json_bool { value: bool }\n",
    "  json_int { value: int }\n",
    "  json_float { value: float }\n",
    "  json_string { value: string }\n",
    "  json_array { items: list[json] }\n",
    "  json_object { fields: map[string, json] }\n",
    "}\n",
    "pub extern fn parse_json (text: string) -> json | parse_err\n",
    "pub extern fn str[t] (v: t) -> string\n",
);

/// Ambient stdlib stubs per the linearity_modes_rc list: modes included,
/// since C2 consumption is what the affine pass reads at call sites. The
/// close/to_u32_le externs are the corpus file's own (the ambient shed
/// its dead declarations); open/read_exact stay stub-only absence
/// stand-ins so the affine pass sees the moves they stage.
const LINEARITY_AMBIENT: &str = concat!(
    "pub tag int\n",
    "pub tag char\n",
    "pub tag bool\n",
    "pub tag string\n",
    "pub type u32 = int\n",
    "pub trait Copyable { }\n",
    "pub struct bytes: ~Copyable { len: int }\n",
    "pub struct list[t]\n",
    "pub extern fn open (path: string) -> file\n",
    "pub extern fn read_exact (f: mut file, n: int) -> bytes\n",
    "pub extern fn split (s: string, sep: char) -> list[string]\n",
    "pub extern fn map[t, r] (xs: list[t], f: fn(t) -> r) -> list[r]\n",
    "pub extern fn collect[r] (xs: list[r]) -> list[r]\n",
    "pub extern fn len[t] (xs: t) -> int\n",
);

/// Ambient for the linearity negative suite: types only, everything else
/// the file declares itself.
const LINEARITY_NEG_AMBIENT: &str = concat!(
    "pub tag int\n",
    "pub tag string\n",
    "pub trait Copyable { }\n",
);

/// The minimal shared vocabulary for the corpus-wide affine sweep: every
/// file declares its own data types.
const SWEEP_AMBIENT: &str = concat!(
    "pub tag int\n",
    "pub tag float\n",
    "pub tag string\n",
    "pub tag bool\n",
    "pub tag char\n",
    "pub trait Copyable { }\n",
    "pub struct list[t]\n",
    "pub struct map[k, v]\n",
    "impl[t] list[t] {\n",
    "  fn get (self, i: int) -> t | none\n",
    "  fn push (mut self, x: t) -> list[t]\n",
    "}\n",
    "impl[k, v] map[k, v] {\n",
    "  fn get (self, key: k) -> v | none\n",
    "  fn set (mut self, key: k, value: v) -> map[k, v]\n",
    "  fn keys (self) -> list[k]\n",
    "}\n",
);

/// Ambient stdlib stubs per the generics_coherent_traits list.
const GENERICS_AMBIENT: &str = concat!(
    "pub tag int\n",
    "pub tag string\n",
    "pub tag char\n",
    "pub tag bool\n",
    "pub trait Copyable { }\n",
    "pub trait ord {\n",
    "  fn compare(self, other: Self) -> int\n",
    "}\n",
    "impl ord for int {\n",
    "  fn compare(self, other: Self) -> int {\n",
    "    if self < other { -1 } else if self > other { 1 } else { 0 }\n",
    "  }\n",
    "}\n",
    "pub struct list[t]\n",
    "pub struct field { name: string, type: type, tag: string }\n",
    "pub extern fn cons[t] (x: t, xs: list[t]) -> list[t]\n",
    "pub extern fn first[t] (xs: list[t]) -> t?\n",
    "pub extern fn tail[t] (xs: list[t]) -> list[t]\n",
    "pub extern fn join (parts: list[string], sep: char) -> string\n",
    "pub extern fn map[t, r] (xs: list[t], f: fn(t) -> r) -> list[r]\n",
    "pub extern fn str[t] (v: t) -> string\n",
    "pub extern fn fields (t: type) -> list[field]\n",
    "pub extern fn type_name (t: type) -> string\n",
);

/// Ambient for pattern_dispatch: the primitives, println, and the
/// success-typed read_file against the ambient-owned failure taxonomy
/// (the net cases feed the file's or-pattern demo).
const PATTERN_DISPATCH_AMBIENT: &str = concat!(
    "pub tag int\n",
    "pub tag string\n",
    "pub tag bool\n",
    "pub tag not_found\n",
    "pub tag permission_denied\n",
    "pub tag already_exists\n",
    "pub tag timed_out\n",
    "pub tag unexpected_eof\n",
    "pub tag io_other { msg: string }\n",
    "pub type io_err = not_found | permission_denied | already_exists | timed_out | \
     unexpected_eof | io_other\n",
    "pub enum net_err {\n",
    "  timeout\n",
    "  refused\n",
    "  addr_in_use\n",
    "  reset\n",
    "  net_other { msg: string }\n",
    "}\n",
    "pub trait Copyable { }\n",
    "pub extern fn read_file (path: string) -> string | io_err\n",
    "pub extern fn println (msg: string) -> ()\n",
);

/// Ambient stdlib stubs per the tuples_surface list: the primitives,
/// the list type, and the free `len` the tuple item's count builds
/// against.
const TUPLES_AMBIENT: &str = concat!(
    "pub tag int\n",
    "pub tag string\n",
    "pub tag bool\n",
    "pub trait Copyable { }\n",
    "pub struct list[t]\n",
    "pub extern fn len[t] (xs: t) -> int\n",
);

/// Ambient stdlib stubs for nursery_surface: the primitives, the shared
/// list type with its mutation member and free reads, the ambient-owned
/// net failure taxonomy, and the generic cancel seam the nursery items
/// name.
const NURSERY_AMBIENT: &str = concat!(
    "pub tag int\n",
    "pub tag string\n",
    "pub trait Copyable { }\n",
    "pub struct list[t]\n",
    "pub enum net_err {\n",
    "  timeout\n",
    "  refused\n",
    "}\n",
    "pub extern fn len[t] (xs: t) -> int\n",
    "pub extern fn str[t] (v: t) -> string\n",
    "pub extern fn println (msg: string) -> ()\n",
    "pub extern fn cancel[t] (e: t) -> ()\n",
    "impl[t] list[t] {\n",
    "  fn push (mut self, x: t) -> list[t]\n",
    "}\n",
);

/// Ambient stdlib stubs for closures_surface: the primitives alone; the
/// items carry their own fn-typed parameters and lambdas.
const CLOSURES_AMBIENT: &str = concat!(
    "pub tag int\n",
    "pub tag string\n",
    "pub trait Copyable { }\n"
);

/// Ambient for inherent_impls: the primitives alone; the file owns its
/// receiver types and their impls.
const INHERENT_AMBIENT: &str = concat!("pub tag int\n", "pub trait Copyable { }\n");

/// Ambient for toplevel_consumption: the primitives plus the Copyable
/// marker the file's linear struct subtracts — and one top-level consumer
/// of the corpus file's `far_seed`, added before the corpus file itself:
/// the cross-file order-independence pin (an earlier file's binding
/// consuming a later file's, typed under the binding's home-file
/// resolutions).
const TOPLEVEL_AMBIENT: &str = concat!(
    "pub tag int\n",
    "pub trait Copyable { }\n",
    "pub const echo = digest_of(far_seed)\n",
);

/// Ambient stdlib stubs per the lists_surface list: the checked list
/// reads, the free count, and the joined-list member the literal and
/// the constructor build against.
const LISTS_AMBIENT: &str = concat!(
    "pub tag int\n",
    "pub tag string\n",
    "pub tag char\n",
    "pub tag bool\n",
    "pub trait Copyable { }\n",
    "pub struct list[t]\n",
    "pub extern fn len[t] (xs: t) -> int\n",
    "pub extern fn cons[t] (x: t, xs: list[t]) -> list[t]\n",
    "pub extern fn tail[t] (xs: list[t]) -> list[t]\n",
    "pub extern fn collect[t] (xs: list[t]) -> list[t]\n",
    "pub extern fn join (parts: list[string], sep: char) -> string\n",
    "pub extern fn println (msg: string) -> ()\n",
    "impl[t] list[t] {\n",
    "  fn get (self, i: int) -> t | none\n",
    "  fn push (mut self, x: t) -> list[t]\n",
    "  fn concat (self, other: list[t]) -> list[t]\n",
    "}\n",
);

/// Ambient stdlib stubs per the maps_surface list: the constructor, the
/// inherent member spellings the builder calls and checked reads run
/// through (the native seams' `set`/`get`/`keys` members), the free
/// `len` the entry-count item spells, and the list the key face
/// answers.
const MAPS_AMBIENT: &str = concat!(
    "pub tag int\n",
    "pub tag string\n",
    "pub tag bool\n",
    "pub trait Copyable { }\n",
    "pub struct map[k, v]\n",
    "pub struct list[t]\n",
    "pub extern fn len[t] (xs: t) -> int\n",
    "impl[k, v] map[k, v] {\n",
    "  fn get (self, key: k) -> v | none\n",
    "  fn set (mut self, key: k, value: v) -> map[k, v]\n",
    "  fn keys (self) -> list[k]\n",
    "}\n",
);
/// Ambient for extraction_family: the primitives, the collection member
/// reads, the family's own extern spellings, and the ambient-owned
/// config the b14 spellings extract over.
const EXTRACTION_AMBIENT: &str = concat!(
    "pub tag int\n",
    "pub tag string\n",
    "pub tag bool\n",
    "pub tag char\n",
    "pub trait Copyable { }\n",
    "pub struct list[t]\n",
    "pub struct map[k, v]\n",
    "pub extern fn println (msg: string) -> ()\n",
    "pub extern fn unwrap_or[t] (x: t?, y: t) -> t\n",
    "pub extern fn unwrap_or_else[t] (x: t?, f: fn() -> t) -> t\n",
    "pub extern fn ok_or[t, e] (x: t?, err: e) -> t | e\n",
    "pub extern fn map_err[t, e, e2] (x: t | e, f: fn(e) -> e2) -> t | e2\n",
    "impl[t] list[t] {\n",
    "  fn get (self, i: int) -> t | none\n",
    "}\n",
    "impl[k, v] map[k, v] {\n",
    "  fn get (self, key: k) -> v | none\n",
    "}\n",
);

/// Ambient stdlib stubs for trait_defaults: the primitives, `len` (the
/// word receiver's field read feeds it), and `println`; the file owns
/// its trait, both impls, and the receiver types.
const TRAIT_DEFAULTS_AMBIENT: &str = concat!(
    "pub tag int\n",
    "pub tag string\n",
    "pub trait Copyable { }\n",
    "pub extern fn len[t] (xs: t) -> int\n",
    "pub extern fn println (msg: string) -> ()\n",
);

/// Ambient stdlib stubs for records_lists: the ambient list surface the
/// pushed records, checked reads, and free count build against.
const RECORDS_LISTS_AMBIENT: &str = concat!(
    "pub tag int\n",
    "pub tag string\n",
    "pub trait Copyable { }\n",
    "pub struct list[t]\n",
    "pub extern fn len[t] (xs: t) -> int\n",
    "pub extern fn println (msg: string) -> ()\n",
    "impl[t] list[t] {\n",
    "  fn get (self, i: int) -> t | none\n",
    "  fn push (mut self, x: t) -> list[t]\n",
    "}\n",
);

/// Ambient stdlib stubs for frozen_iteration: the list surface's push
/// member the builder calls and the mutation ride, the free `len`, and
/// `println`; the file owns nothing.
const FROZEN_ITER_AMBIENT: &str = concat!(
    "pub tag int\n",
    "pub tag string\n",
    "pub trait Copyable { }\n",
    "pub struct list[t]\n",
    "pub extern fn len[t] (xs: t) -> int\n",
    "pub extern fn println (msg: string) -> ()\n",
    "impl[t] list[t] {\n",
    "  fn push (mut self, x: t) -> list[t]\n",
    "}\n",
);

/// Ambient stdlib stubs for transitive_dispatch: the primitives, the
/// list type (the generic impl heads name it), and `println`; the file
/// owns both traits and the four impls.
const TRANSITIVE_AMBIENT: &str = concat!(
    "pub tag int\n",
    "pub tag string\n",
    "pub trait Copyable { }\n",
    "pub struct list[t]\n",
    "pub extern fn println (msg: string) -> ()\n",
);

/// Ambient stdlib stubs for shift_bitwise: the int/float primitives,
/// the `none` tag the union spelling names beside the bitwise `|`, and
/// `println`.
const SHIFT_BITWISE_AMBIENT: &str = concat!(
    "pub tag int\n",
    "pub tag float\n",
    "pub tag bool\n",
    "pub tag string\n",
    "pub tag none\n",
    "pub extern fn println (msg: string) -> ()\n",
);

/// Ambient stdlib stubs for teardown_deinit: the ambient-owned net
/// failure taxonomy, the generic cancel seam, and `println`; the file
/// owns its deinit-carrying `resource`.
const TEARDOWN_AMBIENT: &str = concat!(
    "pub tag int\n",
    "pub tag string\n",
    "pub enum net_err {\n",
    "  timeout\n",
    "  refused\n",
    "}\n",
    "pub trait Copyable { }\n",
    "pub extern fn println (msg: string) -> ()\n",
    "pub extern fn cancel[t] (e: t) -> ()\n",
);

/// Ambient stdlib stubs for none_fields: the primitives alone (`none`
/// is the predeclared unit atom, no ambient declaration needed); the
/// file owns nothing.
const NONE_FIELDS_AMBIENT: &str = "pub tag int\n";

fn items_of(ast: &Ast) -> Vec<&Item> {
    ast.blocks.iter().flat_map(|b| b.items.iter()).collect()
}

fn within(span: Span, item: &Item) -> bool {
    span.file == item.span.file && span.start >= item.span.start && span.end <= item.span.end
}

fn gate(corpus: &str, ambient: &str) -> Vec<(String, String, ds_diag::Diagnostic)> {
    let path = ds_corpus_path::resolve(corpus);
    let source = std::fs::read_to_string(&path).unwrap();
    let stages = stages(&source);
    let mut files = Files::new();
    let amb = files.add_source("ambient.ds", ambient);
    let cor = files.add_source("corpus.ds", &source);
    let amb_ast = parse(&files, amb).unwrap_or_else(|d| panic!("{corpus} ambient: {d:?}"));
    let cor_ast = parse(&files, cor).unwrap_or_else(|d| panic!("{corpus} corpus: {d:?}"));
    let checked = type_check(&files, &[amb_ast, cor_ast.clone()]);

    let items = items_of(&cor_ast);
    assert_eq!(
        items.len(),
        stages.len(),
        "{corpus}: header items and parsed items disagree"
    );
    let mut out = Vec::new();
    for (item, stage) in items.iter().zip(stages.iter()) {
        let item_diags: Vec<&ds_diag::Diagnostic> = checked
            .diags
            .iter()
            .filter(|d| d.labels.iter().any(|l| within(l.span, item)))
            .collect();
        if stage.stage == "typecheck" {
            assert!(
                item_diags.is_empty(),
                "{}: item `{}` must typecheck, got:\n{}",
                corpus,
                stage.id,
                item_diags
                    .iter()
                    .map(|d| format!("{} {}", d.code.as_str(), d.message))
                    .collect::<Vec<_>>()
                    .join("\n")
            );
        } else if let Some(what) = stage.stage.strip_prefix("reject ") {
            assert_eq!(
                item_diags.len(),
                1,
                "{}: item `{}` expects exactly one rejection, got {:?}",
                corpus,
                stage.id,
                what
            );
            out.push((stage.id.clone(), what.to_owned(), item_diags[0].clone()));
        }
        // Later-milestone and run items may carry diagnostics at M2; the
        // per-file tests below pin exactly which and why.
    }
    out
}

#[test]
fn types_hybrid_typecheck_items_pass() {
    // 12 typecheck-stage items; no diagnostics may fall outside get_port
    // (stage run) — the json value atoms do not include `int`, so
    // `pv is int` narrows to empty inside get_port until the M3 stdlib
    // types `map.get`; that tension is the recorded leftover, everything
    // else (load, cfg's `.err` handler, kv, same) checks clean.
    let rejects = gate("../../conformance/types_hybrid.ds", TYPES_AMBIENT);
    assert_eq!(rejects, [] as [(String, String, ds_diag::Diagnostic); 0]);
}

/// The tutorial-pass regressions: every item (typecheck and run) checks
/// clean under the record-shaped ambient — the bare-tag arms credit
/// exhaustiveness, the or-pattern of unit tags is one member set, and
/// the tail `?` over the contained parse union is the return itself.
#[test]
fn pattern_dispatch_items_check_clean() {
    let rejects = gate(
        "../../conformance/pattern_dispatch.ds",
        PATTERN_DISPATCH_AMBIENT,
    );
    assert_eq!(rejects, [] as [(String, String, ds_diag::Diagnostic); 0]);
    let path = ds_corpus_path::resolve("../../conformance/pattern_dispatch.ds");
    let source = std::fs::read_to_string(&path).unwrap();
    let mut files = Files::new();
    let amb = files.add_source("ambient.ds", PATTERN_DISPATCH_AMBIENT);
    let cor = files.add_source("corpus.ds", &source);
    let asts = [parse(&files, amb).unwrap(), parse(&files, cor).unwrap()];
    let checked = type_check(&files, &asts);
    assert!(
        checked.diags.is_empty(),
        "{}",
        checked
            .diags
            .iter()
            .map(|d| format!("{} {}", d.code.as_str(), d.message))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// The extraction-family round [E4]: the typecheck-stage items (the
/// b14/b15 spellings over the struct field and the row read included)
/// and every run item check clean under the family ambient; the one
/// rejection is the file's own reject item.
#[test]
fn extraction_family_items_check_clean() {
    let rejects = gate("../../conformance/extraction_family.ds", EXTRACTION_AMBIENT);
    assert_eq!(rejects.len(), 1);
    assert_eq!(rejects[0].0, "plain_receiver");
    assert!(
        rejects[0]
            .2
            .message
            .contains("`unwrap_or` extracts a `t | none` receiver; `int` carries no `none` member")
    );
}

#[test]
fn types_hybrid_narrows_the_ambient_json_cleanly() {
    let path = ds_corpus_path::resolve("../../conformance/types_hybrid.ds");
    let source = std::fs::read_to_string(&path).unwrap();
    let mut files = Files::new();
    let amb = files.add_source("ambient.ds", TYPES_AMBIENT);
    let cor = files.add_source("corpus.ds", &source);
    let asts = [parse(&files, amb).unwrap(), parse(&files, cor).unwrap()];
    let checked = type_check(&files, &asts);
    // get_port narrows the ambient's json_object and json_int cases and
    // load carries parse_err in its union, so the whole file checks clean
    // under the stub that mirrors the ambient's own json face
    assert!(
        checked.diags.is_empty(),
        "types_hybrid checks clean: {}",
        checked
            .diags
            .iter()
            .map(|d| ds_diag::human(d, &files))
            .collect::<String>()
    );
}

#[test]
fn generics_reject_item_is_the_overlap_witness() {
    let rejects = gate(
        "../../conformance/generics_coherent_traits.ds",
        GENERICS_AMBIENT,
    );
    assert_eq!(rejects.len(), 1, "one reject item in the file header");
    let (id, _what, diag) = &rejects[0];
    assert_eq!(id, "impl-json-text-int-bool");
    assert_eq!(diag.code.as_str(), "DS2006");
    assert_eq!(
        diag.message,
        "instance head `int | bool` intersects the `int` instance; witness value type `int`"
    );
    assert_eq!(
        diag.labels.first().map(|l| l.label.as_str()),
        Some("overlapping head")
    );
}

#[test]
fn generics_diagnostics_stay_in_later_milestone_items() {
    // The generics file's other diagnostics all sit in m6 items (comptime
    // `fields`/`type_name` builtins, the `ord`-derived `max`, and the
    // derive-driven execution items), which the M2 gate exempts.
    let path = ds_corpus_path::resolve("../../conformance/generics_coherent_traits.ds");
    let source = std::fs::read_to_string(&path).unwrap();
    let stages = stages(&source);
    let mut files = Files::new();
    let amb = files.add_source("ambient.ds", GENERICS_AMBIENT);
    let cor = files.add_source("corpus.ds", &source);
    let asts = [parse(&files, amb).unwrap(), parse(&files, cor).unwrap()];
    let checked = type_check(&files, &asts);
    let items = items_of(&asts[1]);
    for diag in &checked.diags {
        let stage = items
            .iter()
            .zip(stages.iter())
            .find(|(item, _)| diag.labels.iter().any(|l| within(l.span, item)))
            .map_or_else(
                || "outside every item".to_owned(),
                |(_, stage)| stage.stage.clone(),
            );
        assert!(
            stage.starts_with("reject") || stage.starts_with("m6"),
            "diagnostic in `{stage}`: {} {}",
            diag.code.as_str(),
            diag.message
        );
    }
}

/// Runs both passes over one corpus file plus ambient and returns every
/// item with the diagnostics whose labels fall inside it.
#[allow(clippy::type_complexity)]
fn gate_full(
    corpus: &str,
    ambient: &str,
) -> (
    Vec<(String, String, ds_diag::Diagnostic)>,
    Vec<(String, String, ds_diag::Diagnostic)>,
    ds_check::Linearity,
    String,
) {
    let path = ds_corpus_path::resolve(corpus);
    let source = std::fs::read_to_string(&path).unwrap();
    // Unresolved items are commented out in the body and skipped by every
    // runner (corpus README), so they align with no live item.
    let stages: Vec<Stage> = stages(&source)
        .into_iter()
        .filter(|stage| !stage.stage.starts_with("unresolved"))
        .collect();
    let mut files = Files::new();
    let amb = files.add_source("ambient.ds", ambient);
    let cor = files.add_source("corpus.ds", &source);
    let amb_ast = parse(&files, amb).unwrap_or_else(|d| panic!("{corpus} ambient: {d:?}"));
    let cor_ast = parse(&files, cor).unwrap_or_else(|d| panic!("{corpus} corpus: {d:?}"));
    let (checked, lin) = ds_check::linearity::check(&files, &[amb_ast, cor_ast.clone()]);

    let items = items_of(&cor_ast);
    assert_eq!(
        items.len(),
        stages.len(),
        "{corpus}: header items and parsed items disagree"
    );
    let mut rejects = Vec::new();
    let mut all = Vec::new();
    for (item, stage) in items.iter().zip(stages.iter()) {
        let type_diags: Vec<ds_diag::Diagnostic> = checked
            .diags
            .iter()
            .filter(|d| d.labels.iter().any(|l| within(l.span, item)))
            .cloned()
            .collect();
        let lin_diag: Option<ds_diag::Diagnostic> = lin
            .diags
            .iter()
            .find(|d| d.labels.iter().any(|l| within(l.span, item)))
            .cloned();
        for d in &type_diags {
            all.push((stage.id.clone(), stage.stage.clone(), d.clone()));
        }
        if let Some(d) = &lin_diag {
            all.push((stage.id.clone(), stage.stage.clone(), d.clone()));
        }
        if let Some(what) = stage.stage.strip_prefix("reject ")
            && let Some(diag) = &lin_diag
        {
            rejects.push((stage.id.clone(), what.to_owned(), diag.clone()));
        }
    }
    (rejects, all, lin, source)
}

/// linearity_modes_rc.ds checks fully clean: typecheck and run items (which must
/// typecheck from M2 per the cumulation rules) produce no diagnostic of
/// either pass, and the recorded facts match the record's own annotations
/// ("b consumed here", "f dies here: deinit closes fd").
#[test]
fn linearity_modes_rc_items_pass_with_expected_facts() {
    let (rejects, all, lin, source) =
        gate_full("../../conformance/linearity_modes_rc.ds", LINEARITY_AMBIENT);
    assert_eq!(rejects.len(), 0, "no reject items in the file header");
    assert!(all.is_empty(), "items carry diagnostics: {all:#?}");
    let text = |span: Span| source[span.start as usize..span.end as usize].to_owned();
    // `b` moves into `to_u32_le` (the record's affine move at b.to_u32_le()):
    // the use span is the `b` occurrence inside the call, past its binder.
    assert_eq!(lin.moves.len(), 1, "exactly one move: b into to_u32_le");
    assert_eq!(text(lin.moves[0].binder), "b");
    assert_eq!(text(lin.moves[0].at), "b");
    assert!(
        lin.moves[0].at.start > lin.moves[0].binder.end,
        "the move site is the use, not the binder"
    );
    // `f` dies live at sum_file's body end: the deinit site the runtime
    // half must honor, including early-return paths.
    assert_eq!(lin.drops.len(), 1, "exactly one drop: f at scope end");
    assert_eq!(text(lin.drops[0].binder), "f");
    assert!(
        text(lin.drops[0].at).starts_with('{'),
        "the drop site is the scope exit"
    );
}

/// The negative suite: every reject item carries exactly its described
/// DS3xxx diagnostic with the recorded two-span shape.
#[test]
fn linearity_negative_rejects_render_recorded_shapes() {
    let (rejects, ..) = gate_full(
        "../../conformance/linearity_negative.ds",
        LINEARITY_NEG_AMBIENT,
    );
    let by_id = |id: &str| {
        rejects
            .iter()
            .find(|(rid, ..)| rid == id)
            .unwrap_or_else(|| panic!("missing reject item {id}"))
            .clone()
    };
    assert_eq!(
        rejects.len(),
        7,
        "every reject item carries exactly its affine diagnostic"
    );
    for (id, _, diag) in &rejects {
        assert_eq!(
            diag.code.class(),
            ds_diag::ErrorClass::Linearity,
            "{id}: the rejection is an affine diagnostic"
        );
    }
    let (id, what, diag) = by_id("moved-then-used");
    assert!(what.contains("DS3001"), "{id}: {what}");
    assert_eq!(diag.code.as_str(), "DS3001");
    assert_eq!(diag.message, "value `h` is no longer usable");
    let labels = &diag.labels;
    assert_eq!(labels.len(), 2, "the moved/used pair is two spans");
    assert_eq!(labels[0].label, "moved here");
    assert_eq!(labels[1].label, "used again here after move");

    let (id, what, diag) = by_id("moved-across-arms");
    assert!(what.contains("DS3001"), "{id}: {what}");
    assert_eq!(diag.code.as_str(), "DS3001");
    assert_eq!(diag.message, "value `h` is no longer usable");

    let (id, what, diag) = by_id("moved-in-loop");
    assert!(what.contains("DS3001"), "{id}: {what}");
    assert_eq!(diag.code.as_str(), "DS3001");
    assert_eq!(
        diag.message,
        "value `h` is moved again on a re-executing path"
    );
    assert_eq!(diag.labels.len(), 1, "the loop move names its own site");

    let (id, what, diag) = by_id("plain");
    assert!(what.contains("DS3004"), "{id}: {what}");
    assert_eq!(diag.code.as_str(), "DS3004");
    assert_eq!(
        diag.message,
        "deinit is restricted to `~Copyable` types [C5]"
    );

    let (id, what, diag) = by_id("param-moved-then-used");
    assert!(what.contains("DS3001"), "{id}: {what}");
    assert_eq!(diag.code.as_str(), "DS3001");
    assert_eq!(diag.message, "value `h` is no longer usable");
    assert_eq!(
        diag.labels.len(),
        2,
        "the consumed parameter's pair is two spans"
    );
    assert_eq!(diag.labels[0].label, "moved here");
    assert_eq!(diag.labels[1].label, "used again here after move");

    let (id, what, diag) = by_id("captured-var-then-used");
    assert!(what.contains("DS3001"), "{id}: {what}");
    assert_eq!(diag.code.as_str(), "DS3001");
    assert_eq!(diag.message, "value `n` is no longer usable");
    assert_eq!(
        diag.labels.len(),
        2,
        "the construction/use pair is two spans"
    );
    assert_eq!(diag.labels[0].label, "moved here");

    let (id, what, diag) = by_id("projection-moved-then-used");
    assert!(what.contains("DS3001"), "{id}: {what}");
    assert_eq!(diag.code.as_str(), "DS3001");
    assert_eq!(diag.message, "value `s` is no longer usable");
    assert_eq!(diag.labels.len(), 2, "the transfer's pair is two spans");
    assert_eq!(diag.labels[0].label, "moved here");
    assert_eq!(diag.labels[1].label, "used again here after move");
}

/// lists_surface.ds: the list-literal and `list[t]()` constructor round —
/// every typecheck and run item checks clean under the list ambient
/// stubs, and the reject item reports exactly its mixed-element error.
#[test]
fn lists_surface_items_check_with_the_mixed_rejection() {
    let rejects = gate("../../conformance/lists_surface.ds", LISTS_AMBIENT);
    let [(id, what, diag)] = rejects.try_into().expect("exactly the one reject item");
    assert_eq!(id, "reject-mixed");
    assert!(what.contains("DS2001"), "the reject text names the code");
    assert_eq!(diag.code.as_str(), "DS2001");
    assert!(
        diag.message.contains("list literal mixes element types"),
        "the mixed-element rejection: {}",
        diag.message
    );

    // every other item checks clean under the same stubs
    let path = ds_corpus_path::resolve("../../conformance/lists_surface.ds");
    let source = std::fs::read_to_string(&path).unwrap();
    let stages = stages(&source);
    let mut files = Files::new();
    let amb = files.add_source("ambient.ds", LISTS_AMBIENT);
    let cor = files.add_source("corpus.ds", &source);
    let asts = [parse(&files, amb).unwrap(), parse(&files, cor).unwrap()];
    let checked = type_check(&files, &asts);
    let items = items_of(&asts[1]);
    for (item, stage) in items.iter().zip(stages.iter()) {
        if stage.stage.starts_with("reject") {
            continue;
        }
        let diags: Vec<&ds_diag::Diagnostic> = checked
            .diags
            .iter()
            .filter(|d| d.labels.iter().any(|l| within(l.span, item)))
            .collect();
        assert!(
            diags.is_empty(),
            "item `{}` carries diagnostics: {diags:?}",
            stage.id
        );
    }
}

/// maps_surface.ds: the map constructor round — every typecheck and run
/// item checks clean under the map ambient stubs (the `get` spelling item
/// typechecks; its generic dispatch waits for the M6 seam), and the reject
/// item reports exactly its constructor-arguments error.
#[test]
fn maps_surface_items_check_with_the_constructor_rejection() {
    let rejects = gate("../../conformance/maps_surface.ds", MAPS_AMBIENT);
    let [(id, what, diag)] = rejects.try_into().expect("exactly the one reject item");
    assert_eq!(id, "reject-ctor-args");
    assert!(what.contains("DS2001"), "the reject text names the code");
    assert_eq!(diag.code.as_str(), "DS2001");
    assert!(
        diag.message
            .contains("empty-map constructor takes no arguments"),
        "the constructor-arguments rejection: {}",
        diag.message
    );

    // every other item checks clean under the same stubs
    let path = ds_corpus_path::resolve("../../conformance/maps_surface.ds");
    let source = std::fs::read_to_string(&path).unwrap();
    let stages = stages(&source);
    let mut files = Files::new();
    let amb = files.add_source("ambient.ds", MAPS_AMBIENT);
    let cor = files.add_source("corpus.ds", &source);
    let asts = [parse(&files, amb).unwrap(), parse(&files, cor).unwrap()];
    let checked = type_check(&files, &asts);
    let items = items_of(&asts[1]);
    for (item, stage) in items.iter().zip(stages.iter()) {
        if stage.stage.starts_with("reject") {
            continue;
        }
        let diags: Vec<&ds_diag::Diagnostic> = checked
            .diags
            .iter()
            .filter(|d| d.labels.iter().any(|l| within(l.span, item)))
            .collect();
        assert!(
            diags.is_empty(),
            "item `{}` carries diagnostics: {diags:?}",
            stage.id
        );
    }
}

/// tuples_surface.ds: the anonymous-tuple round — every typecheck and
/// run item checks clean under the tuple ambient stubs, and the reject
/// item reports exactly its unproven-member rejection: a declaration
/// destructuring over a union would bind one member's types with no
/// coverage proof [B3, F4].
#[test]
fn tuples_surface_items_check_with_the_union_destructure_rejection() {
    let rejects = gate("../../conformance/tuples_surface.ds", TUPLES_AMBIENT);
    let [(id, what, diag)] = rejects.try_into().expect("exactly the one reject item");
    assert_eq!(id, "reject-union-destructure");
    assert!(what.contains("DS2001"), "the reject text names the code");
    assert_eq!(diag.code.as_str(), "DS2001");
    assert!(
        diag.message.contains("needs one tuple of 2 element(s)"),
        "the unproven-member rejection: {}",
        diag.message
    );

    // every other item checks clean under the same stubs
    let path = ds_corpus_path::resolve("../../conformance/tuples_surface.ds");
    let source = std::fs::read_to_string(&path).unwrap();
    let stages = stages(&source);
    let mut files = Files::new();
    let amb = files.add_source("ambient.ds", TUPLES_AMBIENT);
    let cor = files.add_source("corpus.ds", &source);
    let asts = [parse(&files, amb).unwrap(), parse(&files, cor).unwrap()];
    let checked = type_check(&files, &asts);
    let items = items_of(&asts[1]);
    for (item, stage) in items.iter().zip(stages.iter()) {
        if stage.stage.starts_with("reject") {
            continue;
        }
        let diags: Vec<&ds_diag::Diagnostic> = checked
            .diags
            .iter()
            .filter(|d| d.labels.iter().any(|l| within(l.span, item)))
            .collect();
        assert!(
            diags.is_empty(),
            "item `{}` carries diagnostics: {diags:?}",
            stage.id
        );
    }
}

/// No affine diagnostic may land outside `reject` items (whose text names
/// the code) or later-milestone items, in any corpus file. Header stages
/// pair with items by declared name: files with comment-only header items
/// (a ds.pkg manifest) break positional pairing, and imports declare no
/// affine-relevant names at all.
#[test]
fn affine_diagnostics_stay_in_reject_or_later_milestone_items() {
    let corpus = [
        ("../../conformance/types_hybrid.ds", SWEEP_AMBIENT),
        ("../../conformance/pattern_dispatch.ds", SWEEP_AMBIENT),
        ("../../conformance/linearity_modes_rc.ds", LINEARITY_AMBIENT),
        (
            "../../conformance/linearity_negative.ds",
            LINEARITY_NEG_AMBIENT,
        ),
        ("../../conformance/effects_nursery.ds", SWEEP_AMBIENT),
        ("../../conformance/lists_surface.ds", LISTS_AMBIENT),
        ("../../conformance/tuples_surface.ds", TUPLES_AMBIENT),
        ("../../conformance/syntax_ts_heart.ds", SWEEP_AMBIENT),
        (
            "../../conformance/generics_coherent_traits.ds",
            GENERICS_AMBIENT,
        ),
        ("../../conformance/modules_hash_core.ds", SWEEP_AMBIENT),
        ("../../conformance/inherent_impls.ds", INHERENT_AMBIENT),
        (
            "../../conformance/toplevel_consumption.ds",
            concat!("pub tag int\n", "pub trait Copyable { }\n"),
        ),
        ("../../conformance/frozen_iteration.ds", FROZEN_ITER_AMBIENT),
    ];
    for (corpus, ambient) in corpus {
        let path = ds_corpus_path::resolve(corpus);
        let source = std::fs::read_to_string(&path).unwrap();
        let stages: Vec<Stage> = stages(&source)
            .into_iter()
            .filter(|stage| !stage.stage.starts_with("unresolved"))
            .collect();
        let mut files = Files::new();
        let amb = files.add_source("ambient.ds", ambient);
        let cor = files.add_source("corpus.ds", &source);
        let asts = [
            parse(&files, amb).unwrap_or_else(|d| panic!("{corpus} ambient: {d:?}")),
            parse(&files, cor).unwrap_or_else(|d| panic!("{corpus} corpus: {d:?}")),
        ];
        let (checked, lin) = ds_check::linearity::check(&files, &asts);
        let items = items_of(&asts[1]);
        let mut affine = 0;
        for diag in checked
            .diags
            .iter()
            .chain(lin.diags.iter())
            .filter(|d| d.code.class() == ds_diag::ErrorClass::Linearity)
        {
            affine += 1;
            let Some(item) = items
                .iter()
                .find(|item| diag.labels.iter().any(|l| within(l.span, item)))
            else {
                panic!(
                    "{corpus}: affine diagnostic outside every item: {}",
                    diag.message
                );
            };
            let Some(name) = item_declares(item) else {
                panic!(
                    "{corpus}: affine diagnostic in an item that declares no name: {}",
                    diag.message
                );
            };
            let hyphen = name.replace('_', "-");
            let stage = stages.iter().find(|stage| stage.id == hyphen).map_or_else(
                || panic!("{corpus}: no header stage for item `{name}`"),
                |s| s.stage.clone(),
            );
            assert!(
                stage.starts_with("reject")
                    || stage.starts_with("m5")
                    || stage.starts_with("m6")
                    || stage.starts_with("m7"),
                "{corpus}: affine diagnostic in item `{name}` staged `{stage}`: {} {}",
                diag.code.as_str(),
                diag.message
            );
        }
        let _ = affine;
    }
}

/// The name a top-level item declares, when it declares one.
fn item_declares(item: &Item) -> Option<String> {
    let name = match &item.kind {
        ItemKind::Fn(f) | ItemKind::ExternFn(f) => &f.name.name,
        ItemKind::Struct(s) => &s.name.name,
        ItemKind::Enum(e) => &e.name.name,
        ItemKind::Tag(t) => &t.name.name,
        ItemKind::TypeAlias(a) => &a.name.name,
        ItemKind::Trait(t) => &t.name.name,
        ItemKind::Local(l) => match &l.pat {
            ds_parse::Pattern::Binding { name, .. } => &name.name,
            _ => return None,
        },
        ItemKind::Impl(_) | ItemKind::Import(_) | ItemKind::Platform(_) => return None,
    };
    Some(name.clone())
}

/// closures_surface.ds: lambda values in every argument shape —
/// fn-typed parameters, trailing block and parenthesized arguments,
/// let-stored and passed lambdas, a capturing lambda — all check clean
/// [F2, F7]; every item lowers (the trailing block staging exactly like
/// the written trailing lambda), and the runs live in the ds-lower
/// block_arg e2e suite.
#[test]
fn closures_surface_items_check_clean() {
    let rejects = gate("../../conformance/closures_surface.ds", CLOSURES_AMBIENT);
    assert!(
        rejects.is_empty(),
        "the closures surface declares no reject item"
    );
}

/// nursery_surface.ds: the in-language nursery surface — spawn-in-scope,
/// join at scope exit in spawn order, first-failure cancellation, child
/// error unioning out through the boundary, and a completing child's
/// deinit claim — every item checks clean [E2, E5]; the runs live in the
/// ds-lower nursery e2e suite and examples/nursery.ds.
#[test]
fn nursery_surface_items_check_clean() {
    let rejects = gate("../../conformance/nursery_surface.ds", NURSERY_AMBIENT);
    assert!(
        rejects.is_empty(),
        "the nursery surface declares no reject item"
    );
}

/// inherent_impls.ds: the owner-typed inherent surface — two owners share
/// one method name, every typecheck item (the dot-calls and the Self-bound
/// field reads included) checks clean, and the one rejection is the
/// wrong-receiver dot-call; the member bodies plan under the
/// `<target>#<member>` mangle and the dot-calls dispatch as plain
/// local calls.
#[test]
fn inherent_impls_items_check_with_the_wrong_receiver_rejection() {
    let rejects = gate("../../conformance/inherent_impls.ds", INHERENT_AMBIENT);
    let [(id, what, diag)] = rejects.try_into().expect("exactly the one reject item");
    assert_eq!(id, "wrong-receiver");
    assert!(what.contains("DS2013"), "the reject text names the code");
    assert_eq!(diag.code.as_str(), "DS2013");
    assert_eq!(
        diag.message, "no method `peek` accepts receiver `int`",
        "the owner filter rejects at the dot"
    );

    // every other item checks clean under the same stubs
    let path = ds_corpus_path::resolve("../../conformance/inherent_impls.ds");
    let source = std::fs::read_to_string(&path).unwrap();
    let stages = stages(&source);
    let mut files = Files::new();
    let amb = files.add_source("ambient.ds", INHERENT_AMBIENT);
    let cor = files.add_source("corpus.ds", &source);
    let asts = [parse(&files, amb).unwrap(), parse(&files, cor).unwrap()];
    let checked = type_check(&files, &asts);
    let items = items_of(&asts[1]);
    for (item, stage) in items.iter().zip(stages.iter()) {
        if stage.stage.starts_with("reject") {
            continue;
        }
        let diags: Vec<&ds_diag::Diagnostic> = checked
            .diags
            .iter()
            .filter(|d| d.labels.iter().any(|l| within(l.span, item)))
            .collect();
        assert!(
            diags.is_empty(),
            "item `{}` carries diagnostics: {diags:?}",
            stage.id
        );
    }
}

/// toplevel_consumption.ds: order-independent top-level consumption — the
/// consumer may be declared before the value it consumes, the moved-twice
/// reject carries its two-span pair inside one item, and the gate's
/// ambient (an earlier file) consumes the corpus's own `far_seed`, so the
/// whole two-file package checks clean: the cross-file direction of the
/// home-file typing fix, with linearity running package-wide over both.
#[test]
fn toplevel_consumption_checks_with_cross_file_consumer_clean() {
    let (rejects, ..) = gate_full(
        "../../conformance/toplevel_consumption.ds",
        TOPLEVEL_AMBIENT,
    );
    assert_eq!(
        rejects.len(),
        1,
        "the moved-twice reject item carries its affine diagnostic"
    );
    let (id, what, diag) = &rejects[0];
    assert_eq!(id, "twice");
    assert!(what.contains("DS3001"), "{id}: {what}");
    assert_eq!(diag.code.as_str(), "DS3001");
    assert_eq!(diag.message, "value `seed3` is no longer usable");
    assert_eq!(
        diag.labels.len(),
        2,
        "the moved/used pair is two spans, both inside the item"
    );

    // The reject aside, the whole two-file package (ambient consumer
    // included) checks and linearity-checks clean: the earlier file's
    // `echo` consumes the later corpus file's `far_seed`.
    let path = ds_corpus_path::resolve("../../conformance/toplevel_consumption.ds");
    let source = std::fs::read_to_string(&path).unwrap();
    let mut files = Files::new();
    let amb = files.add_source("ambient.ds", TOPLEVEL_AMBIENT);
    let cor = files.add_source("corpus.ds", &source);
    let asts = [parse(&files, amb).unwrap(), parse(&files, cor).unwrap()];
    let (checked, lin) = ds_check::linearity::check(&files, &asts);
    let mut all = checked.diags.iter().chain(lin.diags.iter());
    let only = all.next().expect("the reject is present");
    assert_eq!(
        all.count(),
        0,
        "the moved-twice reject is the only diagnostic in the two-file package"
    );
    assert_eq!(only.code.as_str(), "DS3001");
}

/// trait_defaults.ds: the trait-member default authority round — every
/// item (the trait's defaulted signature, the omitting impls, and the
/// inherent member's own body default included) checks clean; the run
/// goldens live in the CLI conformance gate.
#[test]
fn trait_defaults_items_check_clean() {
    let rejects = gate(
        "../../conformance/trait_defaults.ds",
        TRAIT_DEFAULTS_AMBIENT,
    );
    assert!(
        rejects.is_empty(),
        "the trait defaults surface declares no reject item"
    );
}

/// records_lists.ds: the list[record] round — every item checks clean
/// under the ambient list surface (the `.ok`/`.err` extraction chains
/// over the checked `list_get` read included); the run goldens live in
/// the CLI conformance gate.
#[test]
fn records_lists_items_check_clean() {
    let rejects = gate("../../conformance/records_lists.ds", RECORDS_LISTS_AMBIENT);
    assert!(
        rejects.is_empty(),
        "the records lists surface declares no reject item"
    );
}

/// frozen_iteration.ds: the D3 iteration ruling and its borrow-conflict
/// refinement — the body's direct `mut` window over the iterated
/// binding is the file's one reject (DS3007), while the aliased-var run
/// item (the runtime guarantee: the frozen entry count bounds the walk)
/// and main check clean; the alias shape's run golden lives in the
/// ds-lower list_for_in suite.
#[test]
fn frozen_iteration_rejects_the_iterated_mut_window() {
    let rejects = gate("../../conformance/frozen_iteration.ds", FROZEN_ITER_AMBIENT);
    let [(id, what, diag)] = rejects.try_into().expect("exactly the one reject item");
    assert_eq!(id, "grow-while-iterating");
    assert!(what.contains("DS3007"), "the reject text names the code");
    assert_eq!(diag.code.as_str(), "DS3007");
    assert!(
        diag.message.contains("this loop's iterated binding"),
        "the iterated-window refusal: {}",
        diag.message
    );

    // every other item checks clean under the same stubs
    let path = ds_corpus_path::resolve("../../conformance/frozen_iteration.ds");
    let source = std::fs::read_to_string(&path).unwrap();
    let stages = stages(&source);
    let mut files = Files::new();
    let amb = files.add_source("ambient.ds", FROZEN_ITER_AMBIENT);
    let cor = files.add_source("corpus.ds", &source);
    let asts = [parse(&files, amb).unwrap(), parse(&files, cor).unwrap()];
    let checked = type_check(&files, &asts);
    let items = items_of(&asts[1]);
    for (item, stage) in items.iter().zip(stages.iter()) {
        if stage.stage.starts_with("reject") {
            continue;
        }
        let diags: Vec<&ds_diag::Diagnostic> = checked
            .diags
            .iter()
            .filter(|d| d.labels.iter().any(|l| within(l.span, item)))
            .collect();
        assert!(
            diags.is_empty(),
            "item `{}` carries diagnostics: {diags:?}",
            stage.id
        );
    }
}

/// transitive_dispatch.ds: the transitive evidence round — every item
/// (the generic impl heads over `list[t]` and the nested dispatch inside
/// the serving body included) checks clean; the run goldens live in the
/// CLI conformance gate.
#[test]
fn transitive_dispatch_items_check_clean() {
    let rejects = gate(
        "../../conformance/transitive_dispatch.ds",
        TRANSITIVE_AMBIENT,
    );
    assert!(
        rejects.is_empty(),
        "the transitive dispatch surface declares no reject item"
    );
}

/// shift_bitwise.ds: the shift and bitwise round — every item (the
/// value, sign, precedence, compound-store, and union-beside-bits rows
/// and the trap-edge functions) checks clean, and the one rejection is
/// the float `&` operand the integer table refuses [B7]; the runs and
/// the oversized-shift traps live in the ds-lower shift_bitwise e2e
/// suite.
#[test]
fn shift_bitwise_items_check_with_the_float_rejection() {
    let rejects = gate("../../conformance/shift_bitwise.ds", SHIFT_BITWISE_AMBIENT);
    let [(id, what, diag)] = rejects.try_into().expect("exactly the one reject item");
    assert_eq!(id, "float-bit");
    assert!(
        what.contains("needs integers"),
        "the reject text names the rule"
    );
    assert_eq!(diag.code.as_str(), "DS2001");
    assert!(
        diag.message.contains("`&` needs integers"),
        "the float operand rejection: {}",
        diag.message
    );

    // every other item checks clean under the same stubs
    let path = ds_corpus_path::resolve("../../conformance/shift_bitwise.ds");
    let source = std::fs::read_to_string(&path).unwrap();
    let stages = stages(&source);
    let mut files = Files::new();
    let amb = files.add_source("ambient.ds", SHIFT_BITWISE_AMBIENT);
    let cor = files.add_source("corpus.ds", &source);
    let asts = [parse(&files, amb).unwrap(), parse(&files, cor).unwrap()];
    let checked = type_check(&files, &asts);
    let items = items_of(&asts[1]);
    for (item, stage) in items.iter().zip(stages.iter()) {
        if stage.stage.starts_with("reject") {
            continue;
        }
        let diags: Vec<&ds_diag::Diagnostic> = checked
            .diags
            .iter()
            .filter(|d| d.labels.iter().any(|l| within(l.span, item)))
            .collect();
        assert!(
            diags.is_empty(),
            "item `{}` carries diagnostics: {diags:?}",
            stage.id
        );
    }
}

/// teardown_deinit.ds: the E5 teardown round — every item (the
/// deinit-carrying `resource`, the nursery passes, and the boundary
/// matches) checks clean under the nursery-shaped ambient; the run
/// goldens live in the CLI conformance gate.
#[test]
fn teardown_deinit_items_check_clean() {
    let rejects = gate("../../conformance/teardown_deinit.ds", TEARDOWN_AMBIENT);
    assert!(
        rejects.is_empty(),
        "the teardown deinit surface declares no reject item"
    );
}

/// none_fields.ds: the item 104 none-typed field honesty round — a
/// field whose written type solves to the none atom alone refuses at
/// its own span in every spelling (the eight DS2042 rejects, each
/// exactly one diagnostic at its own item), a none-headed application
/// hears the tag's own refusal (the DS2040 reject), and the two
/// optional spellings agree at a field and at a case slot (the six
/// typecheck items construct alike).
#[test]
fn none_fields_items_check_with_the_none_only_rejections() {
    let rejects = gate("../../conformance/none_fields.ds", NONE_FIELDS_AMBIENT);
    let ids: Vec<&str> = rejects.iter().map(|(id, ..)| id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "bare-cell",
            "paren-cell",
            "opt-cell",
            "union-cell",
            "alias-cell",
            "payload-cell",
            "case-cell",
            "slot-cell",
            "app-cell",
        ],
        "the reject class is the eight none-typed field spellings plus the none-headed application"
    );
    for (id, _what, diag) in &rejects {
        let code = if id == "app-cell" { "DS2040" } else { "DS2042" };
        assert_eq!(diag.code.as_str(), code, "`{id}` refuses at its own span");
    }
}

/// The corpus header discipline over every conformance file, not only
/// the gated ones: the unresolved-filtered stage count equals the item
/// count (a mismatch makes `item_stages` answer None, so the whole file
/// runs unstaged and every diagnostic turns fatal), and each item that
/// declares a name pairs with the header id at its position — a
/// count-right-order-wrong header otherwise drifts staging silently,
/// since `item_stages` zips positionally and the stage ids are only
/// names. An item-id keeps the declared identifier (either hyphen or
/// underscore spelling); items declaring no name (impls, imports) carry
/// descriptive slugs and check nothing here.
#[test]
fn every_corpus_header_aligns_with_its_items() {
    let dir = ds_corpus_path::dir();
    let mut paths: Vec<PathBuf> = std::fs::read_dir(&dir)
        .expect("the corpus directory lists")
        .filter_map(|entry| {
            let path = entry.expect("the entry reads").path();
            path.extension()
                .is_some_and(|ext| ext == "ds")
                .then_some(path)
        })
        .collect();
    paths.sort();
    assert!(!paths.is_empty(), "the corpus directory holds files");
    for path in &paths {
        let source = std::fs::read_to_string(path).expect("the corpus file reads");
        let name = path.file_name().expect("a corpus name").to_string_lossy();
        let mut files = Files::new();
        let id = files.add_source("corpus.ds", &source);
        let ast = parse(&files, id).expect("the corpus file parses");
        let stages: Vec<Stage> = stages(&source)
            .into_iter()
            .filter(|stage| !stage.stage.starts_with("unresolved"))
            .collect();
        let items = items_of(&ast);
        assert_eq!(
            items.len(),
            stages.len(),
            "{name}: header items and parsed items disagree"
        );
        for (item, stage) in items.iter().zip(stages.iter()) {
            let Some(declared) = item_declares(item) else {
                continue;
            };
            assert_eq!(
                stage.id.replace('-', "_"),
                declared.replace('_', "-").replace('-', "_"),
                "{name}: the header entry at item `{declared}` names another item"
            );
        }
    }
}
