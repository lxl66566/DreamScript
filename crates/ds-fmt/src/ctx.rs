//! Source access for rendering: verbatim slices, line lookup, and the two
//! layout probes (single-line spans, blank-line gaps) that drive the
//! formatter's source-driven layout decisions.

use ds_diag::Span;
use ds_parse::Trivia;

/// Rendering context over one file's source text.
pub(crate) struct Ctx<'s> {
    src: &'s str,
    /// Byte offsets where each line starts; line 0 starts at 0.
    line_starts: Vec<u32>,
}

impl<'s> Ctx<'s> {
    pub(crate) fn new(src: &'s str) -> Self {
        let mut line_starts = vec![0u32];
        for (i, b) in src.bytes().enumerate() {
            if b == b'\n' {
                // Offsets are u32 by the Files contract.
                #[allow(clippy::cast_possible_truncation)]
                let start = i as u32 + 1;
                line_starts.push(start);
            }
        }
        Self { src, line_starts }
    }

    /// Source text of a span (verbatim, for comments and string literals).
    pub(crate) fn text(&self, span: Span) -> &'s str {
        &self.src[span.start as usize..span.end as usize]
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
        self.line(next_start) > self.line(prev_end) + 1
    }

    /// Whether the source between two offsets contains a newline — used to
    /// detect author-broken `.`-chains, the only continuation the grammar
    /// allows across lines.
    pub(crate) fn newline_between(&self, prev_end: u32, next_start: u32) -> bool {
        self.src[prev_end as usize..next_start as usize].contains('\n')
    }

    /// Comments render verbatim on their own line.
    pub(crate) fn comment_text(&self, t: &Trivia) -> &'s str {
        self.text(t.span)
    }
}
