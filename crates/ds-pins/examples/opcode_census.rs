//! Opcode census over the benchmark kernels: the BL-4 evidence tool.
//!
//! Every `benchmarks/kernels/*.ds` goes through the chain `ds run` uses — the
//! `ds-stdlib` ambient, script-mode parse, `ds_check::analyze`, the platform
//! check, `ds_lower::lower` — and the report prints, per kernel: the static
//! opcode histogram, the per-function opcode mix, the branch/loop structure,
//! and the ranked adjacent opcode pairs (the fusion candidates) with their
//! weighted coverage of that kernel's stream. Coverage is a pair's weighted
//! count over the kernel's whole weighted stream (every function), so a pair's
//! share is the fraction of the executed stream fusing it would touch. Pairs
//! are adjacent runnable uops inside one basic block; a placed `Rc` word
//! between two uops removes the adjacency, so the ranking already accounts for
//! the dup/drop words the assembler would see.
//!
//! Weighting is a static proxy for dynamic counts. Each function is assumed
//! to run once; a natural loop's trip count is read off a constant bound the
//! CFG makes visible (the loop head compares a slot that only a `Kimm` writes
//! against a positive pool constant — the value is the count, assuming a
//! 0-based, step-1 induction), and a loop whose bound is not such a constant
//! uses [`TRIP_PROXY`]. A block's weight is the product of the trip counts of
//! the natural loops containing it, so a doubly nested 1000x1000 loop weighs
//! 10^6. The CFG cannot see a callee's call multiplicity (recursion, the
//! hosted CALLG reentry per element) and cannot see loop bounds computed at
//! runtime: kernels whose cost is call-dominated underweight their bodies
//! relative to a runtime profile, which is why the numbers rank candidates
//! and never decide one.
//!
//! Nothing here asserts a count: the census is not a gate.
//!
//! Re-run: `cargo run -p ds-pins --example opcode_census` for every kernel, or
//! `cargo run -p ds-pins --example opcode_census -- fib lists` to select
//! kernels by stem. [`TOP_PAIRS`] ranked pairs print per kernel, and the
//! trailing table repeats each kernel's top three.

// the shares and mantissas below are printed values, never measurements
#![allow(clippy::cast_precision_loss)]

use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    path::{Path, PathBuf},
};

use ds_abi::{Major, Uop};
use ds_diag::Files;
use ds_lower::{Lowered, PlacedOp, lower};
use ds_mod::{Constant, Hash128};
use ds_parse::{parse, parse_script};

/// The proxy trip count for a loop whose bound is not a pool constant.
const TRIP_PROXY: u64 = 10;

/// The largest constant bound taken as a loop trip count.
const MAX_DERIVED_TRIP: i64 = 1_000_000_000;

/// The smallest constant bound taken as a loop trip count.
const MIN_DERIVED_TRIP: i64 = 2;

/// Ranked pairs printed per kernel.
const TOP_PAIRS: usize = 12;

/// Pairs repeated in the trailing summary table per kernel.
const SUMMARY_PAIRS: usize = 3;

fn main() {
    let filter: Vec<String> = std::env::args().skip(1).collect();
    let selected: Vec<PathBuf> = kernel_files()
        .into_iter()
        .filter(|path| filter.is_empty() || filter.iter().any(|want| stem(path) == *want))
        .collect();
    assert!(
        !selected.is_empty(),
        "no benchmark kernel matched {filter:?}"
    );
    let censuses: Vec<KernelCensus> = selected.iter().map(|path| census(path)).collect();
    for census in &censuses {
        print_kernel(census);
    }
    print_summary(&censuses);
}

