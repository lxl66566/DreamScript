#![no_main]

// Property: the dsmod reader never panics on arbitrary bytes, and every
// module it accepts re-serializes to the identical bytes (canonical
// encodings only). A nextest-runnable arbitrary-driven mirror of this
// property lives in crates/ds-mod/tests/fuzz_read.rs.
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(module) = ds_mod::read(data) {
        assert_eq!(ds_mod::write(&module), data);
    }
});
