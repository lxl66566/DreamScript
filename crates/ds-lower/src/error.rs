//! Lowering errors: constructs the runtime contract does not yet cover.

use ds_diag::Span;

/// Why a checked package cannot be lowered under the current runtime
/// contract. Every variant names the surface seam that would cover it;
/// none of them is a checker gap — the package checked clean first.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum LowerError {
    /// A construct with no runtime contract yet [BL-2, BL-5].
    #[error(
        "{what} is outside the M3 runtime contract ({reason}); the checker accepted it, the uop \
         set does not cover it yet"
    )]
    Unsupported {
        /// Span of the offending node, for diagnostics.
        span: Span,
        /// What the node is.
        what: &'static str,
        /// Which seam the construct needs.
        reason: &'static str,
    },
    /// No lowerable `main` entry: the module needs one.
    #[error("no lowerable `main` entry in the package")]
    NoMain,
    /// The package still carries checker diagnostics.
    #[error("the package carries {count} checker diagnostics; lower only clean packages")]
    NotChecked { count: usize },
    /// A function record was rejected by the ds-mod builder.
    #[error("the function record was rejected: {0}")]
    Build(String),
    /// A frame needs more than the 256-slot cap [BL-5].
    #[error("the frame of `{name}` needs {need} slots, past the 256-slot cap")]
    FrameTooWide {
        /// The function's name.
        name: String,
        /// Words the frame would need.
        need: usize,
    },
}

impl LowerError {
    /// The offending node's span, for diagnostics; `None` for the
    /// package-level errors.
    #[must_use]
    pub fn span(&self) -> Option<Span> {
        match self {
            Self::Unsupported { span, .. } => Some(*span),
            _ => None,
        }
    }

    /// An [`LowerError::Unsupported`] shorthand.
    pub(crate) fn unsupported(span: Span, what: &'static str, reason: &'static str) -> Self {
        Self::Unsupported { span, what, reason }
    }
}