/// One kernel's report.
struct KernelCensus {
    name: String,
    functions: usize,
    static_uops: usize,
    weighted_uops: u128,
    histogram: Vec<Row>,
    per_function: Vec<FunctionCensus>,
    pairs: Vec<PairRow>,
    /// Every adjacent pair of each family as a share of the kernel's stream.
    family_shares: Vec<(&'static str, f64)>,
}

/// One opcode's static count and loop-weighted count.
struct Row {
    mnemonic: String,
    count: usize,
    weighted: u128,
}

/// One function's mix and branch structure.
struct FunctionCensus {
    name: String,
    static_uops: usize,
    weighted_uops: u128,
    mix: Vec<Row>,
    blocks: usize,
    jumps: usize,
    conditionals: usize,
    loops: Vec<LoopRow>,
}

/// One natural loop.
struct LoopRow {
    head: usize,
    latch: usize,
    blocks: usize,
    trip: u64,
    derived: bool,
}

/// Accumulated weight of one adjacent opcode pair, with the family the pair
/// first classified as (a pair's opcodes fix its family, so one reading holds).
struct PairTally {
    weighted: u128,
    family: Option<&'static str>,
}

/// One adjacent opcode pair with its weighted coverage of the kernel stream.
struct PairRow {
    first: String,
    second: String,
    weighted: u128,
    coverage: f64,
    family: Option<&'static str>,
}

/// The BL-4 candidate families, in the column order the report prints them.
const FAMILIES: [&str; 4] = ["compare-branch", "tag-branch", "load-compare", "load-op"];

fn stem(path: &Path) -> String {
    path.file_stem()
        .map_or_else(String::new, |stem| stem.to_string_lossy().into_owned())
}

/// Every `.ds` kernel, sorted by stem.
fn kernel_files() -> Vec<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../benchmarks/kernels");
    let mut paths: Vec<PathBuf> = std::fs::read_dir(&dir)
        .expect("the benchmarks/kernels directory exists")
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "ds"))
        .collect();
    paths.sort();
    paths
}

fn census(path: &Path) -> KernelCensus {
    let lowered = compiled(path);
    let mut histogram: BTreeMap<String, (usize, u128)> = BTreeMap::new();
    let mut pairs: BTreeMap<(String, String), PairTally> = BTreeMap::new();
    let mut per_function: Vec<FunctionCensus> = Vec::new();
    for name in lowered.functions() {
        let index = lowered
            .function_index(name)
            .expect("a pooled function name has an index");
        let pool = lowered.module().functions()[index as usize].constants();
        let placed = lowered
            .placed(name)
            .expect("every planned function has a placed sequence");
        let census = function_census(name, placed, pool, &mut pairs);
        for row in &census.mix {
            let entry = histogram.entry(row.mnemonic.clone()).or_default();
            entry.0 += row.count;
            entry.1 += row.weighted;
        }
        per_function.push(census);
    }
    let static_uops = per_function.iter().map(|f| f.static_uops).sum();
    let weighted_uops = per_function.iter().map(|f| f.weighted_uops).sum();
    let histogram = ranked(histogram);
    let mut shares: BTreeMap<&'static str, u128> = BTreeMap::new();
    for tally in pairs.values() {
        if let Some(family) = tally.family {
            *shares.entry(family).or_default() += tally.weighted;
        }
    }
    let family_shares: Vec<(&'static str, f64)> = FAMILIES
        .iter()
        .map(|family| {
            let weighted = shares.get(family).copied().unwrap_or(0);
            (*family, coverage(weighted, weighted_uops))
        })
        .collect();
    let mut pairs: Vec<PairRow> = pairs
        .into_iter()
        .map(|((first, second), tally)| PairRow {
            coverage: coverage(tally.weighted, weighted_uops),
            family: tally.family,
            first,
            second,
            weighted: tally.weighted,
        })
        .collect();
    pairs.sort_by(|a, b| b.weighted.cmp(&a.weighted).then(a.first.cmp(&b.first)));
    KernelCensus {
        name: stem(path),
        functions: per_function.len(),
        static_uops,
        weighted_uops,
        histogram,
        per_function,
        pairs,
        family_shares,
    }
}

/// Lowers one kernel through the `ds run` chain.
fn compiled(path: &Path) -> Lowered {
    let source = std::fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("reading {}: {error}", path.display()));
    let mut files = Files::new();
    let ambient = files.add_source(ds_stdlib::AMBIENT_FILE, ds_stdlib::AMBIENT_SOURCE);
    let mut asts = vec![parse(&files, ambient).expect("the ambient parses")];
    let id = files.add_source(path.to_string_lossy().into_owned(), &source);
    asts.push(match parse_script(&files, id) {
        Ok(ast) => ast,
        Err(diags) => panic!("{} has syntax errors: {diags:?}", path.display()),
    });
    let analyzed = ds_check::analyze(&files, &asts);
    assert!(
        analyzed.checked.diags.is_empty(),
        "{} does not check clean: {:?}",
        path.display(),
        analyzed.checked.diags
    );
    let effects = ds_check::effects_of(&asts, ds_stdlib::declared_rows(), ds_stdlib::world_caps());
    assert!(
        ds_check::check_platform(&asts, &effects).is_empty(),
        "{} fails the platform check",
        path.display()
    );
    lower(
        &stem(path),
        Hash128::compute(source.as_bytes()),
        &asts,
        &analyzed,
    )
    .unwrap_or_else(|error| panic!("{} does not lower: {error}", path.display()))
}

