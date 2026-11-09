//! Unboxed slot values [BL-2] and the words of the typed value stack [BL-6].

use ds_abi::SlotType;

/// A raw unboxed scalar in one frame slot [BL-2]: no NaN-boxing, no per-slot
/// tags, no boxed scalars. Covers every kind the uop set operates on
/// [BL-5]: the scalars, union tag words, and the reference word into the
/// VM's heap object store (M3: string objects; [`crate::object::Objects`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Value {
    /// A 64-bit integer, the `int` default [B7].
    I64(i64),
    /// A 64-bit float, the `float` default [B7].
    F64(f64),
    /// An unboxed boolean.
    Bool(bool),
    /// The tag word of an unboxed tag+payload union [BL-2]: a bare
    /// discriminant in one slot; payload words are the following slots.
    Tag(u8),
    /// A raw reference to a heap object: the index into the VM's object
    /// table. `Ref` slots hold only these words, so no null niche is spent
    /// and `T | none` packing stays a legal later pass [B4].
    Ref(u32),
}

impl Value {
    /// The static slot type this value inhabits.
    #[must_use]
    pub fn slot_type(&self) -> SlotType {
        match self {
            Self::I64(_) => SlotType::I64,
            Self::F64(_) => SlotType::F64,
            Self::Bool(_) => SlotType::Bool,
            Self::Tag(_) => SlotType::Tag,
            Self::Ref(_) => SlotType::Ref,
        }
    }
}

/// One word of the value stack: a value slot, or a frame's metadata word.
///
/// Frames are contiguous spans of [`Word`]s inline in the stack [BL-6]: the
/// metadata word at the frame base, then the statically typed slots, then the
/// call word (which hosts a callee's metadata word during a call) and the
/// outgoing-argument area — see the layout diagram in `ds-abi/src/layout.rs`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Word {
    /// A value slot word.
    Slot(Value),
    /// The metadata word at a frame base, or in a call word hosting a
    /// callee's metadata during a call.
    Meta(ds_abi::FrameMetadata),
}
