//! Criterion wall-clock benches over the lowering pipeline: one
//! conformance fixture (`conformance/records_lists.ds`, the list[record]
//! span channel) through parse, check, and lower separately plus the
//! whole pipeline, so a per-stage regression is attributable.
//!
//! The ambient stub is this bench's own: benches link ds-lower as an
//! external crate and cannot reach the gate's copies, and the stub only
//! has to make the fixture check clean, which the conformance gate pins
//! independently. `lower` takes the checked bundle by reference, so the
//! lower-only id measures lowering over a warm bundle exactly as the
//! compiler's memoized query path would call it.

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use ds_check::{Analyzed, analyze};
use ds_diag::Files;
use ds_lower::lower;
use ds_mod::Hash128;
use ds_parse::{Ast, parse};

const FIXTURE: &str = include_str!("../../../conformance/records_lists.ds");

/// The fixture's ambient surface (mirrors the conformance gate's stub).
const AMBIENT: &str = concat!(
    "tag int\n",
    "tag string\n",
    "trait Copyable { }\n",
    "struct list[t]\n",
    "extern fn list_len[t] (xs: list[t]) -> int\n",
    "extern fn list_get[t] (xs: list[t], i: int) -> t | none\n",
    "extern fn list_push[t] (xs: mut list[t], x: t) -> ()\n",
    "extern fn println (msg: string) -> ()\n",
);

/// One parsed package: the ambient member first, then the fixture.
fn parsed() -> (Files, Vec<Ast>) {
    let mut files = Files::new();
    let ambient = files.add_source("ambient.ds", AMBIENT);
    let corpus = files.add_source("records_lists.ds", FIXTURE);
    let asts = vec![
        parse(&files, ambient).expect("the ambient parses"),
        parse(&files, corpus).expect("the fixture parses"),
    ];
    (files, asts)
}

fn checked(files: &Files, asts: &[Ast]) -> Analyzed {
    let analyzed = analyze(files, asts);
    assert!(
        analyzed.checked.diags.is_empty(),
        "the fixture checks clean: {:?}",
        analyzed.checked.diags
    );
    analyzed
}

fn lowered(asts: &[Ast], analyzed: &Analyzed) -> usize {
    let module = lower(
        "records_lists",
        Hash128::compute(FIXTURE.as_bytes()),
        asts,
        analyzed,
    )
    .expect("the fixture lowers");
    black_box(module.functions().len())
}

fn bench_parse(c: &mut Criterion) {
    c.bench_function("lower/parse", |b| {
        b.iter(|| {
            let (_files, asts) = parsed();
            black_box(asts.iter().map(|ast| ast.blocks.len()).sum::<usize>())
        });
    });
}

fn bench_check(c: &mut Criterion) {
    let (files, asts) = parsed();
    c.bench_function("lower/check", |b| {
        b.iter(|| {
            let analyzed = checked(&files, &asts);
            black_box(analyzed.checked.expr_types.len())
        });
    });
}

fn bench_lower(c: &mut Criterion) {
    let (files, asts) = parsed();
    let analyzed = checked(&files, &asts);
    c.bench_function("lower/lower", |b| {
        b.iter(|| lowered(&asts, &analyzed));
    });
}

fn bench_pipeline(c: &mut Criterion) {
    c.bench_function("lower/pipeline", |b| {
        b.iter(|| {
            let (files, asts) = parsed();
            let analyzed = checked(&files, &asts);
            lowered(&asts, &analyzed)
        });
    });
}

criterion_group!(
    benches,
    bench_parse,
    bench_check,
    bench_lower,
    bench_pipeline
);
criterion_main!(benches);
