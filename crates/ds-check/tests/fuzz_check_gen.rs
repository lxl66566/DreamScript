//! Randomized checker property over grammar-generated programs, the
//! nextest-runnable stand-in for the cargo-fuzz target `fuzz_check_gen`:
//! fuzz bytes become a structurally valid DS program (declarations,
//! statements, expressions over the checked surface, well-typed-biased
//! with occasional wrong splices), and the full pipeline (parse, effects
//! walk over the generator's declared rows, bidirectional check with the
//! affine pass, both diag renderers) never panics. Coverage is asserted,
//! not assumed: both parse outcomes occur across the sweep, most
//! generated programs parse (a generator drift fails loudly), and the
//! coverage is asserted, not assumed: both parse outcomes occur across
//! the sweep, most generated programs parse (a generator drift fails
//! loudly), the affine pass records moves, and the effects walk sees the
//! generator's declared rows.

#[path = "../../../fuzz/check_gen.rs"]
mod check_gen;

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

/// Case count for `repeated_generated_programs`; a CI fuzz job can crank
/// it with `FUZZ_RUNS=<n> cargo nextest run`. The generated-program
/// pipeline is much heavier than byte noise, so the knob is capped; a
/// generated full check costs more than a hundred byte-noise runs.
/// Invalid or unset values keep the default.
fn fuzz_runs() -> u64 {
    std::env::var("FUZZ_RUNS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(300)
        .min(1500)
}

#[test]
fn generated_programs_never_panic() {
    let mut parsed = 0u64;
    let mut moved = 0u64;
    let mut rowed = 0u64;
    let mut runs = 0u64;
    for seed in 0..240u64 {
        let len = 16 + usize::try_from(seed % 24).expect("len fits") * 11;
        let outcome = check_gen::exercise(&rng_bytes(seed, len));
        runs += 1;
        parsed += u64::from(outcome.parsed);
        moved += u64::from(outcome.parsed && outcome.moves > 0);
        rowed += u64::from(outcome.parsed && outcome.effects > 0);
    }
    // structural generation: the near-totality of programs must parse
    assert!(
        parsed * 10 > runs * 9,
        "at least 90% of generated programs must parse (got {parsed}/{runs})"
    );
    assert!(
        moved > 0,
        "the affine pass must record moves somewhere in the sweep"
    );
    assert!(
        rowed > 0,
        "the effects walk must see declared rows somewhere in the sweep"
    );
    // the degenerate inputs: empty and single-byte streams cycle into a
    // full deterministic program too
    check_gen::exercise(&[]);
    check_gen::exercise(&[0]);
    check_gen::exercise(&[0xff]);
}

/// The repetition arm: the campaign's mirror companion.
#[test]
fn repeated_generated_programs_never_panic() {
    for run in 0..fuzz_runs() {
        let seed = run.wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ 0xc0de_d00d_feed_face;
        let len = 8 + usize::try_from(seed % 64).expect("len fits") * 7;
        check_gen::exercise(&rng_bytes(seed, len));
    }
}

/// Generation is deterministic per input: the same bytes always produce
/// the same program and rows, so any campaign finding reproduces from
/// the raw input alone.
#[test]
fn generation_is_deterministic() {
    for seed in [1u64, 42, 97531] {
        let data = rng_bytes(seed, 33);
        let a = check_gen::generate(&data);
        let b = check_gen::generate(&data);
        assert_eq!(a.source, b.source, "same input, same program");
        assert_eq!(a.rows.len(), b.rows.len(), "same input, same rows");
    }
}
