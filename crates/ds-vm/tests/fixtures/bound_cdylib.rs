//! The bound-dispatch fixtures' C-ABI surface: a real cdylib the host's
//! rustc builds at test time, its exports covering every register-class
//! shape the scalar slice rules — the i64 GP pair, the f64 FP pair, the
//! u8 GP word, and the mixed GP+FP sequence.

#[unsafe(no_mangle)]
pub extern "C" fn ds_add(left: i64, right: i64) -> i64 {
    left.wrapping_add(right)
}

#[unsafe(no_mangle)]
pub extern "C" fn ds_scale(value: f64, by: f64) -> f64 {
    value * by
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

/// Answers nothing at all — the unit return's own shape, the absent
/// word.
#[unsafe(no_mangle)]
pub extern "C" fn ds_silent() {}
