//! The `#[sealed]` origin attribute across a served prefix: the seal is
//! a declaration-side fact, so a served compile must read it exactly
//! like the fresh pass does — a consumer impl over a served sealed
//! trait refuses with the same diagnostic, and the origin's own impl
//! beside the declaration stays legal. The checked-ambient serve is the
//! deployment face: a sealed trait in a served ambient behaves
//! identically to a fresh compile.

use std::sync::Arc;

use ds_check::{DeclaredRows, WorldCaps, decls, resolve_package};
use ds_diag::{ErrorCode, Files};
use ds_dsi::{PrefixDsi, extract, serve_prefix};
use ds_parse::{Ast, parse};

/// The sealed origin member: the trait carries the seal, and its own
/// impl sits beside the declaration — the only impls the seal admits.
const ORIGIN: &str = concat!(
    "pub tag int\n",
    "pub tag string\n",
    "#[sealed]\n",
    "pub trait greet {\n",
    "  fn greet(self) -> string\n",
    "}\n",
    "\n",
    "pub struct cell {\n",
    "  n: int\n",
    "}\n",
    "\n",
    "impl greet for cell {\n",
    "  fn greet(self) -> string { \"base\" }\n",
    "}\n",
);

/// The consumer member: its own struct and its impl over the origin's
/// sealed trait — a head the origin never impls, so the coherence
/// overlap never masks the seal's own refusal.
const CONSUMER: &str = concat!(
    "struct widget {\n",
    "  label: string\n",
    "}\n",
    "\n",
    "impl greet for widget {\n",
    "  fn greet(self) -> string { \"root\" }\n",
    "}\n",
);

/// The same shape without the seal: the control proving the refusal is
/// the seal's, not the two-file shape's.
fn open_origin() -> String {
    ORIGIN.replace("#[sealed]\n", "")
}

/// The member pair over one `Files`: the origin registered first (the
/// discipline the serve's file check pins), the consumer second. Both
/// passes below read the same spans.
fn trees(files: &mut Files, origin: &str) -> (ds_diag::FileId, Arc<Ast>, Arc<Ast>) {
    let origin_id = files.add_source("origin.ds", origin);
    let consumer_id = files.add_source("consumer.ds", CONSUMER);
    let origin_ast = parse(files, origin_id).expect("the origin parses");
    let consumer_ast = parse(files, consumer_id).expect("the consumer parses");
    (origin_id, Arc::new(origin_ast), Arc::new(consumer_ast))
}

/// The fresh pass over the member pair: its diagnostics and its seal
/// table are the answer the served pass must reproduce.
fn fresh(origin: &Arc<Ast>, consumer: &Arc<Ast>) -> (ds_check::Decls, Vec<ds_diag::Diagnostic>) {
    let (package, resolutions, resolve_diags) =
        resolve_package(&[origin.as_ref(), consumer.as_ref()]);
    assert!(
        resolve_diags.is_empty(),
        "the pair resolves clean: {resolve_diags:?}"
    );
    decls::build_over(
        &package,
        &[origin.as_ref(), consumer.as_ref()],
        &resolutions,
        None,
    )
    .expect("a fresh build carries no serve to drift")
}

