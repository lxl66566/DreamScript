//! JSON rendering for `--error-format=json`.
//!
//! The schema is a stable contract: field names and shapes never change;
//! fields are only added, never renamed or removed. Positions are 1-based
//! line/column pairs resolved against the [`Files`] registry; `end_col` is
//! the column of the first byte after the span.

use serde::Serialize;

use crate::{
    diagnostic::{Diagnostic, LabelStyle, Severity, Suggestion},
    files::{Files, Span},
};

/// One resolved location range: file plus 1-based start/end line/column.
#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub struct JsonLoc {
    /// File name as registered.
    pub file: String,
    /// 1-based line of the start offset.
    pub start_line: u32,
    /// 1-based column of the start offset.
    pub start_col: u32,
    /// 1-based line of the exclusive end offset.
    pub end_line: u32,
    /// 1-based column of the exclusive end offset.
    pub end_col: u32,
}

/// One entry of the `spans` array: a flat location plus label and role.
#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub struct JsonSpan {
    /// Location range, flattened into this object.
    #[serde(flatten)]
    pub loc: JsonLoc,
    /// The span's explanatory text; may be empty.
    pub label: String,
    /// Whether the span is the diagnostic's primary location.
    pub primary: bool,
}

/// One entry of the `suggestions` array.
#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub struct JsonSuggestion {
    /// Edit shape: `replace`, `insert`, or `remove`.
    pub kind: &'static str,
    /// Location the edit applies to.
    pub span: JsonLoc,
    /// Text written into the source; empty for `remove`.
    pub text: String,
}

/// Stable JSON shape of one diagnostic.
#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub struct JsonDiagnostic<'a> {
    /// Stable code string, e.g. `"DS3001"`.
    pub code: &'a str,
    /// `error` or `warning`.
    pub severity: &'a str,
    /// One-line summary.
    pub message: &'a str,
    /// Labeled spans, in diagnostic order.
    pub spans: Vec<JsonSpan>,
    /// Prose notes.
    pub notes: &'a [String],
    /// Structured suggestions.
    pub suggestions: Vec<JsonSuggestion>,
}

impl<'a> JsonDiagnostic<'a> {
    /// Resolves `diagnostic` against `files` for serialization.
    #[must_use]
    pub fn new(diagnostic: &'a Diagnostic, files: &Files) -> Self {
        Self {
            code: diagnostic.code.as_str(),
            severity: match diagnostic.severity {
                Severity::Error => "error",
                Severity::Warning => "warning",
            },
            message: &diagnostic.message,
            spans: diagnostic
                .labels
                .iter()
                .map(|l| JsonSpan {
                    loc: json_loc(files, l.span),
                    label: l.label.clone(),
                    primary: l.style == LabelStyle::Primary,
                })
                .collect(),
            notes: &diagnostic.notes,
            suggestions: diagnostic
                .suggestions
                .iter()
                .map(|s| JsonSuggestion {
                    kind: match s {
                        Suggestion::Replace { .. } => "replace",
                        Suggestion::Insert { .. } => "insert",
                        Suggestion::Remove { .. } => "remove",
                    },
                    span: json_loc(files, s.span()),
                    text: s.text().to_owned(),
                })
                .collect(),
        }
    }
}

fn json_loc(files: &Files, span: Span) -> JsonLoc {
    // A span may predate a `set_source` swap; rendering clamps it into the
    // current bounds instead of panicking.
    let span = files.clamp_span(span);
    let start = files.location(span.file, span.start);
    let end = files.location(span.file, span.end);
    JsonLoc {
        file: files.name(span.file).to_owned(),
        start_line: start.line,
        start_col: start.col,
        end_line: end.line,
        end_col: end.col,
    }
}

/// Serializes `diagnostic` to one compact JSON object, the unit
/// `--error-format=json` prints per diagnostic.
#[must_use]
pub fn to_json(diagnostic: &Diagnostic, files: &Files) -> String {
    serde_json::to_string(&JsonDiagnostic::new(diagnostic, files))
        .expect("serializing a diagnostic cannot fail")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{code::ErrorCode, files::Span};

    /// Same diagnostic as the human golden: pins schema, field order, and
    /// the two-span shape.
    #[test]
    fn two_span_json_golden() {
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
        let expected = r#"{"code":"DS3001","severity":"error","message":"value `h` is no longer usable","spans":[{"file":"main.ds","start_line":1,"start_col":19,"end_line":1,"end_col":20,"label":"moved here","primary":false},{"file":"main.ds","start_line":2,"start_col":5,"end_line":2,"end_col":6,"label":"used again here after move","primary":true}],"notes":["`h` is `~Copyable` and was consumed"],"suggestions":[{"kind":"replace","span":{"file":"main.ds","start_line":2,"start_col":5,"end_line":2,"end_col":6},"text":"h2"}]}"#;
        assert_eq!(to_json(&diag, &files), expected);
    }

    #[test]
    fn empty_sections_stay_arrays() {
        let files = Files::new();
        let diag = Diagnostic::warning(ErrorCode::UnterminatedString, "quote me");
        assert_eq!(
            to_json(&diag, &files),
            r#"{"code":"DS1004","severity":"warning","message":"quote me","spans":[],"notes":[],"suggestions":[]}"#
        );
    }
}
