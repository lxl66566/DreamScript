//! Integration tests for the RC half of ds-rt over the raw store surface:
//! inc/dec balance, deinit exactly-once at the unique death, reuse threading
//! (candidate consumption, class compatibility, pool cap), deterministic free
//! and reuse order, arena coexistence, and (behind `diag`) the rc tripwires.

use std::{cell::RefCell, ptr::NonNull};

use ds_rt::{Heap, POOL_DEPTH, RcHeader};

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
    let tag = unsafe { core::ptr::read(payload.cast::<Tag>().as_ptr()) }.0;
    DEINIT_LOG.with(|log| log.borrow_mut().push(tag));
}

fn take_log() -> Vec<u64> {
    DEINIT_LOG.with(|log| std::mem::take(&mut *log.borrow_mut()))
}

/// Allocates a counted block with the logging hook and returns its header.
fn alloc_logged(heap: &Heap, tag: u64) -> NonNull<RcHeader> {
    heap.alloc_store(Tag(tag), Some(log_deinit))
}

/// The store-driven dec: the count half plus the owed death work.
fn dec_store<T>(heap: &Heap, header: NonNull<RcHeader>) {
    assert!(
        heap.rc_dec_count(header),
        "a live birth-count block dies uniquely"
    );
    heap.rc_dispose::<T>(header);
}

#[test]
fn inc_dec_balance_to_unique_death() {
    let heap = Heap::new();
    let h = alloc_logged(&heap, 1);
    assert_eq!(heap.rc_count(h), 1);
    for _ in 0..3 {
        heap.rc_inc_raw(h);
    }
    assert_eq!(heap.rc_count(h), 4);
    for _ in 0..3 {
        assert!(!heap.rc_dec_count(h), "shared drops never reach zero");
    }
    assert_eq!(heap.rc_count(h), 1);
    assert!(take_log().is_empty(), "shared drops never deinit");
    dec_store::<Tag>(&heap, h);
    assert_eq!(take_log(), vec![1], "unique death deinits exactly once");
    assert_eq!(heap.stats().pooled_blocks, 1);
}

#[test]
fn reuse_candidate_consumed_by_next_compatible_alloc() {
    let heap = Heap::new();
    let a = heap.alloc_store(Wide([0; 4]), None);
    assert!(heap.rc_dec_count(a));
    heap.rc_dispose::<Wide>(a);
    assert_eq!(heap.stats().pooled_blocks, 1);
    // Same payload type: same class, the candidate is consumed (alloc_at).
    let b = heap.alloc_store(Wide([1; 4]), None);
    assert_eq!(b, a, "alloc_at reuses the dead block");
    assert_eq!(
        heap.payload::<Wide>(b).0,
        [1; 4],
        "the reused block carries the new payload"
    );
    assert_eq!(heap.stats().pooled_blocks, 0);
    // A different class neither consumes the Tag candidate nor pools wrongly.
    let wide = heap.alloc_store([0u64; 32], None);
    assert!(heap.rc_dec_count(wide));
    heap.rc_dispose::<[u64; 32]>(wide);
    let small = heap.alloc_store(Tag(9), None);
    assert_eq!(heap.payload::<Tag>(small).0, 9);
    assert_eq!(
        heap.stats().pooled_blocks,
        1,
        "the class-512 candidate survives a class-32 allocation"
    );
    dec_store::<Wide>(&heap, b);
    dec_store::<Tag>(&heap, small);
}

#[test]
fn pool_depth_caps_and_lifo_reuse() {
    let heap = Heap::new();
    let blocks: Vec<_> = (0..(POOL_DEPTH as usize + 2))
        .map(|i| heap.alloc_store(Wide([i as u64; 4]), None))
        .collect();
    let addrs: Vec<_> = blocks.clone();
    for h in blocks {
        assert!(heap.rc_dec_count(h));
        heap.rc_dispose::<Wide>(h);
    }
    assert_eq!(heap.stats().pooled_blocks, POOL_DEPTH as usize);
    // LIFO under the cap: drops past the cap went to the allocator, so the
    // head is the most recently *pooled* block - the POOL_DEPTH-th one.
    let next = heap.alloc_store(Wide([0; 4]), None);
    assert_eq!(next, addrs[POOL_DEPTH as usize - 1]);
    assert!(heap.rc_dec_count(next));
    heap.rc_dispose::<Wide>(next);
}

#[test]
fn free_and_reuse_order_is_deterministic_across_runs() {
    fn scene() -> Vec<u64> {
        let heap = Heap::new();
        let a = heap.alloc_store(Wide([10; 4]), Some(log_deinit));
        let b = heap.alloc_store(Wide([20; 4]), Some(log_deinit));
        // Shared death: an inc'd owner drops back without reaching zero.
        heap.rc_inc_raw(b);
        assert!(!heap.rc_dec_count(b));
        for h in [a, b] {
            assert!(heap.rc_dec_count(h));
            heap.rc_dispose::<Wide>(h);
        }
        // Two fresh allocations of the same class: LIFO hands back b's block
        // first, then a's - the free/reuse order is a pure function of the
        // drop sequence, repeatable run after run.
        let c = heap.alloc_store(Wide([30; 4]), None);
        let d = heap.alloc_store(Wide([40; 4]), None);
        assert_eq!(c, b, "LIFO: most recent death first");
        assert_eq!(d, a);
        for h in [c, d] {
            assert!(heap.rc_dec_count(h));
            heap.rc_dispose::<Wide>(h);
        }
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
    let boxed = heap.alloc_store(Tag(7), None);
    {
        let _frame = heap.frame();
        let tmp = heap.arena().alloc(99u64);
        // SAFETY: the grant is live: the frame restore has not run yet.
        assert_eq!(unsafe { *tmp.as_ptr() }, 99);
    }
    // The frame restore reclaimed the temporary; the counted block is intact
    // at the same address - boxed blocks are never arena-backed.
    assert_eq!(heap.payload::<Tag>(boxed).0, 7);
    assert_eq!(heap.rc_count(boxed), 1);
    let stats = heap.stats();
    assert_eq!(stats.arena.used_bytes, 0);
    assert_eq!(stats.pooled_blocks, 0);
    dec_store::<Tag>(&heap, boxed);
}

#[cfg(feature = "diag")]
mod diag {
    use std::panic::{AssertUnwindSafe, catch_unwind};

    use super::*;

    #[test]
    fn rc_underflow_through_pooled_block_flags() {
        let heap = Heap::new();
        let h = alloc_logged(&heap, 1);
        dec_store::<Tag>(&heap, h);
        assert_eq!(heap.stats().pooled_blocks, 1);
        // A second dec through the pooled block trips the rc tombstone.
        assert!(
            catch_unwind(AssertUnwindSafe(|| {
                let _ = heap.rc_dec_count(h);
            }))
            .is_err()
        );
    }
}
