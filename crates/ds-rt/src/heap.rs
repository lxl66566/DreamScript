//! The per-fiber heap (C4/H4): the bump arena for non-escaping temporaries
//! plus the counted-block store for boxed/shared values.
//!
//! Boxed blocks are header + payload in one system-allocator allocation; they
//! are never carved from the arena, because an arena restore reclaims at frame
//! granularity and would kill shared data. The integration with the arena half
//! is ownership: one `Heap` per fiber owns both, so per-fiber marks and
//! chunk-append address stability hold exactly as before, and the heap moves
//! whole with the fiber under E3 ownership transfer.
//!
//! Reuse threading (C4): a unique death pools the block as a reuse candidate
//! (freelist per power-of-two size class, LIFO, depth-capped), and the next
//! compatible allocation is placed at the candidate's address - the
//! `kk_block_alloc_at` pattern, as [`Heap::alloc_at`]. Blocks that are
//! over-aligned, oversized, or beyond the pool cap go back to the system
//! allocator (mimalloc stays a later swap: no new dependencies now).
//!
//! Reentrancy: `rc_dec`'s death path runs `deinit` (arbitrary code, free to
//! allocate or drop other blocks) before the block is pooled, so reentrant
//! traffic can never observe a half-dead block. No `&mut` borrow of the heap
//! interior is live across user code - the same discipline the arena half
//! follows.
//!
//! Two consumer surfaces over the same blocks: lexical scopes take
//! [`Heap::alloc_box`] (the returned [`RcRef`] owns the count), while stores
//! that keep their own id-to-block mapping (the VM's object table) take
//! [`Heap::alloc_store`] and drive the count lifecycle through the split dec
//! ([`Heap::rc_dec_count`] plus [`Heap::rc_dispose`]) so their own death order
//! can run between the count reaching zero and the payload dying.

use core::{
    alloc::Layout,
    cell::UnsafeCell,
    mem::{align_of, size_of},
    ptr::{self, NonNull},
};
use std::alloc::{alloc, dealloc};

use crate::{
    arena::{Arena, ArenaStats, MIN_ALIGN},
    rc::{DeinitFn, RcHeader, RcRef},
};

/// Smallest reuse size class; blocks are always MIN_ALIGN-aligned and sized to
/// a multiple of 16 before classing.
pub const MIN_CLASS: usize = MIN_ALIGN;

/// Largest size class kept in the reuse pool. Bigger blocks return to the
/// system allocator at death (pooling megabyte blocks would pin memory no
/// allocation pattern is likely to want back).
pub const MAX_REUSE_CLASS: usize = 64 * 1024;

/// How many candidates each size class keeps before further deaths fall back
/// to the system allocator; bounds pooled memory under drop-heavy loops.
pub const POOL_DEPTH: u8 = 4;

/// Number of size classes: powers of two from [`MIN_CLASS`] to
/// [`MAX_REUSE_CLASS`] inclusive.
const CLASS_COUNT: usize =
    (MAX_REUSE_CLASS.trailing_zeros() - MIN_CLASS.trailing_zeros()) as usize + 1;

/// Smallest power-of-two class at least `size`; the ladder's smallest rung
/// [`MIN_CLASS`] covers every size below it, so the ladder never steps.
fn ladder_class(size: usize) -> usize {
    size.max(MIN_CLASS).next_power_of_two()
}

/// The freelist row a block of `size` reuses from, when it is a ladder rung the
/// pool keeps: a power-of-two size in `[MIN_CLASS, MAX_REUSE_CLASS]`.
fn pool_row(size: usize) -> Option<usize> {
    (size.is_power_of_two() && (MIN_CLASS..=MAX_REUSE_CLASS).contains(&size))
        .then(|| (size.trailing_zeros() - MIN_CLASS.trailing_zeros()) as usize)
}

/// The freelist row `layout` is allocated at, if it is reuse-eligible: block
/// alignment at most MIN_ALIGN (so payload offsets stay valid at any class
/// base) and a ladder size within the pool range. The row is the class index
/// itself, so the caller indexes the freelist once instead of re-deriving it
/// from the class size at each step.
fn reusable_row(layout: Layout) -> Option<usize> {
    if layout.align() > MIN_ALIGN {
        return None;
    }
    pool_row(ladder_class(layout.size()))
}

