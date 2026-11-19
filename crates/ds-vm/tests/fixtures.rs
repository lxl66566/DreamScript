//! Encoded fixture programs: the canonical ds-abi
//! hand encodings), the wide-callee CALLT regression, two-slot module swaps,
//! trap/backtrace behavior, and the float/conversion uops.

use ds_vm::{
    Const, ConstFuncRef, FrameAnswer, FunctionDef, ModuleRegistry, ModuleVersion, Trap, Value, Vm,
    abi::{
        BIAS_J, ConstIdx, FrameLayout, SlideError, SlotType,
        SlotType::{Bool, F64, I64},
        uop::Uop,
    },
    assemble,
};

fn k(idx: u16) -> ConstIdx {
    ConstIdx(idx)
}

fn funcref(module: &str, func: u32) -> Const {
    Const::FuncRef(ConstFuncRef::Named {
        module: module.into(),
        func,
    })
}

fn def(name: &str, slots: &[SlotType], params: usize, args: usize, body: &[Uop]) -> FunctionDef {
    FunctionDef {
        name: name.into(),
        layout: FrameLayout::new(slots.to_vec(), params, args).expect("valid fixture layout"),
        pool: Vec::new(),
        code: assemble(body).expect("fixture body encodes"),
    }
}

fn registry(module: &str, pool: &[Const], defs: Vec<FunctionDef>) -> ModuleRegistry {
    // fixtures are authored against one shared pool; each function's own
    // pool (the payload granularity) is a copy, and every index each body
    // uses is valid in it
    let defs = defs
        .into_iter()
        .map(|mut d| {
            d.pool = pool.to_vec();
            d
        })
        .collect();
    let mut reg = ModuleRegistry::new();
    reg.install(ModuleVersion::new(module, defs).expect("valid fixture module"));
    reg
}

/// `fn sum_to(n: i64) -> i64 { let mut acc = 0; while n > 0 { acc += n; n -= 1 }; acc }`
/// — the canonical shape, adapted from ds-abi's hand encoding
/// with two executable corrections (ds-abi's tests only round-trip the
/// words, never run them): the loop exit jumps with JMPIF, because the
/// locked JMPIF_NOT semantics ("jump when the condition is false") would
/// exit on the first iteration; and the 0 and 1 constants get separate
/// scratch slots, because ds-abi's single-scratch spelling turns the
/// comparison into `n <= 1` after the first iteration. Slots:
/// [n, acc, t, z, one]; pool k0 = 0, k1 = 1.
fn sum_to_fixture() -> ModuleRegistry {
    registry("mut", &[Const::I64(0), Const::I64(1)], vec![def(
        "sum_to",
        &[I64, I64, Bool, I64, I64],
        1,
        0,
        &[
            Uop::Funcf { frame_slots: 5 },
            Uop::Kimm { dst: 1, kidx: k(0) }, // acc = 0
            Uop::Kimm { dst: 3, kidx: k(0) }, // z = 0
            Uop::LeI64 {
                dst: 2,
                lhs: 0,
                rhs: 3,
            }, // t = n <= z
            Uop::Jmpif { cond: 2, target: 9 },
            Uop::AddI64 {
                dst: 1,
                lhs: 1,
                rhs: 0,
            }, // acc += n
            Uop::Kimm { dst: 4, kidx: k(1) }, // one = 1
            Uop::SubI64 {
                dst: 0,
                lhs: 0,
                rhs: 4,
            }, // n -= 1
            Uop::Jmp { target: 3 },
            Uop::Ret { src: 1 },
        ],
    )])
}

#[test]
fn sum_to_100_returns_5050() {
    let mut vm = Vm::new(sum_to_fixture());
    let result = vm
        .run("mut", "sum_to", &[Value::I64(100)])
        .expect("sum_to runs");
    assert_eq!(result, FrameAnswer::one(Value::I64(5050)));
}

