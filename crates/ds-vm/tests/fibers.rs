//! Fiber substrate fixtures [E2, H2]: hand-assembled programs over the
//! debug control switch (`fiber_resume`, builtin index 25), the host
//! face (`spawn_fiber`/`resume_fiber`), and the host's suspension
//! request (`request_suspend`, the dev host's capability face). What the
//! fixtures pin:
//!
//! - ping-pong: root resumes fiber A, A and B resume each other — the value thread (1, 3, ... 21)
//!   is observable proof of the interleaving, and each fiber's locals survive suspension.
//! - join: a fiber completing at its entry frame marks itself dead and switches back to its
//!   resumer, delivering the final value (here a string `Ref`) to the parked resume call.
//! - suspension injection: an armed request parks a loop-driving fiber at its first taken backward
//!   branch, the resume re-takes the branch (no call site stands, the payload drops), and the
//!   request never bites inside a hosted reentry frame — the hosted drive runs to completion and
//!   the entry parks after it.
//! - per-fiber TCO: a tail-looping fiber runs a million iterations at a constant segment
//!   (high-water == frame size, one chunk).
//! - chunk-append growth: deep non-tail recursion grows the segment by appended chunks with earlier
//!   frame addresses stable across growth.
//! - the O(1) switch: a parked fiber's top-frame word keeps its address across park/resume
//!   round-trips — no stack is ever copied.
//! - the delivered count: a completing child's `Ref` answer keeps exactly its transferred count
//!   over the dead child — the crossing mutates no count, and everything else in the child drained.
//!
//! Layout conventions (as everywhere): overlay slots that stage arguments
//! and then receive results declare `Raw`; fiber ids are i64 immediates
//! (the pool's root is id 0, spawns number up from 1).

