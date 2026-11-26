//! The bounded channel e2e: a
//! heap-object channel whose buffer lives on the creating fiber's store,
//! hand-assembled programs over the three builtin seams (`chan_new` 87,
//! `chan_send` 88, `chan_recv` 89), and the host nursery face to drive
//! children. What the fixtures pin:
//!
//! - the fast paths: send stocks the buffer, recv drains it FIFO, the answers land at the call
//!   sites like any native's.
//! - park on empty: a child recv'ing an empty channel parks (the drive never blocks), a later
//!   sibling's send wakes it through the scheduler's delivery, and the received word lands at the
//!   parked call site.
//! - park on full: a sender past the bound parks, the receiving sibling's drain refills the freed
//!   slot from the parked sender's staged payload.
//! - the tail shapes: a child whose entry frame tail-calls recv parks with the final-delivery
//!   sentinel, the wake completing the fiber with the received word as its answer.
//! - cross-heap payloads: a child's own-heap string deposits into the parent-owned buffer (the
//!   foreign-post funnel's edge count) and the receiving fiber reads it back content-equal.
//! - the stall guard: a recv with no live sender is the honest deadlock (`SchedulerDeadlock`), and
//!   `chan_new` refuses a bound below one.
//! - cancellation: a channel-parked child under a sibling's first failure is cut at the
//!   resume-shaped point, the boundary reporting it `Cancelled`.
//! - the eager start: a spawned-but-unentered nursery child runs while its joiner parks on the
//!   empty channel's recv (the in-language nursery block's own child the only sender), and a first
//!   child's trap stays contained to the boundary while the later sibling's send still lands.

