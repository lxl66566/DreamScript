//! Integration tests for the arena half of ds-rt: alloc/restore sequences,
//! chunk-append stability, per-fiber interleaving, idempotent restores,
//! oversized/over-aligned requests, panic-path guards, and bounded memory under
//! repeated deep recursion.
//!
//! Grants are raw pointers, so every access goes through the helpers below;
//! each test upholds the aliasing contract by taking addresses before restores
//! instead of holding references across them.

use std::{
    alloc::Layout,
    panic::{AssertUnwindSafe, catch_unwind},
    ptr::NonNull,
};

use ds_rt::{Arena, FIRST_CHUNK, MAX_CHUNK, MIN_ALIGN, RestoreOnDrop};

/// Reads one live grant.
///
/// Contract: the caller upholds the arena's aliasing rules - no restore has
/// freed the grant's range, and no later grant covers it.
fn read<T: Copy>(at: NonNull<T>) -> T {
    // SAFETY: live grant per the helper's contract.
    unsafe { *at.as_ptr() }
}

/// Writes one live grant. Same contract as [`read`].
fn write<T: Copy>(at: NonNull<T>, value: T) {
    // SAFETY: live grant per the helper's contract.
    unsafe { *at.as_ptr() = value }
}

/// Reads element `i` of a live slice grant. Same contract as [`read`].
fn read_at<T: Copy>(at: NonNull<T>, i: usize) -> T {
    // SAFETY: live grant, `i` within the length the allocation site passed.
    unsafe { *at.as_ptr().add(i) }
}

/// Writes element `i` of a live slice grant. Same contract as [`read_at`].
fn write_at<T: Copy>(at: NonNull<T>, i: usize, value: T) {
    // SAFETY: live grant, `i` within the length the allocation site passed.
    unsafe { *at.as_ptr().add(i) = value }
}

#[test]
fn alloc_then_restore_reuses_address() {
    let arena = Arena::new();
    let mark = arena.mark();
    let first = arena.alloc(7u64);
    assert_eq!(read(first), 7);
    write(first, 9);
    assert_eq!(arena.stats().used_bytes, MIN_ALIGN);
    arena.restore(mark);
    assert_eq!(arena.stats().used_bytes, 0);
    let second = arena.alloc(11u64);
    assert_eq!(first.addr(), second.addr());
    assert_eq!(read(second), 11);
}

#[test]
fn empty_arena_mark_is_restorable_before_any_alloc() {
    let arena = Arena::new();
    let mark = arena.mark(); // chunks list is empty here
    arena.restore(mark);
    assert_eq!(arena.stats().chunks, 0);
    assert_eq!(arena.stats().used_bytes, 0);
    let x = arena.alloc(1u64);
    arena.restore(mark);
    let y = arena.alloc(2u64);
    assert_eq!(x.addr(), y.addr());
}

#[test]
fn restore_to_same_mark_is_idempotent() {
    let arena = Arena::new();
    let mark = arena.mark();
    let a = arena.alloc(1u64);
    arena.restore(mark);
    arena.restore(mark);
    arena.restore(mark);
    let b = arena.alloc(2u64);
    assert_eq!(a.addr(), b.addr());
    assert_eq!(arena.stats().used_bytes, MIN_ALIGN);
}

#[test]
fn nested_marks_restore_lifo() {
    let arena = Arena::new();
    let outer = arena.mark();
    let keep = arena.alloc(0x1111u64);
    let inner = arena.mark();
    arena.alloc(0x2222u64);
    arena.restore(inner);
    assert_eq!(read(keep), 0x1111);
    assert_eq!(arena.stats().used_bytes, MIN_ALIGN);
    arena.restore(outer);
    assert_eq!(arena.stats().used_bytes, 0);
}

#[test]
fn chunk_append_keeps_prior_addresses_stable() {
    let arena = Arena::new();
    let early = arena.alloc(7u64);
    let after_early = arena.mark();
    // Force growth: two 12 KiB fills exceed the 16 KiB first chunk.
    let fill_len = 12 * 1024 / 8;
    for i in 0..2u64 {
        let chunk_fill = arena.alloc_slice_fill::<u64>(fill_len, i);
        write_at(chunk_fill, 0, i);
        write_at(chunk_fill, fill_len - 1, i);
    }
    assert!(arena.stats().chunks >= 2);
    // Addresses from before the growth stay valid and writable.
    write(early, 42);
    assert_eq!(read(early), 42);
    // Restore drops the grown chunk; the early allocation is untouched.
    arena.restore(after_early);
    assert_eq!(arena.stats().chunks, 1);
    assert_eq!(read(early), 42);
}

