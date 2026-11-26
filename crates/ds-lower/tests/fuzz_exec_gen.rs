//! Randomized end-to-end execution property over source-generated
//! programs, the nextest-runnable stand-in for the cargo-fuzz target
//! `fuzz_vm`'s source leg (the shared generator in fuzz/exec_gen.rs,
//! included by path so target and mirror can never drift): fuzz bytes
//! become one well-typed DS program over the landed match and fork-join
//! vocabulary, and the whole chain — parse, check, lower, module round
//! trip, loader install, VM run — never panics, the captured answers
//! equal the generator's oracle, and the heap drains at run end. The
//! census asserts the vocabulary stays in the draw set: or-patterns,
//! struct patterns, case payload tests, guards over union dispatch,
//! nested struct sub-patterns, whole-span union equality, par_map and
//! par_reduce sites, closure callables, multi-word answers, value
//! blocks whose tail reads a local binder, and
//! the free generic call vocabulary (call sites into the ONE
//! bodies, serving impl bodies, the nested runtime form, tail
//! transfers, two-level instantiations, and inert uncalled
//! declarations) — the json derive roundtrip legs (an optional list
//! field encoded through to_json and decoded back through from_json,
//! presence and absence), the cross-package linked-enum legs (a
//! two-module compile: the consumer constructs, matches, narrows, and
//! scores over the linked enum while union-typed answers cross the
//! boundary both directions, plus the drawn variant whose second
//! linked surface claims a seeded tag id and refuses at lowering),
//! the `pub import` chain legs (a three-module compile: the consumer
//! binds a re-exported callable, const, and struct through the middle
//! package's surface with the calls addressing the origin's module,
//! beside the three drawn refusal variants — the privately-imported
//! name refusing the consumer's link walk, the selectionless pub row
//! refusing the middle's own compile, and the `pub import` of `@std`
//! refusing the merged-ambient seam — and the @std declaration-graph
//! legs: a second consumer compiling over a core-shaped ambient with
//! the reserved package's linked surface answering through the `std`
//! identity — the extern face beside the migrated members' bindable
//! shapes (the map alias annotating a keyed map value, the optional-field
//! record constructing and reading through the presence-tag word, the
//! list const embedding its elements over the list seam) and the drawn
//! refusal variant whose `response` value use still refuses at
//! lowering) — and the parser-budget episode: long type
//! chains (a `?` postfix run, an `|`/`&` chain) in a helper's
//! parameter, return, and binding annotations beside near-budget typed
//! lambda chains and paren nesting (checking clean, erasing at
//! lowering, a constant answer), and the two drawn refusal variants (a
//! past-budget chain refusing the parse with the chain fold's
//! NestingTooDeep diagnostic, a past-budget arrow run refusing the
//! pre-scan's live-arrow diagnostic), each input never running.

