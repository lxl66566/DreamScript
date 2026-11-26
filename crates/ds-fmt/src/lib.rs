//! `ds fmt` as a compiler component (H6 item 2): a block-aware, zero-option
//! formatter over the parse query. The public entry formats a whole file;
//! internally every `##` block renders from its own AST plus trivia only
//! (`render_block`), and one item renders standalone ([`format_item`]), so
//! the LSP on-type path formats a single declaration without touching its
//! neighbors — block boundaries are the only thing the file-level assembly
//! reads.
//!
//! # Canonical form (zero options)
//!
//! - 2-space indent; the output ends with a single trailing newline (empty input formats to the
//!   empty string).
//! - Generic application renders with square brackets (`list[int]`); the angle form the parser
//!   tolerates is never emitted. Case-type references (`bad_number(string)`) keep their parentheses
//!   — the AST's `Path` carries the delimiter.
//! - Trailing commas are tolerated everywhere per F1 and never emitted; the one exception is the
//!   one-element tuple/pattern `(x,)`, where the comma alone keeps it a tuple.
//! - Trait/impl members always render as `fn name(params) -> ret { ... }`; the AST erases the
//!   const-arrow member spelling, so one form is chosen.
//! - Multiline struct/enum/trait/impl bodies and match arms are newline-separated without commas
//!   `[6.1]`; commas survive only inside a one-line inline container. Named arguments render `name:
//!   value` and literal signature defaults render `= lit` (F7).
//! - `.`-chains keep the author's line breaks (continuations at base indent + 2); everything else
//!   that fits its container's layout renders flat.
//! - Paths render with `::` separators; `a.b` types render `a::b`.
//! - A call whose only argument is the trailing block (F7) renders without a parenthesis pair of
//!   its own (`with_file(p, f) { ... }`); `else if` chains keep the `if`.
//! - There is NO wrapping: long lines stay long. A braced container renders inline iff its span
//!   lies on one source line; otherwise it opens up — the exceptions are struct literals and a
//!   tag's struct payload, which fold to one line whatever the source spelled (their field lists
//!   are comma-only in the grammar, so folding keeps one canonical spelling).
//! - Layout line breaks are bare LF; string and raw-string literals, comments, and foreign types
//!   re-emit their source spans byte for byte, line endings included — [`format_source_tracked`]
//!   names those ranges so an EOL fold can leave them alone.
//!
//! # Trivia placement
//!
//! Line comments attach to the following item (or statement); a comment on
//! the same line as the previous item's end trails that item; comments after
//! the last item of a block (including EOF comments) are kept as the block's
//! trailing lines — nothing is dropped. `///` doc-comments on items are
//! ordinary leading trivia and travel with their item; a member's or field's
//! attached doc run (the parse layer's `doc` slot) prints immediately above
//! its member at the member's indent. Blank-line
//! runs between
//! elements collapse to at most one blank line. `##` separators are
//! recovered verbatim from the source (text and column preserved) and a
//! blank line around a separator is preserved; in a script entry they
//! keep their place too — a `##`-carrying entry renders in its script
//! shape with the separators between its statement groups, and only an
//! unseparated entry folds to the written `fn main`. Comments interior to a
//! declaration that no member or field doc slot owns (deep trivia) migrate
//! to the item's trailing position — a v1 limit: position moves, content
//! never disappears.
//!
//! ```
//! use ds_diag::{FileId, Files};
//! use ds_fmt::format_source;
//! use ds_parse::parse;
//!
//! let mut files = Files::new();
//! let src = "const one=1\nconst  two   = 2\n";
//! let file = files.add_source("m.ds", src);
//! let ast = parse(&files, file).expect("parses");
//! let formatted = format_source(&files, file, &ast);
//! assert_eq!(formatted, "const one = 1\nconst two = 2\n");
//! ```

#![warn(missing_docs)]

