//! Randomized checker property, the nextest-runnable stand-in for the
//! cargo-fuzz target `fuzz_check`: the full pipeline (parse, bidirectional
//! check with the affine pass, effects walk) never panics on arbitrary
//! source bytes. Corpus seeds and their mutations (real surface shapes),
//! generated nesting (ScopedMap snapshot/restore stress), refinement
//! combinations, and two-file checks with rendering round out the random
//! byte arm.

use std::fmt::Write as _;

use ds_check::{DeclaredRows, Effect, EffectRow, WorldCaps, check, effects_of};
use ds_diag::{Files, render};
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

    fn below(&mut self, bound: usize) -> usize {
        let bound = u64::try_from(bound).expect("bound fits u64").max(1);
        usize::try_from(self.next_u64() % bound).expect("remainder fits usize")
    }
}

/// One deterministic mutation of a seed: truncate, flip, insert, or
/// splice (the same four-op scheme as the parse mirror, so both mirrors
/// walk the corpus identically).
fn mutate(seed: &[u8], rng: &mut Rng) -> Vec<u8> {
    let mut mutated = seed.to_vec();
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
            let from = rng.below(mutated.len());
            let to = from + rng.below(mutated.len() - from);
            let tail = mutated.split_off(to);
            let run = mutated.split_off(from);
            mutated.extend(tail);
            mutated.extend(run);
        },
    }
    mutated
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
    let effects = effects_of(&[&ast], &declared_rows(data), &WorldCaps::new());
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

/// The committed seed corpus of the cargo-fuzz targets (real `.ds`
/// sources) plus deterministic mutations of every seed, through the full
/// pipeline: broken input is diagnostics-as-data, never an abort. Only
/// `*.ds` files are seeds; extension-less files are local campaign
/// outputs and are skipped so the run stays deterministic.
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
        seeds.len() >= 23,
        "the committed seed corpus must be present (found {})",
        seeds.len()
    );
    for (index, path) in seeds.iter().enumerate() {
        let seed = std::fs::read(path).unwrap_or_else(|err| panic!("{}: {err}", path.display()));
        property(&seed);
        for round in 0..4u64 {
            let salt = u64::try_from(index).expect("index fits u64") + 1;
            let mut rng = Rng(salt.wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ round);
            property(&mutate(&seed, &mut rng));
        }
    }
}

/// A deeply nested source: every level opens a fresh checking scope (an
/// if's then-branch, a match binder arm, or a for body), so the checker's
/// ScopedMap snapshot/restore pairing runs at the generated depth and
/// the undo log replays in strict LIFO order. Levels cycle the three
/// scope kinds so the combinations interleave.
fn nested_source(depth: usize) -> String {
    let mut src = String::from("fn deep(v0: int | string) -> string {\n");
    let mut close: Vec<String> = Vec::new();
    for d in 0..depth {
        let v = format!("v{d}");
        let n = format!("v{}", d + 1);
        match d % 3 {
            0 => {
                let _ = write!(src, "  if {v} is int {{\n  const {n} = {v}\n");
                close.push("  } else {\n  \"out\"\n  }\n".to_owned());
            },
            1 => {
                let _ = write!(
                    src,
                    "  match {v} {{\n  0 => {{\n  \"zero\"\n  }}\n  {n} => {{\n"
                );
                close.push("  }\n  _ => {\n  \"other\"\n  }\n  }\n".to_owned());
            },
            _ => {
                let _ = write!(src, "  for i{d} in 0..1 {{\n  const {n} = {v}\n");
                close.push("  }\n".to_owned());
            },
        }
    }
    src.push_str("  \"deep\"\n");
    while let Some(tail) = close.pop() {
        src.push_str(&tail);
    }
    src.push_str("}\n");
    src
}

/// Deep and wide scope nesting through the full pipeline. Depth drives
/// nested snapshot/restore; the wide sibling body drives repeated
/// mark/restore at one level (a long undo log with many pairs). The
/// generated sources must parse: a generator drift fails loudly instead
/// of silently skipping the checker.
#[test]
fn nested_and_sibling_scopes_never_panic() {
    for depth in [3, 7, 15, 27] {
        let source = nested_source(depth);
        let mut files = Files::new();
        let file = files.add_source("nested.ds", &source);
        parse(&files, file).unwrap_or_else(|diags| panic!("depth {depth} must parse: {diags:?}"));
        property(source.as_bytes());
    }
    let mut wide = String::from("fn sibs(v: int | string) -> int {\n  var out = 0\n");
    for s in 0..24 {
        let arm = if s % 2 == 0 {
            format!("  if v is int {{\n  out = out + {s}\n  }} else {{\n  out = out + 1\n  }}\n")
        } else {
            format!(
                "  match v {{\n  {s} => {{\n  out = out + {s}\n  }}\n  _ => {{\n  out = out + 2\n  }}\n  }}\n"
            )
        };
        wide.push_str(&arm);
    }
    wide.push_str("  out\n}\n");
    let mut files = Files::new();
    let file = files.add_source("wide.ds", &wide);
    parse(&files, file).unwrap_or_else(|diags| panic!("wide must parse: {diags:?}"));
    property(wide.as_bytes());
}

