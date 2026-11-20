//! The scalar C-ABI marshaling (H5): one bound symbol called over a
//! scalar signature — `int`, `bool`, and `float` in and out, the i64,
//! u8, and f64 words H5 rules. A signature wider than four parameters
//! refuses ([`ScalarError`]); the lowering seam refuses the same shape
//! before any run starts, so the refusal here is the backstop, never
//! the diagnostic a source sees.
//!
//! The call itself is a per-register-class transmute: each parameter
//! position passes in the general-purpose or the floating register file
//! exactly as its kind demands, so the function-pointer type the
//! address transmutes into must match the kind sequence — a fixed
//! enumeration over the GP/FP placements up to the arity cap, which is
//! why the cap exists at all (an arbitrary runtime signature needs
//! libffi, and the scalar slice stays dependency-free like the rest of
//! the crate). The two register classes are honest in both directions:
//! `bool` rides the GP word as u8 and `int` as i64, while `float`
//! rides the FP word as f64.
//!
//! The H5 contract clauses live at this seam: a bound extern means
//! panic=abort — a foreign abort kills the process, the VM never
//! unwinding through foreign frames — and a blocking foreign call pins
//! its fiber, cancellation observed only at VM re-entry (no preemption
//! point exists inside the call).

use crate::Symbol;

/// The scalar marshaling slice's arity cap: the enumerated transmute
/// covers every GP/FP placement up to this many parameters.
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

/// One marshaled argument: the register class with its word.
#[derive(Clone, Copy)]
enum ArgWord {
    /// The general-purpose register word.
    Gp(i64),
    /// The floating register word.
    Fp(f64),
}

/// The marshaled words' cursor: the call site reads one word per
/// parameter position in order, converting through the class the
/// monomorphized function-pointer type demands.
struct Cursor<'a> {
    words: &'a [ArgWord],
    at: usize,
}

/// The general-purpose word's conversions: the return types the
/// enumerated call sites serve.
trait FromGp {
    /// The conversion off the GP word.
    fn from_gp(word: i64) -> Self;
}

/// The floating word's conversions.
trait FromFp {
    /// The conversion off the FP word.
    fn from_fp(word: f64) -> Self;
}

impl FromGp for i64 {
    fn from_gp(word: i64) -> Self {
        word
    }
}

impl FromGp for u8 {
    // the C-ABI contract: a u8 parameter reads the GP word's low byte
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    fn from_gp(word: i64) -> Self {
        word as u8
    }
}

impl FromFp for f64 {
    fn from_fp(word: f64) -> Self {
        word
    }
}

impl Cursor<'_> {
    /// Takes the next word as a general-purpose conversion.
    fn gp<R: FromGp>(&mut self) -> R {
        let word = self.words[self.at];
        self.at += 1;
        R::from_gp(match word {
            ArgWord::Gp(value) => value,
            // unreachable against the lowering's checked kinds; the
            // class pairing is the transmute's own contract
            ArgWord::Fp(_) => 0,
        })
    }

    /// Takes the next word as a floating conversion.
    fn fp<R: FromFp>(&mut self) -> R {
        let word = self.words[self.at];
        self.at += 1;
        R::from_fp(match word {
            ArgWord::Fp(value) => value,
            ArgWord::Gp(_) => 0.0,
        })
    }
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