/// One function's mix, branch structure, and adjacent pairs.
fn function_census(
    name: &str,
    placed: &[PlacedOp],
    pool: &[Constant],
    pairs: &mut BTreeMap<(String, String), PairTally>,
) -> FunctionCensus {
    let stream = stream(placed);
    let blocks = blocks(&stream);
    let bounds = constant_bounds(&stream, pool);
    let loops = natural_loops(&blocks, &stream, &bounds);
    let weights = block_weights(&blocks, &loops);
    let mut mix: BTreeMap<String, (usize, u128)> = BTreeMap::new();
    for (index, block) in blocks.iter().enumerate() {
        let weight = weights[index];
        let mut previous: Option<Uop> = None;
        for op in &stream[block.start..block.end] {
            let StreamOp::Uop(uop) = op else {
                previous = None;
                continue;
            };
            let entry = mix.entry(mnemonic(*uop)).or_default();
            entry.0 += 1;
            entry.1 += weight;
            if let Some(first) = previous {
                let key = (mnemonic(first), mnemonic(*uop));
                let tally = pairs.entry(key).or_insert(PairTally {
                    weighted: 0,
                    family: family(first, *uop),
                });
                tally.weighted += weight;
            }
            previous = Some(*uop);
        }
    }
    let mix = ranked(mix);
    FunctionCensus {
        name: name.to_owned(),
        static_uops: mix.iter().map(|row| row.count).sum(),
        weighted_uops: mix.iter().map(|row| row.weighted).sum(),
        mix,
        blocks: blocks.len(),
        jumps: count_ops(&stream, |uop| matches!(uop, Uop::Jmp { .. })),
        conditionals: count_ops(&stream, |uop| {
            matches!(uop, Uop::Jmpif { .. } | Uop::JmpifNot { .. })
        }),
        loops,
    }
}

/// The placed sequence with the RC words kept as position holders, so an
/// index stays an instruction index (the jump targets' own frame).
fn stream(placed: &[PlacedOp]) -> Vec<StreamOp> {
    placed
        .iter()
        .map(|op| match op {
            PlacedOp::Uop(uop) => StreamOp::Uop(*uop),
            PlacedOp::Rc(_) => StreamOp::Rc,
        })
        .collect()
}

/// One entry of a placed sequence.
#[derive(Clone, Copy)]
enum StreamOp {
    Uop(Uop),
    Rc,
}

/// A basic block: a half-open instruction range plus its successors.
struct Block {
    start: usize,
    end: usize,
    successors: Vec<usize>,
    /// The head block of a back edge ending here, when this block is a latch.
    back_edge: Option<usize>,
}

