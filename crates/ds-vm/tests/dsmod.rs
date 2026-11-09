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
/// slots [n, t, scratch, staged], bools via the pool, callees as real
/// same-module FuncRefs (empty module names the enclosing module; the odd
/// direction also exercises the own-name spelling).
fn parity_module(depth: i64) -> Module {
    let even = Function::new(
        "even",
        layout(&[I64, Bool, I64, I64], 1, 1),
        vec![
            Constant::I64(0),
            Constant::I64(1),
            Constant::Bool(true),
            Constant::FuncRef {
                module: String::new(), // enclosing module: odd, table index 1
                func: 1,
            },
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
            Constant::FuncRef {
                module: "mut".into(), // the own name also means same-module
                func: 0,
            },
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
        vec![Constant::I64(depth), Constant::FuncRef {
            module: String::new(),
            func: 0,
        }],
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
fn loader_reports_unresolved_funcrefs() {
    let mut vm = Vm::new(ModuleRegistry::new());
    // a same-module index past the module's own table
    let bad = Function::new(
        "bad",
        layout(&[I64], 0, 0),
        vec![Constant::FuncRef {
            module: String::new(),
            func: 9,
        }],
        vec![Uop::Funcf { frame_slots: 1 }],
    )
    .expect("valid function");
    let module = Module::new("m", TargetClass::Bytecode, Hash128::compute(b"m")).with_function(bad);
    assert_eq!(
        vm.install_dsmod(&module).unwrap_err(),
        ds_vm::LoadError::UnresolvedCallee {
            func: "bad".into(),
            module: String::new(),
            index: 9,
        }
    );
    // a foreign module that is not installed (dependencies install first)
    let foreign = Function::new(
        "foreign",
        layout(&[I64], 0, 0),
        vec![Constant::FuncRef {
            module: "other".into(),
            func: 0,
        }],
        vec![Uop::Funcf { frame_slots: 1 }],
    )
    .expect("valid function");
    let module =
        Module::new("m2", TargetClass::Bytecode, Hash128::compute(b"m2")).with_function(foreign);
    assert_eq!(
        vm.install_dsmod(&module).unwrap_err(),
        ds_vm::LoadError::UnresolvedCallee {
            func: "foreign".into(),
            module: "other".into(),
            index: 0,
        }
    );
}

/// Cross-module FuncRefs: the caller's pool names a foreign module; the call
/// resolves through the target's current slot, so installing a new version
/// of the target redirects the next call [H6 item 5].
#[test]
fn cross_module_funcref_resolves_through_the_current_slot() {
    // std v1: double(n) = n + n; std v2: triple(n) = n + n + n. The
    // accumulator lives in a local slot so the parameter is never clobbered
    let arith = |rounds: usize| {
        let mut code = vec![Uop::Funcf { frame_slots: 2 }];
        code.push(Uop::AddI64 {
            dst: 1,
            lhs: 0,
            rhs: 0,
        }); // acc = n + n
        for _ in 1..rounds {
            code.push(Uop::AddI64 {
                dst: 1,
                lhs: 1,
                rhs: 0,
            }); // acc += n
        }
        code.push(Uop::Ret { src: 1 });
        code
    };
    let std_version = |rounds: usize| {
        let f = Function::new(
            "apply",
            layout(&[I64, I64], 1, 0),
            Vec::new(),
            arith(rounds),
        )
        .expect("valid function");
        Module::new("std", TargetClass::Bytecode, Hash128::compute(b"std")).with_function(f)
    };
    // caller: main stages 5 and CALLs std.apply through a foreign FuncRef
    let main = Function::new(
        "main",
        layout(&[I64], 0, 1),
        vec![Constant::I64(5), Constant::FuncRef {
            module: "std".into(),
            func: 0,
        }],
        vec![
            Uop::Funcf { frame_slots: 1 },
            Uop::Kimm { dst: 0, kidx: k(0) },
            Uop::Call {
                callee: k(1),
                nargs: 1,
            },
            Uop::Halt { slot: 0 },
        ],
    )
    .expect("valid function");
    let bytes = write(
        &Module::new("app", TargetClass::Bytecode, Hash128::compute(b"app")).with_function(main),
    );
    let app = read(&bytes).expect("parses");

    let mut vm = Vm::new(ModuleRegistry::new());
    vm.install_dsmod(&std_version(1)).expect("std v1 installs");
    vm.install_dsmod(&app).expect("app installs");
    assert_eq!(vm.run("app", "main", &[]), Ok(Value::I64(10)));
    // swapping std's slot redirects the SAME FuncRef to the new version
    vm.install_dsmod(&std_version(2)).expect("std v2 swaps in");
    assert_eq!(vm.run("app", "main", &[]), Ok(Value::I64(15)));
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
