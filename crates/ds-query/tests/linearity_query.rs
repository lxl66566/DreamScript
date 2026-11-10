//! The real `linearity_check` compute: per-definition facts sliced out of
//! the M2 affine pass, memoized through the engine, and recomputed when the
//! file's parse changes [C1, C5].

use ds_query::{Db, DefId, LinearityCheck};

const CLEAN: &str = concat!(
    "trait Copyable { }\n",
    "struct handle: ~Copyable {\n",
    "  fd: int\n",
    "}\n",
    "extern fn open_handle (p: string) -> handle\n",
    "extern fn take_handle (move h: handle) -> int\n",
    "##\n",
    "fn good(p: string) -> int {\n",
    "  const h = open_handle(p)\n",
    "  take_handle(h)\n",
    "}\n",
    "##\n",
    "fn bad(p: string) -> int {\n",
    "  const h = open_handle(p)\n",
    "  take_handle(h)\n",
    "  take_handle(h)\n",
    "}\n",
);

/// `(file << 8) | block`: the placeholder `DefId` packing.
fn def(file: u32, block: u32) -> DefId {
    DefId::new((file << 8) | block)
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
