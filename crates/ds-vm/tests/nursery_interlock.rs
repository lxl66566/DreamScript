//! The cross-nursery join interlock pins: a joiner that is itself a child
//! under a drive joining another open nursery parks, the joined nursery's
//! drive runs on the interlock stack (frames pushed LIFO, the nesting the
//! recursive driver used to carry as nested `execute` frames on the Rust
//! stack — a recursive nursery block per call level overflowed the process
//! at depth ~50), and the answer is delivered onto the parked call site at
//! each level. What the fixtures pin:
//!
//! - delivery: a two- and three-level boundary chain (root joins N1, whose child joins N2, whose
//!   child joins N3) lands end to end with every nursery consumed and every fiber dead, and the
//!   count-join seam surfaces each level's trapped count.
//! - the deep chain: a generated depth-1024 chain lands — nesting depth costs heap, never the Rust
//!   stack.
//! - the interlock cycle: a second joiner of a nursery whose drive already stands refuses with the
//!   structured nursery trap (the own-nursery rule's cycle, one level up), and the refusal surfaces
//!   at the outer boundary like any child failure.
//! - the gone joiner under nesting: a grandchild resuming the parked joiner mid-drive moves it on;
//!   its continuation runs under the parent frame and its trap surfaces at the outer boundary
//!   deterministically.

use ds_vm::{
    Const, ConstFuncRef, FrameAnswer, FunctionDef, ModuleRegistry, ModuleVersion, NurseryId,
    NurseryState, Trap, Value, Vm,
    abi::{
        ConstIdx, FrameLayout, SlotType,
        SlotType::{I64, Raw},
        uop::Uop,
    },
    assemble,
    native::{
        BUILTIN_FIBER_RESUME, BUILTIN_NURSERY_BOUNDARY, BUILTIN_NURSERY_JOIN, BUILTIN_NURSERY_OPEN,
        BUILTIN_NURSERY_SPAWN,
    },
};

fn k(idx: u16) -> ConstIdx {
    ConstIdx(idx)
}

fn builtin(index: u32) -> Const {
    Const::FuncRef(ConstFuncRef::Named {
        module: "builtin".into(),
        func: index,
    })
}

