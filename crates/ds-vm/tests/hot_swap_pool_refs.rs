//! The retiring code's own pool references [H6 item 5 residual]: a live
//! code row's direct call names the swapped module through its constant
//! pool by function-table index, the code is immutable, and resolution
//! follows the module's current slot — so a reordering swap that moves or
//! drops the identity the index holds would silently route every live
//! frame's next direct call into whatever function now holds the index.
//! The swap's re-check refuses those reorders (a staged callable VALUE
//! re-keys or refuses at the swap; executable CODE can only refuse),
//! while an index-stable swap keeps installing and the parked frame's
//! next call picks up the new code — the dev loop's byte-identical
//! redirect. A retiring frame's post-swap KCONST mint translates its
//! own pool's index into the current table, and a dying frame's teardown
//! hook drives through its own version row.

use ds_vm::{
    Const, ConstFuncRef, FiberId, FrameAnswer, FunctionDef, LoadError, ModuleRegistry,
    ModuleVersion, Platform, Value, Vm,
    abi::{
        ConstIdx, FrameLayout, Inst, SlotType,
        SlotType::{Bool, Fn as FnSlot, I64, Raw, Ref},
        uop::Uop,
    },
    assemble,
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
    body: &[Inst],
) -> FunctionDef {
    FunctionDef {
        name: name.into(),
        layout: FrameLayout::new(slots.to_vec(), params, args).expect("valid fixture layout"),
        pool,
        code: assemble(body).expect("fixture body encodes"),
    }
}

/// The drive's one-shot suspension park, staged into the frame's `t`/`f`/
/// `c` slots: the armed request fires on the taken backward branch, and a
/// resumed drive re-takes the branch with the recomputed test false, so
/// the code after falls through. The counter zeroes first — a recycled
/// pool slot keeps its chunks, and their words carry stale values.
/// `at` is the `AddI64`'s own pc; `one` is k(0), `two` k(1), `zero` k(2),
/// the drive's own callee refs past them.
fn park_ops(at: u32, t: u8, f: u8, c: u8) -> Vec<Inst> {
    vec![
        Uop::Kimm { dst: f, kidx: k(2) }.into(),
        Uop::Kimm { dst: t, kidx: k(0) }.into(),
        Uop::AddI64 {
            dst: f,
            lhs: f,
            rhs: t,
        }
        .into(),
        Uop::Kimm { dst: t, kidx: k(1) }.into(),
        Uop::LtI64 {
            dst: c,
            lhs: f,
            rhs: t,
        }
        .into(),
        Uop::Jmpif {
            cond: c,
            target: at,
        }
        .into(),
    ]
}

/// The drive pool's shared head: `one`, `two`, `zero`, then the callee
/// refs the drive names from k(3).
fn park_pool(rest: &[Const]) -> Vec<Const> {
    let mut pool = vec![Const::I64(1), Const::I64(2), Const::I64(0)];
    pool.extend(rest.iter().cloned());
    pool
}

fn own_ref(func: u32) -> Const {
    Const::FuncRef(ConstFuncRef::Named {
        module: "m".into(),
        func,
    })
}

/// `scaled(n) = n * scale`, the callee every drive names.
fn scaled(scale: i64) -> FunctionDef {
    def("scaled", &[I64, I64], 1, 0, vec![Const::I64(scale)], &[
        Uop::Funcf { frame_slots: 2 }.into(),
        Uop::Kimm { dst: 1, kidx: k(0) }.into(),
        Uop::MulI64 {
            dst: 0,
            lhs: 0,
            rhs: 1,
        }
        .into(),
        Uop::Ret { src: 0 }.into(),
    ])
}

/// `pad(n) = 999`, the function a misrouted direct call would land in:
/// same layout as `scaled`, a different name, so the fixtures' reorders
/// are pure table-order moves.
fn pad() -> FunctionDef {
    def("pad", &[I64, I64], 1, 0, vec![Const::I64(999)], &[
        Uop::Funcf { frame_slots: 2 }.into(),
        Uop::Kimm { dst: 1, kidx: k(0) }.into(),
        Uop::Ret { src: 1 }.into(),
    ])
}

