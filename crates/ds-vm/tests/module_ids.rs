//! The module-id space (F2 scalar FuncRef words), pinned. The format-v4
//! payload spells uop callees as dep-list positions, and the dsmod
//! install resolves position → dep name → module id into the pool, so a
//! loaded callee word carries the id its VM minted at first install
//! (first-install order, stable across hot-reload swaps, uop modules
//! only); native surfaces and directly installed host modules stay
//! name-keyed, resolved at issue time through the name's current slot.
//! Ids are per-VM state the format never persists, and words never
//! legitimately cross VMs — two embedders are two independent VMs [H5];
//! the one hazard, a foreign word silently aliasing another registry's
//! module, is recorded, not hidden.
use ds_vm::{
    Const, ConstFuncRef, FrameAnswer, FuncRef, FunctionDef, ModuleRegistry, ModuleVersion,
    Platform, Trap, Value, Vm,
    abi::{
        ConstIdx, FrameLayout, SlotType,
        SlotType::{Fn as FnSlot, I64},
        uop::Uop,
    },
    assemble, native,
};

fn k(idx: u16) -> ConstIdx {
    ConstIdx(idx)
}

fn def(
    name: &str,
    slots: &[SlotType],
    params: usize,
    args: usize,
    pool: Vec<Const>,
    body: &[Uop],
) -> FunctionDef {
    FunctionDef {
        name: name.into(),
        layout: FrameLayout::new(slots.to_vec(), params, args).expect("valid fixture layout"),
        pool,
        code: assemble(body).expect("fixture body encodes"),
    }
}

/// The `a` module: `double(x) = x * 2`.
fn a_module() -> ModuleVersion {
    ModuleVersion::new("a", vec![def(
        "double",
        &[I64, I64],
        1,
        0,
        vec![Const::I64(2)],
        &[
            Uop::Funcf { frame_slots: 2 },
            Uop::Kimm { dst: 1, kidx: k(0) },
            Uop::MulI64 {
                dst: 0,
                lhs: 0,
                rhs: 1,
            },
            Uop::Ret { src: 0 },
        ],
    )])
    .expect("valid module")
}

/// The `b` module: `negate(x) = 0 - x`.
fn b_module() -> ModuleVersion {
    ModuleVersion::new("b", vec![def(
        "negate",
        &[I64, I64],
        1,
        0,
        vec![Const::I64(0)],
        &[
            Uop::Funcf { frame_slots: 2 },
            Uop::Kimm { dst: 1, kidx: k(0) },
            Uop::SubI64 {
                dst: 0,
                lhs: 1,
                rhs: 0,
            },
            Uop::Ret { src: 0 },
        ],
    )])
    .expect("valid module")
}

/// The `main` module: `f(x) = b.negate(a.double(x))` through two pool
/// FuncRef call sites — directly installed callees, name-keyed and
/// resolved at issue time like the dsmod natives.
fn main_module() -> ModuleVersion {
    ModuleVersion::new("main", vec![def(
        "f",
        &[I64, I64, I64, I64],
        1,
        1,
        vec![
            Const::FuncRef(ConstFuncRef::Named {
                module: "a".into(),
                func: 0,
            }),
            Const::FuncRef(ConstFuncRef::Named {
                module: "b".into(),
                func: 0,
            }),
        ],
        &[
            Uop::Funcf { frame_slots: 4 },
            Uop::Mov { dst: 3, src: 0 }, // stage x in the arg area
            Uop::Call {
                callee: k(0),
                nargs: 1,
            },
            Uop::Mov { dst: 1, src: 3 }, // keep a.double(x)
            Uop::Mov { dst: 3, src: 1 }, // stage it for the second call
            Uop::Call {
                callee: k(1),
                nargs: 1,
            },
            Uop::Ret { src: 3 },
        ],
    )])
    .expect("valid module")
}

/// The `app` module: CALLS through a staged Fn word (a copy to a Fn local,
/// then the per-element arg-area staging of 21).
fn apply_module() -> ModuleVersion {
    ModuleVersion::new("app", vec![def(
        "apply",
        &[FnSlot, FnSlot, I64],
        1,
        1,
        vec![Const::I64(21)],
        &[
            Uop::Funcf { frame_slots: 3 },
            Uop::Mov { dst: 1, src: 0 }, // the Fn word copies like any scalar
            Uop::Kimm { dst: 2, kidx: k(0) }, // stage 21 in the arg area
            Uop::Calls {
                callee_slot: 1,
                nargs: 1,
            },
            Uop::Ret { src: 2 },
        ],
    )])
    .expect("valid module")
}

/// Ids assign at first install, in first-install order, and never move:
/// a hot-reload swap keeps the id, later installs only append, and an
/// uninstalled name has none.
#[test]
fn ids_assign_in_first_install_order_and_stay_put_across_swaps() {
    let mut reg = ModuleRegistry::new();
    reg.install(a_module());
    reg.install(b_module());
    assert_eq!(reg.module_id("a"), Some(0));
    assert_eq!(reg.module_id("b"), Some(1));
    // a second version of `a` swaps the slots and keeps the id
    reg.install(a_module());
    assert_eq!(reg.module_id("a"), Some(0), "a swap keeps the id");
    assert_eq!(reg.module_id("b"), Some(1));
    // a later install appends; nothing reorders
    reg.install(
        ModuleVersion::new("c", vec![def("z", &[I64], 0, 0, vec![Const::I64(9)], &[
            Uop::Funcf { frame_slots: 1 },
            Uop::Kimm { dst: 0, kidx: k(0) },
            Uop::Ret { src: 0 },
        ])])
        .expect("valid module"),
    );
    assert_eq!(reg.module_id("c"), Some(2));
    assert_eq!(reg.module_id("a"), Some(0));
    assert_eq!(reg.module_id("b"), Some(1));
    // an uninstalled name has no id
    assert_eq!(reg.module_id("nowhere"), None);
}

