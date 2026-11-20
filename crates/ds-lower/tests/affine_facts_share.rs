//! Wall-clock harness for the affine-facts share of lowering: a package
//! of N functions over U tracked bindings each lowers once per rep with
//! the affine walk handed in pre-run, so the timed span is the lowerer
//! alone. The placement facts are package-wide tables read by every
//! frame, so the measured shape is functions x rows — a run of this
//! harness before and after a facts-sharing change attributes the delta.

use std::{fmt::Write, time::Instant};

use ds_check::{analyze, linearity_check};
use ds_diag::Files;
use ds_lower::{ComptimeValues, lower_with_comptime};
use ds_mod::Hash128;
use ds_parse::parse;

/// One generated package: `functions` fns of `params` int parameters plus
/// `lets` const-binders each (every binder tracked, one row per binder),
/// and a `main` calling the first fn. Ints keep the checker cheap so the
/// timed lower holds most of the run's cost.
fn generate(functions: usize, params: usize, lets: usize) -> String {
    let mut source = String::from("tag int\n\n");
    for f in 0..functions {
        let _ = write!(source, "fn f{f} (");
        for p in 0..params {
            if p > 0 {
                source.push_str(", ");
            }
            let _ = write!(source, "a{p}: int");
        }
        source.push_str(") -> int {\n");
        for l in 0..lets {
            let lhs = if l == 0 { format!("a{l}") } else { format!("s{}", l - 1) };
            let _ = writeln!(source, "  const s{l} = {lhs} + a{}", l % params);
        }
        let _ = writeln!(source, "  s{}\n}}\n", lets.saturating_sub(1));
    }
    source.push_str("fn main () -> int {\n  f0(");
    for p in 0..params {
        if p > 0 {
            source.push_str(", ");
        }
        source.push('1');
    }
    source.push_str(")\n}\n");
    source
}

/// Times `reps` lowers of one generated package and returns the medians
/// per package in ms.
fn medians(sizes: &[(usize, usize, usize)], reps: usize) -> Vec<f64> {
    let values = ComptimeValues::default();
    let mut out = Vec::new();
    for &(functions, params, lets) in sizes {
        let source = generate(functions, params, lets);
        let mut files = Files::new();
        let id = files.add_source("facts_perf.ds", &source);
        let asts = [parse(&files, id).expect("the source parses")];
        let analyzed = analyze(&files, &asts);
        assert!(
            analyzed.checked.diags.is_empty(),
            "the source checks clean: {}",
            analyzed
                .checked
                .diags
                .iter()
                .map(|d| format!("{d:?}"))
                .collect::<Vec<_>>()
                .join("\n")
        );
        let lin = linearity_check(
            &asts,
            &analyzed.package,
            &analyzed.resolutions,
            &analyzed.decls,
            &analyzed.checked,
        );
        let hash = Hash128::compute(source.as_bytes());
        // warm-up rep outside the sample: page faults and the checker's
        // lazily built tables must not enter the medians
        let _ = lower_with_comptime("facts_perf", hash, &asts, &analyzed, Some(&lin), &values)
            .expect("the source lowers");
        let mut samples = Vec::with_capacity(reps);
        for _ in 0..reps {
            let start = Instant::now();
            let _ = lower_with_comptime("facts_perf", hash, &asts, &analyzed, Some(&lin), &values)
                .expect("the source lowers");
            samples.push(start.elapsed().as_secs_f64() * 1000.0);
        }
        samples.sort_by(f64::total_cmp);
        let median = samples[reps / 2];
        let rows = functions * (params + lets);
        println!("{functions} fns x {rows} rows: median {median:.1} ms over {reps} reps");
        out.push(median);
    }
    out
}

#[test]
#[ignore = "a wall-clock measurement, not a gate"]
fn affine_facts_share_of_lowering() {
    medians(&[(50, 40, 10), (200, 40, 10), (400, 40, 10)], 7);
}
