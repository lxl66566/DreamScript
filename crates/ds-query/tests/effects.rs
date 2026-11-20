//! The effects-of query `[E1]`: the ds-check compute registered by
//! [`Db::new`] over the real parse — memoization, package-grain
//! invalidation, graceful degradation on broken files, the out-of-range
//! id error, and the declared-rows table the database carries.

use ds_check::{DeclaredRows, EffectRow};
use ds_query::{Db, DefId, Effect, EffectsOf, QueryError, QueryKey};

#[test]
fn rows_compute_over_registered_files() {
    let mut db = Db::new();
    let a = db.add_file("a.ds", "fn f(x: int) -> int { x }\n");
    db.add_file("b.ds", "fn g(x: int) -> int { f(x) + 1 }\n");
    let _ = a;

    // DefIds are package indices: declaration order across the files.
    let f_row = db.get::<EffectsOf>(DefId::new(0)).expect("f's row");
    assert!(f_row.is_pure());
    let g_row = db.get::<EffectsOf>(DefId::new(1)).expect("g's row");
    assert!(g_row.contains(Effect::Trap));

    // memoized across repeat reads.
    let key = QueryKey::EffectsOf(DefId::new(0));
    assert_eq!(db.compute_count(&key), 1);
    let _ = db.get::<EffectsOf>(DefId::new(0)).unwrap();
    assert_eq!(db.compute_count(&key), 1);
}

#[test]
fn edits_recompute_the_row() {
    let mut db = Db::new();
    let a = db.add_file("a.ds", "fn f(x: int) -> int { x }\n");
    db.add_file("b.ds", "fn g(x: int) -> int { f(x) }\n");
    assert!(db.get::<EffectsOf>(DefId::new(1)).unwrap().is_pure());

    // package grain [M2]: an edit of the first file re-derives the second
    // file's definition's row through the callee edge.
    db.set_file_content(a, "fn f(x: int) -> int { x + 1 }\n")
        .unwrap();
    let row = db.get::<EffectsOf>(DefId::new(1)).unwrap();
    assert!(row.contains(Effect::Trap));
    assert_eq!(db.compute_count(&QueryKey::EffectsOf(DefId::new(1))), 2);
}

#[test]
fn propagation_contributes_error_through_the_query() {
    let mut db = Db::new();
    db.add_file(
        "a.ds",
        concat!(
            "extern fn read (x: int) -> int | io_err\n",
            "fn f(x: int) -> int { read(x)? }\n",
        ),
    );
    let row = db.get::<EffectsOf>(DefId::new(1)).expect("f's row");
    assert!(row.contains(Effect::Error));
    assert_eq!(row.render(), "error");
}

/// The database's declared-row table `[E1]` is a lower bound on the computed
/// row: a world-touching ambient name carries its row into every caller.
#[test]
fn declared_rows_flow_through_the_database() {
    let rows = DeclaredRows::new().with(
        "read",
        EffectRow::of(Effect::World) | EffectRow::of(Effect::Error),
    );
    let mut db = Db::with_declared_rows(rows);
    db.add_file(
        "a.ds",
        concat!(
            "extern fn read (x: int) -> int\n",
            "fn g(x: int) -> int { read(x) }\n",
        ),
    );
    let row = db.get::<EffectsOf>(DefId::new(1)).expect("g's row");
    assert_eq!(row.render(), "error | world");

    // The default database declares nothing: every ambient name is pure.
    let mut plain = Db::new();
    plain.add_file(
        "a.ds",
        concat!(
            "extern fn read (x: int) -> int\n",
            "fn g(x: int) -> int { read(x) }\n",
        ),
    );
    assert!(plain.get::<EffectsOf>(DefId::new(1)).unwrap().is_pure());
}

#[test]
fn unknown_definition_ids_error() {
    let mut db = Db::new();
    db.add_file("a.ds", "fn f(x: int) -> int { x }\n");
    assert_eq!(
        db.get::<EffectsOf>(DefId::new(9)),
        Err(QueryError::NoEffectRow(DefId::new(9)))
    );
}

#[test]
fn type_level_definitions_carry_no_row() {
    let mut db = Db::new();
    db.add_file("a.ds", "tag io_err\nfn g(x: int) -> int { x + 1 }\n");
    assert_eq!(
        db.get::<EffectsOf>(DefId::new(0)),
        Err(QueryError::NoEffectRow(DefId::new(0)))
    );
    assert!(
        db.get::<EffectsOf>(DefId::new(1))
            .unwrap()
            .contains(Effect::Trap)
    );
}

#[test]
fn broken_files_degrade_to_the_rest() {
    let mut db = Db::new();
    db.add_file("broken.ds", "fn f(x: int) -> {\n");
    db.add_file("good.ds", "fn g(x: int) -> int { x + 1 }\n");

    // the broken file's parse carries no tree: it contributes no
    // definitions, and the good file's definition takes index 0.
    let row = db.get::<EffectsOf>(DefId::new(0)).expect("g's row");
    assert!(row.contains(Effect::Trap));
}
