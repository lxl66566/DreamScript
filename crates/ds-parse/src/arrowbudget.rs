//! The unbracketed-recursion budgets. The bracket nesting guard in
//! [`crate::lexscan`] cannot see the grammar's two bracket-free recursion
//! classes:
//!
//! - the lambda arrow chain: every `=>` re-enters the full precedence chain (`lambda_bare` ->
//!   `lambda_rhs` -> `expr` -> the chain -> `primary` -> `lambda`), the same ~19-rule traversal a
//!   bracket costs, with no bracket for the counter to count;
//! - the `else if` chain: `if_expr` right-recurses through `sp ~ kw_else ~ sp ~ (if_expr |
//!   block_expr)` after its block has closed, so every `else` adds a nesting level no bracket ever
//!   sees (each level holds only the live `if_expr` activation, ~2 KiB).
//!
//! A long chain of either class walked the grammar onto the parse
//! worker's stack; a stack overflow aborts the process rather than
//! panicking. pest 2.9 refuses both gracefully on its default native
//! targets (a built-in stack guard fails the parse with "stack limit
//! reached" once less than 64 KiB remains), but that guard is target-
//! gated and best-effort, so the budgets here own the guarantee: they
//! are charged by the pre-scan, which runs before the worker, and
//! refuse at the cap as a diagnostic instead — with a stable code and
//! the offending position, and without first consuming the whole
//! segment.

// Span offsets are u32 by contract (Files::add_source rejects longer
// sources), so narrowing casts from usize are total.
#![allow(clippy::cast_possible_truncation)]

use ds_diag::{Diagnostic, ErrorCode, FileId, Span};

/// Live-arrow refusal point, kept independent of the other limits. Worst
/// admitted source: 39 open brackets (`MAX_NEST` in [`crate::lexscan`])
/// plus a 39-arrow run is ~78 chain traversals; at the 12-30 KiB measured
/// per traversal (worker.rs) that stays under 2.4 MiB against the parse
/// worker's 8 MiB stack, while the conversion pass (~3 frames per arrow)
/// and the caller-side Box-drop recursion (~1 frame per node) are orders
/// below it. The same arithmetic bounds the near-legal prefixes pest
/// backtracks through, so an illegal tail cannot smuggle depth past the
/// budget either.
pub(crate) const MAX_ARROWS: u32 = 40;

/// Live-`else` refusal point. Measured on the 8 MiB parse stack, pest's
/// own stack guard refuses an `else if` chain at ~3648 branches, so one
/// level costs ~2 KiB; 499 live elses then cost ~1 MiB. Combined with
/// the other worst cases (39 brackets + 39 arrows, ~2.4 MiB) the whole
/// admitted worst case stays under half the segment, and the caller-side
/// Box-drop recursion (~1 frame per if node) is trivial at this depth.
/// Real generated decoders carry 100+ branches, so 500 sits far above
/// legal use and ~7x under the measured guard point.
pub(crate) const MAX_ELSES: u32 = 500;

/// Spacing classification of the last meaningful byte. A newline ends a
/// statement or arm only after a *completed operand*: the grammar's `sp`
/// crosses newlines after every operator (`h ~ op ~ sp ~ operand`), after
/// `=>`, and after the keywords that reach an expression, so the reset
/// must not fire there.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Tail {
    /// An operator, `=>`, or an opening context: an operand is expected,
    /// so a newline is spacing inside one expression.
    Op,
    /// A complete operand (identifier, literal, `?`, closed bracket): a
    /// following newline ends the statement or arm.
    Operand,
}

/// The live runs snapshot at a frame open and restored at its close, so
/// charges accumulated inside a bracketed region cannot leak outward (a
/// nested if/else chain inside a block dies with the block).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct RunSnap {
    arrows: u32,
    elses: u32,
}

