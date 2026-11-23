//! The discard seams' count accounting: a spawned child's completed
//! answer is taken over by exactly three receivers — the fork stage, the
//! host face, and a claims-crossing re-acquire — while the in-language
//! boundary collects every completed span into its answers list (the
//! join channel) and the debug count join takes no answer at all. The
//! count seam owes every `Ref` word of each discarded `Completed` span
//! one no-ledger dec (the child's claim row released at its death, the
//! count left in flight for a receiver that never comes), so the word's
//! unique death still runs at the join; the boundary's list adopts the
//! words instead, and the joiner's own dec of the delivered list frees
//! them — either way the child's heap drains with the parent's, and no
//! output observes the discard.

use ds_vm::{
    Const, ConstFuncRef, FIBER_ROOT, FiberId, FrameAnswer, FunctionDef, ModuleRegistry,
    ModuleVersion, Platform, Value, Vm,
    abi::{
        ConstIdx, FrameLayout, Inst, SlotType,
        uop::{Uop, rc::RcOp},
    },
    assemble, native,
    native::{
        BUILTIN_NURSERY_BOUNDARY, BUILTIN_NURSERY_JOIN, BUILTIN_NURSERY_OPEN, BUILTIN_NURSERY_SPAWN,
    },
};

fn k(idx: u16) -> ConstIdx {
    ConstIdx(idx)
}

fn def(name: &str, slots: &[SlotType], params: usize, args: usize, body: &[Inst]) -> FunctionDef {
    FunctionDef {
        name: name.into(),
        layout: FrameLayout::new(slots.to_vec(), params, args).expect("valid fixture layout"),
        pool: Vec::new(),
        code: assemble(body).expect("fixture body encodes"),
    }
}

fn builtin(index: u32) -> Const {
    Const::FuncRef(ConstFuncRef::Named {
        module: "builtin".into(),
        func: index,
    })
}

/// The answering child: its entry frame births one fresh string and
/// returns the `Ref` word — the count transfers with the completion, no
/// dec anywhere in the child.
fn answer_child() -> FunctionDef {
    let mut child = def("answer_child", &[SlotType::Ref], 0, 0, &[
        Uop::Funcf { frame_slots: 1 }.into(),
        Uop::Kstr { dst: 0, kidx: k(0) }.into(),
        Uop::Ret { src: 0 }.into(),
    ]);
    child.pool = vec![Const::Str("discarded answer".into())];
    child
}

fn registry(version: ModuleVersion) -> Vm {
    let mut reg = ModuleRegistry::new();
    reg.install_native(native::BUILTIN_MODULE, native::builtin_module());
    reg.install_platform(Platform::std());
    reg.install(version);
    let mut vm = Vm::new(reg);
    vm.capture_output();
    vm
}

