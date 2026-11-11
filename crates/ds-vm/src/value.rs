//! Unboxed slot values [BL-2] and the words of the typed value stack [BL-6].

use ds_abi::SlotType;

use crate::fiber::FiberId;

/// The scalar callable word (the `Fn` slot classification, F2): a module
/// id plus a function-table index packed into one copyable word. Never a
/// code pointer — resolution goes through the two-slot module table at
/// call time, so a staged word picks up a swapped module at its next call
/// [H6 item 5]. The module id space is the registry's (one id per module
/// name, assigned at first install) until the ds-mod link table assigns
/// module indices.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FuncRef {
    module: u32,
    func: u32,
}

impl FuncRef {
    /// Packs a module id and a function-table index into the word.
    #[must_use]
    pub const fn new(module: u32, func: u32) -> Self {
        Self { module, func }
    }

    /// The target module's registry id.
    #[must_use]
    pub const fn module(self) -> u32 {
        self.module
    }

    /// The target function's table index.
    #[must_use]
    pub const fn func(self) -> u32 {
        self.func
    }
}

/// A raw unboxed scalar in one frame slot [BL-2]: no NaN-boxing, no per-slot
/// tags, no boxed scalars. Covers every kind the uop set operates on
/// [BL-5]: the scalars, union tag words, the reference word into the
/// VM's heap object store (M3: string objects; [`crate::object::Objects`]),
/// and the callable word (the `Fn` classification, F2).
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
    /// A raw reference to a heap object: the owner-routing word, the
    /// owning fiber plus the object id inside that fiber's store
    /// (the word self-describes its owner, so a delivered or host-held
    /// word routes to its heap anywhere). `Ref` slots hold only these
    /// words, so no null niche is spent and `T | none` packing stays a
    /// legal later pass [B4].
    Ref(RefId),
    /// A callable word (the `Fn` classification, F2): the scalar
    /// [`FuncRef`] naming a non-capturing function. Copied by MOV like any
    /// scalar, zero RC traffic. A capturing lambda is not this word: it is
    /// a `Ref` word to a closure object whose header word is the FuncRef.
    Fn(FuncRef),
}

/// The `Ref` word's payload: the owning fiber plus the object id inside
/// that fiber's store. Each fiber owns its heap, so the word
/// self-describes its owner and routes to its heap from any hand: a
/// delivered or host-held word serves wherever it lands. Runtime words
/// never serialize, so the pair rides the payload with no dsmod impact.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RefId {
    /// The owning fiber.
    fiber: FiberId,
    /// The object id inside the owning fiber's store.
    id: u32,
}

impl RefId {
    /// The word of object `id` inside `owner`'s store. The owner names
    /// the pool slot, so the component stays stable across slot reuse —
    /// a foreign-held object outlives its dead owner's recycling and the
    /// next spawn continues the same id space.
    #[must_use]
    pub const fn new(owner: FiberId, id: u32) -> Self {
        Self { fiber: owner, id }
    }

    /// The object id inside the owning fiber's store.
    #[must_use]
    pub const fn id(self) -> u32 {
        self.id
    }

    /// The owning fiber.
    #[must_use]
    pub const fn fiber(self) -> FiberId {
        self.fiber
    }
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
            Self::Fn(_) => SlotType::Fn,
        }
    }
}

/// One word of the value stack: a value slot, or a frame's metadata word.
///
/// Frames are contiguous spans of [`Word`]s inline in the stack [BL-6]: the
/// metadata word at the frame base, then the statically typed slots, then
/// the call word (which hosts a callee's metadata word during a call) and
/// the outgoing-argument area — see the layout diagram in `ds-abi/src/layout.rs`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Word {
    /// A value slot word.
    Slot(Value),
    /// The metadata word at a frame base, or in a call word hosting a
    /// callee's metadata during a call.
    Meta(ds_abi::FrameMetadata),
}
