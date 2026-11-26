//! The widened C-ABI encoding: the signature
//! vocabulary — the scalar words, a `string` or `bytes` parameter
//! riding a call-scoped (pointer, length) pair of
//! GP words, the borrow valid for the call's duration alone (the type
//! layer forbids the pointer from escaping), and a unit return riding
//! the absent word. The word boundary counts C words, not parameters: a
//! pair parameter occupies two, so two pair parameters fill the
//! enumerated transmute's envelope exactly.
//!
//! Dispatch is two-tier: a signature within [`MAX_PARAMS`] C words
//! rides the enumerated transmute (the hand-rolled hot path), and a
//! wider one stages the same word patterns through a libffi CIF — the
//! arity the vocabulary admits has no cap. The returning direction (a
//! foreign `char*` answer needing an ownership story) and
//! deinit-carrying handles are outside this face.
//!
//! The language-side consumption runs over this face: the lowering
//! seam admits the pair encodings and the unit return at the
//! bound-extern declaration (the pool constant carrying the return's
//! marshaling word), and the VM stage marshals each staged word per
//! its own kind onto the signature below.

use crate::{
    Symbol,
    word::{ArgWord, dispatch},
};

/// The marshaling face's fast-path word boundary: a signature within
/// this many C words dispatches over the enumerated transmute (a
/// string/bytes pair parameter occupying two), a wider one through the
/// libffi CIF path.
pub const MAX_PARAMS: usize = 4;

/// One extern parameter's marshaling encoding: the scalar words
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

/// One extern return's marshaling encoding: a scalar word or the
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

/// Why a marshaled call refused: the libffi interface refused to
/// prepare the call, or an argument disagrees with its parameter's
/// encoding (a count disagreement names the first index past the
/// shorter side).
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum MarshalError {
    /// The libffi call interface refused to prepare the signature.
    #[error("the libffi call interface refused to prepare the marshaled signature")]
    Prep,
    /// The argument at `index` disagrees with its parameter's encoding.
    #[error("the argument at {index} disagrees with its parameter's marshaling")]
    Kind {
        /// The first disagreeing argument's index.
        index: usize,
    },
}

/// One borrowed buffer's (pointer, length) words as the raw GP
/// patterns both dispatch tiers stage: the pointer bit-cast into the
/// register's pattern, the length riding the word beside it. The
/// enumerated targets rule pointers and buffer lengths the same
/// INTEGER class at the same width, so the bit-cast is exact.
#[allow(clippy::cast_possible_wrap)]
fn pair_gp(bytes: &[u8]) -> [i64; 2] {
    [bytes.as_ptr() as usize as i64, bytes.len() as i64]
}

/// The pair words the enumerated transmute stages: the raw patterns
/// wrapped in their register class.
fn pair_words(bytes: &[u8]) -> [ArgWord; 2] {
    pair_gp(bytes).map(ArgWord::Gp)
}

/// One CIF argument's storage cell: the value the call's `Arg`
/// references point into, alive through the call.
enum Cell {
    /// The GP word's i64 storage.
    Gp(i64),
    /// The bool word's byte storage (the low byte is the contract).
    Byte(u8),
    /// The FP word's f64 storage.
    Fp(f64),
}

