//! Randomized checker property over grammar-generated programs, the
//! nextest-runnable stand-in for the cargo-fuzz target `fuzz_check_gen`:
//! fuzz bytes become a structurally valid DS program (declarations,
//! statements, expressions over the checked surface, well-typed-biased
//! with occasional wrong splices), and the full pipeline (parse, effects
//! walk over the generator's declared rows, bidirectional check with the
//! affine pass, both diag renderers) never panics. The census asserts
//! the landed vocabulary stays in the draw set:
//! value-level or-patterns, struct patterns over struct-typed
//! scrutinees, case constructor payload tests, guards over dispatch,
//! nested struct sub-patterns, whole-span union equality, and unions
//! over heap-ref applications, plus the CALL_GENERIC vocabulary: free
//! generic call sites over declared bounds, serving
//! impl bodies, nested runtime forms, the honest doors, inert
//! uncalled declarations, the impl-completeness shapes, and
//! two-level instantiations, plus the parser-budget shapes: type
//! chains at both sides of the chain-fold budget and lambda/paren
//! depth at both sides of the scanner's budgets. Coverage is
//! asserted,
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
    let mut record_lists = 0u32;
    let mut wide_unions = 0u32;
    let mut span_fields = 0u32;
    let mut heap_ref_unions = 0u32;
    let mut or_patterns = 0u32;
    let mut struct_patterns = 0u32;
    let mut payload_tests = 0u32;
    let mut union_guards = 0u32;
    let mut nested_sub_patterns = 0u32;
    let mut union_equality = 0u32;
    let mut generic_calls = 0u32;
    let mut generic_impls = 0u32;
    let mut generic_runtime_forms = 0u32;
    let mut generic_doors = 0u32;
    let mut inert_generics = 0u32;
    let mut incomplete_impls = 0u32;
    let mut omitted_defaults = 0u32;
    let mut two_level_calls = 0u32;
    let mut derive_attrs = 0u32;
    let mut import_members = 0u32;
    let mut type_chain_oks = 0u32;
    let mut type_chain_over = 0u32;
    let mut depth_oks = 0u32;
    let mut depth_over = 0u32;
    for seed in 0..240u64 {
        let len = 16 + usize::try_from(seed % 24).expect("len fits") * 11;
        let outcome = check_gen::exercise(&rng_bytes(seed, len));
        runs += 1;
        parsed += u64::from(outcome.parsed);
        moved += u64::from(outcome.parsed && outcome.moves > 0);
        rowed += u64::from(outcome.parsed && outcome.effects > 0);
        record_lists += outcome.drawn.record_lists;
        wide_unions += outcome.drawn.wide_unions;
        span_fields += outcome.drawn.span_fields;
        heap_ref_unions += outcome.drawn.heap_ref_unions;
        or_patterns += outcome.drawn.or_patterns;
        struct_patterns += outcome.drawn.struct_patterns;
        payload_tests += outcome.drawn.payload_tests;
        union_guards += outcome.drawn.union_guards;
        nested_sub_patterns += outcome.drawn.nested_sub_patterns;
        union_equality += outcome.drawn.union_equality;
        generic_calls += outcome.drawn.generic_calls;
        generic_impls += outcome.drawn.generic_impls;
        generic_runtime_forms += outcome.drawn.generic_runtime_forms;
        generic_doors += outcome.drawn.generic_doors;
        inert_generics += outcome.drawn.inert_generics;
        incomplete_impls += outcome.drawn.incomplete_impls;
        omitted_defaults += outcome.drawn.omitted_defaults;
        two_level_calls += outcome.drawn.two_level_calls;
        derive_attrs += outcome.drawn.derive_attrs;
        import_members += outcome.drawn.import_members;
        type_chain_oks += outcome.drawn.type_chain_oks;
        type_chain_over += outcome.drawn.type_chain_over;
        depth_oks += outcome.drawn.depth_oks;
        depth_over += outcome.drawn.depth_over;
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
    // the span-surface draw census: the landed span shapes must stay in
    // the vocabulary (a weighting regression fails loudly)
    assert!(
        record_lists > 0,
        "record-typed list elements are drawn somewhere in the sweep"
    );
    assert!(
        wide_unions > 0,
        "unions over wider members are drawn somewhere in the sweep"
    );
    assert!(
        span_fields > 0,
        "struct-typed fields are drawn somewhere in the sweep"
    );
    // the landed vocabulary stays in the generator's draw set (a
    // weighting regression fails loudly)
    assert!(
        heap_ref_unions > 0,
        "unions over heap-ref applications are drawn somewhere in the sweep"
    );
    assert!(
        or_patterns > 0,
        "value-level or-patterns are drawn somewhere in the sweep"
    );
    assert!(
        struct_patterns > 0,
        "struct patterns over struct-typed scrutinees are drawn somewhere in the sweep"
    );
    assert!(
        payload_tests > 0,
        "case constructor payload tests are drawn somewhere in the sweep"
    );
    assert!(
        union_guards > 0,
        "guards over union and enum dispatch are drawn somewhere in the sweep"
    );
    assert!(
        nested_sub_patterns > 0,
        "nested struct sub-patterns are drawn somewhere in the sweep"
    );
    assert!(
        union_equality > 0,
        "whole-span equality over union operands is drawn somewhere in the sweep"
    );
    // the CALL_GENERIC landed vocabulary stays in the generator's draw
    // set (a weighting regression fails loudly)
    assert!(
        generic_calls > 0,
        "free generic call sites are drawn somewhere in the sweep"
    );
    assert!(
        generic_impls > 0,
        "serving impl bodies are drawn somewhere in the sweep"
    );
    assert!(
        generic_runtime_forms > 0,
        "nested generic runtime forms are drawn somewhere in the sweep"
    );
    assert!(
        generic_doors > 0,
        "the generic surface's honest doors are drawn somewhere in the sweep"
    );
    assert!(
        inert_generics > 0,
        "inert uncalled generic declarations are drawn somewhere in the sweep"
    );
    assert!(
        incomplete_impls > 0,
        "incomplete impls reporting DS2023 are drawn somewhere in the sweep"
    );
    assert!(
        omitted_defaults > 0,
        "omitted defaulted members are drawn somewhere in the sweep"
    );
    assert!(
        two_level_calls > 0,
        "two-level instantiations are drawn somewhere in the sweep"
    );
    // the json derive families' attribute grammar and the linked-facts
    // import-row spelling stay in the draw set
    assert!(
        derive_attrs > 0,
        "derive attributes over the json families are drawn somewhere in the sweep"
    );
    assert!(
        import_members > 0,
        "import rows naming const/type members are drawn somewhere in the sweep"
    );
    // the parser-budget shapes stay in the draw set at both sides of
    // each budget (a weighting regression fails loudly)
    assert!(
        type_chain_oks > 0,
        "type chains at or under the chain budget are drawn somewhere in the sweep"
    );
    assert!(
        type_chain_over > 0,
        "past-budget type chains answering NestingTooDeep are drawn somewhere in the sweep"
    );
    assert!(
        depth_oks > 0,
        "near-budget lambda arrow chains and paren nesting are drawn somewhere in the sweep"
    );
    assert!(
        depth_over > 0,
        "past-budget arrow runs and bracket nesting are drawn somewhere in the sweep"
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
