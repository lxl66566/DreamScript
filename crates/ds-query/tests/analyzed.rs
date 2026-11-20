//! The analyzed-package query: the analyzer's own pass memoized at
//! package grain, the value the run path lowers from. The bundle's
//! trees are the derive-expanded ones the tables were built over, so a
//! consumer lowering from it never re-analyzes the queries' inputs.

use std::{rc::Rc, sync::Arc};

use ds_diag::FileId;
use ds_query::{
    Analyzed, AnalyzedOutput, BlockId, Db, DefId, Expanded, PackageId, ParseFile, QueryKey,
    Resolve, TypeOf,
};

/// A package without derives: the expansion is an identical clone, so
/// the bundle's tables answer the package's own facts. The tag declares
/// the int atom the same way the ambient would (no ambient is registered
/// in these fixtures).
const PLAIN: &str = concat!(
    "tag int\n",
    "struct cell {\n  value: int\n}\n",
    "##\n",
    "const answer = 42\n",
);

/// A typed const with both atom tags declared: a later edit moves the
/// binding's type text between two declared atoms without any other
/// fact moving (no diagnostic, no namespace change), so a renewal
/// compare that misses the type rows would wrongly answer equal.
const TYPED: &str = concat!("tag int\n", "tag string\n", "##\n", "const answer = 42\n",);

/// `TYPED` with the binding retyped to the string atom.
fn retyped() -> String {
    TYPED.replace("42", "\"note\"")
}

/// One derive attribute: the expansion strips it and appends one
/// synthesized impl item beside the attributed declaration.
const DERIVED: &str = "#[derive(show)] struct point { x: int, y: int }\n";

fn analyzed() -> QueryKey {
    QueryKey::Analyzed(PackageId::new(0))
}

/// Top-level items across every block of one tree.
fn item_count(ast: &ds_parse::Ast) -> usize {
    ast.blocks.iter().map(|block| block.items.len()).sum()
}

#[test]
fn the_bundle_carries_the_analyzer_s_own_tables() {
    let mut db = Db::new();
    let file = db.add_file("plain.ds", PLAIN);
    let bundle = db.get::<Analyzed>(PackageId::new(0)).unwrap();
    let tables = &bundle.analyzed;

    // The namespace and the top-level types answer from the bundle's own
    // walk, rendered through its own universe.
    let answer = tables
        .package
        .lookup("answer")
        .expect("the const is in the namespace");
    let ty = tables
        .checked
        .def_types
        .get(&answer)
        .expect("the const has a solved type");
    assert_eq!(tables.decls.universe.render(*ty), "int");
    assert!(
        tables.checked.diags.is_empty(),
        "the package checks clean: {:?}",
        tables.checked.diags
    );
    assert!(tables.package.lookup("cell").is_some());

    // The parse memo still holds the source tree; the bundle holds the
    // expanded clone the tables were built over (an identical clone for
    // a package without derives).
    let parse = db.get::<ParseFile>(file).unwrap();
    let ast = parse.ast.as_ref().expect("the file parses");
    assert_eq!(item_count(ast), 3);
    assert_eq!(tables.asts.len(), 1);
    assert_eq!(item_count(&tables.asts[0]), 3);
}

#[test]
fn the_bundle_s_trees_are_the_derive_expanded_ones() {
    let mut db = Db::new();
    let file = db.add_file("derived.ds", DERIVED);
    let bundle = db.get::<Analyzed>(PackageId::new(0)).unwrap();
    let tables = &bundle.analyzed;

    let parse = db.get::<ParseFile>(file).unwrap();
    let ast = parse.ast.as_ref().expect("the file parses");
    assert_eq!(item_count(ast), 1, "the source tree holds the one struct");

    // The consumed attribute is stripped and one synthesized impl sits
    // beside the declaration: the trees the bundle's tables were built
    // over, never the parse memo's originals.
    assert_eq!(tables.asts.len(), 1);
    assert_eq!(item_count(&tables.asts[0]), 2);
    assert!(
        tables.asts[0]
            .blocks
            .iter()
            .all(|block| block.items.iter().all(|item| item.attrs.is_empty())),
        "the expansion consumed the derive attribute"
    );

    // The check ran over the expanded trees: the synthesized member's
    // trait names nothing this package defines (no ambient is
    // registered), so the bundle carries the expanded walk's
    // resolution diagnostics.
    assert!(
        !tables.checked.diags.is_empty(),
        "the synthesized member's unresolvable trait reports through the bundle"
    );
}

#[test]
fn memo_hits_share_every_payload() {
    let mut db = Db::new();
    let _ = db.add_file("plain.ds", PLAIN);
    let _ = db.add_file("derived.ds", DERIVED);

    let first = db.get::<Analyzed>(PackageId::new(0)).unwrap();
    let second = db.get::<Analyzed>(PackageId::new(0)).unwrap();
    assert_eq!(db.compute_count(&analyzed()), 1);
    assert!(Arc::ptr_eq(
        &first.analyzed.package,
        &second.analyzed.package
    ));
    assert!(Arc::ptr_eq(
        &first.analyzed.resolutions,
        &second.analyzed.resolutions
    ));
    assert!(Rc::ptr_eq(&first.analyzed.decls, &second.analyzed.decls));
    assert!(Arc::ptr_eq(
        &first.analyzed.checked,
        &second.analyzed.checked
    ));
    assert!(Arc::ptr_eq(&first.analyzed.asts, &second.analyzed.asts));
}

