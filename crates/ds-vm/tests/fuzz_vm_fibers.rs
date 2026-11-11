//! Randomized fiber/nursery property, the nextest-runnable stand-in for the
//! cargo-fuzz target `fuzz_vm`'s fiber leg: structurally generated modules
//! (the shared generator in fuzz/vm_gen.rs, included by path so target and
//! mirrors can never drift) run through the writer/reader round trip, the
//! loader, a budgeted execution, and the host fiber face (spawn with
//! staged Ref/string words, resume with payloads, join or teardown), and
//! every stop is structured — never a panic.

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

/// The sweep: the same seed set as the base mirror (fuzz_vm.rs), asserted
/// on coverage — every fiber/nursery shape the generator draws must occur
/// across the sweep, and the child-death paths (completed with a Ref word
/// delivered to the joiner, cancelled at a backward branch, trapped) must
/// all have occurred.
#[test]
fn fiber_shapes_stop_structurally_and_cover() {
    let mut total = vm_gen::Fibers::default();
    for (seed, len) in (0u64..240).map(|s| (s, 16 + (s % 24) as usize * 11)) {
        let (outcome, shape) = vm_gen::exercise_fibers(&rng_bytes(seed, len));
        assert!(matches!(outcome, 0 | 1), "a structured stop only");
        total.spawned += shape.spawned;
        total.resumed += shape.resumed;
        total.resumed_ref += shape.resumed_ref;
        total.completed += shape.completed;
        total.completed_ref += shape.completed_ref;
        total.cancelled += shape.cancelled;
        total.trapped += shape.trapped;
        total.joined += shape.joined;
        total.torn_down += shape.torn_down;
        total.run_cancelled += shape.run_cancelled;
        total.run_uncontained += shape.run_uncontained;
    }
    vm_gen::exercise_fibers(&[]);
    vm_gen::exercise_fibers(&[0]);
    vm_gen::exercise_fibers(&[0xff]);
    assert!(total.spawned > 0, "children spawn across the sweep");
    assert!(total.resumed > 0, "the host resume face switches");
    assert!(total.completed > 0, "children complete at boundaries");
    assert!(
        total.completed_ref > 0,
        "a completion delivers a Ref word to the joiner"
    );
    assert!(
        total.cancelled > 0,
        "the cancel flag cuts a looping child at a backward branch"
    );
    assert!(total.trapped > 0, "children die trapped");
    assert!(
        total.joined > 0 && total.torn_down > 0,
        "both boundary classes run"
    );
    assert!(
        total.run_cancelled > 0,
        "an entry dies cancelled in-language"
    );
}

/// Case count for `repeated_random_inputs`, the same knob as the base
/// mirror: a CI fuzz job can crank it with `FUZZ_RUNS=<n> cargo nextest
/// run`. Invalid or unset values keep the default.
fn fuzz_runs() -> u64 {
    std::env::var("FUZZ_RUNS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(2000)
}

/// The repetition arm: the same property and coverage record over a
/// wider seed sweep, without per-seed assertions (the fixed sweep above
/// owns those).
#[test]
fn repeated_random_inputs_cover_fiber_shapes() {
    for run in 0..fuzz_runs() {
        let seed = run.wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ 0xc0de_fe01_badc_0ffe;
        let len = 8 + (seed % 64) as usize * 7;
        let (_outcome, shape) = vm_gen::exercise_fibers(&rng_bytes(seed, len));
        std::hint::black_box(&shape);
    }
}
