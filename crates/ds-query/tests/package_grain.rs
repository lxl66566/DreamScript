//! The package-grain memos: one whole-package pass serves every
//! definition, so the per-definition queries are slices of it rather than
//! whole-package passes of their own.

use std::sync::Arc;

use ds_diag::FileId;
use ds_query::{
    BlockId, BlockIndex, CheckedPackage, Db, DefId, EffectsOf, LinearityCheck, PackageEffects,
    PackageId, PlatformCheck, QueryError, QueryKey, TypeOf,
};

/// A file with declarations and one consuming definition: the affine pass
/// and the bidirectional check both have facts here.
const A: &str = concat!(
    "trait Copyable { }\n",
    "struct handle: ~Copyable {\n  fd: int\n}\n",
    "extern fn open_handle (p: string) -> handle\n",
    "extern fn take_handle (move h: handle) -> int\n",
    "##\n",
    "fn good(p: string) -> int {\n  const h = open_handle(p)\n  take_handle(h)\n}\n",
);

/// A second file: a call into the first file's definition plus a binding,
/// so the package-grain values carry facts from both files.
const B: &str = concat!(
    "fn caller(p: string) -> int {\n  good(p)\n}\n",
    "##\n",
    "const answer = 42\n",
);

/// The placeholder `DefId` packing of one block (see `DefId::from_block`).
fn def(file: u32, block: u32) -> DefId {
    DefId::from_block(BlockId {
        file: FileId::new(file),
        index: block,
    })
    .expect("fixture files stay under the packing bounds")
}

fn package() -> QueryKey {
    QueryKey::CheckedPackage(PackageId::new(0))
}

fn effects() -> QueryKey {
    QueryKey::PackageEffects(PackageId::new(0))
}

fn linearity() -> QueryKey {
    QueryKey::PackageLinearity(PackageId::new(0))
}

/// Reads every per-definition query over the whole package.
fn read_every_definition(db: &mut Db, blocks: &[(FileId, u32)], def_count: usize) {
    for &(file, block) in blocks {
        let id = def(file.as_u32(), block);
        let _ = db.get::<TypeOf>(id).unwrap();
        let _ = db.get::<LinearityCheck>(id).unwrap();
    }
    for raw in 0..def_count {
        let raw = u32::try_from(raw).expect("def count fits u32");
        match db.get::<EffectsOf>(DefId::new(raw)) {
            Ok(_) | Err(QueryError::NoEffectRow(_)) => {},
            Err(err) => panic!("effect row of definition {raw}: {err}"),
        }
    }
    let _ = db.get::<PlatformCheck>(PackageId::new(0)).unwrap();
}

#[test]
fn one_package_pass_serves_every_definition() {
    let mut db = Db::new();
    let a = db.add_file("a.ds", A);
    let b = db.add_file("b.ds", B);
    let index_a = db.get::<BlockIndex>(a).unwrap();
    let index_b = db.get::<BlockIndex>(b).unwrap();
    let blocks: Vec<(FileId, u32)> = index_a
        .blocks
        .iter()
        .chain(index_b.blocks.iter())
        .map(|info| (info.id.file, info.id.index))
        .collect();

    let checked = db.get::<CheckedPackage>(PackageId::new(0)).unwrap();
    let def_count = checked.package.len();
    read_every_definition(&mut db, &blocks, def_count);

    // One pass per package, however many definitions or files it holds.
    assert_eq!(db.compute_count(&package()), 1);
    assert_eq!(db.compute_count(&effects()), 1);
    assert_eq!(db.compute_count(&linearity()), 1);
    assert_eq!(db.compute_count(&QueryKey::Resolve(PackageId::new(0))), 1);
    for &(file, block) in &blocks {
        let id = def(file.as_u32(), block);
        assert_eq!(db.compute_count(&QueryKey::TypeOf(id)), 1);
        assert_eq!(db.compute_count(&QueryKey::LinearityCheck(id)), 1);
    }

    // The slices answer the package's own facts: file a's block 1 consumes
    // its handle once, and the package value carries one row per
    // value-producing definition (the two externs and `good` in a, the
    // call and the binding in b).
    assert_eq!(db.get::<LinearityCheck>(def(0, 1)).unwrap().moves.len(), 1);
    assert_eq!(
        db.get::<PackageEffects>(PackageId::new(0))
            .unwrap()
            .effects
            .defs()
            .len(),
        5,
    );
}

