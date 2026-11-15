//! The peek-next tag-branch fusion (BL-4's census-ranked third family): a
//! TAGTEST followed by the measured census branch spelling — JMPIF, the
//! match dispatch's arm-guard exit — testing the tag test's own dst takes
//! its branch decision inside the tag test's dispatch. The fused arm must
//! be indistinguishable from the unfused run: both branch outcomes
//! correct, the bool dst still readable on either path, a fused backward
//! branch observing the nursery cancel flag exactly where the
//! two-dispatch run does (jump-to-self included), the code-end boundary
//! guarded, an intervening RC word breaking the pair, and the unmeasured
//! spellings (JMPIF_NOT after a TAGTEST, every TAGTEST_NOT) staying
//! unfused.

use ds_vm::{
    ChildOutcome, Const, FiberState, FrameAnswer, FunctionDef, JoinOutcome, ModuleRegistry,
    ModuleVersion, NurseryId, Trap, Value, Vm,
    abi::{
        CodePc, ConstIdx, FrameLayout, SlotType,
        SlotType::{Bool, I64, Ref, Tag},
        uop::{Inst, RcOp, Uop},
    },
    assemble,
    native::PLATFORM_CANCELLED,
};

fn k(idx: u16) -> ConstIdx {
    ConstIdx(idx)
}

fn def(name: &str, slots: &[SlotType], params: usize, body: &[Uop]) -> FunctionDef {
    FunctionDef {
        name: name.into(),
        layout: FrameLayout::new(slots.to_vec(), params, 0).expect("valid fixture layout"),
        pool: Vec::new(),
        code: assemble(body).expect("fixture body encodes"),
    }
}

/// The mixed uop/RC-body spelling the adjacency fixture needs.
fn def_insts(name: &str, slots: &[SlotType], params: usize, body: &[Inst]) -> FunctionDef {
    FunctionDef {
        name: name.into(),
        layout: FrameLayout::new(slots.to_vec(), params, 0).expect("valid fixture layout"),
        pool: Vec::new(),
        code: assemble(body).expect("fixture body encodes"),
    }
}

/// One shared pool: k0 1, k1 10 (not-held marker), k2 20 (held marker),
/// k3 100 (the probe addend), k4 "doomed", k5 0, k6 false.
fn registry(defs: Vec<FunctionDef>) -> ModuleRegistry {
    let pool = [
        Const::I64(1),
        Const::I64(10),
        Const::I64(20),
        Const::I64(100),
        Const::Str("doomed".into()),
        Const::I64(0),
        Const::Bool(false),
    ];
    let defs = defs
        .into_iter()
        .map(|mut d| {
            d.pool = pool.to_vec();
            d
        })
        .collect();
    let mut reg = ModuleRegistry::new();
    reg.install(ModuleVersion::new("tf", defs).expect("valid fixture module"));
    reg
}

/// The straight-line pair body shared by the fused tag tests: the head
/// (FUNCF, TAGNEW) puts the scrutinee word in slot 0, the addend load
/// ahead of the pair so both paths carry it, the tag test at pc3 fused
/// with its Jmpif at pc4, a marker per path (20 the test held, 10 it
/// failed), and the pc8 probe — an unfused Jmpif reading the tag test's
/// dst — folding in 100 when the bool stayed false. Held: 20; failed:
/// 10 + 100.
fn pair_tail(tested: u8) -> Vec<Uop> {
    vec![
        Uop::Kimm { dst: 4, kidx: k(3) }, // pc2 addend 100
        Uop::TagTest {
            dst: 3,
            src: 0,
            tag: tested,
        }, // pc3 the fused tag test
        Uop::Jmpif { cond: 3, target: 7 }, // pc4 taken when held
        Uop::Kimm { dst: 2, kidx: k(1) }, // pc5 marker 10
        Uop::Jmp { target: 8 },           // pc6 join at the probe
        Uop::Kimm { dst: 2, kidx: k(2) }, // pc7 marker 20
        Uop::Jmpif {
            cond: 3,
            target: 10,
        }, // pc8 the probe
        Uop::AddI64 {
            dst: 2,
            lhs: 2,
            rhs: 4,
        }, // pc9
        Uop::Ret { src: 2 },              // pc10
    ]
}

