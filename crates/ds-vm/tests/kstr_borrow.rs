//! KSTR_BORROW (stage three's temporary slice): the claim-free string
//! load the placement pass emits for designated temporaries. The VM arm
//! borrows the intern map's pinned canonical copy — no store count, no
//! claim ledger row, no terminal dec — so the pins here are: the borrow
//! load serves the same object an acquiring KSTR loads, the borrowed
//! word outlives an acquiring claimant's dec-to-zero churn, and the
//! store still drains at run end (the completed root releases the pins
//! with its death, so the borrowed words cost nothing that outlives
//! the frame that hosted them).

use ds_vm::{
    Const, ConstKind, FunctionDef, ModuleBuildError, ModuleRegistry, ModuleVersion, Value, Vm,
    abi::{ConstIdx, FrameLayout, Inst, RcOp, SlotType, uop::Uop},
    assemble,
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

/// The fixture: `main` borrows the constant once, acquires the same
/// content, proves the two words name one object (EQSTR true), then decs
/// the acquiring copy — the pin keeps the object serving — and reads the
/// borrowed word again through a second borrow load of another constant.
fn registry() -> ModuleRegistry {
    let mut main = def(
        "main",
        &[
            SlotType::Ref,
            SlotType::Ref,
            SlotType::Bool,
            SlotType::Ref,
            SlotType::Bool,
        ],
        0,
        0,
        &[
            Uop::Funcf { frame_slots: 5 }.into(),
            // the borrow load: no count, no claim
            Uop::KstrBorrow { dst: 0, kidx: k(0) }.into(),
            // the acquiring load of the same content: one count, one claim
            Uop::Kstr { dst: 1, kidx: k(0) }.into(),
            // both words name the one pinned object
            Uop::EqStr {
                dst: 2,
                lhs: 0,
                rhs: 1,
            }
            .into(),
            // the acquiring copy's claimant decs to what the pin holds
            RcOp::Dec { src: 1 }.into(),
            // the borrowed word still serves across the churn, and a
            // second constant borrows cleanly after it
            Uop::KstrBorrow { dst: 3, kidx: k(1) }.into(),
            Uop::EqStr {
                dst: 4,
                lhs: 0,
                rhs: 3,
            }
            .into(),
            Uop::Ret { src: 2 }.into(),
        ],
    );
    main.pool = vec![Const::Str("pinned".into()), Const::Str("other".into())];
    let mut reg = ModuleRegistry::new();
    reg.install(ModuleVersion::new("kstr_borrow", vec![main]).expect("valid fixture module"));
    reg
}

#[test]
fn a_borrow_load_serves_the_pinned_object_across_claimant_churn() {
    let mut vm = Vm::new(registry());
    let got = vm.run("kstr_borrow", "main", &[]);
    assert_eq!(
        got,
        Ok(Value::Bool(true)),
        "the borrow and acquire load one object"
    );
    assert!(
        vm.objects().is_empty(),
        "the borrowed words held no count, and the completed root released the pin: {} live",
        vm.objects().len()
    );
}

/// A borrow load against a pool entry that is not a string is refused
/// at module validation with the same kind mismatch an acquiring load
/// raises — the loader's check covers both spellings.
#[test]
fn a_borrow_load_validates_the_pool_kind() {
    let mut main = def("main", &[SlotType::I64, SlotType::I64], 0, 0, &[
        Uop::Funcf { frame_slots: 2 }.into(),
        Uop::KstrBorrow { dst: 0, kidx: k(0) }.into(),
        Uop::Kimm { dst: 1, kidx: k(1) }.into(),
        Uop::Ret { src: 1 }.into(),
    ]);
    main.pool = vec![Const::I64(0), Const::I64(0)];
    match ModuleVersion::new("bad_pool", vec![main]) {
        Err(ModuleBuildError::PoolKind {
            kidx: 0,
            expected: ConstKind::Str,
            found: ConstKind::I64,
            ..
        }) => {},
        other => panic!("the borrow load refuses a non-string pool entry at validation: {other:?}"),
    }
}
