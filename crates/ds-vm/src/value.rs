//! Unboxed slot values and the words of the typed value stack.

use ds_abi::SlotType;

use crate::fiber::FiberId;

/// The scalar callable word (the `Fn` slot classification): a module
/// id plus a function-table index packed into one copyable word. Never a
/// code pointer — resolution goes through the two-slot module table at
/// call time, so a staged word picks up a swapped module at its next call.
/// The module id space is the registry's: one id per module name, assigned at
/// first install.
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
    #[inline]
    pub const fn module(self) -> u32 {
        self.module
    }

    /// The target function's table index.
    #[must_use]
    #[inline]
    pub const fn func(self) -> u32 {
        self.func
    }
}

/// A raw unboxed scalar in one frame slot: no NaN-boxing, no per-slot
/// tags, no boxed scalars. Covers every kind the uop set operates on:
/// the scalars, union tag words, the reference word into the VM's heap
/// object store (string objects; [`crate::object::Objects`]), and the
/// callable word (the `Fn` classification).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Value {
    /// A 64-bit integer, the `int` default.
    I64(i64),
    /// A 64-bit float, the `float` default.
    F64(f64),
    /// An unboxed boolean.
    Bool(bool),
    /// The tag word of an unboxed tag+payload union: a bare
    /// discriminant in one slot; payload words are the following slots.
    Tag(u8),
    /// A raw reference to a heap object: the owner-routing word, the
    /// owning fiber plus the object id inside that fiber's store
    /// (the word self-describes its owner, so a delivered or host-held
    /// word routes to its heap anywhere). `Ref` slots hold only these
    /// words, so no null niche is spent and `T | none` packing stays a
    /// legal later pass.
    Ref(RefId),
    /// A callable word (the `Fn` classification): the scalar
    /// [`FuncRef`] naming a non-capturing function. Copied by MOV like any
    /// scalar, zero RC traffic. A capturing lambda is not this word: it is
    /// a `Ref` word to a closure object whose header word is the FuncRef.
    Fn(FuncRef),
    /// An arena temporary: the packed handle into the running fiber's
    /// frame arena. Inhabits `Ref` slots (its static slot kind is the
    /// string word's), carries no count, and reaches no count-bearing
    /// funnel — the placement pass proves its lifetime inside its own
    /// loop iteration or frame, so the arena serves and frees its bytes.
    /// The string data ops are its only readers.
    Arena(ArenaStr),
}

/// One arena slice's length limit, the packed handle's 24-bit field.
pub const ARENA_LEN_LIMIT: usize = 1 << 24;
/// One arena's chunk-index limit, the packed handle's 16-bit field.
pub const ARENA_CHUNK_LIMIT: usize = 1 << 16;

/// The `Arena` word's payload: the packed handle — chunk index 16 bits,
/// byte offset 24 bits, byte length 24 bits — so the value stays one
/// 16-byte word. Runtime words
/// never serialize, so the packing has no dsmod impact; pack boundaries
/// trap by name rather than truncating.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ArenaStr {
    raw: u64,
}

/// Which packed field overflowed [`ArenaStr::pack`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArenaPackError {
    /// The slice length sits at or past [`ARENA_LEN_LIMIT`].
    Length,
    /// The chunk offset sits at or past [`ARENA_LEN_LIMIT`].
    Offset,
    /// The chunk index sits at or past [`ARENA_CHUNK_LIMIT`].
    Chunk,
}

impl ArenaStr {
    /// Packs a position and length; a field at its limit is a refusal,
    /// never a truncation.
    #[inline]
    pub fn pack(chunk: usize, offset: usize, len: usize) -> Result<Self, ArenaPackError> {
        if len >= ARENA_LEN_LIMIT {
            return Err(ArenaPackError::Length);
        }
        if offset >= ARENA_LEN_LIMIT {
            return Err(ArenaPackError::Offset);
        }
        if chunk >= ARENA_CHUNK_LIMIT {
            return Err(ArenaPackError::Chunk);
        }
        Ok(Self {
            raw: len as u64 | (offset as u64) << 24 | (chunk as u64) << 48,
        })
    }

