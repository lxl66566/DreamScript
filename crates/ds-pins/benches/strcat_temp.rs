//! Gungraun instruction-count pin over a concatenation-temporary kernel
//! (the arc's stage-three STRCAT gate): one hot loop whose body builds a
//! two-operand concatenation and compares it, once per iteration. The
//! two ids are the same kernel under the two spellings — `claiming` is
//! the pre-slice emission (STRCAT acquires a fresh counted heap block
//! plus the placed terminal dec per iteration, the claim ledger riding
//! each acquire), `serving` is stage three's STRCAT_ARENA with the
//! loop's mark/restore pair (no count, no claim, no dec, no heap
//! allocation at all) — so the Ir delta between the ids IS the
//! per-iteration cost the slice deleted, deterministically, and the
//! serving id's absolute count is the regression pin for the dispatch
//! arm and the arena bump path.
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

fn def(name: &str, slots: &[SlotType], body: &[Inst]) -> FunctionDef {
    FunctionDef {
        name: name.into(),
        layout: FrameLayout::new(slots.to_vec(), 0, 0).expect("valid fixture layout"),
        pool: Vec::new(),
        code: assemble(body).expect("fixture body encodes"),
    }
}

/// The kernel: `i = 0; while i < N { "ab" == "abcd"; i += 1 }` — one
/// concatenation temporary born, compared, and dead inside one
/// iteration, over 5M iterations. `serving` emits STRCAT_ARENA inside a
/// mark/restore pair with no RC ops; `claiming` emits the pre-slice
/// STRCAT acquires with the terminal dec the placement pass places at
/// the comparison's read.
fn registry(serving: bool) -> ModuleRegistry {
    let mut body: Vec<Inst> = vec![Uop::Funcf { frame_slots: 9 }.into()];
    body.push(Uop::Kimm { dst: 0, kidx: k(0) }.into()); // i = 0
    body.push(Uop::Kimm { dst: 1, kidx: k(1) }.into()); // one = 1
    body.push(Uop::Kimm { dst: 2, kidx: k(2) }.into()); // limit = N
    let head = body.len();
    if serving {
        // the loop-head mark the placement pass inserts for a loop
        // carrying a designated serving
        body.push(Uop::ArenaMark { dst: 7 }.into());
    }
    body.push(Uop::KstrBorrow { dst: 3, kidx: k(3) }.into());
    body.push(Uop::KstrBorrow { dst: 4, kidx: k(4) }.into());
    if serving {
        body.push(
            Uop::StrCatArena {
                dst: 5,
                lhs: 3,
                rhs: 4,
            }
            .into(),
        );
    } else {
        body.push(Uop::Kstr { dst: 3, kidx: k(3) }.into());
        body.push(Uop::Kstr { dst: 4, kidx: k(4) }.into());
        body.push(
            Uop::StrCat {
                dst: 5,
                lhs: 3,
                rhs: 4,
            }
            .into(),
        );
    }
    body.push(Uop::KstrBorrow { dst: 6, kidx: k(5) }.into());
    body.push(
        Uop::EqStr {
            dst: 8,
            lhs: 5,
            rhs: 6,
        }
        .into(),
    );
    if !serving {
        // the temporaries die at the comparison's read: the placed decs
        body.push(RcOp::Dec { src: 3 }.into());
        body.push(RcOp::Dec { src: 4 }.into());
        body.push(RcOp::Dec { src: 5 }.into());
    }
    if serving {
        // the back-edge restore that keeps the arena bounded
        body.push(Uop::ArenaRestore { src: 7 }.into());
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
            dst: 8,
            lhs: 0,
            rhs: 2,
        }
        .into(),
    );
    body.push(
        Uop::Jmpif {
            cond: 8,
            target: u32::try_from(head).expect("label index fits u32"),
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
            SlotType::Ref,
            SlotType::Ref,
            SlotType::I64,
            SlotType::Bool,
        ],
        &body,
    );
    main.pool = vec![
        Const::I64(0),
        Const::I64(1),
        Const::I64(ITERATIONS),
        Const::Str("ab".into()),
        Const::Str("cd".into()),
        Const::Str("abcd".into()),
    ];
    let mut reg = ModuleRegistry::new();
    reg.install(ModuleVersion::new("strcat_temp", vec![main]).expect("valid fixture module"));
    reg
}

pub(crate) fn drive(serving: bool) -> Value {
    let mut vm = Vm::new(registry(serving));
    let answer = vm.run("strcat_temp", "main", &[]).expect("the kernel runs");
    let [got] = answer.as_slice() else {
        panic!("the kernel answers one word");
    };
    assert_eq!(got, &Value::I64(ITERATIONS), "the loop ran to completion");
    assert!(
        vm.objects().is_empty(),
        "the kernel's counts balance: {} live",
        vm.objects().len()
    );
    *got
}

#[library_benchmark]
fn serving_cats() -> Value {
    black_box(drive(true))
}

#[library_benchmark]
fn claiming_cats() -> Value {
    black_box(drive(false))
}

library_benchmark_group!(
    name = strcat_temp,
    benchmarks = [serving_cats, claiming_cats]
);

main!(library_benchmark_groups = strcat_temp);
