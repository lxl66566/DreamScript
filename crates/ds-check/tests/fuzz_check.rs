//! Randomized checker property, the nextest-runnable stand-in for the
//! cargo-fuzz target `fuzz_check`: the full pipeline (parse, bidirectional
//! check with the affine pass, effects walk) never panics on arbitrary
//! source bytes.

use ds_check::{DeclaredRows, Effect, EffectRow, check, effects_of};
use ds_diag::Files;
use ds_parse::parse;

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

/// The never-panic property: arbitrary bytes through the whole pipeline.
/// Diagnostics are the pipeline's contract for bad input; consumption only
/// pins that the walks complete and produce their outputs.
fn property(data: &[u8]) {
    let source = String::from_utf8_lossy(data);
    let mut files = Files::new();
    let file = files.add_source("fuzz.ds", &source);
    let Ok(ast) = parse(&files, file) else {
        return;
    };
    let effects = effects_of(&[&ast], &declared_rows(data));
    let (checked, linearity) = check(&files, &[ast]);
    let _ = (
        checked.diags.len(),
        linearity.moves.len(),
        effects.def_count(),
    );
}

/// Mirrors the fuzz target's ambient-row derivation: identifier-shaped runs
/// from the input become declared rows, exercising `effects_of`'s union
/// edges rather than only the empty-row walk.
fn declared_rows(data: &[u8]) -> DeclaredRows {
    const EFFECTS: [Effect; 5] = [
        Effect::Error,
        Effect::Trap,
        Effect::World,
        Effect::Cancel,
        Effect::Discontinue,
    ];
    let mut declared = DeclaredRows::new();
    let mut names = 0;
    let mut run_start = None;
    for (i, &byte) in data.iter().chain(std::iter::once(&0)).enumerate() {
        let ident = byte.is_ascii_alphanumeric() || byte == b'_';
        if ident && run_start.is_none() {
            run_start = Some(i);
        }
        if !ident
            && let Some(start) = run_start.take()
            && i - start <= 8
            && names < 8
        {
            names += 1;
            let name = String::from_utf8_lossy(&data[start..i]).into_owned();
            let mut row = EffectRow::PURE;
            for (bit, effect) in EFFECTS.iter().enumerate() {
                if data[start.midpoint(i)] >> bit & 1 == 1 {
                    row = row.union(EffectRow::of(*effect));
                }
            }
            declared = declared.with(&name, row);
        }
    }
    declared
}

/// Case count; a CI fuzz job can crank it with `FUZZ_RUNS=<n> cargo nextest
/// run`. Invalid or unset values keep the default.
fn fuzz_runs() -> u64 {
    std::env::var("FUZZ_RUNS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(240)
}

#[test]
fn random_bytes_never_panic() {
    for seed in 1..=fuzz_runs() {
        let mut rng = Rng(seed | 1);
        let len = usize::from(rng.byte()) as u64 * 4;
        let data: Vec<u8> = (0..len).map(|_| rng.byte()).collect();
        property(&data);
    }
}

/// The pipeline completes over every conformance source; the corpus
/// checker does not require them to be diagnostic-free (they reference
/// stdlib names that have no declaration yet), only panic-free. These are
/// also the seed corpus of the cargo-fuzz target.
#[test]
fn conformance_sources_never_panic() {
    for name in [
        "effects_nursery.ds",
        "generics_coherent_traits.ds",
        "linearity_modes_rc.ds",
        "linearity_negative.ds",
        "modules_hash_core.ds",
        "syntax_ts_heart.ds",
        "types_hybrid.ds",
    ] {
        let path = format!("../../conformance/{name}");
        let source = std::fs::read_to_string(&path).unwrap_or_else(|err| panic!("{path}: {err}"));
        let mut files = Files::new();
        let file = files.add_source(name, &source);
        let Ok(ast) = parse(&files, file) else {
            panic!("{name}: conformance source must parse: parse failure is a regression")
        };
        let (checked, linearity) = check(&files, &[ast]);
        let _ = (checked.diags.len(), linearity.moves.len());
    }
}
