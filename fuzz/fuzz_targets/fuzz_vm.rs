#![no_main]

// Property: the VM execution pipeline never panics on fuzz bytes turned
// into structurally valid-ish dsmod modules (the content-hash gate makes
// mutated bytes useless, so the module is generated, not mutated): a
// deterministic uop-program builder assembles through ds-mod's public
// encode path, the bytes round-trip through the real reader, the loader
// installs under the natives-first contract, a budgeted run (a tight
// `Vm::with_limit`) stops only in structured ways — a halt value, a trap,
// or the stack-limit stop — and the fiber/nursery host face (spawn,
// resume, join, teardown) stays structured with execution sealed (no real
// net/file effects; only the captured-output console pair is reachable).
// The generator, its termination invariants (bounded back edges, the
// depth-one spawn tree, park-at-pc+1 switch progress — the stand-in for
// the VM's missing step budget), and the emit path's target-retargeting
// re-audit note live in the shared `vm_gen` module; nextest-runnable
// mirrors live in crates/ds-vm/tests/fuzz_vm.rs (the base sweep) and
// crates/ds-vm/tests/fuzz_vm_fibers.rs (the fiber-coverage sweep).
#[path = "../vm_gen.rs"]
mod vm_gen;

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    vm_gen::exercise(data);
});
