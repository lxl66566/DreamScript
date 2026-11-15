//! Gungraun instruction-count pin over the loop-claims extension's
//! over-approximation (the placement pass's loop-entry ownership stack):
//! a loop-carried Ref word whose textual last use is mid-body keeps its
//! count until the next overwrite or the exit. Each kernel runs under
//! the two dec spellings — the discipline's (the rollover dec at the
//! overwrite: the count held from the mid-body textual last use, across
//! the producing op, to the install) and the counterfactual refinement's
//! (the dec moved to the textual last use itself).
//!
//! The rebuild kernel (a fresh counted production per iteration through
//! the concat native — the strings kernel's inner shape) measures the
//! two spellings EQUAL: the previous content is freed either way, so the
//! hold moves a dec and adds zero retain/release pairs — the
//! over-approximation is free on every kernel that rebuilds its carried
//! word. The reset kernel (`s = "x"` — the head-reset shape) measures
//! the discipline strictly CHEAPER: its production-before-dec order
//! keeps the interned literal's count at one across the whole loop (no
//! teardown, no re-intern), while the release-at-use order drops the
//! count to zero every iteration and pays the object teardown plus the
//! re-intern — the measured delta is the naive refinement's per-
//! iteration regression, and wherever a refinement would instead skip
//! the re-acquire it is the historical double-kill (see
//! `crates/ds-lower/tests/loop_reset_double_kill.rs`). The discipline's
//! per-iteration traffic is the semantic minimum on both kernels, and
//! the absolute counts are the regression pins for the loop-claim
//! shapes.
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
    Const, ConstFuncRef, FunctionDef, ModuleRegistry, ModuleVersion, Value, Vm,
    abi::{ConstIdx, FrameLayout, Inst, RcOp, SlotType, uop::Uop},
    assemble, native,
};
use gungraun::prelude::*;

const ITERATIONS: i64 = 5_000_000;

fn k(idx: u16) -> ConstIdx {
    ConstIdx(idx)
}

fn def(
    name: &str,
    slots: &[SlotType],
    args: usize,
    body: &[Inst],
    pool: Vec<Const>,
) -> FunctionDef {
    FunctionDef {
        name: name.into(),
        layout: FrameLayout::new(slots.to_vec(), 0, args).expect("valid fixture layout"),
        pool,
        code: assemble(body).expect("fixture body encodes"),
    }
}