#[test]
fn memo_hits_share_their_payloads() {
    // A slice value carries arcs, so answering a read from the memo clones
    // pointers instead of the facts: the second read observes the same
    // allocations.
    let mut db = Db::new();
    let a = db.add_file("a.ds", A);
    let _ = db.add_file("b.ds", B);
    let _ = db.get::<BlockIndex>(a).unwrap();

    let first = db.get::<TypeOf>(def(0, 1)).unwrap();
    let second = db.get::<TypeOf>(def(0, 1)).unwrap();
    assert!(Arc::ptr_eq(&first.diags, &second.diags));
    assert!(Arc::ptr_eq(&first.types, &second.types));

    let first = db.get::<LinearityCheck>(def(0, 1)).unwrap();
    let second = db.get::<LinearityCheck>(def(0, 1)).unwrap();
    assert!(Arc::ptr_eq(&first.diags, &second.diags));
    assert!(Arc::ptr_eq(&first.moves, &second.moves));
    assert!(Arc::ptr_eq(&first.drops, &second.drops));
    assert!(Arc::ptr_eq(&first.last_uses, &second.last_uses));

    let first = db.get::<CheckedPackage>(PackageId::new(0)).unwrap();
    let second = db.get::<CheckedPackage>(PackageId::new(0)).unwrap();
    assert!(Arc::ptr_eq(&first.package, &second.package));
    assert!(Arc::ptr_eq(&first.diags, &second.diags));
    assert!(Arc::ptr_eq(&first.type_names, &second.type_names));

    let first = db.get::<BlockIndex>(a).unwrap();
    let second = db.get::<BlockIndex>(a).unwrap();
    assert!(Arc::ptr_eq(&first.blocks, &second.blocks));
}

#[test]
fn an_edit_recomputes_each_package_pass_once() {
    let mut db = Db::new();
    let a = db.add_file("a.ds", A);
    let b = db.add_file("b.ds", B);
    let index_a = db.get::<BlockIndex>(a).unwrap();
    let index_b = db.get::<BlockIndex>(b).unwrap();
    let blocks: Vec<(FileId, u32)> = index_a
        .blocks
        .iter()
        .chain(index_b.blocks.iter())
        .map(|info| (info.id.file, info.id.index))
        .collect();
    let def_count = db
        .get::<CheckedPackage>(PackageId::new(0))
        .unwrap()
        .package
        .len();
    read_every_definition(&mut db, &blocks, def_count);

    // Break file a's consuming body: the package's diagnostics, rows, and
    // affine facts all move, so each of the three passes runs a second
    // time — once, not once per definition.
    db.set_file_content(a, &A.replace("take_handle(h)\n}", "take_handle(h)\n  h\n}"))
        .unwrap();
    read_every_definition(&mut db, &blocks, def_count);

    assert_eq!(db.compute_count(&package()), 2);
    assert_eq!(db.compute_count(&effects()), 2);
    // The affine pass re-derives once for the whole package, like the
    // other two: its grain is the expansion, not the edited file.
    assert_eq!(db.compute_count(&linearity()), 2);
    // The resolve value re-derives (the edited block is one of its parse
    // dependencies) and renews: the edited block declares the same
    // definition it did before, so nothing above it moves.
    assert_eq!(db.compute_count(&QueryKey::Resolve(PackageId::new(0))), 2);
}
