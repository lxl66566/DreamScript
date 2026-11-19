//! The served prefix's solved span-keyed records — the trait-reference
//! arguments and the generic inherent bounds — rebuild over the served
//! trees ([`build_over`](ds_check::decls::build_over)'s rebuild pass): a
//! prefix whose declaration tables arrive pre-built still answers them
//! identically to the declaring pass, because the rebuild replays the
//! declaring arms' derivations over the same universe content. The
//! serve simulation here hands `build_over` the fresh pass's own
//! tables with the records cleared — the in-crate form of what an
//! artifact serve installs — while the ds-stdlib drift gates pin the
//! replayed-universe form end to end.

use ds_check::{ServedDecls, decls, resolve_package};
use ds_diag::Files;
use ds_parse::parse;

/// The rebuild surface: a generic trait impl spelling its trait
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

/// An impl whose trait reference spells a trait as a set: the declaring
/// pass solves it against the membership state of its moment, a state
/// the rebuild cannot reproduce, so the rebuild records nothing and the
/// artifact's clean generation refuses such a prefix (the drift gate's
/// record comparison).
const TRAIT_RELATIVE: &str = concat!(
    "tag int\n",
    "tag string\n",
    "trait shows {\n",
    "  fn show(self) -> string\n",
    "}\n",
    "trait tells[t: shows] {\n",
    "  fn tell(self, other: t) -> string\n",
    "}\n",
    "impl shows for int {\n",
    "  fn show(self) -> string { \"i\" }\n",
    "}\n",
    "impl tells[shows] for string {\n",
    "  fn tell(self, other: string) -> string { \"s\" }\n",
    "}\n",
);

/// The span-keyed records of one declaration pass.
#[derive(Debug, Default)]
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

/// One source through the fresh declaration pass, then the same trees
/// through `build_over` over the fresh tables with the records cleared
/// (the serve simulation), asserting both passes declare clean.
fn passes(source: &str) -> (Records, Records) {
    let mut files = Files::new();
    let id = files.add_source("t.ds", source);
    let ast = parse(&files, id).expect("the source parses");
    let (package, resolutions, resolve_diags) = resolve_package(&[&ast]);
    assert!(resolve_diags.is_empty(), "the source resolves clean");
    let (mut decls, declare_diags) = decls::build(&package, &[&ast], &resolutions);
    assert!(declare_diags.is_empty(), "the source declares clean");
    let fresh = Records::of(&decls);
    decls.served_defs = u32::try_from(package.len()).expect("def count fits u32");
    decls.served_methods = u32::try_from(package.methods().count()).expect("method count fits");
    decls.trait_impl_args.clear();
    decls.inherent_impl_bounds.clear();
    let served = ServedDecls {
        decls,
        memberships: Vec::new(),
        mints: Vec::new(),
        files: 1,
    };
    let (rebuilt, declare_diags) = decls::build_over(&package, &[&ast], &resolutions, Some(served));
    assert!(declare_diags.is_empty(), "the served pass declares clean");
    (fresh, Records::of(&rebuilt))
}

/// The rebuild answers the declaring pass's records exactly — the same
/// spans, the same solved handles (the shared universe makes the raw
/// handle comparison the identity pin: a rebuild that minted or interned
/// anything would answer different indices).
#[test]
fn the_served_prefix_rebuilds_the_solved_records() {
    let (fresh, rebuilt) = passes(STABLE);
    assert!(
        !fresh.args.is_empty(),
        "the fixture records trait arguments"
    );
    assert!(
        !fresh.bounds.is_empty(),
        "the fixture records inherent bounds"
    );
    assert_eq!(
        rebuilt.args, fresh.args,
        "the trait arguments rebuild identically"
    );
    assert_eq!(
        rebuilt.bounds, fresh.bounds,
        "the inherent bounds rebuild identically"
    );
}

/// A trait-relative argument is the recorded boundary: the declaring
/// pass records it against its mid-growth membership state, the rebuild
/// cannot reproduce that state and records nothing — the divergence the
/// artifact's clean generation must refuse (never a silent drift).
#[test]
fn a_trait_relative_argument_stays_a_refused_rebuild() {
    let (fresh, rebuilt) = passes(TRAIT_RELATIVE);
    assert!(
        !fresh.args.is_empty(),
        "the declaring pass records the argument"
    );
    assert!(
        rebuilt.args.is_empty(),
        "the rebuild refuses the trait-relative argument"
    );
}