mod ctx;
mod decl;
mod expr;
/// The `.dsi` signature spellings (G4): the pub items' re-parsable
/// declaration lines `ds info` prints and the LSP hover answers.
pub mod signature;
mod ty;

use ctx::Ctx;
use ds_diag::{FileId, Files};
use ds_parse::{Ast, Block, Expr, Item, ItemKind, LocalDecl, Pattern, Stmt};
use expr::{ElemStream, push_comment, stmt_span_end, stmt_span_start, write_stmt_elem};

/// Formats one parsed file. Zero options: layout follows the canonical form
/// and the source's own line structure (inline vs multiline containers,
/// chain breaks, blank lines) — there is no width parameter and no wrapping.
///
/// The `ast` must be the parse of `file` in `files` (the formatter reads the
/// source for verbatim strings, comments, and separator lines).
#[must_use]
pub fn format_source(files: &Files, file: FileId, ast: &Ast) -> String {
    format_source_tracked(files, file, ast).text
}

/// A whole-file format result: the canonical text plus the output byte
/// ranges that hold source text verbatim (string and raw-string literals,
/// comments, foreign types). Line-ending normalization may fold only the
/// bytes outside the verbatim ranges — a literal's or comment's own line
/// endings are program data and survive byte for byte.
#[must_use]
pub struct FormattedSource {
    /// The formatted text (layout line breaks are bare LF).
    pub text: String,
    /// Output byte ranges of verbatim source text, sorted and
    /// non-overlapping.
    pub verbatim: Vec<(usize, usize)>,
}

/// [`format_source`] plus the verbatim ranges of its output, for callers
/// that re-spell line endings: folding must skip the verbatim ranges.
pub fn format_source_tracked(files: &Files, file: FileId, ast: &Ast) -> FormattedSource {
    let ctx = Ctx::new(files.source(file));
    let parts = match synthesized_main(&ast.blocks) {
        // A `##`-carrying script entry keeps its script shape (the
        // separator between statement groups cannot live inside the
        // fold's one `fn main`); an unseparated one folds to the written
        // `fn main` like before.
        Some((_, stmts)) => {
            script_parts(&ctx, &ast.blocks, stmts).unwrap_or_else(|| block_parts(&ctx, &ast.blocks))
        },
        None => block_parts(&ctx, &ast.blocks),
    };
    let (text, verbatim) = assemble(&ctx, &parts);
    FormattedSource { text, verbatim }
}

/// One part per block: the rendered text, the content bounds its separator
/// gap windows live between, and the text-local verbatim ranges.
fn block_parts(ctx: &Ctx, blocks: &[Block]) -> Vec<Part> {
    blocks
        .iter()
        .map(|b| Part {
            text: render_block(ctx, b),
            start: content_start(b),
            end: content_end(b),
            verbatim: ctx.take_verbatim(),
        })
        .collect()
}

/// The script entry's synthesized `main` (the parse-side fold): an
/// immutable `main` binding of one lambda whose pattern and lambda spans
/// are empty — no real node owns an empty span — carrying the folded
/// top-level statements in its block body.
fn synthesized_main(blocks: &[Block]) -> Option<(&Item, &[Stmt])> {
    let item = blocks
        .iter()
        .flat_map(|b| b.items.iter())
        .find(|i| is_folded_main_item(i))?;
    let ItemKind::Local(l) = &item.kind else {
        return None;
    };
    let Expr::Lambda { body, .. } = &l.init else {
        return None;
    };
    let Expr::Block(b) = &**body else {
        return None;
    };
    Some((item, &b.stmts))
}

/// Whether the item is the script entry's synthesized `main`.
fn is_folded_main_item(item: &Item) -> bool {
    matches!(&item.kind, ItemKind::Local(l) if is_folded_main(l))
}

