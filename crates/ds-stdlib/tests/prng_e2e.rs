//! The seeded PRNG's output pin: the @std bodied members (`rand`,
//! `rand_next`, `rand_seed`) draw the splitmix64 stream, so the probe
//! prints a fixed-seed run over the merged ambient and the gate
//! compares the lines against an independent u64 reference — the
//! i64 spellings of the mixer's constants and the sign-propagating
//! shift's masked emulations must answer the exact unsigned values.
//! `rand_seed` steps the wall clock, so its half pins execution alone
//! (the answer runs and parses, never its value).

use ds_check::analyze;
use ds_diag::{Files, human};
use ds_lower::lower;
use ds_mod::{Hash128, read, write};
use ds_parse::parse;
use ds_stdlib::{AMBIENT_FILE, AMBIENT_SOURCE};
use ds_vm::{FrameAnswer, ModuleRegistry, Platform, Value, Vm, loader, native};

/// The splitmix64 constants, u64 spelling (the ds bodies carry their
/// i64 spellings).
const GOLDEN: u64 = 0x9e37_79b9_7f4a_7c15;
const MIX_A: u64 = 0xbf58_476d_1ce4_e5b9;
const MIX_B: u64 = 0x94d0_49bb_1331_11eb;

/// The draw following one stream state (the mix over the advanced
/// state), the reference the ds `rand` must answer.
fn reference(state: u64) -> u64 {
    let mut z = state.wrapping_add(GOLDEN);
    z = (z ^ (z >> 30)).wrapping_mul(MIX_A);
    z = (z ^ (z >> 27)).wrapping_mul(MIX_B);
    z ^ (z >> 31)
}

/// The first `count` draws from one seed, signed renderings: the
/// stream advances by one wrapping add per draw, exactly the
/// `s = rand_next(s)` the probe spells. The u64-to-i64 cast is the
/// bit-identity reinterpretation the ds body answers.
#[allow(clippy::cast_possible_wrap)]
fn draws(seed: u64, count: usize) -> Vec<String> {
    let mut state = seed;
    (0..count)
        .map(|_| {
            let out = (reference(state) as i64).to_string();
            state = state.wrapping_add(GOLDEN);
            out
        })
        .collect()
}

/// Parse -> check -> lower -> round-trip -> run over the merged
/// ambient, answering the captured stdout of `main`.
fn runs(name: &str, probe: &str) -> String {
    let mut files = Files::new();
    let ambient = files.add_source(AMBIENT_FILE, AMBIENT_SOURCE);
    let id = files.add_source("probe.ds", probe);
    let asts = [
        parse(&files, ambient).expect("the ambient parses"),
        parse(&files, id).expect("the probe parses"),
    ];
    let analyzed = analyze(&files, &asts);
    assert!(
        analyzed.checked.diags.is_empty(),
        "{}",
        analyzed
            .checked
            .diags
            .iter()
            .map(|diag| human(diag, &files))
            .collect::<String>()
    );
    let lowered = lower(name, Hash128::compute(probe.as_bytes()), &asts, &analyzed)
        .unwrap_or_else(|err| panic!("the probe lowers: {err}"));
    let bytes = write(lowered.module());
    let module = read(&bytes).expect("the module round-trips");
    let mut registry = ModuleRegistry::new();
    registry.install_native(native::BUILTIN_MODULE, native::builtin_module());
    registry.install_platform(Platform::std());
    loader::install(&mut registry, module).expect("the module installs");
    let mut vm = Vm::new(registry);
    vm.capture_output();
    assert_eq!(
        vm.run(name, "main", &[]),
        Ok(FrameAnswer::one(Value::I64(0))),
        "the unit entry result"
    );
    String::from_utf8_lossy(vm.captured_output()).into_owned()
}

/// The stream from a fixed seed answers the reference values: five
/// draws from state 0 and three from seed 42, each line the signed
/// rendering of the u64 reference word.
#[test]
fn the_fixed_seed_stream_answers_the_reference_values() {
    let probe = concat!(
        "fn main() -> int {\n",
        "  var s = 0\n",
        "  var i = 0\n",
        "  while i < 5 {\n",
        "    println(\"${rand(s)}\")\n",
        "    s = rand_next(s)\n",
        "    i += 1\n",
        "  }\n",
        "  var t = 42\n",
        "  var j = 0\n",
        "  while j < 3 {\n",
        "    println(\"${rand(t)}\")\n",
        "    t = rand_next(t)\n",
        "    j += 1\n",
        "  }\n",
        "  0\n",
        "}\n",
    );
    let mut expected = draws(0, 5);
    expected.extend(draws(42, 3));
    let out = runs("prng_probe", probe);
    assert_eq!(
        out,
        format!("{}\n", expected.join("\n")),
        "the fixed-seed stream answers the splitmix64 reference"
    );
    // the absolute pin: the first draw from state 0, so a future
    // constant edit cannot keep the gate green through a matching
    // reference drift
    assert_eq!(expected[0], "-2152535657050944081");
}

/// The time-stepped seed runs: `rand_seed` answers a usable state —
/// the draw over it executes and renders an int, the value itself the
/// wall clock's (never pinned).
#[test]
fn the_time_stepped_seed_runs() {
    let probe = concat!(
        "fn main() -> int {\n",
        "  println(\"${rand(rand_seed())}\")\n",
        "  0\n",
        "}\n",
    );
    let out = runs("prng_time_probe", probe);
    let line = out.trim();
    assert!(
        line.parse::<i64>().is_ok(),
        "the time-stepped draw renders an int: {out}"
    );
}
