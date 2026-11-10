//! `ds fmt` as a compiler component [H6 item 2]: a block-aware, zero-option
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
//! trailing lines — nothing is dropped. `///` doc-comments are ordinary
//! leading trivia and travel with their item. Blank-line runs between
//! elements collapse to at most one blank line. `##` separators are
//! recovered verbatim from the source (text and column preserved) and a
//! blank line around a separator is preserved. Comments interior to a
//! declaration (deep trivia) migrate to the item's trailing position — a v1
//! limit: position moves, content never disappears.
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
use expr::{Elem, push_elems, trivia_elem};

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
    let mut elems: Vec<Elem> = Vec::new();
    for item in &block.items {
        for t in &item.leading_trivia {
            elems.push(trivia_elem(ctx, 0, t));
        }
        let mut text = String::new();
        decl::write_item(ctx, &mut text, 0, item);
        for t in &item.trailing_trivia {
            text.push(' ');
            text.push_str(ctx.comment_text(t));
        }
        elems.push(Elem {
            // The item element starts at the item itself; its leading
            // comments are separate elements, so the gap between the last
            // comment and the item is measured comment-end -> item start.
            start: item.span.start,
            end: item
                .trailing_trivia
                .last()
                .map_or(item.span.end, |t| t.span.end),
            text,
        });
    }
    for t in &block.trailing_trivia {
        elems.push(trivia_elem(ctx, 0, t));
    }
    let mut out = String::new();
    push_elems(ctx, &mut out, &elems, false);
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
    start
}

/// Last content offset of a block (items, their comments, trailing
/// comments). Not the block span: a trailing comment behind the last item
/// sits outside the pair (the separator's spacing rule consumes it), so
/// blank-line detection around the separator needs real content offsets.
fn content_end(block: &Block) -> u32 {
    let mut end = 0;
    for item in &block.items {
        end = end.max(item.span.end);
        if let Some(t) = item.trailing_trivia.last() {
            end = end.max(t.span.end);
        }
    }
    if let Some(t) = block.trailing_trivia.last() {
        end = end.max(t.span.end);
    }
    if end == 0 {
        // Empty block (consecutive separators): it sits right after the
        // previous separator, so anchor the window at its start.
        end = block.span.start;
    }
    end
}

/// A recovered `##` separator line, verbatim from the source (the marker
/// is line-initial, so the line starts at it; its text is preserved).
struct Separator {
    text: String,
    /// Offset of the separator line's first byte.
    line_start: u32,
    /// Offset of the newline (or EOF) ending the separator line.
    line_end: u32,
}

/// Finds the `##` separator inside a gap window. All comments in the gap
/// are attached as trivia (rendered inside block content), so the window
/// holds only whitespace around the separator token and its line text.
// Offsets are u32 by the Files contract.
#[allow(clippy::cast_possible_truncation)]
fn find_separator(ctx: &Ctx, from: u32, to: u32) -> Option<Separator> {
    let src = ctx.src();
    let tok = src[from as usize..to as usize].find("##")? + from as usize;
    let line_start = src[..tok]
        .rfind('\n')
        .map_or(from as usize, |i| i + 1)
        .max(from as usize);
    let line_end = src[tok..].find('\n').map_or(src.len(), |i| tok + i);
    Some(Separator {
        text: src[line_start..line_end].trim_end().to_owned(),
        line_start: line_start as u32,
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
    let mut parts: Vec<String> = Vec::new();
    for (i, text) in rendered.iter().enumerate() {
        if !text.is_empty() {
            parts.push(text.clone());
        }
        let last = i + 1 == blocks.len();
        let window_end = if last { src_len } else { content_start(&blocks[i + 1]) };
        let Some(sep) = find_separator(ctx, content_end(&blocks[i]), window_end) else {
            continue;
        };
        if !text.is_empty() && ctx.blank_between(content_end(&blocks[i]), sep.line_start) {
            parts.push(String::new());
        }
        parts.push(sep.text);
        if !last
            && !rendered[i + 1].is_empty()
            && ctx.blank_between(sep.line_end, content_start(&blocks[i + 1]))
        {
            parts.push(String::new());
        }
    }
    if parts.is_empty() {
        String::new()
    } else {
        let mut out = parts.join("\n");
        out.push('\n');
        out
    }
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
