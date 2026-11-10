//! Timing harness for the checking passes: the conformance corpus through
//! `linearity::check` (which runs the full analyze pipeline plus the
//! affine pass) N times under `std::time::Instant`, printing per-file and
//! total medians. Not a libtest bench: run with
//! `cargo bench -p ds-check --bench check_perf`; the round count defaults
//! to 30 and is overridable via `DS_CHECK_BENCH_ROUNDS`. The corpus set
//! and ambients mirror the conformance gate so the measured work is the
//! gate's work.

use std::{path::PathBuf, time::Instant};

use ds_check::linearity::check;
use ds_diag::Files;
use ds_parse::parse;

const TYPES_AMBIENT: &str = concat!(
    "tag int\n",
    "tag float\n",
    "tag string\n",
    "tag bool\n",
    "trait Copyable { }\n",
    "struct list[t]\n",
    "struct map[k, v]\n",
    "extern fn read_file (path: string) -> string | io_err\n",
    "extern fn parse_json (text: string) -> json | io_err\n",
    "extern fn str[t] (v: t) -> string\n",
    "extern fn log (msg: string) -> ()\n",
    "extern fn default_config () -> config\n",
    "extern fn get[k, v] (m: map[k, v], key: k) -> v | none\n",
);

const LINEARITY_AMBIENT: &str = concat!(
    "tag int\n",
    "tag char\n",
    "tag bool\n",
    "tag string\n",
    "type u32 = int\n",
    "trait Copyable { }\n",
    "struct bytes: ~Copyable { len: int }\n",
    "struct list[t]\n",
    "extern fn open (path: string) -> file\n",
    "extern fn close (fd: raw_fd) -> ()\n",
    "extern fn read_exact (f: mut file, n: int) -> bytes\n",
    "extern fn to_u32_le (b: move bytes) -> u32\n",
    "extern fn split (s: string, sep: char) -> list[string]\n",
    "extern fn map[t, r] (xs: list[t], f: fn(t) -> r) -> list[r]\n",
    "extern fn collect[r] (xs: list[r]) -> list[r]\n",
    "extern fn len[t] (xs: t) -> int\n",
);

const LINEARITY_NEG_AMBIENT: &str = concat!("tag int\n", "tag string\n", "trait Copyable { }\n",);

const SWEEP_AMBIENT: &str = concat!(
    "tag int\n",
    "tag float\n",
    "tag string\n",
    "tag bool\n",
    "tag char\n",
    "trait Copyable { }\n",
    "struct list[t]\n",
    "struct map[k, v]\n",
);

const GENERICS_AMBIENT: &str = concat!(
    "tag int\n",
    "tag string\n",
    "tag bool\n",
    "trait Copyable { }\n",
    "struct list[t]\n",
    "extern fn cons[t] (x: t, xs: list[t]) -> list[t]\n",
    "extern fn head[t] (xs: list[t]) -> t?\n",
    "extern fn tail[t] (xs: list[t]) -> list[t]\n",
    "extern fn join (parts: list[string], sep: string) -> string\n",
    "extern fn map[t, r] (xs: list[t], f: fn(t) -> r) -> list[r]\n",
    "extern fn str[t] (v: t) -> string\n",
);

const PATTERN_DISPATCH_AMBIENT: &str = concat!(
    "tag int\n",
    "tag string\n",
    "tag bool\n",
    "trait Copyable { }\n",
    "extern fn read_file (path: string) -> string | io_error\n",
    "extern fn println (msg: string) -> ()\n",
);

const TUPLES_AMBIENT: &str = concat!(
    "tag int\n",
    "tag string\n",
    "tag bool\n",
    "trait Copyable { }\n",
    "struct list[t]\n",
    "extern fn list_len[t] (xs: list[t]) -> int\n",
);

const CLOSURES_AMBIENT: &str = concat!("tag int\n", "tag string\n", "trait Copyable { }\n");

const LISTS_AMBIENT: &str = concat!(
    "tag int\n",
    "tag string\n",
    "tag bool\n",
    "trait Copyable { }\n",
    "struct list[t]\n",
    "extern fn list_len[t] (xs: list[t]) -> int\n",
    "extern fn list_get[t] (xs: list[t], i: int) -> t | none\n",
    "extern fn list_concat[t] (a: list[t], b: list[t]) -> list[t]\n",
    "extern fn println (msg: string) -> ()\n",
);

