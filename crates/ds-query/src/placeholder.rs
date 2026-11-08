//! Placeholder computes modeling the `ds-parse` seam.
//!
//! These three functions are registered by [`Db::new`](crate::Db::new) and
//! stand in for the real front-end pipeline until `ds-parse` lands. The
//! follow-up replaces them (by editing this module or by
//! [`Db::set_compute`](crate::Db::set_compute)) while keeping the dependency
//! shape they establish:
//!
//! * `parse` reads the `FileContent` input and returns the syntax tree. Today the "tree" is the raw
//!   source; the real one is the green AST with spans and trivia.
//! * `block_index` reads the parse result and maps byte spans to blocks (`///|` separation per G3).
//!   Today blocks are found by a line scan; the real one derives them from the tree, which handles
//!   trivia the scan cannot see.
//! * `block_source` reads the parse result and the block index and slices out one block's text.
//!   Downstream block-keyed queries depend on this value — not on the file — so a file edit that
//!   leaves a block's text unchanged renews (not invalidates) that block's dependents. The real one
//!   keeps this contract verbatim; only the span source changes.

use std::sync::Arc;

use ds_diag::{FileId, Span};

use crate::{
    db::Ctx,
    error::QueryError,
    key::{BlockId, QueryKey},
    query::{BlockIndex, ParseFile},
    value::{BlockIndexOutput, BlockInfo, ParseOutput, QueryValue},
};

/// The `///|` block separator [G3].
const SEPARATOR: &str = "///|";

/// Placeholder parse: the "tree" is the file source itself.
pub(crate) fn parse(ctx: &mut Ctx<'_>, key: QueryKey) -> Result<QueryValue, QueryError> {
    let QueryKey::ParseFile(file) = key else {
        return Err(QueryError::KindMismatch(key));
    };
    let source = ctx.file_content(file)?;
    Ok(QueryValue::ParseFile(ParseOutput { source }))
}

/// Placeholder block index: blocks found by scanning separator lines.
pub(crate) fn block_index(ctx: &mut Ctx<'_>, key: QueryKey) -> Result<QueryValue, QueryError> {
    let QueryKey::BlockIndex(file) = key else {
        return Err(QueryError::KindMismatch(key));
    };
    let parse = ctx.get::<ParseFile>(file)?;
    let blocks = split_blocks(file, &parse.source);
    Ok(QueryValue::BlockIndex(BlockIndexOutput {
        blocks: blocks.into_boxed_slice(),
    }))
}

/// Placeholder block source: the block's text sliced out of the parse result.
pub(crate) fn block_source(ctx: &mut Ctx<'_>, key: QueryKey) -> Result<QueryValue, QueryError> {
    let QueryKey::BlockSource(block) = key else {
        return Err(QueryError::KindMismatch(key));
    };
    let parse = ctx.get::<ParseFile>(block.file)?;
    let index = ctx.get::<BlockIndex>(block.file)?;
    let Some(info) = index.blocks.iter().find(|info| info.id == block) else {
        return Err(QueryError::UnknownBlock(block));
    };
    let range = info.span.start as usize..info.span.end as usize;
    let text = parse
        .source
        .get(range)
        .expect("block spans are char boundaries produced by the splitter");
    Ok(QueryValue::BlockSource(Arc::from(text)))
}

/// Splits `source` into blocks at separator lines (a line whose first
/// non-blank characters are `///|`). Placeholder semantics, pending the
/// parser's authoritative separation: separator lines belong to no block, a
/// leading separator opens an empty first block, and a file without
/// separators is one block spanning everything [G3].
fn split_blocks(file: FileId, source: &str) -> Vec<BlockInfo> {
    let len = u32::try_from(source.len()).expect("source overflows u32 spans");
    let mut blocks = Vec::new();
    let mut block_start = 0_u32;
    let mut offset = 0_usize;
    for line in source.split_inclusive('\n') {
        let line_start = offset;
        offset += line.len();
        if line.trim_start().starts_with(SEPARATOR) {
            blocks.push(BlockInfo {
                id: BlockId {
                    file,
                    index: u32::try_from(blocks.len()).expect("block count overflows u32"),
                },
                span: Span::new(
                    file,
                    block_start,
                    u32::try_from(line_start).expect("offset overflows u32"),
                ),
            });
            block_start = u32::try_from(offset).expect("offset overflows u32");
        }
    }
    blocks.push(BlockInfo {
        id: BlockId {
            file,
            index: u32::try_from(blocks.len()).expect("block count overflows u32"),
        },
        span: Span::new(file, block_start, len),
    });
    blocks
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spans(file: FileId, source: &str) -> Vec<(u32, u32)> {
        split_blocks(file, source)
            .into_iter()
            .map(|info| (info.span.start, info.span.end))
            .collect()
    }

    #[test]
    fn no_separators_is_one_block() {
        let spans = spans(FileId::new(0), "let a = 1\nlet b = a\n");
        assert_eq!(spans, vec![(0, 20)]);
    }

    #[test]
    fn separators_split_and_belong_to_no_block() {
        // "A\n" [0,2), "///|\n" [2,7), "B\n" [7,9), "///|\n" [9,14), "C\n" [14,16)
        let spans = spans(FileId::new(0), "A\n///|\nB\n///|\nC\n");
        assert_eq!(spans, vec![(0, 2), (7, 9), (14, 16)]);
    }

    #[test]
    fn leading_separator_opens_empty_first_block() {
        let spans = spans(FileId::new(0), "///|\nB\n");
        assert_eq!(spans, vec![(0, 0), (5, 7)]);
    }

    #[test]
    fn indented_separator_still_separates() {
        let spans = spans(FileId::new(0), "A\n  ///|  \nB\n");
        assert_eq!(spans, vec![(0, 2), (11, 13)]);
    }

    #[test]
    fn separator_like_text_inside_a_line_is_not_a_separator() {
        // `///|` must start the line's non-blank content.
        let spans = spans(FileId::new(0), "let a = x ///|\nB\n");
        assert_eq!(spans, vec![(0, 17)]);
    }

    #[test]
    fn empty_file_is_one_empty_block() {
        let spans = spans(FileId::new(0), "");
        assert_eq!(spans, vec![(0, 0)]);
    }

    #[test]
    fn block_ids_are_numbered_in_source_order() {
        let blocks = split_blocks(FileId::new(3), "A\n///|\nB\n");
        let ids: Vec<u32> = blocks.iter().map(|info| info.id.index).collect();
        assert_eq!(ids, vec![0, 1]);
        assert!(blocks.iter().all(|info| info.id.file.as_u32() == 3));
    }
}
