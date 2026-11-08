//! Diagnostic values: severity, code, message, labeled spans, notes, and
//! structured suggestions.

use core::fmt;

use crate::{code::ErrorCode, files::Span};

/// Diagnostic severity.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Severity {
    /// Compilation cannot proceed.
    Error,
    /// Compilation can proceed, but the code is suspect.
    Warning,
}

impl fmt::Display for Severity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Error => "error",
            Self::Warning => "warning",
        })
    }
}

/// Whether a labeled span is the diagnostic's primary location or
/// supporting context.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum LabelStyle {
    /// The location the header points at; rendered with `^`.
    Primary,
    /// Supporting context; rendered with `-`.
    Secondary,
}

/// A span plus its role and its explanatory text.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct LabeledSpan {
    /// The covered source range.
    pub span: Span,
    /// Primary or secondary role.
    pub style: LabelStyle,
    /// Human-readable text attached to the span; may be empty.
    pub label: String,
}

impl LabeledSpan {
    /// A primary labeled span.
    #[must_use]
    pub fn primary(span: Span, label: impl Into<String>) -> Self {
        Self {
            span,
            style: LabelStyle::Primary,
            label: label.into(),
        }
    }

    /// A secondary labeled span.
    #[must_use]
    pub fn secondary(span: Span, label: impl Into<String>) -> Self {
        Self {
            span,
            style: LabelStyle::Secondary,
            label: label.into(),
        }
    }
}

/// A structured, machine-readable fix suggestion.
///
/// Suggestions describe an edit, not a prose hint; prose belongs in
/// [`Diagnostic::with_note`].
#[non_exhaustive]
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Suggestion {
    /// Replace the source covered by `span` with `with`.
    Replace {
        /// The range to overwrite.
        span: Span,
        /// The replacement text.
        with: String,
    },
    /// Insert `text` at `span`, an empty position span.
    Insert {
        /// The zero-width position to insert at.
        span: Span,
        /// The text to insert.
        text: String,
    },
    /// Delete the source covered by `span`.
    Remove {
        /// The range to delete.
        span: Span,
    },
}

impl Suggestion {
    /// The span the suggestion edits.
    #[must_use]
    pub fn span(&self) -> Span {
        match self {
            Self::Replace { span, .. } | Self::Insert { span, .. } | Self::Remove { span } => *span,
        }
    }

    /// The text written into the source; empty for [`Suggestion::Remove`].
    #[must_use]
    pub fn text(&self) -> &str {
        match self {
            Self::Replace { with, .. } => with,
            Self::Insert { text, .. } => text,
            Self::Remove { .. } => "",
        }
    }
}

/// One reported problem: what happened, where, and how to fix it.
///
/// Labels support the two-span linear pattern: a secondary span at the move
/// ("moved here") and a primary span at the later use ("used again here").
#[non_exhaustive]
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Diagnostic {
    /// How severe the problem is.
    pub severity: Severity,
    /// Stable registry code.
    pub code: ErrorCode,
    /// One-line summary.
    pub message: String,
    /// Labeled source spans, rendered in order.
    pub labels: Vec<LabeledSpan>,
    /// Prose remarks after the excerpts.
    pub notes: Vec<String>,
    /// Machine-readable fix suggestions.
    pub suggestions: Vec<Suggestion>,
}

impl Diagnostic {
    /// Creates a diagnostic with no labels, notes, or suggestions yet.
    #[must_use]
    pub fn new(severity: Severity, code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            severity,
            code,
            message: message.into(),
            labels: Vec::new(),
            notes: Vec::new(),
            suggestions: Vec::new(),
        }
    }

    /// Shorthand for [`Diagnostic::new`] with severity error.
    #[must_use]
    pub fn error(code: ErrorCode, message: impl Into<String>) -> Self {
        Self::new(Severity::Error, code, message)
    }

    /// Shorthand for [`Diagnostic::new`] with severity warning.
    #[must_use]
    pub fn warning(code: ErrorCode, message: impl Into<String>) -> Self {
        Self::new(Severity::Warning, code, message)
    }

    /// Appends an already-built label.
    #[must_use]
    pub fn with_label(mut self, label: LabeledSpan) -> Self {
        self.labels.push(label);
        self
    }

    /// Appends a primary label at `span`.
    #[must_use]
    pub fn with_primary(self, span: Span, label: impl Into<String>) -> Self {
        self.with_label(LabeledSpan::primary(span, label))
    }

    /// Appends a secondary label at `span`.
    #[must_use]
    pub fn with_secondary(self, span: Span, label: impl Into<String>) -> Self {
        self.with_label(LabeledSpan::secondary(span, label))
    }

    /// Appends a prose note.
    #[must_use]
    pub fn with_note(mut self, note: impl Into<String>) -> Self {
        self.notes.push(note.into());
        self
    }

    /// Appends a structured suggestion.
    #[must_use]
    pub fn with_suggestion(mut self, suggestion: Suggestion) -> Self {
        self.suggestions.push(suggestion);
        self
    }
}
