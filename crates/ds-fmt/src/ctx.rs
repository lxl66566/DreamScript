//! Source access for rendering: verbatim slices, line lookup, and the two
//! layout probes (single-line spans, blank-line gaps) that drive the
//! formatter's source-driven layout decisions.

use std::cell::RefCell;

use ds_diag::Span;

/// Rendering context over one file's source text.
pub(crate) struct Ctx<'s> {
    src: &'s str,
    /// Byte offsets where each line starts; line 0 starts at 0.
    line_starts: Vec<u32>,
    /// Output byte ranges of verbatim text pushed since the last
    /// [`Ctx::take_verbatim`], in output order. Interior mutability keeps
    /// the render functions on `&Ctx`.
    verbatim: RefCell<Vec<(usize, usize)>>,
}

impl<'s> Ctx<'s> {
    pub(crate) fn new(src: &'s str) -> Self {
        Self {
            src,
            line_starts: ds_diag::line_starts_of(src),
            verbatim: RefCell::new(Vec::new()),
        }
    }

    /// Source text of a span (verbatim, for comments and string literals).
    pub(crate) fn text(&self, span: Span) -> &'s str {
        &self.src[span.start as usize..span.end as usize]
    }

    /// Appends one span's source text verbatim, recording the output
    /// range: line-ending normalization must leave these bytes alone (a
    /// literal's or comment's own line endings are program data).
    pub(crate) fn push_verbatim(&self, out: &mut String, span: Span) {
        let text = self.text(span);
        let start = out.len();
        self.verbatim.borrow_mut().push((start, start + text.len()));
        out.push_str(text);
    }

    /// The verbatim output ranges recorded so far, in output order; the
    /// whole-file assembly shifts each block's ranges by the block's
    /// offset in the assembled text.
    pub(crate) fn take_verbatim(&self) -> Vec<(usize, usize)> {
        std::mem::take(&mut *self.verbatim.borrow_mut())
    }

    /// The whole source, for separator recovery over gap windows.
    pub(crate) fn src(&self) -> &'s str {
        self.src
    }

    /// 0-based line index of a byte offset.
    pub(crate) fn line(&self, offset: u32) -> usize {
        self.line_starts.partition_point(|&s| s <= offset) - 1
    }

    /// Whether a span lies entirely on one source line. Drives the
    /// inline-vs-multiline decision for every braced container; a multiline
    /// string inside a one-statement block simply keeps the block multiline,
    /// which is stable across re-formats.
    pub(crate) fn one_line(&self, span: Span) -> bool {
        !self.src[span.start as usize..span.end as usize].contains('\n')
    }

    /// Whether a blank line separates the end of one element (offset `prev_end`)
    /// from the start of the next (`next_start`). Runs of blanks collapse to
    /// one; same-line and adjacent-line elements get no blank.
    pub(crate) fn blank_between(&self, prev_end: u32, next_start: u32) -> bool {
        if prev_end >= next_start {
            return false;
        }
        // A line index is a newline count, so the gap holds two line breaks
        // exactly when it holds two newlines. Scanning the gap itself costs
        // less than two line-table probes per element boundary.
        let gap = &self.src.as_bytes()[prev_end as usize..next_start as usize];
        memchr::memchr(b'\n', gap).is_some_and(|i| memchr::memchr(b'\n', &gap[i + 1..]).is_some())
    }

    /// Whether the source between two offsets contains a newline — used to
    /// detect author-broken `.`-chains, the only continuation the grammar
    /// allows across lines.
    pub(crate) fn newline_between(&self, prev_end: u32, next_start: u32) -> bool {
        self.src[prev_end as usize..next_start as usize].contains('\n')
    }
}
