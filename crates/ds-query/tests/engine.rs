//! Engine behavior over the real parse: memoization, transitive
//! invalidation, block-grain renewal, error files, and the extension seams.

use std::sync::Arc;

use ds_diag::{Diagnostic, ErrorCode, FileId};
use ds_query::{
    BlockId, BlockIndex, BlockInfo, BlockSource, ComputeFn, Ctx, Db, DefId, ParseFile, ParseOutput,
    Query, QueryError, QueryKey, QueryValue,
};

/// A clean two-block file: block 0 spans `const a = 1`, block 1 starts at
/// the separator line's newline (tree spans include it).
const TWO_BLOCKS: &str = "const a = 1\n##\nconst b = 2\n";
const BLOCK0: &str = "const a = 1";
const BLOCK1: &str = "\nconst b = 2";

/// Test marker for the real TypeOf key, overridden with a compute that
/// models an M2-style downstream query keyed by block, reading that
/// block's source. Also proves consumers can implement `Query` for their
/// own markers outside the crate and replace registered computes.
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
    let file = db.add_file("main.ds", TWO_BLOCKS);

    let parse = db.get::<ParseFile>(file).unwrap();
    assert!(parse.ast.is_some(), "clean source must parse");
    assert!(parse.diagnostics.is_empty());
    assert_eq!(&*parse.source, TWO_BLOCKS);

    let index = db.get::<BlockIndex>(file).unwrap();
    assert_eq!(index.blocks.len(), 2);
    assert_eq!(
        (index.blocks[0].span.start, index.blocks[0].span.end),
        (0, 11)
    );
    assert_eq!(&*db.get::<BlockSource>(index.blocks[0].id).unwrap(), BLOCK0);
    assert_eq!(&*db.get::<BlockSource>(index.blocks[1].id).unwrap(), BLOCK1);
}

