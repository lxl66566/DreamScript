//! The real `resolve` and `type_of` computes: package-level resolution
//! and the per-definition typed story, memoized through the engine and
//! recomputed when the file's parse changes [B2, G3].

use ds_query::{Db, DefId, PackageId, Resolve, TypeOf};

/// A package with one duplicate definition: block 0 declares `size`,
/// block 2 declares it again (collection reports the duplicate, keeping
/// the first), and block 1 declares a distinct binding.
const PKG: &str = concat!(
    "tag int\n",
    "///|\n",
    "const size = (s: int) -> int { 1 }\n",
    "///|\n",
    "const other = 2\n",
    "///|\n",
    "const size = (s: int) -> int { 3 }\n",
);

/// `(file << 8) | block`: the placeholder `DefId` packing.
fn def(file: u32, block: u32) -> DefId {
    DefId::new((file << 8) | block)
}

#[test]
fn resolve_reports_duplicates_and_slices_per_block() {
    let mut db = Db::new();
    let file = db.add_file("t.ds", PKG);
    assert_eq!(file.as_u32(), 0);

    // The resolve query carries the whole package's collection and
    // resolution diagnostics [G3].
    let resolved = db.get::<Resolve>(PackageId::new(0)).unwrap();
    assert_eq!(resolved.diags.len(), 1);
    assert_eq!(resolved.diags[0].message, "`size` is already defined");
    // The duplicate keeps the first definition's identity: int, size, other.
    assert_eq!(resolved.package.len(), 3);

    // Block 1 holds `size`: its typed story is the rendered function
    // signature, no diagnostics.
    let first = db.get::<TypeOf>(def(0, 1)).unwrap();
    assert!(first.diags.is_empty());
    assert_eq!(first.types.len(), 1);
    assert_eq!(first.types[0].0, "size");
    assert_eq!(first.types[0].1, "fn(int) -> int");

    // Block 2 holds `other`: a plain int binding.
    let other = db.get::<TypeOf>(def(0, 2)).unwrap();
    assert!(other.diags.is_empty());
    assert_eq!(other.types.len(), 1);
    assert_eq!(other.types[0].0, "other");
    assert_eq!(other.types[0].1, "int");

    // Block 3 redeclares `size`: collection keeps the first definition's
    // identity (span in block 1), so the block holds no binding of its
    // own, and the duplicate diagnostic is collection data — it lives in
    // the resolve value, not the typed half.
    let dup = db.get::<TypeOf>(def(0, 3)).unwrap();
    assert!(dup.diags.is_empty());
    assert!(dup.types.is_empty());
}

#[test]
fn an_edit_recomputes_the_changed_story_and_renews_the_rest() {
    let mut db = Db::new();
    let file = db.add_file("t.ds", PKG);

    let clean = db.get::<TypeOf>(def(0, 1)).unwrap();
    assert!(clean.diags.is_empty());

    // Changing block 1's annotation makes its body ill-typed: the story
    // of the changed block gains the diagnostic, while the untouched
    // block 2 renews (its value recomputes to an equal one and its
    // change-revision stays put).
    let broken = PKG.replace(
        "const size = (s: int) -> int { 1 }",
        "const size = (s: int) -> int { \"x\" }",
    );
    db.set_file_content(file, &broken).unwrap();

    let changed = db.get::<TypeOf>(def(0, 1)).unwrap();
    assert_eq!(changed.diags.len(), 1, "the body no longer fits `int`");

    let renewed = db.get::<TypeOf>(def(0, 2)).unwrap();
    assert!(renewed.diags.is_empty());
    assert_eq!(renewed.types.len(), 1);
    assert_eq!(renewed.types[0].0, "other");
    assert_eq!(renewed.types[0].1, "int");
}