#[path = "../../../fuzz/exec_gen.rs"]
mod exec_gen;

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
/// it with `FUZZ_RUNS=<n> cargo nextest run`. One run compiles and
/// executes a whole generated program, so the knob is capped like the
/// checker leg's. Invalid or unset values keep the default.
fn fuzz_runs() -> u64 {
    std::env::var("FUZZ_RUNS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(300)
        .min(1500)
}

/// The sweep: every seed's program answers its oracle and drains, and
/// the vocabulary census stays nonzero across the sweep (a weighting
/// regression fails loudly).
#[test]
fn generated_programs_answer_their_oracle() {
    let mut total = exec_gen::Drawn::default();
    for seed in 0..240u64 {
        let len = 16 + usize::try_from(seed % 24).expect("len fits") * 11;
        let drawn = exec_gen::exercise(&rng_bytes(seed, len));
        total.or_patterns += drawn.or_patterns;
        total.struct_patterns += drawn.struct_patterns;
        total.payload_tests += drawn.payload_tests;
        total.union_guards += drawn.union_guards;
        total.nested_sub_patterns += drawn.nested_sub_patterns;
        total.union_equality += drawn.union_equality;
        total.bytes_equality += drawn.bytes_equality;
        total.par_map += drawn.par_map;
        total.par_reduce += drawn.par_reduce;
        total.closures += drawn.closures;
        total.multiword += drawn.multiword;
        total.generic_calls += drawn.generic_calls;
        total.generic_impls += drawn.generic_impls;
        total.generic_runtime_forms += drawn.generic_runtime_forms;
        total.generic_tails += drawn.generic_tails;
        total.two_level_calls += drawn.two_level_calls;
        total.inert_generics += drawn.inert_generics;
        total.block_tails += drawn.block_tails;
        total.json_derive += drawn.json_derive;
        total.linked_enums += drawn.linked_enums;
        total.linked_refusals += drawn.linked_refusals;
        total.pub_imports += drawn.pub_imports;
        total.pub_private_refusals += drawn.pub_private_refusals;
        total.pub_selectionless_refusals += drawn.pub_selectionless_refusals;
        total.pub_std_refusals += drawn.pub_std_refusals;
        total.std_graphs += drawn.std_graphs;
        total.std_rows += drawn.std_rows;
        total.std_configs += drawn.std_configs;
        total.std_url_consts += drawn.std_url_consts;
        total.std_opaque_refusals += drawn.std_opaque_refusals;
        total.type_chain_oks += drawn.type_chain_oks;
        total.type_chain_over += drawn.type_chain_over;
        total.depth_oks += drawn.depth_oks;
        total.depth_over += drawn.depth_over;
    }
    assert!(total.or_patterns > 0, "value-level or-patterns are drawn");
    assert!(total.struct_patterns > 0, "struct patterns are drawn");
    assert!(total.payload_tests > 0, "case payload tests are drawn");
    assert!(
        total.union_guards > 0,
        "guards over union dispatch are drawn"
    );
    assert!(
        total.nested_sub_patterns > 0,
        "nested struct sub-patterns are drawn"
    );
    assert!(
        total.union_equality > 0,
        "whole-span union equality is drawn"
    );
    assert!(
        total.bytes_equality > 0,
        "bytes-typed equality chains are drawn"
    );
    assert!(total.par_map > 0, "par_map sites are drawn");
    assert!(total.par_reduce > 0, "par_reduce folds are drawn");
    assert!(total.closures > 0, "closure callables are drawn");
    assert!(total.multiword > 0, "multi-word answers are drawn");
    assert!(total.generic_calls > 0, "free generic call sites are drawn");
    assert!(total.generic_impls > 0, "serving impl bodies are drawn");
    assert!(
        total.generic_runtime_forms > 0,
        "nested generic runtime forms are drawn"
    );
    assert!(
        total.generic_tails > 0,
        "tail-position generic calls are drawn"
    );
    assert!(
        total.two_level_calls > 0,
        "two-level instantiations are drawn"
    );
    assert!(
        total.inert_generics > 0,
        "inert uncalled declarations are drawn"
    );
    assert!(
        total.block_tails > 0,
        "value-block tail reads of local binders are drawn"
    );
    assert!(
        total.json_derive > 0,
        "json derive roundtrip legs are drawn"
    );
    assert!(total.linked_enums > 0, "linked-enum episodes are drawn");
    assert!(
        total.linked_refusals > 0,
        "the linked surface's conflicting-tag-row refusal is drawn"
    );
    assert!(total.pub_imports > 0, "pub import chains are drawn");
    assert!(
        total.pub_private_refusals > 0,
        "the chain's privately-imported-name refusal is drawn"
    );
    assert!(
        total.pub_selectionless_refusals > 0,
        "the chain's selectionless-pub-row refusal is drawn"
    );
    assert!(
        total.pub_std_refusals > 0,
        "the chain's merged-ambient @std refusal is drawn"
    );
    assert!(total.std_graphs > 0, "@std declaration graphs are drawn");
    assert!(
        total.std_rows > 0,
        "the `row` alias's keyed map value is drawn"
    );
    assert!(
        total.std_configs > 0,
        "the `config` record's construction and presence read are drawn"
    );
    assert!(
        total.std_url_consts > 0,
        "the `urls` list const's element embed is drawn"
    );
    assert!(
        total.std_opaque_refusals > 0,
        "the `response` value use's M3 contract refusal is drawn"
    );
    assert!(
        total.type_chain_oks > 0,
        "type chains at or under the chain budget are drawn"
    );
    assert!(
        total.type_chain_over > 0,
        "the episode's past-budget chain refusal is drawn"
    );
    assert!(total.depth_oks > 0, "near-budget lambda chains are drawn");
    assert!(
        total.depth_over > 0,
        "the episode's past-budget arrow refusal is drawn"
    );
    // the degenerate inputs: empty and single-byte streams cycle into a
    // full deterministic program too
    exec_gen::exercise(&[]);
    exec_gen::exercise(&[0]);
    exec_gen::exercise(&[0xff]);
}

/// The repetition arm: the campaign's mirror companion.
#[test]
fn repeated_generated_programs_answer_their_oracle() {
    for run in 0..fuzz_runs() {
        let seed = run.wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ 0x5eed_feed_c0de_d00d;
        let len = 8 + usize::try_from(seed % 64).expect("len fits") * 7;
        exec_gen::exercise(&rng_bytes(seed, len));
    }
}

/// Generation is deterministic per input: the same bytes always produce
/// the same program, library, chain members, std episode, oracle, and
/// census, so any campaign finding reproduces from the raw input alone.
#[test]
fn generation_is_deterministic() {
    for seed in [1u64, 42, 97531] {
        let data = rng_bytes(seed, 33);
        let a = exec_gen::generate(&data);
        let b = exec_gen::generate(&data);
        assert_eq!(a.source, b.source, "same input, same program");
        assert_eq!(a.library, b.library, "same input, same library");
        assert_eq!(a.origin, b.origin, "same input, same origin member");
        assert_eq!(a.mid, b.mid, "same input, same middle member");
        assert_eq!(
            a.pub_variant, b.pub_variant,
            "same input, same drawn refusal variant"
        );
        assert_eq!(a.std_core, b.std_core, "same input, same std core");
        assert_eq!(
            a.std_members, b.std_members,
            "same input, same std members file"
        );
        assert_eq!(a.std_source, b.std_source, "same input, same std consumer");
        assert_eq!(
            a.std_opaque, b.std_opaque,
            "same input, same drawn std refusal"
        );
        assert_eq!(a.expected, b.expected, "same input, same oracle");
        assert_eq!(
            a.std_expected, b.std_expected,
            "same input, same std oracle"
        );
        assert_eq!(a.drawn.or_patterns, b.drawn.or_patterns, "same census");
        assert_eq!(
            a.drawn.pub_imports, b.drawn.pub_imports,
            "same chain census"
        );
        assert_eq!(a.drawn.std_graphs, b.drawn.std_graphs, "same std census");
        assert_eq!(
            a.drawn.std_opaque_refusals, b.drawn.std_opaque_refusals,
            "same std refusal census"
        );
        assert_eq!(
            a.chain_over, b.chain_over,
            "same input, same drawn budget variant"
        );
    }
}
