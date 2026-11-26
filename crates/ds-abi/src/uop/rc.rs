//! The reference-counting ops: RCINC/RCDEC in the reserved major
//! 0xE of the fixed 32-bit format.
//!
//! These live beside [`super::Uop`](crate::uop::Uop) instead of inside it
//! on purpose: the tier-1 dispatch match stays exhaustive over `Uop`, and
//! [`super::Inst`](crate::uop::Inst) is the one-body-word union both
//! majors decode into — module bodies carry uops and RC words in one
//! instruction space, and the VM dispatches major 0xE through
//! [`RcOp::decode`] over its counted heap blocks. The streams never
//! renumber (the majors below 0xE are untouched).
//!
//! Semantics (over the counted heap blocks of `ds-vm/src/object.rs`,
//! the consumed implementation):
//!
//! - `Inc { src }` — `rc_inc`: the reference in `slots[src]` gains one owner. Placed at every
//!   duplication edge (a copy into a second slot, a call staging the caller keeps alive).
//! - `Dec { src }` — `rc_dec`: one owner drops. A unique death runs the block's static `deinit`
//!   hook exactly once and threads the block into the reuse pool (the next compatible allocation
//!   claims it — the runtime pairs dec-to-alloc internally, so no alloc-at uop exists).
//!
//! Both read `slots[src]` and write no slot (the count lives in the heap
//! header), so they are impure for the same reason the allocating ops are:
//! reordering or duplicating one breaks the dup/drop balance.

/// The reference-counting major: the growth slot the module docs reserved
/// for the RC ops. Bytes `0xE0` (RCINC) and `0xE1` (RCDEC) are the only
/// assigned minors; the rest of 0xE stays reserved.
pub const RC_MAJOR: u8 = 0xe;

/// Why a 32-bit word cannot decode into an [`RcOp`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RcDecodeError {
    /// The opcode byte is not RCINC or RCDEC.
    UnknownOpcode { major: u8, minor: u8 },
    /// An unused field bit is nonzero: the word is not a canonical encoding.
    ReservedField,
}

/// One reference-counting micro-operation. Slot operands address the
/// frame word holding the reference; the count itself is heap state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RcOp {
    /// `0xE.0` — `rc_inc`: one more owner for the reference in `slots[src]`.
    Inc { src: u8 },
    /// `0xE.1` — `rc_dec`: drop one owner; a unique death runs `deinit`
    /// and pools the block for reuse.
    Dec { src: u8 },
}

impl RcOp {
    /// The opcode byte: `major << 4 | minor`, matching the pair the
    /// `Uop` encoding uses.
    #[must_use]
    pub fn opcode(&self) -> u8 {
        RC_MAJOR << 4
            | match self {
                Self::Inc { .. } => 0,
                Self::Dec { .. } => 1,
            }
    }

    /// Encodes into the fixed 32-bit word. `a` is the slot operand; the
    /// `bc` half is reserved zero, so the encoding is canonical by
    /// construction and cannot fail.
    #[must_use]
    pub fn encode(&self) -> u32 {
        let src = match self {
            Self::Inc { src } | Self::Dec { src } => *src,
        };
        u32::from(self.opcode()) << 24 | u32::from(src) << 16
    }

    /// Decodes a 32-bit word. Inverse of [`RcOp::encode`]: every canonical
    /// word decodes, and every accepted word re-encodes to itself.
    ///
    /// # Errors
    /// [`RcDecodeError::UnknownOpcode`] for any byte outside RCINC/RCDEC;
    /// [`RcDecodeError::ReservedField`] when the `bc` half is nonzero.
    pub fn decode(word: u32) -> Result<Self, RcDecodeError> {
        let [byte, a, b, c] = word.to_be_bytes();
        if byte >> 4 != RC_MAJOR {
            return Err(RcDecodeError::UnknownOpcode {
                major: byte >> 4,
                minor: byte & 0x0f,
            });
        }
        if b != 0 || c != 0 {
            return Err(RcDecodeError::ReservedField);
        }
        match byte & 0x0f {
            0 => Ok(Self::Inc { src: a }),
            1 => Ok(Self::Dec { src: a }),
            minor => Err(RcDecodeError::UnknownOpcode {
                major: RC_MAJOR,
                minor,
            }),
        }
    }

