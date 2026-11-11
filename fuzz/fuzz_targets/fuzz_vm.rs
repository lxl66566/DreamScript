#![no_main]

// Property: the VM execution pipeline never panics on fuzz bytes turned
// into structurally valid-ish dsmod modules (the content-hash gate makes
// mutated bytes useless, so the module is generated, not mutated): a
// deterministic uop-program builder assembles through ds-mod's public
// encode path, the bytes round-trip through the real reader, the loader
// installs under the natives-first contract, and a budgeted run (a tight
// `Vm::with_limit`) stops only in structured ways — a halt value, a trap,
// or the stack-limit stop. The generator and assertions live in the
// shared `vm_gen` module; a nextest-runnable mirror lives in
// crates/ds-vm/tests/fuzz_vm.rs.
#[path = "../vm_gen.rs"]
mod vm_gen;

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    vm_gen::exercise(data);
});