fn blocks(stream: &[StreamOp]) -> Vec<Block> {
    let mut boundaries: BTreeSet<usize> = BTreeSet::from([0, stream.len()]);
    for (pc, op) in stream.iter().enumerate() {
        let StreamOp::Uop(uop) = op else {
            continue;
        };
        if let Some(target) = jump_target(*uop) {
            boundaries.insert(target);
        }
        if is_terminator(*uop) {
            boundaries.insert(pc + 1);
        }
    }
    let boundaries: Vec<usize> = boundaries.into_iter().collect();
    let mut starts: HashMap<usize, usize> = HashMap::new();
    for (index, start) in boundaries.iter().enumerate() {
        starts.insert(*start, index);
    }
    let mut blocks: Vec<Block> = boundaries
        .windows(2)
        .map(|pair| Block {
            start: pair[0],
            end: pair[1],
            successors: Vec::new(),
            back_edge: None,
        })
        .collect();
    for index in 0..blocks.len() {
        let block = &blocks[index];
        let Some(uop) = stream[block.start..block.end]
            .iter()
            .rev()
            .find_map(|op| match op {
                StreamOp::Uop(uop) => Some(*uop),
                StreamOp::Rc => None,
            })
        else {
            continue;
        };
        let target = jump_target(uop).and_then(|target| starts.get(&target).copied());
        let fallthrough = (index + 1 < blocks.len()).then_some(index + 1);
        let mut successors = Vec::new();
        match uop {
            Uop::Jmp { .. } => successors.extend(target),
            Uop::Jmpif { .. } | Uop::JmpifNot { .. } => {
                successors.extend(target);
                successors.extend(fallthrough);
            },
            _ if !is_terminator(uop) => successors.extend(fallthrough),
            _ => {},
        }
        if let Some(target) = target {
            if matches!(
                uop,
                Uop::Jmp { .. } | Uop::Jmpif { .. } | Uop::JmpifNot { .. }
            ) && blocks[target].start <= block.start
            {
                blocks[index].back_edge = Some(target);
            }
        }
        blocks[index].successors = successors;
    }
    blocks
}

fn jump_target(uop: Uop) -> Option<usize> {
    match uop {
        Uop::Jmp { target } | Uop::Jmpif { target, .. } | Uop::JmpifNot { target, .. } => {
            Some(target as usize)
        },
        _ => None,
    }
}

/// Ops that end a basic block: control leaves the block, or the frame.
fn is_terminator(uop: Uop) -> bool {
    matches!(
        uop,
        Uop::Jmp { .. }
            | Uop::Jmpif { .. }
            | Uop::JmpifNot { .. }
            | Uop::Ret { .. }
            | Uop::RetSpan { .. }
            | Uop::Halt { .. }
            | Uop::Callt { .. }
            | Uop::Calltg { .. }
    )
}

fn count_ops(stream: &[StreamOp], predicate: impl Fn(Uop) -> bool) -> usize {
    stream
        .iter()
        .filter_map(|op| match op {
            StreamOp::Uop(uop) => Some(*uop),
            StreamOp::Rc => None,
        })
        .filter(|uop| predicate(*uop))
        .count()
}

/// Every slot that exactly one kind of `Kimm` writes, with that constant:
/// a loop bound's operand candidate.
fn constant_bounds(stream: &[StreamOp], pool: &[Constant]) -> HashMap<u8, i64> {
    let mut seen: HashMap<u8, Option<i64>> = HashMap::new();
    for op in stream {
        let StreamOp::Uop(uop) = op else {
            continue;
        };
        for &slot in uop.writes().as_slice() {
            let value = match uop {
                Uop::Kimm { kidx, .. } => match pool.get(usize::from(kidx.0)) {
                    Some(Constant::I64(value)) => Some(*value),
                    _ => None,
                },
                _ => None,
            };
            match seen.entry(slot) {
                std::collections::hash_map::Entry::Vacant(entry) => {
                    entry.insert(value);
                },
                std::collections::hash_map::Entry::Occupied(mut entry) => {
                    if *entry.get() != value {
                        entry.insert(None);
                    }
                },
            }
        }
    }
    seen.into_iter()
        .filter_map(|(slot, value)| value.map(|value| (slot, value)))
        .collect()
}

/// The natural loops the back edges span, with each one's trip count.
fn natural_loops(blocks: &[Block], stream: &[StreamOp], bounds: &HashMap<u8, i64>) -> Vec<LoopRow> {
    let mut predecessors: Vec<Vec<usize>> = vec![Vec::new(); blocks.len()];
    for (index, block) in blocks.iter().enumerate() {
        for &successor in &block.successors {
            predecessors[successor].push(index);
        }
    }
    let mut loops = Vec::new();
    for (latch, block) in blocks.iter().enumerate() {
        let Some(head) = block.back_edge else {
            continue;
        };
        let mut members: BTreeSet<usize> = BTreeSet::from([head]);
        let mut pending = vec![latch];
        while let Some(index) = pending.pop() {
            for &predecessor in &predecessors[index] {
                if members.insert(predecessor) {
                    pending.push(predecessor);
                }
            }
        }
        let derived = derived_trip((blocks[head].start, blocks[head].end), stream, bounds);
        loops.push(LoopRow {
            head: blocks[head].start,
            latch: block.start,
            blocks: members.len(),
            trip: derived.unwrap_or(TRIP_PROXY),
            derived: derived.is_some(),
        });
    }
    loops.sort_by_key(|loop_row| loop_row.head);
    loops
}

