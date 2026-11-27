//! The solved span-keyed records a checked prefix serves: the
//! trait-reference arguments and the generic inherent bounds ride the
//! artifact as solved snapshots over the sets table, so a served
//! declaration build installs them instead of re-deriving them from the
//! trees. The fixture runs one source through the fresh declaring pass
//! and the same trees through a serve of that pass's own artifact, then
//! compares the records raw — the shared intern sequence the
//! mint-determinism pin holds is what makes the raw handles comparable.

use std::sync::Arc;

use ds_check::{DeclaredRows, WorldCaps, decls, resolve_package};
use ds_diag::Files;
use ds_dsi::{extract, serve_prefix};
use ds_parse::parse;

/// The record surface: a generic trait impl spelling its trait
/// reference's arguments, a generic inherent impl with declared bounds,
/// a conditional generic impl recording nothing (its trait reference
/// spells no arguments), and plain impls beside them.
const STABLE: &str = concat!(
    "tag int\n",
    "tag string\n",
    "struct list[t]\n",
    "trait shows {\n",
    "  fn show(self) -> string\n",
    "}\n",
    "trait tells[t: shows] {\n",
    "  fn tell(self, other: t) -> string\n",
    "}\n",
    "impl shows for int {\n",
    "  fn show(self) -> string { \"i\" }\n",
    "}\n",
    "impl[t: shows] tells[list[t]] for list[t] {\n",
    "  fn tell(self, other: list[t]) -> string { \"l\" }\n",
    "}\n",
    "impl[t: shows] shows for list[t] {\n",
    "  fn show(self) -> string { \"l\" }\n",
    "}\n",
    "impl[u: shows] list[u] {\n",
    "  fn first(self) -> string { \"x\" }\n",
    "}\n",
);

/// The span-keyed records of one declaration pass, sorted by span.
#[derive(Debug, PartialEq, Eq, Default)]
struct Records {
    /// The solved trait-reference arguments per impl span.
    args: Vec<(ds_diag::Span, Vec<ds_check::Ty>)>,
    /// The solved inherent bounds per impl span.
    bounds: Vec<(ds_diag::Span, ds_check::InherentBounds)>,
}

impl Records {
    /// Collects one pass's records, sorted by span.
    fn of(decls: &ds_check::Decls) -> Self {
        let mut args: Vec<_> = decls
            .trait_impl_args
            .iter()
            .map(|(span, list)| (*span, list.clone()))
            .collect();
        args.sort_unstable_by_key(|(span, _)| (span.start, span.end));
        let mut bounds: Vec<_> = decls
            .inherent_impl_bounds
            .iter()
            .map(|(span, bounds)| (*span, bounds.clone()))
            .collect();
        bounds.sort_unstable_by_key(|(span, _)| (span.start, span.end));
        Self { args, bounds }
    }
}

/// One source through the fresh declaring pass: the declaration tables
/// whose records the comparison pins.
fn fresh(source: &str) -> ds_check::Decls {
    let mut files = Files::new();
    let id = files.add_source("t.ds", source);
    let ast = parse(&files, id).expect("the source parses");
    let (package, resolutions, resolve_diags) = resolve_package(&[&ast]);
    assert!(resolve_diags.is_empty(), "the source resolves clean");
    let (decls, declare_diags) = decls::build(&package, &[&ast], &resolutions);
    assert!(declare_diags.is_empty(), "the source declares clean");
    decls
}

/// One source's serving pipeline: the artifact extracted from the fresh
/// pass's declaration-phase snapshot beside its full analyze, served
/// back over the same trees through the declaration build. Answers the
/// built tables, or panics where the pipeline refuses (the artifact
/// serves a clean generation or nothing).
fn served(source: &str) -> ds_check::Decls {
    let mut files = Files::new();
    let id = files.add_source("t.ds", source);
    let ast = parse(&files, id).expect("the source parses");
    let (package, resolutions, resolve_diags) = resolve_package(&[&ast]);
    assert!(resolve_diags.is_empty(), "the source resolves clean");
    let (decls, declare_diags) = decls::build(&package, &[&ast], &resolutions);
    assert!(declare_diags.is_empty(), "the source declares clean");
    let tree = Arc::new(ast);
    let analyzed = ds_check::analyze_resolved(
        &files,
        Arc::from(vec![Arc::clone(&tree)]),
        package.into(),
        Arc::from(vec![resolutions.into_iter().next().expect("one")]),
        resolve_diags,
        Arc::default(),
    );
    assert!(
        analyzed.checked.diags.is_empty(),
        "the fixture checks clean: {:?}",
        analyzed.checked.diags
    );
    let rows = DeclaredRows::new();
    let caps = WorldCaps::new();
    let effects = ds_check::effects::effects_of_resolved(
        &analyzed.asts[..],
        &analyzed.package,
        &analyzed.resolutions[..],
        &analyzed.checked.hole_show_dispatches,
        &rows,
        &caps,
    );
    let method_rows = ds_check::effects::solved_method_rows(
        &analyzed.asts[..],
        &analyzed.package,
        &analyzed.resolutions[..],
        &rows,
        &caps,
    );
    let artifact = extract(&decls, &analyzed, &effects, &method_rows);
    let prefix = serve_prefix(&artifact, &[id]).expect("the artifact serves");
    let ds_dsi::ServedPrefix {
        package,
        resolutions,
        decls,
        ..
    } = prefix;
    let (built, diags) = decls::build_over(&package, &[tree.as_ref()], &resolutions, Some(decls))
        .expect("the served rows resolve over the same walk");
    assert!(diags.is_empty(), "the served pass declares clean");
    let _ = ds_check::Ty::index;
    built
}

