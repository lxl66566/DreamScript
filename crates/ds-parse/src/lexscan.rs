//! Lexical pre-scan: a single iterative pass that never panics on arbitrary
//! input. It runs before the pest parse for two jobs:
//!
//! 1. a nesting guard — pest's recursive descent (and this crate's own conversion pass) would
//!    overflow the stack on adversarial nesting, which aborts rather than panics, so [`MAX_NEST`]
//!    rejects it as a diagnostic;
//! 2. precise two-span diagnostics for the lexical class where pest's furthest-failure position is
//!    useless: unterminated strings and interpolation holes, raw-string hash counting, unbalanced
//!    or mismatched bracket pairs, stray semicolons, and characters valid in no token.

// Span offsets are u32 by contract (Files::add_source rejects longer
// sources), so narrowing casts from usize are total.
#![allow(clippy::cast_possible_truncation)]

use ds_diag::{Diagnostic, ErrorCode, FileId, Span, Suggestion};

use crate::arrowbudget::{ArrowBudget, RunSnap};

/// Maximum bracket nesting depth. Bracket nesting bounds the grammar's
/// recursion depth, so this is what keeps any source inside the stack segment
/// the parse runs on. The bracket-free recursion classes (lambda arrow and
/// `else if` chains) are budgeted separately by [`crate::arrowbudget`].
pub(crate) const MAX_NEST: u32 = 40;

/// What the scanner is inside of.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Frame {
    /// An expression bracket `( [ {` opened at a byte offset, with the
    /// live recursion runs at the open (restored when the bracket closes).
    Open(char, u32, RunSnap),
    /// A `${` interpolation hole opened inside a string, with the live
    /// recursion runs at the open (restored when the hole closes).
    Hole(u32, RunSnap),
    /// A normal string opened at the `"` byte offset.
    Str(u32),
    /// A char or byte literal opened at the quote offset.
    Char(u32),
    /// A raw string with the given hash count, opened at the `r` offset.
    Raw(usize, u32),
    /// A line comment.
    LineComment,
    /// A block comment.
    BlockComment,
}

/// Bytes each context reacts to, as one flag per byte value: the main loop
/// skips a run of bytes whose flags miss the context's mask, so a byte costs a
/// table load instead of a match chain.
mod byte_class {
    /// Special in code and interpolation-hole context.
    pub(super) const CODE: u8 = 1 << 0;
    /// Special inside a string, char, or byte literal.
    pub(super) const QUOTED: u8 = 1 << 1;

    pub(super) const TABLE: [u8; 256] = build();

    /// `TABLE` is over-inclusive by design: a byte a context lists but its
    /// dispatch does not react to costs one harmless step.
    const fn build() -> [u8; 256] {
        let mut table = [0u8; 256];
        let code = b"/\"'rb{([)]};$`\\#\r";
        let mut k = 0;
        while k < code.len() {
            table[code[k] as usize] |= CODE;
            k += 1;
        }
        let quoted = b"\"'\\\n\r$";
        let mut k = 0;
        while k < quoted.len() {
            table[quoted[k] as usize] |= QUOTED;
            k += 1;
        }
        table
    }

    /// Advances `i` past every byte whose class misses `mask`.
    pub(super) fn skip(b: &[u8], mut i: usize, mask: u8) -> usize {
        while i < b.len() && TABLE[b[i] as usize] & mask == 0 {
            i += 1;
        }
        i
    }
}

/// Outcome of pushing an opening bracket.
enum Pushed {
    Ok,
    TooDeep(Diagnostic),
}

/// The open frames plus the bracket depth the guard reads; every pop keeps the
/// two in step.
struct Frames {
    stack: Vec<Frame>,
    opens: u32,
}

impl Frames {
    fn new() -> Self {
        Self {
            stack: Vec::new(),
            opens: 0,
        }
    }

    fn top(&self) -> Option<Frame> {
        self.stack.last().copied()
    }

    /// Pushes anything but an opening bracket (brackets go through
    /// [`Frames::push_open`], which guards depth).
    fn push(&mut self, frame: Frame) {
        debug_assert!(!matches!(frame, Frame::Open(..)));
        self.stack.push(frame);
    }

    fn pop(&mut self) -> Option<Frame> {
        let frame = self.stack.pop();
        if matches!(frame, Some(Frame::Open(..))) {
            self.opens -= 1;
        }
        frame
    }

    /// Pushes an opening bracket, guarding depth; `snap` is the live
    /// recursion runs, restored when the bracket closes.
    fn push_open(&mut self, file: FileId, ch: u8, i: usize, snap: RunSnap) -> Pushed {
        if self.opens >= MAX_NEST {
            return Pushed::TooDeep(
                Diagnostic::error(
                    ErrorCode::NestingTooDeep,
                    format!("bracket nesting exceeds the parser limit of {MAX_NEST}"),
                )
                .with_primary(
                    Span::new(file, i as u32, i as u32 + 1),
                    format!("`{}` nested here", ch as char),
                ),
            );
        }
        self.stack.push(Frame::Open(ch as char, i as u32, snap));
        self.opens += 1;
        Pushed::Ok
    }

