//! DreamScript runtime core: the M4 memory machinery.
//!
//! Two halves over one per-fiber [`Heap`] owner:
//!
//! - Bump arenas for non-escaping temporaries ([`Arena`]): mark/save at entry, bump alloc, restore
//!   on every pop path; chunk-append growth keeps captured frame addresses stable; marks are
//!   per-fiber so fiber switching needs no arena action. The arena module's docs carry the full
//!   contract mapping from the design record ([C4], [H4] in `docs/en/design-decisions.md`).
//! - Precise non-atomic reference counting for boxed/shared values ([`RcHeader`], [`RcRef`]):
//!   `rc_inc` at each explicit clone, `rc_dec` at each unique death; the dying block becomes a
//!   reuse candidate threaded to the next compatible allocation (the `alloc_at` pattern), with the
//!   system allocator as fallback; `deinit` runs exactly once at the unique death on every path -
//!   normal, early return, panic unwind ([C5]). The counts are non-atomic and sound because values
//!   are thread-confined; E3's fork-join transfers ownership of whole heaps, never shares them.
//!
//! The `diag` feature (default off) adds the debugging instrument for the
//! whole memory story: rc underflow/overflow tripwires, rc==1 assertions
//! before destructive operations, and an Inko-style borrow counter that flags
//! use during an exclusive window.
//!
//! VM seam: ds-vm's object store consumes the block half as its object
//! substrate - the store keeps the stable id table (the `Value::Ref`
//! word's id part indexes it), and every live id names a counted block here. The store drives
//! count lifecycles through the raw surface ([`Heap::alloc_store`],
//! [`Heap::rc_inc_raw`], [`Heap::rc_dec_count`] plus [`Heap::rc_dispose`],
//! [`Heap::payload`]/[`Heap::payload_mut`]) so its own death order runs
//! between the count reaching zero and the payload dying; the header layout
//! is the committed `repr(C)` format carrying the dormant two-hook collector
//! word. The arena half stays unconsumed until the temporaries stage lands
//! (per-frame marks and chunk-append growth already fit the fiber substrate).
//!
//! Confinement: this crate is the unsafe core of the runtime (`ds-rt` in the
//! implementation plan); the rest of the workspace consumes it through safe
//! APIs. Every unsafe block carries a safety comment.
//!
//! Miri is not available on this Windows setup; the unsafe code is nonetheless
//! written to miri rules (in-bounds pointer arithmetic, matched alloc/dealloc
//! layouts, provenance-free dangling pointers for zero-sized requests).
//!
//! [C4]: https://github.com/lxl66566/DreamScript/blob/main/docs/en/design-decisions.md
//! [H4]: https://github.com/lxl66566/DreamScript/blob/main/docs/en/design-decisions.md
//! [C5]: https://github.com/lxl66566/DreamScript/blob/main/docs/en/design-decisions.md

mod arena;
mod guard;
mod heap;
mod rc;

pub use arena::{Arena, ArenaStats, FIRST_CHUNK, MAX_CHUNK, MIN_ALIGN, Mark};
pub use guard::RestoreOnDrop;
pub use heap::{Heap, HeapStats, MAX_REUSE_CLASS, MIN_CLASS, POOL_DEPTH};
pub use rc::{DeinitFn, ExclusiveBorrow, RcHeader, RcRef, SharedBorrow, drop_deinit};
