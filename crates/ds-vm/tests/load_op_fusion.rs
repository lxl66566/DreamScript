//! The peek-next load-op fusion (BL-4's census-ranked first slice): a
//! KIMM/KCONST followed by its measured census arith consuming the loaded
//! slot runs both uops in the load's dispatch arm. The fused arm must be
//! indistinguishable from the unfused run: results equal, the loaded slot
//! still readable afterwards, traps attributed to the arith's own pc, an
//! intervening RC word breaking the pair, and the code-end boundary
//! guarded.

use ds_vm::{
    Const, FrameAnswer, FunctionDef, ModuleRegistry, ModuleVersion, Trap, Value, Vm,
    abi::{
        CodePc, ConstIdx, FrameLayout, Inst, RcOp, SlotType,
        SlotType::{F64, I64, Ref},
        uop::Uop,
    },
    assemble,
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

fn registry(pool: &[Const], defs: Vec<FunctionDef>) -> ModuleRegistry {
    let defs = defs
        .into_iter()
        .map(|mut d| {
            d.pool = pool.to_vec();
            d
        })
        .collect();
    let mut reg = ModuleRegistry::new();
    reg.install(ModuleVersion::new("f", defs).expect("valid fixture module"));
    reg
}

/// Every fused pair computes its unfused result, and the loaded slot stays
/// readable after the pair: a third op re-reads it. The add/sub/mul pairs
/// consume the load through lhs, the mod and float pairs through rhs (or
/// both), covering each operand position of the predicate.
#[test]
fn fused_pairs_compute_and_keep_the_loaded_slot_readable() {
    let reg = registry(
        &[
            Const::I64(7),
            Const::F64(0.25),
            Const::I64(i64::MAX),
            Const::I64(2),
        ],
        vec![
            // r1 = 7; r2 = r1 + n; r3 = r1 + r2
            def("add_k", &[I64, I64, I64, I64], 1, &[
                Uop::Funcf { frame_slots: 4 },
                Uop::Kimm { dst: 1, kidx: k(0) },
                Uop::AddI64 {
                    dst: 2,
                    lhs: 1,
                    rhs: 0,
                },
                Uop::AddI64 {
                    dst: 3,
                    lhs: 1,
                    rhs: 2,
                },
                Uop::Ret { src: 3 },
            ]),
            // r1 = 7; r2 = n - r1; r3 = r1 - r2
            def("sub_k", &[I64, I64, I64, I64], 1, &[
                Uop::Funcf { frame_slots: 4 },
                Uop::Kimm { dst: 1, kidx: k(0) },
                Uop::SubI64 {
                    dst: 2,
                    lhs: 0,
                    rhs: 1,
                },
                Uop::SubI64 {
                    dst: 3,
                    lhs: 1,
                    rhs: 2,
                },
                Uop::Ret { src: 3 },
            ]),
            // r1 = 7; r2 = r1 * n; r3 = r2 * r1
            def("mul_k", &[I64, I64, I64, I64], 1, &[
                Uop::Funcf { frame_slots: 4 },
                Uop::Kimm { dst: 1, kidx: k(0) },
                Uop::MulI64 {
                    dst: 2,
                    lhs: 1,
                    rhs: 0,
                },
                Uop::MulI64 {
                    dst: 3,
                    lhs: 2,
                    rhs: 1,
                },
                Uop::Ret { src: 3 },
            ]),
            // r1 = 7; r2 = n % r1; r3 = r2 + r1
            def("mod_k", &[I64, I64, I64, I64], 1, &[
                Uop::Funcf { frame_slots: 4 },
                Uop::Kimm { dst: 1, kidx: k(0) },
                Uop::ModI64 {
                    dst: 2,
                    lhs: 0,
                    rhs: 1,
                },
                Uop::AddI64 {
                    dst: 3,
                    lhs: 2,
                    rhs: 1,
                },
                Uop::Ret { src: 3 },
            ]),
            // r1 = 0.25; r2 = r1 + x; r3 = r1 + r2
            def("fadd_k", &[F64, F64, F64, F64], 1, &[
                Uop::Funcf { frame_slots: 4 },
                Uop::Kconst { dst: 1, kidx: k(1) },
                Uop::AddF64 {
                    dst: 2,
                    lhs: 1,
                    rhs: 0,
                },
                Uop::AddF64 {
                    dst: 3,
                    lhs: 1,
                    rhs: 2,
                },
                Uop::Ret { src: 3 },
            ]),
            // r1 = 0.25; r2 = x * r1; r3 = r2 * r1
            def("fmul_k", &[F64, F64, F64, F64], 1, &[
                Uop::Funcf { frame_slots: 4 },
                Uop::Kconst { dst: 1, kidx: k(1) },
                Uop::MulF64 {
                    dst: 2,
                    lhs: 0,
                    rhs: 1,
                },
                Uop::MulF64 {
                    dst: 3,
                    lhs: 2,
                    rhs: 1,
                },
                Uop::Ret { src: 3 },
            ]),
        ],
    );
    let mut vm = Vm::new(reg);
    let i = |v: i64| Value::I64(v);
    let f = |v: f64| Value::F64(v);
    // 7 + 3 = 10, then 7 + 10 = 17
    assert_eq!(vm.run("f", "add_k", &[i(3)]), Ok(FrameAnswer::one(i(17))));
    // 3 - 7 = -4, then 7 - (-4) = 11
    assert_eq!(vm.run("f", "sub_k", &[i(3)]), Ok(FrameAnswer::one(i(11))));
    // 7 * 6 = 42, then 42 * 7 = 294
    assert_eq!(vm.run("f", "mul_k", &[i(6)]), Ok(FrameAnswer::one(i(294))));
    // 30 % 7 = 2, then 2 + 7 = 9
    assert_eq!(vm.run("f", "mod_k", &[i(30)]), Ok(FrameAnswer::one(i(9))));
    // 0.25 + 0.5 = 0.75, then 0.25 + 0.75 = 1.0
    assert_eq!(
        vm.run("f", "fadd_k", &[f(0.5)]),
        Ok(FrameAnswer::one(f(1.0)))
    );
    // 0.5 * 0.25 = 0.125, then 0.125 * 0.25 = 0.03125
    assert_eq!(
        vm.run("f", "fmul_k", &[f(0.5)]),
        Ok(FrameAnswer::one(f(0.03125)))
    );
}

/// The arith may overwrite the loaded slot itself: the load's write, the
/// operand reads, and the arith's write keep their unfused order.
#[test]
fn a_fused_pair_may_overwrite_the_loaded_slot() {
    let reg = registry(&[Const::I64(7)], vec![def("acc", &[I64, I64], 1, &[
        Uop::Funcf { frame_slots: 2 },
        Uop::Kimm { dst: 1, kidx: k(0) },
        Uop::AddI64 {
            dst: 1,
            lhs: 1,
            rhs: 0,
        },
        Uop::Ret { src: 1 },
    ])]);
    let mut vm = Vm::new(reg);
    assert_eq!(
        vm.run("f", "acc", &[Value::I64(3)]),
        Ok(FrameAnswer::one(Value::I64(10)))
    );
}

/// A fused arith's trap attributes to the arith's own pc — the backtrace
/// of a fused overflow names pc+1, never the load's pc — and the fused
/// MODI64 keeps the zero-divide trap kind.
#[test]
fn fused_arith_traps_attribute_to_the_ariths_own_pc() {
    let reg = registry(&[Const::I64(i64::MAX), Const::I64(0)], vec![
        // r1 = MAX; r2 = r1 + r1: overflow at pc 2, the ADDI64
        def("ovf", &[I64, I64, I64], 1, &[
            Uop::Funcf { frame_slots: 3 },
            Uop::Kimm { dst: 1, kidx: k(0) },
            Uop::AddI64 {
                dst: 2,
                lhs: 1,
                rhs: 1,
            },
            Uop::Ret { src: 2 },
        ]),
        // r1 = 0; r2 = n % r1: zero divide at pc 2, the MODI64
        def("mod0", &[I64, I64, I64], 1, &[
            Uop::Funcf { frame_slots: 3 },
            Uop::Kimm { dst: 1, kidx: k(1) },
            Uop::ModI64 {
                dst: 2,
                lhs: 0,
                rhs: 1,
            },
            Uop::Ret { src: 2 },
        ]),
    ]);
    let mut vm = Vm::new(reg);
    assert_eq!(
        vm.run("f", "ovf", &[Value::I64(1)]),
        Err(Trap::IntegerOverflow)
    );
    assert_eq!(vm.backtrace()[0].pc, CodePc(2));
    assert_eq!(
        vm.run("f", "mod0", &[Value::I64(5)]),
        Err(Trap::DivideByZero)
    );
    assert_eq!(vm.backtrace()[0].pc, CodePc(2));
}

/// An intervening RC word breaks the pair: the counted word executes in
/// its own dispatch (the first dec frees the string), the arith after it
/// still computes, and the second dec names the freed object and traps —
/// a fusion leaping over the RC word would have skipped the first dec and
/// returned instead.
#[test]
fn an_intervening_rc_word_breaks_the_pair_and_still_executes() {
    let reg = registry(&[Const::Str("doomed".into()), Const::I64(7)], vec![
        def_insts("rc_break", &[Ref, I64, I64], 0, &[
            Uop::Funcf { frame_slots: 3 }.into(),
            Uop::Kstr { dst: 0, kidx: k(0) }.into(),
            Uop::Kimm { dst: 1, kidx: k(1) }.into(),
            RcOp::Dec { src: 0 }.into(),
            Uop::AddI64 {
                dst: 2,
                lhs: 1,
                rhs: 1,
            }
            .into(),
            RcOp::Dec { src: 0 }.into(),
            Uop::Ret { src: 2 }.into(),
        ]),
    ]);
    let mut vm = Vm::new(reg);
    assert_eq!(
        vm.run("f", "rc_break", &[]),
        Err(Trap::UnknownObject { obj: 0 }),
        "the first dec freed the string, so the second dec names a dead id"
    );
    // the trap is the second RCDEC at pc 5 — the arith between ran as its
    // own dispatch
    assert_eq!(vm.backtrace()[0].pc, CodePc(5));
}

/// The end-of-code boundary: a load as the final word peeks nothing and
/// runs its plain path, and a fused pair as the final two words lands on
/// the same bounds trap the unfused run takes.
#[test]
fn the_code_end_boundary_peeks_nothing() {
    let reg = registry(&[Const::I64(7)], vec![
        // the bare KIMM is the whole body: no idx+1 word exists
        def("tail_load", &[I64], 0, &[
            Uop::Funcf { frame_slots: 1 },
            Uop::Kimm { dst: 0, kidx: k(0) },
        ]),
        // the fused pair closes the body: pc += 2 lands past the end
        def("tail_pair", &[I64, I64], 0, &[
            Uop::Funcf { frame_slots: 2 },
            Uop::Kimm { dst: 0, kidx: k(0) },
            Uop::AddI64 {
                dst: 1,
                lhs: 0,
                rhs: 0,
            },
        ]),
    ]);
    let mut vm = Vm::new(reg);
    assert_eq!(
        vm.run("f", "tail_load", &[]),
        Err(Trap::InvalidPc { pc: 2, code_len: 2 })
    );
    assert_eq!(
        vm.run("f", "tail_pair", &[]),
        Err(Trap::InvalidPc { pc: 3, code_len: 3 })
    );
}

/// DIVI64 consuming the loaded slot never fuses (the census never measured
/// the pair) and computes through the plain two-dispatch path.
#[test]
fn div_consumes_the_loaded_slot_without_fusing() {
    let reg = registry(&[Const::I64(7)], vec![def(
        "div_k",
        &[I64, I64, I64],
        1,
        &[
            Uop::Funcf { frame_slots: 3 },
            Uop::Kimm { dst: 1, kidx: k(0) },
            Uop::DivI64 {
                dst: 2,
                lhs: 0,
                rhs: 1,
            },
            Uop::Ret { src: 2 },
        ],
    )]);
    let mut vm = Vm::new(reg);
    assert_eq!(
        vm.run("f", "div_k", &[Value::I64(14)]),
        Ok(FrameAnswer::one(Value::I64(2)))
    );
}