/// The canonical block size of a freelist row: its rung on the ladder.
fn class_size(row: usize) -> usize {
    MIN_CLASS << row
}

/// The payload offset within a block carrying a `T`: the `repr(C)` header
/// rounded up to its own alignment, then to `T`'s - the same arithmetic
/// `Layout::extend` performs over the padded header, as a const fn so the hot
/// payload accessors fold it to a constant per `T`; adding a header field
/// moves the offset with the type instead of leaving a magic constant.
const fn payload_offset<T>() -> usize {
    let align_h = align_of::<RcHeader>();
    let size = (size_of::<RcHeader>() + align_h - 1) & !(align_h - 1);
    let align_t = align_of::<T>();
    (size + align_t - 1) & !(align_t - 1)
}

/// Block layout for a payload type: `repr(C)` header, payload at
/// [`payload_offset`]. The offset is a pure function of `T`, so every site
/// holding the type computes the same payload address.
fn block_layout<T>() -> (Layout, usize) {
    let offset = payload_offset::<T>();
    let size = offset + size_of::<T>();
    let align = align_of::<RcHeader>().max(align_of::<T>());
    (
        Layout::from_size_align(size, align).expect("ds-rt: block layout overflow"),
        offset,
    )
}

/// Snapshot of heap memory state, for the M4 residency accounting and tests.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct HeapStats {
    /// The bump-arena side (temporaries).
    pub arena: ArenaStats,
    /// Blocks currently pooled as reuse candidates.
    pub pooled_blocks: usize,
}

/// The per-fiber runtime heap: bump arena + counted blocks (C4/H4).
///
/// `Send` (moves whole with its fiber under fork-join ownership transfer) but
/// not `Sync`: the non-atomic counts and the freelist are confined state.
///
/// # Misuse contract
/// Raw `rc_dec` on an already-dead block, or an `rc_inc`/`rc_dec` imbalance,
/// is a caller bug - the `diag` feature catches most instances (underflow
/// tombstones, overflow tripwire); linearity catches it at the language level.
pub struct Heap {
    inner: UnsafeCell<Inner>,
}

/// One size class's freelist row. The head and the depth share the row, so a
/// pool decision and the link it then writes touch one cache line.
struct ClassPool {
    /// The freelist head; nodes are the pooled blocks themselves, linked
    /// through the header's `deinit` slot.
    head: Option<NonNull<u8>>,
    /// Current freelist depth, capped at [`POOL_DEPTH`].
    depth: u8,
}

impl ClassPool {
    /// The empty row, for the heap's const constructor.
    const EMPTY: Self = Self {
        head: None,
        depth: 0,
    };
}

struct Inner {
    arena: Arena,
    /// One freelist row per size class.
    pools: [ClassPool; CLASS_COUNT],
}

// SAFETY: the heap uniquely owns its arena, pool, and blocks; moving it to
// another thread moves that ownership (fibers are thread-confined (C4),
// transferred whole under E3). `Sync` is deliberately not implemented: the
// counts and freelists mutate through `&self`.
unsafe impl Send for Heap {}

