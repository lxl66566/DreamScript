//! Criterion parse-only bench: every conformance `.ds` file through
//! `ds_parse::parse` with no checker, lowerer, or VM behind it, so the group
//! measures the lexer plus the pest grammar alone. The corpus is the fixed
//! parse workload the conformance gate already keeps valid; a file that fails
//! to parse fails the bench instead of timing a diagnostic path.

use std::path::PathBuf;

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use ds_diag::{FileId, Files};
use ds_parse::parse;

/// The corpus sources held in one [`Files`] registry, ordered by name, plus
/// their ids and total byte count.
struct Corpus {
    files: Files,
    ids: Vec<FileId>,
    bytes: u64,
}

fn corpus() -> Corpus {
    let root = ds_corpus_path::dir();
    let mut paths: Vec<PathBuf> = std::fs::read_dir(&root)
        .expect("the conformance directory lists")
        .map(|entry| entry.expect("the directory entry reads").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "ds"))
        .collect();
    paths.sort();
    let mut files = Files::new();
    let mut ids = Vec::with_capacity(paths.len());
    let mut bytes = 0;
    for path in &paths {
        let source = std::fs::read_to_string(path).expect("the corpus file reads");
        bytes += source.len() as u64;
        ids.push(
            files.add_source(
                path.file_name()
                    .expect("the corpus file has a name")
                    .to_string_lossy()
                    .as_ref(),
                &source,
            ),
        );
    }
    assert!(!ids.is_empty(), "the corpus carries at least one file");
    Corpus { files, ids, bytes }
}

fn bench_parse(c: &mut Criterion) {
    let corpus = corpus();
    for id in &corpus.ids {
        parse(&corpus.files, *id).expect("the corpus file parses");
    }
    let mut group = c.benchmark_group("parse");
    group.throughput(Throughput::Bytes(corpus.bytes));
    group.bench_function("corpus", |b| {
        b.iter(|| {
            for id in &corpus.ids {
                let ast = parse(&corpus.files, *id).expect("the corpus file parses");
                std::hint::black_box(ast);
            }
        });
    });
    group.finish();
}

criterion_group!(benches, bench_parse);
criterion_main!(benches);