/// The synthesized-`main` shape: a `main` binding whose pattern and
/// lambda spans are empty. Real `fn main` spellings carry real token
/// spans, so the empty spans identify the fold beyond the name.
fn is_folded_main(l: &LocalDecl) -> bool {
    let Pattern::Binding { span, name, .. } = &l.pat else {
        return false;
    };
    name.name == "main"
        && span.is_empty()
        && matches!(&l.init, Expr::Lambda { span, .. } if span.is_empty())
}

/// The unfolded parts of a `##`-carrying script entry, or `None` when no
/// `##` shares the file with the folded statements: each block renders
/// its own items plus the statements the fold pulled out of it, so every
/// separator stays between the statement groups it separates (the
/// grammar admits `##` only between top-level blocks, so the one-`fn
/// main` spelling cannot carry them and stays for unseparated entries).
fn script_parts(ctx: &Ctx, blocks: &[Block], stmts: &[Stmt]) -> Option<Vec<Part>> {
    let groups = script_groups(blocks, stmts);
    let bounds: Vec<(u32, u32)> = blocks
        .iter()
        .zip(&groups)
        .map(|(b, g)| script_bounds(b, g))
        .collect();
    if !script_has_separator(ctx, &bounds) {
        return None;
    }
    Some(
        blocks
            .iter()
            .zip(&groups)
            .zip(bounds)
            .map(|((b, g), (start, end))| Part {
                text: render_script_block(ctx, b, g),
                start,
                end,
                verbatim: ctx.take_verbatim(),
            })
            .collect(),
    )
}

/// The folded statements grouped by their source block: each one
/// converted inside exactly one file_block pair, so the unique block
/// whose span contains it is its own.
fn script_groups<'a>(blocks: &[Block], stmts: &'a [Stmt]) -> Vec<Vec<&'a Stmt>> {
    let mut groups = vec![Vec::new(); blocks.len()];
    for s in stmts {
        let bi = blocks
            .iter()
            .position(|b| b.span.start <= s.span.start && s.span.end <= b.span.end)
            .expect("every folded statement lies inside its block");
        groups[bi].push(s);
    }
    groups
}

/// Whether a `##` separator lives in any gap between (or behind) the
/// blocks' content: the fold would have to displace it, because the one
/// `fn main` it synthesizes hangs on the last block.
fn script_has_separator(ctx: &Ctx, bounds: &[(u32, u32)]) -> bool {
    // Offsets are u32 by the Files contract.
    #[allow(clippy::cast_possible_truncation)]
    let src_len = ctx.src().len() as u32;
    bounds
        .windows(2)
        .any(|w| find_separator(ctx, w[0].1, w[1].0).is_some())
        || bounds
            .last()
            .is_some_and(|&(.., end)| find_separator(ctx, end, src_len).is_some())
}

/// Honest content bounds of one unfolded script block: the declaration
/// items (minus the synthesized `main`) plus the folded statements that
/// render back into it. Every element really lives in the block, so the
/// separator gap windows need no clamps (the fold's lying span never
/// enters here).
fn script_bounds(block: &Block, stmts: &[&Stmt]) -> (u32, u32) {
    let mut start = u32::MAX;
    let mut end = 0;
    for item in &block.items {
        if is_folded_main_item(item) {
            continue;
        }
        start = start.min(item_start(item));
        end = end.max(item.span.end);
        if let Some(t) = item.trailing_trivia.last() {
            end = end.max(t.span.end);
        }
    }
    for stmt in stmts {
        start = start.min(stmt_span_start(stmt));
        end = end.max(stmt_span_end(stmt));
    }
    if let Some(t) = block.trailing_trivia.first() {
        start = start.min(t.span.start);
    }
    if let Some(t) = block.trailing_trivia.last() {
        end = end.max(t.span.end);
    }
    if start == u32::MAX {
        start = block.span.start;
    }
    if end == 0 {
        end = block.span.end;
    }
    (start, end)
}

