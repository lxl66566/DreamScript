//! Gungraun instruction-count pin over a string-temporary kernel (the
//! arc's stage-three gate): one hot loop whose body builds two string
//! temporaries and compares them, once per iteration. The two ids are
//! the same kernel under the two load spellings — `claiming` is the
//! pre-slice emission (KSTR acquires plus the placed terminal RCDEC
//! per load, the claim ledger riding each acquire), `borrowing` is
//! stage three's KSTR_BORROW (no count, no claim, no dec) — so the Ir
//! delta between the ids IS the per-iteration cost the slice deleted,
//! deterministically, and the borrowing id's absolute count is the
//! regression pin for the dispatch arm and the intern-hit path.
//!
//! Fixture builders are duplicated from tests/fixtures.rs on purpose
//! (the criterion bench's note): benches link ds-vm as an external
//! crate, and the builders are small and stable. The registry build and
//! the VM setup inside the bench fn are deterministic and a few orders
//! below the loop's instruction mass (5M iterations), so the counts are
//! stable run to run — Callgrind is immune to machine noise.

// the attribute macro's expansion carries qualified std paths, which the
// workspace's unused_qualifications lint otherwise rejects
#![allow(unused_qualifications)]

use std::hint::black_box;

use ds_vm::{
    Const, FunctionDef, ModuleRegistry, ModuleVersion, Value, Vm,
    abi::{ConstIdx, FrameLayout, Inst, RcOp, SlotType, uop::Uop},
    assemble,
};
use gungraun::prelude::*;

const ITERATIONS: i64 = 5_000_000;

fn k(idx: u16) -> ConstIdx {
    ConstIdx(idx)
}

fn def(name: &str, slots: &[SlotType], params: usize, args: usize, body: &[Inst]) -> FunctionDef {
    FunctionDef {
        name: name.into(),
        layout: FrameLayout::new(slots.to_vec(), params, args).expect("valid fixture layout"),
        pool: Vec::new(),
        code: assemble(body).expect("fixture body encodes"),
    }
}

/// The kernel: `i = 0; while i < N { "a" == "b"; i += 1 }` — two string
/// temporaries born, compared, and dead inside one iteration, over
/// 5M iterations. `borrowing` emits KSTR_BORROW with no RC ops;
/// `claiming` emits the pre-slice KSTR acquires with the terminal decs
/// the placement pass placed at the comparison's read.
fn registry(borrowing: bool) -> ModuleRegistry {
    let mut body = vec![Uop::Funcf { frame_slots: 6 }.into()];
    body.push(Uop::Kimm { dst: 0, kidx: k(0) }.into()); // i = 0
    body.push(Uop::Kimm { dst: 1, kidx: k(1) }.into()); // one = 1
    body.push(Uop::Kimm { dst: 2, kidx: k(2) }.into()); // limit = N
    let loop_head = u32::try_from(body.len()).expect("loop head fits the u32 target word");
    if borrowing {
        body.push(Uop::KstrBorrow { dst: 3, kidx: k(3) }.into());
        body.push(Uop::KstrBorrow { dst: 4, kidx: k(4) }.into());
    } else {
        body.push(Uop::Kstr { dst: 3, kidx: k(3) }.into());
        body.push(Uop::Kstr { dst: 4, kidx: k(4) }.into());
    }
    body.push(
        Uop::EqStr {
            dst: 5,
            lhs: 3,
            rhs: 4,
        }
        .into(),
    );
    if !borrowing {
        // the temporaries die at the comparison's read: the placed decs
        body.push(RcOp::Dec { src: 3 }.into());
        body.push(RcOp::Dec { src: 4 }.into());
    }
    body.push(
        Uop::AddI64 {
            dst: 0,
            lhs: 0,
            rhs: 1,
        }
        .into(),
    );
    body.push(
        Uop::LtI64 {
            dst: 5,
            lhs: 0,
            rhs: 2,
        }
        .into(),
    );
    body.push(
        Uop::Jmpif {
            cond: 5,
            target: loop_head,
        }
        .into(),
    );
    body.push(Uop::Ret { src: 0 }.into());
    let mut main = def(
        "main",
        &[
            SlotType::I64,
            SlotType::I64,
            SlotType::I64,
            SlotType::Ref,
            SlotType::Ref,
            SlotType::Bool,
        ],
        0,
        0,
        &body,
    );
    main.pool = vec![
        Const::I64(0),
        Const::I64(1),
        Const::I64(ITERATIONS),
        Const::Str("a".into()),
        Const::Str("b".into()),
    ];
    let mut reg = ModuleRegistry::new();
    reg.install(ModuleVersion::new("string_temp", vec![main]).expect("valid fixture module"));
    reg
}

pub(crate) fn drive(borrowing: bool) -> Value {
    let mut vm = Vm::new(registry(borrowing));
    vm.run("string_temp", "main", &[]).expect("the kernel runs")
}

#[library_benchmark]
fn borrowing_loads() -> Value {
    black_box(drive(true))
}

#[library_benchmark]
fn claiming_loads() -> Value {
    black_box(drive(false))
}

library_benchmark_group!(
    name = string_temp,
    benchmarks = [borrowing_loads, claiming_loads]
);

main!(library_benchmark_groups = string_temp);
