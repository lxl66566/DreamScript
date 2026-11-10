//! Integration tests for the RC half of ds-rt: inc/dec balance, deinit
//! exactly-once across normal/early-return/unwind paths, reuse threading
//! (candidate consumption, class compatibility, pool cap), deterministic free
//! and reuse order, arena coexistence, and (behind `diag`) the borrow counter
//! and rc tripwires.

use std::{
    cell::RefCell,
    panic::{AssertUnwindSafe, catch_unwind},
    ptr::{self, NonNull},
};

use ds_rt::{Heap, POOL_DEPTH};

/// Payload used everywhere: a tag the deinit hook logs, plus (for class
/// variety) 32 bytes so blocks land in a bigger size class than `Tag` alone.
#[derive(Clone, Copy, PartialEq, Debug)]
struct Tag(u64);

#[derive(Clone, Copy)]
struct Wide([u64; 4]);

thread_local! {
    static DEINIT_LOG: RefCell<Vec<u64>> = const { RefCell::new(Vec::new()) };
}

fn log_deinit(payload: NonNull<u8>) {
    // SAFETY: the hook runs at the unique death with the payload pointer the
    // allocation site computed; the payload is valid, initialized, and sits at
    // an offset aligned for its type.
    let tag = unsafe { ptr::read(payload.cast::<Tag>().as_ptr()) }.0;
    DEINIT_LOG.with(|log| log.borrow_mut().push(tag));
}

fn take_log() -> Vec<u64> {
    DEINIT_LOG.with(|log| std::mem::take(&mut *log.borrow_mut()))
}

#[test]
fn clone_inc_and_drop_dec_balance_to_unique_death() {
    let heap = Heap::new();
    let a = heap.alloc_box(Tag(1), Some(log_deinit));
    assert_eq!(a.rc(), 1);
    assert!(a.is_unique());
    let mut clones = vec![a.clone(), a.clone(), a.clone()];
    assert_eq!(a.rc(), 4);
    drop(clones.pop().expect("three clones"));
    assert_eq!(a.rc(), 3);
    drop(clones.pop().expect("two clones"));
    drop(clones.pop().expect("one clone"));
    assert_eq!(a.rc(), 1);
    assert!(take_log().is_empty(), "shared drops never deinit");
    drop(a);
    assert_eq!(take_log(), vec![1], "unique death deinits exactly once");
    assert_eq!(heap.stats().pooled_blocks, 1);
}

#[test]
fn deinit_runs_exactly_once_on_every_path() {
    // Normal: falls off the end.
    fn normal(heap: &Heap) {
        let _held = heap.alloc_box(Tag(1), Some(log_deinit));
    }
    // Early return: the binding dies mid-body.
    fn early(heap: &Heap, bail: bool) -> Option<u64> {
        let held = heap.alloc_box(Tag(2), Some(log_deinit));
        if bail {
            return None;
        }
        Some(held.0)
    }
    let heap = Heap::new();
    normal(&heap);
    assert_eq!(early(&heap, true), None);
    // Panic: the binding dies during unwinding.
    let panicked = catch_unwind(AssertUnwindSafe(|| {
        let _held = heap.alloc_box(Tag(3), Some(log_deinit));
        panic!("unwind through the drop glue");
    }));
    assert!(panicked.is_err());
    assert_eq!(
        take_log(),
        vec![1, 2, 3],
        "each death deinits once, in drop order"
    );
}

#[test]
fn reuse_candidate_consumed_by_next_compatible_alloc() {
    let heap = Heap::new();
    let a = heap.alloc_box(Wide([0; 4]), None);
    let addr_a = a.payload().as_ptr();
    drop(a);
    assert_eq!(heap.stats().pooled_blocks, 1);
    // Same payload type: same class, the candidate is consumed (alloc_at).
    let b = heap.alloc_box(Wide([1; 4]), None);
    assert_eq!(
        b.payload().as_ptr(),
        addr_a,
        "alloc_at reuses the dead block"
    );
    assert_eq!(b.0, [1; 4], "the reused block carries the new payload");
    assert_eq!(heap.stats().pooled_blocks, 0);
    // A different class neither consumes the Tag candidate nor pools wrongly.
    let wide = heap.alloc_box([0u64; 32], None);
    drop(wide);
    let small = heap.alloc_box(Tag(9), None);
    assert_eq!(*small, Tag(9));
    assert_eq!(
        heap.stats().pooled_blocks,
        1,
        "the class-512 candidate survives a class-32 allocation"
    );
}