use ds_vm::{
    ChildOutcome, Const, ConstFuncRef, FrameAnswer, FunctionDef, JoinOutcome, ModuleRegistry,
    ModuleVersion, Trap, Value, Vm,
    abi::{
        ConstIdx, FrameLayout, SlotType,
        SlotType::{I64, Raw, Ref},
        uop::Uop,
    },
    assemble,
    native::{
        BUILTIN_CHAN_NEW, BUILTIN_CHAN_RECV, BUILTIN_CHAN_SEND, BUILTIN_NURSERY_JOIN,
        BUILTIN_NURSERY_OPEN, BUILTIN_NURSERY_SPAWN,
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

/// The shared pool: the three seams, then the int payloads.
fn pool() -> Vec<Const> {
    vec![
        builtin(BUILTIN_CHAN_NEW),  // k0
        builtin(BUILTIN_CHAN_SEND), // k1
        builtin(BUILTIN_CHAN_RECV), // k2
        Const::I64(41),             // k3
        Const::I64(77),             // k4
        Const::I64(100),            // k5: the FIFO fold's radix
        Const::I64(0),              // k6: the rendezvous bound
    ]
}

fn def(name: &str, slots: &[SlotType], params: usize, args: usize, body: &[Uop]) -> FunctionDef {
    FunctionDef {
        name: name.into(),
        layout: FrameLayout::new(slots.to_vec(), params, args).expect("valid fixture layout"),
        pool: pool(),
        code: assemble(body).expect("fixture body encodes"),
    }
}

fn registry(defs: Vec<FunctionDef>) -> ModuleRegistry {
    let mut reg = ModuleRegistry::new();
    reg.install(ModuleVersion::new("ch", defs).expect("valid fixture module"));
    reg
}

/// `main(cap: int) -> int`: new, two sends, two recvs, the sum — the fast
/// paths with no fiber ever parking, FIFO order pinned by the summands.
fn fast_main_def() -> FunctionDef {
    def("main", &[I64, Ref, I64, Raw, Raw], 1, 2, &[
        Uop::Funcf { frame_slots: 5 },
        Uop::Mov { dst: 3, src: 0 },
        Uop::Call {
            callee: k(0),
            nargs: 1,
        },
        // slot 1 keeps the channel word across the overlay-clobbering calls
        Uop::Mov { dst: 1, src: 3 },
        Uop::Mov { dst: 3, src: 1 },
        Uop::Kimm { dst: 4, kidx: k(3) },
        Uop::Call {
            callee: k(1),
            nargs: 2,
        },
        Uop::Mov { dst: 3, src: 1 },
        Uop::Kimm { dst: 4, kidx: k(4) },
        Uop::Call {
            callee: k(1),
            nargs: 2,
        },
        Uop::Mov { dst: 3, src: 1 },
        Uop::Call {
            callee: k(2),
            nargs: 1,
        },
        Uop::Mov { dst: 2, src: 3 },
        Uop::Mov { dst: 3, src: 1 },
        Uop::Call {
            callee: k(2),
            nargs: 1,
        },
        // first*100 + second: 41 then 77 answers 4177, order-sensitive
        Uop::Kimm { dst: 4, kidx: k(5) },
        Uop::MulI64 {
            dst: 2,
            lhs: 2,
            rhs: 4,
        },
        Uop::AddI64 {
            dst: 2,
            lhs: 2,
            rhs: 3,
        },
        Uop::Ret { src: 2 },
    ])
}

#[test]
fn the_fast_paths_round_trip_fifo() {
    let mut vm = Vm::new(registry(vec![fast_main_def()]));
    let answer = vm
        .run("ch", "main", &[Value::I64(2)])
        .expect("fast paths run");
    assert_eq!(
        answer,
        FrameAnswer::one(Value::I64(4177)),
        "41 then 77 drain in order"
    );
}

/// `recv_child(ch) -> int`: one recv — the parking probe at a plain call.
fn recv_child_def() -> FunctionDef {
    def("recv_child", &[Ref, Raw, Raw], 1, 2, &[
        Uop::Funcf { frame_slots: 3 },
        Uop::Mov { dst: 1, src: 0 },
        Uop::Call {
            callee: k(2),
            nargs: 1,
        },
        Uop::Ret { src: 1 },
    ])
}

/// `send_child(ch) -> int`: one send of 77, the unit answered — the waker.
fn send_child_def() -> FunctionDef {
    def("send_child", &[Ref, Raw, Raw], 1, 2, &[
        Uop::Funcf { frame_slots: 3 },
        Uop::Mov { dst: 1, src: 0 },
        Uop::Kimm { dst: 2, kidx: k(4) },
        Uop::Call {
            callee: k(1),
            nargs: 2,
        },
        Uop::Ret { src: 1 },
    ])
}

/// `make(cap: int) -> chan`: the host-held construction seam.
fn make_def() -> FunctionDef {
    def("make", &[I64, Raw], 1, 1, &[
        Uop::Funcf { frame_slots: 2 },
        Uop::Mov { dst: 1, src: 0 },
        Uop::Call {
            callee: k(0),
            nargs: 1,
        },
        Uop::Ret { src: 1 },
    ])
}

fn channel_of(vm: &mut Vm, cap: i64) -> Value {
    vm.run("ch", "make", &[Value::I64(cap)])
        .expect("construction runs")
        .as_slice()
        .first()
        .copied()
        .expect("the channel word")
}

#[test]
fn a_parked_recv_wakes_through_the_siblings_send() {
    let mut vm = Vm::new(registry(vec![
        make_def(),
        recv_child_def(),
        send_child_def(),
    ]));
    let ch = channel_of(&mut vm, 1);
    let nursery = vm.open_root_nursery();
    vm.spawn_fiber_in(nursery, "ch", "recv_child", &[ch])
        .expect("spawn the receiver");
    vm.spawn_fiber_in(nursery, "ch", "send_child", &[ch])
        .expect("spawn the sender");
    match vm.join_nursery(nursery).expect("the drive consumes") {
        JoinOutcome::Joined(spans) => {
            assert_eq!(
                spans[0],
                FrameAnswer::one(Value::I64(77)),
                "the parked receiver's answer landed at its call site"
            );
            assert_eq!(
                spans[1],
                FrameAnswer::one(Value::I64(0)),
                "the sender's unit"
            );
        },
        other @ JoinOutcome::Failed(_) => panic!("a clean join: {other:?}"),
    }
}

/// `sender2(ch) -> int`: two sends past a bound of one — the second parks.
fn sender2_def() -> FunctionDef {
    def("sender2", &[Ref, Raw, Raw], 1, 2, &[
        Uop::Funcf { frame_slots: 3 },
        Uop::Mov { dst: 1, src: 0 },
        Uop::Kimm { dst: 2, kidx: k(3) },
        Uop::Call {
            callee: k(1),
            nargs: 2,
        },
        Uop::Mov { dst: 1, src: 0 },
        Uop::Kimm { dst: 2, kidx: k(4) },
        Uop::Call {
            callee: k(1),
            nargs: 2,
        },
        Uop::Ret { src: 1 },
    ])
}

/// `recvr2(ch) -> int`: two recvs, the pair folded — the refiller.
fn recvr2_def() -> FunctionDef {
    def("recvr2", &[Ref, I64, Raw, Raw], 1, 2, &[
        Uop::Funcf { frame_slots: 4 },
        Uop::Mov { dst: 2, src: 0 },
        Uop::Call {
            callee: k(2),
            nargs: 1,
        },
        Uop::Mov { dst: 1, src: 2 },
        Uop::Mov { dst: 2, src: 0 },
        Uop::Call {
            callee: k(2),
            nargs: 1,
        },
        // first*100 + second: 41 then 77 answers 4177 across the park
        Uop::Kimm { dst: 3, kidx: k(5) },
        Uop::MulI64 {
            dst: 1,
            lhs: 1,
            rhs: 3,
        },
        Uop::AddI64 {
            dst: 1,
            lhs: 1,
            rhs: 2,
        },
        Uop::Ret { src: 1 },
    ])
}

#[test]
fn a_parked_sender_refills_from_the_receivers_drain() {
    let mut vm = Vm::new(registry(vec![make_def(), sender2_def(), recvr2_def()]));
    let ch = channel_of(&mut vm, 1);
    let nursery = vm.open_root_nursery();
    vm.spawn_fiber_in(nursery, "ch", "sender2", &[ch])
        .expect("spawn the sender");
    vm.spawn_fiber_in(nursery, "ch", "recvr2", &[ch])
        .expect("spawn the receiver");
    match vm.join_nursery(nursery).expect("the drive consumes") {
        JoinOutcome::Joined(spans) => {
            assert_eq!(
                spans[0],
                FrameAnswer::one(Value::I64(0)),
                "the parked sender woke with the unit word"
            );
            assert_eq!(
                spans[1],
                FrameAnswer::one(Value::I64(4177)),
                "both payloads crossed in order, the parked one refilled into the freed slot"
            );
        },
        other @ JoinOutcome::Failed(_) => panic!("a clean join: {other:?}"),
    }
}

/// `tail_recv(ch) -> int`: the entry frame's own tail call is the recv —
/// the final-delivery sentinel park, the wake completing the fiber.
fn tail_recv_def() -> FunctionDef {
    def("tail_recv", &[Ref, Raw], 1, 1, &[
        Uop::Funcf { frame_slots: 2 },
        Uop::Mov { dst: 1, src: 0 },
        Uop::Callt {
            callee: k(2),
            nargs: 1,
        },
    ])
}

#[test]
fn an_entry_frame_tail_recv_parks_and_completes_with_the_delivered_word() {
    let mut vm = Vm::new(registry(vec![
        make_def(),
        tail_recv_def(),
        send_child_def(),
    ]));
    let ch = channel_of(&mut vm, 1);
    let nursery = vm.open_root_nursery();
    vm.spawn_fiber_in(nursery, "ch", "tail_recv", &[ch])
        .expect("spawn the tail receiver");
    vm.spawn_fiber_in(nursery, "ch", "send_child", &[ch])
        .expect("spawn the sender");
    match vm.join_nursery(nursery).expect("the drive consumes") {
        JoinOutcome::Joined(spans) => {
            assert_eq!(
                spans[0],
                FrameAnswer::one(Value::I64(77)),
                "the sentinel park completed with the delivered word as the fiber's answer"
            );
        },
        other @ JoinOutcome::Failed(_) => panic!("a clean join: {other:?}"),
    }
}

/// `str_send_child(ch) -> int`: loads its own-heap string const and sends
/// it — the cross-heap deposit through the foreign-post funnel.
fn str_send_child_def() -> FunctionDef {
    FunctionDef {
        name: "str_send_child".into(),
        layout: FrameLayout::new(vec![Ref, Raw, Raw, Raw], 1, 2).expect("valid fixture layout"),
        pool: vec![
            builtin(BUILTIN_CHAN_NEW),
            builtin(BUILTIN_CHAN_SEND),
            builtin(BUILTIN_CHAN_RECV),
            Const::I64(41),
            Const::Str("cross-heap".into()),
        ],
        code: assemble(&[
            Uop::Funcf { frame_slots: 4 },
            Uop::Mov { dst: 2, src: 0 },
            Uop::Kstr { dst: 3, kidx: k(4) },
            Uop::Call {
                callee: k(1),
                nargs: 2,
            },
            Uop::Ret { src: 2 },
        ])
        .expect("fixture body encodes"),
    }
}

/// `str_recv_child(ch) -> bool`: recvs the foreign string and compares it
/// content-equal against its own heap's interned copy — the payload
/// crossed heaps intact.
fn str_recv_child_def() -> FunctionDef {
    FunctionDef {
        name: "str_recv_child".into(),
        layout: FrameLayout::new(vec![Ref, Raw, Raw, Raw], 1, 2).expect("valid fixture layout"),
        pool: vec![
            builtin(BUILTIN_CHAN_NEW),
            builtin(BUILTIN_CHAN_SEND),
            builtin(BUILTIN_CHAN_RECV),
            Const::I64(41),
            Const::Str("cross-heap".into()),
        ],
        code: assemble(&[
            Uop::Funcf { frame_slots: 4 },
            Uop::Mov { dst: 2, src: 0 },
            Uop::Call {
                callee: k(2),
                nargs: 1,
            },
            Uop::Mov { dst: 1, src: 2 },
            Uop::Kstr { dst: 3, kidx: k(4) },
            Uop::EqStr {
                dst: 1,
                lhs: 1,
                rhs: 3,
            },
            Uop::Ret { src: 1 },
        ])
        .expect("fixture body encodes"),
    }
}

#[test]
fn a_childs_own_heap_string_crosses_through_the_channel() {
    let mut vm = Vm::new(registry(vec![
        make_def(),
        str_recv_child_def(),
        str_send_child_def(),
    ]));
    let ch = channel_of(&mut vm, 1);
    let nursery = vm.open_root_nursery();
    vm.spawn_fiber_in(nursery, "ch", "str_recv_child", &[ch])
        .expect("spawn the receiver");
    vm.spawn_fiber_in(nursery, "ch", "str_send_child", &[ch])
        .expect("spawn the sender");
    match vm.join_nursery(nursery).expect("the drive consumes") {
        JoinOutcome::Joined(spans) => {
            assert_eq!(
                spans[0],
                FrameAnswer::one(Value::Bool(true)),
                "the foreign payload read back content-equal on the receiving heap"
            );
        },
        other @ JoinOutcome::Failed(_) => panic!("a clean join: {other:?}"),
    }
}

#[test]
fn a_recv_with_no_live_sender_is_the_honest_deadlock() {
    // a dedicated module: the make+recv pair never stocks the buffer
    let mut reg = ModuleRegistry::new();
    reg.install(
        ModuleVersion::new("dl", vec![
            make_def(),
            recv_child_def(),
            def("main", &[I64, Ref, Raw, Raw], 0, 2, &[
                Uop::Funcf { frame_slots: 4 },
                Uop::Kimm { dst: 0, kidx: k(3) },
                Uop::Mov { dst: 2, src: 0 },
                Uop::Call {
                    callee: k(0),
                    nargs: 1,
                },
                Uop::Mov { dst: 1, src: 2 },
                Uop::Mov { dst: 2, src: 1 },
                Uop::Call {
                    callee: k(2),
                    nargs: 1,
                },
                Uop::Ret { src: 2 },
            ]),
        ])
        .expect("valid fixture module"),
    );
    let mut vm = Vm::new(reg);
    match vm.run("dl", "main", &[]) {
        Err(Trap::SchedulerDeadlock) => {},
        other => panic!("the stall guard answers the channel deadlock: {other:?}"),
    }
}

#[test]
fn a_negative_bound_refuses() {
    let mut vm = Vm::new(registry(vec![make_def()]));
    match vm.run("ch", "make", &[Value::I64(-1)]) {
        Err(Trap::ChanCapacity { cap }) => assert_eq!(cap, -1),
        other => panic!("the constructor refuses a negative bound: {other:?}"),
    }
}

#[test]
fn a_rendezvous_sender_parks_until_a_receive_takes_the_staged_span() {
    // bound zero: the buffer holds nothing, so the sender parks with
    // its staged span in the channel's own queue and the arriving
    // receive takes it whole — the waking peer the resumer, exactly the
    // buffered park's discipline
    let mut vm = Vm::new(registry(vec![
        make_def(),
        recv_child_def(),
        send_child_def(),
    ]));
    let ch = channel_of(&mut vm, 0);
    let nursery = vm.open_root_nursery();
    vm.spawn_fiber_in(nursery, "ch", "send_child", &[ch])
        .expect("spawn the sender");
    vm.spawn_fiber_in(nursery, "ch", "recv_child", &[ch])
        .expect("spawn the receiver");
    match vm.join_nursery(nursery).expect("the drive consumes") {
        JoinOutcome::Joined(spans) => {
            assert_eq!(
                spans[0],
                FrameAnswer::one(Value::I64(0)),
                "the parked rendezvous sender woke with the unit word"
            );
            assert_eq!(
                spans[1],
                FrameAnswer::one(Value::I64(77)),
                "the receiver took the staged span whole"
            );
        },
        other @ JoinOutcome::Failed(_) => panic!("a clean join: {other:?}"),
    }
}

#[test]
fn a_rendezvous_receiver_parks_and_the_send_pairs_directly() {
    // the other door: the receive parks first, and the send — with no
    // buffer room ever — hands its element straight to the parked
    // receiver through the deposit's edge counts and the release's
    // claim transfer
    let mut vm = Vm::new(registry(vec![
        make_def(),
        recv_child_def(),
        send_child_def(),
    ]));
    let ch = channel_of(&mut vm, 0);
    let nursery = vm.open_root_nursery();
    vm.spawn_fiber_in(nursery, "ch", "recv_child", &[ch])
        .expect("spawn the receiver");
    vm.spawn_fiber_in(nursery, "ch", "send_child", &[ch])
        .expect("spawn the sender");
    match vm.join_nursery(nursery).expect("the drive consumes") {
        JoinOutcome::Joined(spans) => {
            assert_eq!(
                spans[0],
                FrameAnswer::one(Value::I64(77)),
                "the parked receiver's answer landed at its call site"
            );
            assert_eq!(
                spans[1],
                FrameAnswer::one(Value::I64(0)),
                "the pairing send answered the unit word"
            );
        },
        other @ JoinOutcome::Failed(_) => panic!("a clean join: {other:?}"),
    }
}

#[test]
fn a_rendezvous_pair_answers_in_staged_order_across_a_reparking_sender() {
    // both of the sender's sends park (its own fiber re-parks after the
    // first wake), and the two receives take the staged spans in park
    // order — the queue is the channel's own, FIFO exactly the buffered
    // sender queue's
    let mut vm = Vm::new(registry(vec![make_def(), sender2_def(), recvr2_def()]));
    let ch = channel_of(&mut vm, 0);
    let nursery = vm.open_root_nursery();
    vm.spawn_fiber_in(nursery, "ch", "sender2", &[ch])
        .expect("spawn the sender");
    vm.spawn_fiber_in(nursery, "ch", "recvr2", &[ch])
        .expect("spawn the receiver");
    match vm.join_nursery(nursery).expect("the drive consumes") {
        JoinOutcome::Joined(spans) => {
            assert_eq!(
                spans[0],
                FrameAnswer::one(Value::I64(0)),
                "the twice-parked sender woke with the unit word both times"
            );
            assert_eq!(
                spans[1],
                FrameAnswer::one(Value::I64(4177)),
                "both payloads crossed in staged order"
            );
        },
        other @ JoinOutcome::Failed(_) => panic!("a clean join: {other:?}"),
    }
}

#[test]
fn a_parked_rendezvous_multi_word_span_crosses_whole() {
    // the staged span machinery the full-buffer park already owns: the
    // rendezvous take deposits the staged element (fixing the buffer's
    // element stride) and the pop answers the tag-less total of its
    // words — a wider payload needs no new seam
    let mut vm = Vm::new(registry(vec![
        make_def(),
        pair_recv_child_def(),
        pair_send_child_def(),
    ]));
    let ch = channel_of(&mut vm, 0);
    let nursery = vm.open_root_nursery();
    vm.spawn_fiber_in(nursery, "ch", "pair_send_child", &[ch])
        .expect("spawn the sender");
    vm.spawn_fiber_in(nursery, "ch", "pair_recv_child", &[ch])
        .expect("spawn the receiver");
    match vm.join_nursery(nursery).expect("the drive consumes") {
        JoinOutcome::Joined(spans) => {
            assert_eq!(
                spans[0],
                FrameAnswer::one(Value::I64(0)),
                "the parked rendezvous sender woke with the unit word"
            );
            assert_eq!(
                spans[1],
                FrameAnswer::one(Value::I64(4_177_001)),
                "the staged two-word span crossed word-for-word at the rendezvous"
            );
        },
        other @ JoinOutcome::Failed(_) => panic!("a clean join: {other:?}"),
    }
}

#[test]
fn a_rendezvous_send_with_no_receiver_is_the_honest_deadlock() {
    // the stall guard's channel report covers the rendezvous door too:
    // a sender parked on a channel no live fiber serves deadlocks by
    // design, exactly the empty receive's own degenerate
    let mut reg = ModuleRegistry::new();
    reg.install(
        ModuleVersion::new("dl", vec![
            make_def(),
            def("main", &[I64, Ref, Raw, Raw], 0, 2, &[
                Uop::Funcf { frame_slots: 4 },
                Uop::Kimm { dst: 0, kidx: k(6) },
                Uop::Mov { dst: 2, src: 0 },
                Uop::Call {
                    callee: k(0),
                    nargs: 1,
                },
                // send 41 onto the rendezvous channel: no receiver ever
                // arrives, so the one fiber parks on the send
                Uop::Mov { dst: 1, src: 2 },
                Uop::Kimm { dst: 3, kidx: k(3) },
                Uop::Mov { dst: 2, src: 1 },
                Uop::Call {
                    callee: k(1),
                    nargs: 2,
                },
                Uop::Ret { src: 2 },
            ]),
        ])
        .expect("valid fixture module"),
    );
    let mut vm = Vm::new(reg);
    match vm.run("dl", "main", &[]) {
        Err(Trap::SchedulerDeadlock) => {},
        other => panic!("the stall guard answers the rendezvous deadlock: {other:?}"),
    }
}

/// `trapper() -> int`: an immediate division by zero — the first failure.
fn trapper_def() -> FunctionDef {
    FunctionDef {
        name: "trapper".into(),
        layout: FrameLayout::new(vec![I64, I64], 0, 0).expect("valid fixture layout"),
        pool: vec![
            builtin(BUILTIN_CHAN_NEW),
            builtin(BUILTIN_CHAN_SEND),
            builtin(BUILTIN_CHAN_RECV),
            Const::I64(1),
            Const::I64(0),
        ],
        code: assemble(&[
            Uop::Funcf { frame_slots: 2 },
            Uop::Kimm { dst: 0, kidx: k(3) },
            Uop::Kimm { dst: 1, kidx: k(4) },
            Uop::DivI64 {
                dst: 0,
                lhs: 0,
                rhs: 1,
            },
            Uop::Ret { src: 0 },
        ])
        .expect("fixture body encodes"),
    }
}

#[test]
fn a_channel_parked_child_is_cut_under_a_siblings_first_failure() {
    let mut vm = Vm::new(registry(vec![make_def(), recv_child_def(), trapper_def()]));
    let ch = channel_of(&mut vm, 1);
    let nursery = vm.open_root_nursery();
    vm.spawn_fiber_in(nursery, "ch", "recv_child", &[ch])
        .expect("spawn the parked receiver");
    vm.spawn_fiber_in(nursery, "ch", "trapper", &[])
        .expect("spawn the failing sibling");
    match vm.join_nursery(nursery).expect("the drive consumes") {
        JoinOutcome::Failed(outcomes) => {
            assert!(
                matches!(outcomes[0], ChildOutcome::Cancelled),
                "the channel-parked child cut at its resume point: {:?}",
                outcomes[0]
            );
            assert!(
                matches!(outcomes[1], ChildOutcome::Trapped(Trap::DivideByZero)),
                "the first failure ledgers its trap: {:?}",
                outcomes[1]
            );
        },
        other @ JoinOutcome::Joined(_) => panic!("a failed join: {other:?}"),
    }
}

/// The eager-start fixtures' pool: the channel seams, the nursery seams,
/// the join, the payload, and the spawnees' function words.
fn eager_pool() -> Vec<Const> {
    vec![
        builtin(BUILTIN_CHAN_NEW),      // k0
        builtin(BUILTIN_CHAN_SEND),     // k1
        builtin(BUILTIN_CHAN_RECV),     // k2
        builtin(BUILTIN_NURSERY_OPEN),  // k3
        builtin(BUILTIN_NURSERY_SPAWN), // k4
        builtin(BUILTIN_NURSERY_JOIN),  // k5
        Const::I64(77),                 // k6: the sent payload
    ]
}

fn eager_ref(index: u32) -> Const {
    Const::FuncRef(ConstFuncRef::Named {
        module: "ch".into(),
        func: index,
    })
}

fn eager_def(
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

/// `(ch: Ref) -> int`: one send of 77 — the waker the joiner parks on.
fn eager_sender_def() -> FunctionDef {
    eager_def("eager_sender", &[Ref, Raw, Raw], 1, 2, eager_pool(), &[
        Uop::Funcf { frame_slots: 3 },
        Uop::Mov { dst: 1, src: 0 },
        Uop::Kimm { dst: 2, kidx: k(6) },
        Uop::Call {
            callee: k(1),
            nargs: 2,
        },
        Uop::Ret { src: 1 },
    ])
}

/// `() -> int`: an immediate division by zero — the first failure.
fn eager_trapper_def() -> FunctionDef {
    eager_def(
        "eager_trapper",
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

/// `(cap: int) -> int`: the in-language nursery block — open the
/// nursery, spawn `trapper` first when staged (the first failure in
/// spawn order), then the sender closure (the channel its one capture),
/// park on the empty channel's recv, then the debug join answering the
/// failed-children count. The joiner's recv alone never wakes: only the
/// eagerly started children do.
fn eager_main_def(trapper: Option<u32>) -> FunctionDef {
    let mut pool = eager_pool();
    let spawn_trapper: &[Uop] = if let Some(idx) = trapper {
        pool.push(eager_ref(idx)); // k7: the trapper's scalar word
        pool.push(eager_ref(0)); // k8: the sender's word for the closure
        &[
            Uop::Kconst { dst: 2, kidx: k(7) },
            Uop::Mov { dst: 5, src: 0 },
            Uop::Mov { dst: 6, src: 2 },
            Uop::Call {
                callee: k(4),
                nargs: 2,
            },
        ]
    } else {
        pool.push(eager_ref(0)); // k7: the sender's word for the closure
        &[]
    };
    let sender_kidx = k(7 + u16::from(trapper.is_some()));
    let mut body = vec![
        Uop::Funcf { frame_slots: 7 },
        // chan_new(cap): the channel word lands on overlay word 5
        Uop::Mov { dst: 5, src: 0 },
        Uop::Call {
            callee: k(0),
            nargs: 1,
        },
        Uop::Mov { dst: 1, src: 5 },
        // the open: the nursery id lands on word 5
        Uop::Call {
            callee: k(3),
            nargs: 0,
        },
        Uop::Mov { dst: 0, src: 5 },
    ];
    body.extend_from_slice(spawn_trapper);
    body.extend_from_slice(&[
        // the sender closure over the channel, then its spawn
        Uop::Kconst {
            dst: 2,
            kidx: sender_kidx,
        },
        Uop::Mov { dst: 5, src: 1 },
        Uop::ClosureNew {
            dst: 3,
            func: 2,
            n: 1,
        },
        Uop::Mov { dst: 5, src: 0 },
        Uop::Mov { dst: 6, src: 3 },
        Uop::Call {
            callee: k(4),
            nargs: 2,
        },
        // the joiner parks on the empty channel
        Uop::Mov { dst: 5, src: 1 },
        Uop::Call {
            callee: k(2),
            nargs: 1,
        },
        Uop::Mov { dst: 4, src: 5 },
        // the scope-exit join: the failed-children count answers
        Uop::Mov { dst: 5, src: 0 },
        Uop::Call {
            callee: k(5),
            nargs: 1,
        },
        Uop::AddI64 {
            dst: 4,
            lhs: 4,
            rhs: 5,
        },
        Uop::Ret { src: 4 },
    ]);
    eager_def(
        "eager_main",
        &[I64, Ref, SlotType::Fn, Ref, Raw, Raw, Raw],
        1,
        2,
        pool,
        &body,
    )
}
/// The eagerly started child wakes its own parked joiner: the main fiber
/// parks on an empty channel's recv, and only the spawned-but-unentered
/// child's send can wake it — the executor starts the Ready child while
/// the joiner parks instead of deadlocking on the stall guard.
#[test]
fn an_eagerly_started_child_wakes_its_own_parked_joiner() {
    let mut vm = Vm::new(registry(vec![eager_sender_def(), eager_main_def(None)]));
    let answer = vm
        .run("ch", "eager_main", &[Value::I64(1)])
        .expect("the eagerly started child woke its joiner");
    assert_eq!(
        answer,
        FrameAnswer::one(Value::I64(77)),
        "the parked recv answered the child's send beside the clean count"
    );
}

/// The eager containment: a first child that traps while the joiner
/// parks is the nursery's first failure — the boundary counts it, the
/// host run never sees the trap — and the later sibling's straight-line
/// send still lands, so the woken recv answers beside the trapped count.
#[test]
fn an_eager_childs_trap_is_contained_and_counted_at_the_join() {
    let mut vm = Vm::new(registry(vec![
        eager_sender_def(),
        eager_trapper_def(),
        eager_main_def(Some(1)),
    ]));
    let answer = vm
        .run("ch", "eager_main", &[Value::I64(1)])
        .expect("the trapped child contained, the joiner woke anyway");
    assert_eq!(
        answer,
        FrameAnswer::one(Value::I64(78)),
        "the recv answered 77 beside the boundary's trapped count 1"
    );
}

/// The multi-word fixtures' pool: the three seams, then the fold's
/// constants (41 and 77 the heads, 1 and 2 the tail words, the radixes).
fn pair_pool() -> Vec<Const> {
    vec![
        builtin(BUILTIN_CHAN_NEW),  // k0
        builtin(BUILTIN_CHAN_SEND), // k1
        builtin(BUILTIN_CHAN_RECV), // k2
        Const::I64(41),             // k3: the first element's head word
        Const::I64(77),             // k4: the second element's head word
        Const::I64(1),              // k5: the first element's tail word
        Const::I64(2),              // k6: the second element's tail word
        Const::I64(100),            // k7: the head fold's radix
        Const::I64(1000),           // k8: the tail fold's radix
        Const::I64(10),             // k9: the drain fold's tail radix
    ]
}

fn pair_def(
    name: &str,
    slots: &[SlotType],
    params: usize,
    args: usize,
    body: &[Uop],
) -> FunctionDef {
    FunctionDef {
        name: name.into(),
        layout: FrameLayout::new(slots.to_vec(), params, args).expect("valid fixture layout"),
        pool: pair_pool(),
        code: assemble(body).expect("fixture body encodes"),
    }
}

/// `pair_main(cap: int) -> int`: new, two two-word sends, two recvs, the
/// fold — the multi-word fast paths with no fiber ever parking, the
/// buffer's element stride fixed by the first deposit and the recv's
/// answer the tag-less total of the element's words. The fold mixes both
/// words of both elements, so a dropped or reordered word flips it.
fn pair_main_def() -> FunctionDef {
    pair_def("pair_main", &[I64, Ref, I64, I64, Raw, Raw, Raw], 1, 3, &[
        Uop::Funcf { frame_slots: 7 },
        Uop::Mov { dst: 4, src: 0 },
        Uop::Call {
            callee: k(0),
            nargs: 1,
        },
        // slot 1 keeps the channel word across the overlay-clobbering calls
        Uop::Mov { dst: 1, src: 4 },
        // send (41, 1): the payload's whole two-word span staged past the
        // channel word
        Uop::Mov { dst: 4, src: 1 },
        Uop::Kimm { dst: 5, kidx: k(3) },
        Uop::Kimm { dst: 6, kidx: k(5) },
        Uop::Call {
            callee: k(1),
            nargs: 3,
        },
        // send (77, 2)
        Uop::Mov { dst: 4, src: 1 },
        Uop::Kimm { dst: 5, kidx: k(4) },
        Uop::Kimm { dst: 6, kidx: k(6) },
        Uop::Call {
            callee: k(1),
            nargs: 3,
        },
        // recv one element: the total's two words land on overlay words
        // 4 and 5, moved out ahead of the next call's staging
        Uop::Mov { dst: 4, src: 1 },
        Uop::Call {
            callee: k(2),
            nargs: 1,
        },
        Uop::Mov { dst: 2, src: 4 },
        Uop::Mov { dst: 3, src: 5 },
        // recv the second element
        Uop::Mov { dst: 4, src: 1 },
        Uop::Call {
            callee: k(2),
            nargs: 1,
        },
        // (41*100 + 77) * 1000 + 1*10 + 2 = 4177012: head order, tail
        // words, and word-in-span all folded (slot 6 the radix scratch,
        // the second element's tail word at slot 5 kept clear)
        Uop::Kimm { dst: 6, kidx: k(7) },
        Uop::MulI64 {
            dst: 2,
            lhs: 2,
            rhs: 6,
        },
        Uop::AddI64 {
            dst: 2,
            lhs: 2,
            rhs: 4,
        },
        Uop::Kimm { dst: 6, kidx: k(8) },
        Uop::MulI64 {
            dst: 2,
            lhs: 2,
            rhs: 6,
        },
        Uop::Kimm { dst: 6, kidx: k(9) },
        Uop::MulI64 {
            dst: 3,
            lhs: 3,
            rhs: 6,
        },
        Uop::AddI64 {
            dst: 2,
            lhs: 2,
            rhs: 3,
        },
        Uop::AddI64 {
            dst: 2,
            lhs: 2,
            rhs: 5,
        },
        Uop::Ret { src: 2 },
    ])
}

#[test]
fn the_multi_word_fast_paths_round_trip_whole_spans() {
    let mut vm = Vm::new(registry(vec![pair_main_def()]));
    let answer = vm
        .run("ch", "pair_main", &[Value::I64(2)])
        .expect("multi-word fast paths run");
    assert_eq!(
        answer,
        FrameAnswer::one(Value::I64(4_177_012)),
        "both two-word elements crossed whole, FIFO, word-in-span sensitive"
    );
}

/// `pair_send_child(ch) -> int`: one send of the two-word element
/// (41, 1) — the multi-word waker.
fn pair_send_child_def() -> FunctionDef {
    pair_def("pair_send_child", &[Ref, Raw, Raw, Raw], 1, 3, &[
        Uop::Funcf { frame_slots: 4 },
        Uop::Mov { dst: 1, src: 0 },
        Uop::Kimm { dst: 2, kidx: k(3) },
        Uop::Kimm { dst: 3, kidx: k(5) },
        Uop::Call {
            callee: k(1),
            nargs: 3,
        },
        Uop::Ret { src: 1 },
    ])
}

/// `pair_recv_child(ch) -> int`: one recv of a two-word element, folded —
/// the parking probe's multi-word shape at a plain call, the answer
/// (41*100 + 77)*1000 + 1 = 4177001 over the delivered total's words.
fn pair_recv_child_def() -> FunctionDef {
    pair_def("pair_recv_child", &[Ref, I64, Raw, Raw], 1, 2, &[
        Uop::Funcf { frame_slots: 4 },
        Uop::Mov { dst: 2, src: 0 },
        Uop::Call {
            callee: k(2),
            nargs: 1,
        },
        // overlay words 2 and 3 hold the total: head at 2, tail at 3
        Uop::Mov { dst: 1, src: 2 },
        Uop::Mov { dst: 2, src: 3 },
        Uop::Kimm { dst: 3, kidx: k(7) },
        Uop::MulI64 {
            dst: 1,
            lhs: 1,
            rhs: 3,
        },
        Uop::Kimm { dst: 3, kidx: k(4) },
        Uop::AddI64 {
            dst: 1,
            lhs: 1,
            rhs: 3,
        },
        Uop::Kimm { dst: 3, kidx: k(8) },
        Uop::MulI64 {
            dst: 1,
            lhs: 1,
            rhs: 3,
        },
        Uop::AddI64 {
            dst: 1,
            lhs: 1,
            rhs: 2,
        },
        Uop::Ret { src: 1 },
    ])
}

#[test]
fn a_parked_multi_word_recv_wakes_through_the_siblings_send() {
    let mut vm = Vm::new(registry(vec![
        make_def(),
        pair_recv_child_def(),
        pair_send_child_def(),
    ]));
    let ch = channel_of(&mut vm, 1);
    let nursery = vm.open_root_nursery();
    vm.spawn_fiber_in(nursery, "ch", "pair_recv_child", &[ch])
        .expect("spawn the receiver");
    vm.spawn_fiber_in(nursery, "ch", "pair_send_child", &[ch])
        .expect("spawn the sender");
    match vm.join_nursery(nursery).expect("the drive consumes") {
        JoinOutcome::Joined(spans) => {
            assert_eq!(
                spans[0],
                FrameAnswer::one(Value::I64(4_177_001)),
                "the parked receiver's total landed word-for-word at its call site"
            );
            assert_eq!(
                spans[1],
                FrameAnswer::one(Value::I64(0)),
                "the sender's unit"
            );
        },
        other @ JoinOutcome::Failed(_) => panic!("a clean join: {other:?}"),
    }
}

/// `pair_sender2(ch) -> int`: two two-word sends past a bound of one —
/// the second parks with its whole staged span.
fn pair_sender2_def() -> FunctionDef {
    pair_def("pair_sender2", &[Ref, Raw, Raw, Raw], 1, 3, &[
        Uop::Funcf { frame_slots: 4 },
        Uop::Mov { dst: 1, src: 0 },
        Uop::Kimm { dst: 2, kidx: k(3) },
        Uop::Kimm { dst: 3, kidx: k(5) },
        Uop::Call {
            callee: k(1),
            nargs: 3,
        },
        Uop::Mov { dst: 1, src: 0 },
        Uop::Kimm { dst: 2, kidx: k(4) },
        Uop::Kimm { dst: 3, kidx: k(6) },
        Uop::Call {
            callee: k(1),
            nargs: 3,
        },
        Uop::Ret { src: 1 },
    ])
}

/// `pair_recvr2(ch) -> int`: two recvs of two-word elements, folded to
/// 4177012 — the multi-word refiller.
fn pair_recvr2_def() -> FunctionDef {
    pair_def("pair_recvr2", &[Ref, I64, I64, Raw, Raw, Raw], 1, 2, &[
        Uop::Funcf { frame_slots: 6 },
        Uop::Mov { dst: 4, src: 0 },
        Uop::Call {
            callee: k(2),
            nargs: 1,
        },
        Uop::Mov { dst: 1, src: 4 },
        Uop::Mov { dst: 2, src: 5 },
        Uop::Mov { dst: 4, src: 0 },
        Uop::Call {
            callee: k(2),
            nargs: 1,
        },
        // (41*100 + 77) * 1000 + 1*10 + 2 over both elements' words
        Uop::Kimm { dst: 3, kidx: k(7) },
        Uop::MulI64 {
            dst: 1,
            lhs: 1,
            rhs: 3,
        },
        Uop::AddI64 {
            dst: 1,
            lhs: 1,
            rhs: 4,
        },
        Uop::Kimm { dst: 3, kidx: k(8) },
        Uop::MulI64 {
            dst: 1,
            lhs: 1,
            rhs: 3,
        },
        Uop::Kimm { dst: 3, kidx: k(9) },
        Uop::MulI64 {
            dst: 2,
            lhs: 2,
            rhs: 3,
        },
        Uop::AddI64 {
            dst: 2,
            lhs: 2,
            rhs: 5,
        },
        Uop::AddI64 {
            dst: 1,
            lhs: 1,
            rhs: 2,
        },
        Uop::Ret { src: 1 },
    ])
}

#[test]
fn a_parked_multi_word_send_refills_from_the_receivers_drain() {
    let mut vm = Vm::new(registry(vec![
        make_def(),
        pair_sender2_def(),
        pair_recvr2_def(),
    ]));
    let ch = channel_of(&mut vm, 1);
    let nursery = vm.open_root_nursery();
    vm.spawn_fiber_in(nursery, "ch", "pair_sender2", &[ch])
        .expect("spawn the sender");
    vm.spawn_fiber_in(nursery, "ch", "pair_recvr2", &[ch])
        .expect("spawn the receiver");
    match vm.join_nursery(nursery).expect("the drive consumes") {
        JoinOutcome::Joined(spans) => {
            assert_eq!(
                spans[0],
                FrameAnswer::one(Value::I64(0)),
                "the parked sender woke with the unit word"
            );
            assert_eq!(
                spans[1],
                FrameAnswer::one(Value::I64(4_177_012)),
                "both spans crossed in order, the parked one refilled whole into the freed slot"
            );
        },
        other @ JoinOutcome::Failed(_) => panic!("a clean join: {other:?}"),
    }
}
