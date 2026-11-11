//! CALLS fixtures (the callable-word call form, F2): the callee operand is
//! a frame slot — a `Fn` slot holds the copyable scalar FuncRef word
//! (module id + function index, the registry's id space), a `Ref` slot
//! holds a closure object whose header word is the FuncRef and whose
//! capture block follows inside the object. Resolution goes through the
//! two-slot module table at call time, so a staged word picks up a
//! hot-reload swap; a closure's captures are copied into the callee's
//! frame starting at the first local slot. No tail variant exists, so
//! deep CALLS recursion is bounded by the stack limit like every
//! non-tail call.

use ds_vm::{
    Const, FuncRef, FunctionDef, ModuleRegistry, ModuleVersion, Trap, Value, Vm,
    abi::{
        ConstIdx, FrameLayout, SlotType,
        SlotType::{Fn as FnSlot, I64, Ref},
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

/// The `lib` module's one function, `scaled(n) = n * scale`: installed
/// twice with different pools, it is the hot-reload body the staged fn
/// word must pick up at its next call.
fn scaled_lib(scale: i64) -> ModuleVersion {
    ModuleVersion::new("lib", vec![def(
        "scaled",
        &[I64, I64],
        1,
        0,
        vec![Const::I64(scale)],
        &[
            Uop::Funcf { frame_slots: 2 },
            Uop::Kimm { dst: 1, kidx: k(0) },
            Uop::MulI64 {
                dst: 0,
                lhs: 0,
                rhs: 1,
            },
            Uop::Ret { src: 0 },
        ],
    )])
    .expect("valid lib module")
}

/// The `app` module's entry: receives a Fn word, copies it to a Fn local
/// with MOV (the plain scalar copy), and CALLS through the copy.
fn apply_module() -> ModuleVersion {
    ModuleVersion::new("app", vec![def(
        "apply",
        &[FnSlot, FnSlot, I64],
        1,
        1,
        vec![Const::I64(21)],
        &[
            Uop::Funcf { frame_slots: 3 },
            Uop::Mov { dst: 1, src: 0 }, // the Fn word copies like any scalar
            Uop::Kimm { dst: 2, kidx: k(0) }, // stage 21 in the arg area
            Uop::Calls {
                callee_slot: 1,
                nargs: 1,
            },
            Uop::Ret { src: 2 },
        ],
    )])
    .expect("valid app module")
}

/// A cross-module CALLS through a MOV-copied FuncRef word, then the SAME
/// staged word against a hot-reload swap of the target module: the word
/// never caches a code pointer, so the second run reaches the new body.
#[test]
fn cross_module_calls_word_picks_up_hot_reload_swaps() {
    let mut reg = ModuleRegistry::new();
    reg.install(scaled_lib(2));
    reg.install(apply_module());
    let mut vm = Vm::new(reg);
    let word = Value::Fn(FuncRef::new(
        vm.registry().module_id("lib").expect("installed"),
        0,
    ));
    assert_eq!(vm.run("app", "apply", &[word]), Ok(Value::I64(42)));
    vm.install_module(scaled_lib(3));
    assert_eq!(
        vm.run("app", "apply", &[word]),
        Ok(Value::I64(63)),
        "the staged word resolves through the module's current slot at call time"
    );
}

/// `f(self_word, n) = if n == 0 { 0 } else { 1 + f(n - 1) }`, recursing
/// through its own Fn word: slots [self, n, t, one, staged-word,
/// staged-n] over a Raw overlay (it stages a Fn word and an i64, then
/// receives the i64 result on word 0), pool k0=0, k1=1.
fn recursion_module() -> ModuleRegistry {
    let mut reg = ModuleRegistry::new();
    reg.install(
        ModuleVersion::new("rec", vec![def(
            "f",
            &[
                FnSlot,
                I64,
                SlotType::Bool,
                I64,
                SlotType::Raw,
                SlotType::Raw,
            ],
            2,
            2,
            vec![Const::I64(0), Const::I64(1)],
            &[
                Uop::Funcf { frame_slots: 6 },
                Uop::Kimm { dst: 3, kidx: k(0) }, // one = 0 (the base-path combine word)
                Uop::Kimm { dst: 4, kidx: k(0) }, // the base-path result word = 0
                Uop::Kimm { dst: 5, kidx: k(0) }, // stage 0, the == probe
                Uop::EqI64 {
                    dst: 2,
                    lhs: 1,
                    rhs: 5,
                }, // t = n == 0
                Uop::Jmpif {
                    cond: 2,
                    target: 10,
                }, // base: skip the recursion
                Uop::Kimm { dst: 3, kidx: k(1) }, // one = 1
                Uop::Mov { dst: 4, src: 0 },      // stage the fn word
                Uop::SubI64 {
                    dst: 5,
                    lhs: 1,
                    rhs: 3,
                }, // stage n - 1
                Uop::Calls {
                    callee_slot: 0,
                    nargs: 2,
                }, // result overlays arg word 0
                Uop::AddI64 {
                    dst: 4,
                    lhs: 4,
                    rhs: 3,
                }, // result + one
                Uop::Ret { src: 4 },
            ],
        )])
        .expect("valid recursion module"),
    );
    reg
}

fn recursion_word(vm: &Vm) -> Value {
    Value::Fn(FuncRef::new(
        vm.registry().module_id("rec").expect("installed"),
        0,
    ))
}

#[test]
fn calls_recursion_through_a_fn_word_counts() {
    let mut vm = Vm::new(recursion_module());
    let word = recursion_word(&vm);
    assert_eq!(
        vm.run("rec", "f", &[word, Value::I64(3)]),
        Ok(Value::I64(3))
    );
}

/// CALLS has no tail variant: every recursion is an ordinary frame push,
/// so deep CALLS recursion is bounded by the per-entry stack-limit
/// compare [H1] exactly like CALL — the trap, not a segfault. Frames are
/// 8 words netting 5 (the callee's params overlay the caller's arg area):
/// a 64-word limit admits 12 frames, and the 13th needs 68.
#[test]
fn deep_calls_recursion_traps_at_the_stack_limit() {
    let mut vm = Vm::with_limit(recursion_module(), 64);
    let word = recursion_word(&vm);
    let trap = vm
        .run("rec", "f", &[word, Value::I64(1000)])
        .expect_err("must overflow");
    assert_eq!(trap, Trap::StackOverflow {
        needed: 68,
        limit: 64
    });
    assert_eq!(vm.backtrace().len(), 12);
    assert_eq!(vm.max_stack_len(), 63);
}

/// A closure-object call: the Ref slot holds one object whose header word
/// names the body and whose capture block is copied into the callee's
/// frame starting at the first local slot. The body reads its captures
/// there: `body(n) = n + c0 * c1` over captures [2, 5].
#[test]
fn a_closure_call_receives_its_captures() {
    let mut reg = ModuleRegistry::new();
    reg.install(
        ModuleVersion::new("cl", vec![
            def("body", &[I64, I64, I64, I64], 1, 0, Vec::new(), &[
                Uop::Funcf { frame_slots: 4 },
                Uop::MulI64 {
                    dst: 3,
                    lhs: 1,
                    rhs: 2,
                }, // c0 * c1
                Uop::AddI64 {
                    dst: 3,
                    lhs: 3,
                    rhs: 0,
                }, // + n
                Uop::Ret { src: 3 },
            ]),
            def("main", &[Ref, I64], 1, 1, vec![Const::I64(1)], &[
                Uop::Funcf { frame_slots: 2 },
                Uop::Kimm { dst: 1, kidx: k(0) }, // stage 1 in the arg area
                Uop::Calls {
                    callee_slot: 0,
                    nargs: 1,
                },
                Uop::Ret { src: 1 },
            ]),
        ])
        .expect("valid closure module"),
    );
    let mut vm = Vm::new(reg);
    let closure = vm.alloc_closure(
        FuncRef::new(vm.registry().module_id("cl").expect("installed"), 0),
        vec![Value::I64(2), Value::I64(5)],
    );
    assert_eq!(vm.run("cl", "main", &[closure]), Ok(Value::I64(11)));
}

/// A closure word in a Fn-classified slot (the callable-defaulted
/// parameter, F2): CALLS resolves the callee word by its runtime kind —
/// the closure's header FuncRef and capture block behind the Ref word —
/// exactly like the scalar word in the same slot classification.
#[test]
fn a_closure_word_in_a_fn_slot_calls_through_calls() {
    let mut reg = ModuleRegistry::new();
    reg.install(
        ModuleVersion::new("cd", vec![
            def("body", &[I64, I64, I64], 1, 0, Vec::new(), &[
                Uop::Funcf { frame_slots: 3 },
                Uop::AddI64 {
                    dst: 2,
                    lhs: 0,
                    rhs: 1,
                }, // n + c0
                Uop::Ret { src: 2 },
            ]),
            def("main", &[FnSlot, I64], 1, 1, vec![Const::I64(1)], &[
                Uop::Funcf { frame_slots: 2 },
                Uop::Kimm { dst: 1, kidx: k(0) }, // stage 1 in the arg area
                Uop::Calls {
                    callee_slot: 0,
                    nargs: 1,
                },
                Uop::Ret { src: 1 },
            ]),
        ])
        .expect("valid callable-default module"),
    );
    let mut vm = Vm::new(reg);
    // the closure word rides the Fn-classified parameter slot; the scalar
    // word in the same classification is the R11 CALLS shape (the
    // lambda-value probes pin it)
    let closure = vm.alloc_closure(
        FuncRef::new(vm.registry().module_id("cd").expect("installed"), 0),
        vec![Value::I64(41)],
    );
    assert_eq!(
        vm.run("cd", "main", &[closure]),
        Ok(Value::I64(42)),
        "the Fn slot reads the closure word by its kind: header FuncRef plus captures"
    );
}

/// The `bogus` module: `call_fn` CALLS a Fn slot, `call_ref` CALLS a Ref
/// slot, `tiny` is a zero-parameter two-slot body for the capture-range
/// check (the CALLS sites stage no arguments).
fn bogus_module() -> ModuleRegistry {
    let calls = |slot_ty: SlotType| {
        def(
            if slot_ty == FnSlot { "call_fn" } else { "call_ref" },
            &[slot_ty],
            1,
            0,
            Vec::new(),
            &[
                Uop::Funcf { frame_slots: 1 },
                Uop::Calls {
                    callee_slot: 0,
                    nargs: 0,
                },
                Uop::Halt { slot: 0 },
            ],
        )
    };
    let mut reg = ModuleRegistry::new();
    reg.install(
        ModuleVersion::new("bogus", vec![
            calls(FnSlot),
            calls(Ref),
            def("tiny", &[I64, I64], 0, 0, Vec::new(), &[
                Uop::Funcf { frame_slots: 2 },
                Uop::Ret { src: 0 },
            ]),
        ])
        .expect("valid bogus module"),
    );
    reg
}

/// A CALLS on a slot whose static classification is neither Fn nor Ref
/// traps through the slot-type discipline; within the two, the callee
/// word's own kind selects the read (the callable-defaulted `Fn` slot
/// admits the closure word exactly like the scalar).
#[test]
fn calls_on_a_non_callable_slot_traps() {
    let mut reg = bogus_module();
    reg.install(
        ModuleVersion::new("bogus", vec![def("call_i64", &[I64], 1, 0, Vec::new(), &[
            Uop::Funcf { frame_slots: 1 },
            Uop::Calls {
                callee_slot: 0,
                nargs: 0,
            },
            Uop::Halt { slot: 0 },
        ])])
        .expect("valid module"),
    );
    let mut vm = Vm::new(reg);
    assert_eq!(
        vm.run("bogus", "call_i64", &[Value::I64(0)]),
        Err(Trap::SlotTypeMismatch {
            slot: 0,
            expected: FnSlot,
            actual: I64,
        })
    );
}

/// Garbage callable words trap with honest diagnostics: an unassigned
/// module id, a function index past the module's table, a Ref slot
/// holding a non-closure object, and a closure whose capture block does
/// not fit the callee's frame.
#[test]
fn garbage_callable_words_trap_honestly() {
    let mut vm = Vm::new(bogus_module());
    let module = vm.registry().module_id("bogus").expect("installed");
    // the module id names no installed module
    assert_eq!(
        vm.run("bogus", "call_fn", &[Value::Fn(FuncRef::new(99, 0))]),
        Err(Trap::UnknownModule { module: 99 })
    );
    // the id names the module, the index names no function in its table
    assert_eq!(
        vm.run("bogus", "call_fn", &[Value::Fn(FuncRef::new(module, 77))]),
        Err(Trap::UnknownCallee {
            module: "bogus".into(),
            func: 77,
        })
    );
    // a Ref slot holding a string, not a closure object
    let id = match vm.intern_str("s") {
        Value::Ref(id) => id,
        other => panic!("an interned string is a Ref word, got {other:?}"),
    };
    assert_eq!(
        vm.run("bogus", "call_ref", &[Value::Ref(id)]),
        Err(Trap::UnknownObject { obj: id })
    );
    // a closure whose capture block is wider than the callee's body
    let closure = vm.alloc_closure(
        FuncRef::new(module, 2), // tiny: two body slots
        vec![Value::I64(0); 5],
    );
    assert_eq!(
        vm.run("bogus", "call_ref", &[closure]),
        Err(Trap::SlotOutOfRange {
            slot: 4,
            frame_slots: 2,
        })
    );
}

/// The `lib` module's constant-answering `work`, installed twice with
/// different pools: the hot-reload body a CACHED call site must pick up.
fn answer_lib(answer: i64) -> ModuleVersion {
    ModuleVersion::new("lib", vec![def(
        "work",
        &[I64],
        1,
        0,
        vec![Const::I64(answer)],
        &[
            Uop::Funcf { frame_slots: 1 },
            Uop::Kimm { dst: 0, kidx: k(0) },
            Uop::Ret { src: 0 },
        ],
    )])
    .expect("valid lib module")
}

/// An `app` entry whose body stages its argument and CALLs (or CALLTs)
/// `lib.work` through a pool FuncRef: the call site the inline cache
/// keys on.
fn caller_module(tail: bool) -> ModuleVersion {
    let call = if tail {
        Uop::Callt {
            callee: k(0),
            nargs: 1,
        }
    } else {
        Uop::Call {
            callee: k(0),
            nargs: 1,
        }
    };
    ModuleVersion::new("app", vec![def(
        "main",
        &[I64, I64],
        1,
        1,
        vec![Const::FuncRef {
            module: "lib".into(),
            func: 0,
        }],
        &[
            Uop::Funcf { frame_slots: 2 },
            Uop::Mov { dst: 1, src: 0 },
            call,
            Uop::Ret { src: 1 },
        ],
    )])
    .expect("valid app module")
}

/// A cached CALL/CALLT site must not outlive an install: the site's key
/// (caller function, pc) is untouched by a hot-reload swap of the TARGET
/// module, so only the generation stamp saves the second run from the
/// first install's callee.
#[test]
fn call_site_cache_redirects_after_a_target_swap() {
    for tail in [false, true] {
        let mut reg = ModuleRegistry::new();
        reg.install(answer_lib(7));
        reg.install(caller_module(tail));
        let mut vm = Vm::new(reg);
        assert_eq!(vm.run("app", "main", &[Value::I64(0)]), Ok(Value::I64(7)));
        vm.install_module(answer_lib(9));
        assert_eq!(
            vm.run("app", "main", &[Value::I64(0)]),
            Ok(Value::I64(9)),
            "the cached site re-resolves through the target's current slot"
        );
    }
}

/// The cache keys a site on the FUNCTION, not its module version: two
/// same-shaped functions in one module — identical bodies, call sites at
/// identical pcs, different callees — must never serve each other's
/// resolution. (A version-keyed cache cross-serves here by construction.)
#[test]
fn call_site_cache_keys_the_site_on_the_function_not_the_module() {
    fn adder(name: &str, addend: i64) -> ModuleVersion {
        ModuleVersion::new(name, vec![def(
            "work",
            &[I64, I64],
            1,
            0,
            vec![Const::I64(addend)],
            &[
                Uop::Funcf { frame_slots: 2 },
                Uop::Kimm { dst: 1, kidx: k(0) },
                Uop::AddI64 {
                    dst: 0,
                    lhs: 0,
                    rhs: 1,
                },
                Uop::Ret { src: 0 },
            ],
        )])
        .expect("valid adder module")
    }
    fn site(name: &str, callee: &str) -> FunctionDef {
        def(
            name,
            &[I64, I64],
            1,
            1,
            vec![Const::FuncRef {
                module: callee.into(),
                func: 0,
            }],
            &[
                Uop::Funcf { frame_slots: 2 },
                Uop::Mov { dst: 1, src: 0 },
                Uop::Call {
                    callee: k(0),
                    nargs: 1,
                },
                Uop::Ret { src: 1 },
            ],
        )
    }
    let mut reg = ModuleRegistry::new();
    reg.install(adder("one", 1));
    reg.install(adder("two", 2));
    reg.install(
        ModuleVersion::new("app", vec![site("f", "one"), site("g", "two")])
            .expect("valid app module"),
    );
    let mut vm = Vm::new(reg);
    assert_eq!(vm.run("app", "f", &[Value::I64(10)]), Ok(Value::I64(11)));
    assert_eq!(
        vm.run("app", "g", &[Value::I64(10)]),
        Ok(Value::I64(12)),
        "the same-pc site in the sibling function resolves its own callee"
    );
    assert_eq!(vm.run("app", "f", &[Value::I64(10)]), Ok(Value::I64(11)));
}