/// Renders one unfolded script block: its declaration items and the folded
/// statements that came from it, merged back in source order, each at
/// indent 0 — the file's own script shape, which keeps every `##`
/// separator between the statement groups it separates.
fn render_script_block(ctx: &Ctx, block: &Block, stmts: &[&Stmt]) -> String {
    let mut out = String::new();
    let mut stream = ElemStream::new(false);
    let mut items = block
        .items
        .iter()
        .filter(|i| !is_folded_main_item(i))
        .peekable();
    let mut stmts = stmts.iter().peekable();
    loop {
        match (items.peek().copied(), stmts.peek().copied()) {
            (None, None) => break,
            (Some(item), None) => {
                items.next();
                render_item(ctx, &mut out, &mut stream, item);
            },
            (None, Some(stmt)) => {
                stmts.next();
                write_stmt_elem(ctx, &mut out, &mut stream, 0, stmt);
            },
            (Some(item), Some(stmt)) => {
                if item_start(item) <= stmt_span_start(stmt) {
                    items.next();
                    render_item(ctx, &mut out, &mut stream, item);
                } else {
                    stmts.next();
                    write_stmt_elem(ctx, &mut out, &mut stream, 0, stmt);
                }
            },
        }
    }
    for t in &block.trailing_trivia {
        push_comment(ctx, &mut out, &mut stream, 0, t);
    }
    out
}

/// First content offset of one item: its leading comments or its own
/// span, whichever starts first — the merge key against the folded
/// statements' own starts.
fn item_start(item: &Item) -> u32 {
    item.leading_trivia
        .first()
        .map_or(item.span.start, |t| t.span.start.min(item.span.start))
}

/// Renders one block from its own items and trivia only. This is the
/// block-grain unit; it never reads another block's nodes.
pub(crate) fn render_block(ctx: &Ctx, block: &Block) -> String {
    let mut out = String::new();
    let mut stream = ElemStream::new(false);
    for item in &block.items {
        render_item(ctx, &mut out, &mut stream, item);
    }
    for t in &block.trailing_trivia {
        push_comment(ctx, &mut out, &mut stream, 0, t);
    }
    out
}

/// Writes one item's full standalone rendering — its leading comments,
/// the item at indent 0, and the same-line trailing comments — appended
/// through the caller's element stream. A single item rendered over a
/// fresh stream is exactly its slice of the block render, which is what
/// [`format_item`] serves the on-type path.
fn render_item(ctx: &Ctx, out: &mut String, stream: &mut ElemStream, item: &Item) {
    for t in &item.leading_trivia {
        push_comment(ctx, out, stream, 0, t);
    }
    stream.begin(ctx, out, item.span.start);
    decl::write_item(ctx, out, 0, item);
    // An attached member/field doc run prints at its member inside the
    // rendered body, so the trailing migration skips those trivia —
    // only the remaining interior comments (and a same-line trailing
    // comment) land behind the item. The element's end is the furthest
    // of the item's own extent and the comments it rendered: a
    // same-line tail comment sits past the item, while a migrated deep
    // comment sits before the closing brace in the source, so its end
    // alone would manufacture a phantom blank line before the next
    // element.
    for t in &item.trailing_trivia {
        if decl::is_member_doc(&item.kind, t.span) {
            continue;
        }
        out.push(' ');
        ctx.push_verbatim(out, t.span);
    }
    let (.., end) = item_region(item);
    stream.end(end);
}

/// The source bounds (byte offsets) of one block item's format region:
/// the item's span widened left over its leading comments and right over
/// the trailing comments the rendering appends inline (member docs stay
/// inside the item's own span). The on-type path probes containment
/// against exactly these bounds, so what it formats is what it replaces.
#[must_use]
pub fn item_region(item: &Item) -> (u32, u32) {
    let start = item
        .leading_trivia
        .first()
        .map_or(item.span.start, |t| t.span.start.min(item.span.start));
    let mut end = item.span.end;
    for t in &item.trailing_trivia {
        if !decl::is_member_doc(&item.kind, t.span) {
            end = end.max(t.span.end);
        }
    }
    (start, end)
}

