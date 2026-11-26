//! The REPL entry protocol's state refusals: each entry needs the root
//! fiber in its own state, and a wrong state answers the dedicated trap
//! naming the entry — never an unknown-callee refusal over a virtual
//! module.

use ds_mod::{Constant, FuncRefKey, Function, Hash128, Module, TargetClass};
use ds_vm::{
    ModuleRegistry, ReplEntry, ReplOutcome, Trap, Vm,
    abi::{CodePc, ConstIdx, FrameLayout, SlotType, uop::Uop},
    native,
    native::BUILTIN_REPL_PARK,
    platform::Platform,
    splice_park,
};

/// A session module whose `main` parks at the seam at once: the minimum
/// drive that leaves the root parked.
fn parking_session() -> Module {
    let main = Function::new(
        "main".to_owned(),
        FrameLayout::new(vec![SlotType::Raw, SlotType::Raw], 0, 1).expect("valid layout"),
        vec![Constant::FuncRef {
            key: FuncRefKey::Native(native::BUILTIN_MODULE.to_owned()),
            func: BUILTIN_REPL_PARK,
        }],
        vec![
            Uop::Funcf { frame_slots: 2 },
            Uop::Call {
                callee: ConstIdx(0),
                nargs: 0,
            },
            Uop::Ret { src: 0 },
        ],
    )
    .expect("valid entry");
    let module = Module::new(
        "rline",
        TargetClass::Bytecode,
        Hash128::compute(b"rline-entry-refusals"),
    );
    splice_park(module.with_function(main), "main", 1).expect("the splice lands")
}

fn vm() -> Vm {
    let mut reg = ModuleRegistry::new();
    reg.install_native(native::BUILTIN_MODULE, native::builtin_module());
    reg.install_platform(Platform::std());
    Vm::new(reg)
}

#[test]
fn repl_entries_refuse_on_the_wrong_session_state() {
    let mut vm = vm();
    vm.install_dsmod(parking_session())
        .expect("the session installs");
    // an empty root: resume and drop answer their own refusal, the
    // start entry being that state's entry
    assert_eq!(
        vm.repl_resume("rline", "main", CodePc(1)),
        Err(Trap::ReplState {
            entry: ReplEntry::Resume
        })
    );
    assert_eq!(
        vm.repl_drop_binding("rline", "main", &[]),
        Err(Trap::ReplState {
            entry: ReplEntry::DropBinding
        })
    );
    // the first submission parks at the seam
    assert!(matches!(
        vm.repl_start("rline", "main"),
        Ok(ReplOutcome::Parked(_))
    ));
    // a live session never starts over its parked root
    assert_eq!(
        vm.repl_start("rline", "main"),
        Err(Trap::ReplState {
            entry: ReplEntry::Start
        })
    );
}