#[test]
fn query_computed_once_regardless_of_reader_count() {
    let mut db = Db::new();
    let file = db.add_file("main.ds", TWO_BLOCKS);

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
    let a = db.add_file("a.ds", "const a0 = 0\n##\nconst a1 = 1\n");
    let b = db.add_file("b.ds", "const b0 = 0\n");

    // Populate every memo.
    let _ = db.get::<TypeOf>(DefId::new(0)).unwrap();
    let _ = db.get::<TypeOf>(DefId::new(1)).unwrap();
    let _ = db.get::<EffectsOf>(DefId::new(0)).unwrap();
    let _ = db.get::<ParseFile>(a).unwrap();
    let _ = db.get::<ParseFile>(b).unwrap();

    // Edit only block 1 of file a.
    db.set_file_content(a, "const a0 = 0\n##\nconst a1 = 99\n")
        .unwrap();

    // Renewal-sensitive reader first: it drives the whole a-chain through
    // deep verification without recomputing itself.
    let text = db.get::<TypeOf>(DefId::new(0)).unwrap();
    assert_eq!(&*text, "const a0 = 0");
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
fn separator_text_edit_renews_every_block() {
    // Editing a separator line's trailing text changes the file, the parse,
    // and every shifted span — but no block's text (whitespace before a
    // separator would belong to the preceding block, so edit only the text
    // after `##`), so no downstream query recomputes.
    let mut db = db_with_downstream_queries();
    let file = db.add_file("main.ds", TWO_BLOCKS);

    let _ = db.get::<TypeOf>(DefId::new(0)).unwrap();
    let _ = db.get::<TypeOf>(DefId::new(1)).unwrap();

    db.set_file_content(file, "const a = 1\n## section two\nconst b = 2\n")
        .unwrap();

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
    let file = db.add_file("main.ds", TWO_BLOCKS);
    let _ = db.get::<BlockIndex>(file).unwrap();

    db.set_file_content(file, TWO_BLOCKS).unwrap();

    assert_eq!(db.revision().as_u64(), 0);
    let _ = db.get::<BlockIndex>(file).unwrap();
    assert_eq!(db.compute_count(&parse_key(file)), 1);
    assert_eq!(db.compute_count(&index_key(file)), 1);
}

#[test]
fn revision_is_monotonic() {
    let mut db = Db::new();
    let file = db.add_file("main.ds", "const a = 1\n");
    assert_eq!(db.revision().as_u64(), 0);

    db.set_file_content(file, "const a = 2\n").unwrap();
    assert_eq!(db.revision().as_u64(), 1);

    db.set_file_content(file, "const a = 2\n").unwrap();
    assert_eq!(db.revision().as_u64(), 1);

    db.set_file_content(file, "const a = 3\n").unwrap();
    assert!(db.revision().as_u64() > 1);

    let _ = db.get::<ParseFile>(file).unwrap();
    assert_eq!(db.revision().as_u64(), 2);
}

#[test]
fn edited_file_yields_new_values() {
    let mut db = Db::new();
    let file = db.add_file("main.ds", TWO_BLOCKS);
    assert_eq!(
        &*db.get::<BlockSource>(BlockId { file, index: 0 }).unwrap(),
        BLOCK0
    );

    db.set_file_content(file, "const a2 = 2\n##\nconst b = 2\n")
        .unwrap();

    assert_eq!(
        &*db.get::<ParseFile>(file).unwrap().source,
        "const a2 = 2\n##\nconst b = 2\n"
    );
    assert_eq!(
        &*db.get::<BlockSource>(BlockId { file, index: 0 }).unwrap(),
        "const a2 = 2"
    );
    assert_eq!(
        &*db.get::<BlockSource>(BlockId { file, index: 1 }).unwrap(),
        BLOCK1
    );
}

#[test]
fn syntax_errors_are_data_not_engine_errors() {
    let mut db = Db::new();
    let file = db.add_file("bad.ds", "const a = 1\n##\nconst b = \"open\n");

    // The parse query still yields a value: diagnostics ride along.
    let parse = db.get::<ParseFile>(file).unwrap();
    assert!(parse.ast.is_none(), "broken source yields no tree");
    assert_eq!(parse.diagnostics.len(), 1);
    assert_eq!(parse.diagnostics[0].code, ErrorCode::UnterminatedString);

    // Dependents compute over the degraded (text-scan) block structure.
    let index = db.get::<BlockIndex>(file).unwrap();
    assert_eq!(index.blocks.len(), 2);
    assert_eq!(
        &*db.get::<BlockSource>(index.blocks[0].id).unwrap(),
        "const a = 1"
    );
    assert_eq!(
        &*db.get::<BlockSource>(index.blocks[1].id).unwrap(),
        "\nconst b = \"open\n"
    );
}

#[test]
fn broken_file_does_not_poison_other_blocks_and_healing_renews() {
    let mut db = db_with_downstream_queries();
    let broken = "const a = 1\n##\nconst b = \"open\n";
    let file = db.add_file("main.ds", broken);

    // Downstream of the intact block works over the scan fallback.
    let text = db.get::<TypeOf>(DefId::new(0)).unwrap();
    assert_eq!(&*text, "const a = 1");
    let _ = db.get::<TypeOf>(DefId::new(1)).unwrap();
    assert_eq!(db.compute_count(&QueryKey::TypeOf(DefId::new(0))), 1);

    // Heal block 1: the parse succeeds and the index switches to tree spans.
    db.set_file_content(file, "const a = 1\n##\nconst b = \"closed\"\n")
        .unwrap();
    let parse = db.get::<ParseFile>(file).unwrap();
    assert!(parse.ast.is_some());
    assert!(parse.diagnostics.is_empty());

    let _ = db.get::<TypeOf>(DefId::new(1)).unwrap();
    // Block 0's text is identical under scan and tree extents: its
    // downstream renewed, never recomputed. Block 1's did (new text and new
    // extents).
    assert_eq!(db.compute_count(&QueryKey::TypeOf(DefId::new(0))), 1);
    assert_eq!(db.compute_count(&QueryKey::TypeOf(DefId::new(1))), 2);
}

#[test]
fn conformance_file_parses_and_edits_block_grain() {
    // Relative to crates/ds-query/tests: three levels up is the repo root.
    const TYPES: &str = include_str!("../../../conformance/types_hybrid.ds");
    let mut db = db_with_downstream_queries();
    let file = db.add_file("types_hybrid.ds", TYPES);

    // The corpus file parses clean and is one block [G3: no separators].
    let parse = db.get::<ParseFile>(file).unwrap();
    assert!(parse.ast.is_some());
    assert!(parse.diagnostics.is_empty());
    let index = db.get::<BlockIndex>(file).unwrap();
    assert_eq!(index.blocks.len(), 1);
    let block0_text = db.get::<BlockSource>(index.blocks[0].id).unwrap();
    let _ = db.get::<TypeOf>(DefId::new(0)).unwrap();

    // Append a second block: the original block's text is untouched.
    let mut edited = TYPES.to_owned();
    edited.push_str("##\nconst conformance_probe = 1\n");
    db.set_file_content(file, &edited).unwrap();

    let renewed = db.get::<BlockSource>(BlockId { file, index: 0 }).unwrap();
    assert_eq!(*renewed, *block0_text);
    let _ = db.get::<TypeOf>(DefId::new(0)).unwrap();
    let _ = db.get::<TypeOf>(DefId::new(1)).unwrap();

    // The file re-parsed and re-indexed, block 0's source recomputed and
    // renewed, and only block 1's downstream ran for the first time.
    assert_eq!(db.compute_count(&parse_key(file)), 2);
    assert_eq!(db.compute_count(&index_key(file)), 2);
    assert_eq!(db.compute_count(&source_key(file, 0)), 2);
    assert_eq!(db.compute_count(&source_key(file, 1)), 1);
    assert_eq!(db.compute_count(&QueryKey::TypeOf(DefId::new(0))), 1);
    assert_eq!(db.compute_count(&QueryKey::TypeOf(DefId::new(1))), 1);
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
    let file = db.add_file("main.ds", TWO_BLOCKS);
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
    let file = db.add_file("main.ds", TWO_BLOCKS);

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
    let file = db.add_file("main.ds", TWO_BLOCKS);
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
        Ok(QueryValue::ParseFile(ParseOutput {
            ast: None,
            diagnostics: Arc::from(Vec::<Diagnostic>::new()),
            source,
        }))
    };
    db.set_compute(ds_query::QueryKind::ParseFile, sane);
    let parse = db.get::<ParseFile>(file).unwrap();
    assert_eq!(&*parse.source, TWO_BLOCKS);
}

/// Test marker for the placeholder Lower key: unregistered until its
/// milestone, so asking for it is [`QueryError::Unavailable`].
struct Lower;

impl Query for Lower {
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

#[test]
fn placeholder_query_without_compute_is_unavailable() {
    let mut db = Db::new();
    let block = BlockId {
        file: FileId::new(0),
        index: 0,
    };
    let err = db.get::<Lower>(block).unwrap_err();
    assert_eq!(err, QueryError::Unavailable(QueryKey::Lower(block)));
}

#[test]
fn unknown_inputs_report_errors_and_are_not_memoized() {
    let mut db = Db::new();
    let ghost = FileId::new(99);

    assert_eq!(
        db.set_file_content(ghost, "const a = 1\n"),
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

    let file = db.add_file("main.ds", TWO_BLOCKS);
    let err = db
        .get::<BlockSource>(BlockId { file, index: 7 })
        .unwrap_err();
    assert_eq!(err, QueryError::UnknownBlock(BlockId { file, index: 7 }));
}

#[test]
fn same_inputs_same_results() {
    let mut a = Db::new();
    let mut b = Db::new();
    let fa = a.add_file("main.ds", TWO_BLOCKS);
    let fb = b.add_file("main.ds", TWO_BLOCKS);

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
