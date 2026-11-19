//! The abandoned-nursery half of the trapped-drive release: a trap between
//! spawn and join leaves the owner's nursery open with never-entered
//! children pinning their version rows (the entry frame and resume point
//! name the spawn's version) and their pool slots (nothing drives a Ready
//! child to death), and the nursery slot itself never recycles. The
//! release's owner link finds the nursery (the fiber whose code opened
//! it), and every live child releases under the landed release's own
//! discipline — deinit hooks fire through the owning frames' version rows
//! before the chain clears, the claims drain, the intern rows die, the
//! staged words truncate away with the segment (a released child's staged
//! callable word stops gating swaps, the retiring-frame semantics), and
//! the Dead markings return the slots. No child is entered at this door:
//! a never-entered child never ran, so nothing of it stands but its pins
//! and its staged words' counts. The repl's own line door rides the
//! same close: a trapping line's root survives its rollback, so no
//! dead-owner door ever catches the line's open nursery — the rollback
//! reads the pre-step nursery pool itself and tears the line's open
//! nurseries down.

use ds_vm::{
    Const, ConstFuncRef, FiberId, FiberState, FrameAnswer, FuncRef, FunctionDef, JoinOutcome,
    LoadError, ModuleRegistry, ModuleVersion, NurseryId, NurseryState, Platform, StagedSite, Trap,
    Value, Vm,
    abi::{CodePc, ConstIdx, FrameLayout, Inst, SlotType, uop::Uop},
    assemble, native,
    native::{BUILTIN_NURSERY_OPEN, BUILTIN_NURSERY_SPAWN},
};

fn k(idx: u16) -> ConstIdx {
    ConstIdx(idx)
}

fn def<I: Into<Inst> + Copy>(
    name: &str,
    slots: &[SlotType],
    params: usize,
    args: usize,
    pool: Vec<Const>,
    body: &[I],
) -> FunctionDef {
    FunctionDef {
        name: name.into(),
        layout: FrameLayout::new(slots.to_vec(), params, args).expect("valid fixture layout"),
        pool,
        code: assemble(body).expect("fixture body encodes"),
    }
}

fn builtin(index: u32) -> Const {
    Const::FuncRef(ConstFuncRef::Named {
        module: "builtin".into(),
        func: index,
    })
}

