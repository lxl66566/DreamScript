//! Engine behavior: memoization, transitive invalidation, block-grain
//! renewal, revisions, cycle detection, and the extension seams.

use std::sync::Arc;

use ds_diag::FileId;
use ds_query::{
    BlockId, BlockIndex, BlockInfo, BlockSource, ComputeFn, Ctx, Db, DefId, ParseFile, ParseOutput,
    Query, QueryError, QueryKey, QueryValue,
};

/// Test marker for the placeholder TypeOf key: models an M2-style downstream
/// query keyed by block, reading that block's source. Also proves consumers
/// can implement `Query` for their own markers outside the crate.
struct TypeOf;

impl Query for TypeOf {
    type Key = DefId;
    type Value = Arc<str>;

    fn make_key(key: DefId) -> QueryKey {
        QueryKey::TypeOf(key)
    }

    fn extract(value: QueryValue) -> Option<Self::Value> {
        match value {
            QueryValue::BlockSource(text) => Some(text),
            _ => None,
        }
    }
}

/// Marker for the placeholder EffectsOf key, bound to the second file.
struct EffectsOf;

impl Query for EffectsOf {
    type Key = DefId;
    type Value = Arc<str>;

    fn make_key(key: DefId) -> QueryKey {
        QueryKey::EffectsOf(key)
    }

    fn extract(value: QueryValue) -> Option<Self::Value> {
        match value {
            QueryValue::BlockSource(text) => Some(text),
            _ => None,
        }
    }
}

/// Downstream compute for the first file: DefId(i) reads block i's source.
fn type_of_block(ctx: &mut Ctx<'_>, key: QueryKey) -> Result<QueryValue, QueryError> {
    let QueryKey::TypeOf(def) = key else {
        return Err(QueryError::KindMismatch(key));
    };
    let text = ctx.get::<BlockSource>(BlockId {
        file: FileId::new(0),
        index: def.as_u32(),
    })?;
    Ok(QueryValue::BlockSource(text))
}

/// Downstream compute for the second file: DefId(i) reads block i's source.
fn effects_of_block(ctx: &mut Ctx<'_>, key: QueryKey) -> Result<QueryValue, QueryError> {
    let QueryKey::EffectsOf(def) = key else {
        return Err(QueryError::KindMismatch(key));
    };
    let text = ctx.get::<BlockSource>(BlockId {
        file: FileId::new(1),
        index: def.as_u32(),
    })?;
    Ok(QueryValue::BlockSource(text))
}

fn db_with_downstream_queries() -> Db {
    let mut db = Db::new();
    db.set_compute(ds_query::QueryKind::TypeOf, type_of_block);
    db.set_compute(ds_query::QueryKind::EffectsOf, effects_of_block);
    db
}

fn parse_key(file: FileId) -> QueryKey {
    QueryKey::ParseFile(file)
}

fn index_key(file: FileId) -> QueryKey {
    QueryKey::BlockIndex(file)
}

fn source_key(file: FileId, block: u32) -> QueryKey {
    QueryKey::BlockSource(BlockId { file, index: block })
}

#[test]
fn parse_and_block_index_produce_expected_values() {
    let mut db = Db::new();
    let file = db.add_file("main.ds", "let a = 1\n///|\nlet b = 2\n");

    let parse = db.get::<ParseFile>(file).unwrap();
    assert_eq!(&*parse.source, "let a = 1\n///|\nlet b = 2\n");

    let index = db.get::<BlockIndex>(file).unwrap();
    assert_eq!(index.blocks.len(), 2);
    assert_eq!(index.blocks[0].span.start, 0);
    assert_eq!(index.blocks[0].span.end, 10);
    assert_eq!(
        &*db.get::<BlockSource>(index.blocks[0].id).unwrap(),
        "let a = 1\n"
    );
    assert_eq!(
        &*db.get::<BlockSource>(index.blocks[1].id).unwrap(),
        "let b = 2\n"
    );
}