    /// The packed handle, for the cursor word an ARENA_MARK writes into
    /// an `i64` slot (the restore unpacks it without re-reading arena
    /// state).
    #[must_use]
    #[inline]
    pub fn raw(self) -> u64 {
        self.raw
    }

    /// Unpacks `raw` into its chunk index, byte offset, and length.
    /// Construction confined each field to its mask width, so no read
    /// narrows a value and unpacking re-validates nothing `pack` proved.
    #[must_use]
    #[inline]
    pub fn unpack_raw(raw: u64) -> (usize, usize, usize) {
        (
            usize::try_from(raw >> 48).expect("chunk index fits usize"),
            usize::try_from((raw >> 24) & 0xff_ffff).expect("offset fits usize"),
            usize::try_from(raw & 0xff_ffff).expect("length fits usize"),
        )
    }

    /// Unpacks this handle into its chunk index, byte offset, and length.
    #[must_use]
    #[inline]
    pub fn unpack(self) -> (usize, usize, usize) {
        Self::unpack_raw(self.raw)
    }
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
    #[inline]
    pub const fn id(self) -> u32 {
        self.id
    }

    /// The owning fiber.
    #[must_use]
    #[inline]
    pub const fn fiber(self) -> FiberId {
        self.fiber
    }
}

impl Value {
    /// The static slot type this value inhabits: one read of the word's
    /// tag, holding no per-variant state.
    #[must_use]
    #[inline]
    pub fn slot_type(&self) -> SlotType {
        match self {
            Self::I64(_) => SlotType::I64,
            Self::F64(_) => SlotType::F64,
            Self::Bool(_) => SlotType::Bool,
            Self::Tag(_) => SlotType::Tag,
            Self::Ref(_) | Self::Arena(_) => SlotType::Ref,
            Self::Fn(_) => SlotType::Fn,
        }
    }
}

/// One word of the value stack: a value slot, or a frame's metadata word.
///
/// Frames are contiguous spans of [`Word`]s inline in the stack: the
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The widest legal triple: every field one below its limit.
    const WIDEST: (usize, usize, usize) = (
        ARENA_CHUNK_LIMIT - 1,
        ARENA_LEN_LIMIT - 1,
        ARENA_LEN_LIMIT - 1,
    );

    #[test]
    fn a_packed_handle_round_trips_at_every_field_maximum() {
        // construction proved each field inside its limit, so the unpack is
        // mask-only: no read can fail and no field overwrites a neighbour
        let ArenaStr { raw } = ArenaStr::pack(WIDEST.0, WIDEST.1, WIDEST.2)
            .expect("every field sits one below its limit");
        assert_eq!(ArenaStr::unpack_raw(raw), WIDEST);
        assert_eq!(
            ArenaStr::pack(WIDEST.0, WIDEST.1, WIDEST.2)
                .expect("packed")
                .unpack(),
            WIDEST
        );
        assert_eq!(ArenaStr::pack(0, 0, 0).expect("an empty slice").raw(), 0);
        assert_eq!(ArenaStr::pack(0, 0, 1).expect("one byte").raw(), 1);
        assert_eq!(ArenaStr::pack(0, 1, 0).expect("an offset").raw(), 1 << 24);
        assert_eq!(ArenaStr::pack(1, 0, 0).expect("a chunk").raw(), 1 << 48);
    }

    #[test]
    fn a_field_at_its_limit_is_a_named_refusal() {
        assert_eq!(
            ArenaStr::pack(0, 0, ARENA_LEN_LIMIT),
            Err(ArenaPackError::Length)
        );
        assert_eq!(
            ArenaStr::pack(0, ARENA_LEN_LIMIT, 0),
            Err(ArenaPackError::Offset)
        );
        assert_eq!(
            ArenaStr::pack(ARENA_CHUNK_LIMIT, 0, 0),
            Err(ArenaPackError::Chunk)
        );
    }
}
