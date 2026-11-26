//! The channel deadline twins e2e over the builtin seams (`chan_send_deadline`
//! 108, `chan_recv_deadline` 109): hand-assembled programs beside the channel
//! triple and the sleep op, the clock itself the witness — hermetic, bounds with
//! generous headroom (a concurrent lane shares the box). What the fixtures
//! pin:
//!
//! - the at-once letter: a past deadline answers the failure span in place, no
//!   park, while a servable channel answers the ok span under the same past
//!   deadline (the deadline bounds only the wait).
//! - the ok answer spans: the send's unit word under the ok discriminant, the
//!   recv's element word under it, both at the parked call site.
//! - the wheel's deadline door: a parked recv on the empty channel and a parked
//!   send on the full one answer the `timed_out` class span at their deadline.
//! - the win door: a deadline waiter woken before its deadline answers the ok
//!   span (the win drops the wheel row, so no stale door can meet the fiber).
//! - the loser's deregistration: a timed-out recv leaves the next parked
//!   receiver the wake; a timed-out send's staged payload never crosses, the
//!   survivor's refills in park order; a timed-out sender re-parking deposits
//!   exactly one payload (the loser's first answer never staged twice).
//! - the middle-loser ordering: three parked receivers, the middle one timed
//!   out, the two wakes still pair in park order.

use std::time::{Duration, Instant};

