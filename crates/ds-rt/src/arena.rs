//! Per-frame bump arena over an append-only chunk list.

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

/// How an arena position failed to name live memory. The VM maps each
/// class to one structured trap; the panicking [`Arena::restore`] keeps
/// its own misuse contract for internal callers.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ArenaMisuse {
    /// The position names a chunk index the arena never grew to (or one
    /// a restore freed below it).
    Chunk,
    /// The position's range runs past its chunk's bump cursor (freed by
    /// a restore, or never allocated).
    Range,
}

impl Mark {
    /// The position at `chunk`'s `offset` - the packed-handle reader's
    /// reconstruction from the fields a VM word carries.
    #[must_use]
    pub const fn at(chunk: usize, offset: usize) -> Self {
        Self { chunk, offset }
    }

    /// The chunk index.
    #[must_use]
    pub const fn chunk(self) -> usize {
        self.chunk
    }

    /// The byte offset within the chunk.
    #[must_use]
    pub const fn offset(self) -> usize {
        self.offset
    }
}

/// A bump arena for non-escaping temporaries.
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
/// Grants are raw pointers because a restore hands the reclaimed bytes to the
/// next allocation: no borrow-checkable lifetime can span the mark discipline,
/// so the aliasing contract sits on the caller. A pointer is live until the
/// first restore past its mark; a restore invalidates every grant the arena
/// handed out after the restored mark, and only the latest grant for a byte
/// range is live. Dereferencing a dead grant (or holding a reference to its
/// bytes across the later grant that reuses them) is undefined behavior - the
/// same contract every bump allocator (e.g. bumpalo's `reset`) ships with;
/// linearity upholds it at the language level, the VM's claim ledger at the
/// runtime level.
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
    /// One entry per arena-allocating frame, pushed at its first allocation
    /// and popped at its return: the frame-pop restore of the contract's
    /// reclamation clause, staged here so the VM drives it through `&self`
    /// beside the bytes themselves.
    frame_marks: Vec<Mark>,
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

