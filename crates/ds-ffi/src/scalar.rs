//! The scalar C-ABI marshaling face (H5): one bound symbol called over
//! a scalar signature — `int`, `bool`, and `float` in and out, the
//! i64, u8, and f64 words H5 rules — the first landed slice, kept as
//! the scalar-only shape; the VM stage marshals through the widened
//! face beside it, so no in-tree stage calls this one today. A
//! signature wider than four words refuses
//! ([`ScalarError`]); the lowering seam refuses the same shape before
//! any run starts, so the refusal here is the backstop, never the
//! diagnostic a source sees. The widened encodings (string/bytes
//! pairs, unit returns) live in the marshaling face over the same
//! enumerated transmute.
//!
//! The H5 contract clauses live at this seam: a bound extern means
//! panic=abort — a foreign abort kills the process, the VM never
//! unwinding through foreign frames — and a blocking foreign call pins
//! its fiber, cancellation observed only at VM re-entry (no preemption
//! point exists inside the call).

use crate::{
    Symbol,
    word::{ArgWord, dispatch},
};

/// The scalar marshaling slice's arity cap: the enumerated transmute
/// covers every GP/FP placement up to this many words. The marshaling
/// face counts the same cap in C words, a string/bytes pair parameter
/// occupying two.
pub const MAX_PARAMS: usize = 4;

/// One scalar parameter or return kind (H5): `int` the i64 GP word,
/// `bool` the u8 GP word, `float` the f64 FP word.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScalarKind {
    /// The i64 general-purpose word.
    Int,
    /// The u8 general-purpose word.
    Bool,
    /// The f64 floating word.
    Float,
}

/// One scalar value in flight: the marshaled word itself.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Scalar {
    /// The i64 word.
    Int(i64),
    /// The u8 word.
    Bool(bool),
    /// The f64 word.
    Float(f64),
}

impl Scalar {
    /// The word's kind.
    #[must_use]
    pub fn kind(self) -> ScalarKind {
        match self {
            Self::Int(_) => ScalarKind::Int,
            Self::Bool(_) => ScalarKind::Bool,
            Self::Float(_) => ScalarKind::Float,
        }
    }
}

/// The scalar signature one bound symbol is called under: the parameter
/// kinds in order and the return kind.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScalarSig {
    /// The parameter kinds, in call order.
    pub params: Vec<ScalarKind>,
    /// The return kind.
    pub ret: ScalarKind,
}

/// Why a scalar call refused: the signature is wider than the enumerated
/// transmute serves, a shape the lowering seam refuses ahead of any run.
#[derive(Clone, Copy, Debug, thiserror::Error)]
#[error(
    "a scalar C-ABI signature of {width} parameters; the marshaling slice serves at most \
     {MAX_PARAMS}"
)]
pub struct ScalarError {
    /// The refused signature's parameter count.
    pub width: usize,
}

/// One argument's marshaled register word: `int` and `bool` ride the GP
/// register (bool as its 0/1 u8, occupying the same slot an i64 would),
/// `float` the FP register.
fn word(arg: Scalar) -> ArgWord {
    match arg {
        Scalar::Int(value) => ArgWord::Gp(value),
        Scalar::Bool(value) => ArgWord::Gp(i64::from(value)),
        Scalar::Float(value) => ArgWord::Fp(value),
    }
}

impl Symbol {
    /// Calls the bound symbol over one scalar signature (H5): the
    /// arguments marshal per their kinds — `int` and `bool` through the
    /// general-purpose registers as i64 and u8, `float` through the
    /// floating register as f64 — and the return marshals back the same
    /// way. The call is blocking and synchronous: it pins the calling
    /// fiber for its whole duration, and a foreign abort kills the
    /// process (panic=abort, the VM never unwinding through a foreign
    /// frame).
    ///
    /// # Errors
    ///
    /// [`ScalarError`] when the signature is wider than
    /// [`MAX_PARAMS`] — a shape the lowering seam refuses before any
    /// run starts, so a live answer never pays this path.
    pub fn call_scalar(&self, sig: &ScalarSig, args: &[Scalar]) -> Result<Scalar, ScalarError> {
        if sig.params.len() > MAX_PARAMS {
            return Err(ScalarError {
                width: sig.params.len(),
            });
        }
        debug_assert_eq!(
            sig.params,
            args.iter().map(|arg| arg.kind()).collect::<Vec<_>>(),
            "the staged words match the signature the lowering checked"
        );
        let words: Vec<ArgWord> = args.iter().copied().map(word).collect();
        // the placement mask: bit i set when word i rides the FP
        // register — the first word at the low bit, the transmute
        // arms' own order
        let mask = sig
            .params
            .iter()
            .enumerate()
            .fold(0u8, |mask, (index, kind)| {
                mask | (u8::from(matches!(kind, ScalarKind::Float)) << index)
            });
        Ok(match sig.ret {
            ScalarKind::Int => Scalar::Int(dispatch!(self, &words, mask, i64)),
            ScalarKind::Bool => Scalar::Bool(dispatch!(self, &words, mask, u8) != 0),
            ScalarKind::Float => Scalar::Float(dispatch!(self, &words, mask, f64)),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{MAX_PARAMS, Scalar, ScalarError, ScalarKind, ScalarSig};

    /// A wider-than-cap signature refuses with its width, the backstop
    /// the lowering seam answers ahead of any run.
    #[test]
    fn a_wide_signature_refuses_with_its_width() {
        let sig = ScalarSig {
            params: vec![ScalarKind::Int; MAX_PARAMS + 1],
            ret: ScalarKind::Int,
        };
        let args = vec![Scalar::Int(0); MAX_PARAMS + 1];
        let error: ScalarError = crate::test_symbol(0)
            .call_scalar(&sig, &args)
            .expect_err("the width refuses");
        assert_eq!(error.width, MAX_PARAMS + 1);
    }
}
