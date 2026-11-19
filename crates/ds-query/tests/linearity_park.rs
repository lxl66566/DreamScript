//! The ambient-prefixed affine walk behind the process park: the
//! package's linearity pass over the stdlib ambient plus member files
//! answers the one plain walk's rows across edits — the first query's
//! own walk filling the park over the ambient prefix, later queries
//! installing it — and a package the ambient does not lead never
//! installs it.

use ds_diag::FileId;
use ds_query::{
    Analyzed, BlockId, Db, DefId, LinearityCheck, PackageId, PackageLinearity, QueryKey,
};

/// One member over the ambient's own linear surface: the handle's
/// borrowed handler call and its scope-exit deinit keep the walk's rows
/// non-trivial.
const MEMBER: &str = concat!(
    "pub fn work(p: string) -> () {\n",
    "  const h = open_file(p)\n",
    "  close_file(h)\n",
    "}\n",
);

/// The member with a second linear block added: a real edit, so the
/// pass re-derives (the expansion is its identity anchor) over the
/// installed park.
const MEMBER_EDIT: &str = concat!(
    "pub fn work(p: string) -> () {\n",
    "  const h = open_file(p)\n",
    "  close_file(h)\n",
    "}\n",
    "##\n",
    "pub fn more(p: string) -> () {\n",
    "  const g = open_file(p)\n",
    "  close_file(g)\n",
    "}\n",
);

/// The member's first block, as a definition id.
fn first_block(file: FileId) -> DefId {
    DefId::from_block(BlockId { file, index: 0 })
        .expect("the member's first block stays under the packing bounds")
}

/// Asserts the served pass's whole-package rows equal the plain walk's
/// over the same bundle: the fill and the install must both answer the
/// one walk.
fn rows_answer_the_plain_walk(db: &mut Db, tag: &str) {
    let served = db
        .get::<PackageLinearity>(PackageId::new(0))
        .unwrap()
        .rows();
    let analyzed = db.get::<Analyzed>(PackageId::new(0)).unwrap();
    let bundle = &analyzed.analyzed;
    let plain = ds_check::linearity_check(
        &bundle.asts,
        &bundle.package,
        &bundle.resolutions,
        &bundle.decls,
        &bundle.checked,
    );
    assert_eq!(served.diags, plain.diags, "diagnostics match ({tag})");
    assert_eq!(served.moves, plain.moves, "moves match ({tag})");
    assert_eq!(served.drops, plain.drops, "drops match ({tag})");
    assert_eq!(served.last_uses, plain.last_uses, "last uses match ({tag})");
}

#[test]
fn the_ambient_prefixed_pass_answers_the_plain_walk_across_edits() {
    let mut db = Db::new();
    let ambient = db.add_file(ds_stdlib::AMBIENT_FILE, ds_stdlib::AMBIENT_SOURCE);
    assert_eq!(ambient.as_u32(), 0);
    let member = db.add_file("member.ds", MEMBER);

    // Round one: the member's sliced facts arrive from the pass's own
    // walk, which fills the process park over the ambient prefix on its
    // way through.
    let first = db.get::<LinearityCheck>(first_block(member)).unwrap();
    assert!(first.diags.is_empty());
    assert!(!first.drops.is_empty(), "the handle's deinit drops");
    assert!(!first.last_uses.is_empty(), "the handle's call is a use");
    assert!(
        ds_stdlib::ambient_linearity().get().is_some(),
        "the first query's walk fills the park"
    );
    rows_answer_the_plain_walk(&mut db, "the filling walk");

    // Round two: the edit re-derives the pass, now installing the park —
    // the compute really ran again, and the rows still answer the plain
    // walk's.
    db.set_file_content(member, MEMBER_EDIT).unwrap();
    let second = db.get::<LinearityCheck>(first_block(member)).unwrap();
    assert!(second.diags.is_empty());
    rows_answer_the_plain_walk(&mut db, "the installed walk");
    assert_eq!(
        db.compute_count(&QueryKey::PackageLinearity(PackageId::new(0))),
        2,
        "the edit re-derived the package pass"
    );
}

#[test]
fn a_package_the_ambient_does_not_lead_never_installs_the_park() {
    // Fill the park over the standard shape first: the ambient as the
    // first-registered member.
    let mut standard = Db::new();
    let _ambient = standard.add_file(ds_stdlib::AMBIENT_FILE, ds_stdlib::AMBIENT_SOURCE);
    let member = standard.add_file("member.ds", MEMBER);
    let _ = standard.get::<LinearityCheck>(first_block(member)).unwrap();
    assert!(ds_stdlib::ambient_linearity().get().is_some());

    // A package led by a member file (the ambient second): the parked
    // rows address the filling registry's file 0 — this package's own
    // first member — so installing them would misattribute the ambient's
    // spans; the walk must stay plain and still answer its rows.
    let mut db = Db::new();
    let first = db.add_file("first.ds", MEMBER);
    let _ambient = db.add_file(ds_stdlib::AMBIENT_FILE, ds_stdlib::AMBIENT_SOURCE);
    let out = db.get::<LinearityCheck>(first_block(first)).unwrap();
    assert!(out.diags.is_empty());
    rows_answer_the_plain_walk(&mut db, "the led-by-a-member walk");
}

/// A package without the ambient at all keeps the plain walk: the park
/// never fills over a boundary it did not walk.
#[test]
fn a_package_without_the_ambient_keeps_the_plain_walk() {
    let mut db = Db::new();
    let file = db.add_file(
        "t.ds",
        concat!(
            "pub trait Copyable { }\n",
            "pub struct handle: ~Copyable {\n",
            "  fd: int\n",
            "}\n",
            "pub extern fn open_handle (p: string) -> handle\n",
            "pub extern fn take_handle (move h: handle) -> int\n",
            "##\n",
            "pub fn good(p: string) -> int {\n",
            "  const h = open_handle(p)\n",
            "  take_handle(h)\n",
            "}\n",
        ),
    );
    // The fn is the second block; the declarations answer no facts.
    let decls = db.get::<LinearityCheck>(first_block(file)).unwrap();
    assert!(decls.moves.is_empty());
    let out = db
        .get::<LinearityCheck>(DefId::from_block(BlockId { file, index: 1 }).unwrap())
        .unwrap();
    assert!(out.diags.is_empty());
    assert_eq!(out.moves.len(), 1);
    rows_answer_the_plain_walk(&mut db, "the ambient-free walk");
}