    /// Reports a closing bracket against the stack top; answers the frame
    /// it popped, if any, so the caller restores the arrow-run snapshot.
    fn close(
        &mut self,
        file: FileId,
        ch: u8,
        i: usize,
        diags: &mut Vec<Diagnostic>,
    ) -> Option<Frame> {
        let partner = match ch {
            b')' => '(',
            b']' => '[',
            _ => '{',
        };
        match self.top() {
            Some(Frame::Open(top, ..)) if top == partner => self.pop(),
            Some(Frame::Open(top, open, _)) => {
                diags.push(
                    Diagnostic::error(
                        ErrorCode::UnbalancedDelimiter,
                        format!("mismatched pairing: `{top}` closed by `{}`", ch as char),
                    )
                    .with_primary(
                        Span::new(file, i as u32, i as u32 + 1),
                        format!("unexpected `{}`", ch as char),
                    )
                    .with_secondary(
                        Span::new(file, open, open + 1),
                        format!("`{top}` opens here"),
                    )
                    .with_suggestion(Suggestion::Replace {
                        span: Span::new(file, i as u32, i as u32 + 1),
                        with: closing_of(top).to_string(),
                    }),
                );
                // Pop the mismatched opener so later closes re-sync.
                self.pop()
            },
            _ => {
                diags.push(
                    Diagnostic::error(
                        ErrorCode::UnbalancedDelimiter,
                        format!("stray `{}`", ch as char),
                    )
                    .with_primary(
                        Span::new(file, i as u32, i as u32 + 1),
                        "does not close anything",
                    ),
                );
                None
            },
        }
    }
}

/// Scans `src`; returns diagnostics if anything is lexically broken.
pub(crate) fn scan(file: FileId, src: &str) -> Option<Vec<Diagnostic>> {
    // A leading BOM is invisible in editors and no token in the grammar,
    // and the scan dispatch itself never reacts to its bytes, so it is
    // refused by name up front instead of leaving the grammar to fail at
    // it with a message about an invisible character.
    if src.starts_with('\u{feff}') {
        return Some(vec![leading_bom(file)]);
    }
    let b = src.as_bytes();
    let mut diags = Vec::new();
    let mut frames = Frames::new();
    let mut arrows = ArrowBudget::new();
    let mut i = 0;

    while i < b.len() {
        // Skip the run of bytes the current context does not react to; the
        // dispatch below then sees exactly one reaction byte. The plain run
        // of code and hole context runs the arrow budget on the way.
        i = skip_to_reaction(&mut frames, b, i, &mut arrows, file);
        if let Some(d) = arrows.take_refusal() {
            return Some(vec![d]);
        }
        if i >= b.len() {
            break;
        }
        i = match dispatch(&mut frames, &mut arrows, file, b, i, &mut diags) {
            Ok(next) => next,
            // A depth refusal ends the scan with only itself.
            Err(stop) => return Some(stop),
        };
        // An else charge can refuse during dispatch; catch it before any
        // later charge could overwrite the span.
        if let Some(d) = arrows.take_refusal() {
            return Some(vec![d]);
        }
    }

    report_unclosed(file, src, &frames.stack, &mut diags);
    (!diags.is_empty()).then_some(diags)
}