/// The served build installs the fresh pass's records exactly — the
/// same spans, the same solved handles (the shared intern sequence
/// makes the raw handle comparison the identity pin: a serve that
/// re-interned anything would answer different indices).
#[test]
fn the_served_prefix_installs_the_solved_records() {
    let fresh = fresh(STABLE);
    let served = served(STABLE);
    let fresh = Records::of(&fresh);
    let served = Records::of(&served);
    assert!(
        !fresh.args.is_empty(),
        "the fixture records trait arguments"
    );
    assert!(
        !fresh.bounds.is_empty(),
        "the fixture records inherent bounds"
    );
    assert_eq!(
        served.args, fresh.args,
        "the trait-reference arguments must install identically"
    );
    assert_eq!(
        served.bounds, fresh.bounds,
        "the inherent bounds must install identically"
    );
}

/// One atom's served identity: the name, the kind's shape face, and the
/// fn-signature face where the atom carries one.
fn atom_faces(decls: &ds_check::Decls) -> Vec<String> {
    let universe = &decls.universe;
    (0..universe.atom_count())
        .map(|index| {
            let id = ds_check::TypeId::from_raw(index);
            let decl = universe.atom_decl(id).expect("every id has a decl");
            let kind = match &decl.kind {
                ds_check::AtomKind::Unit => "unit",
                ds_check::AtomKind::Reserved => "reserved",
                ds_check::AtomKind::Struct(fields) => {
                    return format!("{} {} struct {}", index, decl.name, fields.len());
                },
                ds_check::AtomKind::Enum(cases) => {
                    return format!("{} {} enum {}", index, decl.name, cases.len());
                },
                ds_check::AtomKind::Tuple(elements) => {
                    return format!("{} {} tuple {}", index, decl.name, elements.len());
                },
                ds_check::AtomKind::Apply(..) => "apply",
                ds_check::AtomKind::FnSig(..) => "fnsig",
            };
            let face = universe.fn_sig(id).map_or_else(
                || kind.to_owned(),
                |(params, ret)| {
                    format!(
                        "fn({}) -> {}",
                        params
                            .iter()
                            .map(|ty| universe.render(*ty))
                            .collect::<Vec<_>>()
                            .join(", "),
                        universe.render(ret)
                    )
                },
            );
            format!("{} {} {}", index, decl.name, face)
        })
        .collect()
}

/// A fn-typed struct field and a fn-typed case payload mint
/// fn-signature atoms in the shape phase: the mint log carries them as
/// solved parameter and return sets, the serve replays them through the
/// same hash-consing, and the served universe's atoms answer the fresh
/// pass's ids, names, and signatures — the DSI shape-phase mint the
/// CLI-facing encode walks for every clean program declaring a
/// fn-typed field.
#[test]
fn a_fn_typed_field_serves_through_the_mint_log() {
    let source = concat!(
        "tag int\n",
        "tag string\n",
        "struct handler {\n",
        "  f: fn(int) -> int,\n",
        "  n: int\n",
        "}\n",
        "enum held {\n",
        "  solo(fn(int) -> string, int)\n",
        "}\n",
    );
    let fresh = fresh(source);
    let served = served(source);
    let fresh_faces = atom_faces(&fresh);
    let served_faces = atom_faces(&served);
    // the fn-signature atom's structural name is its own signature
    // face, so the entry spells the sig twice
    assert!(
        fresh_faces
            .iter()
            .any(|face| face.contains(" fn(int) -> int fn(int) -> int")),
        "the fresh pass mints the fn-signature atom: {fresh_faces:?}"
    );
    assert!(
        fresh_faces
            .iter()
            .any(|face| face.contains(" fn(int) -> string fn(int) -> string")),
        "the fresh pass mints the payload's fn-signature atom: {fresh_faces:?}"
    );
    assert_eq!(
        served_faces, fresh_faces,
        "the served universe must replay every shape-phase mint identically"
    );
}