/// Every fused pair takes both branch outcomes correctly and keeps the
/// bool dst readable on either path: the probe's fold answers the value
/// the tag test wrote, so a dropped or stale write changes the result.
#[test]
fn fused_pairs_take_their_branch_and_keep_the_bool_dst_readable() {
    let head = |tag: u8| -> Vec<Uop> {
        vec![
            Uop::Funcf { frame_slots: 5 },
            Uop::TagNew { dst: 0, tag }, // pc1 the scrutinee's discriminant
        ]
    };
    let hit = |tag: u8| {
        head(tag)
            .into_iter()
            .chain(pair_tail(tag))
            .collect::<Vec<_>>()
    };
    let failed = |tag: u8| {
        head(tag)
            .into_iter()
            .chain(pair_tail(tag ^ 0x80))
            .collect::<Vec<_>>()
    };
    let reg = registry(vec![
        def("hit_pair", &[Tag, I64, I64, Bool, I64], 0, &hit(7)),
        def("miss_pair", &[Tag, I64, I64, Bool, I64], 0, &failed(7)),
        def("hit_zero", &[Tag, I64, I64, Bool, I64], 0, &hit(0)),
    ]);
    let mut vm = Vm::new(reg);
    // tag 7 against 7 holds: the pair jumps, the probe reads the true
    assert_eq!(
        vm.run("tf", "hit_pair", &[]),
        Ok(FrameAnswer::one(Value::I64(20)))
    );
    // tag 7 against 135 fails: the pair falls through, the probe folds the false in
    assert_eq!(
        vm.run("tf", "miss_pair", &[]),
        Ok(FrameAnswer::one(Value::I64(110)))
    );
    // tag 0 against 0 holds: the zero discriminant is an ordinary tag
    assert_eq!(
        vm.run("tf", "hit_zero", &[]),
        Ok(FrameAnswer::one(Value::I64(20)))
    );
}

/// The match-dispatch loop shape the census measured the family on: the
/// fused pair runs every iteration (taken into the arm body through the
/// matching tag), beside the compare-branch slice's fused loop-head exit
/// and the unfused backward JMP back edge. Slots: 0 n (the arg), 1 one,
/// 2 the scrutinee, 3 the compare's bool, 4 the accumulator, 5 the
/// induction (zero-filled by the layout), 6 the guard's bool.
#[test]
fn the_match_dispatch_pair_runs_each_iteration() {
    let reg = registry(vec![def(
        "tag_loop",
        &[I64, I64, Tag, Bool, I64, I64, Bool],
        1,
        &[
            Uop::Funcf { frame_slots: 7 },
            Uop::Kimm { dst: 1, kidx: k(0) }, // one
            Uop::TagNew { dst: 2, tag: 4 },   // the scrutinee: tag 4
            Uop::LtI64 {
                dst: 3,
                lhs: 5,
                rhs: 0,
            }, // pc3 i < n, fused with pc4
            Uop::JmpifNot {
                cond: 3,
                target: 12,
            }, // pc4 exit
            Uop::TagTest {
                dst: 6,
                src: 2,
                tag: 4,
            }, // pc5 the arm guard, fused with pc6
            Uop::Jmpif { cond: 6, target: 8 }, // pc6 into the arm
            Uop::Jmp { target: 10 },          // pc7 the catchall skips the arm
            Uop::AddI64 {
                dst: 4,
                lhs: 4,
                rhs: 1,
            }, // pc8 arm body: acc += one
            Uop::Jmp { target: 10 },          // pc9 the arm joins the latch
            Uop::AddI64 {
                dst: 5,
                lhs: 5,
                rhs: 1,
            }, // pc10 i += one
            Uop::Jmp { target: 3 },           // pc11 the back edge
            Uop::Ret { src: 4 },              // pc12
        ],
    )]);
    let mut vm = Vm::new(reg);
    // zero trips: the very first compare pair is the exit
    assert_eq!(
        vm.run("tf", "tag_loop", &[Value::I64(0)]),
        Ok(FrameAnswer::one(Value::I64(0)))
    );
    // four iterations, the arm guard held each one: 4
    assert_eq!(
        vm.run("tf", "tag_loop", &[Value::I64(4)]),
        Ok(FrameAnswer::one(Value::I64(4)))
    );
}

/// `() -> int`: 1 / 0 — dies trapped, raising the nursery's cancel flag.
fn trap_child_def() -> FunctionDef {
    def("trap_child", &[I64, I64], 0, &[
        Uop::Funcf { frame_slots: 2 },
        Uop::Kimm { dst: 0, kidx: k(0) },
        Uop::Kimm { dst: 1, kidx: k(5) },
        Uop::DivI64 {
            dst: 0,
            lhs: 0,
            rhs: 1,
        },
        Uop::Ret { src: 0 },
    ])
}

/// `() -> int`: a TAGTEST that always holds (tag 5 against 5) fused with
/// a JMPIF jumping backward to the tag test — the loop whose only exit is
/// the cancel flag, taken through the fused back-edge.
fn loop_fused_def() -> FunctionDef {
    def("loop_fused", &[Tag, Bool], 0, &[
        Uop::Funcf { frame_slots: 2 },
        Uop::TagNew { dst: 0, tag: 5 }, // pc1
        Uop::TagTest {
            dst: 1,
            src: 0,
            tag: 5,
        }, // pc2 always true, fused with pc3
        Uop::Jmpif { cond: 1, target: 2 }, // pc3 always taken, backward
    ])
}