#[test]
fn pool_depth_caps_and_lifo_reuse() {
    let heap = Heap::new();
    let blocks: Vec<_> = (0..(POOL_DEPTH as usize + 2))
        .map(|i| heap.alloc_box(Wide([i as u64; 4]), None))
        .collect();
    let addrs: Vec<_> = blocks.iter().map(|b| b.payload().as_ptr()).collect();
    for b in blocks {
        drop(b);
    }
    assert_eq!(heap.stats().pooled_blocks, POOL_DEPTH as usize);
    // LIFO under the cap: drops past the cap went to the allocator, so the
    // head is the most recently *pooled* block - the POOL_DEPTH-th one.
    let next = heap.alloc_box(Wide([0; 4]), None);
    assert_eq!(next.payload().as_ptr(), addrs[POOL_DEPTH as usize - 1]);
    drop(next);
}

#[test]
fn free_and_reuse_order_is_deterministic_across_runs() {
    fn scene() -> Vec<u64> {
        let heap = Heap::new();
        let a = heap.alloc_box(Wide([10; 4]), Some(log_deinit));
        let b = heap.alloc_box(Wide([20; 4]), Some(log_deinit));
        let addr_a = a.payload().as_ptr();
        let addr_b = b.payload().as_ptr();
        let b2 = b.clone();
        drop(b2); // shared death: no deinit, no pooling
        drop(a);
        drop(b);
        // Two fresh allocations of the same class: LIFO hands back b's block
        // first, then a's - the free/reuse order is a pure function of the
        // drop sequence, repeatable run after run.
        let c = heap.alloc_box(Wide([30; 4]), None);
        let d = heap.alloc_box(Wide([40; 4]), None);
        assert_eq!(
            c.payload().as_ptr(),
            addr_b,
            "LIFO: most recent death first"
        );
        assert_eq!(d.payload().as_ptr(), addr_a);
        take_log()
    }
    let log1 = scene();
    let log2 = scene();
    assert_eq!(log1, vec![10, 20]);
    assert_eq!(log1, log2, "deinit order identical across runs");
}

#[test]
fn arena_frames_and_boxed_blocks_coexist_per_fiber() {
    let heap = Heap::new();
    let boxed = heap.alloc_box(Tag(7), None);
    let boxed_addr = boxed.payload().as_ptr();
    {
        let _frame = heap.frame();
        let tmp = heap.arena().alloc(99u64);
        assert_eq!(*tmp, 99);
    }
    // The frame restore reclaimed the temporary; the counted block is intact
    // at the same address - boxed blocks are never arena-backed.
    assert_eq!(*boxed, Tag(7));
    assert_eq!(boxed.payload().as_ptr(), boxed_addr);
    assert_eq!(boxed.rc(), 1);
    let stats = heap.stats();
    assert_eq!(stats.arena.used_bytes, 0);
    assert_eq!(stats.pooled_blocks, 0);
    drop(boxed);
}

#[cfg(feature = "diag")]
mod diag {
    use std::mem::ManuallyDrop;

    use super::*;

    #[test]
    fn exclusive_window_flags_aliased_access() {
        let heap = Heap::new();
        let a = heap.alloc_box(Tag(1), None);
        {
            let mut win = a.borrow_exclusive();
            *win = Tag(11);
            assert_eq!(*win, Tag(11));
            // Use during the exclusive window: flagged.
            assert!(catch_unwind(AssertUnwindSafe(|| a.borrow_shared())).is_err());
            assert!(catch_unwind(AssertUnwindSafe(|| a.borrow_exclusive())).is_err());
        }
        // Window closed: borrows work again.
        let read = a.borrow_shared();
        assert_eq!(*read, Tag(11));
        drop(read);
        assert_eq!(a.rc(), 1);
    }

    #[test]
    fn exclusive_borrow_requires_uniqueness() {
        let heap = Heap::new();
        let a = heap.alloc_box(Tag(1), None);
        let alias = a.clone();
        assert!(catch_unwind(AssertUnwindSafe(|| a.borrow_exclusive())).is_err());
        drop(alias);
        drop(a);
    }

    #[test]
    fn death_with_outstanding_borrow_flags() {
        let heap = Heap::new();
        let a = heap.alloc_box(Tag(1), Some(log_deinit));
        let header = a.header();
        let payload = a.payload().cast::<u8>();
        // Open a shared borrow through an alias (the guard outlives `a`).
        let alias = a.clone();
        let shared = alias.borrow_shared();
        let _leak = ManuallyDrop::new(a);
        assert!(catch_unwind(AssertUnwindSafe(|| { heap.rc_dec_raw(header, payload) })).is_err());
        assert!(take_log().is_empty(), "death aborted before deinit ran");
        drop(shared);
        drop(alias);
    }

    #[test]
    fn rc_underflow_through_pooled_block_flags() {
        let heap = Heap::new();
        let a = ManuallyDrop::new(heap.alloc_box(Tag(1), None));
        let header = a.header();
        let payload = a.payload().cast::<u8>();
        assert!(heap.rc_dec_raw(header, payload));
        assert_eq!(heap.stats().pooled_blocks, 1);
        // A second dec through the pooled block trips the rc tombstone.
        assert!(catch_unwind(AssertUnwindSafe(|| { heap.rc_dec_raw(header, payload) })).is_err());
    }
}