/// Reacts to the one reaction byte at `i` in the current context; answers
/// the next index, or the scan-ending diagnostics.
fn dispatch(
    frames: &mut Frames,
    arrows: &mut ArrowBudget,
    file: FileId,
    b: &[u8],
    i: usize,
    diags: &mut Vec<Diagnostic>,
) -> Result<usize, Vec<Diagnostic>> {
    let mut i = i;
    let prev = if i > 0 { Some(b[i - 1]) } else { None };
    let at = |n: usize| -> u8 { b.get(i + n).copied().unwrap_or(0) };
    let ident_ish = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
    match frames.top() {
        Some(Frame::Str(open)) => match b[i] {
            b'\n' => {
                diags.push(unterminated_string(file, open, i as u32, None));
                frames.pop();
            },
            b'\r' if at(1) != b'\n' => {
                diags.push(lone_cr(file, i));
                i += 1;
            },
            b'\\' => {
                if let Some(step) = escape_len(b, i) {
                    i += step;
                } else {
                    diags.push(
                        Diagnostic::error(
                            ErrorCode::InvalidEscape,
                            "invalid escape sequence in string literal",
                        )
                        .with_primary(
                            Span::new(file, i as u32, (i + 2).min(b.len()) as u32),
                            "invalid escape",
                        ),
                    );
                    i += 1;
                }
            },
            b'"' => {
                frames.pop();
                arrows.after_literal();
                i += 1;
            },
            b'$' => {
                if at(1) == b'{' {
                    frames.push(Frame::Hole(i as u32, arrows.snap()));
                    arrows.after_open();
                    i += 2;
                } else if ident_ish(at(1)) && !at(1).is_ascii_digit() {
                    i += 2;
                    while i < b.len() && ident_ish(b[i]) {
                        i += 1;
                    }
                } else {
                    diags.push(
                        Diagnostic::error(
                            ErrorCode::InvalidInterpolation,
                            "interpolation hole must be `$name` or `${expr}`",
                        )
                        .with_primary(Span::new(file, i as u32, i as u32 + 1), "`$` starts a hole"),
                    );
                    i += 1;
                }
            },
            _ => i += 1,
        },
        Some(Frame::Char(open)) => match b[i] {
            b'\n' => {
                diags.push(
                    Diagnostic::error(
                        ErrorCode::UnterminatedString,
                        "unterminated character literal",
                    )
                    .with_primary(Span::new(file, i as u32, i as u32 + 1), "line ended")
                    .with_secondary(Span::new(file, open, open + 1), "literal starts here"),
                );
                frames.pop();
            },
            b'\\' => {
                if let Some(step) = escape_len(b, i) {
                    i += step;
                } else {
                    diags.push(
                        Diagnostic::error(
                            ErrorCode::InvalidEscape,
                            "invalid escape sequence in character literal",
                        )
                        .with_primary(
                            Span::new(file, i as u32, (i + 2).min(b.len()) as u32),
                            "invalid escape",
                        ),
                    );
                    i += 1;
                }
            },
            b'\r' if at(1) != b'\n' => {
                diags.push(lone_cr(file, i));
                i += 1;
            },
            b'\'' => {
                frames.pop();
                arrows.after_literal();
                i += 1;
            },
            _ => i += 1,
        },
        Some(Frame::Hole(..)) => match b[i] {
            b'"' => {
                frames.push(Frame::Str(i as u32));
                i += 1;
            },
            b'\'' => {
                frames.push(Frame::Char(i as u32));
                i += 1;
            },
            b'r' if boundary_prev(prev) && (at(1) == b'"' || at(1) == b'#') => {
                let mut k = 0;
                while at(1 + k) == b'#' {
                    k += 1;
                }
                frames.push(Frame::Raw(k, i as u32));
                // Skip `r`, the hashes, and the opening quote.
                i += 1 + k + 1;
            },
            b'b' if boundary_prev(prev) && at(1) == b'\'' => {
                frames.push(Frame::Char(i as u32 + 1));
                // Skip `b` and the opening quote.
                i += 2;
            },
            b'/' if at(1) == b'/' => {
                frames.push(Frame::LineComment);
                i += 2;
            },
            b'/' if at(1) == b'*' => {
                frames.push(Frame::BlockComment);
                i += 2;
            },
            b'{' | b'(' | b'[' => match frames.push_open(file, b[i], i, arrows.snap()) {
                Pushed::Ok => {
                    arrows.after_open();
                    i += 1;
                },
                Pushed::TooDeep(d) => return Err(vec![d]),
            },
            b'}' => {
                // Top of stack is the hole (an open `{` would have matched
                // the Open arm below), so this closes the hole.
                if let Some(Frame::Hole(_, snap)) = frames.pop() {
                    arrows.restore(snap);
                }
                arrows.after_operand_close();
                i += 1;
            },
            b')' | b']' => {
                let popped = frames.close(file, b[i], i, diags);
                if let Some(Frame::Open(_, _, snap)) = popped {
                    arrows.restore(snap);
                }
                arrows.after_operand_close();
                i += 1;
            },
            b';' => {
                diags.push(stray_semicolon(file, i));
                i += 1;
            },
            b'$' | b'`' | b'\\' => {
                diags.push(invalid_char(file, i));
                i += 1;
            },
            b'\r' if at(1) != b'\n' => {
                diags.push(lone_cr(file, i));
                i += 1;
            },
            _ => {
                arrows.dispatch_byte(b[i]);
                i += 1;
            },
        },
        Some(Frame::Open(..)) | None => match b[i] {
            b'/' if at(1) == b'/' => {
                frames.push(Frame::LineComment);
                i += 2;
            },
            b'/' if at(1) == b'*' => {
                frames.push(Frame::BlockComment);
                i += 2;
            },
            b'"' => {
                frames.push(Frame::Str(i as u32));
                i += 1;
            },
            b'\'' => {
                frames.push(Frame::Char(i as u32));
                i += 1;
            },
            b'r' if boundary_prev(prev) && (at(1) == b'"' || at(1) == b'#') => {
                let mut k = 0;
                while at(1 + k) == b'#' {
                    k += 1;
                }
                frames.push(Frame::Raw(k, i as u32));
                // Skip `r`, the hashes, and the opening quote.
                i += 1 + k + 1;
            },
            b'b' if boundary_prev(prev) && at(1) == b'\'' => {
                frames.push(Frame::Char(i as u32 + 1));
                // Skip `b` and the opening quote.
                i += 2;
            },
            b'{' | b'(' | b'[' => match frames.push_open(file, b[i], i, arrows.snap()) {
                Pushed::Ok => {
                    arrows.after_open();
                    i += 1;
                },
                Pushed::TooDeep(d) => return Err(vec![d]),
            },
            b')' | b']' | b'}' => {
                let popped = frames.close(file, b[i], i, diags);
                if let Some(Frame::Open(_, _, snap)) = popped {
                    arrows.restore(snap);
                }
                if b[i] == b'}' && matches!(popped, Some(Frame::Open('{', _, _))) {
                    arrows.after_block_close(b, i + 1, file);
                } else {
                    arrows.after_operand_close();
                }
                i += 1;
            },
            b';' => {
                diags.push(stray_semicolon(file, i));
                i += 1;
            },
            b'$' | b'`' | b'\\' => {
                diags.push(invalid_char(file, i));
                i += 1;
            },
            b'#' if frames.stack.is_empty()
                && at(1) == b'#'
                && matches!(prev, None | Some(b'\n')) =>
            {
                // A line-initial `##` opens a separator line (G3); the
                // rest of the line is separator text, invisible here.
                i = match memchr::memchr(b'\n', &b[i..]) {
                    Some(p) => i + p + 1,
                    None => b.len(),
                };
                arrows.after_separator();
            },
            b'#' if at(1) != b'[' => {
                diags.push(
                    Diagnostic::error(
                        ErrorCode::InvalidChar,
                        "`#` only starts an attribute (`#[...]`) or a line-initial separator \
                         (`##`)",
                    )
                    .with_primary(Span::new(file, i as u32, i as u32 + 1), "invalid character"),
                );
                i += 1;
            },
            b'\r' if at(1) != b'\n' => {
                diags.push(lone_cr(file, i));
                i += 1;
            },
            _ => {
                arrows.dispatch_byte(b[i]);
                i += 1;
            },
        },
        // The comment and raw-string contexts consume their whole bodies
        // in the skip phase above.
        Some(Frame::LineComment | Frame::BlockComment | Frame::Raw(..)) => i += 1,
    }
    Ok(i)
}

