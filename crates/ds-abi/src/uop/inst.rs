//! One function-body word: the runnable uop stream interleaved with the
//! RC pair. The module body carries both majors, so one
//! instruction space covers the whole placement contract: jump targets
//! count RC words, and no stream ever renumbers (the majors below 0xE
//! are untouched).

use super::{DecodeError, EncodeError, RcDecodeError, RcOp, Uop, rc::RC_MAJOR};

/// Why a 32-bit word cannot decode into an [`Inst`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InstDecodeError {
    /// The word names no assigned uop (majors other than 0xE).
    Uop(DecodeError),
    /// The word is inside the RC major but names no canonical RC op.
    Rc(RcDecodeError),
}

/// One instruction word of a function body: a runnable micro-operation
/// or a placed reference-counting op.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Inst {
    /// A runnable micro-operation.
    Uop(Uop),
    /// A placed reference-counting op.
    Rc(RcOp),
}

impl From<Uop> for Inst {
    fn from(op: Uop) -> Self {
        Self::Uop(op)
    }
}

impl From<RcOp> for Inst {
    fn from(op: RcOp) -> Self {
        Self::Rc(op)
    }
}

impl Inst {
    /// Encodes `self` located at instruction index `pc` into the fixed
    /// 32-bit word. RC ops carry no pc-relative operand, so they encode
    /// anywhere.
    ///
    /// # Errors
    ///
    /// Propagates [`EncodeError`] when a uop cannot be encoded at its pc
    /// (a biased jump outside the u16 window); RC words cannot fail.
    pub fn encode(&self, pc: u32) -> Result<u32, EncodeError> {
        match self {
            Self::Uop(op) => op.encode(pc),
            Self::Rc(op) => Ok(op.encode()),
        }
    }

    /// Decodes the 32-bit word at instruction index `pc`, dispatching on
    /// the major nibble: 0xE goes to [`RcOp::decode`], everything else to
    /// [`Uop::decode`]. Inverse of [`Inst::encode`].
    ///
    /// # Errors
    ///
    /// [`InstDecodeError::Rc`] inside major 0xE, [`InstDecodeError::Uop`]
    /// outside it.
    pub fn decode(pc: u32, word: u32) -> Result<Self, InstDecodeError> {
        if word >> 28 == u32::from(RC_MAJOR) {
            RcOp::decode(word)
                .map(Self::Rc)
                .map_err(InstDecodeError::Rc)
        } else {
            Uop::decode(pc, word)
                .map(Self::Uop)
                .map_err(InstDecodeError::Uop)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Inst, InstDecodeError};
    use crate::uop::{DecodeError, RcDecodeError, RcOp, Uop};

    #[test]
    fn both_majors_round_trip_in_one_space() {
        let body = [
            Inst::Uop(Uop::Funcf { frame_slots: 3 }),
            Inst::Uop(Uop::Kstr {
                dst: 0,
                kidx: crate::ConstIdx(0),
            }),
            Inst::Rc(RcOp::Inc { src: 0 }),
            Inst::Rc(RcOp::Dec { src: 0 }),
            Inst::Uop(Uop::Ret { src: 0 }),
        ];
        for (pc, inst) in body.iter().enumerate() {
            let word = inst
                .encode(u32::try_from(pc).expect("pc fits u32"))
                .expect("encodes");
            assert_eq!(
                Inst::decode(u32::try_from(pc).expect("pc fits u32"), word),
                Ok(*inst),
                "word {word:#x} round-trips"
            );
        }
    }

    #[test]
    fn foreign_words_decode_to_their_major_error() {
        assert_eq!(
            Inst::decode(0, 0xe2_01_00_00),
            Err(InstDecodeError::Rc(RcDecodeError::UnknownOpcode {
                major: 0xe,
                minor: 2
            })),
            "inside 0xE the RC decoder answers"
        );
        assert_eq!(
            Inst::decode(0, 0xf0_01_00_00),
            Err(InstDecodeError::Uop(DecodeError::UnknownOpcode {
                major: 0xf,
                minor: 0
            })),
            "outside 0xE the uop decoder answers"
        );
    }
}