/// The reset kernel: `s = "seed"; i = 0; while i < N { read(s); s = "x";
/// i += 1 }` — the carried word is overwritten with the same interned
/// literal every iteration. One Kstr production (the acquire stands in
/// for the assignment's RHS), one dec, one install; `release_at_use`
/// moves the dec to the read.
fn reset_body(release_at_use: bool) -> Vec<Inst> {
    let mut body = vec![Uop::Funcf { frame_slots: 6 }.into()];
    body.push(Uop::Kstr { dst: 0, kidx: k(0) }.into()); // the carried claim's production
    body.push(Uop::Kimm { dst: 1, kidx: k(1) }.into()); // i = 0
    body.push(Uop::Kimm { dst: 2, kidx: k(2) }.into()); // one = 1
    body.push(Uop::Kimm { dst: 3, kidx: k(3) }.into()); // limit = N
    let loop_head = u32::try_from(body.len()).expect("loop head fits the u32 target word");
    body.push(Uop::Mov { dst: 4, src: 0 }.into()); // the mid-body textual last use
    if release_at_use {
        body.push(RcOp::Dec { src: 0 }.into()); // the refinement's release at the use
    }
    body.push(Uop::Kstr { dst: 4, kidx: k(0) }.into()); // the production
    if !release_at_use {
        body.push(RcOp::Dec { src: 0 }.into()); // the discipline's rollover
    }
    body.push(Uop::Mov { dst: 0, src: 4 }.into()); // the install (the overwrite)
    body.push(
        Uop::AddI64 {
            dst: 1,
            lhs: 1,
            rhs: 2,
        }
        .into(),
    );
    body.push(
        Uop::LtI64 {
            dst: 5,
            lhs: 1,
            rhs: 3,
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
    body.push(RcOp::Dec { src: 0 }.into()); // the exit release
    body.push(Uop::Ret { src: 1 }.into());
    body
}

/// The rebuild kernel: `s = "seed"; i = 0; while i < N { read(s);
/// s = "a".concat("b"); i += 1 }` — the carried word is rebuilt from a
/// fresh heap production every iteration (the strings kernel's inner
/// shape): the previous content dies either way, so the two spellings
/// share the identical count story and differ only in dec position.
fn rebuild_body(release_at_use: bool) -> Vec<Inst> {
    let mut body = vec![Uop::Funcf { frame_slots: 8 }.into()];
    body.push(Uop::Kstr { dst: 0, kidx: k(0) }.into()); // the carried claim's production
    body.push(Uop::Kimm { dst: 1, kidx: k(1) }.into()); // i = 0
    body.push(Uop::Kimm { dst: 2, kidx: k(2) }.into()); // one = 1
    body.push(Uop::Kimm { dst: 3, kidx: k(3) }.into()); // limit = N
    let loop_head = u32::try_from(body.len()).expect("loop head fits the u32 target word");
    body.push(Uop::Mov { dst: 4, src: 0 }.into()); // the mid-body textual last use
    if release_at_use {
        body.push(RcOp::Dec { src: 0 }.into()); // the refinement's release at the use
    }
    // the production: concat("a", "b") — a fresh counted heap string
    body.push(Uop::KstrBorrow { dst: 6, kidx: k(4) }.into());
    body.push(Uop::KstrBorrow { dst: 7, kidx: k(5) }.into());
    body.push(
        Uop::Call {
            callee: k(6),
            nargs: 2,
        }
        .into(),
    ); // the result lands at the arg area
    if !release_at_use {
        body.push(RcOp::Dec { src: 0 }.into()); // the discipline's rollover
    }
    body.push(Uop::Mov { dst: 0, src: 6 }.into()); // the install (the overwrite)
    body.push(
        Uop::AddI64 {
            dst: 1,
            lhs: 1,
            rhs: 2,
        }
        .into(),
    );
    body.push(
        Uop::LtI64 {
            dst: 5,
            lhs: 1,
            rhs: 3,
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
    body.push(RcOp::Dec { src: 0 }.into()); // the exit release
    body.push(Uop::Ret { src: 1 }.into());
    body
}

fn registry(rebuild: bool, release_at_use: bool) -> ModuleRegistry {
    let (slots_reset, slots_rebuild) = (
        vec![
            SlotType::Ref,
            SlotType::I64,
            SlotType::I64,
            SlotType::I64,
            SlotType::Ref,
            SlotType::Bool,
        ],
        vec![
            SlotType::Ref,
            SlotType::I64,
            SlotType::I64,
            SlotType::I64,
            SlotType::Ref,
            SlotType::Bool,
            SlotType::Ref,
            SlotType::Ref,
        ],
    );
    let (pool_reset, pool_rebuild) = (
        vec![
            Const::Str("x".into()),
            Const::I64(0),
            Const::I64(1),
            Const::I64(ITERATIONS),
        ],
        vec![
            Const::Str("seed".into()),
            Const::I64(0),
            Const::I64(1),
            Const::I64(ITERATIONS),
            Const::Str("a".into()),
            Const::Str("b".into()),
            Const::FuncRef(ConstFuncRef::Named {
                module: native::BUILTIN_MODULE.into(),
                func: native::BUILTIN_CONCAT,
            }),
        ],
    );
    let main = if rebuild {
        def(
            "main",
            &slots_rebuild,
            2,
            &rebuild_body(release_at_use),
            pool_rebuild,
        )
    } else {
        def(
            "main",
            &slots_reset,
            0,
            &reset_body(release_at_use),
            pool_reset,
        )
    };
    let mut reg = ModuleRegistry::new();
    reg.install(ModuleVersion::new("loop_claims", vec![main]).expect("valid fixture module"));
    reg
}

pub(crate) fn drive(rebuild: bool, release_at_use: bool) -> Value {
    // Vm::new installs the builtin core (concat's native table), so the
    // rebuild kernel's pool FuncRef resolves
    let mut vm = Vm::new(registry(rebuild, release_at_use));
    let answer = vm.run("loop_claims", "main", &[]).expect("the kernel runs");
    let [got] = answer.as_slice() else {
        panic!("the kernel answers one word");
    };
    *got
}

#[library_benchmark]
fn reset_held_to_overwrite() -> Value {
    black_box(drive(false, false))
}

#[library_benchmark]
fn reset_released_at_use() -> Value {
    black_box(drive(false, true))
}

#[library_benchmark]
fn rebuild_held_to_overwrite() -> Value {
    black_box(drive(true, false))
}

#[library_benchmark]
fn rebuild_released_at_use() -> Value {
    black_box(drive(true, true))
}

library_benchmark_group!(
    name = loop_claims,
    benchmarks = [
        reset_held_to_overwrite,
        reset_released_at_use,
        rebuild_held_to_overwrite,
        rebuild_released_at_use
    ]
);

main!(library_benchmark_groups = loop_claims);