/// Mutual tail calls, the canonical ds-abi encoding
/// (H2.a): slots [n, t, scratch, staged], pool k0=0, k1=1, k2=true,
/// k3=FuncRef odd, k4=FuncRef even, k5=false. KCONST supplies the base-case
/// bools directly.
fn even_odd_fixture() -> ModuleRegistry {
    let body = |callee: u16, base: u16| {
        vec![
            Uop::Funcf { frame_slots: 4 },
            Uop::Kimm { dst: 2, kidx: k(0) }, // stage 0
            Uop::EqI64 {
                dst: 1,
                lhs: 0,
                rhs: 2,
            }, // t = n == 0
            Uop::Jmpif { cond: 1, target: 7 },
            Uop::Kimm { dst: 2, kidx: k(1) }, // stage 1
            Uop::SubI64 {
                dst: 3,
                lhs: 0,
                rhs: 2,
            }, // stage n - 1 in the arg area
            Uop::Callt {
                callee: k(callee),
                nargs: 1,
            }, // slide the staged arg into slot 0
            Uop::Kconst {
                dst: 1,
                kidx: k(base),
            }, // t = true / false
            Uop::Ret { src: 1 },
        ]
    };
    registry(
        "mut",
        &[
            Const::I64(0),
            Const::I64(1),
            Const::Bool(true),
            funcref("mut", 1), // odd
            funcref("mut", 0), // even
            Const::Bool(false),
        ],
        vec![
            def("even", &[I64, Bool, I64, I64], 1, 1, &body(3, 2)),
            def("odd", &[I64, Bool, I64, I64], 1, 1, &body(4, 5)),
        ],
    )
}

#[test]
fn mutual_even_odd_at_ten_million_runs_at_constant_stack() {
    // The acceptance criterion: the value-stack length stays constant across
    // the whole 10^7-deep mutual tail recursion. The canonical fixture's
    // frame is 4 slots + metadata + call word = 6 words; any stack growth at
    // any of the 10^7 CALLTs would raise the high-water mark.
    let reg = even_odd_fixture();
    // the encoded words match ds-abi's own locked assertions: JMPIF at pc 3
    // carries the +3 bias, CALLT's d field is the callee pool index
    let even = reg
        .current_version("mut")
        .expect("installed")
        .function("even")
        .expect("present");
    assert_eq!(even.code[3] & 0xffff, u32::from(BIAS_J + 3));
    assert_eq!(even.code[6] & 0xffff, 3);

    let mut vm = Vm::new(reg);
    let depth = 10_000_000_i64;
    assert_eq!(
        vm.run("mut", "even", &[Value::I64(depth)]),
        Ok(FrameAnswer::one(Value::Bool(true)))
    );
    assert_eq!(vm.max_stack_len(), 6);
    assert_eq!(vm.stack_len(), 2); // metadata word + result word after the root RET
    assert_eq!(
        vm.run("mut", "odd", &[Value::I64(depth)]),
        Ok(FrameAnswer::one(Value::Bool(false)))
    );
    assert_eq!(
        vm.max_stack_len(),
        6,
        "no run may grow the stack past one frame"
    );
}

/// Self tail recursion that traps after N slides: `countdown(n)` tail-calls
/// itself until n == 0, then adds i64::MAX to itself. The trap's backtrace
/// must report the one real frame plus the tail chain's elided count [H2].
fn countdown_fixture() -> ModuleRegistry {
    registry(
        "t",
        &[
            Const::I64(0),
            Const::I64(1),
            Const::I64(i64::MAX),
            funcref("t", 0), // countdown
            Const::I64(5),
        ],
        vec![
            def("countdown", &[I64, Bool, I64, I64], 1, 1, &[
                Uop::Funcf { frame_slots: 4 },
                Uop::Kimm { dst: 2, kidx: k(0) },
                Uop::EqI64 {
                    dst: 1,
                    lhs: 0,
                    rhs: 2,
                },
                Uop::Jmpif { cond: 1, target: 7 },
                Uop::Kimm { dst: 2, kidx: k(1) },
                Uop::SubI64 {
                    dst: 3,
                    lhs: 0,
                    rhs: 2,
                },
                Uop::Callt {
                    callee: k(3),
                    nargs: 1,
                },
                Uop::Kimm { dst: 2, kidx: k(2) }, // stage MAX
                Uop::AddI64 {
                    dst: 0,
                    lhs: 2,
                    rhs: 2,
                }, // MAX + MAX: integer overflow
                Uop::Halt { slot: 0 },            // unreachable; keeps the body closed
            ]),
            def("main", &[I64], 0, 1, &[
                Uop::Funcf { frame_slots: 1 },
                Uop::Kimm { dst: 0, kidx: k(4) }, // stage 5
                Uop::Call {
                    callee: k(3),
                    nargs: 1,
                },
                Uop::Halt { slot: 0 },
            ]),
        ],
    )
}