/// What resets the runs, each rule cut against the grammar:
///
/// - `,` — every legal comma separates repetition elements (`args`, `arm_list`, `list_lit`,
///   `paren_tuple`, `params`), so an arrow or else past a comma starts a fresh chain. Repetitions
///   never grow the grammar's recursion, so sibling elements are free. The one place a comma sits
///   inside one chain — a parameter list, `(a, b) => ...` — is covered because the closing `)`
///   restores the snapshot the scanner took at the `(`.
/// - a newline after a completed operand — statements, match arms, and file items are all
///   repetitions (`stmt_list`, `arm_list`, `file_block`), so the siblings behind them are free too.
///   A newline after an operator or `=>` continues one expression, and so does a newline after
///   `in`, `else`, `if`, `while`, `match`, or `for`: the `sp`-crossed continuation reaches a
///   `primary` (`in_rhs`, control heads, `else if`) that may be the next lambda. A field spelled
///   `.` + keyword (`x.in`) does not suppress: a postfix chain cannot host an unbracketed lambda.
/// - a closing bracket — restores the snapshot taken at the matching opener, so chains through
///   parameter lists (`(a) => (b) => ...`) or through parenthesized calls keep accumulating. A `}`
///   closing a block also restores, and when `else` follows across spacing the else run charges one
///   level: in legal source `} else` is exactly `if_expr`'s else clause (the grammar's only other
///   `else` spellings are field names after a dot), and nested chains die at the block restore, so
///   the charge tracks the recursion.
pub(crate) struct ArrowBudget {
    /// Live arrows charged since the last reset; refuses at `MAX_ARROWS`.
    run: u32,
    /// Live `else if` levels charged since the last reset; refuses at
    /// `MAX_ELSES`.
    else_run: u32,
    tail: Tail,
    /// Trailing word (capped; longer words cannot equal a keyword). Read
    /// at a newline to decide keyword suppression.
    word: [u8; 8],
    word_len: u8,
    /// The trailing word was preceded by `.`, so it is a field name even
    /// when it spells a keyword.
    word_field: bool,
    /// A `.` was the last meaningful byte; the next word is a field name.
    after_dot: bool,
    /// Whitespace was seen since the last word byte, so the next word
    /// byte starts a fresh word.
    word_gap: bool,
    /// Set when a charge crossed a budget; the scan loop returns it.
    refusal: Option<Diagnostic>,
}

impl ArrowBudget {
    pub(crate) fn new() -> Self {
        Self {
            run: 0,
            else_run: 0,
            tail: Tail::Op,
            word: [0; 8],
            word_len: 0,
            word_field: false,
            after_dot: false,
            word_gap: false,
            refusal: None,
        }
    }

    /// The live runs; the snapshot lexscan stores per frame.
    pub(crate) fn snap(&self) -> RunSnap {
        RunSnap {
            arrows: self.run,
            elses: self.else_run,
        }
    }

    /// Restores the runs recorded when a bracket or hole opened.
    pub(crate) fn restore(&mut self, snap: RunSnap) {
        self.run = snap.arrows;
        self.else_run = snap.elses;
    }

    /// Takes the budget refusal, if a charge crossed one.
    pub(crate) fn take_refusal(&mut self) -> Option<Diagnostic> {
        self.refusal.take()
    }

    /// Whether a charge has already crossed a budget.
    pub(crate) fn is_refusing(&self) -> bool {
        self.refusal.is_some()
    }

    /// Processes one plain (non-reaction) byte of code or hole context.
    /// `i` is the byte's offset, used for the refusal span.
    pub(crate) fn plain_byte(&mut self, b: &[u8], i: usize, file: FileId) {
        match b[i] {
            b'\n' => self.newline(),
            b'=' if b.get(i + 1) == Some(&b'>') => self.charge_arrow(file, i),
            b',' => {
                self.reset();
                self.op();
            },
            b'.' => {
                self.op();
                self.after_dot = true;
            },
            b'?' => {
                self.tail = Tail::Operand;
                self.word_len = 0;
                self.after_dot = false;
            },
            b' ' | b'\t' => self.word_gap = true,
            c if c.is_ascii_alphanumeric() || c == b'_' => self.word_byte(c),
            _ => self.op(),
        }
    }