/// `() -> int`: the same always-holding tag test fused with a JMPIF
/// jumping to ITSELF — backward under the unfused spelling (t == the
/// branch's own pc), so the fused arm must observe there too: the pc
/// pre-bump onto the branch's word is what keeps the point identical.
fn loop_self_fused_def() -> FunctionDef {
    def("loop_self", &[Tag, Bool], 0, &[
        Uop::Funcf { frame_slots: 2 },
        Uop::TagNew { dst: 0, tag: 5 }, // pc1
        Uop::TagTest {
            dst: 1,
            src: 0,
            tag: 5,
        }, // pc2 always true, fused with pc3
        Uop::Jmpif { cond: 1, target: 3 }, // pc3 jump to self
    ])
}

/// A fused backward branch preempts a driven fiber exactly where the
/// unfused spelling does: under the raised cancel flag the looper dies
/// with the cancel sentinel at its first fused back-edge, and the join
/// reports the trapped sibling beside it.
#[test]
fn a_fused_backward_branch_observes_cancellation() {
    for looper in ["loop_fused", "loop_self"] {
        let mut vm = Vm::new(registry(vec![
            trap_child_def(),
            loop_fused_def(),
            loop_self_fused_def(),
        ]));
        let n: NurseryId = vm.open_root_nursery();
        let failing = vm
            .spawn_fiber_in(n, "tf", "trap_child", &[])
            .expect("spawn trap child");
        let spinning = vm
            .spawn_fiber_in(n, "tf", looper, &[])
            .expect("spawn looper");
        // the failing child's trap raises the flag
        assert_eq!(
            vm.resume_fiber(failing, Value::I64(0)),
            Err(Trap::DivideByZero)
        );
        // the looper driven under the flag dies at its first fused
        // back-edge (the self-jump variant hangs instead if the fused
        // branch ever lost the observation point)
        assert_eq!(
            vm.resume_fiber(spinning, Value::I64(0)),
            Ok(FrameAnswer::one(Value::Tag(PLATFORM_CANCELLED)))
        );
        assert_eq!(vm.fiber_state(spinning), Some(FiberState::Dead));
        assert_eq!(
            vm.join_nursery(n),
            Ok(JoinOutcome::Failed(vec![
                ChildOutcome::Trapped(Trap::DivideByZero),
                ChildOutcome::Cancelled,
            ]))
        );
    }
}

/// The end-of-code boundary: a tag test as the final word peeks nothing
/// and runs its plain path, and a fused pair as the final two words lands
/// on the same bounds trap the unfused run takes.
#[test]
fn the_code_end_boundary_peeks_nothing() {
    let reg = registry(vec![
        // the bare tag test is the whole body: no idx+1 word exists
        def("tail_test", &[Tag, Bool], 0, &[
            Uop::Funcf { frame_slots: 2 },
            Uop::TagNew { dst: 0, tag: 3 },
            Uop::TagTest {
                dst: 1,
                src: 0,
                tag: 3,
            },
        ]),
        // the fused pair closes the body: the not-taken fallthrough lands
        // past the end (the tag mismatches, so the JMPIF cannot take)
        def("tail_pair", &[Tag, Bool], 0, &[
            Uop::Funcf { frame_slots: 2 },
            Uop::TagNew { dst: 0, tag: 3 },
            Uop::TagTest {
                dst: 1,
                src: 0,
                tag: 9,
            },
            Uop::Jmpif { cond: 1, target: 1 },
        ]),
    ]);
    let mut vm = Vm::new(reg);
    assert_eq!(
        vm.run("tf", "tail_test", &[]),
        Err(Trap::InvalidPc { pc: 3, code_len: 3 })
    );
    assert_eq!(
        vm.run("tf", "tail_pair", &[]),
        Err(Trap::InvalidPc { pc: 4, code_len: 4 })
    );
}

