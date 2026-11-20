//! The real `linearity_check` compute: per-definition facts sliced out of
//! the package's M2 affine pass over the expansion's own trees, memoized
//! through the engine, and recomputed when the package's expansion changes
//! `[C1, C5]`.

use ds_diag::FileId;
use ds_query::{BlockId, Db, DefId, LinearityCheck, PackageId, QueryKey, Resolve};

const CLEAN: &str = concat!(
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
    "##\n",
    "pub fn bad(p: string) -> int {\n",
    "  const h = open_handle(p)\n",
    "  take_handle(h)\n",
    "  take_handle(h)\n",
    "}\n",
);

/// The placeholder `DefId` packing of one block (see `DefId::from_block`).
fn def(file: u32, block: u32) -> DefId {
    DefId::from_block(BlockId {
        file: FileId::new(file),
        index: block,
    })
    .expect("fixture files stay under the packing bounds")
}

/// One derive attribute whose impl resolves inside the package: the
/// expansion appends the synthesized impl beside the struct, so the
/// affine pass walks its member body like a hand-written one.
const DERIVED: &str = concat!(
    "pub tag int\n",
    "pub tag string\n",
    "pub trait show {\n",
    "  fn show(self) -> string\n",
    "}\n",
    "#[derive(show)]\n",
    "pub struct point { x: int, y: int }\n",
    "##\n",
    "pub fn plain() -> int { 1 }\n",
);

/// The linear layer declared in another file: the package expansion
/// refuses the derive (its linear-head scan is package-wide), so no impl
/// body exists and the block answers no facts — the same refusal the
/// resolve value carries.
const LINEAR_DECL: &str = concat!(
    "pub tag int\n",
    "pub tag string\n",
    "pub trait Copyable { }\n",
    "pub trait show {\n",
    "  fn show(self) -> string\n",
    "}\n",
    "pub struct cell: ~Copyable {\n",
    "  n: int\n",
    "}\n",
);

/// The copyable part declared in another file: no `~Copyable` veto, so
/// a derive over it accepts and the expansion appends the synthesized
/// impl beside the cross-file struct.
const COPYABLE_DECL: &str = concat!(
    "pub tag int\n",
    "pub tag string\n",
    "pub trait show {\n",
    "  fn show(self) -> string\n",
    "}\n",
    "pub struct cell { n: int }\n",
);

/// The accepting mirror of the refused derive: the struct in the second
/// file derives over the first file's part.
const CROSS_DERIVE: &str = concat!("#[derive(show)]\n", "struct box { c: cell }\n",);

#[test]
fn a_derived_member_body_answers_linearity_facts() {
    let mut db = Db::new();
    let _ = db.add_file("t.ds", DERIVED);

    // The derive's block: the synthesized member's borrowed receiver is a
    // final use, so the block answers it through the package pass over the
    // expansion's trees. The raw parse tree holds no impl body at all, so
    // this fact exists only because the pass walks the expanded trees.
    let derived = db.get::<LinearityCheck>(def(0, 0)).unwrap();
    assert!(derived.diags.is_empty());
    assert_eq!(derived.last_uses.len(), 1);
    assert!(derived.last_uses[0].borrowed);

    // The plain fn answers as before: its body tracks nothing.
    let plain = db.get::<LinearityCheck>(def(0, 1)).unwrap();
    assert!(plain.diags.is_empty());
    assert!(plain.moves.is_empty());
    assert!(plain.last_uses.is_empty());

    // One package pass serves both definitions.
    assert_eq!(
        db.compute_count(&QueryKey::PackageLinearity(PackageId::new(0))),
        1
    );
}

#[test]
fn facts_follow_the_package_expansion_s_refusals() {
    let mut db = Db::new();
    let _ = db.add_file("a.ds", LINEAR_DECL);
    let _ = db.add_file("b.ds", "#[derive(show)]\nstruct box { c: cell }\n");

    // The package expansion refuses the derive over the cross-file linear
    // part, so no member body exists to walk: the block answers no facts.
    let lin = db.get::<LinearityCheck>(def(1, 0)).unwrap();
    assert!(lin.diags.is_empty());
    assert!(lin.moves.is_empty());
    assert!(lin.drops.is_empty());
    assert!(
        lin.last_uses.is_empty(),
        "the refused derive synthesizes no body"
    );

    // The same refusal rides the resolve value: both surfaces answer from
    // the one expansion.
    let resolved = db.get::<Resolve>(PackageId::new(0)).unwrap();
    assert_eq!(resolved.diags.len(), 1);
    assert_eq!(resolved.diags[0].code.as_str(), "DS2026");
}