#[test]
fn trapped_tail_chain_backtrace_reports_elided_frames() {
    let mut vm = Vm::new(countdown_fixture());
    let trap = vm.run("t", "main", &[]).expect_err("countdown traps");
    assert_eq!(trap, Trap::IntegerOverflow);
    let frames = vm.backtrace();
    // two real frames: main (caller) and countdown (trapping, reused frame)
    assert_eq!(frames.len(), 2);
    assert_eq!(frames[0].func, "countdown");
    assert_eq!(frames[0].pc, ds_vm::FramePc::At(ds_vm::abi::CodePc(8))); // the ADDI64
    assert_eq!(frames[0].elided, 5); // five tail calls: 5 -> 4 -> 3 -> 2 -> 1 -> 0
    assert_eq!(frames[1].func, "main");
    assert_eq!(frames[1].pc, ds_vm::FramePc::At(ds_vm::abi::CodePc(3))); // saved return pc, past the CALL
    assert_eq!(frames[1].elided, 0);
}

/// Two-slot module table [H6 item 5]: swapping in a new version redirects
/// subsequent qualified calls; the old version retires.
fn counter_version(get_value: i64) -> ModuleVersion {
    ModuleVersion::new("counter", vec![FunctionDef {
        pool: vec![Const::I64(get_value)],
        ..def("get", &[I64], 0, 0, &[
            Uop::Funcf { frame_slots: 1 },
            Uop::Kimm { dst: 0, kidx: k(0) },
            Uop::Ret { src: 0 },
        ])
    }])
    .expect("valid fixture module")
}

#[test]
fn module_slot_swap_redirects_qualified_calls() {
    let mut vm = Vm::new(ModuleRegistry::new());
    let v1_row = vm
        .install_module(counter_version(1))
        .expect("the swap installs");
    assert_eq!(
        vm.run("counter", "get", &[]),
        Ok(FrameAnswer::one(Value::I64(1)))
    );

    vm.install_module(counter_version(2))
        .expect("the swap installs");
    // the current slot now answers pool FuncRefs
    assert_eq!(
        vm.run("counter", "get", &[]),
        Ok(FrameAnswer::one(Value::I64(2)))
    );
    // the retiring slot still names the previous version's table row
    let retiring = vm
        .registry()
        .retiring_version("counter")
        .expect("the retiring row is live");
    assert_eq!(retiring.name(), "counter");
    assert_eq!(retiring.function("get").map(|f| f.code.len()), Some(3));

    // a third install retires the oldest version: the row left the
    // registry's own reachability and the sweep drops it, and the row is
    // the version's single home, so the version is gone
    vm.install_module(counter_version(3))
        .expect("the swap installs");
    assert_eq!(
        vm.run("counter", "get", &[]),
        Ok(FrameAnswer::one(Value::I64(3)))
    );
    assert!(vm.registry().version(v1_row).is_none());
}

/// Regression: a WIDE callee via CALLT — callee params exceed the caller's
/// params-plus-locals (the overlapping-ascending `slide_plan` shape from
/// ds-abi's own tests). The callee reuses the frame at the same fp, so its
/// param slots sit at fp+1+i below the callee's own call word; a destination
/// mapped through the caller's body would read the dead call word and
/// clobber the callee's. `g` stages (100, 10, 1) and tail-calls
/// `f(a, b, c) = a - b + c`; only the intact order yields 91.
#[test]
fn callt_wide_callee_receives_all_params_in_order() {
    let reg = registry(
        "wide",
        &[
            Const::I64(100),
            Const::I64(10),
            Const::I64(1),
            funcref("wide", 1),
        ],
        vec![
            def("g", &[I64, I64, I64, I64], 0, 3, &[
                Uop::Funcf { frame_slots: 4 },
                Uop::Kimm { dst: 1, kidx: k(0) }, // stage a
                Uop::Kimm { dst: 2, kidx: k(1) }, // stage b
                Uop::Kimm { dst: 3, kidx: k(2) }, // stage c
                Uop::Callt {
                    callee: k(3),
                    nargs: 3,
                },
                Uop::Ret { src: 0 },
            ]),
            def("f", &[I64, I64, I64, I64], 3, 0, &[
                Uop::Funcf { frame_slots: 4 },
                Uop::SubI64 {
                    dst: 3,
                    lhs: 0,
                    rhs: 1,
                }, // t = a - b
                Uop::AddI64 {
                    dst: 0,
                    lhs: 3,
                    rhs: 2,
                }, // a = t + c
                Uop::Ret { src: 0 },
            ]),
        ],
    );
    let mut vm = Vm::new(reg);
    assert_eq!(
        vm.run("wide", "g", &[]),
        Ok(FrameAnswer::one(Value::I64(91)))
    );
    // the tail call still reuses the frame: both frames are 6 words
    assert_eq!(vm.max_stack_len(), 6);
}