/// Advances to the next byte the current context reacts to, consuming whole
/// comment and raw-string bodies on the way. The plain run of code and hole
/// context runs the arrow budget; a line comment's terminating newline is
/// spacing and runs the reset rule with the tail that preceded the comment.
fn skip_to_reaction(
    frames: &mut Frames,
    b: &[u8],
    i: usize,
    arrows: &mut ArrowBudget,
    file: FileId,
) -> usize {
    match frames.top() {
        Some(Frame::LineComment) => match memchr::memchr(b'\n', &b[i..]) {
            Some(p) => {
                frames.pop();
                arrows.newline();
                i + p + 1
            },
            None => b.len(),
        },
        Some(Frame::BlockComment) => end_of_block_comment(frames, b, i),
        Some(Frame::Raw(hashes, _)) => end_of_raw(frames, b, i, hashes, arrows),
        Some(Frame::Str(_) | Frame::Char(_)) => byte_class::skip(b, i, byte_class::QUOTED),
        Some(Frame::Hole(..) | Frame::Open(..)) | None => scan_plain(b, i, arrows, file),
    }
}

/// Walks the plain run of code or hole context — every byte the dispatch
/// does not react to, including identifiers, operators, commas, and
/// newlines — running the arrow budget over it, and stops at the next
/// reaction byte.
fn scan_plain(b: &[u8], mut i: usize, arrows: &mut ArrowBudget, file: FileId) -> usize {
    while i < b.len() && byte_class::TABLE[b[i] as usize] & byte_class::CODE == 0 {
        arrows.plain_byte(b, i, file);
        if arrows.is_refusing() {
            // The first refusal names the budget-crossing arrow; stop
            // before a later charge overwrites its span.
            break;
        }
        i += 1;
    }
    i
}

/// Advances past a block comment's body, popping the frame once the closing
/// `*/` is consumed; an unterminated comment answers `b.len()`, which
/// [`report_unclosed`] then reports.
fn end_of_block_comment(frames: &mut Frames, b: &[u8], mut i: usize) -> usize {
    loop {
        match memchr::memchr(b'*', &b[i..]) {
            Some(p) if b.get(i + p + 1) == Some(&b'/') => {
                frames.pop();
                return i + p + 2;
            },
            Some(p) => i += p + 1,
            None => return b.len(),
        }
    }
}

/// Advances past a raw string's body, popping the frame at the
/// terminator: a quote followed by at least the opening hash count, the
/// grammar's PUSH/POP rule (POP matches the opening hashes as a prefix,
/// never requiring the count to match exactly). The terminator consumes
/// the quote and exactly the opening count's hashes, leaving surplus
/// hashes in the code stream for the invalid-`#` diagnostic; a quote
/// followed by fewer hashes is body content, and the walk continues from
/// past it -- the answer never lands on a quote for the dispatch to eat
/// unjudged (the raw frame's dispatch arm takes whole body bytes, so a
/// closing quote reaching it would read as content and the string would
/// falsely report unterminated).
fn end_of_raw(
    frames: &mut Frames,
    b: &[u8],
    i: usize,
    hashes: usize,
    arrows: &mut ArrowBudget,
) -> usize {
    let mut i = i;
    while let Some(p) = memchr::memchr(b'"', &b[i..]) {
        let mut k = 0;
        while b.get(i + p + 1 + k) == Some(&b'#') {
            k += 1;
        }
        if k >= hashes {
            frames.pop();
            arrows.after_literal();
            return i + p + 1 + hashes;
        }
        i += p + 1 + k;
    }
    b.len()
}

/// Reports every frame still open at EOF. The innermost terminator frame
/// (string, hole, comment) wins; otherwise every unclosed opener is
/// reported. Never panics: impossible frames fall through silently.
fn report_unclosed(file: FileId, src: &str, stack: &[Frame], diags: &mut Vec<Diagnostic>) {
    let eof = src.len() as u32;
    if let Some(frame) = stack
        .iter()
        .rev()
        .find(|f| !matches!(f, Frame::Open(..) | Frame::LineComment))
    {
        match *frame {
            Frame::Str(open) | Frame::Raw(_, open) => {
                diags.push(unterminated_string(file, open, eof, None));
            },
            Frame::Char(open) => diags.push(
                Diagnostic::error(
                    ErrorCode::UnterminatedString,
                    "unterminated character literal",
                )
                .with_primary(Span::new(file, eof, eof), "end of input")
                .with_secondary(Span::new(file, open, open + 1), "literal starts here"),
            ),
            Frame::BlockComment => diags.push(
                Diagnostic::error(ErrorCode::UnexpectedEof, "unterminated block comment")
                    .with_primary(Span::new(file, eof, eof), "end of input"),
            ),
            Frame::Hole(hole_open, _) => {
                let str_open = stack
                    .iter()
                    .rev()
                    .find_map(|f| match f {
                        Frame::Str(o) => Some(*o),
                        _ => None,
                    })
                    .unwrap_or(hole_open);
                diags.push(unterminated_string(file, str_open, eof, Some(hole_open)));
            },
            Frame::Open(..) | Frame::LineComment => {},
        }
    } else {
        for &frame in stack.iter().rev() {
            if let Frame::Open(ch, open, _) = frame {
                diags.push(
                    Diagnostic::error(ErrorCode::UnbalancedDelimiter, format!("unclosed `{ch}`"))
                        .with_primary(
                            Span::new(file, open, open + 1),
                            format!("`{ch}` opens here"),
                        )
                        .with_secondary(Span::new(file, eof, eof), "input ended here"),
                );
            }
        }
    }
}

