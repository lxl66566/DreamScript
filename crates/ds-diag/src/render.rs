//! Human-readable diagnostic rendering: plain text, no color, no UI
//! dependencies.
//!
//! Shape:
//!
//! ```text
//! error[DS3001]: value `h` is no longer usable
//!  --> main.ds:2:5
//!   |
//! 1 | const rows = read(h)
//!   |                    - moved here
//!   |
//! 2 | log(h.path)
//!   |     ^ used again here after move
//!   |
//!   = note: `h` is `~Copyable` and was consumed
//!   = help: replace with `h2`
//! ```

#[cfg(feature = "json")]
pub mod json;

use std::fmt::Write as _;

#[cfg(feature = "json")]
pub use json::{JsonDiagnostic, JsonLoc, JsonSpan, JsonSuggestion, to_json};

use crate::{
    diagnostic::{Diagnostic, LabelStyle, Suggestion},
    files::{FileId, Files, Span, char_distance},
};

/// The last offset inside `span` a location lookup may use: `end - 1` for
/// non-empty spans (locations are point queries, and `end` is one past the
/// last byte), the start itself when empty.
fn last_offset(span: Span) -> u32 {
    if span.is_empty() { span.start } else { span.end - 1 }
}

/// `write_fmt` into a `String`; formatting into a String cannot fail.
macro_rules! emit {
    ($out:expr, $($arg:tt)*) => {
        $out.write_fmt(format_args!($($arg)*)).expect("formatting into a String cannot fail")
    };
}

/// Renders `diagnostic` against `files` as plain text.
///
/// The header points at the first primary label (the first label when
/// none is primary). Each label gets one excerpt block; a label spanning
/// multiple lines gets one caret line per line, with the label text on
/// the first. An excerpt in a file other than the one last shown (starting
/// with the header's) is introduced by its own `-->` location line, so
/// cross-file diagnostics stay readable. Notes and suggestion helps follow
/// under `= note:` / `= help:`.
#[must_use]
pub fn human(diagnostic: &Diagnostic, files: &Files) -> String {
    let mut out = String::new();
    emit!(
        out,
        "{}[{}]: {}\n",
        diagnostic.severity,
        diagnostic.code,
        diagnostic.message
    );

    let focus = diagnostic
        .labels
        .iter()
        .find(|l| l.style == LabelStyle::Primary)
        .or_else(|| diagnostic.labels.first());
    let mut shown_file: Option<FileId> = None;
    if let Some(label) = focus {
        // A span may predate a `set_source` swap; rendering clamps it into
        // the current bounds instead of panicking.
        let span = files.clamp_span(label.span);
        let loc = files.location(span.file, span.start);
        emit!(
            out,
            " --> {}:{}:{}\n",
            files.name(span.file),
            loc.line,
            loc.col
        );
        shown_file = Some(span.file);
    }

    let width = gutter_width(diagnostic, files);
    for label in &diagnostic.labels {
        emit!(out, "{:>width$} |\n", "");
        let span = files.clamp_span(label.span);
        if shown_file != Some(span.file) {
            let loc = files.location(span.file, span.start);
            emit!(
                out,
                "{:>width$} --> {}:{}:{}\n",
                "",
                files.name(span.file),
                loc.line,
                loc.col
            );
        }
        shown_file = Some(span.file);
        let first_line = files.location(span.file, span.start).line;
        let last_line = files.location(span.file, last_offset(span)).line;
        let caret = match label.style {
            LabelStyle::Primary => "^",
            LabelStyle::Secondary => "-",
        };
        for line in first_line..=last_line {
            let text = line_text(files, span.file, line);
            emit!(out, "{line:>width$} |");
            if !text.is_empty() {
                emit!(out, " {text}");
            }
            out.push('\n');
            let range = files.line_span(span.file, line);
            let clip_start = span.start.max(range.start);
            let clip_end = span.end.min(range.end).max(clip_start);
            let source = files.source(span.file);
            let pad = char_distance(source, range.start, clip_start);
            let carets = char_distance(source, clip_start, clip_end).max(1);
            emit!(
                out,
                "{:>width$} | {}{}",
                "",
                " ".repeat(pad),
                caret.repeat(carets)
            );
            if line == first_line && !label.label.is_empty() {
                emit!(out, " {}", label.label);
            }
            out.push('\n');
        }
    }

    if !diagnostic.notes.is_empty() || !diagnostic.suggestions.is_empty() {
        emit!(out, "{:>width$} |\n", "");
        for note in &diagnostic.notes {
            emit!(out, "{:>width$} = note: {note}\n", "");
        }
        for suggestion in &diagnostic.suggestions {
            emit!(
                out,
                "{:>width$} = help: {}\n",
                "",
                suggestion_help(suggestion)
            );
        }
    }
    out
}