/// Deep NON-tail recursion: every CALL grows the stack by the callee frame,
/// so the per-entry limit compare traps [H1].
#[test]
fn deep_non_tail_recursion_traps_at_the_stack_limit() {
    let reg = registry("loop", &[funcref("loop", 0)], vec![def(
        "f",
        &[I64, I64],
        1,
        1,
        &[
            Uop::Funcf { frame_slots: 2 },
            Uop::SubI64 {
                dst: 1,
                lhs: 0,
                rhs: 0,
            }, // stage n - n
            Uop::Call {
                callee: k(0),
                nargs: 1,
            },
            Uop::Ret { src: 0 },
        ],
    )]);
    // each CALL overlaps the callee's params onto the caller's arg area, so
    // frames net two words each (frame = 4 words): a 64-word limit admits
    // 31 frames, and the 32nd entry needs 66
    let mut vm = Vm::with_limit(reg, 64);
    let trap = vm
        .run("loop", "f", &[Value::I64(1000)])
        .expect_err("must overflow");
    assert_eq!(trap, Trap::StackOverflow {
        needed: 66,
        limit: 64
    });
    assert_eq!(vm.backtrace().len(), 31);
    assert_eq!(vm.max_stack_len(), 64);
}

/// Call-site re-validation: hand-assembled CALLs are checked against the
/// ABI's own layout facts before any frame is pushed.
#[test]
fn call_sites_recheck_the_abi_layout_facts() {
    let g = || {
        def("g", &[I64], 1, 0, &[
            Uop::Funcf { frame_slots: 1 },
            Uop::Ret { src: 0 },
        ])
    };
    let caller = |name: &str, nargs: u8| {
        def(name, &[I64, Bool], 0, 1, &[
            Uop::Funcf { frame_slots: 2 },
            Uop::Kconst { dst: 1, kidx: k(0) }, // stage a bool
            Uop::Call {
                callee: k(1),
                nargs,
            },
            Uop::Halt { slot: 1 },
        ])
    };
    let pool = || vec![Const::Bool(true), funcref("v", 0)];

    // staged bool into an i64 parameter
    let mut vm = Vm::new(registry("v", &pool(), vec![g(), caller("main", 1)]));
    assert_eq!(
        vm.run("v", "main", &[]),
        Err(Trap::CallLayout(SlideError::TypeMismatch {
            index: 0,
            staged: Bool,
            expected: I64,
        }))
    );
    // nargs operand disagrees with the callee's parameter count
    let mut vm = Vm::new(registry("v", &pool(), vec![g(), caller("main", 2)]));
    assert_eq!(
        vm.run("v", "main", &[]),
        Err(Trap::ArityMismatch {
            expected: 1,
            given: 2
        })
    );
    // staging area smaller than the callee's parameters
    let mut vm = Vm::new(registry("v", &pool(), vec![
        g(),
        def("tiny", &[I64], 0, 0, &[
            Uop::Funcf { frame_slots: 1 },
            Uop::Call {
                callee: k(1),
                nargs: 1,
            },
            Uop::Halt { slot: 0 },
        ]),
    ]));
    assert_eq!(
        vm.run("v", "tiny", &[]),
        Err(Trap::CallLayout(SlideError::StagingTooSmall {
            available: 0,
            needed: 1
        }))
    );
}