/// The per-kind-sequence call: the macro takes the marshaled words, the
/// parameter kinds' GP/FP placements, and the return type, transmutes
/// the symbol's address into the matching `unsafe extern "C"`
/// function-pointer type, and calls it. Each arm is a monomorphized
/// call site; the runtime signature chooses among them by the placement
/// mask.
macro_rules! dispatch {
    ($sym:expr, $sig:expr, $words:expr, $ret:ty) => {{
        let params: &[ScalarKind] = &$sig.params;
        let mut cursor = Cursor {
            words: $words,
            at: 0,
        };
        // the placement mask: bit i set when parameter i is FP
        let mut mask = 0u8;
        for (index, kind) in params.iter().enumerate() {
            if matches!(kind, ScalarKind::Float) {
                mask |= 1 << index;
            }
        }
        let address = $sym.address();
        match (params.len(), mask) {
            (0, 0) => {
                let f: unsafe extern "C" fn() -> $ret = unsafe { std::mem::transmute(address) };
                unsafe { f() }
            },
            (1, 0) => {
                let f: unsafe extern "C" fn(i64) -> $ret = unsafe { std::mem::transmute(address) };
                unsafe { f(cursor.gp()) }
            },
            (1, 1) => {
                let f: unsafe extern "C" fn(f64) -> $ret = unsafe { std::mem::transmute(address) };
                unsafe { f(cursor.fp()) }
            },
            (2, 0) => {
                let f: unsafe extern "C" fn(i64, i64) -> $ret =
                    unsafe { std::mem::transmute(address) };
                unsafe { f(cursor.gp(), cursor.gp()) }
            },
            (2, 1) => {
                let f: unsafe extern "C" fn(f64, i64) -> $ret =
                    unsafe { std::mem::transmute(address) };
                unsafe { f(cursor.fp(), cursor.gp()) }
            },
            (2, 2) => {
                let f: unsafe extern "C" fn(i64, f64) -> $ret =
                    unsafe { std::mem::transmute(address) };
                unsafe { f(cursor.gp(), cursor.fp()) }
            },
            (2, 3) => {
                let f: unsafe extern "C" fn(f64, f64) -> $ret =
                    unsafe { std::mem::transmute(address) };
                unsafe { f(cursor.fp(), cursor.fp()) }
            },
            (3, 0) => {
                let f: unsafe extern "C" fn(i64, i64, i64) -> $ret =
                    unsafe { std::mem::transmute(address) };
                unsafe { f(cursor.gp(), cursor.gp(), cursor.gp()) }
            },
            (3, 1) => {
                let f: unsafe extern "C" fn(f64, i64, i64) -> $ret =
                    unsafe { std::mem::transmute(address) };
                unsafe { f(cursor.fp(), cursor.gp(), cursor.gp()) }
            },
            (3, 2) => {
                let f: unsafe extern "C" fn(i64, f64, i64) -> $ret =
                    unsafe { std::mem::transmute(address) };
                unsafe { f(cursor.gp(), cursor.fp(), cursor.gp()) }
            },
            (3, 3) => {
                let f: unsafe extern "C" fn(f64, f64, i64) -> $ret =
                    unsafe { std::mem::transmute(address) };
                unsafe { f(cursor.fp(), cursor.fp(), cursor.gp()) }
            },
            (3, 4) => {
                let f: unsafe extern "C" fn(i64, i64, f64) -> $ret =
                    unsafe { std::mem::transmute(address) };
                unsafe { f(cursor.gp(), cursor.gp(), cursor.fp()) }
            },
            (3, 5) => {
                let f: unsafe extern "C" fn(f64, i64, f64) -> $ret =
                    unsafe { std::mem::transmute(address) };
                unsafe { f(cursor.fp(), cursor.gp(), cursor.fp()) }
            },
            (3, 6) => {
                let f: unsafe extern "C" fn(i64, f64, f64) -> $ret =
                    unsafe { std::mem::transmute(address) };
                unsafe { f(cursor.gp(), cursor.fp(), cursor.fp()) }
            },
            (3, 7) => {
                let f: unsafe extern "C" fn(f64, f64, f64) -> $ret =
                    unsafe { std::mem::transmute(address) };
                unsafe { f(cursor.fp(), cursor.fp(), cursor.fp()) }
            },
            (4, 0) => {
                let f: unsafe extern "C" fn(i64, i64, i64, i64) -> $ret =
                    unsafe { std::mem::transmute(address) };
                unsafe { f(cursor.gp(), cursor.gp(), cursor.gp(), cursor.gp()) }
            },
            (4, 1) => {
                let f: unsafe extern "C" fn(f64, i64, i64, i64) -> $ret =
                    unsafe { std::mem::transmute(address) };
                unsafe { f(cursor.fp(), cursor.gp(), cursor.gp(), cursor.gp()) }
            },
            (4, 2) => {
                let f: unsafe extern "C" fn(i64, f64, i64, i64) -> $ret =
                    unsafe { std::mem::transmute(address) };
                unsafe { f(cursor.gp(), cursor.fp(), cursor.gp(), cursor.gp()) }
            },
            (4, 3) => {
                let f: unsafe extern "C" fn(f64, f64, i64, i64) -> $ret =
                    unsafe { std::mem::transmute(address) };
                unsafe { f(cursor.fp(), cursor.fp(), cursor.gp(), cursor.gp()) }
            },
            (4, 4) => {
                let f: unsafe extern "C" fn(i64, i64, f64, i64) -> $ret =
                    unsafe { std::mem::transmute(address) };
                unsafe { f(cursor.gp(), cursor.gp(), cursor.fp(), cursor.gp()) }
            },
            (4, 5) => {
                let f: unsafe extern "C" fn(f64, i64, f64, i64) -> $ret =
                    unsafe { std::mem::transmute(address) };
                unsafe { f(cursor.fp(), cursor.gp(), cursor.fp(), cursor.gp()) }
            },
            (4, 6) => {
                let f: unsafe extern "C" fn(i64, f64, f64, i64) -> $ret =
                    unsafe { std::mem::transmute(address) };
                unsafe { f(cursor.gp(), cursor.fp(), cursor.fp(), cursor.gp()) }
            },
            (4, 7) => {
                let f: unsafe extern "C" fn(f64, f64, f64, i64) -> $ret =
                    unsafe { std::mem::transmute(address) };
                unsafe { f(cursor.fp(), cursor.fp(), cursor.fp(), cursor.gp()) }
            },
            (4, 8) => {
                let f: unsafe extern "C" fn(i64, i64, i64, f64) -> $ret =
                    unsafe { std::mem::transmute(address) };
                unsafe { f(cursor.gp(), cursor.gp(), cursor.gp(), cursor.fp()) }
            },
            (4, 9) => {
                let f: unsafe extern "C" fn(f64, i64, i64, f64) -> $ret =
                    unsafe { std::mem::transmute(address) };
                unsafe { f(cursor.fp(), cursor.gp(), cursor.gp(), cursor.fp()) }
            },
            (4, 10) => {
                let f: unsafe extern "C" fn(i64, f64, i64, f64) -> $ret =
                    unsafe { std::mem::transmute(address) };
                unsafe { f(cursor.gp(), cursor.fp(), cursor.gp(), cursor.fp()) }
            },
            (4, 11) => {
                let f: unsafe extern "C" fn(f64, f64, i64, f64) -> $ret =
                    unsafe { std::mem::transmute(address) };
                unsafe { f(cursor.fp(), cursor.fp(), cursor.gp(), cursor.fp()) }
            },
            (4, 12) => {
                let f: unsafe extern "C" fn(i64, i64, f64, f64) -> $ret =
                    unsafe { std::mem::transmute(address) };
                unsafe { f(cursor.gp(), cursor.gp(), cursor.fp(), cursor.fp()) }
            },
            (4, 13) => {
                let f: unsafe extern "C" fn(f64, i64, f64, f64) -> $ret =
                    unsafe { std::mem::transmute(address) };
                unsafe { f(cursor.fp(), cursor.gp(), cursor.fp(), cursor.fp()) }
            },
            (4, 14) => {
                let f: unsafe extern "C" fn(i64, f64, f64, f64) -> $ret =
                    unsafe { std::mem::transmute(address) };
                unsafe { f(cursor.gp(), cursor.fp(), cursor.fp(), cursor.fp()) }
            },
            _ => {
                let f: unsafe extern "C" fn(f64, f64, f64, f64) -> $ret =
                    unsafe { std::mem::transmute(address) };
                unsafe { f(cursor.fp(), cursor.fp(), cursor.fp(), cursor.fp()) }
            },
        }
    }};
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
        Ok(match sig.ret {
            ScalarKind::Int => Scalar::Int(dispatch!(self, sig, &words, i64)),
            ScalarKind::Bool => Scalar::Bool(dispatch!(self, sig, &words, u8) != 0),
            ScalarKind::Float => Scalar::Float(dispatch!(self, sig, &words, f64)),
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
