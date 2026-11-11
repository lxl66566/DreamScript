//! Per-frame bump arena over an append-only chunk list (C4/H4).

use core::{
    alloc::Layout,
    cell::UnsafeCell,
    ptr::{self, NonNull},
};
use std::alloc::{alloc, dealloc};

/// Minimum alignment of every arena allocation, and the size granularity: each
/// allocation consumes a multiple of this many bytes so the bump cursor stays
/// aligned. 16 covers i64/f64/reference/tag+payload slots and common SIMD loads.
pub const MIN_ALIGN: usize = 16;

/// Capacity of the first chunk. 16 KiB keeps a typical frame's temporaries plus
/// a few hundred caller frames chunk-local before the first growth.
pub const FIRST_CHUNK: usize = 16 * 1024;

/// Chunk capacities grow geometrically (`2x` the previous chunk) up to this cap.
/// Larger single allocations get a dedicated exactly-sized chunk instead.
pub const MAX_CHUNK: usize = 1024 * 1024;

/// A position in the arena: index of the chunk plus bump offset within it.
///
/// Marks are opaque, cheap to copy, and must be restored in LIFO order (the
/// discipline the VM's frame stack already has). Restoring the same mark twice
/// is fine (the second restore is a no-op); restoring a mark older than one
/// already restored panics.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Mark {
    pub(crate) chunk: usize,
    pub(crate) offset: usize,
}

/// A bump arena for non-escaping temporaries (C4/H4).
///
/// Allocation is pointer addition in the current chunk. Growth appends a new
/// chunk; existing chunks are never reallocated, so every address handed out
/// stays valid until a [`Arena::restore`] past it (chunk-append stability).
/// Reclamation is [`Arena::restore`], which frees everything allocated after a
/// mark by resetting the bump cursor; a [`crate::RestoreOnDrop`] guard makes
/// that happen on every pop path, panic unwinding included.
///
/// A fiber owns its arena; fiber switching performs no arena action (see the
/// crate docs). `Arena` is `Send` (ownership moves with the fiber) but not
/// `Sync` (allocation mutates through `&self`).
///
/// # Misuse contract
/// Allocations reachable after the restore point they postdate are dead by
/// contract; the arena cannot prove that (linearity does, at the language
/// level). Using a reference after the restore that freed it, or across an
/// intervening allocation, is the caller's bug - the same contract every bump
/// allocator (e.g. bumpalo's `reset`) ships with.
pub struct Arena {
    inner: UnsafeCell<Inner>,
}

struct Inner {
    /// Append-only list of chunks; the last one is the active bump chunk.
    chunks: Vec<Chunk>,
    /// Chunks released by a restore, kept for reuse so repeated deep recursion
    /// stays memory-bounded. Holds only canonical ladder-sized chunks.
    free: Vec<Chunk>,
    /// Capacity hint for the next grown chunk; clamped to
    /// `[FIRST_CHUNK, MAX_CHUNK]` and snapped to a ladder value at acquisition.
    next_size: usize,
}

struct Chunk {
    ptr: NonNull<u8>,
    /// Exact capacity the allocation was made with.
    cap: usize,
    /// Bump cursor; only meaningful for the active (last) chunk, where it is
    /// always a multiple of [`MIN_ALIGN`].
    len: usize,
    /// Alignment the chunk base was allocated with.
    align: usize,
}

impl Chunk {
    fn release(self) {
        // SAFETY: the chunk was created in `acquire_chunk` from exactly this
        // `(cap, align)` layout, is uniquely owned, and is released exactly once.
        unsafe {
            dealloc(
                self.ptr.as_ptr(),
                Layout::from_size_align_unchecked(self.cap, self.align),
            );
        }
    }
}

/// Smallest ladder capacity (FIRST_CHUNK doubled repeatedly) that is >= `size`.
/// Callers only invoke this with `size <= MAX_CHUNK`, so the result is a ladder
/// value in `[FIRST_CHUNK, MAX_CHUNK]`.
fn ladder_at_least(size: usize) -> usize {
    let mut cap = FIRST_CHUNK;
    while cap < size {
        cap *= 2;
    }
    cap
}

/// Snapshot of arena memory state, for the residency accounting M4's acceptance
/// requires and for tests.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ArenaStats {
    /// Number of chunks currently in the bump list.
    pub chunks: usize,
    /// Number of chunks sitting in the free list.
    pub pooled_chunks: usize,
    /// Bytes handed out (sum of bump cursors; the abandoned tail of each full
    /// chunk counts as used because nothing else can claim it).
    pub used_bytes: usize,
    /// Total bytes owned across both lists.
    pub capacity_bytes: usize,
}

// SAFETY: the arena uniquely owns its chunks; moving it to another thread moves
// that ownership (fibers are thread-confined [C4], transferred whole under E3).
// `Sync` is deliberately not implemented: allocation mutates through `&self`.
unsafe impl Send for Arena {}

