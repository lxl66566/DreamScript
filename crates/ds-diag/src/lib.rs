//! Diagnostic infrastructure for DreamScript.
//!
//! Every compiler crate reports problems through this crate: a [`Span`] over
//! a source registered in [`Files`], a stable [`ErrorCode`], and a
//! [`Diagnostic`] carrying labeled spans, notes, and structured suggestions.
//! Two renderers turn diagnostics into text: [`render::human`] for
//! terminals and logs, and `render::to_json` (behind the `json` feature)
//! for `--error-format=json`.
//!
//! # Two-span linear errors
//!
//! The linear diagnostic class renders "moved here / used again here" pairs
//! with one secondary label at the move and one primary label at the later
//! use:
//!
//! ```
//! use ds_diag::{Diagnostic, ErrorCode, Files, Severity, Span, Suggestion, render};
//!
//! let mut files = Files::new();
//! let file = files.add_source("main.ds", "const rows = read(h)\nlog(h.path)\n");
//!
//! let diagnostic = Diagnostic::new(
//!     Severity::Error,
//!     ErrorCode::MovedThenUsed,
//!     "value `h` is no longer usable",
//! )
//! .with_secondary(Span::new(file, 18, 19), "moved here")
//! .with_primary(Span::new(file, 25, 26), "used again here after move")
//! .with_note("`h` is `~Copyable` and was consumed")
//! .with_suggestion(Suggestion::Replace {
//!     span: Span::new(file, 25, 26),
//!     with: "h2".to_owned(),
//! });
//!
//! let text = render::human(&diagnostic, &files);
//! assert!(text.contains(" --> main.ds:2:5"));
//! assert!(text.contains("^ used again here after move"));
//!
//! #[cfg(feature = "json")]
//! {
//!     let json = render::to_json(&diagnostic, &files);
//!     assert!(json.starts_with(r#"{"code":"DS3001""#));
//! }
//! ```

#![warn(missing_docs)]

pub mod code;
pub mod diagnostic;
pub mod files;
pub mod render;

pub use code::{ErrorClass, ErrorCode};
pub use diagnostic::{Diagnostic, LabelStyle, LabeledSpan, Severity, Suggestion};
pub use files::{FileId, FileSpans, Files, Location, Span, line_starts_of};
pub use render::human;
#[cfg(feature = "json")]
pub use render::to_json;
