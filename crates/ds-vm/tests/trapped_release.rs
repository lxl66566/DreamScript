//! The trapped-drive interrupt release: a
//! host drive that ends in a Trap leaves its fiber interrupted — Running
//! with no resume point, frames kept for the backtrace — and those kept
//! frames pin the executing version's table row past the two-slot bound
//! while the fiber's pool slot never recycles. The release lands after
//! the report read the kept frames: the deinit hooks fire exactly once
//! (each drive through its owning frame's version row), the claims drain,
//! the intern rows die, the frames clear, and the slot returns to the
//! spawn-reuse set — so the next install's sweep retires the row nothing
//! names, and the staged words of the dead frames stop gating swaps
//! exactly like a completed frame's do.

use ds_vm::{
    Const, ConstFuncRef, FiberId, FiberState, FrameAnswer, FunctionDef, LoadError, ModuleRegistry,
    ModuleVersion, Platform, StagedSite, Trap, Value, Vm,
    abi::{
        ConstIdx, FrameLayout, Inst, SlotType,
        uop::{RcOp, Uop},
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
    body: &[Inst],
) -> FunctionDef {
    FunctionDef {
        name: name.into(),
        layout: FrameLayout::new(slots.to_vec(), params, args).expect("valid fixture layout"),
        pool,
        code: assemble(body).expect("fixture body encodes"),
    }
}

/// The observer hook `res$deinit`: one borrowed `Ref` parameter (the
/// dying value's span), printed through the platform console, its
/// hand-off count balanced at the exit — the same fixture the teardown
/// suites drive.
fn observer_deinit() -> FunctionDef {
    def(
        "res$deinit",
        &[SlotType::Ref, SlotType::Bool, SlotType::Raw],
        1,
        1,
        vec![
            Const::FuncRef(ConstFuncRef::Named {
                module: "std".into(),
                func: 0,
            }),
            Const::Bool(false),
        ],
        &[
            Uop::Funcf { frame_slots: 3 }.into(),
            Uop::Mov { dst: 2, src: 0 }.into(),
            Uop::Call {
                callee: k(0),
                nargs: 1,
            }
            .into(),
            RcOp::Dec { src: 0 }.into(),
            Uop::Kconst { dst: 1, kidx: k(1) }.into(),
            Uop::Ret { src: 1 }.into(),
        ],
    )
}

/// The trapped entry `entry`: establishes one deinit-carrying value (a
/// claimed string at slot 1), then divides by zero. The drop shape rides
/// unreachable past the trap — the claim directory's source, exactly the
/// placement pass's emitted shape, so the release's pass finds the site.
fn entry(value: &str) -> FunctionDef {
    def(
        "entry",
        &[SlotType::I64, SlotType::Ref, SlotType::I64, SlotType::Raw],
        0,
        1,
        vec![
            Const::Str(value.to_owned()),
            Const::I64(0),
            Const::FuncRef(ConstFuncRef::Named {
                module: "trp".into(),
                func: 1,
            }),
        ],
        &[
            Uop::Funcf { frame_slots: 4 }.into(),
            Uop::Kstr { dst: 1, kidx: k(0) }.into(),
            Uop::Kimm { dst: 2, kidx: k(1) }.into(),
            Uop::DivI64 {
                dst: 0,
                lhs: 0,
                rhs: 2,
            }
            .into(),
            Uop::Mov { dst: 3, src: 1 }.into(),
            Uop::Call {
                callee: k(2),
                nargs: 1,
            }
            .into(),
            Uop::Ret { src: 0 }.into(),
        ],
    )
}

/// A landed scalar fn, spawnable without trapping: the slot-recycle
/// witness's next spawn.
fn other() -> FunctionDef {
    def("other", &[SlotType::I64], 0, 0, vec![Const::I64(7)], &[
        Uop::Funcf { frame_slots: 1 }.into(),
        Uop::Kimm { dst: 0, kidx: k(0) }.into(),
        Uop::Ret { src: 0 }.into(),
    ])
}

/// One `trp` module version: the entry, the observer hook, and the
/// landed fn — every version the same shape, so the swaps below install
/// without touching any identity.
fn trp(value: &str) -> ModuleVersion {
    ModuleVersion::new("trp", vec![entry(value), observer_deinit(), other()])
        .expect("valid fixture module")
}

fn vm() -> Vm {
    let mut reg = ModuleRegistry::new();
    reg.install_native(native::BUILTIN_MODULE, native::builtin_module());
    reg.install_platform(Platform::std());
    let mut vm = Vm::new(reg);
    vm.capture_output();
    vm
}

/// Drives one spawned entry to its trap, answering the interrupted
/// fiber.
fn trap_entry(vm: &mut Vm) -> FiberId {
    let fiber = vm
        .spawn_fiber("trp", "entry", &[])
        .expect("the entry resolves");
    let trap = vm
        .resume_fiber(fiber, Value::I64(0))
        .expect_err("the entry divides by zero");
    assert_eq!(trap, Trap::DivideByZero);
    fiber
}

/// The release fires the trapped entry's kept-frame deinit exactly once,
/// drains the value, and returns the slot: the report's backtrace read
/// the frames first, the captured output names the value once, the fiber
/// answers Dead, the next spawn reuses the slot, and a second release
/// answers false.
#[test]
fn a_trapped_entry_releases_its_hooks_claims_and_slot() {
    let mut vm = vm();
    vm.install_module(trp("trapped value"))
        .expect("the version installs");
    let fiber = trap_entry(&mut vm);
    assert!(
        vm.backtrace().iter().any(|frame| frame.func == "entry"),
        "the report read the kept frames before the release"
    );
    assert!(
        vm.release_interrupted(fiber),
        "the interrupted entry releases"
    );
    assert_eq!(vm.fiber_state(fiber), Some(FiberState::Dead));
    assert_eq!(
        vm.captured_output(),
        b"trapped value\n",
        "the release fired the kept frame's deinit exactly once"
    );
    assert!(
        vm.objects_at(fiber).is_empty(),
        "the drained claim freed the value with the slot's store"
    );
    assert!(
        !vm.release_interrupted(fiber),
        "a dead slot releases nothing more"
    );
    let recycled = vm
        .spawn_fiber("trp", "other", &[])
        .expect("the returned slot spawns");
    assert_eq!(recycled, fiber, "the release returned the pool slot");
    assert_eq!(
        vm.resume_fiber(recycled, Value::I64(0)),
        Ok(FrameAnswer::one(Value::I64(7))),
        "the recycled slot drives clean"
    );
}

/// The release drops the row pin the kept frames held: with the fiber
/// interrupted, two installs push the trapped version out of the two-slot
/// table yet the sweep keeps its row (the kept frames name it — the
/// honest pre-release pin this clause records), and the release followed
/// by one more install retires the row outright.
#[test]
fn the_release_unpins_the_trapped_versions_row() {
    let mut vm = vm();
    let v1 = vm.install_module(trp("v1")).expect("the version installs");
    let fiber = trap_entry(&mut vm);
    vm.install_module(trp("v2")).expect("the swap installs");
    vm.install_module(trp("v3")).expect("the swap installs");
    assert!(
        vm.registry().version(v1).is_some(),
        "the interrupted frames pin v1's row past the two-slot bound"
    );
    assert!(
        vm.release_interrupted(fiber),
        "the interrupted entry releases"
    );
    let v4 = vm.install_module(trp("v4")).expect("the swap installs");
    assert_ne!(v1, v4);
    assert!(
        vm.registry().version(v1).is_none(),
        "the sweep retires the row no frame names anymore"
    );
}

/// The released frames leave the staged-word re-check: a staged callable
/// word in the trapped frame's segment refuses a swap that drops its
/// function while the frames stay, and the release — the segment's words
/// die with it — lets the same swap install, exactly the completed-frame
/// semantics.
#[test]
fn a_released_frames_staged_word_stops_gating_swaps() {
    // scaled: the function the staged word names
    let scaled = def(
        "scaled",
        &[SlotType::I64, SlotType::I64],
        1,
        0,
        vec![Const::I64(2)],
        &[
            Uop::Funcf { frame_slots: 2 }.into(),
            Uop::Kimm { dst: 1, kidx: k(0) }.into(),
            Uop::MulI64 {
                dst: 0,
                lhs: 0,
                rhs: 1,
            }
            .into(),
            Uop::Ret { src: 0 }.into(),
        ],
    );
    // staged: the trapped entry — stages scaled's Fn word into a slot,
    // then divides by zero, so the word sits in the interrupted frame's
    // segment across the swaps
    let staged = def(
        "staged",
        &[SlotType::I64, SlotType::Fn, SlotType::I64],
        0,
        0,
        vec![
            Const::I64(0),
            Const::FuncRef(ConstFuncRef::Named {
                module: "stg".into(),
                func: 0,
            }),
        ],
        &[
            Uop::Funcf { frame_slots: 3 }.into(),
            Uop::Kconst { dst: 1, kidx: k(1) }.into(),
            Uop::Kimm { dst: 2, kidx: k(0) }.into(),
            Uop::DivI64 {
                dst: 0,
                lhs: 0,
                rhs: 2,
            }
            .into(),
            Uop::Ret { src: 0 }.into(),
        ],
    );
    let version = |functions: Vec<FunctionDef>| ModuleVersion::new("stg", functions).unwrap();
    // the dropping swap: `scaled` leaves the table, so the kept staged
    // word's identity did not survive
    let dropping = || {
        version(vec![def(
            "staged",
            &[SlotType::I64],
            0,
            0,
            vec![Const::I64(0)],
            &[
                Uop::Funcf { frame_slots: 1 }.into(),
                Uop::Kimm { dst: 0, kidx: k(0) }.into(),
                Uop::Ret { src: 0 }.into(),
            ],
        )])
    };
    let mut vm = vm();
    vm.install_module(version(vec![scaled, staged]))
        .expect("the version installs");
    let fiber = vm
        .spawn_fiber("stg", "staged", &[])
        .expect("the entry resolves");
    let trap = vm
        .resume_fiber(fiber, Value::I64(0))
        .expect_err("the entry divides by zero");
    assert_eq!(trap, Trap::DivideByZero);
    // the dropping swap refuses while the trapped frame's word stays
    let refused = vm
        .install_module(dropping())
        .expect_err("the kept staged word refuses the dropping swap");
    assert_eq!(refused, LoadError::StagedFnMisroute {
        module: "stg".into(),
        index: 0,
        func: Some("scaled".into()),
        site: StagedSite::FrameSlot { fiber: fiber.0 },
    });
    // the release drops the word with the frames, and the same swap
    // installs over the version nothing pins
    assert!(
        vm.release_interrupted(fiber),
        "the interrupted entry releases"
    );
    vm.install_module(dropping())
        .expect("the released frames no longer gate the swap");
}