/// Snapshot of arena memory state, for the residency accounting the
/// runtime keeps and for tests.
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
// that ownership (fibers are thread-confined, transferred whole on fork-join).
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
                frame_marks: Vec::new(),
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
        self.alloc_located(layout).1
    }

    /// [`Arena::alloc_layout`] plus the allocation's position: a fast-path
    /// bump keeps its chunk index and start offset, an appended chunk starts
    /// at its own index and offset zero. Zero-sized allocations report the
    /// current position and the canonical dangling pointer.
    fn alloc_located(&self, layout: Layout) -> (Mark, NonNull<u8>) {
        let size = layout.size();
        let align = layout.align();
        if size == 0 {
            // Zero-sized allocations never dereference; `align` as an address is
            // the canonical aligned dangling pointer (miri-clean: it carries no
            // provenance and never touches arena memory).
            return (
                self.mark(),
                NonNull::new(ptr::without_provenance_mut(align)).expect("align is nonzero"),
            );
        }
        let fit = size.next_multiple_of(MIN_ALIGN);
        let dedicated = align > MIN_ALIGN;
        let inner = self.inner();
        if !dedicated {
            let chunk_index = inner.chunks.len().wrapping_sub(1);
            if let Some(last) = inner.chunks.last_mut() {
                let end = last
                    .len
                    .checked_add(fit)
                    .expect("arena: allocation size overflow");
                if end <= last.cap {
                    let start = last.len;
                    last.len = end;
                    let at = Mark {
                        chunk: chunk_index,
                        offset: start,
                    };
                    // SAFETY: `start + fit <= cap` keeps the pointer inside this
                    // chunk's allocation; `start` is MIN_ALIGN-aligned in a
                    // MIN_ALIGN-aligned chunk, so any request align <= MIN_ALIGN
                    // is satisfied.
                    return (at, unsafe {
                        NonNull::new_unchecked(last.ptr.as_ptr().add(start))
                    });
                }
            }
        }
        // Slow path: append a new chunk (never realloc an existing one, so
        // addresses already handed out stay stable).
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
        let at = Mark {
            chunk: inner.chunks.len(),
            offset: 0,
        };
        inner.chunks.push(chunk);
        inner.next_size = cap.saturating_mul(2).clamp(FIRST_CHUNK, MAX_CHUNK);
        (at, ptr)
    }

    /// Places `value` in the arena and returns the raw pointer to it.
    ///
    /// # Arena aliasing contract
    /// The pointer stays valid until the first [`Arena::restore`] past this
    /// allocation's mark; a restore invalidates every grant made after the
    /// restored mark, and only the latest grant for a byte range is live
    /// (see the type-level misuse contract). Callers hold the contract the
    /// way the VM does: its claim ledger never reaches a word across the
    /// frame-pop restore that freed it.
    ///
    /// # Panics
    /// See [`Arena::alloc_layout`].
    pub fn alloc<T>(&self, value: T) -> NonNull<T> {
        let ptr = self.alloc_layout(Layout::new::<T>()).cast::<T>();
        // SAFETY: `alloc_layout` returned memory valid and aligned for a `T`
        // that this arena exclusively owns; the raw write initializes it
        // without pushing a reference tag onto the arena's memory (a reference
        // would be invalidated by the next grant to the same bytes after a
        // restore, which is why the handout is raw).
        unsafe { ptr.as_ptr().write(value) };
        ptr
    }

    /// Copies `source` into the arena and returns the raw pointer to the copy's
    /// first element; the slice runs `source.len()` elements.
    ///
    /// # Arena aliasing contract
    /// As [`Arena::alloc`].
    ///
    /// # Panics
    /// See [`Arena::alloc_layout`].
    pub fn alloc_slice_copy<T: Copy>(&self, source: &[T]) -> NonNull<T> {
        let ptr = self.alloc_layout(Layout::for_value(source)).cast::<T>();
        // SAFETY: the destination was just allocated with `source`'s layout and
        // cannot overlap it (the arena owns the destination exclusively).
        unsafe { ptr::copy_nonoverlapping(source.as_ptr(), ptr.as_ptr(), source.len()) };
        ptr
    }

    /// Allocates `len` copies of `value` and returns the raw pointer to the
    /// first element.
    ///
    /// # Arena aliasing contract
    /// As [`Arena::alloc`].
    ///
    /// # Panics
    /// See [`Arena::alloc_layout`].
    pub fn alloc_slice_fill<T: Copy>(&self, len: usize, value: T) -> NonNull<T> {
        let ptr = self
            .alloc_layout(Layout::array::<T>(len).expect("arena: slice layout overflow"))
            .cast::<T>();
        // SAFETY: `ptr` is valid for `len` elements of properly aligned `T`; the
        // raw writes initialize the range before the pointer escapes, and stay
        // raw for the same reason as [`Arena::alloc`]'s write.
        unsafe {
            for i in 0..len {
                ptr.as_ptr().add(i).write(value);
            }
        }
        ptr
    }

    /// The checked face of [`Arena::restore`]: the same LIFO discipline
    /// surfaced as a class the VM traps on for hand-assembled code.
    pub fn try_restore(&self, mark: Mark) -> Result<(), ArenaMisuse> {
        let inner = self.inner();
        if inner.chunks.is_empty() {
            if mark.chunk == 0 && mark.offset == 0 {
                inner.next_size = FIRST_CHUNK;
                return Ok(());
            }
            return Err(ArenaMisuse::Chunk);
        }
        let last = inner.chunks.get(mark.chunk).ok_or(ArenaMisuse::Chunk)?;
        if mark.offset > last.len {
            return Err(ArenaMisuse::Range);
        }
        self.restore(mark);
        Ok(())
    }

    /// Frees everything allocated after `mark` by resetting the bump cursor
    /// (reclamation is a restore on frame pop).
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

    /// Bump-allocates `a` followed by `b` as one contiguous byte range and
    /// returns its position - the chunk-and-offset form the VM packs into its
    /// arena word beside the length. Byte payloads only, so alignment stays
    /// [`MIN_ALIGN`].
    ///
    /// # Panics
    /// See [`Arena::alloc_layout`].
    pub fn alloc_concat(&self, a: &[u8], b: &[u8]) -> Mark {
        let total = a
            .len()
            .checked_add(b.len())
            .expect("arena: allocation size overflow");
        let (at, ptr) =
            self.alloc_located(Layout::from_size_align(total, 1).expect("arena: layout overflow"));
        // SAFETY: the destination was just carved out of the arena and no
        // alias of it exists; the arena never borrows from the sources'
        // owners.
        unsafe {
            ptr.as_ptr().copy_from_nonoverlapping(a.as_ptr(), a.len());
            ptr.as_ptr()
                .add(a.len())
                .copy_from_nonoverlapping(b.as_ptr(), b.len());
        }
        at
    }

    /// The `len` bytes at `at` as a raw slice: the reader side of the packed
    /// handle. The range must be live - allocated after the last restore past
    /// it - which the placement proof guarantees for every handle lowered code
    /// serves; a hand-packed handle that fails the bounds names its class
    /// instead of reading.
    ///
    /// # Arena aliasing contract
    /// As [`Arena::alloc`]: the raw slice is live until a restore reclaims its
    /// range, and materializing a `&[u8]` from it is sound only while the
    /// grant is live (the caller owns that discipline, as the VM's claim
    /// ledger does for arena words).
    pub fn bytes_at(&self, at: Mark, len: usize) -> Result<*const [u8], ArenaMisuse> {
        let end = at.offset.checked_add(len).ok_or(ArenaMisuse::Range)?;
        let chunk = self
            .inner()
            .chunks
            .get(at.chunk)
            .ok_or(ArenaMisuse::Chunk)?;
        if end > chunk.len {
            return Err(ArenaMisuse::Range);
        }
        // SAFETY: `end <= chunk.len <= chunk.cap` keeps the range inside this
        // chunk's allocation, and the raw slice never pushes a reference tag
        // onto memory a later grant may reuse.
        Ok(unsafe { ptr::slice_from_raw_parts(chunk.ptr.as_ptr().add(at.offset), len) })
    }

    /// Pushes the current position as one arena-allocating frame's entry
    /// mark. Called at the frame's first allocation, so the mark equals the
    /// frame's entry position by construction (no allocation intervened).
    pub fn push_frame_mark(&self) {
        let at = self.mark();
        self.inner().frame_marks.push(at);
    }

    /// Pops the returning frame's entry mark and restores the arena to it,
    /// reclaiming every temporary the frame allocated.
    ///
    /// # Panics
    /// If the stack is empty (the frame never allocated).
    pub fn pop_frame_mark(&self) {
        let at = self
            .inner()
            .frame_marks
            .pop()
            .expect("arena: frame mark underflow");
        self.restore(at);
    }

    /// Wholesale reset at the fiber death doors: every frame mark drops and
    /// the arena returns to its base position, so a recycled slot starts
    /// empty whatever its death left mid-flight.
    pub fn release_all(&self) {
        self.inner().frame_marks.clear();
        self.restore(Mark {
            chunk: 0,
            offset: 0,
        });
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

impl core::fmt::Debug for Arena {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let stats = self.stats();
        f.debug_struct("Arena")
            .field("chunks", &stats.chunks)
            .field("used_bytes", &stats.used_bytes)
            .field("capacity_bytes", &stats.capacity_bytes)
            .finish()
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
        if align == MIN_ALIGN
            && let Some(pos) = self.free.iter().position(|c| c.cap == cap)
        {
            let mut chunk = self.free.swap_remove(pos);
            chunk.len = 0;
            return chunk;
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
