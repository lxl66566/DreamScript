//! The peek-next compare-branch fusion (the census' second-ranked
//! family): an LTI64/EQI64/GTF64 followed by the measured census branch
//! spelling — JMPIF_NOT testing the compare's own dst — takes its branch
//! decision inside the compare's dispatch. The fused arm must be
//! indistinguishable from the unfused run: both branch outcomes correct,
//! the bool dst still readable on either path, a fused backward branch
//! observing the nursery cancel flag exactly where the two-dispatch run
//! does (jump-to-self included), the code-end boundary guarded, and an
//! intervening RC word breaking the pair.

use ds_vm::{
    ChildOutcome, Const, FiberState, FrameAnswer, FramePc, FunctionDef, JoinOutcome,
    ModuleRegistry, ModuleVersion, NurseryId, Trap, Value, Vm,
    abi::{
        CodePc, ConstIdx, FrameLayout, SlotType,
        SlotType::{Bool, F64, I64, Ref},
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

/// One shared pool: k0 10, k1 10, k2 20, k3 100, k4 2.5, k5 false, k6 1,
/// k7 0, k8 "doomed".
fn registry(defs: Vec<FunctionDef>) -> ModuleRegistry {
    let pool = [
        Const::I64(10),
        Const::I64(10),
        Const::I64(20),
        Const::I64(100),
        Const::F64(2.5),
        Const::Bool(false),
        Const::I64(1),
        Const::I64(0),
        Const::Str("doomed".into()),
    ];
    let defs = defs
        .into_iter()
        .map(|mut d| {
            d.pool = pool.to_vec();
            d
        })
        .collect();
    let mut reg = ModuleRegistry::new();
    reg.install(ModuleVersion::new("cf", defs).expect("valid fixture module"));
    reg
}

/// The straight-line pair body shared by the three fused compares: the
/// compare at pc2, its fused JMPIF_NOT at pc3, a marker per path (10 the
/// compare held, 20 it failed), and the pc8 probe — an unfused JMPIF_NOT
/// reading the compare's dst — folding in 100 when the bool stayed true.
/// Held: 10 + 100; failed: 20 + 0.
fn pair_tail() -> Vec<Uop> {
    vec![
        Uop::JmpifNot { cond: 3, target: 6 }, // pc3
        Uop::Kimm { dst: 2, kidx: k(1) },     // pc4 marker 10
        Uop::Jmp { target: 7 },               // pc5
        Uop::Kimm { dst: 2, kidx: k(2) },     // pc6 marker 20
        Uop::Kimm { dst: 4, kidx: k(3) },     // pc7 addend 100
        Uop::JmpifNot {
            cond: 3,
            target: 10,
        }, // pc8 the probe
        Uop::AddI64 {
            dst: 2,
            lhs: 2,
            rhs: 4,
        }, // pc9
        Uop::Ret { src: 2 },                  // pc10
    ]
}

/// Every fused pair takes both branch outcomes correctly and keeps the
/// bool dst readable on either path: the probe's fold answers the value
/// the compare wrote, so a dropped or stale write changes the result.
#[test]
fn fused_pairs_take_their_branch_and_keep_the_bool_dst_readable() {
    let head = |compare: Uop| -> Vec<Uop> {
        vec![
            Uop::Funcf { frame_slots: 5 },
            Uop::Kimm { dst: 1, kidx: k(0) }, // pc1 the bound
            compare,                          // pc2 the fused compare
        ]
    };
    let reg = registry(vec![
        def(
            "lt_pair",
            &[I64, I64, I64, Bool, I64],
            2,
            &head(Uop::LtI64 {
                dst: 3,
                lhs: 0,
                rhs: 1,
            })
            .into_iter()
            .chain(pair_tail())
            .collect::<Vec<_>>(),
        ),
        def(
            "eq_pair",
            &[I64, I64, I64, Bool, I64],
            2,
            &head(Uop::EqI64 {
                dst: 3,
                lhs: 0,
                rhs: 1,
            })
            .into_iter()
            .chain(pair_tail())
            .collect::<Vec<_>>(),
        ),
        def(
            "gtf_pair",
            &[F64, F64, I64, Bool, I64],
            2,
            &{
                let mut body = vec![Uop::Funcf { frame_slots: 5 }, Uop::Kconst {
                    dst: 1,
                    kidx: k(4),
                }];
                body.push(Uop::GtF64 {
                    dst: 3,
                    lhs: 0,
                    rhs: 1,
                });
                body
            }
            .into_iter()
            .chain(pair_tail())
            .collect::<Vec<_>>(),
        ),
    ]);
    let mut vm = Vm::new(reg);
    let i = |v: i64| Value::I64(v);
    let f = |v: f64| Value::F64(v);
    // 5 < 10 holds: the pair falls through, the probe folds the true in
    assert_eq!(
        vm.run("cf", "lt_pair", &[i(5), i(10)]),
        Ok(FrameAnswer::one(i(110)))
    );
    // 15 < 10 fails: the fused branch jumps, the probe reads false
    assert_eq!(
        vm.run("cf", "lt_pair", &[i(15), i(10)]),
        Ok(FrameAnswer::one(i(20)))
    );
    assert_eq!(
        vm.run("cf", "eq_pair", &[i(10), i(10)]),
        Ok(FrameAnswer::one(i(110)))
    );
    assert_eq!(
        vm.run("cf", "eq_pair", &[i(9), i(10)]),
        Ok(FrameAnswer::one(i(20)))
    );
    assert_eq!(
        vm.run("cf", "gtf_pair", &[f(2.75), f(2.5)]),
        Ok(FrameAnswer::one(i(110)))
    );
    assert_eq!(
        vm.run("cf", "gtf_pair", &[f(2.5), f(2.5)]),
        Ok(FrameAnswer::one(i(20)))
    );
}

/// The loop-head shape the census measured the family on: the fused pair
/// runs every iteration (taken through the body, not taken at the exit),
/// beside the unfused backward JMP back edge.
#[test]
fn the_loop_head_pair_runs_each_iteration() {
    let reg = registry(vec![def("lt_loop", &[I64, I64, I64, I64, Bool], 1, &[
        Uop::Funcf { frame_slots: 5 },
        Uop::Kimm { dst: 2, kidx: k(6) }, // one
        Uop::Kimm { dst: 1, kidx: k(7) }, // i = 0 (slot 0 is n, the arg)
        Uop::LtI64 {
            dst: 4,
            lhs: 1,
            rhs: 0,
        }, // pc3 i < n, fused with pc4
        Uop::JmpifNot { cond: 4, target: 8 }, // pc4 exit
        Uop::AddI64 {
            dst: 3,
            lhs: 3,
            rhs: 1,
        }, // pc5 acc += i
        Uop::AddI64 {
            dst: 1,
            lhs: 1,
            rhs: 2,
        }, // pc6 i += 1
        Uop::Jmp { target: 3 },           // pc7 the back edge
        Uop::Ret { src: 3 },              // pc8
    ])]);
    let mut vm = Vm::new(reg);
    // zero trips: the very first pair is the exit
    assert_eq!(
        vm.run("cf", "lt_loop", &[Value::I64(0)]),
        Ok(FrameAnswer::one(Value::I64(0)))
    );
    // four taken iterations then the exit: 0 + 1 + 2 + 3
    assert_eq!(
        vm.run("cf", "lt_loop", &[Value::I64(4)]),
        Ok(FrameAnswer::one(Value::I64(6)))
    );
}

/// `() -> int`: 1 / 0 — dies trapped, raising the nursery's cancel flag.
fn trap_child_def() -> FunctionDef {
    def("trap_child", &[I64, I64], 0, &[
        Uop::Funcf { frame_slots: 2 },
        Uop::Kimm { dst: 0, kidx: k(6) },
        Uop::Kimm { dst: 1, kidx: k(7) },
        Uop::DivI64 {
            dst: 0,
            lhs: 0,
            rhs: 1,
        },
        Uop::Ret { src: 0 },
    ])
}

/// `() -> int`: `LtI64 0 < 0` (false) + a JMPIF_NOT jumping backward to
/// the compare — the loop whose only exit is the cancel flag, taken
/// through the fused back-edge.
fn loop_fused_def() -> FunctionDef {
    def("loop_fused", &[I64, I64, Bool], 0, &[
        Uop::Funcf { frame_slots: 3 },
        Uop::Kimm { dst: 1, kidx: k(7) }, // bound 0
        Uop::LtI64 {
            dst: 2,
            lhs: 0,
            rhs: 1,
        }, // pc2 always false, fused with pc3
        Uop::JmpifNot { cond: 2, target: 2 }, // pc3 always taken, backward
    ])
}

/// `() -> int`: the same always-false compare fused with a JMPIF_NOT
/// jumping to ITSELF — backward under the unfused spelling (t == the
/// branch's own pc), so the fused arm must observe there too: the pc
/// pre-bump onto the branch's word is what keeps the point identical.
fn loop_self_fused_def() -> FunctionDef {
    def("loop_self", &[I64, Bool], 0, &[
        Uop::Funcf { frame_slots: 2 },
        Uop::LtI64 {
            dst: 1,
            lhs: 0,
            rhs: 0,
        }, // pc1 i < i, always false, fused with pc2
        Uop::JmpifNot { cond: 1, target: 2 }, // pc2 jump to self
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
            .spawn_fiber_in(n, "cf", "trap_child", &[])
            .expect("spawn trap child");
        let spinning = vm
            .spawn_fiber_in(n, "cf", looper, &[])
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

/// The end-of-code boundary: a compare as the final word peeks nothing
/// and runs its plain path, and a fused pair as the final two words lands
/// on the same bounds trap the unfused run takes.
#[test]
fn the_code_end_boundary_peeks_nothing() {
    let reg = registry(vec![
        // the bare compare is the whole body: no idx+1 word exists
        def("tail_cmp", &[I64, I64, Bool], 2, &[
            Uop::Funcf { frame_slots: 3 },
            Uop::LtI64 {
                dst: 2,
                lhs: 0,
                rhs: 1,
            },
        ]),
        // the fused pair closes the body: the not-taken fallthrough lands
        // past the end
        def("tail_pair", &[I64, I64, Bool], 2, &[
            Uop::Funcf { frame_slots: 3 },
            Uop::EqI64 {
                dst: 2,
                lhs: 0,
                rhs: 1,
            },
            Uop::JmpifNot { cond: 2, target: 1 },
        ]),
    ]);
    let mut vm = Vm::new(reg);
    assert_eq!(
        vm.run("cf", "tail_cmp", &[Value::I64(1), Value::I64(2)]),
        Err(Trap::InvalidPc { pc: 2, code_len: 2 })
    );
    assert_eq!(
        vm.run("cf", "tail_pair", &[Value::I64(7), Value::I64(7)]),
        Err(Trap::InvalidPc { pc: 3, code_len: 3 })
    );
}

/// An intervening RC word breaks the pair: the counted word executes in
/// its own dispatch (the first dec frees the string), the branch after it
/// still reads the compare's dst, and the second dec names the freed
/// object and traps — a fusion leaping over the RC word would have
/// skipped the first dec and returned instead.
#[test]
fn an_intervening_rc_word_breaks_the_pair_and_still_executes() {
    let reg = registry(vec![def_insts("rc_break", &[I64, I64, Ref, Bool], 2, &[
        Uop::Funcf { frame_slots: 4 }.into(),
        Uop::Kstr { dst: 2, kidx: k(8) }.into(),
        Uop::LtI64 {
            dst: 3,
            lhs: 0,
            rhs: 1,
        }
        .into(), // pc2: its next word is the dec, not a branch
        RcOp::Dec { src: 2 }.into(), // pc3 frees the string
        Uop::JmpifNot { cond: 3, target: 6 }.into(), /* pc4 unfused: reads the held-true dst,
                                      * falls through */
        RcOp::Dec { src: 2 }.into(), // pc5 names the dead object
        Uop::Ret { src: 0 }.into(),
    ])]);
    let mut vm = Vm::new(reg);
    assert_eq!(
        vm.run("cf", "rc_break", &[Value::I64(5), Value::I64(10)]),
        Err(Trap::UnknownObject { obj: 0 }),
        "the first dec freed the string, so the second dec names a dead id"
    );
    // the trap is the second RCDEC at pc 5 — the branch between ran as its
    // own dispatch
    assert_eq!(vm.backtrace()[0].pc, FramePc::At(CodePc(5)));
}

/// A JMPIF_NOT adjacent to the compare but testing a different slot never
/// fuses: the branch decision reads that slot, not the compare's fresh
/// bool.
#[test]
fn a_branch_testing_another_slot_stays_unfused() {
    let reg = registry(vec![def(
        "cond_mismatch",
        &[I64, I64, I64, Bool, Bool],
        2,
        &[
            Uop::Funcf { frame_slots: 5 },
            Uop::Kconst { dst: 4, kidx: k(5) }, // the other slot: false
            Uop::LtI64 {
                dst: 3,
                lhs: 0,
                rhs: 1,
            }, // pc2 true, but pc3 does not test it
            Uop::JmpifNot { cond: 4, target: 6 }, // pc3 taken on slot 4's false
            Uop::Kimm { dst: 2, kidx: k(1) },
            Uop::Jmp { target: 7 },
            Uop::Kimm { dst: 2, kidx: k(2) },
            Uop::Ret { src: 2 },
        ],
    )]);
    let mut vm = Vm::new(reg);
    // 1 < 10 is true, but the branch tested slot 4 (false) and jumped
    assert_eq!(
        vm.run("cf", "cond_mismatch", &[Value::I64(1), Value::I64(10)]),
        Ok(FrameAnswer::one(Value::I64(20)))
    );
}

/// A JMPIF consuming the compare's dst never fuses (the census measured
/// no such pair) and keeps its own branch sense: taken on the held bool.
#[test]
fn jmpif_consuming_the_compare_dst_stays_unfused() {
    let reg = registry(vec![def("jmpif_dst", &[I64, I64, I64, Bool], 2, &[
        Uop::Funcf { frame_slots: 4 },
        Uop::Kimm { dst: 1, kidx: k(0) },
        Uop::LtI64 {
            dst: 3,
            lhs: 0,
            rhs: 1,
        }, // pc2 true
        Uop::Jmpif { cond: 3, target: 6 }, // pc3 JMPIF: taken, unfused
        Uop::Kimm { dst: 2, kidx: k(1) },
        Uop::Jmp { target: 7 },
        Uop::Kimm { dst: 2, kidx: k(2) },
        Uop::Ret { src: 2 },
    ])]);
    let mut vm = Vm::new(reg);
    assert_eq!(
        vm.run("cf", "jmpif_dst", &[Value::I64(1), Value::I64(10)]),
        Ok(FrameAnswer::one(Value::I64(20)))
    );
}
