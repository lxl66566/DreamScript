//! The fixture native library the bind e2e compiles with the host's own
//! rustc (`--crate-type cdylib`): a real C-ABI shared object whose
//! exported symbols the loading layer binds and the export reader
//! enumerates, no C toolchain involved.

use std::sync::atomic::{AtomicUsize, Ordering};

#[unsafe(no_mangle)]
pub extern "C" fn ds_add(left: i64, right: i64) -> i64 {
    left.wrapping_add(right)
}

#[unsafe(no_mangle)]
pub extern "C" fn ds_scale(value: f64, by: f64) -> f64 {
    value * by
}

#[unsafe(no_mangle)]
pub extern "C" fn ds_answer() -> i64 {
    42
}

#[unsafe(no_mangle)]
pub extern "C" fn ds_not(flag: u8) -> u8 {
    u8::from(flag == 0)
}

#[unsafe(no_mangle)]
pub extern "C" fn ds_gated(flag: u8, scale: f64) -> f64 {
    if flag != 0 {
        scale
    } else {
        0.0
    }
}

/// The borrowed-buffer legs' observable state: the unit-returning calls
/// write it, the word-returning calls read it back.
static RECORDED: AtomicUsize = AtomicUsize::new(0);

/// Sums one borrowed buffer's bytes — the marshaling pair's pointer is
/// dereferenced, so a mis-staged word answers garbage, never a pass.
#[unsafe(no_mangle)]
pub extern "C" fn ds_tally(data: *const u8, len: usize) -> i64 {
    // SAFETY: the marshaling caller stages the borrow for the call's duration.
    unsafe { read_buffer(data, len) }
        .iter()
        .map(|&byte| i64::from(byte))
        .sum()
}

/// Sums one borrowed buffer's bytes weighed by a float word — the pair
/// followed by an FP word, the marshaling face's mixed placement.
#[unsafe(no_mangle)]
pub extern "C" fn ds_weighed(data: *const u8, len: usize, scale: f64) -> f64 {
    // SAFETY: the marshaling caller stages the borrow for the call's duration.
    let tally: f64 = unsafe { read_buffer(data, len) }
        .iter()
        .map(|&byte| f64::from(byte))
        .sum();
    tally * scale
}

/// Sums two borrowed buffers — two pairs, the four-GP-word envelope.
#[unsafe(no_mangle)]
pub extern "C" fn ds_total(
    first: *const u8,
    first_len: usize,
    second: *const u8,
    second_len: usize,
) -> i64 {
    ds_tally(first, first_len).wrapping_add(ds_tally(second, second_len))
}

/// Records one borrowed buffer's length and answers nothing — the unit
/// return over a pair.
#[unsafe(no_mangle)]
pub extern "C" fn ds_record(data: *const u8, len: usize) {
    let _ = data;
    RECORDED.store(len, Ordering::SeqCst);
}

/// Answers the last recorded length — the unit call's effect read back
/// through the scalar face.
#[unsafe(no_mangle)]
pub extern "C" fn ds_recorded() -> i64 {
    RECORDED.load(Ordering::SeqCst) as i64
}

/// Clears the recorded length and answers nothing — the unit return
/// over no words at all.
#[unsafe(no_mangle)]
pub extern "C" fn ds_clear() {
    RECORDED.store(0, Ordering::SeqCst);
}

/// The borrowed bytes as a slice, an empty one for a null pointer (the
/// fixtures never stage one; the guard keeps the probe honest).
///
/// # SAFETY
///
/// The marshaling caller stages the borrow for the call's duration, so
/// the pointer and length describe a live buffer.
unsafe fn read_buffer<'a>(data: *const u8, len: usize) -> &'a [u8] {
    if data.is_null() {
        return &[];
    }
    unsafe { std::slice::from_raw_parts(data, len) }
}