/// The reordering swap every refusal witnesses: v1 holds `scaled` at
/// index 1, the incoming table moves it to index 0, so a live frame's
/// call keyed to index 1 would silently reach `pad`.
fn reorder_v2() -> ModuleVersion {
    ModuleVersion::new("m", vec![scaled(3), pad()]).expect("valid reorder module")
}

/// The index-stable swap: `scaled` keeps index 1, so a live frame's call
/// keyed there survives the identity check and redirects into the new
/// code.
fn stable_v2(scale: i64) -> ModuleVersion {
    ModuleVersion::new("m", vec![pad(), scaled(scale)]).expect("valid index-stable module")
}

/// `drive` parks under the host's suspension request, then CALLs `scaled`
/// through its own pool: the parked frame keeps v1's code and pool across
/// the swap, and the resumed call witnesses which table the resolution
/// read.
fn drive_call() -> FunctionDef {
    let mut body = vec![Uop::Funcf { frame_slots: 5 }.into()];
    body.extend(park_ops(3, 1, 2, 3));
    let tail: Vec<Inst> = vec![
        Uop::Mov { dst: 4, src: 0 }.into(),
        Uop::Call {
            callee: k(3),
            nargs: 1,
        }
        .into(),
        Uop::Mov { dst: 1, src: 4 }.into(),
        Uop::Ret { src: 1 }.into(),
    ];
    body.extend(tail);
    def(
        "drive",
        &[I64, I64, I64, Bool, I64],
        1,
        1,
        park_pool(&[own_ref(1)]),
        &body,
    )
}

/// `drive` parking then tail-calling `scaled` (CALLT): the reused
/// frame's resolution reads the same pool entry the non-tail drive names.
fn drive_callt() -> FunctionDef {
    let mut body = vec![Uop::Funcf { frame_slots: 5 }.into()];
    body.extend(park_ops(3, 1, 2, 3));
    let tail: Vec<Inst> = vec![
        Uop::Mov { dst: 4, src: 0 }.into(),
        Uop::Callt {
            callee: k(3),
            nargs: 1,
        }
        .into(),
        Uop::Ret { src: 0 }.into(),
    ];
    body.extend(tail);
    def(
        "drive",
        &[I64, I64, I64, Bool, I64],
        1,
        1,
        park_pool(&[own_ref(1)]),
        &body,
    )
}

/// `drive` parking then dispatching `scaled` (CALLG, the trait-site
/// shape): the real argument plus the solved-instance word staged past
/// it, exactly the evidence the dispatch arm reads.
fn drive_callg() -> FunctionDef {
    let mut body = vec![Uop::Funcf { frame_slots: 6 }.into()];
    body.extend(park_ops(3, 1, 2, 3));
    let tail: Vec<Inst> = vec![
        Uop::Mov { dst: 4, src: 0 }.into(),
        Uop::Kimm { dst: 5, kidx: k(4) }.into(),
        Uop::Callg {
            callee: k(3),
            nargs: 1,
        }
        .into(),
        Uop::Mov { dst: 1, src: 4 }.into(),
        Uop::Ret { src: 1 }.into(),
    ];
    body.extend(tail);
    def(
        "drive",
        &[I64, I64, I64, Bool, I64, I64],
        1,
        2,
        park_pool(&[own_ref(1), Const::I64(0)]),
        &body,
    )
}

/// `drive` parking then tail-dispatching `scaled` (CALLTG): the frame
/// reuse over the dispatch site, same pool entry.
fn drive_calltg() -> FunctionDef {
    let mut body = vec![Uop::Funcf { frame_slots: 6 }.into()];
    body.extend(park_ops(3, 1, 2, 3));
    let tail: Vec<Inst> = vec![
        Uop::Mov { dst: 4, src: 0 }.into(),
        Uop::Kimm { dst: 5, kidx: k(4) }.into(),
        Uop::Calltg {
            callee: k(3),
            nargs: 1,
        }
        .into(),
        Uop::Ret { src: 0 }.into(),
    ];
    body.extend(tail);
    def(
        "drive",
        &[I64, I64, I64, Bool, I64, I64],
        1,
        2,
        park_pool(&[own_ref(1), Const::I64(0)]),
        &body,
    )
}

