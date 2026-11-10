//! Randomized parser property, the nextest-runnable stand-in for the
//! cargo-fuzz target `fuzz_parse`: the scanner and grammar never panic on
//! arbitrary bytes, valid UTF-8 or not. Both arms are consumed so coverage
//! counts accepted and rejected inputs alike.

use ds_diag::Files;
use ds_parse::parse;

/// xorshift64* PRNG: deterministic, no dependency, good enough for byte
/// noise (mirrors crates/ds-mod/tests/fuzz_read.rs).
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

    fn below(&mut self, bound: usize) -> usize {
        let bound = u64::try_from(bound).expect("bound fits u64").max(1);
        usize::try_from(self.next_u64() % bound).expect("remainder fits usize")
    }
}

/// The never-panic property over one input; diagnostics are the parser's
/// contract for bad source.
fn property(data: &[u8]) {
    let source = String::from_utf8_lossy(data);
    let mut files = Files::new();
    let file = files.add_source("fuzz.ds", &source);
    match parse(&files, file) {
        Ok(ast) => std::hint::black_box((
            ast.blocks.len(),
            ast.blocks.first().map_or(0, |block| block.items.len()),
        )),
        Err(diags) => std::hint::black_box((
            diags.len(),
            diags
                .first()
                .map_or(0, |diagnostic| diagnostic.labels.len()),
        )),
    };
}

/// Case count for `random_bytes_never_panic`; a CI fuzz job can crank it
/// with `FUZZ_RUNS=<n> cargo nextest run`. Invalid or unset values keep the
/// default.
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

/// The committed seed corpus (real `.ds` sources) plus deterministic
/// mutations of every seed: truncation, byte flip, insertion, and splice.
/// Only `*.ds` files are seeds; extension-less files in the directory are
/// local campaign outputs and are skipped so the run stays deterministic.
#[test]
fn corpus_seeds_and_mutations_never_panic() {
    const DIR: &str = "../../fuzz/corpus/fuzz_parse";
    let mut seeds: Vec<_> = std::fs::read_dir(DIR)
        .unwrap_or_else(|err| panic!("{DIR}: {err}"))
        .map(|entry| entry.expect("readable dir entry").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "ds"))
        .collect();
    seeds.sort();
    assert!(
        seeds.len() >= 16,
        "the committed seed corpus must be present (found {})",
        seeds.len()
    );
    for (index, path) in seeds.iter().enumerate() {
        let seed = std::fs::read(path).unwrap_or_else(|err| panic!("{}: {err}", path.display()));
        property(&seed);
        for round in 0..4u64 {
            let salt = u64::try_from(index).expect("index fits u64") + 1;
            let mut rng = Rng(salt.wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ round);
            let mut mutated = seed.clone();
            match rng.below(4) {
                0 => mutated.truncate(rng.below(mutated.len())),
                1 => {
                    let at = rng.below(mutated.len());
                    mutated[at] ^= 1u8 << rng.below(8);
                },
                2 => {
                    let at = rng.below(mutated.len() + 1);
                    mutated.insert(at, rng.byte());
                },
                _ => {
                    // Splice: move a run of bytes to the end.
                    let from = rng.below(mutated.len());
                    let to = from + rng.below(mutated.len() - from);
                    let tail = mutated.split_off(to);
                    let run = mutated.split_off(from);
                    mutated.extend(tail);
                    mutated.extend(run);
                },
            }
            property(&mutated);
        }
    }
}
