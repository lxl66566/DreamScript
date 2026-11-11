//! # The tier-1 VM: the ds-abi uop contract made executable
//!
//! This crate is the tier-1 interpreter of `ds-vm`: a typed value stack with
//! frames inline in it [BL-6], a two-slot module table [H6 item 5], and a
//! dense-match dispatch loop [BL-3 note: stable Rust has no computed goto]
//! over encoded programs. It executes the one instruction contract every
//! tier shares — `ds-abi`'s [`Uop`] set [BL-5] — from its canonical 32-bit
//! word encoding [BL-1], and proves the locked slot-overlay call ABI with
//! hand-assembled fixture programs (the canonical `sum_to` and
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
//!   target's current slot [H6 item 5]. The dsmod loader maps the payload's `FuncRef` constant kind
//!   onto these at install (see `loader`'s module docs).
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
//! - `CALLS` executes the callable-word call form (F2): the callee operand is a frame slot — a `Fn`
//!   slot holds the copyable scalar `FuncRef` word (module id + function index; the id space is
//!   registry-assigned), a `Ref` slot holds a closure object whose header word is the FuncRef and
//!   whose capture block follows inside the object ([`crate::object::Objects::alloc_closure`]).
//!   Resolution goes through the target module's current slot at call time — never a cached code
//!   pointer, so staged words pick up hot-reload swaps — and a closure callee's captures are copied
//!   into its frame starting at the first local slot. No tail variant exists; every CALLS is an
//!   ordinary frame push bounded by the stack limit.
//! - Entries. Every function carries a two-word [`ds_abi::EntryDescriptor`]; word 0 (the function's
//!   table index, written once at module load and immutable after) is all that exists until the JIT
//!   does. All calls route through [`ds_abi::EntryDescriptor::resolve`].
//! - Modules. A two-slot module table [H6 item 5]: each module holds a current and at most one
//!   retiring version; every call resolves through the current slot, so a swap redirects subsequent
//!   calls immediately while live frames keep running their entered version's code and pool.
//! - Backtraces. On a trap, [`Vm::backtrace`] reports each real frame plus its tail-chain elided
//!   count [H2]; lazy materialization of elided frames from per-elision records is deferred to
//!   per-fiber debug recording.
//! - Strings and the heap. String constants ride the pool and load through KSTR as raw `Ref` words
//!   into a VM-owned object store ([`crate::object::Objects`]); STRCAT concatenates (fresh objects,
//!   never interned), EQSTR/NEQSTR compare contents, not identity. No reclamation happens in M3 —
//!   objects live for the VM's lifetime; M4's Perceus placement pass inserts dup/drop uops and
//!   precise RC over this same table.
//! - Builtins. Native (Rust) functions live in two reserved modules: the `builtin` core
//!   ([`crate::native::BUILTIN_MODULE`], the pure `str` conversions and string ops) that every VM
//!   installs into its registry, and the `std` platform module
//!   ([`crate::native::STD_PLATFORM_MODULE`], the `world`-effect surface — console, file IO, net
//!   IO) that the host providing a platform installs explicitly. A pool `FuncRef { module, func: i
//!   }` resolves through the same path as any uop callee, so `CALL` is the only call mechanism. A
//!   native call never pushes a frame: it reads the caller's staged overlay words and overlays its
//!   result on argument slot 0 where a RET would. Chosen over a dedicated uop per builtin because
//!   CALL already is the ISA's function-call mechanism — a uop per builtin grows the locked opcode
//!   set for every stdlib addition, while the native-module route adds builtins as table entries
//!   with arity and typing checks in one place. The fallible natives answer with a two-word
//!   tag+payload span over the caller's overlay slots, with the result-span and platform tag-id
//!   conventions documented in `native`'s module docs.
//! - Multi-word values. BL-2 values (payloaded unions, structs) span consecutive slots. RETSPAN
//!   returns such a span: the words land on the caller's outgoing-argument slots `0..n`, whose
//!   width the lowering must size for the widest callee result it calls (the VM re-checks). Frames
//!   whose overlay words carry more than one call shape — staged arguments of one type, then a
//!   result span of another — declare those slots `SlotType::Raw`: every write accepts, every read
//!   checks the value's concrete type, and call sites check the staged value words.
//! - Fibers. The VM owns a single-threaded fiber pool [E2]: each fiber's value-stack segment is a
//!   chunk list that grows by chunk append, never realloc, so frame addresses stay stable [H2]; TCO
//!   holds per fiber; a switch swaps segment/call-chain ownership (O(1), no copying); a fiber
//!   completing at its entry frame marks itself dead and switches back to its resumer. The debug
//!   surface is the builtin control pair (`fiber_yield`/`fiber_resume`) plus
//!   [`Vm::spawn_fiber`]/[`Vm::resume_fiber`].
//! - Nurseries. Fibers spawn into a nursery (the child-of link) [E2]; the join driver drives
//!   children to death in spawn order and unions per-child outcomes out at the boundary; a first
//!   failure raises the nursery's cancel flag, observed only at taken backward branches [E5] — a
//!   linear consume in flight is never interrupted; teardown drives live children to death under
//!   the flag before releasing the nursery. The debug surface is `nursery_join` (builtin 26, the
//!   failed-children count) plus the host face [`Vm::open_root_nursery`]/[`Vm::spawn_fiber_in`]/
//!   [`Vm::join_nursery`]/[`Vm::teardown_nursery`]; the in-language `nursery`/`spawn` surface lands
//!   with the checker/lowerer rounds.

// Bracketed decision IDs ([H2], [BL-6], ...) cite the design record in
// docs/en/design-decisions.md; they are references, not doc links.
#![allow(rustdoc::broken_intra_doc_links)]

/// The reserved FuncRef module name a runtime-resolved trait dispatch
/// site inside a generic impl body carries (H3): the FuncRef's `func`
/// field names the calling frame's evidence slot, whose staged word
/// holds the serving member body's function-table index in the
/// enclosing module. `$` sits outside the identifier charset, so no
/// user package can collide.
pub const EVIDENCE_MODULE: &str = "$evidence";

mod claims;
pub mod fiber;
pub mod loader;
pub mod module;
pub mod native;
mod net;
pub mod nursery;
pub mod object;
pub mod program;
pub mod value;
pub mod vm;

/// The locked contracts this VM implements, re-exported for callers.
pub use ds_abi as abi;
pub use fiber::{FIBER_ROOT, FiberId, FiberState};
pub use loader::{LoadError, install as install_dsmod};
pub use module::{Function, ModuleBuildError, ModuleRegistry, ModuleSlots, ModuleVersion};
pub use native::{ControlNative, NativeFunction, NativeResult, OutputSink};
pub use nursery::{ChildOutcome, JoinOutcome, NurseryId, NurseryState};
pub use program::{Const, ConstKind, FunctionDef, assemble};
pub use value::{FuncRef, RefId, Value, Word};
pub use vm::{BacktraceFrame, Trap, Vm};
