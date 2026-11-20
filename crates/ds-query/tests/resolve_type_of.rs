//! The real `resolve` and `type_of` computes: package-level resolution
//! and the per-definition typed story, memoized through the engine and
//! recomputed when the file's parse changes `[B2, G3]`.

use std::fmt::Write;

use ds_diag::FileId;
use ds_query::{BlockId, BlockIndex, Db, DefId, PackageId, QueryError, QueryKey, Resolve, TypeOf};

/// A package with one duplicate definition: block 0 declares `size`,
/// block 2 declares it again (collection reports the duplicate, keeping
/// the first), and block 1 declares a distinct binding.
const PKG: &str = concat!(
    "tag int\n",
    "##\n",
    "fn size(s: int) -> int { 1 }\n",
    "##\n",
    "const other = 2\n",
    "##\n",
    "fn size(s: int) -> int { 3 }\n",
);

/// The placeholder `DefId` packing of one block (see `DefId::from_block`).
fn def(file: u32, block: u32) -> DefId {
    DefId::from_block(BlockId {
        file: FileId::new(file),
        index: block,
    })
    .expect("fixture files stay under the packing bounds")
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
        "fn size(s: int) -> int { 1 }",
        "fn size(s: int) -> int { \"x\" }",
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

#[test]
fn a_block_holding_several_bindings_slices_all_of_them() {
    // One block, two top-level bindings plus a type declaration: the
    // slice answers every fact the block holds and nothing from the
    // neighbouring block.
    const MULTI: &str = concat!(
        "tag int
",
        "const first = 1
",
        "const second = 2
",
        "##
",
        "const third = 3
",
    );
    let mut db = Db::new();
    db.add_file("t.ds", MULTI);

    let first = db.get::<TypeOf>(def(0, 0)).unwrap();
    assert!(first.diags.is_empty());
    let names: Vec<&str> = first.types.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(names, vec!["first", "second"]);

    let second = db.get::<TypeOf>(def(0, 1)).unwrap();
    assert_eq!(second.types.len(), 1);
    assert_eq!(second.types[0].0, "third");
}

#[test]
fn registering_a_file_invalidates_the_package_queries() {
    let mut db = Db::new();
    let first = db.add_file("first.ds", "tag int\nfn size(s: int) -> int { 1 }\n");
    assert_eq!(first.as_u32(), 0);

    // resolve and one typed story compute over the one-file package
    // (the `tag int` declaration counts: int, size).
    let resolved = db.get::<Resolve>(PackageId::new(0)).unwrap();
    assert_eq!(resolved.package.len(), 2);
    let story = db.get::<TypeOf>(def(0, 0)).unwrap();
    assert_eq!(story.types.len(), 1);
    assert_eq!(story.types[0].0, "size");

    // A file registered mid-session changes the file-set input: the
    // package memos go dirty and the next reads see the new declaration
    // (previously the stale memos hid it, with no diagnostic).
    let second = db.add_file("second.ds", "const later = 2\n");
    assert_eq!(second.as_u32(), 1);
    assert!(!db.is_fresh(&QueryKey::Resolve(PackageId::new(0))));
    let resolved = db.get::<Resolve>(PackageId::new(0)).unwrap();
    assert_eq!(resolved.package.len(), 3);
    let later = db.get::<TypeOf>(def(1, 0)).unwrap();
    assert!(later.diags.is_empty());
    assert_eq!(later.types.len(), 1);
    assert_eq!(later.types[0].0, "later");
    assert_eq!(later.types[0].1, "int");
    assert_eq!(
        db.compute_count(&QueryKey::Resolve(PackageId::new(0))),
        2,
        "the stale memo recomputed exactly once"
    );
}

/// One derive attribute with its trait in the package: the expansion
/// synthesizes an ordinary impl item, so the derived member's call
/// checks at the resolve and type_of grain like a hand-written one.
const DERIVED_CALL: &str = concat!(
    "tag int\n",
    "tag string\n",
    "trait show {\n",
    "  fn show(self) -> string\n",
    "}\n",
    "##\n",
    "#[derive(show)] struct point { x: int, y: int }\n",
    "##\n",
    "fn render(p: point) -> string { p.show() }\n",
);

#[test]
fn a_derived_members_call_checks_at_the_check_grain() {
    let mut db = Db::new();
    db.add_file("derived.ds", DERIVED_CALL);

    // The resolve value runs over the expanded trees: the synthesized
    // impl is an ordinary item, so the member's call carries no
    // unknown-name or no-instance refusal.
    let resolved = db.get::<Resolve>(PackageId::new(0)).unwrap();
    assert!(resolved.diags.is_empty(), "{:?}", resolved.diags);

    // The call's block tells its typed story clean, like a hand-written
    // trait call would: the fn's solved signature, no diagnostics.
    let typed = db.get::<TypeOf>(def(0, 2)).unwrap();
    assert!(typed.diags.is_empty(), "{:?}", typed.diags);
    assert_eq!(typed.types.len(), 1);
    assert_eq!(typed.types[0].0, "render");
    assert_eq!(typed.types[0].1, "fn(point) -> string");
}

#[test]
fn a_derive_edit_invalidates_the_expansion_and_its_readers() {
    let mut db = Db::new();
    let file = db.add_file("derived.ds", DERIVED_CALL);
    assert!(
        db.get::<TypeOf>(def(0, 2)).unwrap().diags.is_empty(),
        "the derived call checks clean before the edit"
    );
    assert_eq!(db.compute_count(&QueryKey::Expanded(PackageId::new(0))), 1);

    // Stripping the derive attribute removes the synthesized impl with
    // it: the parse edit re-derives the expansion, resolution runs over
    // the attribute-stripped trees again, and the call's typed story
    // reports the missing instance.
    let stripped = DERIVED_CALL.replace("#[derive(show)] ", "");
    db.set_file_content(file, &stripped).unwrap();
    assert!(!db.is_fresh(&QueryKey::Expanded(PackageId::new(0))));
    let typed = db.get::<TypeOf>(def(0, 2)).unwrap();
    assert_eq!(
        typed.diags[0].code.as_str(),
        "DS2013",
        "the member's call refuses again over the raw trees: {:?}",
        typed.diags
    );
    assert_eq!(
        db.compute_count(&QueryKey::Expanded(PackageId::new(0))),
        2,
        "the derive edit re-derived the expansion exactly once"
    );
}

#[test]
fn blocks_past_the_packing_width_reject_instead_of_aliasing() {
    let mut db = Db::new();
    let first = db.add_file("first.ds", "tag int\nconst first = 1\n");
    assert_eq!(first.as_u32(), 0);

    // The second file holds 257 blocks: index 256 exists but cannot be
    // packed — `(1 << 8) | 256` would alias file 2's block 0.
    let mut many = String::from("const b0 = 0\n");
    for i in 1..=256_u32 {
        many.push_str("##\n");
        // writing into a String cannot fail
        let _ = writeln!(many, "const b{i} = {i}");
    }
    let second = db.add_file("many.ds", &many);
    let index = db.get::<BlockIndex>(second).unwrap();
    assert_eq!(index.blocks.len(), 257);

    // The highest fitting block addresses its own declaration...
    let story = db.get::<TypeOf>(def(1, 255)).unwrap();
    assert_eq!(story.types.len(), 1);
    assert_eq!(story.types[0].0, "b255");
    // ...and block 256 is a typed rejection, not the next file's block 0.
    assert_eq!(
        DefId::from_block(index.blocks[256].id),
        Err(QueryError::DefIdOverflow(index.blocks[256].id))
    );
}