/// The float uops: arithmetic, bit-structural equality, ordering, and both
/// conversions.
#[test]
fn float_uops_execute_with_bit_structural_equality() {
    let reg = registry("f", &[Const::F64(4.0), Const::F64(3.0)], vec![
        def("f64_ops", &[F64, F64, F64, F64, Bool, I64], 2, 0, &[
            Uop::Funcf { frame_slots: 6 },
            Uop::AddF64 {
                dst: 2,
                lhs: 0,
                rhs: 1,
            }, // t = 1.5 + 2.5 = 4.0
            Uop::Kconst { dst: 3, kidx: k(0) },
            Uop::EqF64 {
                dst: 4,
                lhs: 2,
                rhs: 3,
            }, // bit-structural: 4.0 == 4.0
            Uop::JmpifNot { cond: 4, target: 8 },
            Uop::SubF64 {
                dst: 2,
                lhs: 1,
                rhs: 0,
            }, // 1.0
            Uop::MulF64 {
                dst: 2,
                lhs: 2,
                rhs: 0,
            }, // 1.5
            Uop::DivF64 {
                dst: 2,
                lhs: 2,
                rhs: 0,
            }, // 1.0
            Uop::CvtF2I { dst: 5, src: 2 }, // 1.0 -> 1
            Uop::Halt { slot: 5 },
        ]),
        def("f64_cmp", &[F64, F64, Bool], 2, 0, &[
            Uop::Funcf { frame_slots: 3 },
            Uop::LtF64 {
                dst: 2,
                lhs: 0,
                rhs: 1,
            },
            Uop::Halt { slot: 2 },
        ]),
        def("f64_nan", &[F64, F64, Bool], 2, 0, &[
            Uop::Funcf { frame_slots: 3 },
            Uop::EqF64 {
                dst: 2,
                lhs: 0,
                rhs: 1,
            }, // NaN == NaN by bits [B9]
            Uop::Halt { slot: 2 },
        ]),
        def("icvt", &[I64, F64, F64, Bool], 1, 0, &[
            Uop::Funcf { frame_slots: 4 },
            Uop::CvtI2F { dst: 1, src: 0 },
            Uop::Kconst { dst: 2, kidx: k(1) },
            Uop::EqF64 {
                dst: 3,
                lhs: 1,
                rhs: 2,
            },
            Uop::Halt { slot: 3 },
        ]),
    ]);
    let mut vm = Vm::new(reg);
    assert_eq!(
        vm.run("f", "f64_ops", &[Value::F64(1.5), Value::F64(2.5)]),
        Ok(FrameAnswer::one(Value::I64(1)))
    );
    assert_eq!(
        vm.run("f", "f64_cmp", &[Value::F64(1.0), Value::F64(2.0)]),
        Ok(FrameAnswer::one(Value::Bool(true)))
    );
    assert_eq!(
        vm.run("f", "f64_cmp", &[Value::F64(2.0), Value::F64(1.0)]),
        Ok(FrameAnswer::one(Value::Bool(false)))
    );
    assert_eq!(
        vm.run("f", "f64_nan", &[
            Value::F64(f64::NAN),
            Value::F64(f64::NAN)
        ]),
        Ok(FrameAnswer::one(Value::Bool(true)))
    );
    assert_eq!(
        vm.run("f", "icvt", &[Value::I64(3)]),
        Ok(FrameAnswer::one(Value::Bool(true)))
    );
}

/// The B7 trap edges: zero divide, MIN / -1, multiplication overflow, the
/// f64-to-i64 conversion range, and pool-kind mismatches.
#[test]
fn arithmetic_and_conversion_edges_trap() {
    let bin_i64 = |name: &str, ka: u16, kb: u16, op: Uop| {
        def(name, &[I64, I64], 0, 0, &[
            Uop::Funcf { frame_slots: 2 },
            Uop::Kimm {
                dst: 0,
                kidx: k(ka),
            },
            Uop::Kimm {
                dst: 1,
                kidx: k(kb),
            },
            op,
            Uop::Ret { src: 0 },
        ])
    };
    let reg = registry(
        "edge",
        &[
            Const::I64(7),
            Const::I64(0),
            Const::I64(i64::MAX),
            Const::I64(2),
            Const::F64(1e30),
            Const::I64(i64::MIN),
            Const::I64(-1),
            Const::Bool(true),
        ],
        vec![
            bin_i64("div0", 0, 1, Uop::DivI64 {
                dst: 0,
                lhs: 0,
                rhs: 1,
            }),
            bin_i64("min_div", 5, 6, Uop::DivI64 {
                dst: 0,
                lhs: 0,
                rhs: 1,
            }),
            bin_i64("mul_ovf", 2, 3, Uop::MulI64 {
                dst: 0,
                lhs: 0,
                rhs: 1,
            }),
            def("cvt_ovf", &[F64, I64], 0, 0, &[
                Uop::Funcf { frame_slots: 2 },
                Uop::Kconst { dst: 0, kidx: k(4) },
                Uop::CvtF2I { dst: 1, src: 0 },
                Uop::Ret { src: 1 },
            ]),
        ],
    );
    let mut vm = Vm::new(reg);
    assert_eq!(vm.run("edge", "div0", &[]), Err(Trap::DivideByZero));
    assert_eq!(vm.run("edge", "min_div", &[]), Err(Trap::IntegerOverflow));
    assert_eq!(vm.run("edge", "mul_ovf", &[]), Err(Trap::IntegerOverflow));
    assert_eq!(vm.run("edge", "cvt_ovf", &[]), Err(Trap::CvtOutOfRange));

    // KIMM on a bool entry is a typed LOAD error now, not a runtime trap
    let bad = FunctionDef {
        pool: vec![Const::Bool(true)],
        ..def("pool_kind", &[Bool], 0, 0, &[
            Uop::Funcf { frame_slots: 1 },
            Uop::Kimm { dst: 0, kidx: k(0) }, // KIMM on a bool entry
            Uop::Ret { src: 0 },
        ])
    };
    assert_eq!(
        ModuleVersion::new("edge", vec![bad]).unwrap_err(),
        ds_vm::ModuleBuildError::PoolKind {
            func: "pool_kind".into(),
            kidx: 0,
            expected: ds_vm::ConstKind::I64,
            found: ds_vm::ConstKind::Bool,
        }
    );
}