/// Decimal width of the largest line number any label shows, minimum 1.
fn gutter_width(diagnostic: &Diagnostic, files: &Files) -> usize {
    let mut max_line = 0;
    for label in &diagnostic.labels {
        let span = files.clamp_span(label.span);
        max_line = max_line
            .max(files.location(span.file, span.start).line)
            .max(files.location(span.file, last_offset(span)).line);
    }
    max_line.to_string().len().max(1)
}

/// Line text without its terminator.
fn line_text(files: &Files, file: FileId, line: u32) -> &str {
    let range = files.line_span(file, line);
    &files.source(file)[range.start as usize..range.end as usize]
}

/// One-line human phrasing of a suggestion.
fn suggestion_help(suggestion: &Suggestion) -> String {
    match suggestion {
        Suggestion::Replace { with, .. } => format!("replace with `{with}`"),
        Suggestion::Insert { text, .. } => format!("insert `{text}`"),
        Suggestion::Remove { .. } => "remove this".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        code::ErrorCode,
        diagnostic::Severity,
        files::{FileId, Span},
    };

    /// The classic two-span linear error: secondary at the move, primary at
    /// the later use.
    #[test]
    fn two_span_linear_error_golden() {
        let mut files = Files::new();
        let file = files.add_source("main.ds", "const rows = read(h)\nlog(h.path)\n");
        let diag = Diagnostic::error(ErrorCode::MovedThenUsed, "value `h` is no longer usable")
            .with_secondary(Span::new(file, 18, 19), "moved here")
            .with_primary(Span::new(file, 25, 26), "used again here after move")
            .with_note("`h` is `~Copyable` and was consumed")
            .with_suggestion(Suggestion::Replace {
                span: Span::new(file, 25, 26),
                with: "h2".to_owned(),
            });
        let expected = "\
error[DS3001]: value `h` is no longer usable
 --> main.ds:2:5
  |
1 | const rows = read(h)
  |                   - moved here
  |
2 | log(h.path)
  |     ^ used again here after move
  |
  = note: `h` is `~Copyable` and was consumed
  = help: replace with `h2`
";
        assert_eq!(human(&diag, &files), expected);
    }

    #[test]
    fn multi_line_span_gets_one_caret_line_per_line() {
        let mut files = Files::new();
        let src = "fn f() {\n  foo(\n    1,\n  )\n}\n";
        let file = files.add_source("t.ds", src);
        let start = u32::try_from(src.find("foo").unwrap()).unwrap();
        let end = u32::try_from(src.rfind(')').unwrap() + 1).unwrap();
        let diag = Diagnostic::error(ErrorCode::UnexpectedToken, "multi-line span rendering")
            .with_primary(Span::new(file, start, end), "multi-line expression");
        let expected = "\
error[DS1001]: multi-line span rendering
 --> t.ds:2:3
  |
2 |   foo(
  |   ^^^^ multi-line expression
3 |     1,
  | ^^^^^^
4 |   )
  | ^^^
";
        assert_eq!(human(&diag, &files), expected);
    }

    /// Empty file, zero-width span at EOF: line 1 col 1, empty source line.
    #[test]
    fn empty_file_eof_span_golden() {
        let mut files = Files::new();
        let file = files.add_source("empty.ds", "");
        let diag = Diagnostic::error(ErrorCode::UnexpectedEof, "unexpected end of file")
            .with_primary(Span::new(file, 0, 0), "here");
        let expected = "\
error[DS1002]: unexpected end of file
 --> empty.ds:1:1
  |
1 |
  | ^ here
";
        assert_eq!(human(&diag, &files), expected);
    }

    /// Spans held across a `set_source` swap address the file's previous
    /// content; rendering clamps them into the current bounds instead of
    /// panicking in the display layer.
    #[test]
    fn stale_span_renders_clamped_without_panicking() {
        let mut files = Files::new();
        let file = files.add_source("main.ds", "const rows = read(h)\nlog(h.path)\n");
        let diag = Diagnostic::error(ErrorCode::MovedThenUsed, "value `h` is no longer usable")
            .with_secondary(Span::new(file, 18, 19), "moved here")
            .with_primary(Span::new(file, 25, 26), "used again here after move");
        files.set_source(file, "short");
        // Both spans clamp to the end of the new source (offset 5).
        let expected = "\
error[DS3001]: value `h` is no longer usable
 --> main.ds:1:6
  |
1 | short
  |      - moved here
  |
1 | short
  |      ^ used again here after move
";
        assert_eq!(human(&diag, &files), expected);
        #[cfg(feature = "json")]
        assert_eq!(
            to_json(&diag, &files),
            r#"{"code":"DS3001","severity":"error","message":"value `h` is no longer usable","spans":[{"file":"main.ds","start_line":1,"start_col":6,"end_line":1,"end_col":6,"label":"moved here","primary":false},{"file":"main.ds","start_line":1,"start_col":6,"end_line":1,"end_col":6,"label":"used again here after move","primary":true}],"notes":[],"suggestions":[]}"#
        );
    }

    /// A diagnostic whose labels sit in two files: the header names the
    /// primary's file, and every excerpt in a different file than the one
    /// before it carries its own location line (line numbers restart there,
    /// so the file must be re-announced on every switch).
    #[test]
    fn cross_file_excerpt_names_its_file() {
        let mut files = Files::new();
        let main = files.add_source("main.ds", "log(h.path)\n");
        let handle = files.add_source("handle.ds", "const rows = read(h)\nfn take(x: file)\n");
        let h_at = |file: FileId, off: u32| Span::new(file, off, off + 1);
        let diag = Diagnostic::error(ErrorCode::MovedThenUsed, "value `h` is no longer usable")
            .with_secondary(h_at(handle, 18), "moved here")
            .with_primary(h_at(main, 4), "used again here after move")
            .with_secondary(h_at(handle, 32), "taken here");
        let expected = "\
error[DS3001]: value `h` is no longer usable
 --> main.ds:1:5
  |
  --> handle.ds:1:19
1 | const rows = read(h)
  |                   - moved here
  |
  --> main.ds:1:5
1 | log(h.path)
  |     ^ used again here after move
  |
  --> handle.ds:2:12
2 | fn take(x: file)
  |            - taken here
";
        assert_eq!(human(&diag, &files), expected);
    }

    /// No labels: no arrow, no excerpt; minimum-1 gutter for notes.
    #[test]
    fn labelless_diagnostic_golden() {
        let files = Files::new();
        let diag = Diagnostic::warning(ErrorCode::UnterminatedString, "quote me")
            .with_note("no spans attached");
        let expected = "\
warning[DS1004]: quote me
  |
  = note: no spans attached
";
        assert_eq!(human(&diag, &files), expected);
        assert_eq!(diag.severity, Severity::Warning);
    }
}
