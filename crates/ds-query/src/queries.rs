//! The front-end computes: parse, block index, the block-source seam, and
//! the M2 linearity check.
//!
//! Dependency shape, top to bottom:
//!
//! * `parse` reads the `FileContent` input (through [`Ctx::file_content`]) and the `ds-diag`
//!   registry (through [`Ctx::files`]), and runs `ds-parse`. The value carries the tree and the
//!   diagnostics together: syntax errors are data, not engine errors, so a broken file does not
//!   poison dependents.
//! * `block_index` reads the parse result and maps the tree's block spans to [`BlockId`]s. When the
//!   parse failed (`ast: None`) it degrades to the `##` text scan below, so tooling keeps block
//!   structure — and the block-grain seam — for files with syntax errors.
//! * `block_source` reads the parse result and the block index and slices out one block's text.
//!   Downstream block-keyed queries depend on this value — not on the file — so a file edit that
//!   leaves a block's text unchanged renews (not invalidates) that block's dependents.
//! * `resolve` reads every registered file's parse and runs the ds-check collection and resolution
//!   passes (G3): every registered file is the package, a file whose parse failed contributes no
//!   declarations, and the value carries the namespace, per-file use tables, and the collection and
//!   resolution diagnostics. The dependency is the whole set of parses plus the file-set input
//!   ([`Ctx::file_count`]), so the query invalidates at file grain and on registration — collection
//!   is package-wide, so any parse edit can change any resolution.
//! * `type_of` reads the resolve query (resolution computes once per package) and every parse, runs
//!   the ds-check bidirectional pass (B2) over the package, and slices one definition's story out
//!   of it: the declaration and body diagnostics whose primary labels address the definition's
//!   block, plus the solved type of each binding it holds, rendered through the package universe.
//!   Like `linearity_check` the dependency is the whole parse set, so the query invalidates at file
//!   grain — and on registration, through the file-set input `package_asts` also reads — the check
//!   is package-wide, so block-grain renewal would under-approximate it.
//! * `linearity_check` reads the parse result and the block index of the definition's file and runs
//!   the M2 affine pass [C1, C5] over that file as a single-file package, then slices out the
//!   definition's facts. The dependency is the whole parse, so this query invalidates at file
//!   grain: the pass is a package fixpoint (cross-item moves are legal inside a file), so
//!   block-grain renewal would under-approximate it. The block-grain seam stays what the M3
//!   lowering reads.
//! * `effects_of` reads every registered file's parse and runs the ds-check effects pass (E1): the
//!   M2 package is all registered files (G3), a file whose parse failed contributes no tree (its
//!   definitions are absent), and one definition's row is returned by its package index. The
//!   file-set input rides along with the parses, so registration invalidates every row too.
//!
//! [`Ctx::file_content`]: crate::Ctx::file_content
//! [`Ctx::file_count`]: crate::Ctx::file_count
//! [`Ctx::files`]: crate::Ctx::files

use std::sync::Arc;

use ds_diag::{FileId, LabelStyle, Span};
use ds_parse::Ast;

use crate::{
    db::Ctx,
    error::QueryError,
    key::{BlockId, QueryKey},
    query::{BlockIndex, ParseFile},
    value::{
        BlockIndexOutput, BlockInfo, LinearityOutput, ParseOutput, QueryValue, ResolveOutput,
        TypeOfOutput,
    },
};

/// The parsed trees of every registered file, in file order; a file
/// whose parse failed contributes no tree (its declarations are absent).
fn package_asts(ctx: &mut Ctx<'_>) -> Result<Vec<Arc<Ast>>, QueryError> {
    let count = u32::try_from(ctx.file_count()).expect("file ids are u32");
    let mut asts = Vec::new();
    for file in 0..count {
        if let Some(ast) = &ctx.get::<ParseFile>(FileId::new(file))?.ast {
            asts.push(Arc::clone(ast));
        }
    }
    Ok(asts)
}