fn boundary_prev(prev: Option<u8>) -> bool {
    !matches!(prev, Some(c) if c.is_ascii_alphanumeric() || c == b'_')
}

/// Length of a valid escape sequence starting at `b[i] == '\\'`, if any.
fn escape_len(b: &[u8], i: usize) -> Option<usize> {
    let simple = |c: u8| matches!(c, b'n' | b't' | b'r' | b'0' | b'\\' | b'"' | b'$' | b'\'');
    match b.get(i + 1) {
        Some(&c) if simple(c) => Some(2),
        Some(b'u') if b.get(i + 2) == Some(&b'{') => {
            let mut j = i + 3;
            let mut n = 0;
            while let Some(&c) = b.get(j) {
                if c.is_ascii_hexdigit() {
                    n += 1;
                    j += 1;
                } else {
                    break;
                }
            }
            (n > 0 && n <= 6 && b.get(j) == Some(&b'}')).then_some(j + 1 - i)
        },
        _ => None,
    }
}

fn closing_of(open: char) -> char {
    match open {
        '(' => ')',
        '[' => ']',
        _ => '}',
    }
}

fn unterminated_string(file: FileId, open: u32, end: u32, hole: Option<u32>) -> Diagnostic {
    let mut d = Diagnostic::error(ErrorCode::UnterminatedString, "string literal never closed")
        .with_primary(Span::new(file, end, end), "input ended here")
        .with_secondary(Span::new(file, open, open + 1), "string starts here");
    if let Some(hole_open) = hole {
        d = d
            .with_secondary(
                Span::new(file, hole_open, hole_open + 2),
                "hole `${` opens here",
            )
            .with_note("interpolation hole was never closed");
    }
    d
}

fn invalid_char(file: FileId, i: usize) -> Diagnostic {
    Diagnostic::error(ErrorCode::InvalidChar, "character not valid in any token")
        .with_primary(Span::new(file, i as u32, i as u32 + 1), "invalid character")
}

/// A `\r` not followed by `\n`: rejected in code, in strings and holes, and
/// in char literals alike (raw strings alone may carry it, as multiline
/// data) — the same rule everywhere, so `nl` never splits on a lone `\r`.
fn lone_cr(file: FileId, i: usize) -> Diagnostic {
    Diagnostic::error(
        ErrorCode::InvalidChar,
        "a lone `\\r` is not a line terminator",
    )
    .with_primary(Span::new(file, i as u32, i as u32 + 1), "invalid character")
}

/// The leading UTF-8 byte-order mark: the three bytes are invisible in most
/// editors and part of no token, so the diagnostic names them and offers
/// removal. Stripping the bytes instead would shift every later span against
/// the registered source, so the file is refused, never silently re-keyed.
fn leading_bom(file: FileId) -> Diagnostic {
    let span = Span::new(file, 0, 3);
    Diagnostic::error(
        ErrorCode::InvalidChar,
        "a leading byte-order mark is not part of the source; save the file as UTF-8 without BOM",
    )
    .with_primary(span, "byte-order mark here")
    .with_suggestion(Suggestion::Remove { span })
}

fn stray_semicolon(file: FileId, i: usize) -> Diagnostic {
    Diagnostic::error(
        ErrorCode::UnexpectedToken,
        "semicolons are not part of the surface; statements end at newlines",
    )
    .with_primary(Span::new(file, i as u32, i as u32 + 1), "stray `;`")
    .with_suggestion(Suggestion::Remove {
        span: Span::new(file, i as u32, i as u32 + 1),
    })
}

#[cfg(test)]
mod tests {
    use std::fmt::Write as _;

    use ds_diag::Files;

    use super::*;
    use crate::{
        arrowbudget::{MAX_ARROWS, MAX_ELSES},
        ast::{Expr, ItemKind},
    };

    fn first_diag(src: &str) -> Diagnostic {
        let mut files = Files::new();
        let f = files.add_source("t.ds", src);
        scan(f, src)
            .and_then(|d| d.into_iter().next())
            .expect("a diagnostic")
    }

    #[test]
    fn clean_source_produces_nothing() {
        let src = "const s = \"a ${f(1)} b\"\nconst y = r#\"raw\"#\nprint(s, y)\n";
        let mut files = Files::new();
        let f = files.add_source("t.ds", src);
        assert!(scan(f, src).is_none());
    }