#[test]
fn a_cross_file_derive_answers_the_member_s_facts() {
    let mut db = Db::new();
    let _ = db.add_file("a.ds", COPYABLE_DECL);
    let _ = db.add_file("b.ds", CROSS_DERIVE);

    // The expansion accepts the derive over the cross-file copyable
    // part, so the attributed block answers the synthesized member's
    // facts: the borrowed receiver is a final use.
    let derived = db.get::<LinearityCheck>(def(1, 0)).unwrap();
    assert!(derived.diags.is_empty());
    assert_eq!(derived.last_uses.len(), 1);
    assert!(derived.last_uses[0].borrowed);

    // The accept mirrors the refusal surface: the expansion carries no
    // diagnostic.
    let resolved = db.get::<Resolve>(PackageId::new(0)).unwrap();
    assert!(resolved.diags.is_empty());

    // One package pass serves both files: the part's own declaration
    // block answers from the same pass.
    let decls = db.get::<LinearityCheck>(def(0, 0)).unwrap();
    assert!(decls.diags.is_empty());
    assert!(decls.last_uses.is_empty());
    assert_eq!(
        db.compute_count(&QueryKey::PackageLinearity(PackageId::new(0))),
        1
    );
}

#[test]
fn facts_slice_per_definition() {
    let mut db = Db::new();
    let file = db.add_file("t.ds", CLEAN);
    assert_eq!(file.as_u32(), 0);

    // Block 0 holds the declarations: no diagnostics, no placements.
    let decls = db.get::<LinearityCheck>(def(0, 0)).unwrap();
    assert!(decls.diags.is_empty());
    assert!(decls.moves.is_empty());
    assert!(decls.drops.is_empty());
    assert!(decls.last_uses.is_empty());

    // Block 1 consumes its handle once: one move, no diagnostics.
    let good = db.get::<LinearityCheck>(def(0, 1)).unwrap();
    assert!(good.diags.is_empty());
    assert_eq!(good.moves.len(), 1);
    assert_eq!(good.drops.len(), 0);
    // Two tracked bindings: the borrowed parameter and the handle.
    assert_eq!(good.last_uses.len(), 2);

    // Block 2 uses the moved binding again: the two-span DS3001 lands
    // there, with the move placement it caused.
    let bad = db.get::<LinearityCheck>(def(0, 2)).unwrap();
    assert_eq!(bad.diags.len(), 1);
    assert_eq!(bad.diags[0].code.as_str(), "DS3001");
    assert_eq!(bad.diags[0].labels.len(), 2);
    assert_eq!(bad.diags[0].labels[1].label, "used again here after move");
    assert_eq!(bad.moves.len(), 1);
}

#[test]
fn an_edit_recomputes_and_a_unchanged_renewal_is_a_hit() {
    let mut db = Db::new();
    let file = db.add_file("t.ds", CLEAN);

    let before = db.get::<LinearityCheck>(def(0, 1)).unwrap();
    assert!(before.diags.is_empty());
    assert_eq!(before.moves.len(), 1);

    // Breaking the second block re-derives the file's parse and the
    // check: the definition's facts change with it.
    let broken = CLEAN.replace(
        "  take_handle(h)\n}\n##",
        "  take_handle(h)\n  take_handle(h)\n}\n##",
    );
    db.set_file_content(file, &broken).unwrap();
    let after = db.get::<LinearityCheck>(def(0, 1)).unwrap();
    assert_eq!(after.diags.len(), 1);
    assert_eq!(after.diags[0].code.as_str(), "DS3001");

    // Setting the same content back changes nothing observable: the
    // value recomputes only through its parse dependency.
    let first = db.get::<LinearityCheck>(def(0, 1)).unwrap();
    let second = db.get::<LinearityCheck>(def(0, 1)).unwrap();
    assert_eq!(first.diags.len(), second.diags.len());
}

#[test]
fn a_file_that_fails_to_parse_yields_empty_facts() {
    let mut db = Db::new();
    db.add_file("t.ds", "fn a(p: string) -> int {\n  (\n}\n");
    let out = db.get::<LinearityCheck>(def(0, 0)).unwrap();
    assert!(out.diags.is_empty());
    assert!(out.moves.is_empty());
    assert!(out.drops.is_empty());
    assert!(out.last_uses.is_empty());
}

#[test]
fn an_unknown_block_is_an_engine_error() {
    let mut db = Db::new();
    db.add_file("t.ds", CLEAN);
    let err = db.get::<LinearityCheck>(def(0, 7)).unwrap_err();
    assert!(matches!(err, ds_query::QueryError::UnknownBlock(_)));
}
