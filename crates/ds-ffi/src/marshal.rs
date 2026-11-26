//! The marshaling round's widened C-ABI encoding (H5): the signature
//! vocabulary — the scalar words, a `string` or `bytes` parameter
//! riding a call-scoped (pointer, length) pair of
//! GP words, the borrow valid for the call's duration alone (the type
//! layer forbids the pointer from escaping), and a unit return riding
//! the absent word. The arity cap counts C words, not parameters: a
//! pair parameter occupies two, so two pair parameters fill the
//! enumerated transmute's envelope exactly.
//!
//! The language-side consumption runs over this face: the lowering
//! seam admits the pair encodings and the unit return at the
//! bound-extern declaration (the pool constant carrying the return's
//! marshaling word), and the VM stage marshals each staged word per
//! its own kind onto the signature below. The returning direction (a
//! foreign `char*` answer needing an ownership story) and
//! deinit-carrying handles stay queued with the ownership round.

use crate::{
    Symbol,
    word::{ArgWord, dispatch},
};

/// The marshaling face's word cap: the enumerated transmute covers
/// every GP/FP placement up to this many C words, a string/bytes pair
/// parameter occupying two.
pub const MAX_PARAMS: usize = 4;

/// One extern parameter's marshaling encoding (H5): the scalar words
/// plus the borrowed string/bytes pair.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Param {
    /// The i64 GP word.
    Int,
    /// The u8 GP word.
    Bool,
    /// The f64 FP word.
    Float,
    /// The call-scoped (pointer, length) GP pair over a `string`'s
    /// UTF-8 bytes.
    Str,
    /// The call-scoped (pointer, length) GP pair over a `bytes`
    /// buffer.
    Bytes,
}

impl Param {
    /// The C words the encoding stages.
    #[must_use]
    pub fn words(self) -> usize {
        match self {
            Self::Int | Self::Bool | Self::Float => 1,
            Self::Str | Self::Bytes => 2,
        }
    }
}

/// One extern return's marshaling encoding (H5): a scalar word or the
/// absent word.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ret {
    /// No return word — the C void.
    Unit,
    /// The i64 GP word.
    Int,
    /// The u8 GP word.
    Bool,
    /// The f64 FP word.
    Float,
}

/// One marshaled extern signature: the parameter encodings in call
/// order and the return encoding.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sig {
    /// The parameter encodings, in call order.
    pub params: Vec<Param>,
    /// The return encoding.
    pub ret: Ret,
}

/// One argument in flight: the marshaled word, or the borrow a pair
/// encoding stages its pointer over — call-scoped, valid for the
/// call's duration alone.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Arg<'a> {
    /// The i64 word.
    Int(i64),
    /// The u8 word.
    Bool(bool),
    /// The f64 word.
    Float(f64),
    /// The borrowed UTF-8 bytes.
    Str(&'a str),
    /// The borrowed buffer.
    Bytes(&'a [u8]),
}

/// One call's answer: a scalar word or the absent word.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Answer {
    /// No return word.
    Unit,
    /// The i64 word.
    Int(i64),
    /// The u8 word.
    Bool(bool),
    /// The f64 word.
    Float(f64),
}

/// Why a marshaled call refused: the staged words are wider than the
/// enumerated transmute serves, or an argument disagrees with its
/// parameter's encoding (a count disagreement names the first index
/// past the shorter side).
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum MarshalError {
    /// The staged word count is wider than the enumerated transmute.
    #[error(
        "a marshaled C-ABI signature of {words} words; the enumerated transmute serves at most \
         {MAX_PARAMS}"
    )]
    Width {
        /// The refused signature's staged word count.
        words: usize,
    },
    /// The argument at `index` disagrees with its parameter's encoding.
    #[error("the argument at {index} disagrees with its parameter's marshaling")]
    Kind {
        /// The first disagreeing argument's index.
        index: usize,
    },
}

/// One borrowed buffer's (pointer, length) words: the pointer bit-cast
/// into the GP register's pattern, the length riding the word beside
/// it. The enumerated targets rule pointers and buffer lengths the
/// same INTEGER class at the same width, so the bit-cast is exact.
#[allow(clippy::cast_possible_wrap)]
fn pair_words(bytes: &[u8]) -> [ArgWord; 2] {
    [
        ArgWord::Gp(bytes.as_ptr() as usize as i64),
        ArgWord::Gp(bytes.len() as i64),
    ]
}