impl Heap {
    /// A heap that owns nothing until the first allocation.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            inner: UnsafeCell::new(Inner {
                arena: Arena::new(),
                pools: [const { ClassPool::EMPTY }; CLASS_COUNT],
            }),
        }
    }

    /// The temporaries side of the heap.
    #[must_use]
    pub fn arena(&self) -> &Arena {
        &self.inner().arena
    }

    /// Per-entry mark over the arena half; restores on every pop path.
    #[must_use]
    pub fn frame(&self) -> crate::RestoreOnDrop<'_> {
        self.arena().frame()
    }

    /// Allocates `value` as a freshly unique counted block with `deinit` as
    /// its static death hook (C4/C5).
    ///
    /// # Panics
    /// If the global allocator returns null or the block exceeds u32 bytes.
    pub fn alloc_box<T>(&self, value: T, deinit: Option<DeinitFn>) -> RcRef<'_, T> {
        let header = self.alloc_store(value, deinit);
        let offset = payload_offset::<T>();
        // SAFETY: the block carries at least the layout `block_layout`
        // computed; `offset` is that same layout math, in-bounds and aligned.
        let payload = unsafe { header.byte_add(offset) }.cast::<T>();
        RcRef::new(self, header, payload)
    }

    /// Allocates `value` as a freshly unique counted block whose count
    /// lifecycle the caller (a store, not a lexical scope) drives: the raw
    /// form of [`Heap::alloc_box`]. The birth count is 1; a unique death the
    /// store observes through [`Heap::rc_dec_count`] owes exactly one
    /// [`Heap::rc_dispose`], which runs `deinit` and pools or releases the
    /// block.
    ///
    /// # Panics
    /// If the global allocator returns null or the block exceeds u32 bytes.
    #[must_use = "the header is the block handle; losing it leaks the block"]
    pub fn alloc_store<T>(&self, value: T, deinit: Option<DeinitFn>) -> NonNull<RcHeader> {
        let (layout, offset) = block_layout::<T>();
        let header = self.alloc_block(layout, deinit);
        // SAFETY: `alloc_block` returned a block of at least `layout.size()`
        // bytes at `layout.align()`; `offset` (from the same layout math) is
        // in-bounds and aligned for `T`.
        let payload = unsafe { header.byte_add(offset) }.cast::<T>();
        // SAFETY: `alloc_block` returned memory valid and aligned for the
        // payload layout computed above; the write initializes it.
        unsafe { payload.as_ptr().write(value) };
        header
    }

    /// The payload of the live block `header` names, borrowed from the heap:
    /// block addresses are stable for the block's whole life.
    ///
    /// # Misuse contract
    /// `header` must name a live block of this heap carrying a `T` payload;
    /// the reference must not outlive the block's death, which only the
    /// owning store or a dropping [`RcRef`] can drive.
    #[must_use]
    pub fn payload<T>(&self, header: NonNull<RcHeader>) -> &T {
        let offset = payload_offset::<T>();
        // SAFETY: the block is live with an initialized `T` payload at
        // `offset`; the reference borrows from `&self`, so only the owner's
        // exclusive window (its `&mut`) can end the block's life.
        unsafe { &*header.byte_add(offset).cast::<T>().as_ptr() }
    }

    /// The payload for store-driven in-place mutation. Takes `&mut self` so a
    /// mutation window never overlaps a live [`Heap::payload`] borrow handed
    /// out over the same heap.
    ///
    /// # Misuse contract
    /// Same liveness discipline as [`Heap::payload`].
    #[must_use]
    pub fn payload_mut<T>(&mut self, header: NonNull<RcHeader>) -> &mut T {
        let offset = payload_offset::<T>();
        // SAFETY: live block, initialized `T` payload at `offset`; `&mut self`
        // makes the window exclusive over every accessor of this heap.
        unsafe { &mut *header.byte_add(offset).cast::<T>().as_ptr() }
    }

    /// The live reference count of the block `header` names.
    ///
    /// # Misuse contract
    /// `header` must name a live block of this heap.
    #[must_use]
    pub fn rc_count(&self, header: NonNull<RcHeader>) -> u32 {
        // SAFETY: the header names a live block on this (owning) thread.
        unsafe { (*header.as_ptr()).rc() }
    }

    /// Raw `rc_inc` (C4): one more owner for the block `header` names. The
    /// compiler-inserted dup pairs with [`Heap::rc_dec_raw`]; both take the
    /// heap for surface symmetry with the dec side.
    #[allow(clippy::unused_self)]
    pub fn rc_inc_raw(&self, header: NonNull<RcHeader>) {
        RcHeader::inc_at(header);
    }

    /// Raw `rc_dec` (C4): drops one owner. A unique death runs the block's
    /// `deinit` exactly once (C5), then threads the block to the reuse pool or
    /// returns it to the system allocator. Returns whether the death was
    /// unique.
    ///
    /// `payload` is the payload address (the argument `deinit` receives); the
    /// caller computes it from the static type, exactly like `alloc_box`.
    ///
    /// # Panics (diag only)
    /// On count underflow (double drop, or a dec through a pooled block - the
    /// pool tombstones the count to zero) or at death with a borrow outstanding.
    ///
    /// # Misuse contract
    /// `header` must name a live block owned by this heap; a dec on a dead
    /// block is undefined behavior the diag layer flags where detectable.
    #[must_use = "the death flag drives drop-glue-free callers; ignoring it misses unique death"]
    pub fn rc_dec_raw(&self, header: NonNull<RcHeader>, payload: NonNull<u8>) -> bool {
        self.dec(header, payload)
    }

    /// The count move of `rc_dec` without the death work: one owner drops, and
    /// the returned flag names the unique death. The store-facing half of the
    /// split dec - an owner that drives its own death order (extracting owned
    /// words before the payload dies) gets `true` exactly once and owes one
    /// matching [`Heap::rc_dispose`].
    ///
    /// # Panics (diag only)
    /// On count underflow (double drop) or at death with a borrow outstanding.
    ///
    /// # Misuse contract
    /// `header` must name a live block owned by this heap.
    #[must_use = "the flag promises exactly one rc_dispose; ignoring it leaks the block"]
    pub fn rc_dec_count(&self, header: NonNull<RcHeader>) -> bool {
        // SAFETY: the header names a live block this heap owns; confined to
        // this thread, and the borrow ends before any user code runs.
        let h = unsafe { &mut *header.as_ptr() };
        #[cfg(feature = "diag")]
        {
            assert!(h.rc > 0, "ds-rt diag: rc underflow (double drop)");
            h.assert_no_borrows();
        }
        h.rc -= 1;
        h.rc == 0
    }

    /// The death work of `rc_dec` for a block whose owner observed the unique
    /// death ([`Heap::rc_dec_count`] returned `true`): the block's `deinit`
    /// hook runs exactly once (C5), then the block threads to the reuse pool
    /// or returns to the system allocator.
    ///
    /// # Misuse contract
    /// One `rc_dispose` per observed unique death; a second one is a double
    /// free.
    pub fn rc_dispose<T>(&self, header: NonNull<RcHeader>) {
        let offset = payload_offset::<T>();
        // SAFETY: same layout math as the allocation; in-bounds and aligned.
        let payload = unsafe { header.byte_add(offset) }.cast::<u8>();
        self.dispose(header, payload);
    }

    /// The dec path the drop glue also takes; no `must_use` so drops can
    /// ignore the death flag.
    pub(crate) fn dec(&self, header: NonNull<RcHeader>, payload: NonNull<u8>) -> bool {
        if !self.rc_dec_count(header) {
            return false;
        }
        self.dispose(header, payload);
        true
    }

    /// The shared death tail: the block's `deinit` runs once while the block
    /// is dead but not yet pooled, so reentrant allocation can never hand it
    /// out; a panic in deinit leaks the block (sound: it is neither pooled nor
    /// freed). Then the block threads to the pool or releases.
    fn dispose(&self, header: NonNull<RcHeader>, payload: NonNull<u8>) {
        // SAFETY: the header names a dead, allocated block this heap owns.
        let deinit = unsafe { (*header.as_ptr()).deinit };
        if let Some(deinit) = deinit {
            deinit(payload);
        }
        self.thread_block(header);
    }

    /// Snapshot of both sides of the heap.
    #[must_use]
    pub fn stats(&self) -> HeapStats {
        let inner = self.inner();
        HeapStats {
            arena: inner.arena.stats(),
            pooled_blocks: inner.pools.iter().map(|pool| pool.depth).sum::<u8>() as usize,
        }
    }

    /// Allocates a block for `layout` (header + payload): a pooled candidate
    /// of the matching class when one exists ([`Heap::alloc_at`]), else the
    /// system allocator with a class-canonical size, else a dedicated exact
    /// allocation for over-aligned or oversized requests.
    fn alloc_block(&self, layout: Layout, deinit: Option<DeinitFn>) -> NonNull<RcHeader> {
        if let Some(row) = reusable_row(layout) {
            let inner = self.inner();
            if inner.pools[row].head.is_some() {
                return inner.alloc_at(row, deinit);
            }
            // Fallback: fresh system-allocator block at the canonical class
            // size, so the block stays class-shaped for its next reuse.
            let class = class_size(row);
            let layout = Layout::from_size_align(class, MIN_ALIGN)
                .expect("ds-rt: class layout is always valid");
            let block = system_alloc(layout).cast::<RcHeader>();
            write_header(block, class, MIN_ALIGN, deinit);
            return block;
        }
        // Dedicated: exact layout (over-aligned or oversized); never pooled,
        // released with the exact layout recorded in its header.
        let block = system_alloc(layout).cast::<RcHeader>();
        write_header(block, layout.size(), layout.align(), deinit);
        block
    }

    /// Pools the dead block `header` as a reuse candidate of its size class,
    /// or returns it to the system allocator when it is not class-shaped or
    /// the class pool is full (C4).
    fn thread_block(&self, header: NonNull<RcHeader>) {
        let inner = self.inner();
        // SAFETY: the block is allocated and dead; size/align sit at their
        // committed header offsets, untouched by pool bookkeeping.
        let size = unsafe { ptr::read(ptr::addr_of!((*header.as_ptr()).size)) } as usize;
        let align = unsafe { ptr::read(ptr::addr_of!((*header.as_ptr()).align)) } as usize;
        // One row probe answers both the shape gate and the depth cap.
        let row =
            pool_row(size).filter(|row| align == MIN_ALIGN && inner.pools[*row].depth < POOL_DEPTH);
        let Some(row) = row else {
            // SAFETY: the header records the exact allocation layout
            // (class-canonical or dedicated exact); released exactly once at
            // the unique death.
            unsafe {
                dealloc(
                    header.as_ptr().cast(),
                    Layout::from_size_align_unchecked(size, align),
                );
            }
            return;
        };
        let pool = &mut inner.pools[row];
        // SAFETY: the block is dead and exclusively ours. The deinit slot is
        // pointer-sized and the block stays allocated while pooled, so the
        // intrusive next pointer is always readable; the rc tombstone (zero)
        // makes a stray dec through a pooled block trip the diag underflow
        // check instead of silently corrupting the count.
        unsafe {
            let slot = ptr::addr_of_mut!((*header.as_ptr()).deinit).cast::<Option<NonNull<u8>>>();
            ptr::write(slot, pool.head);
            ptr::addr_of_mut!((*header.as_ptr()).rc).write(0);
        }
        pool.head = Some(header.cast());
        pool.depth += 1;
    }

    #[inline]
    // Sound against `&self`: same UnsafeCell escape hatch and confinement
    // argument as the arena half; no `&mut Inner` is live across user code
    // (see the reentrancy note in the module docs).
    #[allow(clippy::mut_from_ref)]
    fn inner(&self) -> &mut Inner {
        // SAFETY: `Heap` is !Sync, so `&self` is confined to the owning
        // thread; every borrow is transient (no user code runs on it), so no
        // two `&mut Inner` coexist.
        unsafe { &mut *self.inner.get() }
    }
}