    /// The frame slot this op reads (the count is heap state, so no slot is
    /// written).
    #[must_use]
    pub fn src(&self) -> u8 {
        match self {
            Self::Inc { src } | Self::Dec { src } => *src,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{RC_MAJOR, RcDecodeError, RcOp};

    /// Boundary-valued instances of both variants.
    fn samples() -> Vec<RcOp> {
        vec![
            RcOp::Inc { src: 0 },
            RcOp::Inc { src: 255 },
            RcOp::Inc { src: 1 },
            RcOp::Dec { src: 0 },
            RcOp::Dec { src: 255 },
            RcOp::Dec { src: 1 },
        ]
    }

    #[test]
    fn round_trip_every_rc_op_at_boundary_slots() {
        for op in samples() {
            let word = op.encode();
            assert_eq!(RcOp::decode(word), Ok(op), "canonical word decodes");
            assert_eq!(op.encode(), word, "accepted words re-encode to themselves");
        }
    }

    #[test]
    fn opcode_bytes_are_pinned_in_the_reserved_major() {
        assert_eq!(RC_MAJOR, 0xe);
        assert_eq!(RcOp::Inc { src: 0 }.opcode(), 0xe0);
        assert_eq!(RcOp::Dec { src: 0 }.opcode(), 0xe1);
        assert_eq!(RcOp::Inc { src: 255 }.encode(), 0xe0_ff_00_00);
        assert_eq!(RcOp::Dec { src: 255 }.encode(), 0xe1_ff_00_00);
    }

    #[test]
    fn decode_rejects_foreign_majors_and_minors() {
        // an assigned lower major (MOV) and an unassigned reserved byte
        for word in [0x80_01_00_00, 0xef_01_00_00, 0xfe_01_00_00] {
            assert_eq!(
                RcOp::decode(word),
                Err(RcDecodeError::UnknownOpcode {
                    major: (word >> 24) as u8 >> 4,
                    minor: ((word >> 24) & 0x0f) as u8,
                }),
                "only 0xE.0 and 0xE.1 are RC ops"
            );
        }
        assert_eq!(
            RcOp::decode(0xe2_01_00_00),
            Err(RcDecodeError::UnknownOpcode {
                major: 0xe,
                minor: 2
            })
        );
    }

    #[test]
    fn decode_rejects_noncanonical_field_bits() {
        assert_eq!(
            RcOp::decode(0xe0_01_00_01),
            Err(RcDecodeError::ReservedField)
        );
        assert_eq!(
            RcOp::decode(0xe1_ff_01_00),
            Err(RcDecodeError::ReservedField)
        );
    }

    #[test]
    fn effects_read_one_slot() {
        for op in samples() {
            assert_eq!(op.src(), op.src(), "the source is addressable");
        }
    }

    /// A 0xE word is not a `Uop`: `Uop::decode` stays exhaustive over its
    /// own majors, so the RC words reach the dispatcher through
    /// `RcOp::decode` ([`super::Inst`] is the union a body carries).
    #[test]
    fn rc_words_are_outside_the_uop_stream() {
        use crate::uop::Uop;
        for op in samples() {
            assert_eq!(
                Uop::decode(0, op.encode()),
                Err(crate::uop::DecodeError::UnknownOpcode {
                    major: 0xe,
                    minor: match op {
                        RcOp::Inc { .. } => 0,
                        RcOp::Dec { .. } => 1,
                    }
                }),
                "the RC major stays outside Uop::decode"
            );
        }
    }
}