#[test]
fn query_computed_once_regardless_of_reader_count() {
    let mut db = Db::new();
    let file = db.add_file("main.ds", "A\n///|\nB\n");

    // ParseFile has one direct reader and three indirect ones (BlockIndex
    // plus two BlockSource computes), plus repeated direct gets.
    let index = db.get::<BlockIndex>(file).unwrap();
    let _ = db.get::<BlockSource>(index.blocks[0].id).unwrap();
    let _ = db.get::<BlockSource>(index.blocks[1].id).unwrap();
    let _ = db.get::<ParseFile>(file).unwrap();
    let _ = db.get::<ParseFile>(file).unwrap();
    let _ = db.get::<BlockIndex>(file).unwrap();

    assert_eq!(db.compute_count(&parse_key(file)), 1);
    assert_eq!(db.compute_count(&index_key(file)), 1);
    assert_eq!(db.compute_count(&source_key(file, 0)), 1);
    assert_eq!(db.compute_count(&source_key(file, 1)), 1);
}

#[test]
fn transitive_invalidation_reaches_exactly_the_dependents() {
    let mut db = db_with_downstream_queries();
    let a = db.add_file("a.ds", "A0\n///|\nA1\n");
    let b = db.add_file("b.ds", "B0\n");

    // Populate every memo.
    let _ = db.get::<TypeOf>(DefId::new(0)).unwrap();
    let _ = db.get::<TypeOf>(DefId::new(1)).unwrap();
    let _ = db.get::<EffectsOf>(DefId::new(0)).unwrap();
    let _ = db.get::<ParseFile>(a).unwrap();
    let _ = db.get::<ParseFile>(b).unwrap();

    // Edit only block 1 of file a.
    db.set_file_content(a, "A0\n///|\nEDITED\n").unwrap();

    // Renewal-sensitive reader first: it drives the whole a-chain through
    // deep verification without recomputing itself.
    let text = db.get::<TypeOf>(DefId::new(0)).unwrap();
    assert_eq!(&*text, "A0\n");
    let _ = db.get::<TypeOf>(DefId::new(1)).unwrap();
    let _ = db
        .get::<BlockSource>(BlockId { file: a, index: 0 })
        .unwrap();
    let _ = db.get::<EffectsOf>(DefId::new(0)).unwrap();
    let _ = db.get::<ParseFile>(b).unwrap();

    // The a-chain re-derived; only block 1's downstream recomputed.
    assert_eq!(db.compute_count(&parse_key(a)), 2);
    assert_eq!(db.compute_count(&index_key(a)), 2);
    assert_eq!(db.compute_count(&source_key(a, 0)), 2); // recomputed, value renewed
    assert_eq!(db.compute_count(&source_key(a, 1)), 2);
    assert_eq!(db.compute_count(&QueryKey::TypeOf(DefId::new(0))), 1); // renewed, not recomputed
    assert_eq!(db.compute_count(&QueryKey::TypeOf(DefId::new(1))), 2);

    // File b is outside the dependency closure of the edit.
    assert_eq!(db.compute_count(&parse_key(b)), 1);
    assert_eq!(db.compute_count(&index_key(b)), 1);
    assert_eq!(db.compute_count(&source_key(b, 0)), 1);
    assert_eq!(db.compute_count(&QueryKey::EffectsOf(DefId::new(0))), 1);

    for key in [
        parse_key(a),
        index_key(a),
        source_key(a, 0),
        source_key(a, 1),
    ] {
        assert!(db.is_fresh(&key), "{key:?} should be fresh after the gets");
    }
}

#[test]
fn layout_shift_outside_blocks_renews_every_block() {
    // Indenting a separator line changes the file, the parse, and every
    // shifted span — but no block's text, so no downstream query recomputes.
    let mut db = db_with_downstream_queries();
    let file = db.add_file("main.ds", "A\n///|\nB\n");

    let _ = db.get::<TypeOf>(DefId::new(0)).unwrap();
    let _ = db.get::<TypeOf>(DefId::new(1)).unwrap();

    db.set_file_content(file, "A\n  ///|\nB\n").unwrap();

    let _ = db.get::<TypeOf>(DefId::new(0)).unwrap();
    let _ = db.get::<TypeOf>(DefId::new(1)).unwrap();

    assert_eq!(db.compute_count(&parse_key(file)), 2);
    assert_eq!(db.compute_count(&index_key(file)), 2); // spans shifted: value changed
    assert_eq!(db.compute_count(&source_key(file, 0)), 2); // recomputed, same text
    assert_eq!(db.compute_count(&source_key(file, 1)), 2);
    assert_eq!(db.compute_count(&QueryKey::TypeOf(DefId::new(0))), 1);
    assert_eq!(db.compute_count(&QueryKey::TypeOf(DefId::new(1))), 1);
}