#[test]
fn growth_reuses_pooled_chunk_at_same_address() {
    let arena = Arena::new();
    let base = arena.mark();
    arena.alloc_slice_fill::<u8>(FIRST_CHUNK, 0); // fills chunk 0 exactly
    let grow_at = arena.mark();
    let in_second_chunk = arena.alloc(1234u64); // forces the 32 KiB chunk
    let addr_before = in_second_chunk.addr();
    arena.restore(grow_at);
    assert_eq!(arena.stats().pooled_chunks, 1);
    arena.restore(base);
    arena.alloc_slice_fill::<u8>(FIRST_CHUNK, 0);
    let again = arena.alloc(5678u64);
    assert_eq!(
        again.addr(),
        addr_before,
        "grown chunk must be reused from the pool"
    );
}

#[test]
fn oversized_allocation_gets_dedicated_chunk_and_is_returned_on_restore() {
    let arena = Arena::new();
    let keep = arena.alloc(1u64);
    let mark = arena.mark();
    let big_len = 2 * MAX_CHUNK;
    let big = arena.alloc_slice_fill::<u8>(big_len, 0xab);
    write_at(big, 0, 0x01);
    write_at(big, big_len - 1, 0x02);
    assert_eq!(read_at(big, 0), 0x01);
    assert_eq!(read_at(big, big_len - 1), 0x02);
    assert!(arena.stats().chunks >= 2);
    let with_dedicated = arena.stats().capacity_bytes;
    arena.restore(mark);
    // Dedicated chunks are released to the allocator, not pooled.
    assert_eq!(arena.stats().chunks, 1);
    assert_eq!(arena.stats().pooled_chunks, 0);
    assert!(arena.stats().capacity_bytes < with_dedicated);
    assert_eq!(read(keep), 1);
}

#[test]
fn cap_sized_request_is_not_oversized_and_pools_on_restore() {
    let arena = Arena::new();
    arena.alloc(1u64); // chunk 0 exists, so the big request appends chunk 1
    let mark = arena.mark();
    let top = arena.alloc_slice_fill::<u8>(MAX_CHUNK, 9);
    assert_eq!(read_at(top, 0), 9);
    assert_eq!(read_at(top, MAX_CHUNK - 1), 9);
    arena.restore(mark);
    assert_eq!(
        arena.stats().pooled_chunks,
        1,
        "a cap-sized request takes the ladder path and pools, unlike oversized ones"
    );
}

#[test]
fn growth_after_dedicated_anchored_restore_stays_poolable() {
    // A restore anchored on a dedicated chunk leaves `next_size` at a
    // non-ladder hint (dedicated cap * 2); normal growth must snap to the
    // ladder so every non-dedicated chunk remains poolable (bounded memory
    // under later deep recursion).
    let arena = Arena::new();
    arena.alloc_slice_fill::<u8>(FIRST_CHUNK, 0); // fill chunk 0 exactly
    let wide = arena.alloc_layout(Layout::from_size_align(20 * 1024, 32).unwrap());
    assert_eq!(wide.addr().get() % 32, 0);
    let anchor = arena.mark(); // sits on the full dedicated chunk
    arena.alloc_slice_fill::<u8>(FIRST_CHUNK / 2, 0); // small overflow alloc
    assert_eq!(arena.stats().chunks, 3);
    arena.restore(anchor);
    assert_eq!(
        arena.stats().pooled_chunks,
        1,
        "growth chunk pools despite the dedicated anchor"
    );
}

#[test]
fn every_allocation_is_min_aligned() {
    #[derive(Clone, Copy)]
    #[repr(align(16))]
    struct A16([u8; 16]);
    let arena = Arena::new();
    for (size, align) in [
        (1, 1),
        (3, 1),
        (5, 2),
        (7, 4),
        (9, 8),
        (17, 16),
        (100, 8),
        (5000, 1),
    ] {
        let layout = Layout::from_size_align(size, align).unwrap();
        let ptr = arena.alloc_layout(layout);
        assert_eq!(ptr.addr().get() % MIN_ALIGN, 0, "size {size} align {align}");
    }
    let typed = arena.alloc(A16([7; 16]));
    assert_eq!(typed.addr().get() % 16, 0);
    assert_eq!(read(typed).0, [7; 16]);
    let slice = arena.alloc_slice_fill::<u64>(7, 0);
    assert_eq!(slice.addr().get() % 8, 0);
}