/// The wrapping IntArith trio: modulo-2^64 answers on the overflow edges
/// the trapping forms refuse, over the same pool constants.
#[test]
fn wrap_arith_arms_wrap_and_never_trap() {
    let bin_i64 = |name: &str, ka: u16, kb: u16, op: Uop| {
        def(name, &[I64, I64], 0, 0, &[
            Uop::Funcf { frame_slots: 2 },
            Uop::Kimm {
                dst: 0,
                kidx: k(ka),
            },
            Uop::Kimm {
                dst: 1,
                kidx: k(kb),
            },
            op,
            Uop::Ret { src: 0 },
        ])
    };
    let reg = registry(
        "wrap",
        &[
            Const::I64(i64::MAX),
            Const::I64(1),
            Const::I64(i64::MIN),
            Const::I64(-1),
            Const::I64(2),
        ],
        vec![
            bin_i64("wadd", 0, 1, Uop::WrapAddI64 {
                dst: 0,
                lhs: 0,
                rhs: 1,
            }),
            bin_i64("wsub", 2, 1, Uop::WrapSubI64 {
                dst: 0,
                lhs: 0,
                rhs: 1,
            }),
            bin_i64("wmul_min", 2, 3, Uop::WrapMulI64 {
                dst: 0,
                lhs: 0,
                rhs: 1,
            }),
            bin_i64("wmul_two", 2, 4, Uop::WrapMulI64 {
                dst: 0,
                lhs: 0,
                rhs: 1,
            }),
        ],
    );
    let mut vm = Vm::new(reg);
    // MAX +% 1 wraps to MIN
    assert_eq!(
        vm.run("wrap", "wadd", &[]),
        Ok(FrameAnswer::one(Value::I64(i64::MIN)))
    );
    // MIN -% 1 wraps to MAX
    assert_eq!(
        vm.run("wrap", "wsub", &[]),
        Ok(FrameAnswer::one(Value::I64(i64::MAX)))
    );
    // MIN *% -1 wraps to MIN (the only value `x *% -1` cannot name)
    assert_eq!(
        vm.run("wrap", "wmul_min", &[]),
        Ok(FrameAnswer::one(Value::I64(i64::MIN)))
    );
    // MIN *% 2 wraps to zero
    assert_eq!(
        vm.run("wrap", "wmul_two", &[]),
        Ok(FrameAnswer::one(Value::I64(0)))
    );
}

/// Entry-argument validation and HALT reporting.
#[test]
fn run_entry_checks_and_halt_report_the_slot_value() {
    let reg = registry("h", &[Const::I64(42)], vec![def("main", &[I64], 0, 0, &[
        Uop::Funcf { frame_slots: 1 },
        Uop::Kimm { dst: 0, kidx: k(0) },
        Uop::Halt { slot: 0 },
    ])]);
    let mut vm = Vm::new(reg);
    assert_eq!(
        vm.run("h", "main", &[]),
        Ok(FrameAnswer::one(Value::I64(42)))
    );

    let mut vm = Vm::new(sum_to_fixture());
    assert_eq!(
        vm.run("mut", "sum_to", &[]),
        Err(Trap::ArityMismatch {
            expected: 1,
            given: 0
        })
    );
    assert_eq!(
        vm.run("mut", "sum_to", &[Value::F64(1.0)]),
        Err(Trap::SlotTypeMismatch {
            slot: 0,
            expected: I64,
            actual: F64,
        })
    );
    assert_eq!(
        vm.run("mut", "nope", &[]),
        Err(Trap::UnknownFunction {
            module: "mut".into(),
            func: "nope".into()
        })
    );
}
