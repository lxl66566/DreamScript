//! End-to-end: build a dsmod module with the mutual even/odd pair plus a
//! main, serialize with the ds-mod writer, read it back (the round-trip
//! path — the mmap loader hands the same `&Module` to the same install),
//! install into the two-slot registry, and execute [BL-7, H6 item 5].

use ds_mod::{Constant, Function, Hash128, Module, TargetClass, read, read_checked, write};
use ds_vm::{
    ModuleRegistry, Value, Vm,
    abi::{
        ConstIdx, FrameLayout, SlotType,
        SlotType::{Bool, I64},
        uop::Uop,
    },
};

fn k(idx: u16) -> ConstIdx {
    ConstIdx(idx)
}

fn layout(slots: &[SlotType], params: usize, args: usize) -> FrameLayout {
    FrameLayout::new(slots.to_vec(), params, args).expect("valid fixture layout")
}

/// The corrected executable even/odd pair (the canonical fixture shape):
/// slots [n, t, scratch, staged], bools via the pool, callee as a qualified
/// `Str` path — the convention the loader resolves into a pool FuncRef,
/// because ds-mod's constant kind has no function-reference variant.
fn parity_module(depth: i64) -> Module {
    let even = Function::new(
        "even",
        layout(&[I64, Bool, I64, I64], 1, 1),
        vec![
            Constant::I64(0),
            Constant::I64(1),
            Constant::Bool(true),
            Constant::Str("mut.odd".into()),
        ],
        vec![
            Uop::Funcf { frame_slots: 4 },
            Uop::Kimm { dst: 2, kidx: k(0) },
            Uop::EqI64 {
                dst: 1,
                lhs: 0,
                rhs: 2,
            },
            Uop::Jmpif { cond: 1, target: 7 },
            Uop::Kimm { dst: 2, kidx: k(1) },
            Uop::SubI64 {
                dst: 3,
                lhs: 0,
                rhs: 2,
            },
            Uop::Callt {
                callee: k(3),
                nargs: 1,
            },
            Uop::Kconst { dst: 1, kidx: k(2) },
            Uop::Ret { src: 1 },
        ],
    )
    .expect("valid function");
    let odd = Function::new(
        "odd",
        layout(&[I64, Bool, I64, I64], 1, 1),
        vec![
            Constant::I64(0),
            Constant::I64(1),
            Constant::Bool(false),
            Constant::Str("mut.even".into()),
        ],
        vec![
            Uop::Funcf { frame_slots: 4 },
            Uop::Kimm { dst: 2, kidx: k(0) },
            Uop::EqI64 {
                dst: 1,
                lhs: 0,
                rhs: 2,
            },
            Uop::Jmpif { cond: 1, target: 7 },
            Uop::Kimm { dst: 2, kidx: k(1) },
            Uop::SubI64 {
                dst: 3,
                lhs: 0,
                rhs: 2,
            },
            Uop::Callt {
                callee: k(3),
                nargs: 1,
            },
            Uop::Kconst { dst: 1, kidx: k(2) },
            Uop::Ret { src: 1 },
        ],
    )
    .expect("valid function");
    let main = Function::new(
        "main",
        layout(&[I64], 0, 1),
        vec![Constant::I64(depth), Constant::Str("mut.even".into())],
        vec![
            Uop::Funcf { frame_slots: 1 },
            Uop::Kimm { dst: 0, kidx: k(0) }, // stage the depth
            Uop::Call {
                callee: k(1),
                nargs: 1,
            },
            Uop::Halt { slot: 0 }, // the returned bool overlays the staged word
        ],
    )
    .expect("valid function");
    Module::new(
        "mut",
        TargetClass::Bytecode,
        Hash128::compute(b"parity source"),
    )
    .with_function(even)
    .with_function(odd)
    .with_function(main)
}

#[test]
fn dsmod_round_trip_installs_and_runs_at_constant_stack() {
    let source = b"parity source";
    let bytes = write(&parity_module(1_000_000));
    // the writer-reader round trip is the no-filesystem path; the mmap
    // loader produces the same `Module` and the same install
    let module = read(&bytes).expect("dsmod bytes parse");
    assert_eq!(&module, &parity_module(1_000_000));
    // checked read proves the stored source hash matches
    read_checked(&bytes, source).expect("checked source hash matches");

    let mut vm = Vm::new(ModuleRegistry::new());
    vm.install_dsmod(&module).expect("module installs");
    // the loader filled the entry-descriptor word-0 placeholders at install
    for (position, f) in vm
        .registry()
        .slots("mut")
        .unwrap()
        .current()
        .functions()
        .iter()
        .enumerate()
    {
        assert_eq!(f.entry.resolve(), position);
    }

    // main CALLs even(1e6); the mutual CALLT chain must run at constant
    // stack: main's frame is 3 words, even/odd frames 6, so the high-water
    // mark is exactly the 7 words of one main frame plus one callee frame
    assert_eq!(vm.run("mut", "main", &[]), Ok(Value::Bool(true)));
    assert_eq!(vm.max_stack_len(), 7);
    // the same loaded module answers the odd direction
    assert_eq!(
        vm.run("mut", "odd", &[Value::I64(1_000_001)]),
        Ok(Value::Bool(true))
    );
    assert_eq!(
        vm.run("mut", "even", &[Value::I64(1_000_001)]),
        Ok(Value::Bool(false))
    );
}

#[test]
fn loader_reports_unqualified_and_unresolved_callees() {
    let mut vm = Vm::new(ModuleRegistry::new());
    // no dot: not a qualified path
    let bad = Function::new(
        "bad",
        layout(&[I64], 0, 0),
        vec![Constant::Str("notqualified".into())],
        vec![Uop::Funcf { frame_slots: 1 }],
    )
    .expect("valid function");
    let module = Module::new("m", TargetClass::Bytecode, Hash128::compute(b"m")).with_function(bad);
    assert_eq!(
        vm.install_dsmod(&module).unwrap_err(),
        ds_vm::LoadError::BadFuncRef {
            func: "bad".into(),
            raw: "notqualified".into(),
        }
    );
    // qualified, but the target module is not installed (deps install first)
    let foreign = Function::new(
        "foreign",
        layout(&[I64], 0, 0),
        vec![Constant::Str("other.fn".into())],
        vec![Uop::Funcf { frame_slots: 1 }],
    )
    .expect("valid function");
    let module =
        Module::new("m2", TargetClass::Bytecode, Hash128::compute(b"m2")).with_function(foreign);
    assert_eq!(
        vm.install_dsmod(&module).unwrap_err(),
        ds_vm::LoadError::Unresolved {
            func: "foreign".into(),
            path: "other.fn".into(),
        }
    );
}

#[test]
fn loader_rejects_funcf_disagreeing_with_the_frame_layout() {
    // ds-mod's Function validation checks encodability and jump targets;
    // FUNCF-vs-layout is a loader concern, and the mismatch is typed
    let bad = Function::new("bad", layout(&[I64], 0, 0), Vec::new(), vec![Uop::Funcf {
        frame_slots: 4,
    }])
    .expect("valid function");
    let bytes =
        write(&Module::new("m", TargetClass::Bytecode, Hash128::compute(b"m")).with_function(bad));
    let module = read(&bytes).expect("parses");
    let mut vm = Vm::new(ModuleRegistry::new());
    assert_eq!(
        vm.install_dsmod(&module).unwrap_err(),
        ds_vm::LoadError::Build(ds_vm::ModuleBuildError::FuncfMismatch {
            func: "bad".into(),
            funcef: 4,
            layout: 1,
        })
    );
}