/// One block item's canonical rendering together with the source region
/// it replaces in isolation: the tightest unit on-type formatting
/// reformats. Neighboring items, separators, and the whitespace between
/// elements stay untouched — those belong to the whole-file assembly.
#[must_use]
pub struct ItemFormat {
    /// First byte of the region.
    pub start: u32,
    /// One past the region's last byte.
    pub end: u32,
    /// The region's canonical text (no trailing newline).
    pub text: String,
}

/// Renders one block item standalone: the canonical text of
/// [`item_region`]'s range, the item's own slice of the whole-file
/// format. The `item` must belong to the parse of `file` in `files`.
pub fn format_item(files: &Files, file: FileId, item: &Item) -> ItemFormat {
    let ctx = Ctx::new(files.source(file));
    let mut text = String::new();
    let mut stream = ElemStream::new(false);
    render_item(&ctx, &mut text, &mut stream, item);
    let (start, end) = item_region(item);
    ItemFormat { start, end, text }
}

/// First content offset of a block (leading comments, first item, or block
/// trailing comments) for separator-gap windows. The block's own span can
/// start earlier (the `file_block` pair covers surrounding whitespace), so
/// only real content counts.
fn content_start(block: &Block) -> u32 {
    let mut start = u32::MAX;
    if let Some(item) = block.items.first() {
        start = item.span.start;
        if let Some(t) = item.leading_trivia.first() {
            start = start.min(t.span.start);
        }
    }
    if let Some(t) = block.trailing_trivia.first() {
        start = start.min(t.span.start);
    }
    if start == u32::MAX {
        start = block.span.start;
    }
    // The synthesized script-entry main joins the file's first..last folded
    // statements, so its span starts before the carrying block's own content;
    // a gap-window end left at that lying start would swallow (or invert,
    // panicking) the windows that hold the separators between the folded
    // statement groups. Real content never starts before its block, so the
    // clamp is a no-op outside the fold.
    start.max(block.span.start)
}

/// Last content offset of a block (items, their comments, trailing
/// comments). Not the block span: a trailing comment behind the last item
/// sits outside the pair (the separator's spacing rule consumes it), so
/// blank-line detection around the separator needs real content offsets.
fn content_end(block: &Block) -> u32 {
    let mut end = 0;
    for item in &block.items {
        // The synthesized script-entry main ends at the last folded
        // statement, which can sit before this block's own span (a trailing
        // separator leaves an empty block the fold then fills); its ground
        // belongs to earlier windows, so a lying end is skipped. Real items
        // never end before their block.
        if item.span.end >= block.span.start {
            end = end.max(item.span.end);
        }
        if let Some(t) = item.trailing_trivia.last() {
            end = end.max(t.span.end);
        }
    }
    if let Some(t) = block.trailing_trivia.last() {
        end = end.max(t.span.end);
    }
    if end == 0 {
        // Empty block: for the empty pairs between consecutive separators
        // start and end coincide; for a block whose statements the fold
        // routed away, the pair's end is where its content really ends.
        end = block.span.end;
    }
    end
}

/// One block's contribution to the assembled file: its rendered text, the
/// source content bounds the separator gap windows live between, and the
/// text-local verbatim output ranges the EOL fold must skip.
struct Part {
    /// The block's canonical rendering (empty when it holds no content).
    text: String,
    /// First real content offset (see [`content_start`]).
    start: u32,
    /// Last real content offset (see [`content_end`]).
    end: u32,
    /// Verbatim ranges local to `text`.
    verbatim: Vec<(usize, usize)>,
}

/// A recovered `##` separator line, verbatim from the source (the marker
/// is line-initial, so the line starts at it; its text is preserved).
struct Separator {
    /// Offset of the separator line's first byte.
    line_start: u32,
    /// Offset just past the separator line's text, its padding trimmed.
    text_end: u32,
    /// Offset of the newline (or EOF) ending the separator line.
    line_end: u32,
}