/// Name resolution for the package (G3): the ds-check collection and
/// resolution passes over the registered files. The [`PackageId`] key is
/// the package grain — M2 has one package, every registered file, so the
/// id is ignored until M7's package loading mints real identity.
pub(crate) fn resolve(ctx: &mut Ctx<'_>, key: QueryKey) -> Result<QueryValue, QueryError> {
    let QueryKey::Resolve(_) = key else {
        return Err(QueryError::KindMismatch(key));
    };
    let asts = package_asts(ctx)?;
    let (package, resolutions, diags) = ds_check::resolve_package(&asts);
    Ok(QueryValue::Resolve(ResolveOutput {
        package: Arc::new(package),
        resolutions: resolutions.into(),
        diags: diags.into(),
    }))
}

/// The type story of one definition (B2): the ds-check bidirectional pass
/// over the package, sliced to the definition's block. `DefId` packs
/// `(file << 8) | block` like [`queries::linearity_check`](crate), so the
/// definition is its block until real definition identity arrives.
///
/// Reads the resolve query — resolution computes once per package — and
/// every parse. A file that fails to parse yields the empty story for
/// every definition in it: syntax diagnostics are the parse query's data
/// and flow through it.
pub(crate) fn type_of(ctx: &mut Ctx<'_>, key: QueryKey) -> Result<QueryValue, QueryError> {
    let QueryKey::TypeOf(def) = key else {
        return Err(QueryError::KindMismatch(key));
    };
    let file = FileId::new(def.as_u32() >> 8);
    let block = BlockId {
        file,
        index: def.as_u32() & 0xff,
    };
    let parse = ctx.get::<ParseFile>(file)?;
    let index = ctx.get::<BlockIndex>(file)?;
    let Some(info) = index.blocks.iter().find(|info| info.id == block) else {
        return Err(QueryError::UnknownBlock(block));
    };
    let span = info.span;
    let empty = TypeOfOutput {
        types: Vec::new().into(),
        diags: Vec::new().into(),
    };
    if parse.ast.as_deref().is_none() {
        return Ok(QueryValue::TypeOf(empty));
    }
    // The package check runs over every parseable file, resolution first.
    let resolved = ctx.get::<crate::query::Resolve>(crate::key::PackageId::new(0))?;
    let asts = package_asts(ctx)?;
    let checked =
        ds_check::check_resolved(ctx.files(), &asts, &resolved.package, &resolved.resolutions);
    let in_block = |at: Span| at.file == span.file && at.start >= span.start && at.end <= span.end;
    let diags = checked
        .checked
        .diags
        .iter()
        .filter(|diag| {
            diag.labels
                .iter()
                .any(|label| label.style == LabelStyle::Primary && in_block(label.span))
        })
        .cloned()
        .collect::<Vec<_>>()
        .into();
    let types = (0..resolved.package.len())
        .filter_map(|raw| {
            let id = ds_check::DefId::from_raw(u32::try_from(raw).expect("def count fits u32"));
            let def = resolved.package.def(id);
            in_block(def.span)
                .then(|| {
                    checked
                        .type_names
                        .get(&id)
                        .map(|ty| (def.name.clone(), ty.clone()))
                })
                .flatten()
        })
        .collect::<Vec<_>>()
        .into();
    Ok(QueryValue::TypeOf(TypeOfOutput { types, diags }))
}

/// One definition's effect row (E1): the ds-check effects pass over the
/// registered files, keyed by the definition's package index.
///
/// Package grain at M2 (G3): every registered file is the package, and
/// the compute reads each file's parse plus the file-set input, so an
/// edit anywhere — or a file registered mid-session — invalidates every
/// row. One limit stays for the M7 `PackageConfig` seam: the ambient row
/// table (`ds_check::DeclaredRows`) is fixed when the database is built,
/// so world rows flow only once the platform declaration supplies it.
pub(crate) fn effects_of(ctx: &mut Ctx<'_>, key: QueryKey) -> Result<QueryValue, QueryError> {
    let QueryKey::EffectsOf(def) = key else {
        return Err(QueryError::KindMismatch(key));
    };
    let asts = package_asts(ctx)?;
    let effects = ds_check::effects_of(&asts, ctx.declared_rows());
    effects
        .of(ds_check::DefId::from_raw(def.as_u32()))
        .map(QueryValue::EffectRow)
        .ok_or(QueryError::NoEffectRow(def))
}