/// `drive` staging `scaled`'s Fn word in a slot, parking, then CALLS-ing
/// through the staged word: the staged VALUE discipline — the swap
/// re-keys it to the surviving identity's new index and the call keeps
/// reading the current slot, so the reordering swap installs.
fn drive_calls() -> FunctionDef {
    let mut body = vec![
        Uop::Funcf { frame_slots: 6 }.into(),
        Uop::Kconst { dst: 1, kidx: k(3) }.into(),
    ];
    body.extend(park_ops(4, 2, 3, 4));
    let tail: Vec<Inst> = vec![
        Uop::Mov { dst: 5, src: 0 }.into(),
        Uop::Calls {
            callee_slot: 1,
            nargs: 1,
        }
        .into(),
        Uop::Mov { dst: 2, src: 5 }.into(),
        Uop::Ret { src: 2 }.into(),
    ];
    body.extend(tail);
    def(
        "drive",
        &[I64, FnSlot, I64, I64, Bool, I64],
        1,
        1,
        park_pool(&[own_ref(1)]),
        &body,
    )
}

/// `drive` parking, then MINTING the fn word from its own pool after the
/// swap, then CALLS-ing it: the retiring pool's ref still carries the
/// old index, so the mint translates it into the current table.
fn drive_mint_calls() -> FunctionDef {
    let mut body = vec![Uop::Funcf { frame_slots: 6 }.into()];
    body.extend(park_ops(3, 2, 3, 4));
    let tail: Vec<Inst> = vec![
        Uop::Kconst { dst: 1, kidx: k(3) }.into(),
        Uop::Mov { dst: 5, src: 0 }.into(),
        Uop::Calls {
            callee_slot: 1,
            nargs: 1,
        }
        .into(),
        Uop::Mov { dst: 2, src: 5 }.into(),
        Uop::Ret { src: 2 }.into(),
    ];
    body.extend(tail);
    def(
        "drive",
        &[I64, FnSlot, I64, I64, Bool, I64],
        1,
        1,
        park_pool(&[own_ref(1)]),
        &body,
    )
}

fn swap_vm(v1: ModuleVersion) -> Vm {
    let mut reg = ModuleRegistry::new();
    reg.install(v1);
    Vm::new(reg)
}

fn v1_with(drive: FunctionDef) -> ModuleVersion {
    ModuleVersion::new("m", vec![drive, scaled(2)]).expect("valid v1 module")
}

fn park(vm: &mut Vm, n: i64) -> FiberId {
    let fiber = vm
        .spawn_fiber("m", "drive", &[Value::I64(n)])
        .expect("the entry resolves");
    vm.request_suspend(fiber);
    vm.resume_fiber(fiber, Value::I64(0))
        .expect("the drive parks at its requested suspension");
    fiber
}

/// A reordering swap refuses over a live frame's direct call: the parked
/// v1 frame's CALL names `scaled` at index 1, the incoming table moves
/// `scaled` to index 0, and installing would route the resumed call into
/// the `pad` holding the old index — so the swap refuses, naming the
/// pool, the index, and the moved function, and the installed version
/// keeps running.
#[test]
fn a_reordering_swap_refuses_over_a_live_frames_direct_call() {
    let mut vm = swap_vm(v1_with(drive_call()));
    let fiber = park(&mut vm, 21);
    assert_eq!(
        vm.install_module(reorder_v2()),
        Err(LoadError::PoolRefMisroute {
            module: "m".into(),
            pool: "m".into(),
            index: 1,
            func: Some("scaled".into()),
        }),
        "the reorder that moves the direct-called identity refuses"
    );
    assert_eq!(
        vm.resume_fiber(fiber, Value::I64(0)),
        Ok(FrameAnswer::one(Value::I64(42))),
        "the refused swap left the installed version running: the frame's call still lands"
    );
    // the identity-preserving swap installs over a freshly parked frame,
    // and the parked frame's next call picks up the new code
    let fiber = park(&mut vm, 21);
    vm.install_module(stable_v2(5))
        .expect("the index-stable swap installs");
    assert_eq!(
        vm.resume_fiber(fiber, Value::I64(0)),
        Ok(FrameAnswer::one(Value::I64(105))),
        "the redirect lands the parked frame's call in the new scaled (21 * 5)"
    );
}

