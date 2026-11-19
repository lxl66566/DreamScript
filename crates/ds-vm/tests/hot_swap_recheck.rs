//! The slot swap's staged-word re-check [H6 item 5]: a scalar callable
//! word keys on the function-table index and resolution reads the name's
//! current slot, so a swap whose incoming table moves or drops the
//! indexed function must re-key the staged word (the transparent path)
//! or refuse the swap (a dropped identity), never silently misroute. A
//! moved index-embedded name (a synthesized lambda, renamed by the
//! reorder itself) re-keys exactly when its content identity survives
//! the move — the body plus the capture set hashing equal against
//! exactly one incoming candidate — while a body edit riding the move,
//! a capture-set difference, or an ambiguous content match keeps the
//! structured refusal.

use ds_vm::{
    Const, ConstFuncRef, FrameAnswer, FuncRef, FunctionDef, LoadError, ModuleRegistry,
    ModuleVersion, RefId, StagedSite, Value, Vm,
    abi::{
        ConstIdx, FrameLayout, SlotType,
        SlotType::{Bool, Fn as FnSlot, I64},
        uop::Uop,
    },
    assemble,
};

fn k(idx: u16) -> ConstIdx {
    ConstIdx(idx)
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

/// `scaled(n) = n * scale`, the function a staged word names.
fn scaled(scale: i64) -> FunctionDef {
    def("scaled", &[I64, I64], 1, 0, vec![Const::I64(scale)], &[
        Uop::Funcf { frame_slots: 2 },
        Uop::Kimm { dst: 1, kidx: k(0) },
        Uop::MulI64 {
            dst: 0,
            lhs: 0,
            rhs: 1,
        },
        Uop::Ret { src: 0 },
    ])
}

/// `pad(n) = 999`, the function a misrouted staged word would land in:
/// same layout as `scaled`, a different name — so the fixture's reorders
/// are pure table-order moves, not identity changes.
fn pad() -> FunctionDef {
    def("pad", &[I64, I64], 1, 0, vec![Const::I64(999)], &[
        Uop::Funcf { frame_slots: 2 },
        Uop::Kimm { dst: 1, kidx: k(0) },
        Uop::Ret { src: 1 },
    ])
}

/// The `app` module's entry (never swapped): stages `lib.scaled`'s Fn
/// word in a slot, parks at its one-shot backward branch under the
/// host's suspension request, then CALLS through the staged word — so
/// the word sits in the parked frame's segment across the swap and its
/// post-swap call witnesses the re-key.
fn staged_app() -> ModuleVersion {
    ModuleVersion::new("app", vec![def(
        "staged",
        &[I64, FnSlot, I64, I64, Bool, I64],
        1,
        1,
        vec![
            Const::I64(1), // k0 one
            Const::I64(2), // k1 two
            Const::I64(0), // k2 zero
            Const::FuncRef(ConstFuncRef::Named {
                module: "lib".into(),
                func: 0,
            }),
        ],
        &[
            Uop::Funcf { frame_slots: 6 },
            Uop::Kconst { dst: 1, kidx: k(3) }, // staged = Fn(lib, 0)
            Uop::Kimm { dst: 3, kidx: k(2) },   // zero the counter
            Uop::Kimm { dst: 2, kidx: k(0) },   // one
            Uop::AddI64 {
                dst: 3,
                lhs: 3,
                rhs: 2,
            },
            Uop::Kimm { dst: 2, kidx: k(1) }, // two
            Uop::LtI64 {
                dst: 4,
                lhs: 3,
                rhs: 2,
            },
            Uop::Jmpif { cond: 4, target: 4 }, // the one-shot park point
            Uop::Mov { dst: 5, src: 0 },       // restage n for the calls
            Uop::Calls {
                callee_slot: 1,
                nargs: 1,
            },
            Uop::Mov { dst: 3, src: 5 },
            Uop::Ret { src: 3 },
        ],
    )])
    .expect("valid app module")
}

/// Unwraps a host-allocated closure word to its `RefId`.
fn ref_word(word: Value) -> RefId {
    let Value::Ref(id) = word else {
        panic!("alloc_closure answers a Ref word");
    };
    id
}

fn lib_vm(functions: Vec<FunctionDef>) -> Vm {
    let mut reg = ModuleRegistry::new();
    reg.install(ModuleVersion::new("lib", functions).expect("valid lib module"));
    reg.install(staged_app());
    Vm::new(reg)
}

/// A reordering swap re-keys the staged word: the parked frame's slot
/// word keys `lib.scaled`'s old index, the incoming table moves `scaled`
/// behind a fresh `pad`, and the resumed CALLS reaches the moved
/// `scaled` — not the `pad` that now holds the old index.
#[test]
fn a_reordering_swap_rekeys_the_parked_frames_staged_word() {
    let mut vm = lib_vm(vec![scaled(2)]);
    let fiber = vm
        .spawn_fiber("app", "staged", &[Value::I64(21)])
        .expect("the entry resolves");
    vm.request_suspend(fiber);
    vm.resume_fiber(fiber, Value::I64(0))
        .expect("the drive parks at its requested suspension");
    // scaled moves from index 0 to 1: index 0 now names pad
    vm.install_module(
        ModuleVersion::new("lib", vec![pad(), scaled(3)]).expect("valid reorder module"),
    )
    .expect("the reordering swap re-keys and installs");
    assert_eq!(
        vm.resume_fiber(fiber, Value::I64(0)),
        Ok(FrameAnswer::one(Value::I64(63))),
        "the staged word calls the moved scaled (21 * 3), never pad's 999"
    );
}

/// A heap-staged word re-keys too: a closure's header word keyed to the
/// old index survives the same reordering swap with the moved index.
#[test]
fn a_reordering_swap_rekeys_a_closure_header() {
    let mut vm = lib_vm(vec![scaled(2)]);
    let lib_id = vm.registry().module_id("lib").expect("installed");
    let word = ref_word(vm.alloc_closure(FuncRef::new(lib_id, 0), Vec::new()));
    vm.install_module(
        ModuleVersion::new("lib", vec![pad(), scaled(3)]).expect("valid reorder module"),
    )
    .expect("the reordering swap re-keys and installs");
    let (header, _) = vm
        .objects()
        .closure_at(word.id())
        .expect("the closure object lives");
    assert_eq!(
        header,
        FuncRef::new(lib_id, 1),
        "the header word carries scaled's moved index"
    );
}

/// A swap that drops the staged word's function refuses, naming the
/// staged value and the vanished function, and keeps the installed
/// version running: a refused swap over a silent misroute.
#[test]
fn a_dropped_function_refuses_the_swap_and_keeps_the_installed_version() {
    let mut vm = lib_vm(vec![scaled(2)]);
    let lib_id = vm.registry().module_id("lib").expect("installed");
    let word = ref_word(vm.alloc_closure(FuncRef::new(lib_id, 0), Vec::new()));
    let refused = vm
        .install_module(ModuleVersion::new("lib", vec![pad()]).expect("valid dropping module"))
        .expect_err("the dropped identity refuses the swap");
    assert_eq!(refused, LoadError::StagedFnMisroute {
        module: "lib".into(),
        index: 0,
        func: Some("scaled".into()),
        site: StagedSite::ClosureHeader { fiber: 0 },
    });
    // the installed version keeps running: the name still resolves, and
    // the staged header keeps its index against the table still installed
    let (version, func) = vm
        .registry()
        .resolve("lib", "scaled")
        .expect("the installed version stays current");
    let row = vm
        .install_module(ModuleVersion::new("lib", vec![scaled(5)]).expect("valid module"))
        .expect("a later same-shape swap still installs");
    assert_ne!(row, version, "the refusal left the old row current");
    assert_eq!(func, 0, "the refused swap never moved the table");
    let (header, _) = vm
        .objects()
        .closure_at(word.id())
        .expect("the closure object lives");
    assert_eq!(
        header,
        FuncRef::new(lib_id, 0),
        "the refused swap re-keyed nothing"
    );
}

/// A staged word whose index already escaped the retiring table has an
/// unknowable identity — no re-key can be proven, so the swap refuses
/// with the index itself named.
#[test]
fn an_out_of_table_staged_word_refuses_the_swap() {
    let mut vm = lib_vm(vec![scaled(2)]);
    let lib_id = vm.registry().module_id("lib").expect("installed");
    let _ = vm.alloc_closure(FuncRef::new(lib_id, 5), Vec::new());
    let refused = vm
        .install_module(
            ModuleVersion::new("lib", vec![pad(), scaled(3)]).expect("valid reorder module"),
        )
        .expect_err("the unknowable identity refuses the swap");
    assert_eq!(refused, LoadError::StagedFnMisroute {
        module: "lib".into(),
        index: 5,
        func: None,
        site: StagedSite::ClosureHeader { fiber: 0 },
    });
}

/// A signature change is an identity change: the same name with a
/// different frame layout did not survive, so a staged word naming it
/// refuses the swap rather than re-keying into an incompatible frame.
#[test]
fn a_retyped_function_refuses_the_staged_words_swap() {
    let mut vm = lib_vm(vec![scaled(2)]);
    let lib_id = vm.registry().module_id("lib").expect("installed");
    let _ = vm.alloc_closure(FuncRef::new(lib_id, 0), Vec::new());
    let retyped = def("scaled", &[I64, I64, I64], 2, 0, vec![Const::I64(2)], &[
        Uop::Funcf { frame_slots: 3 },
        Uop::Mov { dst: 2, src: 0 },
        Uop::Kimm { dst: 0, kidx: k(0) },
        Uop::MulI64 {
            dst: 1,
            lhs: 1,
            rhs: 0,
        },
        Uop::Ret { src: 1 },
    ]);
    let refused = vm
        .install_module(ModuleVersion::new("lib", vec![retyped]).expect("valid module"))
        .expect_err("the changed identity refuses the swap");
    assert_eq!(refused, LoadError::StagedFnMisroute {
        module: "lib".into(),
        index: 0,
        func: Some("scaled".into()),
        site: StagedSite::ClosureHeader { fiber: 0 },
    });
}

/// A synthesized-lambda fixture (`lib$0`, the value-position spelling
/// whose name embeds its table index): `lam(n) = n * scale`, with the
/// capture variant binding one extra parameter word — the incoming
/// capture slot the reordered spelling would stage — over an otherwise
/// identical body, so the two differ in nothing but the capture set.
fn lam(name: &str, scale: i64, capture: bool) -> FunctionDef {
    let params = usize::from(capture) + 1;
    def(name, &[I64, I64], params, 0, vec![Const::I64(scale)], &[
        Uop::Funcf { frame_slots: 2 },
        Uop::Kimm { dst: 1, kidx: k(0) },
        Uop::MulI64 {
            dst: 0,
            lhs: 0,
            rhs: 1,
        },
        Uop::Ret { src: 0 },
    ])
}

/// A reorder that preserves the lambda's content re-keys: the staged
/// word keys `lib$0` (the index-embedded name), the incoming table moves
/// it behind a fresh `pad` as `lib$1` with an identical body and capture
/// set, and the content identity survived the move — so the swap
/// installs, the staged header re-keys onto the moved index, and the
/// resumed CALLS reaches the moved body, never the `pad` holding the old
/// index.
#[test]
fn a_content_surviving_move_rekeys_a_moved_staged_lambda() {
    let mut vm = lib_vm(vec![lam("lib$0", 2, false)]);
    let fiber = vm
        .spawn_fiber("app", "staged", &[Value::I64(21)])
        .expect("the entry resolves");
    vm.request_suspend(fiber);
    vm.resume_fiber(fiber, Value::I64(0))
        .expect("the drive parks at its requested suspension");
    // lib$0 moves from index 0 to 1 behind pad, its content unchanged
    vm.install_module(
        ModuleVersion::new("lib", vec![pad(), lam("lib$1", 2, false)])
            .expect("valid reorder module"),
    )
    .expect("the content-surviving move re-keys and installs");
    assert_eq!(
        vm.resume_fiber(fiber, Value::I64(0)),
        Ok(FrameAnswer::one(Value::I64(42))),
        "the parked frame's staged word calls the moved body (21 * 2), never pad's 999"
    );
}

/// The heap-staged spelling re-keys on the same content judge: a closure
/// header keyed to the moved `lib$0` survives the reorder carrying the
/// moved `lib$1`'s index.
#[test]
fn a_content_surviving_move_rekeys_a_closure_header() {
    let mut vm = lib_vm(vec![lam("lib$0", 2, false)]);
    let lib_id = vm.registry().module_id("lib").expect("installed");
    let word = ref_word(vm.alloc_closure(FuncRef::new(lib_id, 0), Vec::new()));
    vm.install_module(
        ModuleVersion::new("lib", vec![pad(), lam("lib$1", 2, false)])
            .expect("valid reorder module"),
    )
    .expect("the content-surviving move re-keys and installs");
    let (header, _) = vm
        .objects()
        .closure_at(word.id())
        .expect("the closure object lives");
    assert_eq!(
        header,
        FuncRef::new(lib_id, 1),
        "the header word carries the moved lambda's new index"
    );
}

/// A body edit riding the move refuses: the incoming table moves the
/// lambda AND changes its body, so neither the name nor the content
/// identity survived — the structured refusal names the staged value and
/// the vanished spelling, and the installed version keeps running.
#[test]
fn a_body_edit_riding_the_move_refuses_the_staged_words_swap() {
    let mut vm = lib_vm(vec![lam("lib$0", 2, false)]);
    let lib_id = vm.registry().module_id("lib").expect("installed");
    let word = ref_word(vm.alloc_closure(FuncRef::new(lib_id, 0), Vec::new()));
    let refused = vm
        .install_module(
            ModuleVersion::new("lib", vec![pad(), lam("lib$1", 3, false)])
                .expect("valid reorder module"),
        )
        .expect_err("the moved-and-edited identity refuses the swap");
    assert_eq!(refused, LoadError::StagedFnMisroute {
        module: "lib".into(),
        index: 0,
        func: Some("lib$0".into()),
        site: StagedSite::ClosureHeader { fiber: 0 },
    });
    let (header, _) = vm
        .objects()
        .closure_at(word.id())
        .expect("the closure object lives");
    assert_eq!(
        header,
        FuncRef::new(lib_id, 0),
        "the refused swap re-keyed nothing"
    );
}

/// A capture-set difference refuses: the incoming lambda moved with an
/// identical body and pool but one capture word more (its parameter
/// count grew), so the content identity — body plus capture set — did
/// not survive the move.
#[test]
fn a_capture_set_difference_refuses_the_staged_words_swap() {
    let mut vm = lib_vm(vec![lam("lib$0", 2, false)]);
    let lib_id = vm.registry().module_id("lib").expect("installed");
    let _ = vm.alloc_closure(FuncRef::new(lib_id, 0), Vec::new());
    let refused = vm
        .install_module(
            ModuleVersion::new("lib", vec![lam("lib$1", 2, true)]).expect("valid reorder module"),
        )
        .expect_err("the capture-set difference refuses the swap");
    assert_eq!(refused, LoadError::StagedFnMisroute {
        module: "lib".into(),
        index: 0,
        func: Some("lib$0".into()),
        site: StagedSite::ClosureHeader { fiber: 0 },
    });
}

/// Two identical-body identical-capture lambdas in the incoming table
/// refuse as ambiguous: the moved staged lambda's content matches both
/// candidates, and the re-key never guesses.
#[test]
fn an_ambiguous_content_match_refuses_the_staged_words_swap() {
    let mut vm = lib_vm(vec![lam("lib$0", 2, false)]);
    let lib_id = vm.registry().module_id("lib").expect("installed");
    let _ = vm.alloc_closure(FuncRef::new(lib_id, 0), Vec::new());
    let refused = vm
        .install_module(
            ModuleVersion::new("lib", vec![lam("lib$1", 2, false), lam("lib$2", 2, false)])
                .expect("valid reorder module"),
        )
        .expect_err("the ambiguous content match refuses the swap");
    assert_eq!(refused, LoadError::StagedFnMisroute {
        module: "lib".into(),
        index: 0,
        func: Some("lib$0".into()),
        site: StagedSite::ClosureHeader { fiber: 0 },
    });
}