impl Symbol {
    /// Calls the bound symbol over one marshaled signature: the
    /// arguments stage per their encodings — scalar words singly, a
    /// `string`/`bytes` borrow as its call-scoped (pointer, length)
    /// pair — and the answer marshals back, the unit return answering
    /// [`Answer::Unit`]. Within [`MAX_PARAMS`] C words the call runs
    /// over the enumerated transmute; a wider signature stages the
    /// same words through a libffi CIF. The call is blocking and
    /// synchronous: it pins the calling fiber for its whole duration,
    /// and a foreign abort kills the process.
    ///
    /// # Errors
    ///
    /// [`MarshalError::Kind`] when an argument's shape disagrees with
    /// its parameter's encoding, and [`MarshalError::Prep`] when the
    /// libffi interface refuses to prepare the call.
    pub fn call(&self, sig: &Sig, args: &[Arg<'_>]) -> Result<Answer, MarshalError> {
        if args.len() != sig.params.len() {
            return Err(MarshalError::Kind {
                index: args.len().min(sig.params.len()),
            });
        }
        let width: usize = sig.params.iter().map(|param| param.words()).sum();
        if width <= MAX_PARAMS {
            self.call_enumerated(sig, args, width)
        } else {
            self.call_wide(sig, args, width)
        }
    }

    /// The enumerated fast path: the staged words and the placement
    /// mask (bit i set when word i rides the FP register) select the
    /// monomorphized transmute arm. The width gate in [`Symbol::call`]
    /// bounds the staged count, so the mask's u8 shift never overflows
    /// and every arm here reads the words it staged.
    fn call_enumerated(
        self,
        sig: &Sig,
        args: &[Arg<'_>],
        width: usize,
    ) -> Result<Answer, MarshalError> {
        let mut words = Vec::with_capacity(width);
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
        debug_assert_eq!(
            words.len(),
            width,
            "every encoding stages its own word count"
        );
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

    /// The CIF path past the enumerated envelope: the same word
    /// patterns stage into typed storage cells, a libffi CIF over the
    /// matching C types preps, and the call runs through it — libffi
    /// owns the register placement the enumerated arms spell by hand.
    fn call_wide(self, sig: &Sig, args: &[Arg<'_>], width: usize) -> Result<Answer, MarshalError> {
        use libffi::middle::{Arg as CifArg, Cif, CodePtr, Type};

        let mut cells: Vec<Cell> = Vec::with_capacity(width);
        let mut types: Vec<Type> = Vec::with_capacity(width);
        for (index, (arg, param)) in args.iter().zip(&sig.params).enumerate() {
            match (arg, param) {
                (Arg::Int(value), Param::Int) => {
                    cells.push(Cell::Gp(*value));
                    types.push(Type::i64());
                },
                (Arg::Bool(value), Param::Bool) => {
                    cells.push(Cell::Byte(u8::from(*value)));
                    types.push(Type::u8());
                },
                (Arg::Float(value), Param::Float) => {
                    cells.push(Cell::Fp(*value));
                    types.push(Type::f64());
                },
                (Arg::Str(text), Param::Str) => {
                    let [pointer, length] = pair_gp(text.as_bytes());
                    cells.push(Cell::Gp(pointer));
                    cells.push(Cell::Gp(length));
                    types.push(Type::i64());
                    types.push(Type::i64());
                },
                (Arg::Bytes(buffer), Param::Bytes) => {
                    let [pointer, length] = pair_gp(buffer);
                    cells.push(Cell::Gp(pointer));
                    cells.push(Cell::Gp(length));
                    types.push(Type::i64());
                    types.push(Type::i64());
                },
                _ => return Err(MarshalError::Kind { index }),
            }
        }
        debug_assert_eq!(
            cells.len(),
            width,
            "every encoding stages its own word count"
        );
        // the references point into the cells, which hold through the
        // call below
        let cif_args: Vec<CifArg<'_>> = cells
            .iter()
            .map(|cell| match cell {
                Cell::Gp(value) => CifArg::new(value),
                Cell::Byte(value) => CifArg::new(value),
                Cell::Fp(value) => CifArg::new(value),
            })
            .collect();
        let cif = Cif::try_new(types, match sig.ret {
            Ret::Unit => Type::void(),
            Ret::Int => Type::i64(),
            Ret::Bool => Type::u8(),
            Ret::Float => Type::f64(),
        })
        .map_err(|_| MarshalError::Prep)?;
        let code = CodePtr(self.address() as *mut core::ffi::c_void);
        // Safety: the CIF's declared types spell exactly the C-ABI
        // words the signature staged (the scalar words, a pair's
        // pointer and length, the return word), so libffi's register
        // placement matches a callee compiled against the bound
        // extern's written prototype — the same declaration trust the
        // enumerated arms assume. The cells live across the call, and
        // the symbol's address stays valid while its library stays
        // loaded (the bound module's own contract).
        Ok(match sig.ret {
            Ret::Unit => {
                unsafe { cif.call::<()>(code, &cif_args) };
                Answer::Unit
            },
            Ret::Int => Answer::Int(unsafe { cif.call::<i64>(code, &cif_args) }),
            Ret::Bool => Answer::Bool(unsafe { cif.call::<u8>(code, &cif_args) } != 0),
            Ret::Float => Answer::Float(unsafe { cif.call::<f64>(code, &cif_args) }),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{Answer, Arg, MarshalError, Param, Ret, Sig};

    /// The boundary tests call real callees through their own
    /// addresses, both dispatch tiers included — the prototypes here
    /// spell exactly what the signatures below declare.
    extern "C" fn c_sum4(a: i64, b: i64, c: i64, d: i64) -> i64 {
        a + b + c + d
    }

    extern "C" fn c_sum5(one: i64, two: i64, three: i64, four: i64, five: i64) -> i64 {
        one + two + three + four + five
    }

    extern "C" fn c_pick4(choose: u8, left: f64, _spare: u8, right: f64) -> f64 {
        if choose != 0 { left } else { right }
    }

    extern "C" fn c_any5(one: u8, two: u8, three: u8, four: u8, five: u8) -> u8 {
        u8::from(one != 0 || two != 0 || three != 0 || four != 0 || five != 0)
    }

    #[allow(clippy::cast_possible_wrap)]
    extern "C" fn c_text_len(
        _a: *const u8,
        a_len: usize,
        _b: *const u8,
        b_len: usize,
        bias: i64,
    ) -> i64 {
        (a_len + b_len) as i64 + bias
    }

    #[allow(clippy::cast_possible_wrap)]
    extern "C" fn c_two_len(_a: *const u8, a_len: usize, _b: *const u8, b_len: usize) -> i64 {
        (a_len + b_len) as i64
    }

    /// A signature at the word boundary calls through the enumerated
    /// arms: four GP words answer, the placement mask empty.
    #[test]
    fn a_signature_at_the_word_boundary_calls_enumerated() {
        let sig = Sig {
            params: vec![Param::Int; 4],
            ret: Ret::Int,
        };
        let args = [Arg::Int(1), Arg::Int(2), Arg::Int(3), Arg::Int(4)];
        let answer = crate::test_symbol(c_sum4 as *const () as usize)
            .call(&sig, &args)
            .expect("the boundary signature calls");
        assert_eq!(answer, Answer::Int(10));
    }

    /// The boundary's mixed placement: the bool words ride the GP file
    /// and the floats the FP file in one four-word call, the mask
    /// selecting the matching enumerated arm.
    #[test]
    fn the_mixed_placement_at_the_word_boundary_calls_enumerated() {
        let sig = Sig {
            params: vec![Param::Bool, Param::Float, Param::Bool, Param::Float],
            ret: Ret::Float,
        };
        let args = [
            Arg::Bool(true),
            Arg::Float(1.5),
            Arg::Bool(false),
            Arg::Float(2.5),
        ];
        let answer = crate::test_symbol(c_pick4 as *const () as usize)
            .call(&sig, &args)
            .expect("the mixed boundary signature calls");
        assert_eq!(answer, Answer::Float(1.5));
    }

    /// Two pair parameters stage four C words exactly — the envelope's
    /// own arithmetic — and call through the enumerated arms.
    #[test]
    fn two_pairs_fill_the_envelope_and_call_enumerated() {
        let sig = Sig {
            params: vec![Param::Str, Param::Str],
            ret: Ret::Int,
        };
        let args = [Arg::Str("ab"), Arg::Str("cdef")];
        let answer = crate::test_symbol(c_two_len as *const () as usize)
            .call(&sig, &args)
            .expect("the pair-filled signature calls");
        assert_eq!(answer, Answer::Int(6));
    }

    /// One word past the boundary calls through the CIF path: five GP
    /// words marshal and answer.
    #[test]
    fn a_signature_past_the_word_boundary_calls_through_the_cif() {
        let sig = Sig {
            params: vec![Param::Int; 5],
            ret: Ret::Int,
        };
        let args = [
            Arg::Int(1),
            Arg::Int(2),
            Arg::Int(3),
            Arg::Int(4),
            Arg::Int(5),
        ];
        let answer = crate::test_symbol(c_sum5 as *const () as usize)
            .call(&sig, &args)
            .expect("the past-boundary signature calls");
        assert_eq!(answer, Answer::Int(15));
    }

    /// The bool word rides the CIF path's byte storage: five bool
    /// parameters marshal and the u8 answer marshals back.
    #[test]
    fn bool_words_call_through_the_cif_byte_storage() {
        let sig = Sig {
            params: vec![Param::Bool; 5],
            ret: Ret::Bool,
        };
        let args = [
            Arg::Bool(false),
            Arg::Bool(false),
            Arg::Bool(false),
            Arg::Bool(false),
            Arg::Bool(true),
        ];
        let answer = crate::test_symbol(c_any5 as *const () as usize)
            .call(&sig, &args)
            .expect("the bool signature calls");
        assert_eq!(answer, Answer::Bool(true));
    }

    /// A pair-staged signature past the boundary calls through the CIF
    /// path: each string borrow stages its (pointer, length) GP pair,
    /// the borrowed bytes read only for the call's duration.
    #[test]
    fn pairs_past_the_word_boundary_call_through_the_cif() {
        let sig = Sig {
            params: vec![Param::Str, Param::Str, Param::Int],
            ret: Ret::Int,
        };
        let args = [Arg::Str("hello"), Arg::Str("ds"), Arg::Int(2)];
        let answer = crate::test_symbol(c_text_len as *const () as usize)
            .call(&sig, &args)
            .expect("the pair-staged signature calls");
        assert_eq!(answer, Answer::Int(9));
    }

    /// The encodings own word arithmetic: a scalar parameter stages one
    /// C word, a string/bytes pair two — the boundary counts words,
    /// not parameters, so two pairs fill the envelope exactly.
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
    /// first index past the shorter side. The kind check runs on both
    /// dispatch tiers.
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
        let wide = Sig {
            params: vec![Param::Int; 5],
            ret: Ret::Unit,
        };
        let error = crate::test_symbol(0)
            .call(&wide, &[Arg::Int(0); 4])
            .expect_err("the short argument list refuses past the boundary too");
        assert_eq!(error, MarshalError::Kind { index: 4 });
    }
}