    /// Feeds a reaction-context byte the dispatch consumed without a
    /// frame (`r` and `b` that did not open a literal): only these can
    /// carry an identifier the word tracker must not truncate.
    pub(crate) fn dispatch_byte(&mut self, c: u8) {
        if c.is_ascii_alphanumeric() {
            self.word_byte(c);
        }
    }

    /// A bracket or interpolation hole opened: the region is bounded by
    /// the bracket guard, so spacing inside it stays suppressed.
    pub(crate) fn after_open(&mut self) {
        self.op();
    }

    /// A string, char, or raw literal closed: a complete operand.
    pub(crate) fn after_literal(&mut self) {
        self.operand();
    }

    /// `)` or `]` closed (or any mismatched close): the snapshot was
    /// restored by the caller; the closed bracket completes an operand.
    pub(crate) fn after_operand_close(&mut self) {
        self.operand();
    }

    /// A `}` closed a block. When `else` follows across spacing, this is
    /// `if_expr`'s else clause — the arrow tail stays suppressed (the
    /// chain continues past the block) and the else run charges one
    /// recursion level, refusing at the budget. Anything else completes
    /// the statement or arm, so both runs reset at the next newline.
    pub(crate) fn after_block_close(&mut self, b: &[u8], j: usize, file: FileId) {
        match else_span(b, j) {
            Some(at) => {
                self.op();
                self.else_run += 1;
                if self.else_run >= MAX_ELSES && self.refusal.is_none() {
                    self.refusal = Some(
                        Diagnostic::error(
                            ErrorCode::ElseChainTooDeep,
                            format!(
                                "`else if` chain exceeds the parser's recursion budget of \
                                 {MAX_ELSES}"
                            ),
                        )
                        .with_primary(
                            Span::new(file, at as u32, at as u32 + 4),
                            "`else` continues the chain here",
                        ),
                    );
                }
            },
            None => self.operand(),
        }
    }

    /// A `##` separator line was skipped: the next block starts fresh.
    pub(crate) fn after_separator(&mut self) {
        self.reset();
        self.operand();
    }

    fn charge_arrow(&mut self, file: FileId, i: usize) {
        self.run += 1;
        self.op();
        if self.run >= MAX_ARROWS && self.refusal.is_none() {
            self.refusal = Some(
                Diagnostic::error(
                    ErrorCode::ArrowChainTooDeep,
                    format!(
                        "lambda arrow chain exceeds the parser's recursion budget of {MAX_ARROWS}"
                    ),
                )
                .with_primary(Span::new(file, i as u32, i as u32 + 2), "`=>` nested here"),
            );
        }
    }

    fn reset(&mut self) {
        self.run = 0;
        self.else_run = 0;
    }

    /// Runs the newline reset rule once; also called for the newline that
    /// terminates a line comment (it is spacing like any other).
    pub(crate) fn newline(&mut self) {
        if self.tail == Tail::Operand && !self.suppressing_word() {
            self.reset();
        }
    }

    /// The trailing word is a keyword whose `sp`-crossed continuation can
    /// reach a lambda, so the newline is spacing inside one expression.
    fn suppressing_word(&self) -> bool {
        let len = usize::from(self.word_len);
        !self.word_field
            && matches!(
                self.word
                    .get(..len)
                    .and_then(|w| std::str::from_utf8(w).ok()),
                Some("in" | "is" | "if" | "else" | "match" | "while" | "for")
            )
    }

    fn word_byte(&mut self, c: u8) {
        if self.word_len == 0 || self.word_gap {
            self.word_len = 0;
            self.word_field = self.after_dot;
        }
        self.word_gap = false;
        self.after_dot = false;
        let len = usize::from(self.word_len);
        if len < self.word.len() {
            self.word[len] = c;
            self.word_len += 1;
        }
        self.tail = Tail::Operand;
    }

    fn op(&mut self) {
        self.tail = Tail::Op;
        self.word_len = 0;
        self.after_dot = false;
    }