impl Default for Heap {
    fn default() -> Self {
        Self::new()
    }
}

impl Inner {
    /// The `alloc_at` pattern (C4): places a fresh block at the pooled
    /// candidate heading `row`'s freelist, unlinking the node first.
    fn alloc_at(&mut self, row: usize, deinit: Option<DeinitFn>) -> NonNull<RcHeader> {
        let pool = &mut self.pools[row];
        let head = pool
            .head
            .take()
            .expect("ds-rt: caller checked the row is non-empty");
        // SAFETY: the pooled node links through its deinit slot, which
        // `thread_block` wrote when pooling; the block stays allocated while
        // pooled, and the read happens before the slot is overwritten.
        let next = unsafe {
            ptr::read(
                ptr::addr_of!((*head.cast::<RcHeader>().as_ptr()).deinit)
                    .cast::<Option<NonNull<u8>>>(),
            )
        };
        pool.head = next;
        pool.depth -= 1;
        let block = head.cast::<RcHeader>();
        write_header(block, class_size(row), MIN_ALIGN, deinit);
        block
    }
}

impl Drop for Heap {
    fn drop(&mut self) {
        let inner = self.inner();
        for (row, pool) in inner.pools.iter_mut().enumerate() {
            // The freelist is intrusive: each pooled node links through its
            // deinit slot, written by `thread_block`, and a row can hold up
            // to POOL_DEPTH nodes — dropping only the head strands the rest.
            let mut node = pool.head.take();
            while let Some(block) = node {
                // SAFETY: pooled nodes were allocated exactly at (row's
                // class size, MIN_ALIGN) and are released once here; the
                // next link is read before the release, mirroring
                // `Inner::alloc_at`.
                unsafe {
                    let next = ptr::read(
                        ptr::addr_of!((*block.cast::<RcHeader>().as_ptr()).deinit)
                            .cast::<Option<NonNull<u8>>>(),
                    );
                    dealloc(
                        block.as_ptr(),
                        Layout::from_size_align_unchecked(class_size(row), MIN_ALIGN),
                    );
                    node = next;
                }
            }
        }
    }
}