/// The trip count a loop head's constant comparison implies: the loop tests a
/// slot against a pool constant, so the count is that constant.
fn derived_trip(
    head: (usize, usize),
    stream: &[StreamOp],
    bounds: &HashMap<u8, i64>,
) -> Option<u64> {
    let (start, end) = head;
    let condition = stream[start..end].iter().rev().find_map(|op| match op {
        StreamOp::Uop(Uop::Jmpif { cond, .. } | Uop::JmpifNot { cond, .. }) => Some(*cond),
        _ => None,
    })?;
    let comparand = stream[start..end].iter().find_map(|op| match op {
        StreamOp::Uop(uop) if uop.writes().as_slice().contains(&condition) => match uop.major() {
            Major::IntCmp | Major::FloatCmp | Major::BoolCmp => Some(
                uop.reads()
                    .as_slice()
                    .iter()
                    .filter_map(|slot| bounds.get(slot).copied())
                    .max(),
            ),
            _ => None,
        },
        _ => None,
    })??;
    let trip = u64::try_from(comparand).ok()?;
    (MIN_DERIVED_TRIP..=MAX_DERIVED_TRIP)
        .contains(&comparand)
        .then_some(trip)
}

/// Each block's weight: the product of the trip counts of the loops around it.
fn block_weights(blocks: &[Block], loops: &[LoopRow]) -> Vec<u128> {
    let mut weights = vec![1u128; blocks.len()];
    for loop_row in loops {
        for (index, block) in blocks.iter().enumerate() {
            if block.start >= loop_row.head && block.start <= loop_row.latch {
                weights[index] = weights[index].saturating_mul(u128::from(loop_row.trip));
            }
        }
    }
    weights
}

/// The BL-4 candidate family an adjacent pair falls in, if any.
fn family(first: Uop, second: Uop) -> Option<&'static str> {
    // the comparison majors, tested against one uop at a time: the
    // compare-branch arm reads `first`, the load-compare arm `second`
    // (the load side pins `first` to the Const majors, so testing
    // `first` there never held and real load-then-compare pairs fell
    // out unclassified)
    let compares = |uop: Uop| {
        matches!(
            uop.major(),
            Major::IntCmp | Major::FloatCmp | Major::BoolCmp
        ) || matches!(uop, Uop::EqStr { .. } | Uop::NeqStr { .. })
    };
    let branch = matches!(second, Uop::Jmpif { .. } | Uop::JmpifNot { .. });
    if branch && compares(first) {
        return Some("compare-branch");
    }
    if branch && matches!(first, Uop::TagTest { .. } | Uop::TagTestNot { .. }) {
        return Some("tag-branch");
    }
    let load = matches!(
        first,
        Uop::Kimm { .. } | Uop::Kconst { .. } | Uop::Kstr { .. } | Uop::KstrBorrow { .. }
    );
    if load && compares(second) {
        return Some("load-compare");
    }
    if load && matches!(second.major(), Major::IntArith | Major::FloatArith) {
        return Some("load-op");
    }
    None
}

/// The variant name a histogram joins on.
fn mnemonic(uop: Uop) -> String {
    let debug = format!("{uop:?}");
    debug
        .split([' ', '{'])
        .next()
        .unwrap_or_default()
        .to_owned()
}

fn ranked(counts: BTreeMap<String, (usize, u128)>) -> Vec<Row> {
    let mut rows: Vec<Row> = counts
        .into_iter()
        .map(|(mnemonic, (count, weighted))| Row {
            mnemonic,
            count,
            weighted,
        })
        .collect();
    rows.sort_by(|a, b| {
        b.weighted
            .cmp(&a.weighted)
            .then(b.count.cmp(&a.count))
            .then(a.mnemonic.cmp(&b.mnemonic))
    });
    rows
}