/// Every direct-call spelling refuses the same reorder: the tail call
/// (CALLT), the trait dispatch (CALLG), and the dispatch tail (CALLTG)
/// name the same moved identity through their pool operands, so one
/// reorder refuses all three while their frames stay parked.
#[test]
fn tail_and_dispatch_sites_refuse_the_same_reorder() {
    for drive in [drive_callt(), drive_callg(), drive_calltg()] {
        let mut vm = swap_vm(v1_with(drive));
        let fiber = park(&mut vm, 21);
        assert_eq!(
            vm.install_module(reorder_v2()),
            Err(LoadError::PoolRefMisroute {
                module: "m".into(),
                pool: "m".into(),
                index: 1,
                func: Some("scaled".into()),
            }),
            "the reorder refuses over the parked frame's call site"
        );
        assert_eq!(
            vm.resume_fiber(fiber, Value::I64(0)),
            Ok(FrameAnswer::one(Value::I64(42))),
            "the installed version keeps running after the refusal"
        );
    }
}

/// The install-resolved id spelling of an own-module ref (the dsmod
/// loader's Dep(0) mapping) sits under the same valve: the parked
/// frame's pool carries `Uop { module: <m's id>, func: 1 }`, and the
/// reorder refuses instead of routing it into `pad`.
#[test]
fn an_id_spelled_call_refuses_the_same_reorder() {
    let drive = {
        let mut body = vec![Uop::Funcf { frame_slots: 5 }.into()];
        body.extend(park_ops(3, 1, 2, 3));
        let tail: Vec<Inst> = vec![
            Uop::Mov { dst: 4, src: 0 }.into(),
            Uop::Call {
                callee: k(3),
                nargs: 1,
            }
            .into(),
            Uop::Mov { dst: 1, src: 4 }.into(),
            Uop::Ret { src: 1 }.into(),
        ];
        body.extend(tail);
        def(
            "drive",
            &[I64, I64, I64, Bool, I64],
            1,
            1,
            park_pool(&[Const::FuncRef(ConstFuncRef::Uop {
                module: 0, // `m`, the first-installed uop module's id
                func: 1,
            })]),
            &body,
        )
    };
    let mut vm = swap_vm(v1_with(drive));
    assert_eq!(
        vm.registry().module_id("m"),
        Some(0),
        "m is the first uop module"
    );
    let fiber = park(&mut vm, 21);
    assert_eq!(
        vm.install_module(reorder_v2()),
        Err(LoadError::PoolRefMisroute {
            module: "m".into(),
            pool: "m".into(),
            index: 1,
            func: Some("scaled".into()),
        }),
        "the id-spelled call site refuses the reorder"
    );
    assert_eq!(
        vm.resume_fiber(fiber, Value::I64(0)),
        Ok(FrameAnswer::one(Value::I64(42))),
        "the installed version keeps running after the refusal"
    );
}

/// A reordering swap that keeps every direct-called identity at its index
/// installs: `pad` lands ahead of `scaled` without moving it, the live
/// frame's call keyed to index 1 survives the identity check, and the
/// parked frame's resumed call redirects into the new `scaled` (21 * 3)
/// — the precision of the valve, never a blanket refusal over live
/// frames.
#[test]
fn an_identity_preserving_reorder_installs_and_redirects() {
    let mut vm = swap_vm(v1_with(drive_call()));
    let fiber = park(&mut vm, 21);
    vm.install_module(reorder_v2())
        .expect_err("the reorder moving scaled refuses");
    let inserted = ModuleVersion::new("m", vec![pad(), scaled(3)]).expect("valid insert module");
    vm.install_module(inserted)
        .expect("the insert ahead of the called identity installs");
    assert_eq!(
        vm.resume_fiber(fiber, Value::I64(0)),
        Ok(FrameAnswer::one(Value::I64(63))),
        "the parked frame's call redirects into the new scaled"
    );
}

