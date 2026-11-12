//! `ds fmt` as a compiler component (H6 item 2): a block-aware, zero-option
//! formatter over the parse query. The public entry formats a whole file;
//! internally every `##` block renders from its own AST plus trivia only
//! (`render_block`), so the LSP on-type path can later format one block
//! without touching its neighbors — block boundaries are the only thing the
//! file-level assembly reads.
//!
//! # Canonical form (zero options)
//!
//! - 2-space indent; the output ends with a single trailing newline (empty input formats to the
//!   empty string).
//! - Generic application renders with square brackets (`list[int]`); the angle form the parser
//!   tolerates is never emitted. Case-type references (`bad_number(string)`) keep their parentheses
//!   — the AST models both as `Path { args }`, so the delimiter is read from the source span.
//! - Trailing commas are tolerated everywhere per F1 and never emitted; the one exception is the
//!   one-element tuple/pattern `(x,)`, where the comma alone keeps it a tuple.
//! - Trait/impl members always render as `fn name(params) -> ret { ... }`; the AST erases the
//!   const-arrow member spelling, so one form is chosen.
//! - Multiline struct/enum/trait/impl bodies and match arms are newline-separated without commas
//!   [6.1]; commas survive only inside a one-line inline container. Named arguments render `name:
//!   value` and literal signature defaults render `= lit` (F7).
//! - `.`-chains keep the author's line breaks (continuations at base indent + 2); everything else
//!   that fits its container's layout renders flat.
//! - Paths render with `::` separators; `a.b` types render `a::b`.
//! - A call whose only argument is the trailing block (F7) renders without a parenthesis pair of
//!   its own (`with_file(p, f) { ... }`); `else if` chains keep the `if`.
//! - There is NO wrapping: long lines stay long. A braced container renders inline iff its span
//!   lies on one source line; otherwise it opens up.
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
//! blank line around a separator is preserved. Comments interior to a
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
mod ty;

use ctx::Ctx;
use ds_diag::{FileId, Files};
use ds_parse::{Ast, Block};
use expr::{ElemStream, push_comment};

/// Formats one parsed file. Zero options: layout follows the canonical form
/// and the source's own line structure (inline vs multiline containers,
/// chain breaks, blank lines) — there is no width parameter and no wrapping.
///
/// The `ast` must be the parse of `file` in `files` (the formatter reads the
/// source for verbatim strings, comments, and separator lines).
#[must_use]
pub fn format_source(files: &Files, file: FileId, ast: &Ast) -> String {
    let ctx = Ctx::new(files.source(file));
    let rendered: Vec<String> = ast.blocks.iter().map(|b| render_block(&ctx, b)).collect();
    assemble(&ctx, &ast.blocks, &rendered)
}

/// Renders one block from its own items and trivia only. This is the
/// block-grain unit the LSP on-type path will call; it never reads another
/// block's nodes.
pub(crate) fn render_block(ctx: &Ctx, block: &Block) -> String {
    let mut out = String::new();
    let mut stream = ElemStream::new(false);
    for item in &block.items {
        for t in &item.leading_trivia {
            push_comment(ctx, &mut out, &mut stream, 0, t);
        }
        stream.begin(ctx, &mut out, item.span.start);
        decl::write_item(ctx, &mut out, 0, item);
        // An attached member/field doc run prints at its member inside the
        // rendered body, so the trailing migration skips those trivia —
        // only the remaining interior comments (and a same-line trailing
        // comment) land behind the item. The element's end is the furthest
        // of the item's own extent and the comments it rendered: a
        // same-line tail comment sits past the item, while a migrated deep
        // comment sits before the closing brace in the source, so its end
        // alone would manufacture a phantom blank line before the next
        // element.
        let mut end = item.span.end;
        for t in &item.trailing_trivia {
            if decl::is_member_doc(&item.kind, t.span) {
                continue;
            }
            out.push(' ');
            out.push_str(ctx.comment_text(t));
            end = end.max(t.span.end);
        }
        stream.end(end);
    }
    for t in &block.trailing_trivia {
        push_comment(ctx, &mut out, &mut stream, 0, t);
    }
    out
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
    Some(Separator {
        line_start: line_start as u32,
        text_end: text_end as u32,
        line_end: line_end as u32,
    })
}

/// Concatenates rendered blocks with their separators. A blank line before a
/// separator (or after one) is preserved as a single blank line; empty
/// blocks (from consecutive separators) contribute nothing.
fn assemble(ctx: &Ctx, blocks: &[Block], rendered: &[String]) -> String {
    // Offsets are u32 by the Files contract.
    #[allow(clippy::cast_possible_truncation)]
    let src_len = ctx.src().len() as u32;
    let src = ctx.src();
    let mut out = String::new();
    for (i, text) in rendered.iter().enumerate() {
        let last = i + 1 == blocks.len();
        // Both content offsets are read more than once below, and each read
        // walks the block's items.
        let end = content_end(&blocks[i]);
        let next_start = if last { src_len } else { content_start(&blocks[i + 1]) };
        let sep = find_separator(ctx, end, next_start);
        if !text.is_empty() {
            push_part(&mut out, text);
        }
        let Some(sep) = sep else {
            continue;
        };
        if !text.is_empty() && ctx.blank_between(end, sep.line_start) {
            push_part(&mut out, "");
        }
        push_part(
            &mut out,
            &src[sep.line_start as usize..sep.text_end as usize],
        );
        if !last && !rendered[i + 1].is_empty() && ctx.blank_between(sep.line_end, next_start) {
            push_part(&mut out, "");
        }
    }
    if !out.is_empty() {
        out.push('\n');
    }
    out
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
        let rendered: Vec<String> = ast.blocks.iter().map(|b| render_block(&ctx, b)).collect();
        assert_eq!(assemble(&ctx, &ast.blocks, &rendered), whole);
        // Each isolated block text appears verbatim in the whole-file output.
        for text in &rendered {
            if !text.is_empty() {
                assert!(whole.contains(text), "missing block:\n{text}\nin:\n{whole}");
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

    #[test]
    fn empty_and_blank_only_files() {
        assert_eq!(fmt_src(""), "");
        assert_eq!(fmt_src("  \n\n\t\n"), "");
    }
}
