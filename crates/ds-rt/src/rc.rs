//! Precise non-atomic reference counting over heap block headers (C4/C5).
//!
//! A counted value is one heap block: an [`RcHeader`] followed by the payload.
//! The header carries the non-atomic count, the exact block layout (so death
//! can free or reuse-thread without external bookkeeping), the dormant
//! collector word, and the `deinit` fn pointer. The pointer is fixed per
//! concrete type at the allocation site - the lowering knows the type, so
//! dispatch is static, never a runtime type inspection (C5).
//!
//! Non-atomicity is sound under thread confinement: a block and its count are
//! only ever touched by the thread owning the [`crate::Heap`], because E3's
//! fork-join transfers ownership of whole values (and whole heaps) rather than
//! sharing them. `RcHeader` is accordingly not `Sync` by construction.

use core::{marker::PhantomData, ptr::NonNull};

use crate::heap::Heap;

/// The `deinit` hook (C5): receives the payload pointer of the dying instance
/// and runs exactly once, at the unique death, on every path - normal return,
/// early return, and panic unwinding (the [`RcRef`] drop glue routes all of
/// them through the same `rc_dec`).
pub type DeinitFn = fn(NonNull<u8>);

/// The standard `deinit` hook for a payload owning Rust memory: drops the
/// payload in place at the unique death, so a pooled block never leaks it.
/// Owners whose payload owns nothing pass `None` instead.
#[must_use]
pub fn drop_deinit<T>() -> DeinitFn {
    fn drop_payload<T>(payload: NonNull<u8>) {
        // SAFETY: the payload was written as `T` at allocation; the hook
        // runs exactly once, at the unique death, before pooling.
        unsafe { payload.cast::<T>().drop_in_place() };
    }
    drop_payload::<T>
}

/// Header of a reference-counted heap block. `repr(C)` because the layout is
/// the committed format: `flags` is the two-hook collector word (cycle-candidate
/// registration bit, weak-ref head) that H4 commits to carrying dormant - always
/// zero in v0, reserved for the growth path.
#[repr(C)]
pub struct RcHeader {
    /// Count of live references; 1 means the instance is unique. Non-atomic;
    /// thread confinement of the owning heap keeps it sound (C4).
    pub(crate) rc: u32,
    /// Dormant collector word. Zero now; bits are reserved for the H4 two-hook
    /// interface so the header format never migrates.
    pub(crate) flags: u32,
    /// Exact capacity of the whole block in bytes (header + payload).
    pub(crate) size: u32,
    /// Alignment the block was allocated with.
    pub(crate) align: u32,
    /// Inko-style borrow state: 0 none, -1 exclusive window, >0 shared
    /// borrows. Only present under `diag`; zero-cost otherwise.
    #[cfg(feature = "diag")]
    pub(crate) borrows: i32,
    /// Static `deinit` dispatch (C5); `None` for types without one. When the
    /// block is a pooled reuse candidate this slot holds the freelist `next`
    /// pointer instead (see `heap::Heap::thread_block`).
    pub(crate) deinit: Option<DeinitFn>,
}

impl RcHeader {
    /// Fresh header for a uniquely-owned live block.
    pub(crate) fn fresh(size: usize, align: usize, deinit: Option<DeinitFn>) -> Self {
        Self {
            rc: 1,
            flags: 0,
            size: u32::try_from(size).expect("ds-rt: block larger than u32 bytes"),
            align: u32::try_from(align).expect("ds-rt: block alignment larger than u32"),
            #[cfg(feature = "diag")]
            borrows: 0,
            deinit,
        }
    }

    /// The live reference count.
    #[must_use]
    pub fn rc(&self) -> u32 {
        self.rc
    }

    /// Whether exactly one reference exists - the precondition for destructive
    /// in-place operations (C4/D2).
    #[must_use]
    pub fn is_unique(&self) -> bool {
        self.rc == 1
    }

