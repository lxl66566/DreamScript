//! # The DreamScript call ABI: the locked slot-overlay contract
//!
//! This crate is spec-as-code: the portable contract that `ds-vm` (tier 1),
//! `ds-lower` (frame layout assignment), and `ds-jit` (tiers 2-3) implement.
//! It owns no runtime state; every item is a data model or a pure function
//! the three consumers must agree on exactly.
//!
//! The contract, numbered:
//!
//! 1. Typed frame layout. Every frame is a contiguous span of statically typed slots plus a
//!    metadata word (return address, frame size, TCO-chain flags) and a call word hosting a
//!    callee's metadata during a call. Slot types are fixed at compile time: [`SlotType`],
//!    [`FrameLayout`], [`FrameMetadata`].
//! 2. Slot-overlay convention. A callee's argument slots overlay the caller's outgoing-argument
//!    area, which is the tail of the caller's frame. On a normal call the callee frame begins at
//!    the caller's call word with no argument copying; on a tail call the current frame is reused
//!    at the same base, so a tail call is an O(delta) argument slide plus a jump, and the stack
//!    never grows at any depth, in any tier.
//! 3. CALLT semantics. Slide the staged arguments into the frame base ([`slide_plan`]), update the
//!    metadata word in place (return address preserved, frame size replaced, tail chain extended
//!    [`FrameMetadata::enter_tail_call`]), then enter the callee through its entry descriptor.
//!    Argument words move as raw words: a tail call moving arguments transfers ownership with zero
//!    RC traffic.
//! 4. Two-word entry descriptor. Every function has an [`EntryDescriptor`] `{tier1_prologue_ptr,
//!    tier2_code_ptr}`. Word 0 is written once at module load; word 1 flips from the null sentinel
//!    to compiled code exactly once when the callee compiles (one-way [`Tier`] flip). The flip
//!    shares its invalidation story with hot reload: a reload publishes a fresh descriptor in the
//!    second module slot; old frames retire at their next cross-module call.
//! 5. Tail-chain backtraces. The metadata word carries a per-frame TCO-chain counter
//!    ([`TailChain`]). A backtrace renders the count directly; when debug recording is enabled,
//!    per-elision [`ElisionRecord`]s let a debugger lazily materialize the elided logical frames.
//! 6. The uop set. The instruction contract every tier shares — tier-1 dispatch, the copy-and-patch
//!    stencil source, and the bytecode-to-SSA lowering — is the one [`Uop`] enum defined in this
//!    crate: typed opcodes, 32-bit fixed encoding, biased jumps, XOR-negated comparisons, and
//!    per-uop slot effects with `pure` annotations. The exact bit layout (opcode major/minor, `a`
//!    slot, `b`/`c` slots or `d` immediate, bias value) is in the [`uop`] module docs. This item is
//!    an ADDITION to the call ABI, not a modification of it; it consumes the five items above
//!    (frame layouts, slide plan, entry descriptors) without changing them.
//!
//! The tail-call guarantee is scoped to tail calls within the same effect
//! context; handler entry materializes a frame.
//!
//! ## Lock status
//!
//! Every item in this crate is locked: three tiers and the bytecode format consume these words,
//! so a change to any item needs written justification carried with the change.
//!
//! ## Word model
//!
//! A slot is one machine word holding a raw unboxed value; this crate
//! models slot words as `u64`. Bit encodings within a word (i32 placement,
//! bool, reference representation) belong to the tier-1 VM, not to this
//! contract.
//!
//! ## Native vocabulary
//!
//! The [`native`] module holds the native tables both the lowering and the
//! tier-1 VM read: the builtin core's rows, the platform world-op table, the
//! comptime reflection module, the generic-call rows, and the reserved
//! discriminants and module names they share.

mod descriptor;
mod layout;
mod meta;
pub mod native;
mod tailcall;
pub mod uop;

pub use descriptor::{EntryDescriptor, Tier, TierFlipError};
pub use layout::{BoundRet, FrameLayout, LayoutError, SlotType, Staging};
pub use meta::{CodePc, ElisionRecord, FrameMetadata, FuncId, TailChain};
pub use native::{FailureClasses, GenericNative, Native, NativeModule, NativeRouting, NativeShape};
pub use tailcall::{SlideDirection, SlideError, SlidePlan, slide_plan};
pub use uop::{
    BIAS_J, BuiltinInstance, ConstIdx, DecodeError, EncodeError, Inst, InstDecodeError, Major,
    RC_MAJOR, RcDecodeError, RcOp, SlotList, Uop,
};