/// The observer hook `res$deinit`: prints the borrowed span's one word,
/// its hand-off count balanced at the exit — the trapped_release fixture
/// verbatim.
fn observer_deinit() -> FunctionDef {
    use ds_vm::abi::{Inst, uop::RcOp};
    def::<Inst>(
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

/// A landed scalar child `() -> int`, spawnable clean.
fn scalar_child(answer: i64) -> FunctionDef {
    def(
        "child",
        &[SlotType::I64],
        0,
        0,
        vec![Const::I64(answer)],
        &[
            Uop::Funcf { frame_slots: 1 },
            Uop::Kimm { dst: 0, kidx: k(0) },
            Uop::Ret { src: 0 },
        ],
    )
}

/// The owner entry: opens a nursery through the in-language seam, spawns
/// `child_mod`'s `child_fn` into it, then divides by zero — the trap
/// between spawn and join.
fn owner(child_mod: &str, child_fn: u32) -> FunctionDef {
    def(
        "owner",
        &[SlotType::I64, SlotType::I64, SlotType::Raw, SlotType::Raw],
        0,
        2,
        vec![
            builtin(BUILTIN_NURSERY_OPEN),
            builtin(BUILTIN_NURSERY_SPAWN),
            Const::FuncRef(ConstFuncRef::Named {
                module: child_mod.into(),
                func: child_fn,
            }),
            Const::I64(0),
        ],
        &[
            Uop::Funcf { frame_slots: 4 },
            // open: the id overlays overlay word 0
            Uop::Call {
                callee: k(0),
                nargs: 0,
            },
            Uop::Mov { dst: 0, src: 2 },
            // spawn: the id and the callable staged on the overlay words
            Uop::Mov { dst: 2, src: 0 },
            Uop::Kconst { dst: 3, kidx: k(2) },
            Uop::Call {
                callee: k(1),
                nargs: 2,
            },
            // the trap between spawn and join
            Uop::Kimm { dst: 1, kidx: k(3) },
            Uop::DivI64 {
                dst: 0,
                lhs: 0,
                rhs: 1,
            },
            Uop::Ret { src: 0 },
        ],
    )
}

/// Drives an owner fiber to its trap, answering the interrupted fiber.
fn trap_owner(vm: &mut Vm, module: &str) -> FiberId {
    let fiber = vm
        .spawn_fiber(module, "owner", &[])
        .expect("the owner resolves");
    let trap = vm
        .resume_fiber(fiber, Value::I64(0))
        .expect_err("the owner divides by zero");
    assert_eq!(trap, Trap::DivideByZero);
    fiber
}

fn vm() -> Vm {
    let mut reg = ModuleRegistry::new();
    reg.install_native(native::BUILTIN_MODULE, native::builtin_module());
    reg.install_platform(Platform::std());
    let mut vm = Vm::new(reg);
    vm.capture_output();
    vm
}

/// The release tears the abandoned nursery down with its never-entered
/// child: the nursery slot closes, the child's pool slot returns to the
/// spawn-reuse set beside the owner's, and the substrate serves a healthy
/// root-nursery join afterwards — the differential the teardown must not
/// disturb.
#[test]
fn a_trapped_owners_release_closes_the_nursery_and_recycles_the_child() {
    let mut vm = vm();
    vm.install_module(
        ModuleVersion::new("tn", vec![
            observer_deinit(),
            scalar_child(7),
            owner("tn", 1),
        ])
        .expect("valid fixture module"),
    )
    .expect("the version installs");
    let owner = trap_owner(&mut vm, "tn");
    // the abandoned shape: the nursery the owner's block opened stays open
    // with its child ready, never entered
    let (nursery, child) = (NurseryId(0), FiberId(2));
    assert_eq!(vm.nursery_state(nursery), Some(NurseryState::Open));
    assert_eq!(vm.fiber_state(child), Some(FiberState::Ready));
    assert!(
        vm.release_interrupted(owner),
        "the interrupted owner releases"
    );
    assert_eq!(vm.nursery_state(nursery), Some(NurseryState::Dead));
    assert_eq!(vm.fiber_state(owner), Some(FiberState::Dead));
    assert_eq!(
        vm.fiber_state(child),
        Some(FiberState::Dead),
        "the never-entered child releases with its nursery"
    );
    assert!(
        !vm.release_interrupted(owner),
        "a dead slot releases nothing more"
    );
    // both pool slots recycle and drive clean
    let first = vm.spawn_fiber("tn", "child", &[]).expect("slot 1 spawns");
    let second = vm.spawn_fiber("tn", "child", &[]).expect("slot 2 spawns");
    assert_eq!((first, second), (FiberId(1), FiberId(2)));
    for recycled in [first, second] {
        assert_eq!(
            vm.resume_fiber(recycled, Value::I64(0)),
            Ok(FrameAnswer::one(Value::I64(7))),
            "the recycled slot drives clean"
        );
    }
    // the healthy differential: a root-nursery join over the recycled
    // nursery slot works exactly as before
    let root = vm.open_root_nursery();
    assert_eq!(root, nursery, "the closed nursery slot recycles");
    vm.spawn_fiber_in(root, "tn", "child", &[])
        .expect("the root nursery spawns");
    match vm.join_nursery(root).expect("the join drives") {
        JoinOutcome::Joined(spans) => {
            assert_eq!(spans.as_slice(), [FrameAnswer::one(Value::I64(7))]);
        },
        JoinOutcome::Failed(_) => panic!("the healthy join answers its child"),
    }
    assert_eq!(vm.nursery_state(root), Some(NurseryState::Dead));
}

/// The owner's captured value fires its deinit exactly once at the release
/// and settles every count: the value's owner binding (whose directory
/// site fires), the closure object's adopted capture, and the child's
/// entry-arg count all dec — the child's own release is what takes the
/// count to zero, so a missing teardown leaves the value alive in the
/// owner's store and this test red.
#[test]
fn a_spawned_value_fires_its_deinit_once_and_settles_its_counts() {
    // the child body taking the captured value as its entry parameter
    let child = def(
        "child",
        &[SlotType::Ref, SlotType::I64],
        1,
        0,
        vec![Const::I64(0)],
        &[
            Uop::Funcf { frame_slots: 2 },
            Uop::Kimm { dst: 1, kidx: k(0) },
            Uop::Ret { src: 1 },
        ],
    );
    // the owner: binds the value, opens the nursery, captures the value
    // into a closure, spawns the closure's body into the nursery, traps —
    // the drop shape rides unreachable past the trap so the owner's
    // directory carries the site
    let owner = def(
        "owner",
        &[
            SlotType::I64,
            SlotType::Ref,
            SlotType::I64,
            SlotType::Fn,
            SlotType::Ref,
            SlotType::Raw,
            SlotType::Raw,
        ],
        0,
        2,
        vec![
            builtin(BUILTIN_NURSERY_OPEN),
            builtin(BUILTIN_NURSERY_SPAWN),
            Const::Str("cap value".to_owned()),
            Const::I64(0),
            Const::FuncRef(ConstFuncRef::Named {
                module: "cap".into(),
                func: 1,
            }),
            Const::FuncRef(ConstFuncRef::Named {
                module: "cap".into(),
                func: 0,
            }),
        ],
        &[
            Uop::Funcf { frame_slots: 7 },
            Uop::Call {
                callee: k(0),
                nargs: 0,
            },
            Uop::Mov { dst: 0, src: 5 },
            Uop::Kstr { dst: 1, kidx: k(2) },
            Uop::Kimm { dst: 2, kidx: k(3) },
            Uop::Kconst { dst: 3, kidx: k(4) },
            // the closure object adopting the value as its one capture
            Uop::Mov { dst: 5, src: 1 },
            Uop::ClosureNew {
                dst: 4,
                func: 3,
                n: 1,
            },
            // the closure word spawned into the open nursery
            Uop::Mov { dst: 5, src: 0 },
            Uop::Mov { dst: 6, src: 4 },
            Uop::Call {
                callee: k(1),
                nargs: 2,
            },
            // the trap between spawn and join
            Uop::DivI64 {
                dst: 0,
                lhs: 0,
                rhs: 2,
            },
            // the drop shape the directory reads (bandless: the ledger
            // witness), unreachable past the trap
            Uop::Mov { dst: 5, src: 1 },
            Uop::Call {
                callee: k(5),
                nargs: 1,
            },
            Uop::Ret { src: 0 },
        ],
    );
    let mut vm = vm();
    vm.install_module(
        ModuleVersion::new("cap", vec![observer_deinit(), child, owner])
            .expect("valid fixture module"),
    )
    .expect("the version installs");
    let fiber = trap_owner(&mut vm, "cap");
    let child = FiberId(2);
    assert_eq!(vm.captured_output(), b"", "the kept frames hold the claim");
    assert!(
        !vm.objects_at(fiber).is_empty(),
        "the value lives in the owner's store before the release"
    );
    assert!(
        vm.release_interrupted(fiber),
        "the interrupted owner releases"
    );
    assert_eq!(
        vm.captured_output(),
        b"cap value\n",
        "the spawned value's deinit fired exactly once"
    );
    assert!(
        vm.objects_at(fiber).is_empty(),
        "the child's released count took the value to zero"
    );
    assert_eq!(vm.fiber_state(child), Some(FiberState::Dead));
}

/// The never-entered child alone pins the spawn version's row — the
/// owner's frames run another module's code — and the release unpins it:
/// two installs past the two-slot bound keep the row while the child
/// stays, and one install after the release retires it.
#[test]
fn the_childs_release_unpins_the_spawn_versions_row() {
    // a second shape-differing member so each install's content moves
    fn echo(v: &str) -> FunctionDef {
        def(
            "echo",
            &[SlotType::I64],
            1,
            0,
            vec![Const::Str(v.to_owned())],
            &[Uop::Funcf { frame_slots: 1 }, Uop::Ret { src: 0 }],
        )
    }
    fn chl(v: &str) -> ModuleVersion {
        ModuleVersion::new("chl", vec![scalar_child(7), echo(v)]).expect("valid fixture module")
    }
    let mut vm = vm();
    vm.install_module(
        ModuleVersion::new("ownt", vec![owner("chl", 0)]).expect("valid fixture module"),
    )
    .expect("the owner module installs");
    let v1 = vm
        .install_module(chl("v1"))
        .expect("the child module installs");
    let owner = trap_owner(&mut vm, "ownt");
    vm.install_module(chl("v2")).expect("the swap installs");
    vm.install_module(chl("v3")).expect("the swap installs");
    assert!(
        vm.registry().version(v1).is_some(),
        "the never-entered child pins the spawn version's row past the two-slot bound"
    );
    assert!(
        vm.release_interrupted(owner),
        "the interrupted owner releases"
    );
    let v4 = vm.install_module(chl("v4")).expect("the swap installs");
    assert_ne!(v1, v4);
    assert!(
        vm.registry().version(v1).is_none(),
        "the sweep retires the row the released child stopped naming"
    );
}

/// A never-entered child's staged callable word stops gating swaps at the
/// release — the segment truncates with the marking, so a dropping swap
/// that refused over the child's staged word installs once released,
/// exactly the retiring-frame semantics the landed release pins for the
/// owner's own frames. The owner here opens its nursery and traps without
/// spawning; the host stages the child between the trap and the release,
/// its entry argument the staged word — the shape an embedder's spawn
/// face reaches over any open nursery.
#[test]
fn a_released_childs_staged_word_stops_gating_swaps() {
    // the child body taking the staged callable as its entry parameter
    fn staged() -> FunctionDef {
        def(
            "staged",
            &[SlotType::Fn, SlotType::I64],
            1,
            0,
            vec![Const::I64(0)],
            &[
                Uop::Funcf { frame_slots: 2 },
                Uop::Kimm { dst: 1, kidx: k(0) },
                Uop::Ret { src: 1 },
            ],
        )
    }
    fn version(with_scaled: Option<FunctionDef>) -> ModuleVersion {
        let mut functions = Vec::new();
        if let Some(scaled) = with_scaled {
            functions.push(scaled);
        }
        functions.push(staged());
        ModuleVersion::new("stg", functions).expect("valid fixture module")
    }
    // scaled: the function the staged word names
    let scaled = def(
        "scaled",
        &[SlotType::I64, SlotType::I64],
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
    );
    // the owner that only opens and traps
    let owner = def(
        "owner",
        &[SlotType::I64, SlotType::I64, SlotType::Raw],
        0,
        1,
        vec![builtin(BUILTIN_NURSERY_OPEN), Const::I64(0)],
        &[
            Uop::Funcf { frame_slots: 3 },
            Uop::Call {
                callee: k(0),
                nargs: 0,
            },
            Uop::Mov { dst: 0, src: 2 },
            Uop::Kimm { dst: 1, kidx: k(1) },
            Uop::DivI64 {
                dst: 0,
                lhs: 0,
                rhs: 1,
            },
            Uop::Ret { src: 0 },
        ],
    );
    let mut vm = vm();
    vm.install_module(ModuleVersion::new("own3", vec![owner]).expect("valid fixture module"))
        .expect("the owner module installs");
    vm.install_module(version(Some(scaled)))
        .expect("the child module installs");
    let owner = trap_owner(&mut vm, "own3");
    // the host stages the never-entered child over the abandoned nursery,
    // its entry argument the staged word
    let module = vm
        .registry()
        .module_id("stg")
        .expect("the installed module has its id");
    let child = vm
        .spawn_fiber_in(NurseryId(0), "stg", "staged", &[Value::Fn(FuncRef::new(
            module, 0,
        ))])
        .expect("the host stages the child");
    assert_eq!(child, FiberId(2));
    // the dropping swap refuses while the child's entry frame stages the
    // word
    let refused = vm
        .install_module(version(None))
        .expect_err("the never-entered child's staged word refuses the swap");
    assert_eq!(refused, LoadError::StagedFnMisroute {
        module: "stg".into(),
        index: 0,
        func: Some("scaled".into()),
        site: StagedSite::FrameSlot { fiber: child.0 },
    });
    // the release truncates the child's segment with its nursery, and the
    // same swap installs over the version nothing pins
    assert!(
        vm.release_interrupted(owner),
        "the interrupted owner releases"
    );
    vm.install_module(version(None))
        .expect("the released child no longer gates the swap");
}

/// A child that trapped under its own host drive between the owner's trap
/// and the release — interrupted, frames kept — releases under the
/// teardown's same per-fiber pass, its nursery link settling with the
/// nursery's own death.
#[test]
fn an_interrupted_child_releases_under_the_nursery_teardown() {
    // the trapping child: divides by zero on entry
    let trapped_child = def(
        "child",
        &[SlotType::I64, SlotType::I64],
        0,
        0,
        vec![Const::I64(1), Const::I64(0)],
        &[
            Uop::Funcf { frame_slots: 2 },
            Uop::Kimm { dst: 0, kidx: k(0) },
            Uop::Kimm { dst: 1, kidx: k(1) },
            Uop::DivI64 {
                dst: 0,
                lhs: 0,
                rhs: 1,
            },
            Uop::Ret { src: 0 },
        ],
    );
    let mut vm = vm();
    vm.install_module(
        ModuleVersion::new("tn", vec![observer_deinit(), trapped_child, owner("tn", 1)])
            .expect("valid fixture module"),
    )
    .expect("the version installs");
    let owner = trap_owner(&mut vm, "tn");
    let child = FiberId(2);
    // the host drives the child to its own trap: the drive ledgers the
    // child's death into the nursery and leaves it interrupted
    let trap = vm
        .resume_fiber(child, Value::I64(0))
        .expect_err("the child divides by zero");
    assert_eq!(trap, Trap::DivideByZero);
    assert_eq!(
        vm.nursery_cancelled(NurseryId(0)),
        Some(true),
        "the trapped child raised the nursery's flag"
    );
    assert_eq!(vm.fiber_state(child), Some(FiberState::Running));
    assert!(
        vm.release_interrupted(owner),
        "the interrupted owner releases"
    );
    assert_eq!(vm.nursery_state(NurseryId(0)), Some(NurseryState::Dead));
    assert_eq!(
        vm.fiber_state(child),
        Some(FiberState::Dead),
        "the interrupted child released under its nursery's teardown"
    );
    // both freed slots spawn again (the owner's is the first dead one)
    let first = vm
        .spawn_fiber("tn", "child", &[])
        .expect("the owner's slot spawns");
    let second = vm
        .spawn_fiber("tn", "child", &[])
        .expect("the child's slot spawns");
    assert_eq!((first, second), (FiberId(1), child));
}

/// The run face's own door: a root that traps between spawn and join
/// keeps its nursery open for the backtrace, and the next run's reset —
/// the drain door the release poured over pooled fibers — tears the
/// abandoned nursery down with the fresh entry's start.
#[test]
fn a_trapped_runs_reset_door_tears_the_abandoned_nursery() {
    let mut vm = vm();
    vm.install_module(ModuleVersion::new("ownt", vec![owner("tn", 0)]).expect("valid fixture"))
        .expect("the owner module installs");
    vm.install_module(
        ModuleVersion::new("tn", vec![scalar_child(7)]).expect("valid fixture module"),
    )
    .expect("the child module installs");
    let trap = vm
        .run("ownt", "owner", &[])
        .expect_err("the root traps between spawn and join");
    assert_eq!(trap, Trap::DivideByZero);
    let (nursery, child) = (NurseryId(0), FiberId(1));
    assert_eq!(vm.nursery_state(nursery), Some(NurseryState::Open));
    assert_eq!(vm.fiber_state(child), Some(FiberState::Ready));
    // the next run's reset door releases the previous root's frames and
    // its abandoned nursery before the new entry pushes
    assert_eq!(
        vm.run("tn", "child", &[]),
        Ok(FrameAnswer::one(Value::I64(7))),
        "the fresh entry runs clean"
    );
    assert_eq!(vm.nursery_state(nursery), Some(NurseryState::Dead));
    assert_eq!(
        vm.fiber_state(child),
        Some(FiberState::Dead),
        "the reset door released the abandoned child"
    );
}

/// The repl rollback's own line door: the trapping line's root survives
/// its rollback, so the drive release's dead-owner door never catches
/// the line's open nursery — the rollback's pre-step nursery reading
/// floors its own teardown. The never-entered child releases cold, both
/// slots recycle (the line's nursery slot serves the next root nursery,
/// the child's pool slot spawns and drives again), and a nursery open
/// before a later line — the host's root — survives that line's
/// rollback untouched.
#[test]
fn a_trapped_repl_lines_own_nursery_tears_down_at_the_rollback() {
    let mut vm = vm();
    let mut entry = owner("rline", 0);
    entry.name = "main".into();
    vm.install_module(
        ModuleVersion::new("rline", vec![scalar_child(7), entry]).expect("valid fixture module"),
    )
    .expect("the version installs");
    // the trapping line: the drive opens the nursery, spawns the
    // never-entered child, traps before the join
    let trap = vm
        .repl_start("rline", "main")
        .expect_err("the line divides by zero");
    assert_eq!(trap, Trap::DivideByZero);
    let (nursery, child) = (NurseryId(0), FiberId(1));
    assert_eq!(vm.nursery_state(nursery), Some(NurseryState::Open));
    assert_eq!(vm.fiber_state(child), Some(FiberState::Ready));
    vm.repl_rollback("rline", "main", CodePc(0))
        .expect("the rollback restores the frame");
    assert_eq!(vm.nursery_state(nursery), Some(NurseryState::Dead));
    assert_eq!(
        vm.fiber_state(child),
        Some(FiberState::Dead),
        "the never-entered child releases with its nursery"
    );
    // the recycled slots serve: the host's root nursery takes the line's
    // nursery slot, and the child's pool slot spawns and drives again
    let root = vm.open_root_nursery();
    assert_eq!(root, nursery, "the line's nursery slot recycles");
    let recycled = vm
        .spawn_fiber("rline", "child", &[])
        .expect("the child's pool slot spawns");
    assert_eq!(recycled, child);
    assert_eq!(
        vm.resume_fiber(recycled, Value::I64(0)),
        Ok(FrameAnswer::one(Value::I64(7))),
        "the recycled slot drives clean"
    );
    // a nursery open before the line survives the line's rollback: the
    // host's root stays untouched while the line's own nursery — a
    // fresh slot beside it — dies with its never-entered child
    let trap = vm
        .repl_start("rline", "main")
        .expect_err("the second line traps the same way");
    assert_eq!(trap, Trap::DivideByZero);
    assert_eq!(vm.nursery_state(NurseryId(1)), Some(NurseryState::Open));
    vm.repl_rollback("rline", "main", CodePc(0))
        .expect("the rollback restores the frame");
    assert_eq!(vm.nursery_state(NurseryId(1)), Some(NurseryState::Dead));
    assert_eq!(vm.fiber_state(child), Some(FiberState::Dead));
    assert_eq!(
        vm.nursery_state(root),
        Some(NurseryState::Open),
        "the host's pre-step root nursery survives the line's rollback"
    );
    // the healthy differential: the surviving root nursery joins its
    // completed child clean over the recycled slots
    match vm.join_nursery(root).expect("the join drives") {
        JoinOutcome::Joined(spans) => {
            assert_eq!(spans.as_slice(), [FrameAnswer::one(Value::I64(7))]);
        },
        JoinOutcome::Failed(_) => panic!("the healthy join answers its child"),
    }
    assert_eq!(vm.nursery_state(root), Some(NurseryState::Dead));
}
