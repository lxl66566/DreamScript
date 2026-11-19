//! The fixture native library the bind e2e compiles with the host's own
//! rustc (`--crate-type cdylib`): a real C-ABI shared object whose
//! exported symbols the loading layer binds and the export reader
//! enumerates, no C toolchain involved.

#[no_mangle]
pub extern "C" fn ds_add(left: i64, right: i64) -> i64 {
    left.wrapping_add(right)
}

#[no_mangle]
pub extern "C" fn ds_scale(value: f64, by: f64) -> f64 {
    value * by
}

#[no_mangle]
pub extern "C" fn ds_answer() -> i64 {
    42
}