/// The serving pipeline: the artifact extracted from the origin's own
/// clean compile and served back beside the consumer tree — the
/// ambient serve's shape (one served leading tree, the consumer trees
/// fresh after it, the served namespace adopted and the fresh files
/// joined into it exactly like the analyze entries do).
fn served(
    files: &mut Files,
    origin_id: ds_diag::FileId,
    origin: &Arc<Ast>,
    consumer: &Arc<Ast>,
) -> (ds_check::Decls, Vec<ds_diag::Diagnostic>) {
    debug_assert_eq!(origin.file, origin_id, "the origin registers first");
    let (package, resolutions, resolve_diags) = resolve_package(&[origin.as_ref()]);
    assert!(
        resolve_diags.is_empty(),
        "the origin resolves clean: {resolve_diags:?}"
    );
    let (decls, declare_diags) = decls::build(&package, &[origin.as_ref()], &resolutions);
    assert!(declare_diags.is_empty(), "the origin declares clean");
    let analyzed = ds_check::analyze_resolved(
        files,
        Arc::from(vec![Arc::clone(origin)]),
        package.into(),
        Arc::from(vec![resolutions.into_iter().next().expect("one")]),
        resolve_diags,
        Arc::default(),
    );
    assert!(
        analyzed.checked.diags.is_empty(),
        "the origin checks clean: {:?}",
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
    let artifact: PrefixDsi = extract(&decls, &analyzed, &effects, &method_rows);
    let prefix = serve_prefix(&artifact, &[origin_id]).expect("the artifact serves");
    let ds_dsi::ServedPrefix {
        mut package,
        resolutions,
        decls,
        ..
    } = prefix;
    // The analyze entries' composition: the fresh trees join the served
    // namespace, and their resolution tables answer the joined package.
    let add_diags = package.add_file(consumer.as_ref());
    assert!(
        add_diags.is_empty(),
        "the consumer joins clean: {add_diags:?}"
    );
    let (consumer_uses, resolve_diags) = package.resolve(consumer.as_ref());
    assert!(
        resolve_diags.is_empty(),
        "the consumer resolves clean: {resolve_diags:?}"
    );
    let origin_uses = resolutions.into_iter().next().expect("one");
    decls::build_over(
        &package,
        &[origin.as_ref(), consumer.as_ref()],
        &[origin_uses, consumer_uses],
        Some(decls),
    )
    .expect("the served rows resolve over the same walk")
}

/// The seal table of one declaration pass, keyed by def index.
fn seals(decls: &ds_check::Decls) -> Vec<(u32, ds_diag::Span)> {
    let mut rows: Vec<_> = decls
        .sealed_traits
        .iter()
        .map(|(def, span)| (def.index(), *span))
        .collect();
    rows.sort_unstable_by_key(|(def, _)| *def);
    rows
}

/// A consumer impl over a served sealed trait refuses with the fresh
/// pass's exact diagnostic, and the seal table answers identically —
/// the served compile enforcing the seal is the fresh compile.
#[test]
fn a_consumer_impl_over_a_served_sealed_trait_refuses_like_fresh() {
    let mut files = Files::new();
    let (origin_id, origin, consumer) = trees(&mut files, ORIGIN);
    let (fresh_decls, fresh_diags) = fresh(&origin, &consumer);
    assert!(
        fresh_diags
            .iter()
            .any(|diag| diag.code == ErrorCode::SealedImplRefused),
        "the fresh pass refuses the consumer's impl: {fresh_diags:?}"
    );
    let (served_decls, served_diags) = served(&mut files, origin_id, &origin, &consumer);
    assert!(
        served_decls.served_defs > 0,
        "the prefix must arrive served, not rebuilt"
    );
    assert_eq!(
        seals(&served_decls),
        seals(&fresh_decls),
        "the served pass records the same seal"
    );
    assert_eq!(
        served_diags, fresh_diags,
        "the served pass answers the fresh pass's refusal"
    );
}

/// The control: the same two-file shape over an unsealed trait admits
/// the consumer's impl on both paths, so the refusal above is the
/// seal's own.
#[test]
fn an_unsealed_trait_admits_the_consumer_impl_on_both_paths() {
    let mut files = Files::new();
    let (origin_id, origin, consumer) = trees(&mut files, &open_origin());
    let (fresh_decls, fresh_diags) = fresh(&origin, &consumer);
    assert!(
        fresh_diags.is_empty(),
        "the unsealed pair declares clean: {fresh_diags:?}"
    );
    let (served_decls, served_diags) = served(&mut files, origin_id, &origin, &consumer);
    assert_eq!(seals(&served_decls), seals(&fresh_decls));
    assert_eq!(served_diags, fresh_diags);
    assert!(served_diags.is_empty(), "the unsealed pair admits the impl");
}
