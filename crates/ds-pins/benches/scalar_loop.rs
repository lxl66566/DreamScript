//! Gungraun instruction-count pin over the interpreter's hot scalar
//! dispatch: one 5M-iteration Lehmer
//! loop — mul, mod, a parity branch, an add — the byte-table dispatch's
//! absolute instruction count is the regression pin for the layout
//! (the pre-layout double-match baseline for this kernel shape retired
//! ~35.4G instructions process-wide; the placement of this pin measures
//! the dispatch loop alone, so arm additions that re-lower neighbors or
//! leak calls back into the per-op path move it immediately). The
//! registry build and the VM setup are deterministic and sit far below
//! the loop's instruction mass, so the count is stable run to run —
//! Callgrind is immune to machine noise.

// the attribute macro's expansion carries qualified std paths, which the
// workspace's unused_qualifications lint otherwise rejects
#![allow(unused_qualifications)]

use std::hint::black_box;

use ds_vm::{
    Const, FunctionDef, ModuleRegistry, ModuleVersion, Value, Vm,
    abi::{ConstIdx, FrameLayout, Inst, SlotType, uop::Uop},
    assemble,
};
use gungraun::prelude::*;

const ITERATIONS: i64 = 5_000_000;

fn k(idx: u16) -> ConstIdx {
    ConstIdx(idx)
}

fn def(name: &str, slots: &[SlotType], body: &[Inst]) -> FunctionDef {
    FunctionDef {
        name: name.into(),
        layout: FrameLayout::new(slots.to_vec(), 0, 0).expect("valid fixture layout"),
        pool: Vec::new(),
        code: assemble(body).expect("fixture body encodes"),
    }
}

/// The kernel: `x = 1; acc = 0; i = 0; while i < N { x = (x * 48271) %
/// 2147483647; if x % 2 == 0 { acc += x % 97 } else { acc += x % 89 }; i
/// += 1 }` — the scalar-arm hot path with one branch per iteration.
fn registry() -> ModuleRegistry {
    let mut body: Vec<Inst> = vec![Uop::Funcf { frame_slots: 10 }.into()];
    body.push(Uop::Kimm { dst: 0, kidx: k(0) }.into()); // x = 1
    body.push(Uop::Kimm { dst: 1, kidx: k(1) }.into()); // acc = 0
    body.push(Uop::Kimm { dst: 2, kidx: k(1) }.into()); // i = 0
    body.push(Uop::Kimm { dst: 3, kidx: k(2) }.into()); // 48271
    body.push(Uop::Kimm { dst: 4, kidx: k(3) }.into()); // 2147483647
    body.push(Uop::Kimm { dst: 5, kidx: k(0) }.into()); // one
    body.push(Uop::Kimm { dst: 6, kidx: k(5) }.into()); // N
    body.push(Uop::Kimm { dst: 9, kidx: k(1) }.into()); // zero
    let head = body.len();
    body.push(
        Uop::MulI64 {
            dst: 7,
            lhs: 0,
            rhs: 3,
        }
        .into(),
    ); // t = x * 48271
    body.push(
        Uop::ModI64 {
            dst: 0,
            lhs: 7,
            rhs: 4,
        }
        .into(),
    ); // x = t % p
    body.push(Uop::Kimm { dst: 7, kidx: k(6) }.into()); // two
    body.push(
        Uop::ModI64 {
            dst: 7,
            lhs: 0,
            rhs: 7,
        }
        .into(),
    ); // t = x % 2
    body.push(
        Uop::EqI64 {
            dst: 8,
            lhs: 7,
            rhs: 9,
        }
        .into(),
    ); // cond = t == 0
    body.push(
        Uop::JmpifNot {
            cond: 8,
            target: u32::try_from(head + 10).expect("target fits the u32 word"),
        }
        .into(),
    );
    body.push(Uop::Kimm { dst: 7, kidx: k(7) }.into()); // 97
    body.push(
        Uop::ModI64 {
            dst: 7,
            lhs: 0,
            rhs: 7,
        }
        .into(),
    ); // t = x % 97
    body.push(
        Uop::AddI64 {
            dst: 1,
            lhs: 1,
            rhs: 7,
        }
        .into(),
    ); // acc += t
    body.push(
        Uop::Jmp {
            target: u32::try_from(head + 13).expect("target fits the u32 word"),
        }
        .into(),
    );
    body.push(Uop::Kimm { dst: 7, kidx: k(8) }.into()); // 89
    body.push(
        Uop::ModI64 {
            dst: 7,
            lhs: 0,
            rhs: 7,
        }
        .into(),
    ); // t = x % 89
    body.push(
        Uop::AddI64 {
            dst: 1,
            lhs: 1,
            rhs: 7,
        }
        .into(),
    ); // acc += t
    body.push(
        Uop::AddI64 {
            dst: 7,
            lhs: 2,
            rhs: 5,
        }
        .into(),
    ); // t = i + 1
    body.push(Uop::Mov { dst: 2, src: 7 }.into()); // i = t
    body.push(
        Uop::LtI64 {
            dst: 8,
            lhs: 2,
            rhs: 6,
        }
        .into(),
    ); // cond = i < N
    body.push(
        Uop::Jmpif {
            cond: 8,
            target: u32::try_from(head).expect("target fits the u32 word"),
        }
        .into(),
    );
    body.push(Uop::Ret { src: 1 }.into());

    let mut main = def(
        "main",
        &[
            SlotType::I64,
            SlotType::I64,
            SlotType::I64,
            SlotType::I64,
            SlotType::I64,
            SlotType::I64,
            SlotType::I64,
            SlotType::I64,
            SlotType::Bool,
            SlotType::I64,
        ],
        &body,
    );
    main.pool = vec![
        Const::I64(1),
        Const::I64(0),
        Const::I64(48271),
        Const::I64(2_147_483_647),
        Const::I64(1),
        Const::I64(ITERATIONS),
        Const::I64(2),
        Const::I64(97),
        Const::I64(89),
    ];
    let mut reg = ModuleRegistry::new();
    reg.install(ModuleVersion::new("scalar_loop", vec![main]).expect("valid fixture module"));
    reg
}

pub(crate) fn drive() -> Value {
    let mut vm = Vm::new(registry());
    let answer = vm.run("scalar_loop", "main", &[]).expect("the kernel runs");
    let [got] = answer.as_slice() else {
        panic!("the kernel answers one word");
    };
    assert_eq!(
        got,
        &Value::I64(230_047_953),
        "the Lehmer loop accumulates the pinned sum"
    );
    assert!(
        vm.objects().is_empty(),
        "the kernel claims nothing: {} live",
        vm.objects().len()
    );
    *got
}

#[library_benchmark]
fn scalar_dispatch() -> Value {
    black_box(drive())
}

library_benchmark_group!(name = scalar_loop, benchmarks = [scalar_dispatch]);

main!(library_benchmark_groups = scalar_loop);