/// Fresh system-allocator block; the mimalloc swap point (C4).
///
/// # Panics
/// If the layout is invalid or the allocator returns null.
fn system_alloc(layout: Layout) -> NonNull<u8> {
    // SAFETY: `layout` is valid (power-of-two alignment, sizes from Layout
    // arithmetic); null is turned into a panic.
    let ptr = unsafe { alloc(layout) };
    NonNull::new(ptr).expect("ds-rt: global allocator returned null")
}

/// Initializes the header of a freshly claimed block.
///
/// # Panics
/// If the block exceeds u32 bytes or alignment.
fn write_header(block: NonNull<RcHeader>, size: usize, align: usize, deinit: Option<DeinitFn>) {
    let header = RcHeader::fresh(size, align, deinit);
    // SAFETY: the block was just claimed with at least `size` bytes at an
    // alignment covering the header (layout arithmetic or the canonical class
    // alignment); the write initializes dead or stale memory (pooled
    // candidates carry a clobbered header by contract).
    unsafe { block.as_ptr().write(header) };
}

#[cfg(test)]
mod tests {
    use core::alloc::Layout;

    use super::{
        CLASS_COUNT, MAX_REUSE_CLASS, MIN_CLASS, RcHeader, block_layout, class_size, ladder_class,
        payload_offset, pool_row,
    };

