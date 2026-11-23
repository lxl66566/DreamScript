//! DreamScript runtime core: the M4 memory machinery.
//!
//! Two halves over one per-fiber [`Heap`] owner:
//!
//! - Bump arenas for non-escaping temporaries ([`Arena`]): mark/save at entry, bump alloc, restore
//!   on every pop path; chunk-append growth keeps captured frame addresses stable; marks are
//!   per-fiber so fiber switching needs no arena action. The arena module's docs carry the full
//!   contract mapping from the design record (`[C4]`, `[H4]` in `docs/en/design-decisions.md`).
//! - Precise non-atomic reference counting for boxed/shared values ([`RcHeader`]): `rc_inc` at each
//!   explicit clone, `rc_dec` at each owner drop; the dying block becomes a reuse candidate
//!   threaded to the next compatible allocation (the `alloc_at` pattern), with the system allocator
//!   as fallback; `deinit` runs exactly once at the unique death (`[C5]`). The counts are
//!   non-atomic and sound because values are thread-confined; E3's fork-join transfers ownership of
//!   whole heaps, never shares them.
//!
//! The `diag` feature (default off) adds the debugging instrument for the
//! whole memory story: rc underflow/overflow tripwires, the underflow
//! tombstone included (a dec through a pooled block reads the zero the pool
//! wrote).
//!
//! VM seam: ds-vm's object store consumes the block half as its object
//! substrate - the store keeps the stable id table (the `Value::Ref`
//! word's id part indexes it), and every live id names a counted block here. The store drives
//! count lifecycles through the raw surface ([`Heap::alloc_store`],
//! [`Heap::rc_inc_raw`], [`Heap::rc_dec_count`] plus [`Heap::rc_dispose`],
//! [`Heap::payload`]/[`Heap::payload_mut`]) so its own death order runs
//! between the count reaching zero and the payload dying; the header layout
//! is the committed `repr(C)` format carrying the dormant two-hook collector
//! word. The arena half is the temporaries substrate: each fiber owns one
//! arena serving its designated concatenation results (`alloc_concat`, with
//! `bytes_at` behind the packed-handle reads), frames stage their entry
//! marks through `push_frame_mark`/`pop_frame_mark`, and the death doors
//! release whole through `release_all`.
//!
//! Confinement: this crate is the unsafe core of the runtime (`ds-rt` in the
//! implementation plan); the rest of the workspace consumes it through safe
//! APIs. Every unsafe block carries a safety comment.
//!
//! Miri gates the unsafe core (CI runs `cargo miri test -p ds-rt` under both
//! Stacked and Tree Borrows). Arena grants are raw pointers because
//! restore-reuse invalidates references: a restore kills every grant it freed
//! and only the latest grant for a byte range is live, an aliasing contract
//! the arena's misuse contract documents and every caller upholds.

mod arena;
mod guard;
mod heap;
mod rc;

pub use arena::{Arena, ArenaMisuse, ArenaStats, FIRST_CHUNK, MAX_CHUNK, MIN_ALIGN, Mark};
pub use guard::RestoreOnDrop;
pub use heap::{Heap, HeapStats, MAX_REUSE_CLASS, MIN_CLASS, POOL_DEPTH};
pub use rc::{DeinitFn, RcHeader, drop_deinit};