/// Finds the `##` separator inside a gap window. All comments in the gap
/// are attached as trivia (rendered inside block content), so the window
/// holds only whitespace around the separator token and its line text. An
/// inverted window (start past end) holds nothing — slicing it would
/// panic, and a folded script entry can produce one before the clamp in
/// [`content_start`].
// Offsets are u32 by the Files contract.
#[allow(clippy::cast_possible_truncation)]
fn find_separator(ctx: &Ctx, from: u32, to: u32) -> Option<Separator> {
    if from >= to {
        return None;
    }
    let src = ctx.src();
    let bytes = src.as_bytes();
    let tok = from as usize + memchr::memmem::find(&bytes[from as usize..to as usize], b"##")?;
    // The reverse scan stays inside the window: a newline before it would
    // clamp to the window start.
    let line_start = memchr::memrchr(b'\n', &bytes[from as usize..tok])
        .map_or(from as usize, |i| from as usize + i + 1);
    let line_end = memchr::memchr(b'\n', &bytes[tok..]).map_or(src.len(), |i| tok + i);
    let text_end = line_start + src[line_start..line_end].trim_end().len();
    // Pin the invariant the first-match search rides on: outside the
    // separator's own line the window holds whitespace only, so the
    // first `##` is the separator and nothing before it can be one.
    debug_assert!(
        tok == line_start
            && bytes[from as usize..tok]
                .iter()
                .chain(&bytes[line_end.min(to as usize)..to as usize])
                .all(u8::is_ascii_whitespace),
        "the gap window holds content besides the separator line"
    );
    Some(Separator {
        line_start: line_start as u32,
        text_end: text_end as u32,
        line_end: line_end as u32,
    })
}

/// Concatenates the rendered blocks with their separators. A blank line
/// before a separator (or after one) is preserved as a single blank line;
/// empty blocks (from consecutive separators) contribute nothing. Answers
/// the assembled text plus its whole-file verbatim ranges (each part's
/// text-local ranges shifted by where the part landed).
fn assemble(ctx: &Ctx, parts: &[Part]) -> (String, Vec<(usize, usize)>) {
    // Offsets are u32 by the Files contract.
    #[allow(clippy::cast_possible_truncation)]
    let src_len = ctx.src().len() as u32;
    let src = ctx.src();
    let mut out = String::new();
    let mut verbatim = Vec::new();
    for (i, part) in parts.iter().enumerate() {
        let last = i + 1 == parts.len();
        let end = part.end;
        let next_start = if last { src_len } else { parts[i + 1].start };
        let sep = find_separator(ctx, end, next_start);
        if !part.text.is_empty() {
            // push_part prepends the newline when out already holds text.
            let base = out.len() + usize::from(!out.is_empty());
            push_part(&mut out, &part.text);
            for &(s, e) in &part.verbatim {
                verbatim.push((base + s, base + e));
            }
        }
        let Some(sep) = sep else {
            continue;
        };
        if !part.text.is_empty() && ctx.blank_between(end, sep.line_start) {
            push_part(&mut out, "");
        }
        push_part(
            &mut out,
            &src[sep.line_start as usize..sep.text_end as usize],
        );
        if !last && !parts[i + 1].text.is_empty() && ctx.blank_between(sep.line_end, next_start) {
            push_part(&mut out, "");
        }
    }
    if !out.is_empty() {
        out.push('\n');
    }
    (out, verbatim)
}

/// Appends one output part on its own line.
fn push_part(out: &mut String, part: &str) {
    if !out.is_empty() {
        out.push('\n');
    }
    out.push_str(part);
}

#[cfg(test)]
mod tests {
    use ds_parse::parse;

    use super::*;