fn coverage(weighted: u128, total: u128) -> f64 {
    if total == 0 {
        return 0.0;
    }
    let share = weighted as f64 / total as f64;
    share * 100.0
}

/// A weighted count as a compact decimal: exact below 10^4, scientific above.
fn compact(count: u128) -> String {
    if count < 10_000 {
        return count.to_string();
    }
    format!("{:.2e}", count as f64)
}

fn print_kernel(census: &KernelCensus) {
    println!(
        "== {} ==\n{} functions, {} static uops, {} weighted op runs\n(weight: functions once, \
         loop trips from constant bounds or the proxy {TRIP_PROXY})",
        census.name,
        census.functions,
        census.static_uops,
        compact(census.weighted_uops),
    );
    println!("static opcode histogram (count, weighted):");
    for row in &census.histogram {
        println!(
            "  {:<14} {:>5}  {:>9}",
            row.mnemonic,
            row.count,
            compact(row.weighted)
        );
    }
    for function in &census.per_function {
        println!(
            "function {}: {} uops, {} weighted runs, {} blocks, {} jumps, {} conditionals",
            function.name,
            function.static_uops,
            compact(function.weighted_uops),
            function.blocks,
            function.jumps,
            function.conditionals
        );
        for loop_row in &function.loops {
            println!(
                "  loop pc {} <- pc {}: {} blocks, trip {}{}",
                loop_row.head,
                loop_row.latch,
                loop_row.blocks,
                loop_row.trip,
                if loop_row.derived { " (constant bound)" } else { " (proxy)" }
            );
        }
        let mix: Vec<String> = function
            .mix
            .iter()
            .map(|row| format!("{} {}", row.mnemonic, row.count))
            .collect();
        println!("  mix: {}", mix.join(", "));
    }
    println!("ranked adjacent pairs (weighted coverage of the kernel stream):");
    for (rank, pair) in census.pairs.iter().take(TOP_PAIRS).enumerate() {
        println!(
            "  {:>2}. {:<26} {:>9}  {:>5.1}%  {}",
            rank + 1,
            format!("{} + {}", pair.first, pair.second),
            compact(pair.weighted),
            pair.coverage,
            pair.family.unwrap_or(""),
        );
    }
    let best: Vec<String> = FAMILIES
        .iter()
        .map(|family| {
            match census
                .pairs
                .iter()
                .find(|pair| pair.family == Some(*family))
            {
                Some(pair) => format!(
                    "{family} {} + {} {:.1}%",
                    pair.first, pair.second, pair.coverage
                ),
                None => format!("{family} none"),
            }
        })
        .collect();
    println!("best pair per family: {}", best.join(", "));
    let shares: Vec<String> = census
        .family_shares
        .iter()
        .map(|(family, share)| format!("{family} {share:.1}%"))
        .collect();
    println!(
        "family shares (every adjacent pair's weight): {}",
        shares.join(", ")
    );
    println!();
}

fn print_summary(censuses: &[KernelCensus]) {
    println!("== summary: weighted share of each kernel's stream ==");
    print!(
        "{:<12} {:<26} {:>6}",
        "kernel", "top adjacent pair", "share"
    );
    for family in FAMILIES {
        print!(" {family:>15}");
    }
    println!();
    for census in censuses {
        let top = census
            .pairs
            .first()
            .map(|pair| (format!("{} + {}", pair.first, pair.second), pair.coverage));
        let (pair, share) = top.unwrap_or_else(|| (String::new(), 0.0));
        print!("{:<12} {pair:<26} {share:>5.1}%", census.name);
        for (_, family_share) in &census.family_shares {
            print!(" {family_share:>14.1}%");
        }
        println!();
        for pair in census.pairs.iter().skip(1).take(SUMMARY_PAIRS - 1) {
            println!(
                "{:<12} {:<26} {:>5.1}%",
                "",
                format!("{} + {}", pair.first, pair.second),
                pair.coverage,
            );
        }
    }
}
