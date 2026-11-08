//! # The tier-1 VM: the ds-abi uop contract made executable
//!
//! This crate is the tier-1 interpreter of `ds-vm`: a typed value stack with
//! frames inline in it [BL-6], a two-slot module table [H6 item 5], and a
//! dense-match dispatch loop [BL-3 note: stable Rust has no computed goto]
//! over encoded programs. It executes the one instruction contract every
//! tier shares — `ds-abi`'s [`Uop`] set [BL-5] — from its canonical 32-bit
//! word encoding [BL-1], and proves the locked slot-overlay call ABI with
//! hand-assembled fixture programs (the record's section 13.7 `sum_to` and
//! mutual `even`/`odd`).
//!
//! How the contract maps onto this crate:
//!
//! - Words. The stack is a `Vec<Word>` where a word is either an unboxed value slot ([`Value`]; no
//!   NaN-boxing, no boxed scalars [BL-2]) or a frame metadata word modeled by `ds-abi`'s
//!   [`ds_abi::FrameMetadata`]. Frame geometry (metadata word, parameters, locals, call word,
//!   outgoing-argument area) follows the diagram in `ds-abi/src/layout.rs`; slot `i` lives at `fp +
//!   1 + i` in its owner's frame.
//! - Programs. A function is `Vec<u32>` — the canonical encoding — plus its own constant pool (the
//!   dsmod blocks-payload granularity; uop `ConstIdx` operands index it): `KIMM` loads i64 entries,
//!   `KCONST` loads f64/bool entries, and `CALL`/`CALLT` load [`Const::FuncRef`] callees.
//!   [`assemble`] encodes a uop body; module load decode-checks every word, cross-checks `FUNCF`
//!   against the layout, and validates every pool access; the dispatcher decodes on dispatch (see
//!   `vm`'s module docs for why no pre-decoded cache).
//! - Callee addressing. The contract's FuncRef is "module index + function index"; this registry is
//!   name-keyed until the ds-mod link table assigns module indices, so a pool FuncRef carries the
//!   target module's name plus the function-table index (entry word 0), resolved through the
//!   target's current slot [H6 item 5]. The dsmod loader resolves qualified `"module.function"`
//!   string entries into these at install (see `loader`'s module docs).
//! - Non-tail `CALL` pushes a frame through the call word: the callee's metadata word is written
//!   into the caller's call word, so the callee's parameter slots overlay the caller's
//!   outgoing-argument area with zero argument copying, and the stack grows by exactly the callee's
//!   remaining frame words after the per-entry stack-limit compare [H1].
//! - `CALLT` implements the locked tail-call contract exactly [H2]: the frame pointer is kept, the
//!   staged arguments slide into slots `0..n` per `ds-abi`'s [`ds_abi::slide_plan`] (source derived
//!   from the caller's arg area, destination owner-relative per the pinned frame-owner word map),
//!   the metadata word is rewritten in place via [`ds_abi::FrameMetadata::enter_tail_call`] (return
//!   pc preserved, callee frame size, elided count +1), and control enters the callee. The stack
//!   never grows at any depth.
//! - Entries. Every function carries a two-word [`ds_abi::EntryDescriptor`]; word 0 (the function's
//!   table index, written once at module load and immutable after) is all that exists until the JIT
//!   does. All calls route through [`ds_abi::EntryDescriptor::resolve`].
//! - Modules. A two-slot module table [H6 item 5]: each module holds a current and at most one
//!   retiring version; every call resolves through the current slot, so a swap redirects subsequent
//!   calls immediately while live frames keep running their entered version's code and pool.
//! - Backtraces. On a trap, [`Vm::backtrace`] reports each real frame plus its tail-chain elided
//!   count [H2]; lazy materialization of elided frames from per-elision records is deferred to
//!   per-fiber debug recording.

// Bracketed decision IDs ([H2], [BL-6], ...) cite the design record in
// docs/en/design-decisions.md; they are references, not doc links.
#![allow(rustdoc::broken_intra_doc_links)]

pub mod loader;
pub mod module;
pub mod program;
pub mod value;
pub mod vm;

/// The locked contracts this VM implements, re-exported for callers.
pub use ds_abi as abi;
pub use loader::{LoadError, install as install_dsmod};
pub use module::{Function, ModuleBuildError, ModuleRegistry, ModuleSlots, ModuleVersion};
pub use program::{Const, ConstKind, FunctionDef, assemble};
pub use value::{Value, Word};
pub use vm::{BacktraceFrame, Trap, Vm};
