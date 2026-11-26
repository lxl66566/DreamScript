//! Randomized reader property, the nextest-runnable stand-in for the
//! cargo-fuzz target `fuzz_read_dsmod`: the reader never panics on any
//! byte input, and every module it accepts re-serializes to the identical
//! bytes (canonical-encoding enforcement).

use arbitrary::{Arbitrary, Unstructured};
use ds_abi::{
    FrameLayout, SlotType,
    uop::{ConstIdx, Uop},
};
use ds_mod::{Constant, Flags, FuncRefKey, Function, Hash128, Module, TargetClass, read, write};

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

    fn below(&mut self, bound: usize) -> usize {
        let bound = u64::try_from(bound).expect("bound fits u64").max(1);
        usize::try_from(self.next_u64() % bound).expect("remainder fits usize")
    }

    fn byte(&mut self) -> u8 {
        u8::try_from(self.next_u64() >> 56).expect("top 8 bits fit u8")
    }
}

fn rng_bytes(seed: u64, len: usize) -> Vec<u8> {
    let mut rng = Rng(seed | 1);
    (0..len).map(|_| rng.byte()).collect()
}

/// The never-panic property plus byte-stability of accepted inputs.
fn property(bytes: &[u8]) {
    if let Ok(module) = read(bytes) {
        assert_eq!(
            write(&module),
            bytes,
            "an accepted module must re-serialize to identical bytes"
        );
    }
}

/// Case count for `mutated_valid_modules_never_panic`; a CI fuzz job can
/// crank it with `FUZZ_RUNS=<n> cargo nextest run`. Invalid or unset values
/// keep the default.
fn fuzz_runs() -> u64 {
    std::env::var("FUZZ_RUNS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(240)
}

#[test]
fn random_bytes_never_panic() {
    for seed in 0..96u64 {
        let len = usize::try_from(seed % 16).expect("fits") * 71; // header-scale inputs
        property(&rng_bytes(seed, len));
    }
}

/// Every committed seed of the cargo-fuzz corpus holds the property. Only
/// `*.dsmod` files are seeds; extension-less files in the directory are
/// local campaign outputs and are skipped so the run stays deterministic.
#[test]
fn corpus_seeds_never_panic_and_stay_canonical() {
    const DIR: &str = "../../fuzz/corpus/fuzz_read_dsmod";
    let mut seeds: Vec<_> = std::fs::read_dir(DIR)
        .unwrap_or_else(|err| panic!("{DIR}: {err}"))
        .map(|entry| entry.expect("readable dir entry").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "dsmod"))
        .collect();
    seeds.sort();
    assert!(
        seeds.len() >= 49,
        "the committed seed corpus must be present (found {})",
        seeds.len()
    );
    for path in seeds {
        let bytes = std::fs::read(&path).unwrap_or_else(|err| panic!("{}: {err}", path.display()));
        property(&bytes);
    }
}

#[test]
fn mutated_valid_modules_never_panic() {
    for seed in 1..=fuzz_runs() {
        let noise = rng_bytes(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15), 4096);
        let mut unstructured = Unstructured::new(&noise);
        let base = write(&random_module(&mut unstructured));
        property(&base);
        let mut rng = Rng(seed ^ 0xa5a5_a5a5_a5a5_a5a5);
        for _ in 0..8 {
            let mut mutated = base.clone();
            match rng.below(3) {
                0 => {
                    let at = rng.below(mutated.len());
                    let shift = rng.below(8);
                    mutated[at] ^= 1u8 << shift;
                },
                1 => {
                    mutated.truncate(rng.below(mutated.len()));
                },
                _ => {
                    let at = rng.below(mutated.len());
                    mutated.insert(at, rng.byte());
                },
            }
            property(&mutated);
        }
    }
}

fn random_module(u: &mut Unstructured) -> Module {
    let name = random_string(u, 12);
    let target = match u8::arbitrary(u).unwrap_or(0) % 3 {
        0 => TargetClass::Bytecode,
        1 => TargetClass::Wasm,
        _ => TargetClass::Triple(random_string(u, 24)),
    };
    let mut module = Module::new(name, target, random_hash(u));
    let bits = u8::arbitrary(u).unwrap_or(0) & Flags::KNOWN_BITS;
    module = module.with_flags(Flags::from_bits(bits).expect("masked to known bits"));
    let mut dep_count = 0usize;
    for _ in 0..u8::arbitrary(u).unwrap_or(0) % 5 {
        module = module.with_dep(random_string(u, 8), random_hash(u));
        dep_count += 1;
    }
    for _ in 0..u8::arbitrary(u).unwrap_or(0) % 9 {
        module = module.with_constant(random_constant(u, dep_count));
    }
    // Random-but-valid function bodies: full-validity random uops over the
    // whole set is a later fuzz target.
    for _ in 0..u8::arbitrary(u).unwrap_or(0) % 3 {
        module = module.with_function(random_function(u, dep_count));
    }
    // The blocks payload's tag table: dense first-use ids by construction,
    // the declaring declaration's name beside the label.
    for id in 0..u8::arbitrary(u).unwrap_or(0) % 5 {
        module = module.with_tag(random_string(u, 10), random_string(u, 10), id);
    }
    for _ in 0..u8::arbitrary(u).unwrap_or(0) % 9 {
        let (a, b) = (
            u32::arbitrary(u).unwrap_or(0),
            u32::arbitrary(u).unwrap_or(0),
        );
        let (start, end) = if a <= b { (a, b) } else { (b, a) };
        module = module.with_debug_span(start, end);
    }
    module
}