#[test]
fn same_content_edit_is_a_noop() {
    let mut db = Db::new();
    let file = db.add_file("main.ds", "A\n///|\nB\n");
    let _ = db.get::<BlockIndex>(file).unwrap();

    db.set_file_content(file, "A\n///|\nB\n").unwrap();

    assert_eq!(db.revision().as_u64(), 0);
    let _ = db.get::<BlockIndex>(file).unwrap();
    assert_eq!(db.compute_count(&parse_key(file)), 1);
    assert_eq!(db.compute_count(&index_key(file)), 1);
}

#[test]
fn revision_is_monotonic() {
    let mut db = Db::new();
    let file = db.add_file("main.ds", "A\n");
    assert_eq!(db.revision().as_u64(), 0);

    db.set_file_content(file, "B\n").unwrap();
    assert_eq!(db.revision().as_u64(), 1);

    db.set_file_content(file, "B\n").unwrap();
    assert_eq!(db.revision().as_u64(), 1);

    db.set_file_content(file, "C\n").unwrap();
    assert!(db.revision().as_u64() > 1);

    let _ = db.get::<ParseFile>(file).unwrap();
    assert_eq!(db.revision().as_u64(), 2);
}

#[test]
fn edited_file_yields_new_values() {
    let mut db = Db::new();
    let file = db.add_file("main.ds", "A\n///|\nB\n");
    assert_eq!(
        &*db.get::<BlockSource>(BlockId { file, index: 0 }).unwrap(),
        "A\n"
    );

    db.set_file_content(file, "A2\n///|\nB\n").unwrap();

    assert_eq!(&*db.get::<ParseFile>(file).unwrap().source, "A2\n///|\nB\n");
    assert_eq!(
        &*db.get::<BlockSource>(BlockId { file, index: 0 }).unwrap(),
        "A2\n"
    );
    assert_eq!(
        &*db.get::<BlockSource>(BlockId { file, index: 1 }).unwrap(),
        "B\n"
    );
}

#[test]
fn self_cycle_detected_with_full_path() {
    fn self_reading(ctx: &mut Ctx<'_>, key: QueryKey) -> Result<QueryValue, QueryError> {
        let QueryKey::Lower(block) = key else {
            return Err(QueryError::KindMismatch(key));
        };
        let _ = ctx.get::<LowerMarker>(block)?;
        Ok(QueryValue::BlockSource(Arc::from("unreachable")))
    }
    struct LowerMarker;
    impl Query for LowerMarker {
        type Key = BlockId;
        type Value = Arc<str>;

        fn make_key(key: BlockId) -> QueryKey {
            QueryKey::Lower(key)
        }

        fn extract(value: QueryValue) -> Option<Self::Value> {
            match value {
                QueryValue::BlockSource(text) => Some(text),
                _ => None,
            }
        }
    }

    let mut db = Db::new();
    db.set_compute(ds_query::QueryKind::Lower, self_reading);
    let file = db.add_file("main.ds", "A\n");
    let block = BlockId { file, index: 0 };

    let err = db.get::<LowerMarker>(block).unwrap_err();
    assert_eq!(err, QueryError::Cycle {
        path: vec![QueryKey::Lower(block), QueryKey::Lower(block)],
    });
}

#[test]
fn mutual_cycle_detected_with_full_path() {
    fn parse_reading_index(ctx: &mut Ctx<'_>, key: QueryKey) -> Result<QueryValue, QueryError> {
        let QueryKey::ParseFile(file) = key else {
            return Err(QueryError::KindMismatch(key));
        };
        let _ = ctx.get::<BlockIndex>(file)?;
        Ok(QueryValue::BlockSource(Arc::from("unreachable")))
    }

    let mut db = Db::new();
    db.set_compute(ds_query::QueryKind::ParseFile, parse_reading_index);
    let file = db.add_file("main.ds", "A\n");

    let err = db.get::<ParseFile>(file).unwrap_err();
    assert_eq!(err, QueryError::Cycle {
        path: vec![
            QueryKey::ParseFile(file),
            QueryKey::BlockIndex(file),
            QueryKey::ParseFile(file),
        ],
    });
}

