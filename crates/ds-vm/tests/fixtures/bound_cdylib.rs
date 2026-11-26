//! The bound-dispatch fixtures' C-ABI surface: a real cdylib the host's
//! rustc builds at test time, its exports covering every register-class
//! shape the scalar slice rules — the i64 GP pair, the f64 FP pair, the
//! u8 GP word, and the mixed GP+FP sequence — plus the fast-path word
//! boundary: four words riding the enumerated transmute's top
//! placements, five riding libffi's CIF path.

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

/// Four GP words exactly: the fast-path envelope's own width, the
/// enumerated transmute's widest all-GP arm.
#[unsafe(no_mangle)]
pub extern "C" fn ds_sum4(a: i64, b: i64, c: i64, d: i64) -> i64 {
    a + b + c + d
}

/// Five GP words: one past the envelope, the CIF path's register
/// placement serving the call.
#[unsafe(no_mangle)]
pub extern "C" fn ds_sum5(a: i64, b: i64, c: i64, d: i64, e: i64) -> i64 {
    a + b + c + d + e
}

/// Four words at the envelope's mixed placement: the bool words ride
/// the GP file and the floats the FP file in one call (the mask
/// 0b1010 arm).
#[unsafe(no_mangle)]
pub extern "C" fn ds_pick4(choose: u8, left: f64, _spare: u8, right: f64) -> f64 {
    if choose != 0 {
        left
    } else {
        right
    }
}