use ds_vm::{
    Const, ConstFuncRef, FIBER_ROOT, FiberId, FiberState, FrameAnswer, FunctionDef, ModuleRegistry,
    ModuleVersion, Trap, Value, Vm,
    abi::{
        BuiltinInstance, ConstIdx, FrameLayout, SlotType,
        SlotType::{Bool, I64, Raw, Ref},
        uop::Uop,
    },
    assemble,
    native::{
        BUILTIN_FIBER_RESUME, BUILTIN_LIST_NEW, BUILTIN_LIST_PUSH, BUILTIN_MAP, PLATFORM_NONE,
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

/// `(v: int) -> int`: the ping-pong's other paddle. Every round adds one
/// and resumes `a` with the sum, adopting `a`'s next delivery as its own
/// value; at the limit it answers the last delivered word — never
/// resuming a dead `a`, so the drive ends on `b`'s death.
fn b_def() -> FunctionDef {
    def(
        "b",
        &[I64, I64, I64, Bool, Raw, Raw],
        1,
        2,
        vec![
            Const::I64(1),                 // k0 one
            Const::I64(1),                 // k1 a's id
            Const::I64(21),                // k2 the limit
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
                target: 11,
            },
            Uop::AddI64 {
                dst: 0,
                lhs: 0,
                rhs: 1,
            },
            Uop::Kimm { dst: 4, kidx: k(1) }, // restage a's id: the delivery
            // overlays the arg area's first word every round
            Uop::Mov { dst: 5, src: 0 },
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

/// `() -> int`: a plain loop driver — the host's suspension request parks
/// it at its first taken backward branch; the resume re-takes the branch
/// and the loop continues to completion.
fn park_main_def() -> FunctionDef {
    def(
        "park_main",
        &[I64, I64, I64, Bool],
        0,
        0,
        vec![Const::I64(1), Const::I64(3)],
        &[
            Uop::Funcf { frame_slots: 4 },
            Uop::Kimm { dst: 1, kidx: k(0) },
            Uop::Kimm { dst: 2, kidx: k(1) },
            Uop::LtI64 {
                dst: 3,
                lhs: 0,
                rhs: 2,
            },
            Uop::JmpifNot { cond: 3, target: 7 },
            Uop::AddI64 {
                dst: 0,
                lhs: 0,
                rhs: 1,
            },
            Uop::Jmp { target: 3 }, // the park point under a request
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

/// `(n: int) -> int`: non-tail descent; at `n == 200` the body's one-shot
/// backward branch parks the fiber under the host's suspension request
/// (the host snapshots mid-recursion), then descends to zero.
fn deep_def() -> FunctionDef {
    def(
        "deep",
        &[I64, I64, Bool, I64, Bool, Raw],
        1,
        1,
        vec![
            Const::I64(1),   // k0 one
            Const::I64(200), // k1 the mark
            Const::I64(0),   // k2 zero
            Const::I64(2),   // k3 two
            fb_ref(7),       // k4 deep
        ],
        &[
            Uop::Funcf { frame_slots: 6 },
            Uop::Kimm { dst: 1, kidx: k(2) },
            Uop::EqI64 {
                dst: 2,
                lhs: 0,
                rhs: 1,
            },
            Uop::Jmpif {
                cond: 2,
                target: 16,
            }, // n == 0: base case
            Uop::Kimm { dst: 1, kidx: k(1) },
            Uop::EqI64 {
                dst: 2,
                lhs: 0,
                rhs: 1,
            },
            Uop::JmpifNot {
                cond: 2,
                target: 12,
            }, // plain descent
            // the mark's one-shot park: the taken branch fires the armed
            // request; the resumed drive re-takes the branch, the second
            // pass recomputes the test false, and the descent continues
            Uop::Kimm { dst: 1, kidx: k(0) },
            Uop::AddI64 {
                dst: 3,
                lhs: 3,
                rhs: 1,
            },
            Uop::Kimm { dst: 5, kidx: k(3) },
            Uop::LtI64 {
                dst: 4,
                lhs: 3,
                rhs: 5,
            },
            Uop::Jmpif { cond: 4, target: 8 },
            Uop::Kimm { dst: 1, kidx: k(0) }, // 12: the resume lands here
            Uop::SubI64 {
                dst: 5,
                lhs: 0,
                rhs: 1,
            },
            Uop::Call {
                callee: k(4),
                nargs: 1,
            },
            Uop::Ret { src: 5 },
            Uop::Ret { src: 0 }, // 16: echo zero back up
        ],
    )
}

/// `(v: int) -> int`: two one-shot backward branches — two park points
/// under the host's suspension request — then answers `v + 1`.
fn echo_def() -> FunctionDef {
    def(
        "echo",
        &[I64, I64, I64, I64, Bool, I64, Bool],
        1,
        0,
        vec![Const::I64(1), Const::I64(2)],
        &[
            Uop::Funcf { frame_slots: 7 },
            Uop::Kimm { dst: 1, kidx: k(0) },
            Uop::AddI64 {
                dst: 3,
                lhs: 3,
                rhs: 1,
            },
            Uop::Kimm { dst: 2, kidx: k(1) },
            Uop::LtI64 {
                dst: 4,
                lhs: 3,
                rhs: 2,
            },
            Uop::Jmpif { cond: 4, target: 2 }, // park point one
            Uop::AddI64 {
                dst: 5,
                lhs: 5,
                rhs: 1,
            },
            Uop::LtI64 {
                dst: 6,
                lhs: 5,
                rhs: 2,
            },
            Uop::Jmpif { cond: 6, target: 6 }, // park point two
            Uop::AddI64 {
                dst: 0,
                lhs: 0,
                rhs: 1,
            },
            Uop::Ret { src: 0 },
        ],
    )
}

/// `() -> int`: builds a one-element list, then maps a looping lambda
/// over it through CALLG — the hosted drive the armed request must wait
/// out — parks at its own next back edge once the CALLG returns, and
/// answers the mapped list.
fn hosted_map_def() -> FunctionDef {
    def(
        "hosted_map",
        &[Ref, I64, Bool, Raw, Raw, Raw, Raw],
        0,
        4,
        vec![
            builtin(BUILTIN_LIST_NEW),                // k0
            builtin(BUILTIN_LIST_PUSH),               // k1
            Const::I64(7),                            // k2 the element
            builtin(BUILTIN_MAP),                     // k3
            fb_ref(10),                               // k4 the looping lambda
            Const::I64(BuiltinInstance::List as i64), // k5 the evidence words
            Const::I64(1),                            // k6 one
            Const::I64(2),                            // k7 two
        ],
        &[
            Uop::Funcf { frame_slots: 7 },
            Uop::Call {
                callee: k(0),
                nargs: 0,
            }, // fresh list → the arg area's word 0
            Uop::Mov { dst: 0, src: 3 }, // keep the list (push answers unit)
            Uop::Mov { dst: 3, src: 0 }, // list_push: (list, element)
            Uop::Kimm { dst: 4, kidx: k(2) },
            Uop::Call {
                callee: k(1),
                nargs: 2,
            },
            Uop::Mov { dst: 3, src: 0 }, // map: (list, lambda) + evidence
            Uop::Kconst { dst: 4, kidx: k(4) },
            Uop::Kimm { dst: 5, kidx: k(5) },
            Uop::Kimm { dst: 6, kidx: k(5) },
            Uop::Callg {
                callee: k(3),
                nargs: 2,
            },
            // the one-shot park: the request waits out the hosted map
            // drive above and fires here, after the CALLG
            Uop::Kimm { dst: 5, kidx: k(6) },
            Uop::AddI64 {
                dst: 1,
                lhs: 1,
                rhs: 5,
            },
            Uop::Kimm { dst: 5, kidx: k(7) },
            Uop::LtI64 {
                dst: 2,
                lhs: 1,
                rhs: 5,
            },
            Uop::Jmpif {
                cond: 2,
                target: 12,
            },
            Uop::Ret { src: 3 },
        ],
    )
}

/// `(x: int) -> int`: runs `x` backward-branch iterations — taken inside
/// the hosted per-element drive, where an armed request must never
/// bite — and answers `x` unchanged.
fn looping_lambda_def() -> FunctionDef {
    def(
        "looping_lambda",
        &[I64, I64, I64, I64, Bool],
        1,
        0,
        vec![Const::I64(1), Const::I64(0)],
        &[
            Uop::Funcf { frame_slots: 5 },
            Uop::Mov { dst: 1, src: 0 },
            Uop::Kimm { dst: 2, kidx: k(0) },
            Uop::Kimm { dst: 3, kidx: k(1) },
            Uop::SubI64 {
                dst: 0,
                lhs: 0,
                rhs: 2,
            },
            Uop::GtI64 {
                dst: 4,
                lhs: 0,
                rhs: 3,
            },
            Uop::Jmpif { cond: 4, target: 4 },
            Uop::Ret { src: 1 },
        ],
    )
}

/// The fixture module. Function indices are pool-FuncRef contract:
/// 0 main, 1 a, 2 b, 3 join_main, 4 join_child, 5 park_main, 6 spin,
/// 7 deep, 8 echo, 9 hosted_map, 10 looping_lambda.
fn fb_registry() -> ModuleRegistry {
    let mut reg = ModuleRegistry::new();
    reg.install(
        ModuleVersion::new("fb", vec![
            main_def(),
            a_def(),
            b_def(),
            join_main_def(),
            join_child_def(),
            park_main_def(),
            spin_def(),
            deep_def(),
            echo_def(),
            hosted_map_def(),
            looping_lambda_def(),
        ])
        .expect("valid fixture module"),
    );
    reg
}

const DEEP_FRAME_STEP: usize = 6; // words each recursion level adds (arg overlay offset)

#[test]
fn two_fibers_ping_pong_through_resume_both_ways() {
    let mut vm = Vm::new(fb_registry());
    // the root is id 0; spawns number up: a = 1, b = 2
    let a = vm
        .spawn_fiber("fb", "a", &[Value::I64(1)])
        .expect("spawn a");
    let b = vm
        .spawn_fiber("fb", "b", &[Value::I64(0)])
        .expect("spawn b");
    assert_eq!((a, b), (FiberId(1), FiberId(2)));
    // root resumes a (seed 1); a resumes b; b resumes a; ... the value
    // thread 1, 3, ..., 21 alternates a<->b before a stops, and a's
    // resumer link sits on b, so the drive ends on b's death answering
    // the last delivered word
    assert_eq!(
        vm.run("fb", "main", &[]),
        Ok(FrameAnswer::one(Value::I64(21)))
    );
    assert_eq!(vm.fiber_state(a), Some(FiberState::Dead));
    assert_eq!(vm.fiber_state(b), Some(FiberState::Dead));
    // the root parked at its resume of a is a leftover, not a leak: a's
    // resumer link moved to b, so the root was never re-entered
    assert_eq!(vm.fiber_state(FIBER_ROOT), Some(FiberState::Suspended));
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
    let [Value::Ref(id)] = joined.as_slice() else {
        panic!("a string ref, got {joined:?}");
    };
    let id = *id;
    assert_eq!(vm.objects_of(id).str_at(id.id()), Ok("done"));
    assert_eq!(vm.fiber_state(child), Some(FiberState::Dead));
}

#[test]
fn a_requested_fiber_parks_at_its_back_edge_and_completes_on_resume() {
    let mut vm = Vm::new(fb_registry());
    let fiber = vm
        .spawn_fiber("fb", "park_main", &[])
        .expect("spawn the loop driver");
    // the armed request parks the entry at its first taken backward
    // branch; the suspension outcome carries the bare bounce tag
    vm.request_suspend(fiber);
    assert_eq!(
        vm.resume_fiber(fiber, Value::I64(0)),
        Ok(FrameAnswer::one(Value::Tag(PLATFORM_NONE)))
    );
    assert_eq!(vm.fiber_state(fiber), Some(FiberState::Preempted));
    // the resume re-takes the branch (no call site stands, the payload
    // drops) and the loop runs to completion without a further request
    assert_eq!(
        vm.resume_fiber(fiber, Value::I64(0)),
        Ok(FrameAnswer::one(Value::I64(3)))
    );
    assert_eq!(vm.fiber_state(fiber), Some(FiberState::Dead));
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
        Ok(FrameAnswer::one(Value::I64(1_000_000)))
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
    // descends 401 -> 200, then parks mid-recursion at the mark's taken
    // backward branch under the host's suspension request
    vm.request_suspend(deep);
    assert_eq!(
        vm.resume_fiber(deep, Value::I64(0)),
        Ok(FrameAnswer::one(Value::Tag(PLATFORM_NONE)))
    );
    assert_eq!(vm.fiber_state(deep), Some(FiberState::Preempted));
    let fp = vm.fiber_top_fp(deep).expect("parked with live frames");
    let addr = vm.fiber_word_addr(deep, fp).expect("the top frame's base");
    let chunks = vm.fiber_chunk_count(deep).expect("segment exists");
    assert!(
        chunks > 1,
        "the descent to depth 200 already crossed a chunk boundary"
    );
    // continue to zero: ~400 frames total, grown by appended chunks
    assert_eq!(
        vm.resume_fiber(deep, Value::I64(0)),
        Ok(FrameAnswer::one(Value::I64(0)))
    );
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
    // first entry parks at the first branch (the host payload drops: the
    // fiber is Ready)
    vm.request_suspend(echo);
    assert_eq!(
        vm.resume_fiber(echo, Value::I64(0)),
        Ok(FrameAnswer::one(Value::Tag(PLATFORM_NONE)))
    );
    assert_eq!(vm.fiber_state(echo), Some(FiberState::Preempted));
    let fp = vm.fiber_top_fp(echo).expect("parked with live frames");
    let addr = vm.fiber_word_addr(echo, fp).expect("top frame base");
    let chunks = vm.fiber_chunk_count(echo).expect("segment exists");
    // park and re-enter: the same frame must sit at the same address
    vm.request_suspend(echo);
    assert_eq!(
        vm.resume_fiber(echo, Value::I64(9)),
        Ok(FrameAnswer::one(Value::Tag(PLATFORM_NONE)))
    );
    assert_eq!(vm.fiber_state(echo), Some(FiberState::Preempted));
    assert_eq!(vm.fiber_word_addr(echo, fp), Some(addr));
    assert_eq!(vm.fiber_chunk_count(echo), Some(chunks));
    // and the fiber completes: the entry word answers v + 1, RET completes
    assert_eq!(
        vm.resume_fiber(echo, Value::I64(11)),
        Ok(FrameAnswer::one(Value::I64(6)))
    );
    assert_eq!(vm.fiber_state(echo), Some(FiberState::Dead));
}

#[test]
fn resuming_an_unresumable_fiber_names_it_in_the_trap() {
    let mut vm = Vm::new(fb_registry());
    let echo = vm
        .spawn_fiber("fb", "echo", &[Value::I64(0)])
        .expect("spawn echo");
    for _ in 0..2 {
        vm.request_suspend(echo);
        vm.resume_fiber(echo, Value::I64(0))
            .expect("parks at a branch");
    }
    vm.resume_fiber(echo, Value::I64(0))
        .expect("drives to completion");
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
fn an_armed_request_waits_out_a_hosted_drive_and_parks_after_it() {
    let mut vm = Vm::new(fb_registry());
    let hosted = vm
        .spawn_fiber("fb", "hosted_map", &[])
        .expect("spawn the hosted driver");
    // the armed request never bites inside the hosted reentry frame: the
    // map's per-element drives run the looping lambda's taken backward
    // branches to completion (a mid-drive park would refuse the drive
    // with ReentrancyRefused), and the entry parks at its own next back
    // edge once the CALLG returns — the suspension's honesty rule
    vm.request_suspend(hosted);
    assert_eq!(
        vm.resume_fiber(hosted, Value::I64(0)),
        Ok(FrameAnswer::one(Value::Tag(PLATFORM_NONE)))
    );
    assert_eq!(vm.fiber_state(hosted), Some(FiberState::Preempted));
    // the entry completes on the next drive, answering the mapped list
    let done = vm
        .resume_fiber(hosted, Value::I64(0))
        .expect("the map ran to completion");
    let [Value::Ref(_)] = done.as_slice() else {
        panic!("the mapped list ref, got {done:?}");
    };
    assert_eq!(vm.fiber_state(hosted), Some(FiberState::Dead));
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
    let [Value::Ref(id)] = delivered.as_slice() else {
        panic!("a string ref, got {delivered:?}");
    };
    let id = *id;
    assert_eq!(vm.fiber_state(child), Some(FiberState::Dead));
    assert_eq!(vm.objects().rc_at(id.id()), Ok(1));
    assert_eq!(vm.objects().str_at(id.id()), Ok("done"));
    assert_eq!(vm.objects().len(), 1, "only the delivered word stays live");
}