/// The boundary seam: the parent opens a nursery, spawns the answering
/// child, and joins at the boundary — the child's completed answer is
/// discarded, and the discard decs its `Ref` word, so both heaps drain.
#[test]
fn a_boundary_join_drains_the_discarded_completed_answer() {
    let parent = def(
        "parent",
        &[
            SlotType::Raw,
            SlotType::Raw,
            SlotType::Raw,
            SlotType::Raw,
            SlotType::Raw,
        ],
        0,
        2,
        &[
            Uop::Funcf { frame_slots: 5 }.into(),
            Uop::Call {
                callee: k(0),
                nargs: 0,
            }
            .into(),
            Uop::Mov { dst: 0, src: 3 }.into(),
            Uop::Mov { dst: 3, src: 0 }.into(),
            Uop::Kconst { dst: 4, kidx: k(3) }.into(),
            Uop::Call {
                callee: k(1),
                nargs: 2,
            }
            .into(),
            Uop::Mov { dst: 3, src: 0 }.into(),
            Uop::Kimm { dst: 4, kidx: k(4) }.into(),
            Uop::Call {
                callee: k(2),
                nargs: 2,
            }
            .into(),
            // the clean join answers the collected list at overlay word 1
            // (slot 4): the hand parent takes the channel's discard
            // discipline — one dec of the delivered Ref, the lowered
            // statement's own shape — before answering
            Inst::Rc(RcOp::Dec { src: 4 }).into(),
            Uop::Kimm { dst: 4, kidx: k(4) }.into(),
            Uop::Ret { src: 4 }.into(),
        ],
    );
    let mut parent = parent;
    // [0] open, [1] spawn, [2] boundary, [3] the answering child, [4] the
    // containment mask (no error members)
    parent.pool = vec![
        builtin(BUILTIN_NURSERY_OPEN),
        builtin(BUILTIN_NURSERY_SPAWN),
        builtin(BUILTIN_NURSERY_BOUNDARY),
        Const::FuncRef(ConstFuncRef::Named {
            module: "nsd".into(),
            func: 1,
        }),
        Const::I64(0),
    ];

    let version = ModuleVersion::new("nsd", vec![parent, answer_child()]).expect("valid module");
    let mut vm = registry(version);
    assert_eq!(
        vm.run("nsd", "parent", &[]),
        Ok(FrameAnswer::one(Value::I64(0))),
        "the boundary joins clean and the parent lands"
    );
    assert_eq!(
        vm.captured_output(),
        b"",
        "the discarded answer stays unobserved"
    );
    assert!(
        vm.objects_at(FIBER_ROOT).is_empty(),
        "the parent's heap drains"
    );
    assert!(
        vm.objects_at(FiberId(1)).is_empty(),
        "the discarded completed answer's Ref decs at the seam — the child heap drains"
    );
}

/// The debug count seam: the same answering child joined through
/// `nursery_join` — the failed count answers zero and the discarded
/// answer's count still dies at the join.
#[test]
fn a_count_join_drains_the_discarded_completed_answer() {
    let parent = def(
        "parent",
        &[SlotType::I64, SlotType::I64, SlotType::Raw, SlotType::Raw],
        0,
        2,
        &[
            Uop::Funcf { frame_slots: 4 }.into(),
            Uop::Call {
                callee: k(0),
                nargs: 0,
            }
            .into(),
            Uop::Mov { dst: 0, src: 2 }.into(),
            Uop::Mov { dst: 2, src: 0 }.into(),
            Uop::Kconst { dst: 3, kidx: k(3) }.into(),
            Uop::Call {
                callee: k(1),
                nargs: 2,
            }
            .into(),
            Uop::Mov { dst: 2, src: 0 }.into(),
            Uop::Call {
                callee: k(2),
                nargs: 1,
            }
            .into(),
            Uop::Mov { dst: 1, src: 2 }.into(),
            Uop::Ret { src: 1 }.into(),
        ],
    );
    let mut parent = parent;
    // [0] open, [1] spawn, [2] join, [3] the answering child
    parent.pool = vec![
        builtin(BUILTIN_NURSERY_OPEN),
        builtin(BUILTIN_NURSERY_SPAWN),
        builtin(BUILTIN_NURSERY_JOIN),
        Const::FuncRef(ConstFuncRef::Named {
            module: "nsd".into(),
            func: 1,
        }),
    ];

    let version = ModuleVersion::new("nsd", vec![parent, answer_child()]).expect("valid module");
    let mut vm = registry(version);
    assert_eq!(
        vm.run("nsd", "parent", &[]),
        Ok(FrameAnswer::one(Value::I64(0))),
        "the count join answers zero failed children"
    );
    assert_eq!(
        vm.captured_output(),
        b"",
        "the discarded answer stays unobserved"
    );
    assert!(
        vm.objects_at(FIBER_ROOT).is_empty(),
        "the parent's heap drains"
    );
    assert!(
        vm.objects_at(FiberId(1)).is_empty(),
        "the discarded completed answer's Ref decs at the seam — the child heap drains"
    );
}
