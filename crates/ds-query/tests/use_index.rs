//! The reverse edge in the resolve value: the def-to-uses index built in
//! the resolve compute itself, package grain like the resolutions it
//! derives from, so a reference request is one lookup instead of a scan
//! of the package.

use ds_query::{Db, PackageId, Resolve};

#[test]
fn the_resolve_value_carries_the_reverse_edge() {
    let mut db = Db::new();
    let a = db.add_file("a.ds", "const base = 1\n");
    let b = db.add_file("b.ds", "const copy = base\n");
    let resolved = db.get::<Resolve>(PackageId::new(0)).unwrap();
    let base = resolved.package.lookup_value("base").unwrap();
    let uses = resolved.uses.def_uses(base);
    // b's use of a's definition answers: the index spans files.
    assert_eq!(uses.len(), 1, "{uses:?}");
    assert_eq!(uses[0].file, b);
    assert_ne!(uses[0].file, a);
    // A definition nobody uses answers an empty set.
    let copy = resolved.package.lookup_value("copy").unwrap();
    assert_eq!(resolved.uses.def_uses(copy), []);
}

#[test]
fn the_index_rides_the_resolve_renewal() {
    let mut db = Db::new();
    let a = db.add_file("a.ds", "const base = 1\n");
    let _ = db.add_file("b.ds", "const copy = base\n");
    let resolved = db.get::<Resolve>(PackageId::new(0)).unwrap();
    let base = resolved.package.lookup_value("base").unwrap();
    assert_eq!(resolved.uses.def_uses(base).len(), 1);
    // An edit that adds a use renews the index through the resolve value:
    // the new use answers without any second index being keyed.
    db.set_file_content(a, "const base = 1\nconst inner = base\n")
        .unwrap();
    let resolved = db.get::<Resolve>(PackageId::new(0)).unwrap();
    assert_eq!(resolved.uses.def_uses(base).len(), 2);
}