/// Parses one file with `ds-parse`: tree plus diagnostics as one value.
///
/// A pure function of exactly one input — the file's `FileContent` read
/// through [`Ctx::file_content`]; nothing else in the context is read.
/// `ParseOutput::eq` (value.rs) compares only the source text on the
/// strength of that purity: if this compute ever grows more inputs, that
/// equality must widen with it.
pub(crate) fn parse(ctx: &mut Ctx<'_>, key: QueryKey) -> Result<QueryValue, QueryError> {
    let QueryKey::ParseFile(file) = key else {
        return Err(QueryError::KindMismatch(key));
    };
    let source = ctx.file_content(file)?;
    let (ast, diagnostics) = match ds_parse::parse(ctx.files(), file) {
        Ok(ast) => (Some(Arc::new(ast)), Vec::new()),
        Err(diags) => (None, diags),
    };
    Ok(QueryValue::ParseFile(ParseOutput {
        ast,
        diagnostics: Arc::from(diagnostics),
        source,
    }))
}

/// Indexes a file's blocks: tree spans when the parse succeeded, the `##`
/// scan when it failed.
pub(crate) fn block_index(ctx: &mut Ctx<'_>, key: QueryKey) -> Result<QueryValue, QueryError> {
    let QueryKey::BlockIndex(file) = key else {
        return Err(QueryError::KindMismatch(key));
    };
    let parse = ctx.get::<ParseFile>(file)?;
    let blocks = match parse.ast.as_deref() {
        Some(ast) => ast
            .blocks
            .iter()
            .enumerate()
            .map(|(index, block)| BlockInfo {
                id: BlockId {
                    file,
                    index: u32::try_from(index).expect("block count overflows u32"),
                },
                span: block.span,
            })
            .collect::<Vec<_>>(),
        None => split_blocks(file, &parse.source),
    };
    Ok(QueryValue::BlockIndex(BlockIndexOutput {
        blocks: blocks.into_boxed_slice(),
    }))
}

/// Slices one block's text out of the parse result: the seam downstream
/// block-keyed queries depend on. Contract unchanged since the placeholder:
/// the value is the block's source text, and text equality is what renews
/// dependents on unrelated file edits.
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
    let text = parse.source.get(range).expect(
        "block spans are char boundaries (tree spans from the parser, scan spans from separator \
         token edges)",
    );
    Ok(QueryValue::BlockSource(Arc::from(text)))
}

/// Runs the M2 affine pass [C1, C5] over one definition's file and slices
/// the definition's facts out of it. `DefId` packs `(file, block)` until
/// the real definition table lands, so the definition is its block and its
/// facts are those addressing the block's span.
///
/// A file that fails to parse yields empty facts for every definition:
/// syntax diagnostics are the parse query's data and flow through it.
pub(crate) fn linearity_check(ctx: &mut Ctx<'_>, key: QueryKey) -> Result<QueryValue, QueryError> {
    let QueryKey::LinearityCheck(def) = key else {
        return Err(QueryError::KindMismatch(key));
    };
    let file = FileId::new(def.as_u32() >> 8);
    let block = BlockId {
        file,
        index: def.as_u32() & 0xff,
    };
    let parse = ctx.get::<ParseFile>(file)?;
    let index = ctx.get::<BlockIndex>(file)?;
    let Some(info) = index.blocks.iter().find(|info| info.id == block) else {
        return Err(QueryError::UnknownBlock(block));
    };
    let span = info.span;
    let empty = LinearityOutput {
        diags: Vec::new().into(),
        moves: Vec::new().into(),
        drops: Vec::new().into(),
        last_uses: Vec::new().into(),
    };
    let Some(ast) = parse.ast.as_deref() else {
        return Ok(QueryValue::LinearityCheck(empty));
    };
    let (_, lin) = ds_check::linearity::check(ctx.files(), std::slice::from_ref(ast));
    let in_block = |at: Span| at.file == span.file && at.start >= span.start && at.end <= span.end;
    let diags = lin
        .diags
        .iter()
        .filter(|diag| {
            diag.labels
                .iter()
                .any(|label| label.style == LabelStyle::Primary && in_block(label.span))
        })
        .cloned()
        .collect::<Vec<_>>()
        .into();
    let moves = lin
        .moves
        .iter()
        .filter(|fact| in_block(fact.at))
        .copied()
        .collect::<Vec<_>>()
        .into();
    let drops = lin
        .drops
        .iter()
        .filter(|fact| in_block(fact.at))
        .copied()
        .collect::<Vec<_>>()
        .into();
    let last_uses = lin
        .last_uses
        .iter()
        .filter(|fact| in_block(fact.at))
        .copied()
        .collect::<Vec<_>>()
        .into();
    Ok(QueryValue::LinearityCheck(LinearityOutput {
        diags,
        moves,
        drops,
        last_uses,
    }))
}

