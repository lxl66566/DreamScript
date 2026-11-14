//! Fiber substrate fixtures [E2, H2]: hand-assembled programs over the
//! debug control pair (`fiber_yield`/`fiber_resume`, builtin indices
//! 24-25) and the host face (`spawn_fiber`/`resume_fiber`). What the
//! fixtures pin:
//!
//! - ping-pong: root resumes fiber A, A resumes fiber B, B yields back to A — the value thread (1,
//!   3, ... 21) is observable proof of the interleaving, and each fiber's locals survive
//!   suspension.
//! - join: a fiber completing at its entry frame marks itself dead and switches back to its
//!   resumer, delivering the final value (here a string `Ref`) to the parked resume call.
//! - the `run` bounce: a root yielding to the host is re-entered with the bare `PLATFORM_NONE` tag
//!   — a run entry drives to completion.
//! - per-fiber TCO: a tail-looping fiber runs a million iterations at a constant segment
//!   (high-water == frame size, one chunk).
//! - chunk-append growth: deep non-tail recursion grows the segment by appended chunks with earlier
//!   frame addresses stable across growth.
//! - the O(1) switch: a parked fiber's top-frame word keeps its address across switch round-trips —
//!   no stack is ever copied.
//! - the delivered count: a completing child's `Ref` answer keeps exactly its transferred count
//!   over the dead child — the crossing mutates no count, and everything else in the child drained.
//! - hosted-drive honesty: a generic combinator's lambda yielding under `Vm::call_hosted` refuses
//!   the drive, and the interrupted fiber can never be re-entered — a second resume answers the
//!   structured fiber refusal, not the sentinel return pc's InvalidPc lie.
//!
//! Layout conventions (as everywhere): overlay slots that stage arguments
//! and then receive results declare `Raw`; fiber ids are i64 immediates
//! (the pool's root is id 0, spawns number up from 1).