    #[test]
    fn clean_nested_strings_in_holes() {
        let src = "const s = \"a ${ \"b ${c}\" } d\"\n";
        let mut files = Files::new();
        let f = files.add_source("t.ds", src);
        assert!(scan(f, src).is_none());
    }

    /// Block-comment ends are found by scanning for `*`, so a lone `*` must
    /// resume the scan instead of ending the comment.
    #[test]
    fn block_comment_with_lone_star_closes() {
        let src = "/* a * b */\nconst x = 1\n";
        let mut files = Files::new();
        let f = files.add_source("t.ds", src);
        assert!(scan(f, src).is_none());
    }

    #[test]
    fn line_comment_at_eof_is_clean() {
        let src = "const x = 1 // trailing";
        let mut files = Files::new();
        let f = files.add_source("t.ds", src);
        assert!(scan(f, src).is_none());
    }

    #[test]
    fn separator_line_at_eof_is_clean() {
        let src = "## block two";
        let mut files = Files::new();
        let f = files.add_source("t.ds", src);
        assert!(scan(f, src).is_none());
    }

    #[test]
    fn unterminated_string_is_two_span() {
        let d = first_diag("const s = \"abc");
        assert_eq!(d.code, ErrorCode::UnterminatedString);
        assert_eq!(d.labels.len(), 2);
    }

    #[test]
    fn unclosed_interpolation_names_the_hole() {
        let d = first_diag("\"a ${x");
        assert_eq!(d.code, ErrorCode::UnterminatedString);
        assert_eq!(d.labels.len(), 3);
    }

    #[test]
    fn mismatched_pair_reports_both_spans() {
        let d = first_diag("(a]");
        assert_eq!(d.code, ErrorCode::UnbalancedDelimiter);
        assert_eq!(d.labels.len(), 2);
        assert_eq!(d.suggestions.len(), 1);
    }

