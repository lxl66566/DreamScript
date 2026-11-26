//! Lowering errors: constructs the runtime contract does not yet cover.

use ds_diag::Span;

/// Why a checked package cannot be lowered under the current runtime
/// contract. Every variant names the surface seam that would cover it;
/// none of them is a checker gap — the package checked clean first.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum LowerError {
    /// A construct with no runtime contract yet.
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
    /// A bound extern's declaration refuses at the C-ABI seam: the
    /// artifact the graph resolves, the symbol the library exports, or
    /// the signature shape the scalar marshaling slice serves.
    #[error("the bound extern `{name}` refuses at the C-ABI seam: {reason}")]
    ForeignBind {
        /// Span of the offending call site.
        span: Span,
        /// The extern's name.
        name: String,
        /// Why the bind refused.
        reason: String,
    },
    /// No lowerable `main` entry: the module needs one.
    #[error("no lowerable `main` entry in the package")]
    NoMain,
    /// The package still carries checker diagnostics.
    #[error("the package carries {count} checker diagnostics; lower only clean packages")]
    NotChecked { count: usize },
    /// The package's affine walk reports violations: the move and deinit
    /// placement lowering derives from that walk's facts hold only over a
    /// clean one, so a moved-then-used value refuses here instead of
    /// lowering code whose deinit could fire twice at runtime.
    #[error(
        "the package carries {count} linear-use violations; lower only linearly clean packages"
    )]
    NotLinear { count: usize },
    /// A function record was rejected by the ds-mod builder.
    #[error("the function record was rejected: {0}")]
    Build(String),
    /// A frame needs more than the 256-slot cap.
    #[error("the frame of `{name}` needs {need} slots, past the 256-slot cap")]
    FrameTooWide {
        /// The function's name.
        name: String,
        /// Words the frame would need.
        need: usize,
    },
    /// The served ambient bodies disagree with the plan: the artifact's
    /// function-table prefix is not what planning produced, so the
    /// splice cannot install it. The caller falls back to the fresh
    /// lowering; this is a cache miss, never a source problem.
    #[error("the served ambient bodies disagree with the plan: {detail}")]
    ServedDrift {
        /// What disagreed.
        detail: String,
    },
    /// A linked module's tag-table row cannot join this compile's own
    /// table: two linked modules disagreeing on one id is a cross-wire
    /// no remap can fix (their code immediates are frozen in their
    /// artifacts), and a row reaching the platform's reserved range has
    /// no id to take.
    #[error("the linked tag row `{name}` (id {id}) cannot join this module's tag table: {reason}")]
    LinkedTagRow {
        /// The row's name.
        name: String,
        /// The row's id.
        id: u8,
        /// Why the row refuses.
        reason: String,
    },
}

impl LowerError {
    /// The offending node's span, for diagnostics; `None` for the
    /// package-level errors.
    #[must_use]
    pub fn span(&self) -> Option<Span> {
        match self {
            Self::Unsupported { span, .. } | Self::ForeignBind { span, .. } => Some(*span),
            _ => None,
        }
    }

    /// An [`LowerError::Unsupported`] shorthand.
    pub(crate) fn unsupported(span: Span, what: &'static str, reason: &'static str) -> Self {
        Self::Unsupported { span, what, reason }
    }

    /// Re-points a sentinel-spanned [`Self::Unsupported`] at the node
    /// whose lowering observed it: shape and word-address refusals are
    /// type-keyed and observe no node, so without this they render
    /// against an unrelated file's first line.
    #[must_use]
    pub(crate) fn respanned(self, span: Span) -> Self {
        match self {
            Self::Unsupported {
                span: sentinel,
                what,
                reason,
            } if sentinel.file.as_u32() == 0 && sentinel.start == 0 && sentinel.end == 0 => {
                Self::unsupported(span, what, reason)
            },
            other => other,
        }
    }
}
