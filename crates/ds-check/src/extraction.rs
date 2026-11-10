//! The @std extraction family [E4]: `unwrap_or`, `unwrap_or_else`,
//! `ok_or`, and `map_err` — plain UFCS-callable functions whose calls
//! type fully locally against the checked receiver, composed with `?`
//! for early exits.
//!
//! The ambient declares the four (generic spellings are the contract
//! text), but no call site types through generic inference: an inferred
//! `t?` parameter would bind a plain `t` receiver and the fallback's
//! width meeting would go undiagnosed, so [`crate::type_of`] types the
//! calls by rule instead. The family routes by name the same way the
//! lowerer's native table routes — the ambient occupies the value
//! namespace, and redeclaring a family name alongside it is a
//! duplicate-definition error [G3]; without the ambient, a same-named
//! generic function of the same arity rides the family rules.

/// One family member: which local typing rule its calls carry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Extraction {
    /// `(t | none, t) -> t`; the fallback is eager.
    UnwrapOr,
    /// `(t | none, fn() -> t) -> t`; the fallback runs only on the none
    /// side.
    UnwrapOrElse,
    /// `(t | none, e) -> t | e`; composed with `?` the new member
    /// propagates early.
    OkOr,
    /// `(t | e, fn(e) -> e2) -> t | e2`; the mapped members replace
    /// themselves in the result.
    MapErr,
}

impl Extraction {
    /// The family member a call name addresses, if any.
    #[must_use]
    pub fn of_name(name: &str) -> Option<Self> {
        match name {
            "unwrap_or" => Some(Self::UnwrapOr),
            "unwrap_or_else" => Some(Self::UnwrapOrElse),
            "ok_or" => Some(Self::OkOr),
            "map_err" => Some(Self::MapErr),
            _ => None,
        }
    }

    /// The ambient's call name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::UnwrapOr => "unwrap_or",
            Self::UnwrapOrElse => "unwrap_or_else",
            Self::OkOr => "ok_or",
            Self::MapErr => "map_err",
        }
    }
}