fn random_constant(u: &mut Unstructured, dep_count: usize) -> Constant {
    match u8::arbitrary(u).unwrap_or(0) % 6 {
        0 => Constant::I64(i64::arbitrary(u).unwrap_or(0)),
        1 => Constant::F64(f64::from_bits(u64::arbitrary(u).unwrap_or(0))),
        2 => Constant::Bool(bool::arbitrary(u).unwrap_or(false)),
        3 => Constant::Str(random_string(u, 16)),
        4 => Constant::FuncRef {
            // a position the header's dep list can address: 0 the
            // enclosing module, n the n-1-th entry
            key: FuncRefKey::Dep(u32::from(
                u8::arbitrary(u).unwrap_or(0) % u8::try_from(dep_count + 1).expect("few deps"),
            )),
            func: u32::arbitrary(u).unwrap_or(0),
        },
        _ => Constant::FuncRef {
            key: FuncRefKey::Native(random_string(u, 8)),
            func: u32::arbitrary(u).unwrap_or(0),
        },
    }
}

/// A function whose body is always constructor-valid: a small uop subset
/// with jump targets drawn below the body length.
fn random_function(u: &mut Unstructured, dep_count: usize) -> Function {
    let slot_count = usize::from(u8::arbitrary(u).unwrap_or(0) % 9);
    let mut slots = Vec::with_capacity(slot_count);
    for _ in 0..slot_count {
        slots.push(match u8::arbitrary(u).unwrap_or(0) % 6 {
            0 => SlotType::I32,
            1 => SlotType::I64,
            2 => SlotType::F64,
            3 => SlotType::Bool,
            4 => SlotType::Ref,
            _ => SlotType::Tag,
        });
    }
    let params_len = usize::from(u8::arbitrary(u).unwrap_or(0)) % (slot_count + 1);
    let arg_area_len = usize::from(u8::arbitrary(u).unwrap_or(0)) % (slot_count - params_len + 1);
    let layout =
        FrameLayout::new(slots, params_len, arg_area_len).expect("consistent by construction");
    let mut constants = Vec::new();
    for _ in 0..u8::arbitrary(u).unwrap_or(0) % 5 {
        constants.push(random_constant(u, dep_count));
    }
    let code_len = usize::from(u8::arbitrary(u).unwrap_or(0) % 15) + 1;
    let mut code = Vec::with_capacity(code_len);
    for _ in 0..code_len {
        code.push(random_uop(u, code_len));
    }
    Function::new(random_string(u, 10), layout, constants, code)
        .expect("generator produces valid functions")
}

fn random_uop(u: &mut Unstructured, code_len: usize) -> Uop {
    let code_len = u32::try_from(code_len).expect("fits u32");
    let slot = |u: &mut Unstructured| u8::arbitrary(u).unwrap_or(0);
    match u8::arbitrary(u).unwrap_or(0) % 9 {
        0 => Uop::Kimm {
            dst: slot(u),
            kidx: ConstIdx(u16::arbitrary(u).unwrap_or(0)),
        },
        1 => Uop::AddI64 {
            dst: slot(u),
            lhs: slot(u),
            rhs: slot(u),
        },
        2 => Uop::Mov {
            dst: slot(u),
            src: slot(u),
        },
        3 => Uop::Ret { src: slot(u) },
        4 => Uop::Halt { slot: slot(u) },
        // frame_slots must stay at or below ds-abi's 256-slot frame cap:
        // arbitrary u16s would fail Function::new's encode validation.
        5 => Uop::Funcf {
            frame_slots: u16::from(u8::arbitrary(u).unwrap_or(0)),
        },
        6 => Uop::Jmp {
            target: u32::from(slot(u)) % code_len,
        },
        7 => Uop::Jmpif {
            cond: slot(u),
            target: u32::from(slot(u)) % code_len,
        },
        _ => Uop::Callt {
            callee: ConstIdx(u16::arbitrary(u).unwrap_or(0)),
            nargs: slot(u),
        },
    }
}

fn random_string(u: &mut Unstructured, max_len: u8) -> String {
    let len = usize::from(u8::arbitrary(u).unwrap_or(0) % (max_len + 1));
    let mut bytes = Vec::with_capacity(len);
    for _ in 0..len {
        bytes.push(b'a'.wrapping_add(u8::arbitrary(u).unwrap_or(0) % 26));
    }
    String::from_utf8(bytes).expect("ascii")
}

fn random_hash(u: &mut Unstructured) -> Hash128 {
    Hash128::new(<[u8; 16]>::arbitrary(u).unwrap_or_default())
}
