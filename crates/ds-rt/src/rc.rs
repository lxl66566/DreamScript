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
//! sharing them. The header itself carries no synchronization; the confinement
//! rides the owning heap being `!Sync` (auto traits alone do not keep a plain
//! header off a shared thread).
//!
//! The consumer drives the count lifecycle through the raw surface: an
//! explicit clone is `rc_inc`, an owner drop is the count half of the split dec
//! ([`crate::Heap::rc_dec_count`]), and the death work ([`crate::Heap::rc_dispose`])
//! runs exactly once at the observed unique death on every caller path.

use core::ptr::NonNull;

/// The `deinit` hook (C5): receives the payload pointer of the dying instance
/// and runs exactly once, at the unique death, inside the death tail
/// ([`crate::Heap::rc_dispose`]) - before the block pools or releases.
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
///
/// The field order follows the access pattern: every clone and every non-unique
/// drop touches `rc` alone, the unique death reads `deinit` next, and only the
/// death tail reads `size` and `align`. The two hot words sit in the header's
/// first sixteen bytes - the span a MIN_ALIGN-aligned block base cannot split
/// across a cache line - while the layout stays at the size the field set asks
/// for on either pointer width.
#[repr(C)]
pub struct RcHeader {
    /// Count of live references; 1 means the instance is unique. Non-atomic;
    /// thread confinement of the owning heap keeps it sound (C4).
    pub(crate) rc: u32,
    /// Dormant collector word. Zero now; bits are reserved for the H4 two-hook
    /// interface so the header format never migrates.
    pub(crate) flags: u32,
    /// Static `deinit` dispatch (C5); `None` for types without one. When the
    /// block is a pooled reuse candidate this slot holds the freelist `next`
    /// pointer instead (see `heap::Heap::thread_block`).
    pub(crate) deinit: Option<DeinitFn>,
    /// Exact capacity of the whole block in bytes (header + payload).
    pub(crate) size: u32,
    /// Alignment the block was allocated with.
    pub(crate) align: u32,
}

impl RcHeader {
    /// Fresh header for a uniquely-owned live block.
    pub(crate) fn fresh(size: usize, align: usize, deinit: Option<DeinitFn>) -> Self {
        Self {
            rc: 1,
            flags: 0,
            size: u32::try_from(size).expect("ds-rt: block larger than u32 bytes"),
            align: u32::try_from(align).expect("ds-rt: block alignment larger than u32"),
            deinit,
        }
    }

    /// The live reference count.
    #[must_use]
    pub fn rc(&self) -> u32 {
        self.rc
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

#[cfg(test)]
mod tests {
    use core::mem::{offset_of, size_of};

    use super::{DeinitFn, RcHeader};
    use crate::arena::MIN_ALIGN;

    /// The count and the death hook are the death path's hot words and share the
    /// header's first MIN_ALIGN bytes, whatever pointer width the target has:
    /// the header never grows past what its field set asks for, and the pair
    /// never straddles a cache line at a MIN_ALIGN-aligned block base.
    #[test]
    fn the_hot_header_words_fit_the_base_alignment() {
        assert_eq!(offset_of!(RcHeader, rc), 0);
        assert!(
            offset_of!(RcHeader, deinit) + size_of::<Option<DeinitFn>>() <= MIN_ALIGN,
            "the count and the hook must not straddle a cache line"
        );
    }
}