#[test]
fn over_aligned_request_gets_aligned_dedicated_chunk() {
    let arena = Arena::new();
    let layout = Layout::from_size_align(64, 32).unwrap();
    let ptr = arena.alloc_layout(layout);
    assert_eq!(ptr.addr().get() % 32, 0);
    let wider = arena.alloc_layout(Layout::from_size_align(128, 128).unwrap());
    assert_eq!(wider.addr().get() % 128, 0);
    // Normal allocations continue after the dedicated chunks.
    let after = arena.alloc(5u64);
    assert_eq!(read(after), 5);
}

#[test]
fn zero_sized_allocations_are_aligned_dangling_pointers() {
    let arena = Arena::new();
    arena.alloc(());
    let empty = arena.alloc_slice_copy::<u8>(&[]);
    assert_eq!(
        empty.addr().get(),
        1,
        "zero-sized grant is the canonical dangling pointer"
    );
    let dangling = arena.alloc_layout(Layout::new::<()>());
    assert_ne!(dangling.addr().get(), 0);
    assert_eq!(arena.stats().used_bytes, 0);
}

#[test]
fn slice_copy_roundtrips() {
    let src = [1u64, 2, 3, 4, 5, 6, 7, 8];
    let arena = Arena::new();
    let copy = arena.alloc_slice_copy(&src);
    for (i, v) in src.iter().enumerate() {
        assert_eq!(read_at(copy, i), *v);
    }
    write_at(copy, 3, 99);
    assert_eq!(read_at(copy, 3), 99);
    assert_eq!(src[3], 4, "source untouched");
}

#[test]
fn restore_on_drop_guard_covers_normal_exit() {
    let arena = Arena::new();
    let outside = arena.alloc(1u64);
    {
        let _frame = RestoreOnDrop::new(&arena);
        arena.alloc(2u64);
        arena.alloc_slice_fill::<u8>(64, 3);
        assert!(arena.stats().used_bytes > MIN_ALIGN);
    }
    assert_eq!(arena.stats().used_bytes, MIN_ALIGN);
    assert_eq!(read(outside), 1);
}

#[test]
fn frame_guard_restores_on_panic_unwinding() {
    let arena = Arena::new();
    let run = || {
        catch_unwind(AssertUnwindSafe(|| {
            arena.scope(|a| {
                a.alloc(42u64);
                a.alloc_slice_fill::<u8>(FIRST_CHUNK * 2, 1); // forces chunk growth
                panic!("frame body failed");
            })
        }))
    };
    assert!(run().is_err());
    let after_first = arena.stats();
    assert_eq!(after_first.used_bytes, 0);
    assert!(
        after_first.pooled_chunks >= 1,
        "grown chunk pooled by the unwinding drop"
    );
    assert!(run().is_err());
    assert_eq!(
        arena.stats(),
        after_first,
        "second unwinding reuses the pool, adds nothing"
    );
    let probe = arena.alloc(0u64);
    assert_eq!(probe.addr().get() % MIN_ALIGN, 0);
}

#[test]
fn nested_frames_restore_in_reverse_on_unwind() {
    let arena = Arena::new();
    let baseline = arena.alloc(1u64); // predates every scope; must survive
    let result = catch_unwind(AssertUnwindSafe(|| {
        arena.scope(|a| {
            a.alloc(2u64);
            a.scope(|b| {
                b.alloc(3u64);
                b.scope(|c| {
                    c.alloc(4u64);
                    panic!("deep failure");
                })
            })
        })
    }));
    assert!(result.is_err());
    assert_eq!(
        arena.stats().used_bytes,
        MIN_ALIGN,
        "every scope's alloc freed by unwinding"
    );
    write(baseline, 5);
    assert_eq!(read(baseline), 5);
}

#[test]
fn guard_pop_restores_immediately() {
    let arena = Arena::new();
    let frame = arena.frame();
    let temp = arena.alloc(8u64);
    assert_eq!(read(temp), 8);
    frame.pop();
    assert_eq!(arena.stats().used_bytes, 0);
    let next = arena.alloc(9u64);
    assert_eq!(next.addr(), temp.addr());
}