impl Arena {
    /// Creates an arena that allocates nothing until the first allocation.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            inner: UnsafeCell::new(Inner {
                chunks: Vec::new(),
                free: Vec::new(),
                next_size: FIRST_CHUNK,
            }),
        }
    }

    /// Returns the current position; allocations after this point die at
    /// [`Arena::restore(mark)`](Arena::restore).
    #[must_use]
    pub fn mark(&self) -> Mark {
        let inner = self.inner();
        match inner.chunks.last() {
            Some(last) => Mark {
                chunk: inner.chunks.len() - 1,
                offset: last.len,
            },
            None => Mark {
                chunk: 0,
                offset: 0,
            },
        }
    }

    /// Low-level allocation of `layout` bytes; every returned pointer is aligned
    /// to `max(layout.align(), MIN_ALIGN)`.
    ///
    /// Oversized (`> MAX_CHUNK`) and over-aligned (`> MIN_ALIGN`) requests get a
    /// dedicated chunk; normal requests bump the active chunk, growing by chunk
    /// append when it is full.
    ///
    /// Zero-sized requests return an aligned dangling pointer that never
    /// touches arena memory and is not invalidated by restores.
    ///
    /// # Panics
    /// If the rounded-up size overflows `usize` or the global allocator returns
    /// null.
    pub fn alloc_layout(&self, layout: Layout) -> NonNull<u8> {
        let size = layout.size();
        let align = layout.align();
        if size == 0 {
            // Zero-sized allocations never dereference; `align` as an address is
            // the canonical aligned dangling pointer (miri-clean: it carries no
            // provenance and never touches arena memory).
            return NonNull::new(ptr::without_provenance_mut(align)).expect("align is nonzero");
        }
        let fit = size.next_multiple_of(MIN_ALIGN);
        let dedicated = align > MIN_ALIGN;
        let inner = self.inner();
        if !dedicated {
            if let Some(last) = inner.chunks.last_mut() {
                let end = last
                    .len
                    .checked_add(fit)
                    .expect("arena: allocation size overflow");
                if end <= last.cap {
                    let start = last.len;
                    last.len = end;
                    // SAFETY: `start + fit <= cap` keeps the pointer inside this
                    // chunk's allocation; `start` is MIN_ALIGN-aligned in a
                    // MIN_ALIGN-aligned chunk, so any request align <= MIN_ALIGN
                    // is satisfied.
                    return unsafe { NonNull::new_unchecked(last.ptr.as_ptr().add(start)) };
                }
            }
        }
        // Slow path: append a new chunk (never realloc an existing one, so
        // addresses already handed out stay stable [C4/H4]).
        let (cap, chunk_align) = if dedicated || fit > MAX_CHUNK {
            (fit, align.max(MIN_ALIGN))
        } else {
            // Snap to the ladder: `next_size` can be a non-ladder hint after a
            // restore anchored on a dedicated chunk, and every non-dedicated
            // chunk must be ladder-sized so restore can pool it.
            (ladder_at_least(inner.next_size.max(fit)), MIN_ALIGN)
        };
        let mut chunk = inner.acquire_chunk(cap, chunk_align);
        let ptr = chunk.ptr;
        chunk.len = fit;
        inner.chunks.push(chunk);
        inner.next_size = cap.saturating_mul(2).clamp(FIRST_CHUNK, MAX_CHUNK);
        ptr
    }

    /// Places `value` in the arena and returns an exclusive reference to it.
    ///
    /// # Panics
    /// See [`Arena::alloc_layout`].
    // Sound against `&self`: the reference targets memory this call just carved
    // out of the arena, no alias of it exists, and `Arena` is !Sync so handout
    // is thread-confined.
    #[allow(clippy::mut_from_ref)]
    pub fn alloc<T>(&self, value: T) -> &mut T {
        let ptr = self.alloc_layout(Layout::new::<T>()).cast::<T>();
        // SAFETY: `alloc_layout` returned memory valid and aligned for a `T`
        // that this arena exclusively owns; the reference is unique.
        unsafe {
            ptr.as_ptr().write(value);
            &mut *ptr.as_ptr()
        }
    }

    /// Copies `source` into the arena and returns the copy as an exclusive slice.
    ///
    /// # Panics
    /// See [`Arena::alloc_layout`].
    // Sound for the same reason as [`Arena::alloc`]: freshly carved, unaliased,
    // thread-confined memory.
    #[allow(clippy::mut_from_ref)]
    pub fn alloc_slice_copy<T: Copy>(&self, source: &[T]) -> &mut [T] {
        let ptr = self.alloc_layout(Layout::for_value(source)).cast::<T>();
        // SAFETY: the destination was just allocated with `source`'s layout and
        // cannot overlap it (the arena owns the destination exclusively).
        unsafe { ptr::copy_nonoverlapping(source.as_ptr(), ptr.as_ptr(), source.len()) };
        // SAFETY: `ptr` is valid for `source.len()` elements of properly aligned
        // `T`, uniquely owned by the arena.
        unsafe { core::slice::from_raw_parts_mut(ptr.as_ptr(), source.len()) }
    }

    /// Allocates `len` copies of `value`.
    ///
    /// # Panics
    /// See [`Arena::alloc_layout`].
    // Sound for the same reason as [`Arena::alloc`]: freshly carved, unaliased,
    // thread-confined memory.
    #[allow(clippy::mut_from_ref)]
    pub fn alloc_slice_fill<T: Copy>(&self, len: usize, value: T) -> &mut [T] {
        let ptr = self
            .alloc_layout(Layout::array::<T>(len).expect("arena: slice layout overflow"))
            .cast::<T>();
        // SAFETY: `ptr` is valid for `len` elements of properly aligned `T`;
        // filling happens before the exclusive slice escapes.
        let slice = unsafe { core::slice::from_raw_parts_mut(ptr.as_ptr(), len) };
        slice.fill(value);
        slice
    }

    /// Frees everything allocated after `mark` by resetting the bump cursor
    /// (C4: "reclamation a restore on frame pop").
    ///
    /// Chunks appended after the mark's chunk are canonical ladder-sized ones
    /// (kept in the per-arena free list for reuse, which bounds the memory of
    /// repeated deep recursion) or dedicated chunks (returned to the allocator).
    /// Restoring the same mark twice is a no-op the second time.
    ///
    /// # Panics
    /// If `mark` is stale (older than an already-restored mark, or referencing
    /// a freed chunk); marks must be restored in LIFO order.
    pub fn restore(&self, mark: Mark) {
        let inner = self.inner();
        if inner.chunks.is_empty() {
            assert!(
                mark.chunk == 0 && mark.offset == 0,
                "arena: restore of a mark that outlived its chunk"
            );
            inner.next_size = FIRST_CHUNK;
            return;
        }
        assert!(
            mark.chunk < inner.chunks.len(),
            "arena: mark references a freed chunk (restore LIFO)"
        );
        let last = &inner.chunks[mark.chunk];
        assert!(
            mark.offset <= last.len,
            "arena: mark predates a restore below it (restore LIFO)"
        );
        for chunk in inner.chunks.drain((mark.chunk + 1)..) {
            if chunk.align == MIN_ALIGN
                && chunk.cap.is_power_of_two()
                && (FIRST_CHUNK..=MAX_CHUNK).contains(&chunk.cap)
            {
                inner.free.push(chunk);
            } else {
                chunk.release();
            }
        }
        let last = &mut inner.chunks[mark.chunk];
        last.len = mark.offset;
        inner.next_size = last.cap.saturating_mul(2).clamp(FIRST_CHUNK, MAX_CHUNK);
    }

    /// Snapshot of chunk counts and byte usage.
    #[must_use]
    pub fn stats(&self) -> ArenaStats {
        let inner = self.inner();
        let mut used = 0;
        let mut capacity = 0;
        for chunk in &inner.chunks {
            used += chunk.len;
            capacity += chunk.cap;
        }
        for chunk in &inner.free {
            capacity += chunk.cap;
        }
        ArenaStats {
            chunks: inner.chunks.len(),
            pooled_chunks: inner.free.len(),
            used_bytes: used,
            capacity_bytes: capacity,
        }
    }

    #[inline]
    // Sound against `&self`: this is the `UnsafeCell` interior-mutability escape
    // hatch (like `RefCell` without the checks). `Arena` is !Sync, so access is
    // confined to the owning thread, and no method runs user code or reenters
    // the arena while an `&mut Inner` is live.
    #[allow(clippy::mut_from_ref)]
    fn inner(&self) -> &mut Inner {
        // SAFETY: `Arena` is !Sync, so `&self` is confined to the owning thread;
        // every borrow here is transient (no method calls user code or reenters
        // the arena while it is live), so no two `&mut Inner` coexist.
        unsafe { &mut *self.inner.get() }
    }
}

impl Default for Arena {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for Arena {
    fn drop(&mut self) {
        let inner = self.inner();
        for chunk in inner.chunks.drain(..) {
            chunk.release();
        }
        for chunk in inner.free.drain(..) {
            chunk.release();
        }
    }
}

impl Inner {
    /// Returns an uninitialized chunk with the exact capacity `cap`, reusing a
    /// pooled one when `cap` is a canonical ladder size (bump allocations are
    /// always written before they are read, so stale contents are harmless).
    fn acquire_chunk(&mut self, cap: usize, align: usize) -> Chunk {
        if align == MIN_ALIGN {
            if let Some(pos) = self.free.iter().position(|c| c.cap == cap) {
                let mut chunk = self.free.swap_remove(pos);
                chunk.len = 0;
                return chunk;
            }
        }
        let layout = Layout::from_size_align(cap, align).expect("arena: chunk layout overflow");
        // SAFETY: `cap >= MIN_ALIGN > 0` and `align` is a power of two, so
        // `layout` is valid for the global allocator; null is turned into a panic.
        let ptr = unsafe { alloc(layout) };
        let ptr = NonNull::new(ptr).expect("arena: global allocator returned null");
        Chunk {
            ptr,
            cap,
            len: 0,
            align,
        }
    }
}