#[test]
fn the_bundle_reuses_the_expansion_and_resolve_memos() {
    let mut db = Db::new();
    let _ = db.add_file("derived.ds", DERIVED);

    // The bundle is the check half over the expansion and resolve
    // values: no second expansion, no second resolution, and the
    // shared handles are the memos' own arcs.
    let resolved = db.get::<Resolve>(PackageId::new(0)).unwrap();
    let bundle = db.get::<Analyzed>(PackageId::new(0)).unwrap();
    let expanded = db.get::<Expanded>(PackageId::new(0)).unwrap();
    assert!(Arc::ptr_eq(&resolved.package, &bundle.analyzed.package));
    assert!(Arc::ptr_eq(
        &resolved.resolutions,
        &bundle.analyzed.resolutions
    ));
    assert!(Arc::ptr_eq(&expanded.asts, &bundle.analyzed.asts));
    assert_eq!(
        db.compute_count(&QueryKey::Resolve(PackageId::new(0))),
        1,
        "the bundle never resolves the parse trees itself"
    );
    assert_eq!(
        db.compute_count(&QueryKey::Expanded(PackageId::new(0))),
        1,
        "the bundle never expands the parse trees itself"
    );
}

#[test]
fn an_edit_recomputes_the_bundle_once() {
    let mut db = Db::new();
    let file = db.add_file("plain.ds", PLAIN);
    let _ = db.get::<Analyzed>(PackageId::new(0)).unwrap();
    assert_eq!(db.compute_count(&analyzed()), 1);

    // A no-op write bumps no revision and renews nothing.
    db.set_file_content(file, PLAIN).unwrap();
    let _ = db.get::<Analyzed>(PackageId::new(0)).unwrap();
    assert_eq!(db.compute_count(&analyzed()), 1);

    // A real edit moves the parse value the bundle depends on: one
    // recompute for the whole package, however many files it holds.
    db.set_file_content(file, &PLAIN.replace("42", "43"))
        .unwrap();
    let _ = db.get::<Analyzed>(PackageId::new(0)).unwrap();
    assert_eq!(db.compute_count(&analyzed()), 2);
}

/// One bundle over `source`: an independent database, so nothing is
/// shared with another bundle and the equality walks the facts
/// themselves.
fn bundle_of(source: &str) -> AnalyzedOutput {
    let mut db = Db::new();
    let _ = db.add_file("plain.ds", source);
    db.get::<Analyzed>(PackageId::new(0)).unwrap()
}

#[test]
fn bundles_compare_by_facts_across_databases() {
    // No arc is shared between two databases, so pointer identity answers
    // nothing and the compare walks every fact: equal sources compare
    // equal through the whole package, resolutions, diagnostics, and
    // rendered type rows.
    let typed = bundle_of(TYPED);
    let twin = bundle_of(TYPED);
    assert_eq!(typed, twin);

    // One def's type text moved (int -> string) and nothing else did: the
    // rendered rows compare unequal, so a renewal against this bundle
    // must flip, never serve the stale type.
    let retyped = bundle_of(&retyped());
    assert_ne!(typed, retyped);
}

/// The renewal compare's decision is observable through the check's
/// readers: a fact-free edit (bytes move, facts hold) recomputes the
/// checked package but renews it, so the per-definition story's memo
/// verifies without recomputing; a def's type-text change flips the
/// decision, the check's change-revision moves, and the story
/// recomputes.
#[test]
fn a_fact_free_edit_renews_the_check_and_a_type_text_change_flips_it() {
    let mut db = Db::new();
    let file = db.add_file("typed.ds", TYPED);
    let answer = DefId::from_block(BlockId {
        file: FileId::new(0),
        index: 1,
    })
    .expect("the fixture's files stay under the packing bounds");
    let story = db.get::<TypeOf>(answer).unwrap();
    assert_eq!(story.types.len(), 1);
    assert_eq!(story.types[0].1, "int");
    assert!(db.is_fresh(&QueryKey::TypeOf(answer)));

    // A comment-only edit moves the bytes the parse depends on, so the
    // checked package recomputes — but its facts compare equal and the
    // memo renews: the change-revision stays put and the story verifies
    // against it without a recompute.
    db.set_file_content(file, &format!("{TYPED}\n// note\n"))
        .unwrap();
    db.get::<TypeOf>(answer).unwrap();
    assert_eq!(db.compute_count(&QueryKey::TypeOf(answer)), 1);
    assert!(db.is_fresh(&QueryKey::TypeOf(answer)));

    // One def's type text moved (int -> string): the recomputed check
    // compares unequal, the renewal flips, and the story recomputes to
    // the moved type.
    db.set_file_content(file, &retyped()).unwrap();
    let story = db.get::<TypeOf>(answer).unwrap();
    assert_eq!(db.compute_count(&QueryKey::TypeOf(answer)), 2);
    assert_eq!(story.types[0].1, "string");
}