/// A same-module callee by function-table index (the fixture module "ic").
fn ic_ref(index: u32) -> Const {
    Const::FuncRef(ConstFuncRef::Named {
        module: "ic".into(),
        func: index,
    })
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

/// `() -> int`: 7 — the chain's landing child.
fn base_def() -> FunctionDef {
    def("base", &[I64], 0, 0, vec![Const::I64(7)], &[
        Uop::Funcf { frame_slots: 1 },
        Uop::Kimm { dst: 0, kidx: k(0) },
        Uop::Ret { src: 0 },
    ])
}

/// `() -> int`: 1 / 0 — dies trapped at the DIVI64.
fn trap_def() -> FunctionDef {
    def(
        "trap",
        &[I64, I64],
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
    )
}

/// `() -> int` over the boundary seam: open a fresh nursery, spawn
/// `child` through the seam, join through the boundary with `mask` staged,
/// and return the answer span's payload word.
fn boundary_level_def(name: &str, child: u32, mask: i64) -> FunctionDef {
    def(
        name,
        &[Raw, Raw, Raw, Raw],
        0,
        2,
        vec![
            builtin(BUILTIN_NURSERY_OPEN),
            builtin(BUILTIN_NURSERY_SPAWN),
            builtin(BUILTIN_NURSERY_BOUNDARY),
            ic_ref(child),
            Const::I64(mask),
        ],
        &[
            Uop::Funcf { frame_slots: 4 },
            // open: the fresh nursery's id lands on overlay word 0
            Uop::Call {
                callee: k(0),
                nargs: 0,
            },
            Uop::Mov { dst: 0, src: 2 },
            // spawn: (id, the child's scalar callable word)
            Uop::Kconst { dst: 1, kidx: k(3) },
            Uop::Mov { dst: 2, src: 0 },
            Uop::Mov { dst: 3, src: 1 },
            Uop::Call {
                callee: k(1),
                nargs: 2,
            },
            // boundary: (id, the containment mask); the answer span lands
            // on the same overlay words
            Uop::Mov { dst: 2, src: 0 },
            Uop::Kimm { dst: 3, kidx: k(4) },
            Uop::Call {
                callee: k(2),
                nargs: 2,
            },
            Uop::Mov { dst: 0, src: 3 },
            Uop::Ret { src: 0 },
        ],
    )
}

/// `() -> int` over the count-join seam: open, spawn `child`, join, return
/// the failed-children count word.
fn count_level_def(name: &str, child: u32) -> FunctionDef {
    def(
        name,
        &[Raw, Raw, Raw, Raw],
        0,
        2,
        vec![
            builtin(BUILTIN_NURSERY_OPEN),
            builtin(BUILTIN_NURSERY_SPAWN),
            builtin(BUILTIN_NURSERY_JOIN),
            ic_ref(child),
        ],
        &[
            Uop::Funcf { frame_slots: 4 },
            Uop::Call {
                callee: k(0),
                nargs: 0,
            },
            Uop::Mov { dst: 0, src: 2 },
            Uop::Kconst { dst: 1, kidx: k(3) },
            Uop::Mov { dst: 2, src: 0 },
            Uop::Mov { dst: 3, src: 1 },
            Uop::Call {
                callee: k(1),
                nargs: 2,
            },
            // join: (id); the trapped count lands on overlay word 0
            Uop::Mov { dst: 2, src: 0 },
            Uop::Call {
                callee: k(2),
                nargs: 1,
            },
            Uop::Mov { dst: 0, src: 2 },
            Uop::Ret { src: 0 },
        ],
    )
}

/// A boundary-chain module `depth` levels deep: [0] the base child, then
/// one level function per level, each spawning the level below it.
fn boundary_chain(depth: u32) -> ModuleRegistry {
    // indices: 0 = base, level i (1..=depth) sits at i
    let mut fns = vec![base_def()];
    for level in 1..=depth {
        fns.push(boundary_level_def(&format!("level_{level}"), level - 1, 0));
    }
    let mut reg = ModuleRegistry::new();
    reg.install(ModuleVersion::new("ic", fns).expect("valid fixture module"));
    reg
}

/// A count-chain module `depth` levels deep over a trapping deepest child.
fn count_chain(depth: u32) -> ModuleRegistry {
    // indices: 0 = trap, level i (1..=depth) sits at i
    let mut fns = vec![trap_def()];
    for level in 1..=depth {
        fns.push(count_level_def(&format!("count_level_{level}"), level - 1));
    }
    let mut reg = ModuleRegistry::new();
    reg.install(ModuleVersion::new("ic", fns).expect("valid fixture module"));
    reg
}

/// The two-level boundary chain: root joins N1, whose child joins N2 (the
/// base grandchild's nursery) — the answer span delivers at both levels and
/// every slot consumes.
#[test]
fn a_two_level_boundary_chain_delivers_end_to_end() {
    let mut vm = Vm::new(boundary_chain(2));
    assert_eq!(
        vm.run("ic", "level_2", &[]),
        Ok(FrameAnswer::one(Value::I64(0)))
    );
    for id in 0..=1 {
        assert_eq!(
            vm.nursery_state(NurseryId(id)),
            Some(NurseryState::Dead),
            "nursery {id} consumed"
        );
    }
}

/// The three-level chain: two nested interlock frames stand at once (N1's
/// child parked joining N2, whose child parked joining N3) — LIFO frames
/// drain them all.
#[test]
fn a_three_level_boundary_chain_delivers_end_to_end() {
    let mut vm = Vm::new(boundary_chain(3));
    assert_eq!(
        vm.run("ic", "level_3", &[]),
        Ok(FrameAnswer::one(Value::I64(0)))
    );
    for id in 0..=2 {
        assert_eq!(
            vm.nursery_state(NurseryId(id)),
            Some(NurseryState::Dead),
            "nursery {id} consumed"
        );
    }
}

/// The count-join seam across levels: the deepest level's trapped child
/// counts 1 there; that level lands `Completed(1)`, so the outer levels
/// count 0 — each joiner's answer rides its own frame's delivery.
#[test]
fn a_count_join_chain_surfaces_each_levels_count() {
    let mut vm = Vm::new(count_chain(2));
    assert_eq!(
        vm.run("ic", "count_level_2", &[]),
        Ok(FrameAnswer::one(Value::I64(0)))
    );
    // the inner trap raised only its own nursery's flag
    assert_eq!(vm.nursery_cancelled(NurseryId(1)), Some(true));
    assert_eq!(vm.nursery_cancelled(NurseryId(0)), Some(false));
    let mut vm = Vm::new(count_chain(1));
    assert_eq!(
        vm.run("ic", "count_level_1", &[]),
        Ok(FrameAnswer::one(Value::I64(1)))
    );
}

/// The deep chain: a generated depth-1024 boundary chain lands — the
/// interlock is iterative, so program nesting depth costs heap, never the
/// Rust stack (the recursive driver overflowed the process near depth 50).
#[test]
fn a_deep_chain_lands_without_growing_the_rust_stack() {
    let mut vm = Vm::new(boundary_chain(1024));
    assert_eq!(
        vm.run("ic", "level_1024", &[]),
        Ok(FrameAnswer::one(Value::I64(0)))
    );
}

/// The module for the interlock-cycle refusal: root joins N1 (its drive
/// stands), N1's child joins N2, and N2's child — a grandchild with no
/// frame of its own — joins N1, whose drive is in flight. Indices:
/// [0] the grandchild `second`, [1] the mid child `mid`, [2] main.
fn cycle_registry() -> ModuleRegistry {
    // the grandchild: join nursery 0 (N1, opened by main first) — the
    // join refuses while N1's drive stands, and the refusal trap rides the
    // boundaries out
    let second = def(
        "second",
        &[Raw, Raw],
        0,
        1,
        vec![builtin(BUILTIN_NURSERY_JOIN), Const::I64(0)],
        &[
            Uop::Funcf { frame_slots: 2 },
            Uop::Kimm { dst: 1, kidx: k(1) },
            Uop::Call {
                callee: k(0),
                nargs: 1,
            },
            Uop::Ret { src: 1 },
        ],
    );
    // the mid child: open N2, spawn the grandchild, boundary-join N2
    let mid = def(
        "mid",
        &[Raw, Raw, Raw, Raw],
        0,
        2,
        vec![
            builtin(BUILTIN_NURSERY_OPEN),
            builtin(BUILTIN_NURSERY_SPAWN),
            builtin(BUILTIN_NURSERY_BOUNDARY),
            ic_ref(0),
            Const::I64(0),
        ],
        &[
            Uop::Funcf { frame_slots: 4 },
            Uop::Call {
                callee: k(0),
                nargs: 0,
            },
            Uop::Mov { dst: 0, src: 2 },
            Uop::Kconst { dst: 1, kidx: k(3) },
            Uop::Mov { dst: 2, src: 0 },
            Uop::Mov { dst: 3, src: 1 },
            Uop::Call {
                callee: k(1),
                nargs: 2,
            },
            Uop::Mov { dst: 2, src: 0 },
            Uop::Kimm { dst: 3, kidx: k(4) },
            Uop::Call {
                callee: k(2),
                nargs: 2,
            },
            Uop::Mov { dst: 0, src: 3 },
            Uop::Ret { src: 0 },
        ],
    );
    // main: open N1, spawn the mid child, boundary-join N1
    let main = boundary_level_def("level", 1, 0);
    let mut reg = ModuleRegistry::new();
    reg.install(ModuleVersion::new("ic", vec![second, mid, main]).expect("valid fixture module"));
    reg
}

/// A second joiner of a nursery whose drive stands refuses: the two drives
/// would interlock on each other (the own-nursery deadlock one level up),
/// and the refusal surfaces at the outer boundary like any child failure.
#[test]
fn a_second_joiner_of_a_driven_nursery_refuses() {
    let mut vm = Vm::new(cycle_registry());
    assert_eq!(
        vm.run("ic", "level", &[]),
        Err(Trap::UnknownCallee {
            module: "nursery".into(),
            func: 0,
        })
    );
}

/// The gone-joiner shape under nesting: N2's grandchild resumes the parked
/// mid child mid-drive (its boundary call answers with the resume payload),
/// the moved-on joiner traps, and the trap surfaces at the OUTER boundary —
/// deterministically across runs. Indices: [0] `reopener`, [1] `mid`,
/// [2] main.
fn gone_registry() -> ModuleRegistry {
    // the grandchild: resume pool fiber 1 (the parked mid child) with 0,
    // then land
    let reopener = def(
        "reopener",
        &[I64, Raw, Raw],
        0,
        2,
        vec![Const::I64(1), Const::I64(0), builtin(BUILTIN_FIBER_RESUME)],
        &[
            Uop::Funcf { frame_slots: 3 },
            Uop::Kimm { dst: 1, kidx: k(0) },
            Uop::Kimm { dst: 2, kidx: k(1) },
            Uop::Call {
                callee: k(2),
                nargs: 2,
            },
            Uop::Ret { src: 0 },
        ],
    );
    // the mid child: open N2, spawn the reopener, boundary-join N2 — the
    // park breaks early when the reopener resumes it, and the moved-on
    // continuation dies on a divide by zero
    let mid = def(
        "mid",
        &[I64, I64, Raw, Raw, Raw],
        0,
        2,
        vec![
            builtin(BUILTIN_NURSERY_OPEN),
            builtin(BUILTIN_NURSERY_SPAWN),
            builtin(BUILTIN_NURSERY_BOUNDARY),
            ic_ref(0),
            Const::I64(0),
        ],
        &[
            Uop::Funcf { frame_slots: 5 },
            // open N2: the id lands on overlay word 0 (slot 3)
            Uop::Call {
                callee: k(0),
                nargs: 0,
            },
            Uop::Mov { dst: 2, src: 3 }, // keep the id in a body word
            Uop::Kconst { dst: 4, kidx: k(3) }, // the callable rides the id
            Uop::Call {
                callee: k(1),
                nargs: 2,
            },
            // boundary: (id, mask) staged on the same overlay words; the
            // park here is what the reopener breaks
            Uop::Mov { dst: 3, src: 2 },
            Uop::Kimm { dst: 4, kidx: k(4) },
            Uop::Call {
                callee: k(2),
                nargs: 2,
            },
            // the resume payload already answered the call: word 4 holds 0
            Uop::Kimm { dst: 1, kidx: k(4) },
            Uop::DivI64 {
                dst: 0,
                lhs: 0,
                rhs: 1,
            },
            Uop::Ret { src: 0 }, // unreachable under the trap
        ],
    );
    // main: open N1, spawn the mid child, boundary-join N1
    let main = boundary_level_def("level", 1, 0);
    let mut reg = ModuleRegistry::new();
    reg.install(ModuleVersion::new("ic", vec![reopener, mid, main]).expect("valid fixture module"));
    reg
}

/// A gone joiner under nesting: the nested frame's answer finds no parked
/// call site (the reopener moved the joiner on), the moved-on joiner's
/// trap is ledgered into the outer nursery, and the outer boundary raises
/// it — the same structured stop on every run.
#[test]
fn a_nested_gone_joiners_trap_surfaces_at_the_outer_boundary() {
    for _ in 0..10 {
        let mut vm = Vm::new(gone_registry());
        assert_eq!(vm.run("ic", "level", &[]), Err(Trap::DivideByZero));
        // both nurseries consumed on the failure path
        for id in 0..=1 {
            assert_eq!(vm.nursery_state(NurseryId(id)), Some(NurseryState::Dead));
        }
    }
}

/// Teardown — the cancel-first host face — drains a cross-joining child:
/// the flag rises on the outer nursery before the child runs, the child
/// still completes its own join (the inner nursery's flag never rose, and
/// the delivery re-enters the parked joiner past the outer flag), and a
/// post-join back edge then observes the outer flag and stops there.
#[test]
fn teardown_drains_a_cross_joining_child() {
    use ds_vm::{ChildOutcome, FiberId, FiberState};

    // the child: open N2, spawn the base, boundary-join N2, then a back
    // edge that can only stop under the outer nursery's raised flag
    // (body words 0..=2, overlay words 3..=4)
    let looper = def(
        "looper",
        &[Raw, Raw, Raw, Raw, Raw],
        0,
        2,
        vec![
            builtin(BUILTIN_NURSERY_OPEN),
            builtin(BUILTIN_NURSERY_SPAWN),
            builtin(BUILTIN_NURSERY_BOUNDARY),
            ic_ref(0),
            Const::I64(0),
            Const::I64(1),
        ],
        &[
            Uop::Funcf { frame_slots: 5 },
            // open: the fresh nursery's id lands on overlay word 0 (slot 3)
            Uop::Call {
                callee: k(0),
                nargs: 0,
            },
            Uop::Mov { dst: 0, src: 3 },
            // spawn: the callable rides the id on overlay word 1 (slot 4)
            Uop::Kconst { dst: 4, kidx: k(3) },
            Uop::Call {
                callee: k(1),
                nargs: 2,
            },
            // boundary: (id, mask) staged on the same overlay words
            Uop::Mov { dst: 3, src: 0 },
            Uop::Kimm { dst: 4, kidx: k(4) },
            Uop::Call {
                callee: k(2),
                nargs: 2,
            },
            // the join answered: the back edge observes the outer flag at
            // its first taken branch
            Uop::Kimm { dst: 1, kidx: k(5) },
            Uop::AddI64 {
                dst: 1,
                lhs: 1,
                rhs: 1,
            },
            Uop::Jmp { target: 9 },
        ],
    );
    let mut reg = ModuleRegistry::new();
    reg.install(ModuleVersion::new("ic", vec![base_def(), looper]).expect("valid fixture module"));
    let mut vm = Vm::new(reg);
    let outer = vm.open_root_nursery();
    vm.spawn_fiber_in(outer, "ic", "looper", &[])
        .expect("spawn");
    // teardown: the flag rises first, the child completes its inner join
    // anyway, and its post-join loop stops at the first back edge
    assert_eq!(
        vm.teardown_nursery(outer),
        Ok(vec![ChildOutcome::Cancelled])
    );
    assert_eq!(vm.nursery_state(NurseryId(0)), Some(NurseryState::Dead));
    assert_eq!(vm.nursery_state(NurseryId(1)), Some(NurseryState::Dead));
    assert_eq!(vm.fiber_state(FiberId(1)), Some(FiberState::Dead));
    assert_eq!(vm.fiber_state(FiberId(2)), Some(FiberState::Dead));
}