    #[test]
    fn deep_nesting_is_rejected_not_crashing() {
        let src = format!("{}x{})", "(".repeat(300), ")".repeat(300));
        let mut files = Files::new();
        let f = files.add_source("t.ds", &src);
        let diags = scan(f, &src).expect("diagnostics");
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].code, ErrorCode::NestingTooDeep);
    }

    /// The depth counter must track popping brackets exactly, so nesting one
    /// past the limit is caught after a balanced prefix.
    #[test]
    fn nesting_limit_counts_only_open_brackets() {
        let balanced = format!("{}1{}\n", "(".repeat(39), ")".repeat(39));
        let mut files = Files::new();
        let f = files.add_source("t.ds", &balanced);
        assert!(scan(f, &balanced).is_none());

        let past = format!(
            "{}(1{})\n",
            "(".repeat(MAX_NEST as usize),
            ")".repeat(MAX_NEST as usize)
        );
        let f = files.add_source("past.ds", &past);
        assert_eq!(
            scan(f, &past).expect("diagnostics")[0].code,
            ErrorCode::NestingTooDeep
        );
    }

    #[test]
    fn semicolon_gets_a_removal_suggestion() {
        let d = first_diag("const a = 1;");
        assert_eq!(d.code, ErrorCode::UnexpectedToken);
        assert_eq!(d.suggestions.len(), 1);
    }

    #[test]
    fn escape_errors_are_positioned() {
        let d = first_diag("\"a\\q\"");
        assert_eq!(d.code, ErrorCode::InvalidEscape);
    }

    /// A quote followed by more hashes than the opening count ends the
    /// string at the opening count (the grammar's POP matches the opening
    /// hashes as a prefix), so the surplus hashes land in the code stream
    /// as the invalid-`#` diagnostic instead of a bogus unterminated-string
    /// report at the opener -- the two passes agree on the string's end.
    #[test]
    fn surplus_closing_hashes_flag_the_hash_not_the_string() {
        let src = "const s = r##\"a\"###\n";
        let d = first_diag(src);
        assert_eq!(d.code, ErrorCode::InvalidChar);
        assert_eq!(d.labels[0].span.start as usize, src.len() - 2);

        // the one-hash spelling the parse-error suite once rode: the
        // string closes at its own `"#`, the surplus `#` answers first
        let d = first_diag("const s = r#\"a\"##b\"#\n");
        assert_eq!(d.code, ErrorCode::InvalidChar);
    }

    /// Escalating the opening count embeds quote-plus-hash text, and both
    /// passes read the same extent: the inner quote and its two hashes are
    /// content, the string closes at the opening count.
    #[test]
    fn escalated_hashes_embed_quote_hash_text() {
        let src = "const s = r###\"\"##\"###\nconst t = 1\n";
        let mut files = Files::new();
        let f = files.add_source("t.ds", src);
        assert!(scan(f, src).is_none());
        let ast = crate::parse(&files, f).expect("the escalated raw string parses");
        let ItemKind::Local(decl) = &ast.blocks[0].items[0].kind else {
            panic!("the const binding stays a binding");
        };
        let Expr::RawStr { hashes, text, .. } = &decl.init else {
            panic!("the bound value is a raw string");
        };
        assert_eq!((*hashes, text.as_str()), (3, "\"##"));
    }

    /// Builders for the arrow-budget tests; every chain sits inside a fn
    /// body so nothing else about the source can refuse.
    fn bare_chain(n: usize) -> String {
        format!("fn f() -> int {{\n  {}1\n}}\n", "x => ".repeat(n))
    }

    fn paren_chain(n: usize) -> String {
        let mut links = String::new();
        for k in 0..n {
            let _ = write!(links, "(a{k}) => ");
        }
        format!("fn f() -> int {{\n  {links}1\n}}\n")
    }

    /// A chain threaded through `else if`: each lambda is the head of the
    /// next if (its body a block, so the if's own block is not swallowed),
    /// so the arrows nest although every block closes.
    fn else_if_chain(n: usize) -> String {
        let mut s = String::from("x0 => if c { 1 }");
        for k in 1..n {
            let _ = write!(s, "\nelse if x{k} => {{ 1 }} {{ 2 }}");
        }
        s.push_str("\nelse { 0 }");
        format!("fn f() -> int {{\n  {s}\n}}\n")
    }

    /// A chain threaded through `in` right-hand sides across newlines.
    fn in_chain(n: usize) -> String {
        let mut s = String::new();
        for k in 0..n {
            let _ = writeln!(s, "x{k} => y{k} in");
        }
        s.push('z');
        format!("fn f() -> bool {{\n  {s}\n}}\n")
    }

    fn hole_chain(n: usize) -> String {
        format!(
            "fn f() -> string {{\n  \"${{{}1}}\"\n}}\n",
            "x => ".repeat(n)
        )
    }

    #[test]
    fn arrow_chain_over_budget_refuses_at_the_budget_arrow() {
        let src = bare_chain(200);
        let mut files = Files::new();
        let f = files.add_source("t.ds", &src);
        let diags = scan(f, &src).expect("diagnostics");
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].code, ErrorCode::ArrowChainTooDeep);
        // The refused arrow is the budget-th, not the first.
        let prefix = "fn f() -> int {\n  ".len();
        let at = prefix + "x => ".len() * (MAX_ARROWS as usize - 1) + "x ".len();
        assert_eq!(
            diags[0].labels[0].span,
            Span::new(f, at as u32, at as u32 + 2)
        );
        assert_eq!(&src[at..at + 2], "=>");
    }

    /// The budget counts arrows, not `=>` occurrences: one below the
    /// budget scans clean, the budget-th refuses.
    #[test]
    fn arrow_boundary_counts_arrows_exactly() {
        let ok = bare_chain(MAX_ARROWS as usize - 1);
        let mut files = Files::new();
        let f = files.add_source("ok.ds", &ok);
        assert!(scan(f, &ok).is_none());

        let past = bare_chain(MAX_ARROWS as usize);
        let f = files.add_source("past.ds", &past);
        assert_eq!(
            scan(f, &past).expect("diagnostics")[0].code,
            ErrorCode::ArrowChainTooDeep
        );
    }

    /// Sibling `=>`s behind repetition separators are not nesting: match
    /// arms (comma and newline separated alike) must never hit the budget.
    #[test]
    fn match_arms_reset_the_arrow_run() {
        for sep in [",\n", "\n"] {
            let mut arms = String::new();
            for k in 0..60 {
                if k > 0 {
                    arms.push_str(sep);
                }
                let _ = write!(arms, "a{k} => {k}");
            }
            let src = format!("fn f(x: int) -> int {{\n  match x {{\n    {arms}\n  }}\n}}\n");
            let mut files = Files::new();
            let f = files.add_source("t.ds", &src);
            assert!(scan(f, &src).is_none(), "arms separated by {sep:?}");
        }
    }

    #[test]
    fn statement_lambdas_reset_the_arrow_run() {
        let mut src = String::new();
        for k in 0..60 {
            let _ = writeln!(src, "const f{k} = x{k} => x{k}");
        }
        let mut files = Files::new();
        let f = files.add_source("t.ds", &src);
        assert!(scan(f, &src).is_none());
    }

    #[test]
    fn call_arguments_reset_the_arrow_run() {
        let mut args = String::new();
        for k in 0..60 {
            if k > 0 {
                args.push_str(", ");
            }
            let _ = write!(args, "x{k} => x{k}");
        }
        let src = format!("fn f() -> int {{\n  g({args})\n}}\n");
        let mut files = Files::new();
        let f = files.add_source("t.ds", &src);
        assert!(scan(f, &src).is_none());
    }

    /// Chains that nest without brackets through other shapes must keep
    /// accumulating: parameter-list lambdas (the commas sit inside the
    /// params, so only the close-restore carries the run), `else if`
    /// heads, and `in` right-hand sides.
    #[test]
    fn paren_lambda_chains_accumulate() {
        let mut files = Files::new();
        let ok = paren_chain(3);
        let f = files.add_source("ok.ds", &ok);
        assert!(scan(f, &ok).is_none());
        let past = paren_chain(60);
        let f = files.add_source("past.ds", &past);
        assert_eq!(
            scan(f, &past).expect("diagnostics")[0].code,
            ErrorCode::ArrowChainTooDeep
        );
    }

    #[test]
    fn else_if_lambda_chains_accumulate() {
        let mut files = Files::new();
        let ok = else_if_chain(2);
        let f = files.add_source("ok.ds", &ok);
        assert!(scan(f, &ok).is_none());
        let past = else_if_chain(60);
        let f = files.add_source("past.ds", &past);
        assert_eq!(
            scan(f, &past).expect("diagnostics")[0].code,
            ErrorCode::ArrowChainTooDeep
        );
    }

    #[test]
    fn in_continuation_chains_accumulate() {
        let mut files = Files::new();
        let ok = in_chain(2);
        let f = files.add_source("ok.ds", &ok);
        assert!(scan(f, &ok).is_none());
        let past = in_chain(60);
        let f = files.add_source("past.ds", &past);
        assert_eq!(
            scan(f, &past).expect("diagnostics")[0].code,
            ErrorCode::ArrowChainTooDeep
        );
    }

    /// A `${...}` hole holds a full expression the bracket guard never
    /// counts, so arrows charge there too.
    #[test]
    fn interpolation_holes_charge_arrows() {
        let mut files = Files::new();
        let ok = hole_chain(3);
        let f = files.add_source("ok.ds", &ok);
        assert!(scan(f, &ok).is_none());
        let past = hole_chain(60);
        let f = files.add_source("past.ds", &past);
        assert_eq!(
            scan(f, &past).expect("diagnostics")[0].code,
            ErrorCode::ArrowChainTooDeep
        );
    }

    /// A plain `else if` dispatch chain carrying `elses` `else` clauses,
    /// one per line, as one statement.
    fn else_chain(elses: usize) -> String {
        let mut s = String::from("if c == 0 { 0 }");
        for k in 1..elses {
            let _ = writeln!(s, "\nelse if c == {k} {{ {k} }}");
        }
        if elses > 0 {
            s.push_str("\nelse { 0 }");
        }
        s
    }

    fn in_fn_body(body: &str) -> String {
        format!("fn f(c: int) -> int {{\n  {body}\n}}\n")
    }

    #[test]
    fn else_if_chain_over_budget_refuses() {
        // A 600-branch chain (599 `else`s).
        let src = in_fn_body(&else_chain(599));
        let mut files = Files::new();
        let f = files.add_source("t.ds", &src);
        let diags = scan(f, &src).expect("diagnostics");
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].code, ErrorCode::ElseChainTooDeep);
        // The primary span names the budget-crossing `else`.
        let span = diags[0].labels[0].span;
        assert_eq!(span.end - span.start, 4);
        assert_eq!(&src[span.start as usize..span.end as usize], "else");
    }

    /// The budget counts `else`s: one below it scans clean (a real
    /// generated decoder's dispatch), the budget-th refuses.
    #[test]
    fn else_if_boundary_counts_elses_exactly() {
        let mut files = Files::new();
        let ok = in_fn_body(&else_chain(MAX_ELSES as usize - 1));
        let f = files.add_source("ok.ds", &ok);
        assert!(scan(f, &ok).is_none());

        let past = in_fn_body(&else_chain(MAX_ELSES as usize));
        let f = files.add_source("past.ds", &past);
        assert_eq!(
            scan(f, &past).expect("diagnostics")[0].code,
            ErrorCode::ElseChainTooDeep
        );
    }

    /// A completed statement ends the chain: two half-budget chains in
    /// one block stay clean because the newline between them resets the
    /// run (598 `else`s total, 299 live at once).
    #[test]
    fn statement_boundary_resets_the_else_run() {
        let mut body = else_chain(299);
        body.push('\n');
        body.push_str(&else_chain(299));
        let src = in_fn_body(&body);
        let mut files = Files::new();
        let f = files.add_source("t.ds", &src);
        assert!(scan(f, &src).is_none());
    }

    /// Brackets inside branch bodies must not lose the charges: the run
    /// rides the block-close snapshots past the paren restore.
    #[test]
    fn paren_contents_do_not_reset_the_else_run() {
        let mut s = String::from("if c == 0 { g(0) }");
        for k in 1..599 {
            let _ = writeln!(s, "\nelse if c == {k} {{ g({k}) }}");
        }
        s.push_str("\nelse { g(0) }");
        let src = in_fn_body(&s);
        let mut files = Files::new();
        let f = files.add_source("t.ds", &src);
        assert_eq!(
            scan(f, &src).expect("diagnostics")[0].code,
            ErrorCode::ElseChainTooDeep
        );
    }

    /// A mixed chain charges both budgets: a lambda-headed prefix keeps
    /// the arrow run live, then plain branches carry the else run to its
    /// budget while the arrow run stays under its own.
    #[test]
    fn mixed_arrow_and_else_chains_accumulate_both() {
        let mut s = String::from("x0 => if c { 1 }");
        for k in 1..10 {
            let _ = writeln!(s, "\nelse if x{k} => {{ 1 }} {{ 2 }}");
        }
        for k in 10..600 {
            let _ = writeln!(s, "\nelse if c == {k} {{ {k} }}");
        }
        s.push_str("\nelse { 0 }");
        let src = in_fn_body(&s);
        let mut files = Files::new();
        let f = files.add_source("t.ds", &src);
        assert_eq!(
            scan(f, &src).expect("diagnostics")[0].code,
            ErrorCode::ElseChainTooDeep
        );
    }

    /// A `${...}` hole holds a full expression, so an `else if` chain
    /// inside one charges like any other.
    #[test]
    fn interpolation_holes_charge_elses() {
        let mut inner = String::from("if c { 1 }");
        for k in 1..600 {
            let _ = write!(inner, " else if c == {k} {{ {k} }}");
        }
        inner.push_str(" else { 0 }");
        let src = format!("fn f(c: bool) -> string {{\n  \"${{{inner}}}\"\n}}\n");
        let mut files = Files::new();
        let f = files.add_source("t.ds", &src);
        assert_eq!(
            scan(f, &src).expect("diagnostics")[0].code,
            ErrorCode::ElseChainTooDeep
        );
    }
}
