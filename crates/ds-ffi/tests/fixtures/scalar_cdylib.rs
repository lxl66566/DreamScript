//! The fixture native library the bind e2e compiles with the host's own
//! rustc (`--crate-type cdylib`): a real C-ABI shared object whose
//! exported symbols the loading layer binds and the export reader
//! enumerates, no C toolchain involved.

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