use ds_vm::{
    Const, ConstFuncRef, FIBER_ROOT, FiberId, FiberState, FunctionDef, ModuleRegistry,
    ModuleVersion, Trap, Value, Vm,
    abi::{
        BuiltinInstance, ConstIdx, FrameLayout, SlotType,
        SlotType::{Bool, I64, Raw, Ref},
        uop::Uop,
    },
    assemble,
    native::{
        BUILTIN_FIBER_RESUME, BUILTIN_FIBER_YIELD, BUILTIN_LIST_NEW, BUILTIN_LIST_PUSH,
        BUILTIN_MAP, PLATFORM_NONE,
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

/// A same-module callee by function-table index (the fixture module "fb").
fn fb_ref(index: u32) -> Const {
    Const::FuncRef(ConstFuncRef::Named {
        module: "fb".into(),
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

/// `() -> int`: resume fiber 1 (`a`) with seed 1; answer its final value.
fn main_def() -> FunctionDef {
    def(
        "main",
        &[Raw, Raw],
        0,
        2,
        vec![
            Const::I64(1),                 // k0 seed
            Const::I64(1),                 // k1 a's id
            builtin(BUILTIN_FIBER_RESUME), // k2
        ],
        &[
            Uop::Funcf { frame_slots: 2 },
            Uop::Kimm { dst: 0, kidx: k(1) },
            Uop::Kimm { dst: 1, kidx: k(0) },
            Uop::Call {
                callee: k(2),
                nargs: 2,
            },
            Uop::Ret { src: 0 },
        ],
    )
}

/// `(v: int) -> int`: until `v >= 21`, resume fiber 2 (`b`) with `v + 1`.
fn a_def() -> FunctionDef {
    def(
        "a",
        &[I64, I64, I64, Bool, Raw, Raw],
        1,
        2,
        vec![
            Const::I64(1),                 // k0 one
            Const::I64(2),                 // k1 b's id
            Const::I64(21),                // k2 limit
            builtin(BUILTIN_FIBER_RESUME), // k3
        ],
        &[
            Uop::Funcf { frame_slots: 6 },
            Uop::Kimm { dst: 1, kidx: k(0) },
            Uop::Kimm { dst: 2, kidx: k(2) },
            Uop::GeI64 {
                dst: 3,
                lhs: 0,
                rhs: 2,
            },
            Uop::Jmpif {
                cond: 3,
                target: 10,
            },
            Uop::Kimm { dst: 4, kidx: k(1) },
            Uop::AddI64 {
                dst: 5,
                lhs: 0,
                rhs: 1,
            },
            Uop::Call {
                callee: k(3),
                nargs: 2,
            },
            Uop::Mov { dst: 0, src: 4 },
            Uop::Jmp { target: 3 },
            Uop::Ret { src: 0 },
        ],
    )
}

/// `(v: int) -> int`: yield `v + 1` forever (the a<->b ping-pong's ball).
fn b_def() -> FunctionDef {
    def(
        "b",
        &[I64, I64, Raw],
        1,
        1,
        vec![Const::I64(1), builtin(BUILTIN_FIBER_YIELD)],
        &[
            Uop::Funcf { frame_slots: 3 },
            Uop::Kimm { dst: 1, kidx: k(0) },
            Uop::AddI64 {
                dst: 0,
                lhs: 0,
                rhs: 1,
            },
            Uop::Mov { dst: 2, src: 0 },
            Uop::Call {
                callee: k(1),
                nargs: 1,
            },
            Uop::Mov { dst: 0, src: 2 },
            Uop::Jmp { target: 2 },
        ],
    )
}

/// `() -> string`: resume fiber 1 (the child); answer its final value.
fn join_main_def() -> FunctionDef {
    def(
        "join_main",
        &[Raw, Raw],
        0,
        2,
        vec![
            Const::I64(1),                 // k0 child id
            Const::I64(0),                 // k1 dropped payload
            builtin(BUILTIN_FIBER_RESUME), // k2
        ],
        &[
            Uop::Funcf { frame_slots: 2 },
            Uop::Kimm { dst: 0, kidx: k(0) },
            Uop::Kimm { dst: 1, kidx: k(1) },
            Uop::Call {
                callee: k(2),
                nargs: 2,
            },
            Uop::Ret { src: 0 },
        ],
    )
}

/// `() -> string`: completes immediately.
fn join_child_def() -> FunctionDef {
    def(
        "join_child",
        &[Ref],
        0,
        0,
        vec![Const::Str("done".into())],
        &[
            Uop::Funcf { frame_slots: 1 },
            Uop::Kstr { dst: 0, kidx: k(0) },
            Uop::Ret { src: 0 },
        ],
    )
}

/// `() -> int`: the root yields — `run` must bounce with the none tag.
fn bounce_main_def() -> FunctionDef {
    def(
        "bounce_main",
        &[I64, Bool, Raw],
        0,
        1,
        vec![
            Const::I64(7),                // k0 staged value
            Const::I64(1),                // k1
            Const::I64(0),                // k2
            builtin(BUILTIN_FIBER_YIELD), // k3
        ],
        &[
            Uop::Funcf { frame_slots: 3 },
            Uop::Kimm { dst: 0, kidx: k(0) },
            Uop::Mov { dst: 2, src: 0 },
            Uop::Call {
                callee: k(3),
                nargs: 1,
            },
            Uop::TagTest {
                dst: 1,
                src: 2,
                tag: PLATFORM_NONE,
            },
            Uop::Jmpif { cond: 1, target: 8 },
            Uop::Kimm { dst: 0, kidx: k(2) },
            Uop::Ret { src: 0 },
            Uop::Kimm { dst: 0, kidx: k(1) },
            Uop::Ret { src: 0 },
        ],
    )
}

/// `(n, acc: int) -> int`: the tail loop — CALLT self, frame reused.
fn spin_def() -> FunctionDef {
    def(
        "spin",
        &[I64, I64, I64, Bool, Raw, Raw],
        2,
        2,
        vec![
            Const::I64(1), // k0 one
            Const::I64(0), // k1 zero
            fb_ref(6),     // k2 spin
        ],
        &[
            Uop::Funcf { frame_slots: 6 },
            Uop::Kimm { dst: 2, kidx: k(1) },
            Uop::EqI64 {
                dst: 3,
                lhs: 0,
                rhs: 2,
            },
            Uop::Jmpif {
                cond: 3,
                target: 10,
            },
            Uop::Kimm { dst: 2, kidx: k(0) },
            Uop::SubI64 {
                dst: 4,
                lhs: 0,
                rhs: 2,
            },
            Uop::AddI64 {
                dst: 5,
                lhs: 1,
                rhs: 2,
            },
            Uop::Callt {
                callee: k(2),
                nargs: 2,
            },
            Uop::Ret { src: 0 }, // unreachable under the tail call
            Uop::Ret { src: 0 },
            Uop::Ret { src: 1 },
        ],
    )
}

/// `(n: int) -> int`: non-tail descent; at `n == 200` the fiber yields
/// mid-recursion (the host snapshots), then descends to zero.
fn deep_def() -> FunctionDef {
    def(
        "deep",
        &[I64, I64, Bool, Raw],
        1,
        1,
        vec![
            Const::I64(1),                // k0 one
            Const::I64(200),              // k1 the mark
            Const::I64(0),                // k2 zero
            builtin(BUILTIN_FIBER_YIELD), // k3
            fb_ref(7),                    // k4 deep
        ],
        &[
            Uop::Funcf { frame_slots: 4 },
            Uop::Kimm { dst: 1, kidx: k(2) },
            Uop::EqI64 {
                dst: 2,
                lhs: 0,
                rhs: 1,
            },
            Uop::Jmpif {
                cond: 2,
                target: 13,
            }, // n == 0: base case
            Uop::Kimm { dst: 1, kidx: k(1) },
            Uop::EqI64 {
                dst: 2,
                lhs: 0,
                rhs: 1,
            },
            Uop::JmpifNot { cond: 2, target: 9 }, // plain descent
            Uop::Mov { dst: 3, src: 0 },          // the mark: yield n, park
            Uop::Call {
                callee: k(3),
                nargs: 1,
            },
            Uop::Kimm { dst: 1, kidx: k(0) }, // 9: the resume lands here too
            Uop::SubI64 {
                dst: 3,
                lhs: 0,
                rhs: 1,
            },
            Uop::Call {
                callee: k(4),
                nargs: 1,
            },
            Uop::Ret { src: 3 },
            Uop::Ret { src: 0 }, // 13: echo zero back up
        ],
    )
}

/// `(v: int) -> int`: yield v, adopt the payload, yield again, return.
fn echo_def() -> FunctionDef {
    def(
        "echo",
        &[I64, Raw],
        1,
        1,
        vec![builtin(BUILTIN_FIBER_YIELD)],
        &[
            Uop::Funcf { frame_slots: 2 },
            Uop::Mov { dst: 1, src: 0 },
            Uop::Call {
                callee: k(0),
                nargs: 1,
            },
            Uop::Mov { dst: 0, src: 1 },
            Uop::Mov { dst: 1, src: 0 },
            Uop::Call {
                callee: k(0),
                nargs: 1,
            },
            Uop::Mov { dst: 0, src: 1 },
            Uop::Ret { src: 0 },
        ],
    )
}

/// `() -> int`: builds a one-element list, then maps the yielding lambda
/// over it through CALLG — the hosted drive whose refusal interrupts the
/// fiber.
fn hosted_map_def() -> FunctionDef {
    def(
        "hosted_map",
        &[Ref, Raw, Raw, Raw, Raw],
        0,
        4,
        vec![
            builtin(BUILTIN_LIST_NEW),                // k0
            builtin(BUILTIN_LIST_PUSH),               // k1
            Const::I64(7),                            // k2 the element
            builtin(BUILTIN_MAP),                     // k3
            fb_ref(10),                               // k4 the yielding lambda
            Const::I64(BuiltinInstance::List as i64), // k5 the evidence words
        ],
        &[
            Uop::Funcf { frame_slots: 5 },
            Uop::Call {
                callee: k(0),
                nargs: 0,
            }, // fresh list → overlay 0
            Uop::Mov { dst: 0, src: 1 }, // keep the list (push answers unit)
            Uop::Mov { dst: 1, src: 0 }, // list_push: (list, element)
            Uop::Kimm { dst: 2, kidx: k(2) },
            Uop::Call {
                callee: k(1),
                nargs: 2,
            },
            Uop::Mov { dst: 1, src: 0 }, // map: (list, lambda) + evidence
            Uop::Kconst { dst: 2, kidx: k(4) },
            Uop::Kimm { dst: 3, kidx: k(5) },
            Uop::Kimm { dst: 4, kidx: k(5) },
            Uop::Callg {
                callee: k(3),
                nargs: 2,
            },
            Uop::Ret { src: 1 },
        ],
    )
}

/// `(x: int) -> int`: yields `x` — parking the fiber that hosts the
/// drive — then answers the resume payload as the mapped word.
fn yielding_lambda_def() -> FunctionDef {
    def(
        "yielding_lambda",
        &[I64, Raw],
        1,
        1,
        vec![builtin(BUILTIN_FIBER_YIELD)],
        &[
            Uop::Funcf { frame_slots: 2 },
            Uop::Mov { dst: 1, src: 0 },
            Uop::Call {
                callee: k(0),
                nargs: 1,
            },
            Uop::Ret { src: 1 },
        ],
    )
}

/// The fixture module. Function indices are pool-FuncRef contract:
/// 0 main, 1 a, 2 b, 3 join_main, 4 join_child, 5 bounce_main, 6 spin,
/// 7 deep, 8 echo, 9 hosted_map, 10 yielding_lambda.
fn fb_registry() -> ModuleRegistry {
    let mut reg = ModuleRegistry::new();
    reg.install(
        ModuleVersion::new("fb", vec![
            main_def(),
            a_def(),
            b_def(),
            join_main_def(),
            join_child_def(),
            bounce_main_def(),
            spin_def(),
            deep_def(),
            echo_def(),
            hosted_map_def(),
            yielding_lambda_def(),
        ])
        .expect("valid fixture module"),
    );
    reg
}

const DEEP_FRAME_STEP: usize = 4; // words each recursion level adds (arg overlay offset)

#[test]
fn two_fibers_ping_pong_through_resume_and_yield() {
    let mut vm = Vm::new(fb_registry());
    // the root is id 0; spawns number up: a = 1, b = 2
    let a = vm
        .spawn_fiber("fb", "a", &[Value::I64(1)])
        .expect("spawn a");
    let b = vm
        .spawn_fiber("fb", "b", &[Value::I64(0)])
        .expect("spawn b");
    assert_eq!((a, b), (FiberId(1), FiberId(2)));
    // root resumes a (seed 1); a resumes b; b yields back to a; ... the
    // thread 1, 3, ..., 21 alternates a<->b eleven times before a stops
    assert_eq!(vm.run("fb", "main", &[]), Ok(Value::I64(21)));
    assert_eq!(vm.fiber_state(FIBER_ROOT), Some(FiberState::Dead));
    assert_eq!(vm.fiber_state(a), Some(FiberState::Dead));
    // b is parked at its yield with a (dead) resumer: a leftover, not a leak
    assert_eq!(vm.fiber_state(b), Some(FiberState::Suspended));
}

#[test]
fn a_fiber_completing_switches_back_with_its_final_value() {
    let mut vm = Vm::new(fb_registry());
    let child = vm
        .spawn_fiber("fb", "join_child", &[])
        .expect("spawn child");
    assert_eq!(child, FiberId(1));
    let joined = vm.run("fb", "join_main", &[]).expect("join runs");
    // the child's final value — a string Ref — rode the parked resume call
    let Value::Ref(id) = joined else {
        panic!("a string ref, got {joined:?}");
    };
    assert_eq!(vm.objects_of(id).str_at(id.id()), Ok("done"));
    assert_eq!(vm.fiber_state(child), Some(FiberState::Dead));
}

#[test]
fn a_root_yielding_under_run_bounces_with_the_none_tag() {
    let mut vm = Vm::new(fb_registry());
    // run drives to completion: the root's yield is answered with the bare
    // PLATFORM_NONE tag and the root branches on it
    assert_eq!(vm.run("fb", "bounce_main", &[]), Ok(Value::I64(1)));
    assert_eq!(vm.fiber_state(FIBER_ROOT), Some(FiberState::Dead));
}

#[test]
fn a_tail_looping_fiber_never_grows_its_segment() {
    let mut vm = Vm::new(fb_registry());
    let spin = vm
        .spawn_fiber("fb", "spin", &[Value::I64(1_000_000), Value::I64(0)])
        .expect("spawn spin");
    // a Ready fiber's first entry drops the payload
    assert_eq!(
        vm.resume_fiber(spin, Value::I64(0)),
        Ok(Value::I64(1_000_000))
    );
    // per-fiber TCO: the high-water is the entry frame, one chunk, held
    assert_eq!(vm.fiber_max_len(spin), Some(8));
    assert_eq!(vm.fiber_chunk_count(spin), Some(1));
    assert_eq!(vm.fiber_state(spin), Some(FiberState::Dead));
}

#[test]
fn deep_recursion_grows_by_chunk_append_with_stable_addresses() {
    let mut vm = Vm::new(fb_registry());
    let deep = vm
        .spawn_fiber("fb", "deep", &[Value::I64(401)])
        .expect("spawn deep");
    // descends 401 -> 200, then parks mid-recursion at its yield
    assert_eq!(vm.resume_fiber(deep, Value::I64(0)), Ok(Value::I64(200)));
    assert_eq!(vm.fiber_state(deep), Some(FiberState::Suspended));
    let fp = vm.fiber_top_fp(deep).expect("parked with live frames");
    let addr = vm.fiber_word_addr(deep, fp).expect("the top frame's base");
    let chunks = vm.fiber_chunk_count(deep).expect("segment exists");
    assert!(
        chunks > 1,
        "the descent to depth 200 already crossed a chunk boundary"
    );
    // continue to zero: ~400 frames total, grown by appended chunks
    assert_eq!(vm.resume_fiber(deep, Value::I64(0)), Ok(Value::I64(0)));
    assert_eq!(vm.fiber_state(deep), Some(FiberState::Dead));
    assert!(
        vm.fiber_chunk_count(deep).is_some_and(|c| c > chunks),
        "the deeper descent appended chunks"
    );
    assert_eq!(
        vm.fiber_word_addr(deep, fp),
        Some(addr),
        "the earlier frame's address survived the growth: no realloc, no copy"
    );
    let high = vm.fiber_max_len(deep).expect("the segment grew");
    assert!(
        high >= 401 * DEEP_FRAME_STEP,
        "the high-water covers the full descent, got {high}"
    );
}

#[test]
fn a_switch_round_trip_copies_no_words() {
    let mut vm = Vm::new(fb_registry());
    let echo = vm
        .spawn_fiber("fb", "echo", &[Value::I64(5)])
        .expect("spawn echo");
    // first entry yields 5 (the host payload is dropped: the fiber is Ready)
    assert_eq!(vm.resume_fiber(echo, Value::I64(0)), Ok(Value::I64(5)));
    let fp = vm.fiber_top_fp(echo).expect("parked with live frames");
    let addr = vm.fiber_word_addr(echo, fp).expect("top frame base");
    let chunks = vm.fiber_chunk_count(echo).expect("segment exists");
    // park and re-enter: the same frame must sit at the same address
    assert_eq!(vm.resume_fiber(echo, Value::I64(9)), Ok(Value::I64(9)));
    assert_eq!(vm.fiber_word_addr(echo, fp), Some(addr));
    assert_eq!(vm.fiber_chunk_count(echo), Some(chunks));
    // and the fiber completes: 11 rides the last switch in, RET completes
    assert_eq!(vm.resume_fiber(echo, Value::I64(11)), Ok(Value::I64(11)));
    assert_eq!(vm.fiber_state(echo), Some(FiberState::Dead));
}

#[test]
fn resuming_an_unresumable_fiber_names_it_in_the_trap() {
    let mut vm = Vm::new(fb_registry());
    let echo = vm
        .spawn_fiber("fb", "echo", &[Value::I64(0)])
        .expect("spawn echo");
    for payload in [Value::I64(1), Value::I64(2), Value::I64(3)] {
        vm.resume_fiber(echo, payload)
            .expect("drives to completion");
    }
    assert_eq!(vm.fiber_state(echo), Some(FiberState::Dead));
    assert_eq!(
        vm.resume_fiber(echo, Value::I64(0)),
        Err(Trap::UnknownCallee {
            module: "fiber".into(),
            func: echo.0,
        })
    );
    // an id the pool never held answers the same trap
    assert_eq!(
        vm.resume_fiber(FiberId(99), Value::I64(0)),
        Err(Trap::UnknownCallee {
            module: "fiber".into(),
            func: 99,
        })
    );
}

#[test]
fn a_refused_hosted_drive_leaves_the_fiber_never_resumable() {
    let mut vm = Vm::new(fb_registry());
    let hosted = vm
        .spawn_fiber("fb", "hosted_map", &[])
        .expect("spawn the hosted driver");
    // the map body's lambda yields: a Rust-side loop cannot suspend, so
    // the hosted drive refuses with the fiber parked mid-lambda
    assert_eq!(
        vm.resume_fiber(hosted, Value::I64(0)),
        Err(Trap::ReentrancyRefused)
    );
    // the sentinel reentry frame can never be answered: the fiber is
    // interrupted (frames kept, like any trapped drive), and a second
    // resume answers the structured fiber refusal — never the sentinel
    // return pc's InvalidPc lie
    assert_eq!(vm.fiber_state(hosted), Some(FiberState::Running));
    assert_eq!(
        vm.resume_fiber(hosted, Value::I64(0)),
        Err(Trap::UnknownCallee {
            module: "fiber".into(),
            func: hosted.0,
        })
    );
}

/// A delivered `Ref` keeps exactly its transferred count over its dead
/// child: the entry frame's KSTR claim rode out with the returned word
/// (the crossing mutates no count), and everything else in the child
/// drained at its death walk. A per-fiber heap split must keep this
/// shape — the child's drain may not free the delivered word's object.
#[test]
fn a_delivered_ref_keeps_one_transferred_count_over_a_dead_child() {
    let mut vm = Vm::new(fb_registry());
    let child = vm
        .spawn_fiber("fb", "join_child", &[])
        .expect("spawn child");
    let delivered = vm
        .resume_fiber(child, Value::I64(0))
        .expect("the child completes");
    let Value::Ref(id) = delivered else {
        panic!("a string ref, got {delivered:?}");
    };
    assert_eq!(vm.fiber_state(child), Some(FiberState::Dead));
    assert_eq!(vm.objects().rc_at(id.id()), Ok(1));
    assert_eq!(vm.objects().str_at(id.id()), Ok("done"));
    assert_eq!(vm.objects().len(), 1, "only the delivered word stays live");
}