#[test]
fn stale_mark_restore_panics() {
    let arena = Arena::new();
    let old = arena.mark();
    arena.alloc(1u64);
    let new = arena.mark();
    arena.alloc(2u64);
    arena.restore(new);
    arena.restore(old);
    let result = catch_unwind(AssertUnwindSafe(|| arena.restore(new)));
    assert!(result.is_err(), "restoring a popped mark must panic");
}

#[test]
// the fiber guards are named `<fiber>_<depth>` on purpose
#[allow(clippy::similar_names)]
fn per_fiber_arenas_interleave_without_cross_freeing() {
    // marks are per-fiber: a fiber owns its arena, so interleaving two
    // fibers can never free the other's live data and needs no arena action at
    // the switch itself.
    let fiber_a = Arena::new();
    let fiber_b = Arena::new();

    let frame_a1 = fiber_a.frame();
    let live_a = fiber_a.alloc(0xaau64);

    let frame_b1 = fiber_b.frame();
    let live_b = fiber_b.alloc(0xbbu64);

    // Switch: allocate back and forth a few times, including growth on A.
    let frame_a2 = fiber_a.frame();
    let grown_a = fiber_a.alloc_slice_fill::<u64>(FIRST_CHUNK / 4, 1); // 16 KiB: forces growth
    let mark_b2 = fiber_b.mark();
    let temp_b = fiber_b.alloc(0xb1u64);

    assert_eq!(read(live_a), 0xaa);
    assert_eq!(read(live_b), 0xbb);
    assert_eq!(read(temp_b), 0xb1);
    assert_eq!(read_at(grown_a, FIRST_CHUNK / 4 - 1), 1);

    // B unwinds to its nested mark, then to its frame; A's data must be intact.
    fiber_b.restore(mark_b2);
    assert_eq!(read(live_a), 0xaa);
    assert_eq!(read_at(grown_a, 0), 1);
    drop(frame_b1);
    assert_eq!(fiber_b.stats().used_bytes, 0);

    // A keeps running after B finished entirely.
    write(live_a, 0xac);
    assert_eq!(read(live_a), 0xac);
    drop(frame_a2);
    assert_eq!(
        fiber_a.stats().pooled_chunks,
        1,
        "A's grown chunk pooled in A's own free list, untouched by B"
    );
    drop(frame_a1);
    assert_eq!(fiber_a.stats().used_bytes, 0);
}

#[test]
fn repeated_deep_recursion_stays_memory_bounded() {
    fn recurse(arena: &Arena, depth: u32, depth_max: u32) -> usize {
        let _frame = arena.frame();
        let fill = u8::try_from(depth % 256).expect("depth byte");
        let locals = arena.alloc_slice_fill::<u8>(1024, fill);
        write_at(locals, 0, 1);
        write_at(locals, 1023, 1);
        let addr = locals.addr().get();
        if depth == depth_max { addr } else { recurse(arena, depth + 1, depth_max) }
    }

    let arena = Arena::new();
    let depth = 2000; // ~2 MiB of live frame locals at the deepest point
    let deepest1 = recurse(&arena, 0, depth);
    let after1 = arena.stats();
    assert_eq!(after1.used_bytes, 0);
    assert!(
        after1.pooled_chunks >= 6,
        "deep ladder chunks must be pooled"
    );

    let deepest2 = recurse(&arena, 0, depth);
    let after2 = arena.stats();
    let deepest3 = recurse(&arena, 0, depth);
    let after3 = arena.stats();

    assert_eq!(
        after2.capacity_bytes, after1.capacity_bytes,
        "second pass adds no capacity"
    );
    assert_eq!(
        after3.capacity_bytes, after1.capacity_bytes,
        "third pass adds no capacity"
    );
    assert_eq!(
        deepest2, deepest1,
        "pooled chunks are reused at the same addresses"
    );
    assert_eq!(deepest3, deepest1);
    assert!(
        after1.capacity_bytes < 8 * 1024 * 1024,
        "peak footprint stays near the live set"
    );
}

#[test]
fn create_grow_drop_repeats_cleanly() {
    for _ in 0..64 {
        let arena = Arena::new();
        let mark = arena.mark();
        arena.alloc_slice_fill::<u8>(3 * MAX_CHUNK, 0); // dedicated chunk
        arena.alloc_slice_fill::<u8>(3 * FIRST_CHUNK, 0); // ladder growth
        arena.restore(mark);
    } // Drop frees both lists every iteration.
}