    /// `rc_inc` (C4): one more explicit clone owns the block.
    pub(crate) fn inc_at(header: NonNull<Self>) {
        // SAFETY: `header` names a live block on this (owning) thread; the
        // count is non-atomic and confined, so the read-modify-write races with
        // nothing, and the exclusive borrow lasts only this statement.
        unsafe { &mut *header.as_ptr() }.inc();
    }

    pub(crate) fn inc(&mut self) {
        // u32::MAX doubles as the diag overflow tripwire; wrapping keeps the
        // release build branch-free for a state only a balance bug reaches.
        #[cfg(feature = "diag")]
        assert!(
            self.rc != u32::MAX,
            "ds-rt diag: rc overflow (unbalanced inc)"
        );
        self.rc = self.rc.wrapping_add(1);
    }
}

/// An owned reference to a reference-counted payload (C4).
///
/// `clone` is the explicit `rc_inc`; dropping is the `rc_dec`, and a unique
/// death runs `deinit` and threads the block for reuse on every exit path -
/// unwinding included, via ordinary drop glue. Reads dereference freely
/// (shared reads are free under precise RC); in-place mutation goes through
/// [`RcRef::borrow_exclusive`], which requires uniqueness.
pub struct RcRef<'h, T> {
    heap: &'h Heap,
    header: NonNull<RcHeader>,
    payload: NonNull<T>,
}

impl<'h, T> RcRef<'h, T> {
    pub(crate) fn new(heap: &'h Heap, header: NonNull<RcHeader>, payload: NonNull<T>) -> Self {
        Self {
            heap,
            header,
            payload,
        }
    }

    /// The owning heap; the reference is confined to it like the block itself.
    #[must_use]
    pub fn heap(&self) -> &'h Heap {
        self.heap
    }

    /// The header address, for the raw `rc_inc`/`rc_dec` surface.
    #[must_use]
    pub fn header(&self) -> NonNull<RcHeader> {
        self.header
    }

    /// The payload address, the argument deinit hooks receive.
    #[must_use]
    pub fn payload(&self) -> NonNull<T> {
        self.payload
    }

    /// The live reference count.
    #[must_use]
    pub fn rc(&self) -> u32 {
        // SAFETY: the header is alive as long as this reference exists.
        unsafe { self.header.as_ref() }.rc()
    }

    /// Whether this is the only live reference (C4/D2's in-place gate).
    #[must_use]
    pub fn is_unique(&self) -> bool {
        self.rc() == 1
    }

    /// Opens an exclusive mutation window (D1): valid only while the instance
    /// is unique. Under `diag` the borrow counter flags any use inside the
    /// window (a second borrow attempt asserts) and any outstanding borrow at
    /// death; without it the uniqueness precondition is a contract.
    #[must_use]
    pub fn borrow_exclusive(&self) -> ExclusiveBorrow<'_, T> {
        #[cfg(feature = "diag")]
        {
            // SAFETY: the header is alive; confined to this thread.
            let h = unsafe { &mut *self.header.as_ptr() };
            assert!(
                h.is_unique(),
                "ds-rt diag: exclusive borrow of shared value"
            );
            h.borrow_exclusive();
        }
        ExclusiveBorrow {
            #[cfg(feature = "diag")]
            header: self.header,
            payload: self.payload,
            marker: PhantomData,
        }
    }

    /// Registers a shared read borrow. Under `diag`, taking one while an
    /// exclusive window is open is flagged.
    #[must_use]
    pub fn borrow_shared(&self) -> SharedBorrow<'_, T> {
        #[cfg(feature = "diag")]
        {
            // SAFETY: the header is alive; confined to this thread.
            let h = unsafe { &mut *self.header.as_ptr() };
            h.borrow_shared();
        }
        SharedBorrow {
            #[cfg(feature = "diag")]
            header: self.header,
            payload: self.payload,
            marker: PhantomData,
        }
    }
}

impl<T> Clone for RcRef<'_, T> {
    fn clone(&self) -> Self {
        RcHeader::inc_at(self.header);
        Self {
            heap: self.heap,
            header: self.header,
            payload: self.payload,
        }
    }
}

impl<T> Drop for RcRef<'_, T> {
    fn drop(&mut self) {
        self.heap.dec(self.header, self.payload.cast());
    }
}