#[test]
fn cycle_recovery_after_fixing_the_compute() {
    fn parse_reading_index(ctx: &mut Ctx<'_>, key: QueryKey) -> Result<QueryValue, QueryError> {
        let QueryKey::ParseFile(file) = key else {
            return Err(QueryError::KindMismatch(key));
        };
        let _ = ctx.get::<BlockIndex>(file)?;
        Ok(QueryValue::BlockSource(Arc::from("unreachable")))
    }

    let mut db = Db::new();
    let file = db.add_file("main.ds", "A\n");
    db.set_compute(ds_query::QueryKind::ParseFile, parse_reading_index);
    assert!(matches!(
        db.get::<ParseFile>(file),
        Err(QueryError::Cycle { .. })
    ));

    // Errors are not memoized: swap in a sane compute and the same key works.
    let sane: ComputeFn = |ctx, key| {
        let QueryKey::ParseFile(file) = key else {
            return Err(QueryError::KindMismatch(key));
        };
        let source = ctx.file_content(file)?;
        Ok(QueryValue::ParseFile(ParseOutput { source }))
    };
    db.set_compute(ds_query::QueryKind::ParseFile, sane);
    assert_eq!(&*db.get::<ParseFile>(file).unwrap().source, "A\n");
}

#[test]
fn placeholder_query_without_compute_is_unavailable() {
    let mut db = Db::new();
    let err = db.get::<TypeOf>(DefId::new(0)).unwrap_err();
    assert_eq!(
        err,
        QueryError::Unavailable(QueryKey::TypeOf(DefId::new(0)))
    );
}

#[test]
fn unknown_inputs_report_errors_and_are_not_memoized() {
    let mut db = Db::new();
    let ghost = FileId::new(99);

    assert_eq!(
        db.set_file_content(ghost, "x"),
        Err(QueryError::UnknownFile(ghost))
    );
    assert_eq!(
        db.get::<ParseFile>(ghost),
        Err(QueryError::UnknownFile(ghost))
    );
    assert_eq!(
        db.get::<ParseFile>(ghost),
        Err(QueryError::UnknownFile(ghost))
    );

    let file = db.add_file("main.ds", "A\n///|\nB\n");
    let err = db
        .get::<BlockSource>(BlockId { file, index: 7 })
        .unwrap_err();
    assert_eq!(err, QueryError::UnknownBlock(BlockId { file, index: 7 }));
}

#[test]
fn same_inputs_same_results() {
    let mut a = Db::new();
    let mut b = Db::new();
    let source = "let a = 1\n///|\nlet b = 2\n";
    let fa = a.add_file("main.ds", source);
    let fb = b.add_file("main.ds", source);

    assert_eq!(
        a.get::<ParseFile>(fa).unwrap(),
        b.get::<ParseFile>(fb).unwrap()
    );
    assert_eq!(
        a.get::<BlockIndex>(fa).unwrap(),
        b.get::<BlockIndex>(fb).unwrap()
    );
    assert_eq!(
        a.get::<BlockSource>(BlockId { file: fa, index: 1 })
            .unwrap(),
        b.get::<BlockSource>(BlockId { file: fb, index: 1 })
            .unwrap()
    );

    // Memo state is invisible in results: a hit and a fresh compute of the
    // same inputs produce equal values.
    assert_eq!(
        a.get::<BlockIndex>(fa).unwrap(),
        b.get::<BlockIndex>(fb).unwrap()
    );
}

#[test]
fn key_and_value_enums_stay_extensible_for_consumers() {
    // From outside the crate: placeholder variants are constructible, and a
    // wildcard match keeps compiling when new variants land (non_exhaustive).
    let key = QueryKey::TypeOf(DefId::new(3));
    let described = match key {
        QueryKey::ParseFile(_) => "parse",
        QueryKey::TypeOf(_) => "type",
        _ => "other",
    };
    assert_eq!(described, "type");

    let value = QueryValue::BlockSource(Arc::from("x"));
    let described = match value {
        QueryValue::ParseFile(_) => "parse",
        QueryValue::BlockSource(_) => "source",
        _ => "other",
    };
    assert_eq!(described, "source");

    // BlockInfo remains constructible for span->block mappings by consumers.
    let info = BlockInfo {
        id: BlockId {
            file: FileId::new(0),
            index: 0,
        },
        span: ds_diag::Span::new(FileId::new(0), 0, 1),
    };
    assert_eq!(info.span.end, 1);
}
