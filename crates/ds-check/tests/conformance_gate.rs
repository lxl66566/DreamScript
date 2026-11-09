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
/// signatures only (extern), the M3 stdlib owns the bodies.
const TYPES_AMBIENT: &str = concat!(
    "tag int\n",
    "tag float\n",
    "tag string\n",
    "tag bool\n",
    "trait Copyable { }\n",
    "struct list[t]\n",
    "struct map[k, v]\n",
    "extern fn read_file (path: string) -> string | io_err\n",
    "extern fn parse_json (text: string) -> json | io_err\n",
    "extern fn str[t] (v: t) -> string\n",
    "extern fn log (msg: string) -> ()\n",
    "extern fn default_config () -> config\n",
    "extern fn get[k, v] (m: map[k, v], key: k) -> v | none\n",
);

/// Ambient stdlib stubs per the linearity_modes_rc list: modes included,
/// since C2 consumption is what the affine pass reads at call sites.
const LINEARITY_AMBIENT: &str = concat!(
    "tag int\n",
    "tag char\n",
    "tag bool\n",
    "tag string\n",
    "type u32 = int\n",
    "trait Copyable { }\n",
    "struct bytes: ~Copyable { len: int }\n",
    "struct list[t]\n",
    "extern fn open (path: string) -> file\n",
    "extern fn close (fd: raw_fd) -> ()\n",
    "extern fn read_exact (f: mut file, n: int) -> bytes\n",
    "extern fn to_u32_le (b: move bytes) -> u32\n",
    "extern fn split (s: string, sep: char) -> list[string]\n",
    "extern fn map[t, r] (xs: list[t], f: fn(t) -> r) -> list[r]\n",
    "extern fn collect[r] (xs: list[r]) -> list[r]\n",
    "extern fn len[t] (xs: t) -> int\n",
);

/// Ambient for the linearity negative suite: types only, everything else
/// the file declares itself.
const LINEARITY_NEG_AMBIENT: &str = concat!("tag int\n", "tag string\n", "trait Copyable { }\n",);

/// The minimal shared vocabulary for the corpus-wide affine sweep: every
/// file declares its own data types.
const SWEEP_AMBIENT: &str = concat!(
    "tag int\n",
    "tag float\n",
    "tag string\n",
    "tag bool\n",
    "tag char\n",
    "trait Copyable { }\n",
    "struct list[t]\n",
    "struct map[k, v]\n",
);

/// Ambient stdlib stubs per the generics_coherent_traits list.
const GENERICS_AMBIENT: &str = concat!(
    "tag int\n",
    "tag string\n",
    "tag bool\n",
    "trait Copyable { }\n",
    "struct list[t]\n",
    "extern fn cons[t] (x: t, xs: list[t]) -> list[t]\n",
    "extern fn head[t] (xs: list[t]) -> t?\n",
    "extern fn tail[t] (xs: list[t]) -> list[t]\n",
    "extern fn join (parts: list[string], sep: string) -> string\n",
    "extern fn map[t, r] (xs: list[t], f: fn(t) -> r) -> list[r]\n",
    "extern fn str[t] (v: t) -> string\n",
);

fn items_of(ast: &Ast) -> Vec<&Item> {
    ast.blocks.iter().flat_map(|b| b.items.iter()).collect()
}

fn within(span: Span, item: &Item) -> bool {
    span.file == item.span.file && span.start >= item.span.start && span.end <= item.span.end
}

fn gate(corpus: &str, ambient: &str) -> Vec<(String, String, ds_diag::Diagnostic)> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(corpus);
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

#[test]
fn types_hybrid_known_tension_is_confined_to_get_port() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../conformance/types_hybrid.ds");
    let source = std::fs::read_to_string(&path).unwrap();
    let mut files = Files::new();
    let amb = files.add_source("ambient.ds", TYPES_AMBIENT);
    let cor = files.add_source("corpus.ds", &source);
    let asts = [parse(&files, amb).unwrap(), parse(&files, cor).unwrap()];
    let checked = type_check(&files, &asts);
    let get_port = items_of(&asts[1])
        .into_iter()
        .find(|i| {
            matches!(
                &i.kind,
                ItemKind::Local(l)
                    if matches!(&l.pat, ds_parse::Pattern::Binding { name, .. } if name.name == "get_port")
            )
        })
        .expect("get_port item");
    let mut confined = 0;
    for diag in &checked.diags {
        let inside = diag.labels.iter().any(|l| within(l.span, get_port));
        assert!(
            inside,
            "diagnostic outside get_port: {} {}",
            diag.code.as_str(),
            diag.message
        );
        assert_eq!(diag.code.as_str(), "DS2001");
        confined += 1;
    }
    assert_eq!(
        confined, 2,
        "the known get_port tension is exactly two ordering errors"
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
    assert_eq!(id, "impl-json-int-bool");
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
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../conformance/generics_coherent_traits.ds");
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
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(corpus);
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
        if let Some(what) = stage.stage.strip_prefix("reject ") {
            if let Some(diag) = &lin_diag {
                rejects.push((stage.id.clone(), what.to_owned(), diag.clone()));
            }
        }
    }
    (rejects, all, lin, source)
}

/// The 13.2 file checks fully clean: typecheck and run items (which must
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
        6,
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

    let (id, what, diag) = by_id("projection-moved-then-used");
    assert!(what.contains("DS3001"), "{id}: {what}");
    assert_eq!(diag.code.as_str(), "DS3001");
    assert_eq!(diag.message, "value `s` is no longer usable");
    assert_eq!(diag.labels.len(), 2, "the transfer's pair is two spans");
    assert_eq!(diag.labels[0].label, "moved here");
    assert_eq!(diag.labels[1].label, "used again here after move");
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
        ("../../conformance/linearity_modes_rc.ds", LINEARITY_AMBIENT),
        (
            "../../conformance/linearity_negative.ds",
            LINEARITY_NEG_AMBIENT,
        ),
        ("../../conformance/effects_nursery.ds", SWEEP_AMBIENT),
        ("../../conformance/syntax_ts_heart.ds", SWEEP_AMBIENT),
        (
            "../../conformance/generics_coherent_traits.ds",
            GENERICS_AMBIENT,
        ),
        ("../../conformance/modules_hash_core.ds", SWEEP_AMBIENT),
    ];
    for (corpus, ambient) in corpus {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(corpus);
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
        ItemKind::Impl(_) | ItemKind::Import(_) => return None,
    };
    Some(name.clone())
}