/// A dependency's reordering swap refuses over a dependent's live direct
/// call: the parked `app` frame's pool names `lib.scaled` at index 0,
/// the reorder moves `scaled` behind a fresh `pad`, and installing would
/// route the dependent frame's call into `lib`'s `pad` — the valve covers
/// every live code row, not the swapped module's own alone.
#[test]
fn a_dependents_live_call_refuses_the_dependencys_reorder() {
    let drive = {
        let mut body = vec![Uop::Funcf { frame_slots: 5 }.into()];
        body.extend(park_ops(3, 1, 2, 3));
        let tail: Vec<Inst> = vec![
            Uop::Mov { dst: 4, src: 0 }.into(),
            Uop::Call {
                callee: k(3),
                nargs: 1,
            }
            .into(),
            Uop::Mov { dst: 1, src: 4 }.into(),
            Uop::Ret { src: 1 }.into(),
        ];
        body.extend(tail);
        def(
            "drive",
            &[I64, I64, I64, Bool, I64],
            1,
            1,
            park_pool(&[Const::FuncRef(ConstFuncRef::Named {
                module: "lib".into(),
                func: 0,
            })]),
            &body,
        )
    };
    let mut reg = ModuleRegistry::new();
    reg.install(ModuleVersion::new("lib", vec![scaled(2)]).expect("valid lib module"));
    reg.install(ModuleVersion::new("app", vec![drive]).expect("valid app module"));
    let mut vm = Vm::new(reg);
    let fiber = vm
        .spawn_fiber("app", "drive", &[Value::I64(21)])
        .expect("the entry resolves");
    vm.request_suspend(fiber);
    vm.resume_fiber(fiber, Value::I64(0))
        .expect("the drive parks at its requested suspension");
    assert_eq!(
        vm.install_module(
            ModuleVersion::new("lib", vec![pad(), scaled(3)]).expect("valid lib reorder")
        ),
        Err(LoadError::PoolRefMisroute {
            module: "lib".into(),
            pool: "app".into(),
            index: 0,
            func: Some("scaled".into()),
        }),
        "the dependency's reorder refuses over the dependent's parked call"
    );
    // the index-stable swap of the dependency installs and redirects the
    // dependent frame's call into the new lib code
    vm.install_module(ModuleVersion::new("lib", vec![scaled(3)]).expect("valid lib swap"))
        .expect("the index-stable swap installs");
    assert_eq!(
        vm.resume_fiber(fiber, Value::I64(0)),
        Ok(FrameAnswer::one(Value::I64(63))),
        "the dependency's swap redirects the dependent frame's call"
    );
}

/// A staged callable word stays a value under the landed discipline: the
/// reordering swap re-keys it to the surviving identity's new index, and
/// the CALLS resolution keeps reading the current slot — the parked old
/// frame's indirect call lands in the NEW `scaled` (21 * 3), because the
/// mint is not a call site and the value discipline owns it.
#[test]
fn a_staged_own_module_word_keeps_the_current_slot_redirect() {
    let mut vm = swap_vm(v1_with(drive_calls()));
    let fiber = park(&mut vm, 21);
    vm.install_module(reorder_v2())
        .expect("the staged word's swap installs and re-keys");
    assert_eq!(
        vm.resume_fiber(fiber, Value::I64(0)),
        Ok(FrameAnswer::one(Value::I64(63))),
        "the re-keyed staged word calls the new table's scaled"
    );
}

/// A post-swap mint from a retiring frame's own pool translates into the
/// current table: the resumed drive's KCONST reads the v1 pool's ref
/// (index 1, `scaled`), the reordering swap moved `scaled` to index 0,
/// and the minted word must call the NEW `scaled` (21 * 3) — never the
/// `pad` the current slot holds at the stale index. The swap installs
/// because no live call site names the moved index.
#[test]
fn a_post_swap_mint_translates_the_own_pools_index() {
    let mut vm = swap_vm(v1_with(drive_mint_calls()));
    let fiber = park(&mut vm, 21);
    vm.install_module(reorder_v2())
        .expect("the mint is not a call site, so the swap installs");
    assert_eq!(
        vm.resume_fiber(fiber, Value::I64(0)),
        Ok(FrameAnswer::one(Value::I64(63))),
        "the minted word carries the translated index, calling the new scaled"
    );
}

