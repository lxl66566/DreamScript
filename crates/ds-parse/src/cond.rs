//! The conditional import's condition vocabulary: the keys `os` and
//! `arch`, each an equality against one lowercase target-triple word,
//! evaluated against the compiler's own target at the compile that reads
//! the import.

use ds_diag::Span;

use crate::ast::{BinaryOp, Expr, ImportDecl, StrPart};

/// What one import declaration's condition answers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ImportCond {
    /// No condition written: the import holds on every target.
    Absent,
    /// A well-formed condition; the bool answers whether the compiler's
    /// own target words satisfy it. `true` holds the import into this
    /// compile, `false` leaves it out: no link name, no surface, no
    /// binding.
    Held(bool),
    /// A condition outside the vocabulary: the offending expression's
    /// span and the primary label naming the fault. The import stays out
    /// of this compile and the checker refuses.
    Malformed {
        /// Span of the offending expression.
        span: Span,
        /// The refusal's primary label.
        label: String,
    },
}

impl ImportCond {
    /// Whether the import joins this compile: unconditional, or a
    /// condition the compiler's own target satisfies. A malformed
    /// condition joins nothing; the checker owns its refusal.
    #[must_use]
    pub fn held(self) -> bool {
        matches!(self, Self::Absent | Self::Held(true))
    }
}

/// One condition key's words on the compiler's own target: the arch key
/// answers the target's arch word, the os key its os word, with the
/// Apple triple's `darwin` beside the environment spelling `macos` (both
/// name the same host). Every word is lowercase.
fn host_words(key: &str) -> &'static [&'static str] {
    match key {
        "arch" => &[std::env::consts::ARCH],
        _ => {
            if cfg!(target_vendor = "apple") {
                &[std::env::consts::OS, "darwin"]
            } else {
                &[std::env::consts::OS]
            }
        },
    }
}

/// Whether a written word spells the lowercase vocabulary the target
/// triples use: lowercase ASCII letters, digits, underscores, hyphens.
fn lowercase_word(word: &str) -> bool {
    !word.is_empty()
        && word
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

/// The plain string one operand spells, if any: a string literal without
/// interpolation holes, or a raw string.
fn plain_string(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Str { parts, .. } if parts.len() == 1 => match &parts[0] {
            StrPart::Text { text, .. } => Some(text.clone()),
            StrPart::Expr { .. } | StrPart::Ident { .. } => None,
        },
        Expr::RawStr { text, .. } => Some(text.clone()),
        _ => None,
    }
}

/// One import declaration's condition verdict over the vocabulary.
#[must_use]
pub fn import_cond(decl: &ImportDecl) -> ImportCond {
    let Some(cond) = &decl.cond else {
        return ImportCond::Absent;
    };
    let Expr::Binary {
        op: BinaryOp::Eq,
        left,
        right,
        ..
    } = cond
    else {
        return ImportCond::Malformed {
            span: cond.span(),
            label: "not a key comparison".to_owned(),
        };
    };
    let Expr::Path { path, .. } = left.as_ref() else {
        return ImportCond::Malformed {
            span: left.span(),
            label: "the left side names one condition key".to_owned(),
        };
    };
    let [only] = path.segments.as_slice() else {
        return ImportCond::Malformed {
            span: path.span,
            label: "the key is one bare name".to_owned(),
        };
    };
    if only.name != "os" && only.name != "arch" {
        return ImportCond::Malformed {
            span: only.span,
            label: format!("`{}` names no condition key", only.name),
        };
    }
    let Some(word) = plain_string(right) else {
        return ImportCond::Malformed {
            span: right.span(),
            label: "the right side is one plain string".to_owned(),
        };
    };
    if !lowercase_word(&word) {
        return ImportCond::Malformed {
            span: right.span(),
            label: "the word spells lowercase target-triple letters".to_owned(),
        };
    }
    ImportCond::Held(host_words(&only.name).contains(&word.as_str()))
}