    fn operand(&mut self) {
        self.tail = Tail::Operand;
        self.word_len = 0;
        self.after_dot = false;
    }

    /// Runs `src` through the tracker as one plain run, for tests.
    #[cfg(test)]
    fn feed(&mut self, src: &str) {
        let b = src.as_bytes();
        let file = ds_diag::Files::new().add_source("t.ds", "");
        for i in 0..b.len() {
            self.plain_byte(b, i, file);
        }
    }
}

/// The offset of `else` (modulo comments and blank lines) when it is the
/// next token after `j`, the byte past a closing `}`.
fn else_span(b: &[u8], mut j: usize) -> Option<usize> {
    loop {
        match b.get(j) {
            Some(b' ' | b'\t' | b'\n' | b'\r') => j += 1,
            Some(b'/') if b.get(j + 1) == Some(&b'/') => j += memchr::memchr(b'\n', &b[j..])?,
            Some(b'/') if b.get(j + 1) == Some(&b'*') => {
                let mut k = j + 2;
                loop {
                    match b.get(k) {
                        Some(b'*') if b.get(k + 1) == Some(&b'/') => {
                            j = k + 2;
                            break;
                        },
                        Some(_) => k += 1,
                        None => return None,
                    }
                }
            },
            _ => {
                return (b[j..].starts_with(b"else") && !b.get(j + 4).copied().is_some_and(is_word))
                    .then_some(j)
            },
        }
    }
}

fn is_word(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_'
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Charges without crossing a budget; every case names what the
    /// newline or separator sits after.
    fn run_after(feed: &str) -> u32 {
        let mut st = ArrowBudget::new();
        st.feed(feed);
        st.run
    }

    #[test]
    fn newline_after_arrow_continues_the_chain() {
        // `sp` after `=>` crosses newlines (lambda_bare).
        assert_eq!(run_after("x =>\ny =>"), 2);
    }

    #[test]
    fn newline_after_operand_resets_the_chain() {
        assert_eq!(run_after("x => 1\ny =>"), 1);
    }

    #[test]
    fn newline_after_operator_continues_the_chain() {
        // `h ~ op ~ sp`: a lambda is a legal right operand.
        assert_eq!(run_after("x => b +\ny =>"), 2);
    }

    #[test]
    fn newline_after_continuation_keyword_continues_the_chain() {
        assert_eq!(run_after("x => b in\ny =>"), 2);
        assert_eq!(run_after("match\nx =>"), 1);
    }

    #[test]
    fn keyword_spelled_as_field_name_resets() {
        // `x.in` selects a field; a postfix chain hosts no lambda.
        assert_eq!(run_after("z => q.in\ny =>"), 1);
    }

    #[test]
    fn comma_resets_the_chain() {
        assert_eq!(run_after("x => b,\ny =>"), 1);
    }

    #[test]
    fn keyword_match_is_whole_word() {
        // `inr` is an identifier, not `in` followed by junk.
        assert_eq!(run_after("z => q inr\ny =>"), 1);
    }

    /// The else-charge site: `else` past the `}` at offset 0, with the
    /// search starting at `j` (the byte past that `}`).
    fn elses_after(j: usize, tail: &str) -> u32 {
        let mut st = ArrowBudget::new();
        let file = ds_diag::Files::new().add_source("t.ds", "");
        let b = format!("}}{tail}");
        st.after_block_close(b.as_bytes(), j, file);
        st.else_run
    }

    #[test]
    fn else_after_block_close_charges_one_level() {
        assert_eq!(elses_after(1, " else if c { 1 }"), 1);
        // Across blank lines and comments is still the else clause.
        assert_eq!(elses_after(1, "\n// note\nelse { 1 }"), 1);
        assert_eq!(elses_after(1, " /* c */ else { 1 }"), 1);
    }

    #[test]
    fn anything_else_after_block_close_charges_nothing() {
        assert_eq!(elses_after(1, " if c { 1 }"), 0);
        assert_eq!(elses_after(1, "elseif { 1 }"), 0);
    }
}