/// The same module tables run identically under either sibling install
/// order while the ids differ: these directly installed callees resolve
/// by name at issue time, so correctness never rests on the id values —
/// the invariant the id-resolved dsmod pool keeps by resolving ids at
/// install and re-reading the current slot at issue.
#[test]
fn module_bytes_run_identically_under_either_sibling_install_order() {
    let build = |order: [ModuleVersion; 3]| {
        let mut vm = Vm::new(ModuleRegistry::new());
        for module in order {
            vm.install_module(module).expect("the swap installs");
        }
        vm
    };
    let mut vm1 = build([a_module(), b_module(), main_module()]);
    let mut vm2 = build([b_module(), a_module(), main_module()]);
    // `main` installs last in both: the dependencies-first validation
    // (foreign FuncRefs resolve against installed versions) is order's
    // only constraint; the siblings are free to reorder
    assert_eq!(vm1.registry().module_id("a"), Some(0));
    assert_eq!(vm2.registry().module_id("a"), Some(1), "ids are per-VM");
    assert_eq!(
        vm1.run("main", "f", &[Value::I64(21)]),
        Ok(FrameAnswer::one(Value::I64(-42)))
    );
    assert_eq!(
        vm2.run("main", "f", &[Value::I64(21)]),
        Ok(FrameAnswer::one(Value::I64(-42)))
    );
}

/// Native module names never enter the id space: the builtin core and the
/// platform natives resolve as call sites through their native tables, but
/// a native name mints no callable word — the KCONST arm refuses with the
/// honest trap, while a uop callee mints its id-space word.
#[test]
fn native_names_never_enter_the_id_space() {
    let mut reg = ModuleRegistry::new();
    reg.install_platform(Platform::std());
    let mut vm = Vm::new(reg); // installs the builtin core
    assert_eq!(vm.registry().module_id(native::BUILTIN_MODULE), None);
    assert_eq!(vm.registry().module_id("std"), None);
    // a uop module the mint path can reach
    vm.install_module(
        ModuleVersion::new("u", vec![def("f", &[I64], 0, 0, vec![Const::I64(7)], &[
            Uop::Funcf { frame_slots: 1 },
            Uop::Kimm { dst: 0, kidx: k(0) },
            Uop::Ret { src: 0 },
        ])])
        .expect("valid module"),
    )
    .expect("the swap installs");
    // one module minting both kinds: the uop word materializes, the
    // native word refuses at issue
    vm.install_module(
        ModuleVersion::new("m", vec![
            def(
                "mint",
                &[FnSlot, FnSlot],
                0,
                0,
                vec![Const::FuncRef(ConstFuncRef::Named {
                    module: "u".into(),
                    func: 0,
                })],
                &[
                    Uop::Funcf { frame_slots: 2 },
                    Uop::Kconst { dst: 0, kidx: k(0) },
                    Uop::Mov { dst: 1, src: 0 },
                    Uop::Ret { src: 1 },
                ],
            ),
            def(
                "mint_native",
                &[FnSlot],
                0,
                0,
                vec![Const::FuncRef(ConstFuncRef::Named {
                    module: "builtin".into(),
                    func: 0,
                })],
                &[
                    Uop::Funcf { frame_slots: 1 },
                    Uop::Kconst { dst: 0, kidx: k(0) },
                    Uop::Ret { src: 0 },
                ],
            ),
        ])
        .expect("valid module"),
    )
    .expect("the swap installs");
    let expected = Value::Fn(FuncRef::new(
        vm.registry()
            .module_id("u")
            .expect("uop modules enter the id space"),
        0,
    ));
    assert_eq!(vm.run("m", "mint", &[]), Ok(FrameAnswer::one(expected)));
    assert_eq!(
        vm.run("m", "mint_native", &[]),
        Err(Trap::UnknownCallee {
            module: "builtin".into(),
            func: 0,
        }),
        "a native name mints no callable word"
    );
}

/// The id space's one sharp edge: a fn word minted under one registry's
/// id space silently aliases whatever module holds the same id under
/// another registry. Nothing diagnoses it — the word is an id, the id
/// resolves through the running VM's table — and the format never
/// serializes ids (the dep-position payload re-resolves per VM), so the
/// discipline stays that words never cross VMs; only the load protocol's
/// deterministic dep walk can make sibling VMs agree, and only for the
/// same closure.
#[test]
fn a_foreign_word_aliases_the_other_registry_s_module_silently() {
    let mut vm1 = Vm::new(ModuleRegistry::new());
    vm1.install_module(a_module()).expect("the swap installs");
    vm1.install_module(b_module()).expect("the swap installs");
    vm1.install_module(apply_module())
        .expect("the swap installs");
    let word = Value::Fn(FuncRef::new(
        vm1.registry().module_id("a").expect("installed"),
        0,
    ));
    assert_eq!(
        vm1.run("app", "apply", &[word]),
        Ok(FrameAnswer::one(Value::I64(42)))
    );
    // the same word under a registry where id 0 is `b`: the call reaches
    // negate, silently — no trap, no wrong-slot diagnostic
    let mut vm2 = Vm::new(ModuleRegistry::new());
    vm2.install_module(b_module()).expect("the swap installs");
    vm2.install_module(a_module()).expect("the swap installs");
    vm2.install_module(apply_module())
        .expect("the swap installs");
    assert_eq!(
        vm2.run("app", "apply", &[word]),
        Ok(FrameAnswer::one(Value::I64(-21)))
    );
}