/// Lexical frames of the degraded block scan: the nested context the cursor
/// is inside. A separator is recognized only at the file level (empty
/// stack), mirroring the grammar's `sep_unit` at `file_blocks` level.
#[derive(Clone, Copy)]
enum ScanFrame {
    /// A code bracket `(`, `[`, or `{`.
    Open,
    /// An interpolation hole `${` inside a string.
    Hole,
    /// An ordinary string; single-line [6.1], so a newline closes it.
    Str,
    /// A char or byte literal; single-line.
    Char,
    /// A raw string with `hashes` closing `#`s.
    Raw(usize),
    /// A line comment, to its newline.
    LineComment,
    /// A block comment, to `*/`.
    BlockComment,
}

/// The line-initial `##` block separator (G3).
const SEPARATOR: &str = "##";

/// Splits `source` into blocks for files whose parse failed: a byte-level
/// scan aligned with the grammar's separator rule (`sep_unit` requires a
/// newline before `block_sep = "##" + rest of line`). A `##` separates
/// only where the grammar accepts one — at the file level, outside every
/// bracket, at column 0 — and never inside strings, raw strings, char
/// literals, interpolation holes, comments, brackets, or mid-line. The
/// separator owns the rest of its line; a leading separator opens an empty
/// first block; a file without separators is one block (G3).
///
/// A block's extent runs from the newline ending the previous separator
/// line (its opening newline, as in the tree) to its last content byte —
/// the same rule the tree applies, so a file healing from scan to tree
/// keeps every intact block's text and renews its dependents instead of
/// recomputing them. Block 0 is the exception on the opening side: the
/// tree's `top_gap` eats the leading gap before the first item (it lives
/// outside `file_blocks`, unlike the gap after a separator, which the
/// first `file_block` owns), so block 0 opens at its first content byte.
/// Content is every byte outside comments, plus whitespace inside strings
/// and raw strings; horizontal whitespace, newlines, and comment lines
/// trailing an item stay outside the extent, exactly like the tree's span.
///
/// Residual approximation, inherent to scanning unparseable input: pairing
/// brackets and strings on a broken file can mis-lex (a stray closer pops a
/// bracket early; an unterminated raw string runs to end of input), and a
/// `##` hidden by such a mis-lex then goes unseen — degraded extents are
/// advisory until the file parses again and the tree takes over.
fn split_blocks(file: FileId, source: &str) -> Vec<BlockInfo> {
    let b = source.as_bytes();
    let mut blocks = Vec::new();
    let mut block_start = 0_u32;
    let mut content_end = 0_u32;
    // First content byte of block 0 (None while none was seen): the offset
    // the tree's block 0 opens at. An empty block 0 takes its position from
    // the push sites below instead.
    let mut first_content: Option<u32> = None;
    let mut stack: Vec<ScanFrame> = Vec::new();
    let mut i = 0_usize;
    while i < b.len() {
        let at = |n: usize| b.get(i + n).copied().unwrap_or(0);
        let prev = i.checked_sub(1).and_then(|p| b.get(p).copied());
        let boundary_prev = !matches!(prev, Some(c) if c.is_ascii_alphanumeric() || c == b'_');
        // A line-initial `##` at the file level opens a separator line.
        if stack.is_empty()
            && b.get(i..i + SEPARATOR.len()) == Some(SEPARATOR.as_bytes())
            && matches!(prev, None | Some(b'\n'))
        {
            let start = if blocks.is_empty() {
                match first_content {
                    // The tree's empty block 0 sits at the newline ending
                    // the leading gap (sep_unit claims it; top_gap stops
                    // right before).
                    Some(start) => start,
                    None => u32::try_from(i.saturating_sub(1)).expect("offset overflows u32"),
                }
            } else {
                block_start
            };
            blocks.push(BlockInfo {
                id: BlockId {
                    file,
                    index: u32::try_from(blocks.len()).expect("block count overflows u32"),
                },
                span: Span::new(file, start, content_end.max(start)),
            });
            // The separator owns the rest of its line; the newline ending
            // it opens the next block (the tree agrees).
            i = match b[i..].iter().position(|&c| c == b'\n') {
                Some(p) => i + p,
                None => b.len(),
            };
            block_start = u32::try_from(i).expect("offset overflows u32");
            content_end = block_start;
            continue;
        }
        // Content tracking for extent ends (see `content_end`'s contract).
        // A byte opening a comment is trivia, not content.
        match stack.last() {
            Some(ScanFrame::LineComment | ScanFrame::BlockComment) => {},
            Some(ScanFrame::Str | ScanFrame::Char | ScanFrame::Raw(..)) => {
                if blocks.is_empty() {
                    first_content.get_or_insert(u32::try_from(i).expect("offset overflows u32"));
                }
                content_end = u32::try_from(i + 1).expect("offset overflows u32");
            },
            _ if b[i] == b'/' && matches!(at(1), b'/' | b'*') => {},
            _ if matches!(b[i], b' ' | b'\t' | b'\n' | b'\r') => {},
            _ => {
                if blocks.is_empty() {
                    first_content.get_or_insert(u32::try_from(i).expect("offset overflows u32"));
                }
                content_end = u32::try_from(i + 1).expect("offset overflows u32");
            },
        }
        match stack.last().copied() {
            Some(ScanFrame::LineComment) => {
                if b[i] == b'\n' {
                    stack.pop();
                }
                i += 1;
            },
            Some(ScanFrame::BlockComment) => {
                if b[i] == b'*' && at(1) == b'/' {
                    stack.pop();
                    i += 2;
                } else {
                    i += 1;
                }
            },
            Some(ScanFrame::Str) => match b[i] {
                // Single-line [6.1]: resync at the newline.
                b'\n' => {
                    stack.pop();
                },
                b'\\' => i += 2,
                b'"' => {
                    stack.pop();
                    i += 1;
                },
                b'$' if at(1) == b'{' => {
                    stack.push(ScanFrame::Hole);
                    i += 2;
                },
                _ => i += 1,
            },
            Some(ScanFrame::Char) => match b[i] {
                b'\n' | b'\'' => {
                    stack.pop();
                    i += 1;
                },
                b'\\' => i += 2,
                _ => i += 1,
            },
            Some(ScanFrame::Raw(hashes)) => {
                if b[i] == b'"' {
                    let mut k = 0;
                    while b.get(i + 1 + k) == Some(&b'#') {
                        k += 1;
                    }
                    if k == hashes {
                        stack.pop();
                        i += 1 + k;
                    } else {
                        i += 1;
                    }
                } else {
                    i += 1;
                }
            },
            // Code: inside a hole, inside brackets, or at the file level.
            Some(ScanFrame::Hole | ScanFrame::Open) | None => {
                match b[i] {
                    b'/' if at(1) == b'/' => {
                        stack.push(ScanFrame::LineComment);
                        i += 2;
                    },
                    b'/' if at(1) == b'*' => {
                        stack.push(ScanFrame::BlockComment);
                        i += 2;
                    },
                    b'"' => {
                        stack.push(ScanFrame::Str);
                        i += 1;
                    },
                    b'\'' => {
                        stack.push(ScanFrame::Char);
                        i += 1;
                    },
                    b'r' if boundary_prev && (at(1) == b'"' || at(1) == b'#') => {
                        let mut k = 0;
                        while at(1 + k) == b'#' {
                            k += 1;
                        }
                        stack.push(ScanFrame::Raw(k));
                        // Skip `r`, the hashes, and the opening quote.
                        i += 1 + k + 1;
                    },
                    b'b' if boundary_prev && at(1) == b'\'' => {
                        stack.push(ScanFrame::Char);
                        // Skip `b` and the opening quote.
                        i += 2;
                    },
                    b'{' | b'(' | b'[' => {
                        stack.push(ScanFrame::Open);
                        i += 1;
                    },
                    b'}' if matches!(stack.last(), Some(ScanFrame::Hole)) => {
                        stack.pop();
                        i += 1;
                    },
                    b')' | b']' | b'}' if matches!(stack.last(), Some(ScanFrame::Open)) => {
                        stack.pop();
                        i += 1;
                    },
                    _ => i += 1,
                }
            },
        }
    }
    let start = if blocks.is_empty() {
        match first_content {
            Some(start) => start,
            // An all-trivia file: the tree's single block sits at the end
            // of the gap, which is the end of input.
            None => u32::try_from(b.len()).expect("offset overflows u32"),
        }
    } else {
        block_start
    };
    blocks.push(BlockInfo {
        id: BlockId {
            file,
            index: u32::try_from(blocks.len()).expect("block count overflows u32"),
        },
        span: Span::new(file, start, content_end.max(start)),
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
        let spans = spans(FileId::new(0), "const a = 1\nconst b = a\n");
        assert_eq!(spans, vec![(0, 23)]);
    }

    #[test]
    fn separators_split_and_belong_to_no_block() {
        // "const a" [0,7), "##" [8,10), "const b" [11,18): the newline
        // ending a separator line opens the following block, and a block
        // ends at its last content byte.
        let spans = spans(FileId::new(0), "const a\n##\nconst b\n");
        assert_eq!(spans, vec![(0, 7), (10, 18)]);
    }

    #[test]
    fn leading_gap_before_block_0_stays_outside_the_extent() {
        // The tree's `top_gap` before `file_blocks` owns the leading gap;
        // block 0 opens at its first content byte under the scan too, so a
        // file healing from scan to tree renews block 0's dependents.
        let got = spans(FileId::new(0), "// header\nconst a = 1\n");
        assert_eq!(got, vec![(10, 21)]);
        let got = spans(FileId::new(0), "\nconst a = 1\n");
        assert_eq!(got, vec![(1, 12)]);
        let got = spans(FileId::new(0), "// h\nconst a = 1\n##\nconst b = 2\n");
        assert_eq!(got, vec![(5, 16), (19, 31)]);
    }

    #[test]
    fn leading_separator_opens_empty_first_block() {
        let spans = spans(FileId::new(0), "##\nconst b = 2\n");
        assert_eq!(spans, vec![(0, 0), (2, 14)]);
    }

    #[test]
    fn indented_hash_line_is_not_a_separator() {
        // Only column 0 separates; the grammar rejects the file outright,
        // and the scan keeps the line inside the preceding block.
        let src = "const a = 1\n  ##  \nconst b = 2\n";
        let end = u32::try_from(src.len() - 1).expect("source overflows u32 spans");
        let spans = spans(FileId::new(0), src);
        assert_eq!(spans, vec![(0, end)]);
    }

    #[test]
    fn mid_line_hash_is_not_a_separator() {
        // `item ##` never separates (the separator is line-initial).
        let src = "const a = x ##\nconst b = 2\n";
        let end = u32::try_from(src.len() - 1).expect("source overflows u32 spans");
        let got = spans(FileId::new(0), src);
        assert_eq!(got, vec![(0, end)]);
    }

    #[test]
    fn comment_line_before_a_separator_does_not_hide_it() {
        // The comment line is trivia before the separator: block 0 ends at
        // its item, block 1 opens at the separator line's newline and runs
        // to its own last content byte.
        let src = "const a = 1\n/* c */\n##\nconst b = 2\n";
        // "const a = 1" [0,11); "##" at [20,22); "const b = 2" [23,34).
        let got = spans(FileId::new(0), src);
        assert_eq!(got, vec![(0, 11), (22, 34)]);
    }

    #[test]
    fn separator_inside_a_line_comment_is_not_a_separator() {
        let src = "const a = 1 // ##\nconst b = 2\n";
        let end = u32::try_from(src.len() - 1).expect("source overflows u32 spans");
        let spans = spans(FileId::new(0), src);
        assert_eq!(spans, vec![(0, end)]);
    }

    #[test]
    fn separator_inside_a_string_is_not_a_separator() {
        let src = "const a = \"##\"\nconst b = 2\n";
        let end = u32::try_from(src.len() - 1).expect("source overflows u32 spans");
        let spans = spans(FileId::new(0), src);
        assert_eq!(spans, vec![(0, end)]);
    }

    #[test]
    fn separator_inside_a_raw_string_is_not_a_separator() {
        let src = "const a = r#\"##\"#\nconst b = 2\n";
        let end = u32::try_from(src.len() - 1).expect("source overflows u32 spans");
        let spans = spans(FileId::new(0), src);
        assert_eq!(spans, vec![(0, end)]);
    }

    #[test]
    fn separator_inside_brackets_is_not_a_separator() {
        // sep_unit lives at file level only; inside `{ }` the `##` is
        // body text (and the grammar rejects the file outright).
        let src = "fn f() { ##\nconst b = 2\n";
        let end = u32::try_from(src.len() - 1).expect("source overflows u32 spans");
        let spans = spans(FileId::new(0), src);
        assert_eq!(spans, vec![(0, end)]);
    }

    #[test]
    fn empty_file_is_one_empty_block() {
        let spans = spans(FileId::new(0), "");
        assert_eq!(spans, vec![(0, 0)]);
    }

    #[test]
    fn scan_mirrors_tree_block_indices() {
        // A file that later fails to parse degrades to the scan, so the
        // scan must number blocks exactly like the tree it replaces:
        // same count, and every item of tree block i lands in scan block i.
        // The spans must agree too, or healing from scan to tree would
        // spuriously invalidate the blocks whose text never changed.
        let sources = [
            "##\n",
            "##",
            "const a = 1\n",
            "##\nconst b = 2\n",
            "const a = 1\n##\n",
            "const a = 1\n## s\n",
            "##\nconst a = 1\n##\nconst b = 2\n##\n",
            "##\n##\ntag a\n",
            "// c\n##\nconst b = 2\n",
            "const a = 1\n\n##\n\nconst b = 2\n",
            "## one\nconst b = 2\n## two\nconst c = 3\n",
            "// header\nconst a = 1\n",
            "\n\nconst a = 1\n",
            "// h\nconst a = 1\n##\nconst b = 2\n",
            "// a\nconst x = 1 // tail\n##\nconst y = 2\n",
        ];
        for src in sources {
            let mut files = ds_diag::Files::new();
            let file = files.add_source("m.ds", src);
            let ast = ds_parse::parse(&files, file)
                .unwrap_or_else(|d| panic!("{src:?} fails to parse: {d:?}"));
            let scan = split_blocks(file, src);
            assert_eq!(scan.len(), ast.blocks.len(), "block count for {src:?}");
            let tree: Vec<(u32, u32)> = ast
                .blocks
                .iter()
                .map(|b| (b.span.start, b.span.end))
                .collect();
            let got: Vec<(u32, u32)> = scan.iter().map(|b| (b.span.start, b.span.end)).collect();
            assert_eq!(got, tree, "scan spans for {src:?}");
            for (i, block) in ast.blocks.iter().enumerate() {
                for item in &block.items {
                    let hit = scan
                        .iter()
                        .position(|b| {
                            b.span.start <= item.span.start && item.span.start < b.span.end
                        })
                        .unwrap_or_else(|| {
                            panic!("no scan block covers offset {} in {src:?}", item.span.start)
                        });
                    assert_eq!(
                        hit, i,
                        "item of tree block {i} in scan block {hit}: {src:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn block_ids_are_numbered_in_source_order() {
        let blocks = split_blocks(FileId::new(3), "const a = 1\n##\nconst b = 2\n");
        let ids: Vec<u32> = blocks.iter().map(|info| info.id.index).collect();
        assert_eq!(ids, vec![0, 1]);
        assert!(blocks.iter().all(|info| info.id.file.as_u32() == 3));
    }
}
