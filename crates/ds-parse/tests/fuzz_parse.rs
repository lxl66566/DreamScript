//! Randomized parser property, the nextest-runnable stand-in for the
//! cargo-fuzz target `fuzz_parse`: the scanner and grammar never panic on
//! arbitrary bytes, valid UTF-8 or not. Both arms are consumed so coverage
//! counts accepted and rejected inputs alike.

use ds_diag::{ErrorCode, Files};
use ds_parse::{parse, parse_script};

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

/// Bracket nesting around and past the scanner's depth guard (40):
/// random bytes almost never build deep nesting, so the guard's
/// diagnostic path needs these. Below the limit the balanced shapes
/// parse; at and past it the guard reports instead of overflowing the
/// parse stack. Truncated variants cover the two-span diagnostic.
#[test]
fn nested_delimiters_never_panic() {
    let balanced = |open: &str, close: &str, depth: usize| {
        format!(
            "fn f() -> int {{\n  {}1{}\n}}\n",
            open.repeat(depth),
            close.repeat(depth),
        )
    };
    for depth in [1usize, 8, 39, 40, 41, 200] {
        for (open, close) in [("(", ")"), ("[", "]"), ("{", "}")] {
            let source = balanced(open, close, depth);
            property(source.as_bytes());
            let mut truncated = source.clone();
            truncated.truncate(truncated.len() / 2);
            property(truncated.as_bytes());
        }
    }
    // Interpolation holes nest inside strings; each hole is a frame.
    for holes in [1usize, 3, 20, 60] {
        let mut source = String::from("fn f() -> string {\n  \"");
        for _ in 0..holes {
            source.push_str("${1 + ");
        }
        source.push('1');
        for _ in 0..holes {
            source.push_str(" + 2}\"");
        }
        source.push_str("\n}\n");
        property(source.as_bytes());
        let mut truncated = source.clone();
        truncated.truncate(truncated.len() / 2);
        property(truncated.as_bytes());
    }
}

/// Lambda arrow chains around and past the scanner's arrow budget — the
/// grammar's bracket-free recursion class, where a chain long enough
/// would otherwise walk the parse worker off its stack (an abort, not a
/// panic, so surviving in-process is the property). Shapes that nest
/// without brackets through other spellings are included: parameter-list
/// lambdas, `else if` heads, `in` right-hand sides, interpolation holes,
/// and arrows interleaved with counted brackets. Random bytes almost
/// never build chains, so the budget's refusal path needs these.
#[test]
fn arrow_chains_never_panic() {
    use std::fmt::Write as _;

    let wrap = |body: String, ret: &str| format!("fn f() -> {ret} {{\n  {body}\n}}\n");
    let bare = |n: usize| wrap(format!("{}1", "x => ".repeat(n)), "int");
    let paren = |n: usize| {
        let mut links = String::new();
        for k in 0..n {
            let _ = write!(links, "(a{k}) => ");
        }
        wrap(format!("{links}1"), "int")
    };
    let else_if = |n: usize| {
        let mut s = String::from("x0 => if c { 1 }");
        for k in 1..n {
            let _ = write!(s, "\nelse if x{k} => {{ 1 }} {{ 2 }}");
        }
        s.push_str("\nelse { 0 }");
        wrap(s, "int")
    };
    let in_chain = |n: usize| {
        let mut s = String::new();
        for k in 0..n {
            let _ = writeln!(s, "x{k} => y{k} in");
        }
        s.push('z');
        wrap(s, "bool")
    };
    let hole = |n: usize| wrap(format!("\"${{{}1}}\"", "x => ".repeat(n)), "string");
    let mixed = |n: usize| {
        wrap(
            format!("{}{}1{}", "((".repeat(2), "x => ".repeat(n), "))".repeat(2)),
            "int",
        )
    };
    for n in [1usize, 8, 20, 38, 39, 40, 41, 200, 2000] {
        for source in [
            bare(n),
            paren(n),
            else_if(n),
            in_chain(n),
            hole(n),
            mixed(n),
        ] {
            property(source.as_bytes());
            let mut truncated = source.clone();
            truncated.truncate(truncated.len() / 2);
            property(truncated.as_bytes());
        }
    }
    // An over-budget chain is refused as a diagnostic end to end.
    let source = bare(200);
    let mut files = Files::new();
    let file = files.add_source("deep.ds", &source);
    let diags = parse(&files, file).expect_err("the budget refuses");
    assert_eq!(diags[0].code, ErrorCode::ArrowChainTooDeep);
    // The budget's reset rules are cut against the grammar's sp-crossing,
    // so the continuation shapes the resets must not break really parse.
    let parses = |src: &str| {
        let mut files = Files::new();
        let file = files.add_source("cont.ds", src);
        parse_script(&files, file).expect("a legal continuation");
    };
    parses("x =>\ny => z\n");
    parses("x => y +\nz => w\n");
    parses("x => y in\nz => w\n");
    parses("if c { 1 } else\nif x => { 2 } { 3 } else { 0 }\n");
}

/// `else if` chains around and past the scanner's else budget — the
/// second bracket-free recursion class (`if_expr` right-recurses through
/// its else clause after the block has closed). Below the limit real
/// dispatch shapes parse; at and past it the pre-scan reports instead of
/// walking the worker's stack to pest's own backstop.
#[test]
fn else_if_chains_never_panic() {
    use std::fmt::Write as _;

    let chain = |elses: usize| {
        let mut s = String::from("fn f(c: int) -> int {\n  if c == 0 { 0 }");
        for k in 1..elses {
            let _ = writeln!(s, "\nelse if c == {k} {{ {k} }}");
        }
        if elses > 0 {
            s.push_str("\nelse { 0 }");
        }
        s.push_str("\n}\n");
        s
    };
    for elses in [0usize, 1, 100, 119, 499, 500, 599, 2000, 20000] {
        let source = chain(elses);
        property(source.as_bytes());
        let mut truncated = source.clone();
        truncated.truncate(truncated.len() / 2);
        property(truncated.as_bytes());
    }
    // An over-budget chain is refused as a diagnostic end to end.
    let source = chain(2000);
    let mut files = Files::new();
    let file = files.add_source("deep.ds", &source);
    let diags = parse(&files, file).expect_err("the budget refuses");
    assert_eq!(diags[0].code, ErrorCode::ElseChainTooDeep);
    // A real generated decoder's dispatch (120 branches) parses clean.
    let mut files = Files::new();
    let file = files.add_source("dispatch.ds", &chain(119));
    parse_script(&files, file).expect("a 120-branch dispatch parses");
}
