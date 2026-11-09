//! DreamScript runtime core: the per-frame bump-arena half of M4.
//!
//! Contract mapping from the design record ([C4], [H4] in
//! `docs/en/design-decisions.md`):
//!
//! - "mark/save at entry, bump alloc, restore on every pop path" - [`Arena::mark`] /
//!   [`Arena::restore`]; [`Arena::frame`] / [`RestoreOnDrop`] / [`Arena::scope`] put the restore on
//!   every exit path, panic unwinding included.
//! - "segment growth by chunk append so captured frame addresses stay stable" - chunks are raw,
//!   individually allocated blocks; growth appends a new chunk and never reallocates an old one, so
//!   an address handed out stays valid for the arena's life (until a restore past it). Deep
//!   non-tail recursion grows the same way [H2's fiber note].
//! - "arena marks are per-fiber, so fiber switching needs no arena action" - a fiber owns its
//!   [`Arena`]. There is no shared cursor and no FiberId-keyed mark table by design: with one
//!   shared bump cursor, interleaved mark/restore from two fibers would free the other fiber's live
//!   data unless every restore cross-checked every fiber's marks, which is exactly the "arena
//!   action on switch" H4 rules out. Per-fiber arenas make the invariant local (a fiber can only
//!   free its own temporaries) and the switch itself free; `Arena` is `Send` so a fiber's arena
//!   moves with it under ownership transfer.
//! - "allocation is a pointer add ... zero pauses, zero barriers, no safepoints, no GC maps" - the
//!   fast path is one checked pointer addition; reclamation moves a cursor. Nothing here pauses,
//!   scans, or barriers.
//!
//! Chunk policy: the first chunk is 16 KiB ([`FIRST_CHUNK`]); each growth
//! doubles the previous chunk's capacity up to 1 MiB ([`MAX_CHUNK`]).
//! Requests larger than the cap, or aligned above 16, get a dedicated
//! exactly-sized chunk. Restore returns post-mark chunks to a per-arena free
//! list when they have a canonical ladder size (so repeated deep recursion
//! reuses the same chunk set and stays memory-bounded) and returns dedicated
//! chunks to the allocator (their one-off sizes would make pooling noise).
//!
//! Alignment: every allocation is aligned to [`MIN_ALIGN`] = 16 bytes and
//! consumes a multiple of 16; finer requested alignments below 16 are free,
//! coarser ones take the dedicated-chunk path.
//!
//! Confinement: this crate is the unsafe core of the runtime (`ds-rt` in the
//! implementation plan); the rest of the workspace consumes it through safe
//! APIs. Every unsafe block carries a safety comment.
//!
//! What lands later in this crate (the other half of M4): precise non-atomic
//! reference counting with reuse threading (`rc_dec` -> reuse candidate ->
//! `alloc_at`), `deinit` execution at the unique death, and the diagnostic
//! layer (rc==1 checks, borrow counter). Those layer on top of the primitives
//! here; they do not change the arena contract.
//!
//! Miri is not available on this Windows setup; the unsafe code is nonetheless
//! written to miri rules (in-bounds pointer arithmetic, matched alloc/dealloc
//! layouts, provenance-free dangling pointers for zero-sized requests).
//!
//! [C4]: https://github.com/lxl66566/DreamScript/blob/main/docs/en/design-decisions.md
//! [H4]: https://github.com/lxl66566/DreamScript/blob/main/docs/en/design-decisions.md
//! [H2's fiber note]: https://github.com/lxl66566/DreamScript/blob/main/docs/en/design-decisions.md

mod arena;
mod guard;

pub use arena::{Arena, ArenaStats, FIRST_CHUNK, MAX_CHUNK, MIN_ALIGN, Mark};
pub use guard::RestoreOnDrop;