/// An intervening RC word breaks the pair: the counted word executes in
/// its own dispatch (the first dec frees the string), the branch after it
/// still reads the tag test's dst, and the second dec names the freed
/// object and traps — a fusion leaping over the RC word would have
/// skipped the first dec and fallen past the second instead.
#[test]
fn an_intervening_rc_word_breaks_the_pair_and_still_executes() {
    let reg = registry(vec![def_insts("rc_break", &[Ref, Tag, Bool], 0, &[
        Uop::Funcf { frame_slots: 3 }.into(),
        Uop::Kstr { dst: 0, kidx: k(4) }.into(),
        Uop::TagNew { dst: 1, tag: 5 }.into(),
        Uop::TagTest {
            dst: 2,
            src: 1,
            tag: 9,
        }
        .into(), // pc3: its next word is the dec, not a branch
        RcOp::Dec { src: 0 }.into(), // pc4 frees the string
        Uop::Jmpif { cond: 2, target: 7 }.into(), /* pc5 unfused: reads the
                                      * held-false dst, falls through */
        RcOp::Dec { src: 0 }.into(), // pc6 names the dead object
        Uop::Ret { src: 0 }.into(),
    ])]);
    let mut vm = Vm::new(reg);
    assert_eq!(
        vm.run("tf", "rc_break", &[]),
        Err(Trap::UnknownObject { obj: 0 }),
        "the first dec freed the string, so the second dec names a dead id"
    );
    // the trap is the second RCDEC at pc 6 — the branch between ran as its
    // own dispatch
    assert_eq!(vm.backtrace()[0].pc, CodePc(6));
}

/// A JMPIF adjacent to the tag test but testing a different slot never
/// fuses: the branch decision reads that slot, not the tag test's fresh
/// bool.
#[test]
fn a_branch_testing_another_slot_stays_unfused() {
    let reg = registry(vec![def(
        "cond_mismatch",
        &[Tag, I64, I64, Bool, Bool],
        0,
        &[
            Uop::Funcf { frame_slots: 5 },
            Uop::TagNew { dst: 0, tag: 7 },
            Uop::Kconst { dst: 4, kidx: k(6) }, // the other slot: false
            Uop::TagTest {
                dst: 3,
                src: 0,
                tag: 7,
            }, // pc3 true, but pc4 does not test it
            Uop::Jmpif { cond: 4, target: 7 },  // pc4 not taken on slot 4's false
            Uop::Kimm { dst: 2, kidx: k(1) },
            Uop::Jmp { target: 8 },
            Uop::Kimm { dst: 2, kidx: k(2) },
            Uop::Ret { src: 2 },
        ],
    )]);
    let mut vm = Vm::new(reg);
    // the tag test held, but the branch tested slot 4 (false) and fell through
    assert_eq!(
        vm.run("tf", "cond_mismatch", &[]),
        Ok(FrameAnswer::one(Value::I64(10)))
    );
}

/// A JMPIF_NOT consuming the tag test's dst never fuses (the census
/// measured no such pair — the guard loops around tag tests spell JMPIF)
/// and keeps its own branch sense: not taken on the held bool.
#[test]
fn jmpif_not_consuming_the_tag_dst_stays_unfused() {
    let reg = registry(vec![def("jmpifnot_dst", &[Tag, I64, I64, Bool], 0, &[
        Uop::Funcf { frame_slots: 4 },
        Uop::TagNew { dst: 0, tag: 7 },
        Uop::TagTest {
            dst: 3,
            src: 0,
            tag: 7,
        }, // pc2 true
        Uop::JmpifNot { cond: 3, target: 6 }, // pc3 JMPIF_NOT: not taken, unfused
        Uop::Kimm { dst: 2, kidx: k(1) },
        Uop::Jmp { target: 7 },
        Uop::Kimm { dst: 2, kidx: k(2) },
        Uop::Ret { src: 2 },
    ])]);
    let mut vm = Vm::new(reg);
    assert_eq!(
        vm.run("tf", "jmpifnot_dst", &[]),
        Ok(FrameAnswer::one(Value::I64(10)))
    );
}

/// TAGTEST_NOT never fuses — no kernel stream carries the uop, so the
/// census measured no pair — and computes through the plain two-dispatch
/// path beside a JMPIF consuming its dst.
#[test]
fn tag_test_not_never_fuses() {
    let reg = registry(vec![def("not_pair", &[Tag, I64, I64, Bool], 0, &[
        Uop::Funcf { frame_slots: 4 },
        Uop::TagNew { dst: 0, tag: 7 },
        Uop::TagTestNot {
            dst: 3,
            src: 0,
            tag: 7,
        }, // pc2 false (the tag matches)
        Uop::Jmpif { cond: 3, target: 6 }, // pc3 not taken on the false
        Uop::Kimm { dst: 2, kidx: k(1) },
        Uop::Jmp { target: 7 },
        Uop::Kimm { dst: 2, kidx: k(2) },
        Uop::Ret { src: 2 },
    ])]);
    let mut vm = Vm::new(reg);
    // the inverted test failed, so the adjacent JMPIF fell through
    assert_eq!(
        vm.run("tf", "not_pair", &[]),
        Ok(FrameAnswer::one(Value::I64(10)))
    );
}