const MAPS_AMBIENT: &str = concat!(
    "tag int\n",
    "tag string\n",
    "tag bool\n",
    "trait Copyable { }\n",
    "struct map[k, v]\n",
    "extern fn map_set[k, v] (m: mut map[k, v], key: k, value: v) -> ()\n",
    "extern fn map_get[k, v] (m: map[k, v], key: k) -> v | none\n",
    "extern fn map_len[k, v] (m: map[k, v]) -> int\n",
    "extern fn get[k, v] (m: map[k, v], key: k) -> v | none\n",
);

const EXTRACTION_AMBIENT: &str = concat!(
    "tag int\n",
    "tag string\n",
    "tag bool\n",
    "tag char\n",
    "trait Copyable { }\n",
    "struct list[t]\n",
    "struct map[k, v]\n",
    "extern fn println (msg: string) -> ()\n",
    "extern fn list_get[t] (xs: list[t], i: int) -> t | none\n",
    "extern fn get[k, v] (m: map[k, v], key: k) -> v | none\n",
    "extern fn unwrap_or[t] (x: t?, y: t) -> t\n",
    "extern fn unwrap_or_else[t] (x: t?, f: fn() -> t) -> t\n",
    "extern fn ok_or[t, e] (x: t?, err: e) -> t | e\n",
    "extern fn map_err[t, e, e2] (x: t | e, f: fn(e) -> e2) -> t | e2\n",
);

/// One parsed corpus file plus its ambient, ready for repeated checks.
struct Input {
    name: &'static str,
    files: Files,
    asts: Vec<ds_parse::Ast>,
    /// The diagnostic count of the first round: every later round must
    /// reproduce it or the timing is comparing different work.
    diags: usize,
}

fn median(values: &mut [f64]) -> f64 {
    values.sort_by(f64::total_cmp);
    values[values.len() / 2]
}

fn main() {
    let corpus: &[(&str, &str)] = &[
        ("types_hybrid.ds", TYPES_AMBIENT),
        ("pattern_dispatch.ds", PATTERN_DISPATCH_AMBIENT),
        ("linearity_modes_rc.ds", LINEARITY_AMBIENT),
        ("linearity_negative.ds", LINEARITY_NEG_AMBIENT),
        ("effects_nursery.ds", SWEEP_AMBIENT),
        ("lists_surface.ds", LISTS_AMBIENT),
        ("tuples_surface.ds", TUPLES_AMBIENT),
        ("syntax_ts_heart.ds", SWEEP_AMBIENT),
        ("generics_coherent_traits.ds", GENERICS_AMBIENT),
        ("modules_hash_core.ds", SWEEP_AMBIENT),
        ("maps_surface.ds", MAPS_AMBIENT),
        ("extraction_family.ds", EXTRACTION_AMBIENT),
        ("closures_surface.ds", CLOSURES_AMBIENT),
    ];
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../conformance");
    let mut inputs = Vec::new();
    for (name, ambient) in corpus {
        let source = std::fs::read_to_string(root.join(name)).unwrap();
        let mut files = Files::new();
        let amb = files.add_source("ambient.ds", ambient);
        let cor = files.add_source("corpus.ds", &source);
        let asts = [
            parse(&files, amb).unwrap_or_else(|d| panic!("{name} ambient: {d:?}")),
            parse(&files, cor).unwrap_or_else(|d| panic!("{name} corpus: {d:?}")),
        ];
        let (checked, _) = check(&files, &asts);
        inputs.push(Input {
            name,
            files,
            asts: asts.into(),
            diags: checked.diags.len(),
        });
    }

    let rounds: usize = std::env::var("DS_CHECK_BENCH_ROUNDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(30);
    // Warmup rounds stabilize the allocator and caches before timing.
    for _ in 0..3 {
        for input in &inputs {
            let _ = check(&input.files, &input.asts);
        }
    }
    let mut per_file: Vec<Vec<f64>> = inputs.iter().map(|_| Vec::new()).collect();
    let mut totals: Vec<f64> = Vec::new();
    for _ in 0..rounds {
        let round_start = Instant::now();
        for (index, input) in inputs.iter().enumerate() {
            let start = Instant::now();
            let (checked, _) = check(&input.files, &input.asts);
            per_file[index].push(start.elapsed().as_secs_f64() * 1e3);
            assert_eq!(
                checked.diags.len(),
                input.diags,
                "{}: diagnostic count drifted between rounds",
                input.name
            );
        }
        totals.push(round_start.elapsed().as_secs_f64() * 1e3);
    }
    for (index, input) in inputs.iter().enumerate() {
        let values = &mut per_file[index];
        println!("{:>32}  median {:9.3} ms", input.name, median(values));
    }
    println!(
        "{:>32}  median {:9.3} ms  ({rounds} rounds)",
        "TOTAL",
        median(&mut totals)
    );
}