/// A post-swap mint of a dropped identity refuses at the mint: the
/// reordering swap that drops `scaled` leaves the retiring pool's ref
/// naming nothing the current table records, so the mint traps naming
/// the vanished function instead of minting a word the current slot
/// would silently misroute.
#[test]
fn a_post_swap_mint_of_a_dropped_identity_refuses() {
    let mut vm = swap_vm(v1_with(drive_mint_calls()));
    let fiber = park(&mut vm, 21);
    let dropped = ModuleVersion::new("m", vec![pad()]).expect("valid dropping module");
    vm.install_module(dropped)
        .expect("no live call site names the dropped index, so the swap installs");
    assert_eq!(
        vm.resume_fiber(fiber, Value::I64(0)),
        Err(ds_vm::Trap::UnknownCallee {
            module: "m".into(),
            func: 1,
        }),
        "the mint of the vanished identity traps instead of misrouting"
    );
}

/// The observer hook `res$deinit`: prints its borrowed `Ref` parameter
/// through the platform console, the hand-off count balanced at the exit
/// (the mirrored teardown fixture shape). `marker` tells the v1 and the
/// recompiled v2 hook bodies apart, so the row-routing witness below can
/// prove which one drove.
fn observer_deinit(marker: &str) -> FunctionDef {
    def(
        "res$deinit",
        &[Ref, Ref, Bool, Raw],
        1,
        1,
        vec![
            Const::FuncRef(ConstFuncRef::Named {
                module: "std".into(),
                func: 0,
            }),
            Const::Str(marker.into()),
            Const::Bool(false),
        ],
        &[
            Uop::Funcf { frame_slots: 4 }.into(),
            Uop::Kstr { dst: 1, kidx: k(1) }.into(),
            Uop::Mov { dst: 3, src: 1 }.into(),
            Uop::Call {
                callee: k(0),
                nargs: 1,
            }
            .into(),
            Inst::Rc(ds_abi::RcOp::Dec { src: 0 }),
            Uop::Kconst { dst: 2, kidx: k(2) }.into(),
            Uop::Ret { src: 2 }.into(),
        ],
    )
}

/// The trapped root: establishes the deinit claim, then dies on the zero
/// divide, keeping its frames (the drop shape rides unreachable past the
/// RET, exactly the mirrored teardown fixture).
fn boom_root() -> FunctionDef {
    def(
        "boom_root",
        &[Ref, I64, I64, Raw],
        0,
        1,
        vec![Const::Str("root value".into()), own_ref(1), Const::I64(0)],
        &[
            Uop::Funcf { frame_slots: 4 }.into(),
            Uop::Kstr { dst: 0, kidx: k(0) }.into(),
            Uop::Kimm { dst: 1, kidx: k(2) }.into(),
            Uop::DivI64 {
                dst: 2,
                lhs: 1,
                rhs: 1,
            }
            .into(),
            Uop::Ret { src: 2 }.into(),
            Uop::Mov { dst: 3, src: 0 }.into(),
            Uop::Call {
                callee: k(1),
                nargs: 1,
            }
            .into(),
            Inst::Rc(ds_abi::RcOp::Dec { src: 0 }),
        ],
    )
}

fn ok() -> FunctionDef {
    def("ok", &[I64], 0, 0, vec![Const::I64(7)], &[
        Uop::Funcf { frame_slots: 1 }.into(),
        Uop::Kimm { dst: 0, kidx: k(0) }.into(),
        Uop::Ret { src: 0 }.into(),
    ])
}

fn hook_vm(v1: ModuleVersion) -> Vm {
    let mut reg = ModuleRegistry::new();
    reg.install_platform(Platform::std());
    reg.install(v1);
    let mut vm = Vm::new(reg);
    vm.capture_output();
    vm
}

/// A kept frame's deinit call site sits under the valve: the trapped
/// root's drop shape names `res$deinit` at index 1, the reorder that
/// moves the hook identity refuses, and the kept frames keep waiting for
/// the version still installed.
#[test]
fn a_kept_frames_deinit_call_refuses_the_hook_moving_reorder() {
    let mut vm = hook_vm(
        ModuleVersion::new("m", vec![boom_root(), observer_deinit("v1")]).expect("valid v1 module"),
    );
    assert!(
        vm.run("m", "boom_root", &[]).is_err(),
        "the root traps on the zero divide"
    );
    assert_eq!(vm.captured_output(), b"", "the trap itself fires nothing");
    let reordered = ModuleVersion::new("m", vec![ok(), pad()]).expect("valid reorder module");
    assert_eq!(
        vm.install_module(reordered),
        Err(LoadError::PoolRefMisroute {
            module: "m".into(),
            pool: "m".into(),
            index: 1,
            func: Some("res$deinit".into()),
        }),
        "the reorder moving the hook identity the kept frame names refuses"
    );
    // the installed version keeps running: the next run of the installed
    // table's entry still traps, and its reset fires the kept frame's
    // hook through the installed version
    assert!(vm.run("m", "boom_root", &[]).is_err());
    assert_eq!(
        vm.captured_output(),
        b"v1\n",
        "the kept frame's hook fired through the installed version's table"
    );
}

