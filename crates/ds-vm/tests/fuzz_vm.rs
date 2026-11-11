//! Randomized VM-execution property, the nextest-runnable stand-in for the
//! cargo-fuzz target `fuzz_vm`: structurally generated modules (the shared
//! generator in fuzz/vm_gen.rs, included by path so target and mirror can
//! never drift) run through the writer/reader round trip, the loader, and
//! a budgeted execution, and every stop is structured — never a panic.

#[path = "../../../fuzz/vm_gen.rs"]
mod vm_gen;

/// xorshift64* PRNG: deterministic, no dependency, good enough for byte noise.
struct Rng(u64);

impl Rng {
    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn byte(&mut self) -> u8 {
        u8::try_from(self.next_u64() >> 56).expect("top 8 bits fit u8")
    }
}

fn rng_bytes(seed: u64, len: usize) -> Vec<u8> {
    let mut rng = Rng(seed | 1);
    (0..len).map(|_| rng.byte()).collect()
}

/// The never-panic property plus the structured-stop assertions, over
/// random byte buffers of varied shape (including empty and one-byte
/// inputs, where the cycled stream degenerates to all zeros).
#[test]
fn random_inputs_stop_structurally() {
    let mut halted = 0u32;
    let mut trapped = 0u32;
    for (seed, len) in (0u64..240).map(|s| (s, 16 + (s % 24) as usize * 11)) {
        match vm_gen::exercise(&rng_bytes(seed, len)) {
            1 => halted += 1,
            _ => trapped += 1,
        }
    }
    vm_gen::exercise(&[]);
    vm_gen::exercise(&[0]);
    vm_gen::exercise(&[0xff]);
    // both structured stop classes must have occurred across the sweep:
    // zero bytes halt trivially, and garbage operands trap readily
    assert!(
        halted > 0 && trapped > 0,
        "both stop classes must be reached"
    );
}

/// Case count for `repeated_random_inputs`; a CI fuzz job can crank it
/// with `FUZZ_RUNS=<n> cargo nextest run`. Invalid or unset values keep
/// the default.
fn fuzz_runs() -> u64 {
    std::env::var("FUZZ_RUNS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(2000)
}

/// The repetition arm: the campaign's mirror companion.
#[test]
fn repeated_random_inputs_stop_structurally() {
    for run in 0..fuzz_runs() {
        let seed = run.wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ 0xa5a5_a5a5_a5a5_a5a5;
        let len = 8 + (seed % 64) as usize * 7;
        vm_gen::exercise(&rng_bytes(seed, len));
    }
}