impl Symbol {
    /// Calls the bound symbol over one marshaled signature (H5): the
    /// arguments stage per their encodings — scalar words singly, a
    /// `string`/`bytes` borrow as its call-scoped (pointer, length)
    /// pair — and the answer marshals back, the unit return answering
    /// [`Answer::Unit`]. The call is blocking and synchronous: it pins
    /// the calling fiber for its whole duration, and a foreign abort
    /// kills the process.
    ///
    /// # Errors
    ///
    /// [`MarshalError::Width`] when the staged words exceed
    /// [`MAX_PARAMS`] (pair parameters counting two), and
    /// [`MarshalError::Kind`] when an argument's shape disagrees with
    /// its parameter's encoding.
    pub fn call(&self, sig: &Sig, args: &[Arg<'_>]) -> Result<Answer, MarshalError> {
        if args.len() != sig.params.len() {
            return Err(MarshalError::Kind {
                index: args.len().min(sig.params.len()),
            });
        }
        let mut words = Vec::with_capacity(args.len());
        // the placement mask: bit i set when word i rides the FP
        // register — the first word at the low bit, the transmute
        // arms' own order
        let mut mask = 0u8;
        let mut staged = 0usize;
        for (index, (arg, param)) in args.iter().zip(&sig.params).enumerate() {
            match (arg, param) {
                (Arg::Int(value), Param::Int) => {
                    words.push(ArgWord::Gp(*value));
                    staged += 1;
                },
                (Arg::Bool(value), Param::Bool) => {
                    words.push(ArgWord::Gp(i64::from(*value)));
                    staged += 1;
                },
                (Arg::Float(value), Param::Float) => {
                    words.push(ArgWord::Fp(*value));
                    mask |= 1 << staged;
                    staged += 1;
                },
                (Arg::Str(text), Param::Str) => {
                    words.extend_from_slice(&pair_words(text.as_bytes()));
                    staged += 2;
                },
                (Arg::Bytes(buffer), Param::Bytes) => {
                    words.extend_from_slice(&pair_words(buffer));
                    staged += 2;
                },
                _ => return Err(MarshalError::Kind { index }),
            }
        }
        if words.len() > MAX_PARAMS {
            return Err(MarshalError::Width { words: words.len() });
        }
        Ok(match sig.ret {
            Ret::Unit => {
                dispatch!(self, &words, mask, ());
                Answer::Unit
            },
            Ret::Int => Answer::Int(dispatch!(self, &words, mask, i64)),
            Ret::Bool => Answer::Bool(dispatch!(self, &words, mask, u8) != 0),
            Ret::Float => Answer::Float(dispatch!(self, &words, mask, f64)),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{Arg, MarshalError, Param, Ret, Sig};

    /// A staged word count past the cap refuses with its count — the
    /// backstop ahead of the catch-all arm, a pair parameter counting
    /// two words.
    #[test]
    fn a_wide_marshaled_signature_refuses_with_its_word_count() {
        let sig = Sig {
            params: vec![Param::Str, Param::Str, Param::Int],
            ret: Ret::Unit,
        };
        let args = [Arg::Str("a"), Arg::Str("b"), Arg::Int(1)];
        let error = crate::test_symbol(0)
            .call(&sig, &args)
            .expect_err("the staged words refuse");
        assert_eq!(error, MarshalError::Width { words: 5 });
    }

    /// The encodings own word arithmetic: a scalar parameter stages one
    /// C word, a string/bytes pair two — the cap counts words, not
    /// parameters, so two pairs fill the envelope exactly.
    #[test]
    fn pair_parameters_stage_two_words_each() {
        assert_eq!(Param::Int.words(), 1);
        assert_eq!(Param::Bool.words(), 1);
        assert_eq!(Param::Float.words(), 1);
        assert_eq!(Param::Str.words(), 2);
        assert_eq!(Param::Bytes.words(), 2);
        assert_eq!(
            Param::Str.words() + Param::Bytes.words(),
            crate::MAX_PARAMS,
            "two pairs fill the word envelope exactly"
        );
    }

    /// An argument disagreeing with its parameter's encoding refuses
    /// with the first disagreeing index; a count disagreement names the
    /// first index past the shorter side.
    #[test]
    fn a_disagreeing_argument_refuses_with_its_index() {
        let sig = Sig {
            params: vec![Param::Int, Param::Str],
            ret: Ret::Unit,
        };
        let error = crate::test_symbol(0)
            .call(&sig, &[Arg::Int(0), Arg::Bytes(&[1])])
            .expect_err("the bytes argument disagrees with the string parameter");
        assert_eq!(error, MarshalError::Kind { index: 1 });
        let error = crate::test_symbol(0)
            .call(&sig, &[Arg::Int(0)])
            .expect_err("the missing argument refuses");
        assert_eq!(error, MarshalError::Kind { index: 1 });
    }
}