/// A dying frame's teardown hook drives through its own version row: the
/// index-stable swap recompiles `res$deinit` (same identity, a new body
/// printing its own marker), and the kept v1 frame's reset still fires
/// v1's hook — the claim the v1 code established releases through the v1
/// code that declared it, never the recompiled body the current slot
/// holds.
#[test]
fn a_kept_frames_teardown_hook_fires_through_the_owning_frames_row() {
    let mut vm = hook_vm(
        ModuleVersion::new("m", vec![boom_root(), observer_deinit("v1")]).expect("valid v1 module"),
    );
    assert!(
        vm.run("m", "boom_root", &[]).is_err(),
        "the root traps on the zero divide"
    );
    // index-stable: the hook keeps its index and identity, so the swap
    // installs over the kept frames
    let recompiled = ModuleVersion::new("m", vec![ok(), observer_deinit("v2")])
        .expect("valid index-stable module");
    vm.install_module(recompiled)
        .expect("the identity-preserving recompile installs");
    assert_eq!(
        vm.run("m", "ok", &[]),
        Ok(FrameAnswer::one(Value::I64(7))),
        "the next run lands"
    );
    assert_eq!(
        vm.captured_output(),
        b"v1\n",
        "the kept frame's hook drove through its own version row, never the recompiled body"
    );
}

/// A synthesized-lambda callee (`m$N`, the value-position spelling whose
/// name embeds its table index): `lam(n) = n * scale`, the same shape as
/// `scaled` — so a reorder that moves it renames it, and its identity
/// question falls to the content judge.
fn lam(name: &str, scale: i64) -> FunctionDef {
    def(name, &[I64, I64], 1, 0, vec![Const::I64(scale)], &[
        Uop::Funcf { frame_slots: 2 }.into(),
        Uop::Kimm { dst: 1, kidx: k(0) }.into(),
        Uop::MulI64 {
            dst: 0,
            lhs: 0,
            rhs: 1,
        }
        .into(),
        Uop::Ret { src: 0 }.into(),
    ])
}

/// `drive` parking then CALLing the lambda at own index 0: the parked
/// frame's pool names `m$0`, so a reorder that moves the lambda re-keys
/// the site's index and the resumed call witnesses the translation.
fn lam_drive_call() -> FunctionDef {
    let mut body = vec![Uop::Funcf { frame_slots: 5 }.into()];
    body.extend(park_ops(3, 1, 2, 3));
    let tail: Vec<Inst> = vec![
        Uop::Mov { dst: 4, src: 0 }.into(),
        Uop::Call {
            callee: k(3),
            nargs: 1,
        }
        .into(),
        Uop::Mov { dst: 1, src: 4 }.into(),
        Uop::Ret { src: 1 }.into(),
    ];
    body.extend(tail);
    def(
        "drive",
        &[I64, I64, I64, Bool, I64],
        1,
        1,
        park_pool(&[own_ref(0)]),
        &body,
    )
}

/// v1 holding the lambda ahead of the drive: the drive's pool site names
/// index 0, so every fixture's reorder moves the direct-called lambda.
fn lam_v1() -> ModuleVersion {
    ModuleVersion::new("m", vec![lam("m$0", 2), lam_drive_call()]).expect("valid v1 module")
}