/// The R15 refinement surface, enumerated: `&&`-chain conditions of one
/// to four conjuncts with explicit else branches (the else narrows
/// nothing for chains, inverts single conjuncts), and match arms mixing
/// literal, guarded, binder, and wildcard patterns over unions.
#[test]
fn refinement_combinations_never_panic() {
    let mut shapes: Vec<String> = Vec::new();
    for conjuncts in 1..=4usize {
        let tests: Vec<String> = (0..conjuncts).map(|i| format!("x{i} is int")).collect();
        let then: Vec<String> = (0..conjuncts).map(|i| format!("x{i} + 1")).collect();
        let params: Vec<String> = (0..conjuncts)
            .map(|i| format!("x{i}: int | string"))
            .collect();
        shapes.push(format!(
            "fn chain({}) -> int {{\n  if {} {{ {} }} else {{ 0 }}\n}}\n",
            params.join(", "),
            tests.join(" && "),
            then.join(" + "),
        ));
        // implicit else: the value arm alone, a trailing 0 expression
        shapes.push(format!(
            "fn no_else({}) -> int {{\n  if {} {{ {} }}\n  0\n}}\n",
            params.join(", "),
            tests.join(" && "),
            then.join(" + "),
        ));
    }
    shapes.push(
        "fn arms(v: int | string) -> string {\n  match v {\n    0 => \"zero\",\n    \"one\" => \
         \"named\",\n    n if n is int => \"narrow\",\n    n => \"bound\",\n  }\n}\n"
            .to_owned(),
    );
    shapes.push(
        "fn never_cover(v: int | string) -> string {\n  match v {\n    true => \"bool\",\n    'c' \
         => \"char\",\n    b'x' => \"byte\",\n    _ => \"rest\",\n  }\n}\n"
            .to_owned(),
    );
    for source in &shapes {
        let mut files = Files::new();
        let file = files.add_source("refine.ds", source);
        parse(&files, file).unwrap_or_else(|diags| panic!("must parse: {diags:?}"));
        property(source.as_bytes());
    }
}

/// Two files in one check run (the G3 shared namespace): declarations in
/// one file consumed by the other, with honest cross-file diagnostics.
/// Every diagnostic from both the parse and the check walks renders
/// through both ds-diag renderers — the R25 cross-file excerpt naming
/// and the R20 stale-offset clamps must survive real pipeline output.
#[test]
fn cross_file_check_renders_never_panic() {
    let lib = "enum cfg_err {\n  missing\n  bad_port { got: int }\n}\n\nfn get_port(j: int | \
               string) -> int | missing | bad_port {\n  if j is int && j >= 1 && j <= 65535 { j } \
               else { bad_port(got: -1) }\n}\n";
    let user = "fn load(path: string) -> int | missing | bad_port {\n  const port = \
                get_port(path)?\n  port\n}\n\nfn main() -> () {\n  println(load(\"app.cfg\"))\n}\n";
    let mut files = Files::new();
    let lib_id = files.add_source("lib.ds", lib);
    let user_id = files.add_source("user.ds", user);
    for (name, id) in [("lib.ds", lib_id), ("user.ds", user_id)] {
        match parse(&files, id) {
            Ok(ast) => {
                let (checked, linearity) = check(&files, &[ast]);
                for diag in &checked.diags {
                    std::hint::black_box(render::human(diag, &files));
                    std::hint::black_box(render::to_json(diag, &files));
                }
                let _ = (name, linearity.moves.len());
            },
            Err(diags) => {
                for diag in &diags {
                    std::hint::black_box(render::human(diag, &files));
                    std::hint::black_box(render::to_json(diag, &files));
                }
            },
        }
    }
    let lib_ast = parse(&files, lib_id).expect("lib.ds parses");
    let user_ast = parse(&files, user_id).expect("user.ds parses");
    let (checked, linearity) = check(&files, &[lib_ast, user_ast]);
    assert!(
        !checked.diags.is_empty(),
        "the fixture's wrong-argument call must produce diagnostics"
    );
    let rendered: Vec<String> = checked
        .diags
        .iter()
        .map(|diag| render::human(diag, &files))
        .collect();
    std::hint::black_box((&rendered, linearity.moves.len()));
}
