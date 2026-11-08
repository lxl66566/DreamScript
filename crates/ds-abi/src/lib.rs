//! # The DreamScript call ABI: the locked slot-overlay contract [BL-6, H2]
//!
//! This crate is spec-as-code: the portable contract that `ds-vm` (tier 1),
//! `ds-lower` (frame layout assignment), and `ds-jit` (tiers 2-3) implement.
//! It owns no runtime state; every item is a data model or a pure function
//! the three consumers must agree on exactly.
//!
//! The contract, numbered:
//!
//! 1. Typed frame layout [BL-6]. Every frame is a contiguous span of statically typed slots plus a
//!    metadata word (return address, frame size, TCO-chain flags) and a call word hosting a
//!    callee's metadata during a call. Slot types are fixed at compile time: [`SlotType`],
//!    [`FrameLayout`], [`FrameMetadata`].
//! 2. Slot-overlay convention [H2]. A callee's argument slots overlay the caller's
//!    outgoing-argument area, which is the tail of the caller's frame. On a normal call the callee
//!    frame begins at the caller's call word with no argument copying; on a tail call the current
//!    frame is reused at the same base, so a tail call is an O(delta) argument slide plus a jump,
//!    and the stack never grows at any depth, in any tier.
//! 3. CALLT semantics. Slide the staged arguments into the frame base ([`slide_plan`]), update the
//!    metadata word in place (return address preserved, frame size replaced, tail chain extended
//!    [`FrameMetadata::enter_tail_call`]), then enter the callee through its entry descriptor.
//!    Argument words move as raw words: a tail call moving arguments transfers ownership with zero
//!    RC traffic [H2].
//! 4. Two-word entry descriptor. Every function has an [`EntryDescriptor`] `{tier1_prologue_ptr,
//!    tier2_code_ptr}`. Word 0 is written once at module load; word 1 flips from the null sentinel
//!    to compiled code exactly once when the callee compiles (one-way [`Tier`] flip). The flip
//!    shares its invalidation story with hot reload [H6 item 5]: a reload publishes a fresh
//!    descriptor in the second module slot; old frames retire at their next cross-module call.
//! 5. Tail-chain backtraces. The metadata word carries a per-frame TCO-chain counter
//!    ([`TailChain`]). A backtrace renders the count directly; when debug recording is enabled,
//!    per-elision [`ElisionRecord`]s let a debugger lazily materialize the elided logical frames.
//! 6. The uop set [BL-5], locked from M3. The instruction contract every tier shares — tier-1
//!    dispatch, the copy-and-patch stencil source, and the bytecode-to-SSA lowering — is the one
//!    [`Uop`] enum defined in this crate: typed opcodes, 32-bit fixed encoding, biased jumps,
//!    XOR-negated comparisons, and per-uop slot effects with `pure` annotations. The exact bit
//!    layout (opcode major/minor, `a` slot, `b`/`c` slots or `d` immediate, bias value) is in the
//!    [`uop`] module docs. This item is an ADDITION to the M0 call ABI, not a modification of it;
//!    it consumes items 1-5 (frame layouts, slide plan, entry descriptors) without changing them.
//!
//! The tail-call guarantee is scoped to tail calls within the same effect
//! context; handler entry materializes a frame [H2].
//!
//! ## Lock status
//!
//! Items 1-5 are LOCKED from M0; item 6 is LOCKED from M3. Any change to any
//! item in this crate requires written justification recorded in
//! `docs/en/design-decisions.md` before the change lands.
//!
//! ## Word model
//!
//! A slot is one machine word holding a raw unboxed value [BL-2]; this crate
//! models slot words as `u64`. Bit encodings within a word (i32 placement,
//! bool, reference representation) belong to the tier-1 VM, not to this
//! contract.

// Bracketed decision IDs ([H2], [BL-6], ...) cite the design record in
// docs/en/design-decisions.md; they are references, not doc links.
#![allow(rustdoc::broken_intra_doc_links)]

mod descriptor;
mod layout;
mod meta;
mod tailcall;
pub mod uop;

pub use descriptor::{EntryDescriptor, Tier, TierFlipError};
pub use layout::{FrameLayout, LayoutError, SlotType};
pub use meta::{CodePc, ElisionRecord, FrameMetadata, FuncId, TailChain};
pub use tailcall::{SlideDirection, SlideError, SlidePlan, slide_plan};
pub use uop::{BIAS_J, ConstIdx, DecodeError, EncodeError, Major, SlotList, Uop};