    fn fmt_src(src: &str) -> String {
        let mut files = Files::new();
        let file = files.add_source("t.ds", src);
        let ast = parse(&files, file).expect("parses");
        format_source(&files, file, &ast)
    }

    /// Block independence: rendering each block in isolation and assembling
    /// with the recovered separators reproduces the whole-file result, and a
    /// block rendered alone equals its slice of the file output.
    #[test]
    fn blocks_format_independently() {
        let src = "tag a\n// about b\nconst b = 1\n\n## section two\n\nfn c() { x\n}\n\n##\n\ntag \
                   d\n// eof\n";
        let mut files = Files::new();
        let file = files.add_source("t.ds", src);
        let ast = parse(&files, file).expect("parses");
        assert!(ast.blocks.len() >= 3, "three blocks expected");
        let whole = format_source(&files, file, &ast);
        let ctx = Ctx::new(files.source(file));
        let parts = block_parts(&ctx, &ast.blocks);
        let (reassembled, verbatim) = assemble(&ctx, &parts);
        assert_eq!(reassembled, whole);
        // Each verbatim range names source text present in the output.
        for &(s, e) in &verbatim {
            assert!(src.contains(&whole[s..e]), "verbatim range not in output");
        }
        // Each isolated block text appears verbatim in the whole-file output.
        for part in &parts {
            if !part.text.is_empty() {
                assert!(
                    whole.contains(&part.text),
                    "missing block:\n{}\nin:\n{whole}",
                    part.text
                );
            }
        }
    }

    #[test]
    fn separators_and_blanks_survive() {
        assert_eq!(fmt_src("tag a\n##\ntag b\n"), "tag a\n##\ntag b\n");
        assert_eq!(
            fmt_src("tag a\n\n## s\n\ntag b\n"),
            "tag a\n\n## s\n\ntag b\n"
        );
        assert_eq!(fmt_src("##\n##\ntag a\n"), "##\n##\ntag a\n");
        assert_eq!(fmt_src("tag a\n##\n"), "tag a\n##\n");
    }

    /// A single item rendered standalone is its slice of the whole-file
    /// output, and the region bounds cover exactly the bytes the text
    /// replaces: leading comments included, the trailing newline and any
    /// neighbor excluded.
    #[test]
    fn items_render_standalone() {
        let src = "const one=1\n// about f\nfn  f() -> int { x\n}\nconst two=2\n";
        let mut files = Files::new();
        let file = files.add_source("t.ds", src);
        let ast = parse(&files, file).expect("parses");
        let items: Vec<&Item> = ast.blocks.iter().flat_map(|b| b.items.iter()).collect();
        assert_eq!(items.len(), 3, "three top-level items");
        let whole = format_source(&files, file, &ast);
        // The middle item's region starts at its leading comment and ends
        // at its own last byte; the rendered text carries the comment.
        let (start, end) = item_region(items[1]);
        assert_eq!(
            &src[start as usize..end as usize],
            "// about f\nfn  f() -> int { x\n}"
        );
        let standalone = format_item(&files, file, items[1]);
        assert_eq!(standalone.start, start);
        assert_eq!(standalone.end, end);
        assert_eq!(standalone.text, "// about f\nfn f() -> int {\n  x\n}");
        assert!(
            whole.contains(&standalone.text),
            "the standalone render is the item's slice of the file: {whole}"
        );
        // A trailing same-line comment widens the region past the item.
        let src = "const a = 1 // tail\n";
        let file = files.add_source("u.ds", src);
        let ast = parse(&files, file).expect("parses");
        let item = &ast.blocks[0].items[0];
        let (start, end) = item_region(item);
        assert_eq!(&src[start as usize..end as usize], "const a = 1 // tail");
        assert_eq!(format_item(&files, file, item).text, "const a = 1 // tail");
    }

    #[test]
    fn empty_and_blank_only_files() {
        assert_eq!(fmt_src(""), "");
        assert_eq!(fmt_src("  \n\n\t\n"), "");
    }
}
