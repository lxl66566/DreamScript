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

use core::{
    alloc::Layout,
    cell::UnsafeCell,
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

/// Smallest power-of-two class at least `size`.
fn ladder_class(size: usize) -> usize {
    let mut class = MIN_CLASS;
    while class < size {
        class *= 2;
    }
    class
}

/// Freelist bucket index of a power-of-two class in
/// `[MIN_CLASS, MAX_REUSE_CLASS]`.
fn class_index(class: usize) -> usize {
    (class.trailing_zeros() - MIN_CLASS.trailing_zeros()) as usize
}

/// The class `layout` should be allocated at, if it is reuse-eligible: block
/// alignment at most MIN_ALIGN (so payload offsets stay valid at any class
/// base) and a ladder size within the pool range.
fn reusable_class(layout: Layout) -> Option<usize> {
    if layout.align() > MIN_ALIGN {
        return None;
    }
    let class = ladder_class(layout.size());
    (class <= MAX_REUSE_CLASS).then_some(class)
}

/// Block layout for a payload type: `repr(C)` header, payload at a
/// `Layout::extend`-aligned offset. The offset is a pure function of `T`, so
/// every site holding the type computes the same payload address.
fn block_layout<T>() -> (Layout, usize) {
    let header = Layout::new::<RcHeader>().pad_to_align();
    let (layout, offset) =
        Layout::extend(&header, Layout::new::<T>()).expect("ds-rt: block layout overflow");
    (layout, offset)
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

struct Inner {
    arena: Arena,
    /// Freelist heads per size class; nodes are the pooled blocks themselves,
    /// linked through the header's `deinit` slot.
    free: [Option<NonNull<u8>>; CLASS_COUNT],
    /// Current freelist depth per class, capped at [`POOL_DEPTH`].
    depth: [u8; CLASS_COUNT],
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
                free: [const { None }; CLASS_COUNT],
                depth: [0; CLASS_COUNT],
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
        let (layout, offset) = block_layout::<T>();
        let header = self.alloc_block(layout, deinit);
        // SAFETY: `alloc_block` returned a block of at least `layout.size()`
        // bytes at `layout.align()`; `offset` (from the same layout math) is
        // in-bounds and aligned for `T`.
        let payload = unsafe { header.byte_add(offset) }.cast::<T>();
        // SAFETY: `alloc_block` returned memory valid and aligned for the
        // payload layout computed above; the write initializes it.
        unsafe { payload.as_ptr().write(value) };
        RcRef::new(self, header, payload)
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

    /// The dec path the drop glue also takes; no `must_use` so drops can
    /// ignore the death flag.
    pub(crate) fn dec(&self, header: NonNull<RcHeader>, payload: NonNull<u8>) -> bool {
        // SAFETY: the header names a live block this heap owns; confined to
        // this thread, and the borrow ends before any user code runs.
        let h = unsafe { &mut *header.as_ptr() };
        #[cfg(feature = "diag")]
        {
            assert!(h.rc > 0, "ds-rt diag: rc underflow (double drop)");
            h.assert_no_borrows();
        }
        h.rc -= 1;
        if h.rc != 0 {
            return false;
        }
        // Unique death. Deinit runs while the block is dead but not yet
        // pooled, so reentrant allocation can never hand it out; a panic in
        // deinit leaks the block (sound: it is neither pooled nor freed).
        let deinit = h.deinit;
        if let Some(deinit) = deinit {
            deinit(payload);
        }
        self.thread_block(header);
        true
    }

    /// Snapshot of both sides of the heap.
    #[must_use]
    pub fn stats(&self) -> HeapStats {
        let inner = self.inner();
        HeapStats {
            arena: inner.arena.stats(),
            pooled_blocks: inner.depth.iter().sum::<u8>() as usize,
        }
    }

    /// Allocates a block for `layout` (header + payload): a pooled candidate
    /// of the matching class when one exists ([`Heap::alloc_at`]), else the
    /// system allocator with a class-canonical size, else a dedicated exact
    /// allocation for over-aligned or oversized requests.
    fn alloc_block(&self, layout: Layout, deinit: Option<DeinitFn>) -> NonNull<RcHeader> {
        if let Some(class) = reusable_class(layout) {
            let inner = self.inner();
            if inner.free[class_index(class)].is_some() {
                return inner.alloc_at(class, deinit);
            }
            // Fallback: fresh system-allocator block at the canonical class
            // size, so the block stays class-shaped for its next reuse.
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
        let poolable = align == MIN_ALIGN
            && size.is_power_of_two()
            && (MIN_CLASS..=MAX_REUSE_CLASS).contains(&size)
            && inner.depth[class_index(size)] < POOL_DEPTH;
        if !poolable {
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
        }
        let idx = class_index(size);
        // SAFETY: the block is dead and exclusively ours. The deinit slot is
        // pointer-sized and the block stays allocated while pooled, so the
        // intrusive next pointer is always readable; the rc tombstone (zero)
        // makes a stray dec through a pooled block trip the diag underflow
        // check instead of silently corrupting the count.
        unsafe {
            let slot = ptr::addr_of_mut!((*header.as_ptr()).deinit).cast::<Option<NonNull<u8>>>();
            ptr::write(slot, inner.free[idx]);
            ptr::addr_of_mut!((*header.as_ptr()).rc).write(0);
        }
        inner.free[idx] = Some(header.cast());
        inner.depth[idx] += 1;
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
    /// candidate heading `class`'s freelist, unlinking the node first.
    fn alloc_at(&mut self, class: usize, deinit: Option<DeinitFn>) -> NonNull<RcHeader> {
        let head = self.free[class_index(class)]
            .take()
            .expect("ds-rt: caller checked the bucket is non-empty");
        // SAFETY: the pooled node links through its deinit slot, which
        // `thread_block` wrote when pooling; the block stays allocated while
        // pooled, and the read happens before the slot is overwritten.
        let next = unsafe {
            ptr::read(
                ptr::addr_of!((*head.cast::<RcHeader>().as_ptr()).deinit)
                    .cast::<Option<NonNull<u8>>>(),
            )
        };
        self.free[class_index(class)] = next;
        self.depth[class_index(class)] -= 1;
        let block = head.cast::<RcHeader>();
        write_header(block, class, MIN_ALIGN, deinit);
        block
    }
}

impl Drop for Heap {
    fn drop(&mut self) {
        let inner = self.inner();
        for (idx, head) in inner.free.iter_mut().enumerate() {
            if let Some(block) = head.take() {
                // SAFETY: pooled blocks were allocated exactly at
                // (class size of idx, MIN_ALIGN) and are released once here.
                unsafe {
                    dealloc(
                        block.as_ptr(),
                        Layout::from_size_align_unchecked(MIN_CLASS << idx, MIN_ALIGN),
                    );
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