/// A reorder that preserves a direct-called lambda's content re-keys the
/// site: the parked frame's CALL names `m$0` at index 0, the incoming
/// table moves it behind a fresh `pad` as `m$1` with an identical body
/// and capture set, and the content identity survived — so the swap
/// installs and the resumed call translates through the retiring row's
/// recorded identity into the moved body (21 * 2), never the `pad` that
/// now holds index 0.
#[test]
fn a_content_surviving_move_rekeys_a_direct_call_site() {
    let mut vm = swap_vm(lam_v1());
    let fiber = park(&mut vm, 21);
    vm.install_module(
        ModuleVersion::new("m", vec![pad(), lam("m$1", 2), lam_drive_call()])
            .expect("valid reorder module"),
    )
    .expect("the content-surviving move re-keys and installs");
    assert_eq!(
        vm.resume_fiber(fiber, Value::I64(0)),
        Ok(FrameAnswer::one(Value::I64(42))),
        "the retired frame's site calls the moved lambda (21 * 2), never pad's 999"
    );
}

/// A body edit riding the move refuses the direct-call re-check: the
/// incoming table moves the lambda AND changes its body, so neither the
/// name nor the content identity survived, and the installed version
/// keeps running.
#[test]
fn a_moved_direct_called_lambda_with_an_edited_body_refuses() {
    let mut vm = swap_vm(lam_v1());
    let fiber = park(&mut vm, 21);
    let refused = vm
        .install_module(
            ModuleVersion::new("m", vec![pad(), lam("m$1", 3), lam_drive_call()])
                .expect("valid reorder module"),
        )
        .expect_err("the moved-and-edited identity refuses the swap");
    assert_eq!(refused, LoadError::PoolRefMisroute {
        module: "m".into(),
        pool: "m".into(),
        index: 0,
        func: Some("m$0".into()),
    });
    assert_eq!(
        vm.resume_fiber(fiber, Value::I64(0)),
        Ok(FrameAnswer::one(Value::I64(42))),
        "the refused swap left the installed version running"
    );
}

/// Two identical-body identical-capture lambdas in the incoming table
/// refuse a moved direct-call site as ambiguous: the site's content
/// matches both candidates, and the re-key never guesses.
#[test]
fn an_ambiguous_direct_call_match_refuses() {
    let mut vm = swap_vm(lam_v1());
    let fiber = park(&mut vm, 21);
    let refused = vm
        .install_module(
            ModuleVersion::new("m", vec![lam("m$1", 2), lam("m$2", 2), lam_drive_call()])
                .expect("valid reorder module"),
        )
        .expect_err("the ambiguous content match refuses the swap");
    assert_eq!(refused, LoadError::PoolRefMisroute {
        module: "m".into(),
        pool: "m".into(),
        index: 0,
        func: Some("m$0".into()),
    });
    assert_eq!(
        vm.resume_fiber(fiber, Value::I64(0)),
        Ok(FrameAnswer::one(Value::I64(42))),
        "the refused swap left the installed version running"
    );
}

/// `drive` parking, then MINTING the moved lambda's fn word from its own
/// pool after the swap, then CALLS-ing it: the retiring pool's ref still
/// carries the old index, so the mint translates it through the content
/// judge into the current table.
#[test]
fn a_post_swap_mint_translates_a_moved_lambda() {
    let drive = {
        let mut body = vec![Uop::Funcf { frame_slots: 6 }.into()];
        body.extend(park_ops(3, 2, 3, 4));
        let tail: Vec<Inst> = vec![
            Uop::Kconst { dst: 1, kidx: k(3) }.into(),
            Uop::Mov { dst: 5, src: 0 }.into(),
            Uop::Calls {
                callee_slot: 1,
                nargs: 1,
            }
            .into(),
            Uop::Mov { dst: 2, src: 5 }.into(),
            Uop::Ret { src: 2 }.into(),
        ];
        body.extend(tail);
        def(
            "drive",
            &[I64, FnSlot, I64, I64, Bool, I64],
            1,
            1,
            park_pool(&[own_ref(0)]),
            &body,
        )
    };
    let mut vm =
        swap_vm(ModuleVersion::new("m", vec![lam("m$0", 2), drive]).expect("valid v1 module"));
    let fiber = park(&mut vm, 21);
    vm.install_module(
        ModuleVersion::new("m", vec![pad(), lam("m$1", 2), lam_drive_call()])
            .expect("valid reorder module"),
    )
    .expect("the content-surviving move re-keys and installs");
    assert_eq!(
        vm.resume_fiber(fiber, Value::I64(0)),
        Ok(FrameAnswer::one(Value::I64(42))),
        "the retiring frame's mint translated the moved lambda's index (21 * 2)"
    );
}
