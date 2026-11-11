// Writes valid .dsmod seeds into fuzz/corpus/fuzz_read_dsmod so the
// fuzz_read_dsmod campaign can reach past the header checks into the
// accepted-module path (the write(read(x)) == x assert). The module
// builder mirrors crates/ds-mod/tests/fuzz_read.rs with a plain xorshift
// RNG instead of arbitrary, so the fuzz package needs no extra deps.
// Usage: cargo run --manifest-path fuzz/Cargo.toml --bin gen_dsmod_seeds

use ds_abi::{
    FrameLayout, SlotType,
    uop::{ConstIdx, Uop},
};
use ds_mod::{Constant, Flags, Function, Hash128, Module, TargetClass, write};

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

fn random_string(rng: &mut Rng, max_len: u8) -> String {
    let len = usize::from(rng.byte() % (max_len + 1));
    (0..len)
        .map(|_| char::from(b'a' + rng.byte() % 26))
        .collect()
}

fn random_hash(rng: &mut Rng) -> Hash128 {
    let mut bytes = [0; 16];
    for b in &mut bytes {
        *b = rng.byte();
    }
    Hash128::new(bytes)
}

fn random_constant(rng: &mut Rng) -> Constant {
    match rng.byte() % 5 {
        0 => Constant::I64(rng.next_u64() as i64),
        1 => Constant::F64(f64::from_bits(rng.next_u64())),
        2 => Constant::Bool(rng.byte() & 1 == 1),
        3 => Constant::Str(random_string(rng, 16)),
        _ => Constant::FuncRef {
            module: random_string(rng, 8),
            func: rng.next_u64() as u32,
        },
    }
}

fn random_uop(rng: &mut Rng, code_len: usize) -> Uop {
    let code_len = u32::try_from(code_len).expect("fits u32");
    let slot = |rng: &mut Rng| rng.byte();
    match rng.byte() % 9 {
        0 => Uop::Kimm {
            dst: slot(rng),
            kidx: ConstIdx(rng.next_u64() as u16),
        },
        1 => Uop::AddI64 {
            dst: slot(rng),
            lhs: slot(rng),
            rhs: slot(rng),
        },
        2 => Uop::Mov {
            dst: slot(rng),
            src: slot(rng),
        },
        3 => Uop::Ret { src: slot(rng) },
        4 => Uop::Halt { slot: slot(rng) },
        // frame_slots must stay at or below ds-abi's 256-slot frame cap:
        // arbitrary u16s would fail Function::new's encode validation.
        5 => Uop::Funcf {
            frame_slots: u16::from(rng.byte()),
        },
        6 => Uop::Jmp {
            target: u32::from(slot(rng)) % code_len,
        },
        7 => Uop::Jmpif {
            cond: slot(rng),
            target: u32::from(slot(rng)) % code_len,
        },
        _ => Uop::Callt {
            callee: ConstIdx(rng.next_u64() as u16),
            nargs: slot(rng),
        },
    }
}

fn random_function(rng: &mut Rng) -> Function {
    let slot_count = usize::from(rng.byte() % 9);
    let mut slots = Vec::with_capacity(slot_count);
    for _ in 0..slot_count {
        slots.push(match rng.byte() % 6 {
            0 => SlotType::I32,
            1 => SlotType::I64,
            2 => SlotType::F64,
            3 => SlotType::Bool,
            4 => SlotType::Ref,
            _ => SlotType::Tag,
        });
    }
    let params_len = usize::from(rng.byte()) % (slot_count + 1);
    let arg_area_len = usize::from(rng.byte()) % (slot_count - params_len + 1);
    let layout =
        FrameLayout::new(slots, params_len, arg_area_len).expect("consistent by construction");
    let mut constants = Vec::new();
    for _ in 0..rng.byte() % 5 {
        constants.push(random_constant(rng));
    }
    let code_len = usize::from(rng.byte() % 15) + 1;
    let code: Vec<Uop> = (0..code_len).map(|_| random_uop(rng, code_len)).collect();
    Function::new(random_string(rng, 10), layout, constants, code)
        .expect("generator produces valid functions")
}

fn random_module(rng: &mut Rng) -> Module {
    let name = random_string(rng, 12);
    let target = match rng.byte() % 3 {
        0 => TargetClass::Bytecode,
        1 => TargetClass::Wasm,
        _ => TargetClass::Triple(random_string(rng, 24)),
    };
    let mut module = Module::new(name, target, random_hash(rng));
    let bits = rng.byte() & Flags::KNOWN_BITS;
    module = module.with_flags(Flags::from_bits(bits).expect("masked to known bits"));
    for _ in 0..rng.byte() % 5 {
        module = module.with_dep(random_string(rng, 8), random_hash(rng));
    }
    for _ in 0..rng.byte() % 9 {
        module = module.with_constant(random_constant(rng));
    }
    for _ in 0..rng.byte() % 3 {
        module = module.with_function(random_function(rng));
    }
    for _ in 0..rng.byte() % 9 {
        let (a, b) = (rng.next_u64() as u32, rng.next_u64() as u32);
        let (start, end) = if a <= b { (a, b) } else { (b, a) };
        module = module.with_debug_span(start, end);
    }
    module
}

fn main() {
    let out_dir = std::path::Path::new("corpus/fuzz_read_dsmod");
    std::fs::create_dir_all(out_dir).expect("create corpus dir");
    for seed in 1..=48u64 {
        let mut rng = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1);
        let bytes = write(&random_module(&mut rng));
        let path = out_dir.join(format!("seed-{seed:02}.dsmod"));
        std::fs::write(&path, bytes).expect("write seed");
        println!("wrote {}", path.display());
    }
}
