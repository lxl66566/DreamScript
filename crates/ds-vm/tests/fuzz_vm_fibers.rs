//! Randomized fiber/nursery property, the nextest-runnable stand-in for the
//! cargo-fuzz target `fuzz_vm`'s fiber leg: structurally generated modules
//! (the shared generator in fuzz/vm_gen.rs, included by path so target and
//! mirrors can never drift) run through the writer/reader round trip, the
//! loader, a budgeted execution, and the host fiber face (spawn with
//! staged Ref/string words, resume with payloads, join or teardown),
//! and — over the vm's own repl face — the trapped line's rollback
//! tearing its abandoned nursery down (never-entered children released,
//! the slots recycled), and every stop is structured — never a panic.

#[path = "../../../fuzz/vm_gen.rs"]
mod vm_gen;

use ds_vm::abi::uop::{Inst, Uop};

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

/// The record's body decoded to instructions, one per pc.
fn insts(words: &[u32]) -> Vec<Inst> {
    words
        .iter()
        .enumerate()
        .map(|(pc, word)| {
            Inst::decode(u32::try_from(pc).expect("pc fits u32"), *word)
                .expect("ds-mod validated every word")
        })
        .collect()
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
        total.records += shape.records;
        total.bytes += shape.bytes;
        total.reloads += shape.reloads;
        total.reload_accepted += shape.reload_accepted;
        total.reload_refused += shape.reload_refused;
        total.sentinel += shape.sentinel;
        total.defaults += shape.defaults;
        total.deinit += shape.deinit;
        total.repls += shape.repls;
        total.repl_nursery_released += shape.repl_nursery_released;
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
    // the fixed templates' draw census: every landed-surface shape the
    // generator can draw must be drawn somewhere in the sweep (a
    // weighting regression fails loudly); runtime depth is the campaign's
    assert!(total.records > 0, "the records-list template is drawn");
    assert!(total.bytes > 0, "the bytes-bridge template is drawn");
    // the hot-reload re-key surface: mid-drive swaps occur, and both the
    // landing and the structured refusal classes run across the sweep
    assert!(total.reloads > 0, "the reload phase is drawn");
    assert!(
        total.reload_accepted > 0,
        "a swap lands somewhere in the sweep"
    );
    assert!(
        total.reload_refused > 0,
        "a swap refuses structurally somewhere in the sweep"
    );
    assert!(
        total.sentinel > 0,
        "the sentinel evidence template is drawn"
    );
    assert!(total.deinit > 0, "the deinit-claim template is drawn");
    assert!(total.defaults > 0, "the trait-defaults template is drawn");
    assert!(total.repls > 0, "the repl rollback phase runs");
    assert!(
        total.repl_nursery_released > 0,
        "a trapped line's own nursery tears down at the rollback"
    );
}

/// Every committed seed of the cargo-fuzz corpus holds the property. Only
/// `*.bin` files are seeds; the extension-less hashed names in the
/// directory are local campaign outputs and are skipped so the run stays
/// deterministic.
#[test]
fn corpus_seeds_stop_structurally() {
    const DIR: &str = "../../fuzz/corpus/fuzz_vm";
    let mut seeds: Vec<_> = std::fs::read_dir(DIR)
        .unwrap_or_else(|err| panic!("{DIR}: {err}"))
        .map(|entry| entry.expect("readable dir entry").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "bin"))
        .collect();
    seeds.sort();
    assert!(
        seeds.len() >= 24,
        "the committed seed corpus must be present (found {})",
        seeds.len()
    );
    for path in seeds {
        let bytes = std::fs::read(&path).unwrap_or_else(|err| panic!("{}: {err}", path.display()));
        let (outcome, shape) = vm_gen::exercise_fibers(&bytes);
        assert!(matches!(outcome, 0 | 1), "a structured stop only");
        std::hint::black_box(&shape);
    }
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

/// The loop-template landing discipline: a random jump emitted before the
/// loop block may not land inside it (the countdown Kinms, the body, or
/// the three-word tail). Such a landing enters the countdown on
/// uninitialized frame words — recycled fiber chunks carry stale values
/// (the pool-slot reuse the establishment band guards), so a stale
/// counter or one could keep the decrement from ever reaching zero and
/// the join driver would hang with no step budget to cut it. A drawn
/// landing once did exactly that under the repetition arm's seed 1931,
/// so that input is pinned here alongside the sweep.
#[test]
fn pre_loop_jumps_stay_outside_the_loop_block() {
    let check = |bytes: &[u8]| {
        let mut stream = vm_gen::Stream::new(bytes);
        let (module, _entries, _drawn) = vm_gen::build_planned(&mut stream);
        for func in module.functions() {
            let code = insts(func.code());
            for (edge_at, inst) in code.iter().enumerate() {
                let Inst::Uop(Uop::Jmpif { target, .. }) = inst else {
                    continue;
                };
                let head = usize::try_from(*target).expect("targets are tiny");
                if head > edge_at {
                    continue; // a forward branch, not a loop back edge
                }
                // landing exactly on `start` (the first countdown Kimm) is the
                // generator's own safe retarget: the Kinms re-initialize the
                // counter before the back edge ever passes the decrement
                let start = head.checked_sub(3).expect("three Kinms precede the body");
                for (at, inst) in code.iter().enumerate() {
                    let target = match inst {
                        Inst::Uop(
                            Uop::Jmp { target }
                            | Uop::Jmpif { target, .. }
                            | Uop::JmpifNot { target, .. },
                        ) => usize::try_from(*target).expect("targets are tiny"),
                        _ => continue,
                    };
                    assert!(
                        !(at < start && (start + 1..=edge_at).contains(&target)),
                        "{}: a pre-loop jump at {at} lands inside the loop block \
                         {start}..={edge_at}",
                        func.name()
                    );
                }
            }
        }
    };
    for seed in 0..240u64 {
        check(&rng_bytes(seed, 16 + (seed % 24) as usize * 11));
    }
    // the input that hung the repetition arm pre-fix: the base mirror's
    // seed formula for run 1931, driven end to end here as well
    let seed = 1931u64.wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ 0xa5a5_a5a5_a5a5_a5a5;
    let bytes = rng_bytes(seed, 8 + (seed % 64) as usize * 7);
    check(&bytes);
    let (outcome, shape) = vm_gen::exercise_fibers(&bytes);
    assert!(matches!(outcome, 0 | 1), "a structured stop only");
    assert!(shape.defaults > 0 || shape.records > 0 || shape.sentinel > 0);
}