use ds_vm::{
    Const, ConstFuncRef, FrameAnswer, FunctionDef, JoinOutcome, ModuleRegistry, ModuleVersion,
    Value, Vm,
    abi::{
        ConstIdx, FrameLayout, SlotType,
        SlotType::{I64, Raw, Ref},
        uop::Uop,
    },
    assemble,
    native::{
        BUILTIN_CHAN_NEW, BUILTIN_CHAN_RECV, BUILTIN_CHAN_RECV_DEADLINE, BUILTIN_CHAN_SEND,
        BUILTIN_CHAN_SEND_DEADLINE, PLATFORM_IO_OK, PLATFORM_IO_TIMED_OUT,
    },
    platform::{Capability, OP_SLEEP, Platform, STD_NAME},
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

/// The shared pool: the channel triple and the deadline twins, the sleep
/// op, then the int constants — the deadlines, the sequencing sleep, and
/// the payloads.
fn pool() -> Vec<Const> {
    vec![
        builtin(BUILTIN_CHAN_NEW),          // k0
        builtin(BUILTIN_CHAN_SEND),         // k1
        builtin(BUILTIN_CHAN_RECV),         // k2
        builtin(BUILTIN_CHAN_SEND_DEADLINE),// k3
        builtin(BUILTIN_CHAN_RECV_DEADLINE),// k4
        Const::FuncRef(ConstFuncRef::Named {
            module: STD_NAME.into(),
            func: OP_SLEEP,
        }),                                 // k5
        Const::I64(SOON),                   // k6: the short deadline
        Const::I64(SPARE),                   // k7: the generous deadline
        Const::I64(MARGIN),                 // k8: the sequencing sleep
        Const::I64(77),                      // k9
        Const::I64(5),                       // k10: the filler element
        Const::I64(11),                      // k11: the loser's payload
        Const::I64(22),                      // k12: the survivor's payload
        Const::I64(41),                      // k13
        Const::I64(100),                     // k14: the drain fold's radix
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
    reg.install_platform(Platform::new(STD_NAME, &[Capability::Time]));
    reg.install(ModuleVersion::new("dl", defs).expect("valid fixture module"));
    reg
}

/// A VM with the platform's reactor installed: the timing fixtures ride
/// the wheel through a real park.
fn scheduled_vm(defs: Vec<FunctionDef>) -> Vm {
    let mut vm = Vm::new(registry(defs));
    if let Some(reactor) = ds_vm::reactor::platform_default() {
        vm.install_reactor(reactor);
    }
    vm
}

/// Whether this host parks (a reactor driver exists): the wheel-doored
/// fixtures ride it, the rest run everywhere.
fn parks() -> bool {
    ds_vm::reactor::platform_default().is_some()
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
    vm.run("dl", "make", &[Value::I64(cap)])
        .expect("construction runs")
        .as_slice()
        .first()
        .copied()
        .expect("the channel word")
}

/// `send_plain(ch, x)`: one plain send, the unit answered — the filler
/// and the waker.
fn send_plain_def() -> FunctionDef {
    def("send_plain", &[Ref, I64, Raw, Raw], 2, 2, &[
        Uop::Funcf { frame_slots: 4 },
        Uop::Mov { dst: 2, src: 0 },
        Uop::Mov { dst: 3, src: 1 },
        Uop::Call {
            callee: k(1),
            nargs: 2,
        },
        Uop::Ret { src: 2 },
    ])
}

/// `recv_plain(ch) -> int`: one plain recv — the parking probe and the
/// drain word.
fn recv_plain_def() -> FunctionDef {
    def("recv_plain", &[Ref, Raw, Raw], 1, 2, &[
        Uop::Funcf { frame_slots: 3 },
        Uop::Mov { dst: 1, src: 0 },
        Uop::Call {
            callee: k(2),
            nargs: 1,
        },
        Uop::Ret { src: 1 },
    ])
}

/// `senddl_disc(ch, nanos, x) -> tag`: one deadline send answering the
/// span's discriminant word — the loser probe and the parked sender.
fn senddl_disc_def() -> FunctionDef {
    def("senddl_disc", &[Ref, I64, I64, Raw, Raw, Raw], 3, 3, &[
        Uop::Funcf { frame_slots: 6 },
        Uop::Mov { dst: 3, src: 0 },
        Uop::Mov { dst: 4, src: 1 },
        Uop::Mov { dst: 5, src: 2 },
        Uop::Call {
            callee: k(3),
            nargs: 3,
        },
        Uop::Ret { src: 3 },
    ])
}

/// `recvdl_disc(ch, nanos) -> tag`: one deadline recv answering the span's
/// discriminant word — the loser probe and the parked receiver.
fn recvdl_disc_def() -> FunctionDef {
    def("recvdl_disc", &[Ref, I64, Raw, Raw], 2, 2, &[
        Uop::Funcf { frame_slots: 4 },
        Uop::Mov { dst: 2, src: 0 },
        Uop::Mov { dst: 3, src: 1 },
        Uop::Call {
            callee: k(4),
            nargs: 2,
        },
        Uop::Ret { src: 2 },
    ])
}

/// `recvdl_word(ch, nanos) -> int`: the deadline recv's answer payload
/// word — the element under the ok discriminant, the inert unit word on
/// expiry.
fn recvdl_word_def() -> FunctionDef {
    def("recvdl_word", &[Ref, I64, Raw, Raw], 2, 2, &[
        Uop::Funcf { frame_slots: 4 },
        Uop::Mov { dst: 2, src: 0 },
        Uop::Mov { dst: 3, src: 1 },
        Uop::Call {
            callee: k(4),
            nargs: 2,
        },
        Uop::Ret { src: 3 },
    ])
}

/// `senddl_twice(ch) -> tag`: the loser re-park — the first deadline send
/// stages 11 and times out, the second stages 22 with a generous
/// deadline and parks.
fn senddl_twice_def() -> FunctionDef {
    def("senddl_twice", &[Ref, Raw, Raw, Raw, Raw, Raw], 1, 3, &[
        Uop::Funcf { frame_slots: 6 },
        Uop::Mov { dst: 3, src: 0 },
        Uop::Kimm { dst: 4, kidx: k(6) },
        Uop::Kimm { dst: 5, kidx: k(11) },
        Uop::Call {
            callee: k(3),
            nargs: 3,
        },
        Uop::Mov { dst: 1, src: 3 },
        Uop::Mov { dst: 3, src: 0 },
        Uop::Kimm { dst: 4, kidx: k(7) },
        Uop::Kimm { dst: 5, kidx: k(12) },
        Uop::Call {
            callee: k(3),
            nargs: 3,
        },
        Uop::Ret { src: 3 },
    ])
}

/// `sleepy_send(ch, x)`: sleeps the sequencing margin past the loser's
/// short deadline, then plain-sends — the waker the loser no longer
/// serves.
fn sleepy_send_def() -> FunctionDef {
    def("sleepy_send", &[Ref, I64, Raw, Raw], 2, 2, &[
        Uop::Funcf { frame_slots: 4 },
        Uop::Kimm { dst: 2, kidx: k(8) },
        Uop::Call {
            callee: k(5),
            nargs: 1,
        },
        Uop::Mov { dst: 2, src: 0 },
        Uop::Mov { dst: 3, src: 1 },
        Uop::Call {
            callee: k(1),
            nargs: 2,
        },
        Uop::Ret { src: 2 },
    ])
}

/// `sleepy_sends2(ch, a, b)`: after the margin sleep, two plain sends —
/// the pair waking two parked receivers in order.
fn sleepy_sends2_def() -> FunctionDef {
    def("sleepy_sends2", &[Ref, I64, I64, Raw, Raw], 3, 2, &[
        Uop::Funcf { frame_slots: 5 },
        Uop::Kimm { dst: 3, kidx: k(8) },
        Uop::Call {
            callee: k(5),
            nargs: 1,
        },
        Uop::Mov { dst: 3, src: 0 },
        Uop::Mov { dst: 4, src: 1 },
        Uop::Call {
            callee: k(1),
            nargs: 2,
        },
        Uop::Mov { dst: 3, src: 0 },
        Uop::Mov { dst: 4, src: 2 },
        Uop::Call {
            callee: k(1),
            nargs: 2,
        },
        Uop::Ret { src: 3 },
    ])
}

/// `sleepy_drain2(ch) -> int`: after the margin sleep, two plain recvs
/// folded `first*100 + second` — the drain naming which staged payload
/// crossed.
fn sleepy_drain2_def() -> FunctionDef {
    def("sleepy_drain2", &[Ref, I64, I64, Raw, Raw], 1, 2, &[
        Uop::Funcf { frame_slots: 5 },
        Uop::Kimm { dst: 3, kidx: k(8) },
        Uop::Call {
            callee: k(5),
            nargs: 1,
        },
        Uop::Mov { dst: 3, src: 0 },
        Uop::Call {
            callee: k(2),
            nargs: 1,
        },
        Uop::Mov { dst: 1, src: 3 },
        Uop::Mov { dst: 3, src: 0 },
        Uop::Call {
            callee: k(2),
            nargs: 1,
        },
        Uop::Kimm { dst: 2, kidx: k(14) },
        Uop::MulI64 {
            dst: 1,
            lhs: 1,
            rhs: 2,
        },
        Uop::AddI64 {
            dst: 1,
            lhs: 1,
            rhs: 3,
        },
        Uop::Ret { src: 1 },
    ])
}

/// A deadline far inside every parked loser's short deadline the tests
/// arm; the sequencing sleep sits far past it.
const SOON: i64 = 20_000_000;
/// A deadline far past every fixture's completion.
const SPARE: i64 = 5_000_000_000;
/// The sequencing sleep: past the loser's deadline, inside the generous
/// one.
const MARGIN: i64 = 150_000_000;

#[test]
fn a_past_deadline_answers_the_failure_span_at_once() {
    // no reactor: the past deadline never parks, both doors
    let mut vm = Vm::new(registry(vec![
        make_def(),
        send_plain_def(),
        senddl_disc_def(),
        recvdl_disc_def(),
    ]));
    let full = channel_of(&mut vm, 1);
    vm.run("dl", "send_plain", &[full, Value::I64(5)])
        .expect("the fill deposits");
    // the full channel's send: the failure answers in place
    for nanos in [0, -7] {
        assert_eq!(
            vm.run("dl", "senddl_disc", &[full, Value::I64(nanos), Value::I64(99)]),
            Ok(FrameAnswer::one(Value::Tag(PLATFORM_IO_TIMED_OUT))),
            "{nanos}: the past deadline answers the send's failure span"
        );
    }
    // the empty channel's recv: the failure answers in place
    let empty = channel_of(&mut vm, 1);
    for nanos in [0, -7] {
        assert_eq!(
            vm.run("dl", "recvdl_disc", &[empty, Value::I64(nanos)]),
            Ok(FrameAnswer::one(Value::Tag(PLATFORM_IO_TIMED_OUT))),
            "{nanos}: the past deadline answers the recv's failure span"
        );
    }
    // the deadline bounds only the wait: a servable channel answers the
    // ok span under the same past deadline — the stocked drain, the
    // roomy send
    assert_eq!(
        vm.run("dl", "recvdl_disc", &[full, Value::I64(0)]),
        Ok(FrameAnswer::one(Value::Tag(PLATFORM_IO_OK))),
        "the stocked channel drains under a past deadline"
    );
    assert_eq!(
        vm.run("dl", "senddl_disc", &[empty, Value::I64(0), Value::I64(6)]),
        Ok(FrameAnswer::one(Value::Tag(PLATFORM_IO_OK))),
        "the roomy channel stocks under a past deadline"
    );
}

#[test]
fn the_ok_answer_spans_carry_the_unit_and_the_element_word() {
    let mut vm = Vm::new(registry(vec![
        make_def(),
        send_plain_def(),
        senddl_disc_def(),
        recvdl_word_def(),
    ]));
    let ch = channel_of(&mut vm, 1);
    assert_eq!(
        vm.run("dl", "senddl_disc", &[ch, Value::I64(SPARE), Value::I64(77)]),
        Ok(FrameAnswer::one(Value::Tag(PLATFORM_IO_OK))),
        "the send's ok span answers the unit word under the ok discriminant"
    );
    assert_eq!(
        vm.run("dl", "recvdl_word", &[ch, Value::I64(SPARE)]),
        Ok(FrameAnswer::one(Value::I64(77))),
        "the recv's ok span answers the element word under it"
    );
}

#[test]
fn the_wheel_answers_the_parked_recv_loser() {
    if !parks() {
        return;
    }
    let mut vm = scheduled_vm(vec![make_def(), recvdl_disc_def()]);
    let ch = channel_of(&mut vm, 1);
    let started = Instant::now();
    assert_eq!(
        vm.run("dl", "recvdl_disc", &[ch, Value::I64(SOON)]),
        Ok(FrameAnswer::one(Value::Tag(PLATFORM_IO_TIMED_OUT))),
        "the deadline answers the recv's failure span"
    );
    assert!(
        started.elapsed() >= Duration::from_nanos(SOON.cast_unsigned()) * 3 / 4,
        "the deadline was waited out"
    );
}

#[test]
fn the_wheel_answers_the_parked_send_loser() {
    if !parks() {
        return;
    }
    let mut vm = scheduled_vm(vec![
        make_def(),
        send_plain_def(),
        senddl_disc_def(),
    ]);
    let ch = channel_of(&mut vm, 1);
    vm.run("dl", "send_plain", &[ch, Value::I64(5)])
        .expect("the fill deposits");
    let started = Instant::now();
    assert_eq!(
        vm.run("dl", "senddl_disc", &[ch, Value::I64(SOON), Value::I64(11)]),
        Ok(FrameAnswer::one(Value::Tag(PLATFORM_IO_TIMED_OUT))),
        "the deadline answers the send's failure span"
    );
    assert!(
        started.elapsed() >= Duration::from_nanos(SOON.cast_unsigned()) * 3 / 4,
        "the deadline was waited out"
    );
}

#[test]
fn the_win_door_wakes_a_deadline_waiter_before_its_deadline() {
    // both sides: the parked deadline recv takes the sibling's deposit,
    // the parked deadline send refills from the sibling's drain — each
    // answering the ok span, the row the win drops never firing after
    let mut vm = Vm::new(registry(vec![
        make_def(),
        send_plain_def(),
        recv_plain_def(),
        senddl_disc_def(),
        recvdl_word_def(),
    ]));
    // the recv's win: the element word answers at the parked call
    let ch = channel_of(&mut vm, 1);
    let nursery = vm.open_root_nursery();
    vm.spawn_fiber_in(nursery, "dl", "recvdl_word", &[ch, Value::I64(SPARE)])
        .expect("spawn the deadline receiver");
    vm.spawn_fiber_in(nursery, "dl", "send_plain", &[ch, Value::I64(77)])
        .expect("spawn the sender");
    match vm.join_nursery(nursery).expect("the drive consumes") {
        JoinOutcome::Joined(spans) => {
            assert_eq!(
                spans[0],
                FrameAnswer::one(Value::I64(77)),
                "the deadline recv woke with the element word, not the failure span"
            );
            assert_eq!(spans[1], FrameAnswer::one(Value::I64(0)), "the sender's unit");
        },
        other @ JoinOutcome::Failed(_) => panic!("a clean join: {other:?}"),
    }
    // the send's win: the ok discriminant answers at the parked call
    let full = channel_of(&mut vm, 1);
    vm.run("dl", "send_plain", &[full, Value::I64(5)])
        .expect("the fill deposits");
    let nursery = vm.open_root_nursery();
    vm.spawn_fiber_in(
        nursery,
        "dl",
        "senddl_disc",
        &[full, Value::I64(SPARE), Value::I64(11)],
    )
    .expect("spawn the deadline sender");
    vm.spawn_fiber_in(nursery, "dl", "recv_plain", &[full])
        .expect("spawn the drainer");
    match vm.join_nursery(nursery).expect("the drive consumes") {
        JoinOutcome::Joined(spans) => {
            assert_eq!(
                spans[0],
                FrameAnswer::one(Value::Tag(PLATFORM_IO_OK)),
                "the deadline send woke with the ok span, not the failure span"
            );
            assert_eq!(spans[1], FrameAnswer::one(Value::I64(5)), "the drained word");
        },
        other @ JoinOutcome::Failed(_) => panic!("a clean join: {other:?}"),
    }
}

#[test]
fn the_timed_out_recv_leaves_the_next_waiter_the_wake() {
    if !parks() {
        return;
    }
    // the loser parks first, the plain survivor parks behind it, the
    // waker sleeps past the loser's deadline: the loser answers the
    // failure span and the survivor still takes the wake — no lost
    // wake, no stale head swallowing the deposit
    let mut vm = scheduled_vm(vec![
        make_def(),
        send_plain_def(),
        recv_plain_def(),
        recvdl_disc_def(),
        sleepy_send_def(),
    ]);
    let ch = channel_of(&mut vm, 1);
    let nursery = vm.open_root_nursery();
    vm.spawn_fiber_in(nursery, "dl", "recvdl_disc", &[ch, Value::I64(SOON)])
        .expect("spawn the deadline loser");
    vm.spawn_fiber_in(nursery, "dl", "recv_plain", &[ch])
        .expect("spawn the plain survivor");
    vm.spawn_fiber_in(nursery, "dl", "sleepy_send", &[ch, Value::I64(77)])
        .expect("spawn the delayed waker");
    match vm.join_nursery(nursery).expect("the drive consumes") {
        JoinOutcome::Joined(spans) => {
            assert_eq!(
                spans[0],
                FrameAnswer::one(Value::Tag(PLATFORM_IO_TIMED_OUT)),
                "the deadline loser answered the failure span"
            );
            assert_eq!(
                spans[1],
                FrameAnswer::one(Value::I64(77)),
                "the next waiter took the wake the loser left"
            );
            assert_eq!(spans[2], FrameAnswer::one(Value::I64(0)), "the waker's unit");
        },
        other @ JoinOutcome::Failed(_) => panic!("a clean join: {other:?}"),
    }
}

#[test]
fn the_timed_out_senders_payload_never_crosses() {
    if !parks() {
        return;
    }
    // a full channel's deadline loser staged 11, the plain survivor staged
    // 22 behind it; after the loser times out the drain answers the
    // filler then the survivor's payload — the loser's staged span never
    // crosses
    let mut vm = scheduled_vm(vec![
        make_def(),
        send_plain_def(),
        senddl_disc_def(),
        sleepy_drain2_def(),
    ]);
    let ch = channel_of(&mut vm, 1);
    vm.run("dl", "send_plain", &[ch, Value::I64(5)])
        .expect("the fill deposits");
    let nursery = vm.open_root_nursery();
    vm.spawn_fiber_in(nursery, "dl", "senddl_disc", &[ch, Value::I64(SOON), Value::I64(11)])
        .expect("spawn the deadline loser");
    vm.spawn_fiber_in(nursery, "dl", "send_plain", &[ch, Value::I64(22)])
        .expect("spawn the plain survivor");
    vm.spawn_fiber_in(nursery, "dl", "sleepy_drain2", &[ch])
        .expect("spawn the delayed drain");
    match vm.join_nursery(nursery).expect("the drive consumes") {
        JoinOutcome::Joined(spans) => {
            assert_eq!(
                spans[0],
                FrameAnswer::one(Value::Tag(PLATFORM_IO_TIMED_OUT)),
                "the deadline loser answered the failure span"
            );
            assert_eq!(spans[1], FrameAnswer::one(Value::I64(0)), "the survivor's unit");
            assert_eq!(
                spans[2],
                FrameAnswer::one(Value::I64(522)),
                "the drain answered the filler then the survivor's 22, never the loser's 11"
            );
        },
        other @ JoinOutcome::Failed(_) => panic!("a clean join: {other:?}"),
    }
}

#[test]
fn the_timed_out_senders_first_payload_is_deregistered_not_re_delivered() {
    if !parks() {
        return;
    }
    // the deregistration's own pin: the loser's first deadline send timed
    // out and the second re-parked — the drain answers the second's
    // payload alone, so the first entry left the queue with its answer
    // (a lingering first entry would pair the wake with the loser's
    // first payload and answer 511)
    let mut vm = scheduled_vm(vec![
        make_def(),
        send_plain_def(),
        senddl_twice_def(),
        sleepy_drain2_def(),
    ]);
    let ch = channel_of(&mut vm, 1);
    vm.run("dl", "send_plain", &[ch, Value::I64(5)])
        .expect("the fill deposits");
    let nursery = vm.open_root_nursery();
    vm.spawn_fiber_in(nursery, "dl", "senddl_twice", &[ch])
        .expect("spawn the re-parking sender");
    vm.spawn_fiber_in(nursery, "dl", "sleepy_drain2", &[ch])
        .expect("spawn the delayed drain");
    match vm.join_nursery(nursery).expect("the drive consumes") {
        JoinOutcome::Joined(spans) => {
            assert_eq!(
                spans[0],
                FrameAnswer::one(Value::Tag(PLATFORM_IO_OK)),
                "the re-parked second send answered the ok span"
            );
            assert_eq!(
                spans[1],
                FrameAnswer::one(Value::I64(522)),
                "the drain answered the filler then exactly one staged payload, the second's 22"
            );
        },
        other @ JoinOutcome::Failed(_) => panic!("a clean join: {other:?}"),
    }
}

#[test]
fn the_middle_loser_keeps_the_wakes_paired_in_park_order() {
    if !parks() {
        return;
    }
    // three parked receivers, the middle one on a short deadline: it
    // times out, and the two later sends still wake the first and the
    // third in park order
    let mut vm = scheduled_vm(vec![
        make_def(),
        send_plain_def(),
        recv_plain_def(),
        recvdl_disc_def(),
        sleepy_sends2_def(),
    ]);
    let ch = channel_of(&mut vm, 1);
    let nursery = vm.open_root_nursery();
    vm.spawn_fiber_in(nursery, "dl", "recv_plain", &[ch])
        .expect("spawn the first receiver");
    vm.spawn_fiber_in(nursery, "dl", "recvdl_disc", &[ch, Value::I64(SOON)])
        .expect("spawn the middle loser");
    vm.spawn_fiber_in(nursery, "dl", "recv_plain", &[ch])
        .expect("spawn the third receiver");
    vm.spawn_fiber_in(nursery, "dl", "sleepy_sends2", &[ch, Value::I64(41), Value::I64(77)])
        .expect("spawn the delayed pair sender");
    match vm.join_nursery(nursery).expect("the drive consumes") {
        JoinOutcome::Joined(spans) => {
            assert_eq!(spans[0], FrameAnswer::one(Value::I64(41)), "the first took 41");
            assert_eq!(
                spans[1],
                FrameAnswer::one(Value::Tag(PLATFORM_IO_TIMED_OUT)),
                "the middle loser answered the failure span"
            );
            assert_eq!(spans[2], FrameAnswer::one(Value::I64(77)), "the third took 77");
            assert_eq!(spans[3], FrameAnswer::one(Value::I64(0)), "the sender's unit");
        },
        other @ JoinOutcome::Failed(_) => panic!("a clean join: {other:?}"),
    }
}