    /// The class ladder is the smallest power of two at least the size, with
    /// [`MIN_CLASS`] as its floor: the closed form and the doubling ladder
    /// agree on every rung and between two rungs.
    #[test]
    fn the_class_ladder_rounds_up_to_the_next_rung() {
        let reference = |size: usize| {
            let mut class = MIN_CLASS;
            while class < size {
                class *= 2;
            }
            class
        };
        for size in 0..=MAX_REUSE_CLASS + MIN_CLASS {
            assert_eq!(ladder_class(size), reference(size), "size {size}");
        }
    }

    /// Every ladder rung is one freelist row, and that row's canonical size is
    /// the rung itself: the pool index and the block size it allocates at
    /// derive from one table, so they cannot drift apart.
    #[test]
    fn pool_rows_are_the_ladder_rungs() {
        for row in 0..CLASS_COUNT {
            let class = MIN_CLASS << row;
            assert_eq!(pool_row(class), Some(row), "class {class}");
            assert_eq!(class_size(row), class, "row {row}");
            assert_eq!(ladder_class(class), class, "class {class} is its own rung");
        }
        // off the ladder: below its floor, between two rungs, above the pool
        assert_eq!(pool_row(MIN_CLASS - 1), None);
        assert_eq!(pool_row(MIN_CLASS + 1), None);
        assert_eq!(pool_row(MAX_REUSE_CLASS * 2), None);
    }

    /// The const payload offset is the same arithmetic `Layout::extend` runs
    /// over the padded `repr(C)` header, for every alignment shape a payload
    /// can take: the header itself, sub-header, zero-sized, over-aligned,
    /// and large.
    #[test]
    fn const_payload_offset_matches_layout_extend() {
        fn extend_offset<T>() -> usize {
            let header = Layout::new::<RcHeader>().pad_to_align();
            Layout::extend(&header, Layout::new::<T>())
                .expect("ds-rt: block layout overflow")
                .1
        }
        for (offset, extend) in [
            (payload_offset::<RcHeader>(), extend_offset::<RcHeader>()),
            (payload_offset::<u8>(), extend_offset::<u8>()),
            (payload_offset::<[u64; 3]>(), extend_offset::<[u64; 3]>()),
            (payload_offset::<()>(), extend_offset::<()>()),
            (payload_offset::<[u128; 2]>(), extend_offset::<[u128; 2]>()),
            (payload_offset::<[u128; 0]>(), extend_offset::<[u128; 0]>()),
            (payload_offset::<[u64; 32]>(), extend_offset::<[u64; 32]>()),
        ] {
            assert_eq!(offset, extend);
        }
        // the whole layout derives from the same arithmetic
        let (layout, offset) = block_layout::<[u64; 3]>();
        let (extend, extend_off) = {
            let header = Layout::new::<RcHeader>().pad_to_align();
            Layout::extend(&header, Layout::new::<[u64; 3]>())
                .expect("ds-rt: block layout overflow")
        };
        assert_eq!(
            (layout.size(), layout.align(), offset),
            (extend.size(), extend.align(), extend_off)
        );
    }
}