impl<T> core::ops::Deref for RcRef<'_, T> {
    type Target = T;

    fn deref(&self) -> &T {
        // SAFETY: the payload is valid for as long as this reference exists;
        // shared reads never invalidate other references.
        unsafe { self.payload.as_ref() }
    }
}

impl<T: core::fmt::Debug> core::fmt::Debug for RcRef<'_, T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("RcRef")
            .field("rc", &self.rc())
            .field("payload", &**self)
            .finish()
    }
}

/// Guard of an exclusive (mut) borrow window (D1). DerefMut gives the payload;
/// dropping closes the window. With `diag` off it only forwards the reference.
pub struct ExclusiveBorrow<'a, T> {
    #[cfg(feature = "diag")]
    header: NonNull<RcHeader>,
    payload: NonNull<T>,
    marker: PhantomData<&'a T>,
}

impl<T> Drop for ExclusiveBorrow<'_, T> {
    fn drop(&mut self) {
        #[cfg(feature = "diag")]
        {
            // SAFETY: the header is alive until the window closes.
            let h = unsafe { self.header.as_mut() };
            h.end_exclusive();
        }
    }
}

impl<T> core::ops::Deref for ExclusiveBorrow<'_, T> {
    type Target = T;

    fn deref(&self) -> &T {
        // SAFETY: the payload outlives the window; uniqueness (asserted or
        // contracted at open) makes the access exclusive.
        unsafe { self.payload.as_ref() }
    }
}

impl<T> core::ops::DerefMut for ExclusiveBorrow<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: exclusive window - no other reference may use the payload,
        // which diag enforces and the contract guarantees.
        unsafe { self.payload.as_mut() }
    }
}

/// Guard of a shared (read) borrow. Diag-only bookkeeping on open/close.
pub struct SharedBorrow<'a, T> {
    #[cfg(feature = "diag")]
    header: NonNull<RcHeader>,
    payload: NonNull<T>,
    marker: PhantomData<&'a T>,
}

impl<T> Drop for SharedBorrow<'_, T> {
    fn drop(&mut self) {
        #[cfg(feature = "diag")]
        {
            // SAFETY: the header is alive until the borrow ends.
            let h = unsafe { self.header.as_mut() };
            h.end_shared();
        }
    }
}

impl<T> core::ops::Deref for SharedBorrow<'_, T> {
    type Target = T;

    fn deref(&self) -> &T {
        // SAFETY: the payload is valid for the borrow's lifetime.
        unsafe { self.payload.as_ref() }
    }
}

impl RcHeader {
    #[cfg(feature = "diag")]
    pub(crate) fn borrow_shared(&mut self) {
        assert!(
            self.borrows >= 0,
            "ds-rt diag: shared borrow during an exclusive window"
        );
        assert!(
            self.borrows < i32::MAX,
            "ds-rt diag: borrow counter overflow"
        );
        self.borrows += 1;
    }

    #[cfg(feature = "diag")]
    pub(crate) fn end_shared(&mut self) {
        assert!(self.borrows > 0, "ds-rt diag: shared-borrow underflow");
        self.borrows -= 1;
    }

    #[cfg(feature = "diag")]
    pub(crate) fn borrow_exclusive(&mut self) {
        assert!(
            self.borrows == 0,
            "ds-rt diag: exclusive borrow while borrows are outstanding"
        );
        self.borrows = -1;
    }

    #[cfg(feature = "diag")]
    pub(crate) fn end_exclusive(&mut self) {
        assert!(self.borrows == -1, "ds-rt diag: exclusive-window underflow");
        self.borrows = 0;
    }

    /// Diag check at the unique death: no borrow may be outstanding when the
    /// instance is destroyed - that is use during an exclusive window (or after
    /// death), the exact aliasing bug the counter exists to flag.
    #[cfg(feature = "diag")]
    pub(crate) fn assert_no_borrows(&self) {
        assert!(
            self.borrows == 0,
            "ds-rt diag: deinit of a value with an outstanding borrow"
        );
    }
}
