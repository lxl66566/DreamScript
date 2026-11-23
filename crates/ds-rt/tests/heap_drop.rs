//! Drop discipline for the heap's reuse pools: every pooled node, not just
//! each row's head, releases when the heap drops. A counting global
//! allocator measures a window's net outstanding allocations, so leaked
//! pool nodes survive the measurement as a positive residue. The counters
//! are process-global, so the window holds the file's lock for its whole
//! body: nextest isolates each test in its own process, but a plain
//! `cargo test` run shares one.

use std::{
    alloc::{GlobalAlloc, Layout, System},
    sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

use ds_rt::{Heap, POOL_DEPTH};

/// Serializes this file's windows: two windows measuring at once read each
/// other's traffic.
static SERIAL: Mutex<()> = Mutex::new(());
static ALLOCS: AtomicUsize = AtomicUsize::new(0);
static FREES: AtomicUsize = AtomicUsize::new(0);

/// A pass-through counting allocator: entries and exits each count one,
/// so matched allocation/free pairs read zero whatever the churn between
/// them.
struct Counting;

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        FREES.fetch_add(1, Ordering::Relaxed);
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

/// The window's net outstanding allocations.
fn net_outstanding(window: impl FnOnce()) -> usize {
    let before = ALLOCS.load(Ordering::Relaxed) - FREES.load(Ordering::Relaxed);
    window();
    (ALLOCS.load(Ordering::Relaxed) - FREES.load(Ordering::Relaxed)) - before
}

/// A pool row deeper than its head must release whole at heap drop:
/// dropping only the head strands the row's deeper nodes.
#[test]
fn pooled_rows_release_whole_at_drop() {
    let _serial = SERIAL.lock().expect("the counting window is serialized");
    let residue = net_outstanding(|| {
        let heap = Heap::new();
        // More live blocks of one class than the pool's depth cap, so
        // their deaths leave the row full and one block released direct.
        let blocks: Vec<_> = (0..=POOL_DEPTH)
            .map(|_| heap.alloc_store(0u64, None))
            .collect();
        for header in blocks {
            assert!(heap.rc_dec_count(header));
            heap.rc_dispose::<u64>(header);
        }
        assert_eq!(
            heap.stats().pooled_blocks,
            usize::from(POOL_DEPTH),
            "the window actually pools a deep row"
        );
    });
    assert_eq!(residue, 0, "every pooled node releases at heap drop");
}
