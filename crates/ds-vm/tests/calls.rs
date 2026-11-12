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

use std::sync::Arc;

use ds_vm::{
    Const, FuncRef, FunctionDef, ModuleRegistry, ModuleVersion, Trap, Value, Vm,
    abi::{
        ConstIdx, FrameLayout, SlotType,
        SlotType::{Bool, Fn as FnSlot, I64, Ref},
        uop::Uop,
    },
    assemble, native,
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

/// The `app` module's entry: stages its parameter and CALLs `lib.scaled`
/// through a pool FuncRef — the direct-call-site shape the inline cache
/// serves, against the same hot-reload body `scaled_lib` swaps.
fn direct_call_module() -> ModuleVersion {
    ModuleVersion::new("app", vec![def(
        "apply",
        &[I64, I64, I64],
        1,
        1,
        vec![Const::FuncRef {
            module: "lib".into(),
            func: 0,
        }],
        &[
            Uop::Funcf { frame_slots: 3 },
            Uop::Mov { dst: 2, src: 0 }, // stage n in the arg area
            Uop::Call {
                callee: k(0),
                nargs: 1,
            },
            Uop::Ret { src: 2 },
        ],
    )])
    .expect("valid app module")
}

/// The swap-time cache eviction: a hot-reload swap unstamps every CALL
/// inline-cache entry, and the unstamped entries release their
/// swapped-out versions' handles at the swap instead of lingering until
/// eviction. After the first swap the retired version lives only in the
/// registry's retiring slot, and after the second swap nowhere but this
/// test's own handle — the strong count is the witness.
#[test]
fn swapped_out_versions_leave_the_call_cache_at_the_swap() {
    let mut reg = ModuleRegistry::new();
    let v1 = reg.install(scaled_lib(2));
    reg.install(direct_call_module());
    let mut vm = Vm::new(reg);
    assert_eq!(
        vm.run("app", "apply", &[Value::I64(21)]),
        Ok(Value::I64(42))
    );
    vm.install_module(scaled_lib(3));
    assert_eq!(
        Arc::strong_count(&v1),
        2,
        "the retiring slot plus this handle: the cache dropped its entry at the swap"
    );
    vm.install_module(scaled_lib(5));
    assert_eq!(
        Arc::strong_count(&v1),
        1,
        "the second swap retired v1 out of the registry, and nothing lingers for it"
    );
    assert_eq!(
        vm.run("app", "apply", &[Value::I64(21)]),
        Ok(Value::I64(105)),
        "the swept site resolves through the current slot (the full lookup stays authoritative)"
    );
}

/// `f(self_word, n) = if n == 0 { 0 } else { 1 + f(n - 1) }`, recursing
/// through its own Fn word: slots [self, n, t, one, staged-word,
/// staged-n] over a Raw overlay (it stages a Fn word and an i64, then
/// receives the i64 result on word 0), pool k0=0, k1=1.
fn recursion_version() -> ModuleVersion {
    ModuleVersion::new("rec", vec![def(
        "f",
        &[FnSlot, I64, Bool, I64, SlotType::Raw, SlotType::Raw],
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
    .expect("valid recursion module")
}

fn recursion_module() -> ModuleRegistry {
    let mut reg = ModuleRegistry::new();
    reg.install(recursion_version());
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

/// The RET handle take-back: a same-module return moves the caller's
/// chain-entry handle back out as the restored Active and the popped
/// callee entry's handle (the same version) takes its place, so a
/// return moves no Arc count. The strong count is the witness: a
/// recursion any depth deep, run twice (the second run clears the first
/// run's kept entry frame), ends with exactly the registry's current
/// slot, the run-entry hint, the root fiber's kept entry, and this
/// test's own handle — a leaked handle per return would move the count
/// by the recursion depth.
#[test]
fn same_module_returns_take_back_handles_without_leaking() {
    let mut reg = ModuleRegistry::new();
    let version = reg.install(recursion_version());
    let mut vm = Vm::new(reg);
    for _ in 0..2 {
        let word = recursion_word(&vm);
        assert_eq!(
            vm.run("rec", "f", &[word, Value::I64(60)]),
            Ok(Value::I64(60))
        );
    }
    assert_eq!(
        Arc::strong_count(&version),
        4,
        "current slot + run-entry hint + kept root entry + this handle, nothing per return"
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
        Err(Trap::UnknownObject { obj: id.id() })
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

// CALLTG fixtures (the dispatch tail call, H2's guarantee closure): the
// dispatch resolution is CALLG's own and the transfer is CALLT's — the
// staged words slide into the reused frame, the evidence/instance words
// riding past the callee's parameters as count-free dead words.

/// One impl-body-shaped side of the mutual dispatch tail: [n, the
/// trailing evidence parameter, t, zero, one, staged-n,
/// staged-evidence, staged-instance] with params (n, evidence) and a
/// three-word overlay. The CALLTG names the sibling (kidx 4), stages
/// n-1, the evidence word, and the instance word; nargs is 1, so the
/// slide carries the argument and the evidence parameter while the
/// instance word rides dead.
fn dispatch_body(even: bool) -> [Uop; 11] {
    [
        Uop::Funcf { frame_slots: 8 },
        Uop::Kimm { dst: 3, kidx: k(0) }, // zero
        Uop::EqI64 {
            dst: 2,
            lhs: 0,
            rhs: 3,
        }, // t = n == 0
        Uop::Jmpif { cond: 2, target: 9 }, // answer at the tail
        Uop::Kimm { dst: 4, kidx: k(1) }, // one
        Uop::SubI64 {
            dst: 5,
            lhs: 0,
            rhs: 4,
        }, // stage n - 1 (the real argument)
        Uop::Kimm { dst: 6, kidx: k(2) }, // stage the evidence word
        Uop::Kimm { dst: 7, kidx: k(3) }, // stage the instance word past it
        Uop::Calltg {
            callee: k(4),
            nargs: 1,
        },
        Uop::Kconst {
            dst: 2,
            kidx: k(if even { 5 } else { 6 }),
        }, // t = true / false
        Uop::Ret { src: 2 },
    ]
}

/// The dispatch-tail probe module: `deven`/`dodd` are impl bodies claiming
/// one trailing evidence parameter (params_len 2), each tail-dispatching
/// the sibling through CALLTG; pool k0=0, k1=1, k2=42 (evidence), k3=7
/// (instance), k4 the sibling FuncRef, k5/k6 the bool results.
fn dispatch_tail_module() -> ModuleVersion {
    fn side(even: bool) -> FunctionDef {
        def(
            if even { "deven" } else { "dodd" },
            &[I64, I64, Bool, I64, I64, I64, I64, I64],
            2,
            3,
            vec![
                Const::I64(0),
                Const::I64(1),
                Const::I64(42),
                Const::I64(7),
                Const::FuncRef {
                    module: "dmut".into(),
                    func: u32::from(!even), // the sibling
                },
                Const::Bool(true),
                Const::Bool(false),
            ],
            &dispatch_body(even),
        )
    }
    ModuleVersion::new("dmut", vec![side(true), side(false)]).expect("valid dmut module")
}

/// The dispatch tail transfers through the frame-reuse slide: the mutual
/// recursion runs ten million deep at a constant stack, the evidence
/// parameter arriving with the real argument every slide and the instance
/// word riding past the callee's parameters as a dead constant.
#[test]
fn dispatch_tails_slide_through_the_frame_reuse_transfer() {
    let mut reg = ModuleRegistry::new();
    reg.install(dispatch_tail_module());
    let mut vm = Vm::new(reg);
    let depth = 10_000_000_i64;
    assert_eq!(
        vm.run("dmut", "deven", &[Value::I64(depth), Value::I64(42)]),
        Ok(Value::Bool(true)),
        "the even side answers through the dispatch tail transfer"
    );
    // metadata word + call word + 8 slots, held constant across the 10^7
    // dispatch tail transfers
    assert_eq!(vm.max_stack_len(), 10);
    assert_eq!(
        vm.run("dmut", "dodd", &[Value::I64(depth), Value::I64(42)]),
        Ok(Value::Bool(false)),
        "the odd side answers through its own dispatch tail"
    );
    assert_eq!(vm.max_stack_len(), 10);
}

/// The dispatch-site cache keys CALLTG like CALL/CALLT: the same site hits
/// across repeated runs, two same-pc sites in sibling functions never
/// serve each other's rows, a target swap bumps the generation stamp and
/// the site falls back to the full lookup, and an unresolvable callee
/// traps every time (traps are never cached).
#[test]
fn dispatch_tail_sites_key_the_inline_cache_with_full_lookup_fallback() {
    // two same-shaped functions at identical pcs dispatching different
    // callees through CALLTG: the cache must key on the function. The
    // site stages the call's lowered shape: the real argument, the
    // evidence word the callee's trailing parameter claims, and the
    // instance word riding past them
    fn site(name: &str, callee: u32) -> FunctionDef {
        def(
            name,
            &[I64, I64, I64, I64, I64],
            1,
            4,
            vec![
                Const::I64(42), // the evidence word
                Const::I64(7),  // the instance word
                Const::FuncRef {
                    module: "dmut".into(),
                    func: callee,
                },
            ],
            &[
                Uop::Funcf { frame_slots: 5 },
                Uop::Mov { dst: 2, src: 0 }, // stage the real argument
                Uop::Kimm { dst: 3, kidx: k(0) }, // stage the evidence word
                Uop::Kimm { dst: 4, kidx: k(1) }, // stage the instance word
                Uop::Calltg {
                    callee: k(2),
                    nargs: 1,
                },
                Uop::Ret { src: 2 },
            ],
        )
    }
    let mut reg = ModuleRegistry::new();
    reg.install(dispatch_tail_module());
    reg.install(
        ModuleVersion::new("app", vec![site("f", 0), site("g", 1)]).expect("valid app module"),
    );
    let mut vm = Vm::new(reg);
    assert_eq!(
        vm.run("app", "f", &[Value::I64(10)]),
        Ok(Value::Bool(true)),
        "the first function's site dispatches into dmut's even body"
    );
    assert_eq!(
        vm.run("app", "g", &[Value::I64(9)]),
        Ok(Value::Bool(false)),
        "the same-pc sibling function resolves its own callee"
    );
    assert_eq!(
        vm.run("app", "f", &[Value::I64(10)]),
        Ok(Value::Bool(true)),
        "the first site answers again after the sibling's run"
    );
    // a target swap bumps the generation: the cached row dies and the
    // full lookup redirects through the target's current slot (the swap
    // keeps the parameter span, answering false for every input)
    vm.install_module(
        ModuleVersion::new("dmut", vec![def(
            "deven",
            &[I64, I64, Bool],
            2,
            0,
            vec![Const::Bool(false)],
            &[
                Uop::Funcf { frame_slots: 3 },
                Uop::Kconst { dst: 2, kidx: k(0) },
                Uop::Ret { src: 2 },
            ],
        )])
        .expect("valid swap module"),
    );
    assert_eq!(
        vm.run("app", "f", &[Value::I64(10)]),
        Ok(Value::Bool(false)),
        "the cached dispatch row falls back to the full lookup after the swap"
    );
    // an unresolvable callee traps, and keeps trapping: the failure was
    // never cached
    let bogus = ModuleVersion::new("bad", vec![def(
        "main",
        &[I64, I64, I64],
        1,
        2,
        vec![Const::FuncRef {
            module: "nowhere".into(),
            func: 0,
        }],
        &[
            Uop::Funcf { frame_slots: 3 },
            Uop::Mov { dst: 2, src: 0 },
            Uop::Calltg {
                callee: k(0),
                nargs: 1,
            },
            Uop::Ret { src: 2 },
        ],
    )])
    .expect("valid bad module");
    vm.install_module(bogus);
    for _ in 0..3 {
        assert_eq!(
            vm.run("bad", "main", &[Value::I64(1)]),
            Err(Trap::UnknownCallee {
                module: "nowhere".to_owned(),
                func: 0,
            }),
            "the unresolvable dispatch tail traps every time"
        );
    }
}

/// The sentinel evidence FuncRef dispatches a dispatch tail from the
/// frame's own evidence slot: the word staged there names the serving
/// body, the transfer slides the staged words into it, and the recursion
/// through the sentinel runs a million deep at a constant stack. `drive`
/// and `step` share the shape [n, evidence, t, one, staged-n,
/// staged-evidence] with the evidence word naming `step`'s table index.
#[test]
fn a_runtime_evidence_dispatch_tail_transfers_from_the_frame_slot() {
    fn body() -> [Uop; 10] {
        [
            Uop::Funcf { frame_slots: 7 },
            Uop::Kimm { dst: 3, kidx: k(0) }, // zero
            Uop::EqI64 {
                dst: 2,
                lhs: 0,
                rhs: 3,
            },
            Uop::Jmpif { cond: 2, target: 8 },
            Uop::Kimm { dst: 4, kidx: k(1) }, // one
            Uop::SubI64 {
                dst: 5,
                lhs: 0,
                rhs: 4,
            }, // stage n - 1
            Uop::Mov { dst: 6, src: 1 },      // stage the evidence word
            Uop::Calltg {
                callee: k(2),
                nargs: 2,
            }, // the sentinel reads evidence slot 1's word
            Uop::Kconst { dst: 2, kidx: k(3) },
            Uop::Ret { src: 2 },
        ]
    }
    fn sentinel_def(name: &str) -> FunctionDef {
        def(
            name,
            &[I64, I64, Bool, I64, I64, I64, I64],
            2,
            2,
            vec![
                Const::I64(0),
                Const::I64(1),
                Const::FuncRef {
                    module: "$evidence".into(),
                    func: 1,
                }, // the sentinel names evidence slot 1
                Const::Bool(true),
            ],
            &body(),
        )
    }
    let mut reg = ModuleRegistry::new();
    reg.install(
        ModuleVersion::new("sent", vec![sentinel_def("drive"), sentinel_def("step")])
            .expect("valid sent module"),
    );
    let mut vm = Vm::new(reg);
    // the evidence word 1 names `step`'s function-table index
    assert_eq!(
        vm.run("sent", "drive", &[Value::I64(1_000_000), Value::I64(1)]),
        Ok(Value::Bool(true)),
        "the sentinel dispatch tail runs the named body"
    );
    // metadata + call word + 7 slots, constant across the million-deep
    // runtime-evidence transfers
    assert_eq!(vm.max_stack_len(), 9);
}

/// The sentinel dispatch's exact arity contract (the P0 review repro's
/// hand-assembled equivalent): a target claims its real parameters plus
/// its evidence pairs' rider areas (slot plus two follow words per
/// pair), and the caller stages its real arguments plus exactly one
/// pair's area — so a gap of anything but zero or one rider area is an
/// arity violation. The one-word-short receiver against a two-word
/// `self` (gap 1, the missing word staged as an I64 rider the relaxed
/// check read as a field word) must trap, never slide; the one-pair
/// target (gap 3) keeps transferring.
#[test]
fn a_sentinel_dispatch_with_a_rider_eaten_gap_traps() {
    // the entry shape every probe shares: [evidence, receiver, r0, r1, r2]
    // with the evidence word naming the target and the rider words staged
    // past the one real argument
    fn entry(name: &str) -> FunctionDef {
        def(
            name,
            &[I64, I64, I64, I64, I64],
            1,
            4,
            vec![Const::I64(42), Const::I64(7), Const::FuncRef {
                module: "$evidence".into(),
                func: 0,
            }],
            &[
                Uop::Funcf { frame_slots: 5 },
                Uop::Kimm { dst: 1, kidx: k(0) },
                Uop::Kimm { dst: 2, kidx: k(1) },
                Uop::Kimm { dst: 3, kidx: k(1) },
                Uop::Kimm { dst: 4, kidx: k(1) },
                Uop::Callg {
                    callee: k(2),
                    nargs: 1,
                },
                Uop::Ret { src: 1 },
            ],
        )
    }
    // the one-pair serving body: its receiver plus its rider area (4
    // parameters), and the two-word `self` body the short staging would
    // underfeed
    let pair = def("pair", &[I64, I64, I64, I64], 4, 0, vec![], &[
        Uop::Funcf { frame_slots: 4 },
        Uop::Ret { src: 0 },
    ]);
    let wide = def("wide", &[I64, I64], 2, 0, vec![], &[
        Uop::Funcf { frame_slots: 2 },
        Uop::Ret { src: 0 },
    ]);
    let mut reg = ModuleRegistry::new();
    reg.install(
        ModuleVersion::new("p0exact", vec![entry("ok"), pair, wide, entry("trap")])
            .expect("valid p0exact module"),
    );
    let mut vm = Vm::new(reg);
    // the gap-3 shape transfers: one real argument plus the rider area
    // sliding into the pair body's trailing evidence parameters
    assert_eq!(
        vm.run("p0exact", "ok", &[Value::I64(1)]),
        Ok(Value::I64(42)),
        "the one-pair target takes its riders as its evidence parameters"
    );
    // the gap-1 shape traps: the evidence word names `wide` (index 2),
    // whose two-word `self` is one staged word short — the missing word
    // sits staged as an I64 rider, exactly the word the relaxed check
    // read as the receiver's second field word
    assert_eq!(
        vm.run("p0exact", "trap", &[Value::I64(2)]),
        Err(Trap::ArityMismatch {
            expected: 2,
            given: 1,
        }),
        "a rider-eaten gap is an arity violation, never a silent slide"
    );
}

/// A CALLTG addressing a generic builtin runs the one body inside the
/// instruction and returns its result from the frame — the root-frame
/// CALLT-to-native discipline over the generic dispatch shape: `len` of a
/// KSTR-materialized string with the Str instance as the evidence word.
#[test]
fn a_generic_builtin_dispatch_tail_runs_and_returns() {
    let mut reg = ModuleRegistry::new();
    reg.install_native(native::BUILTIN_MODULE, native::builtin_module());
    reg.install(
        ModuleVersion::new("gapp", vec![def(
            "main",
            &[Ref, Ref, I64],
            0,
            2,
            vec![
                Const::Str("four".into()),
                Const::I64(ds_vm::abi::BuiltinInstance::Str as i64),
                Const::FuncRef {
                    module: "builtin".into(),
                    func: 10, // len, the reserved generic slot
                },
            ],
            &[
                Uop::Funcf { frame_slots: 3 },
                Uop::Kstr { dst: 0, kidx: k(0) }, // materialize "four"
                Uop::Mov { dst: 1, src: 0 },      // stage the receiver
                Uop::Kimm { dst: 2, kidx: k(1) }, // stage the evidence word
                Uop::Calltg {
                    callee: k(2),
                    nargs: 1,
                }, // the body runs, the frame rets its result
                Uop::Ret { src: 0 },
            ],
        )])
        .expect("valid gapp module"),
    );
    let mut vm = Vm::new(reg);
    assert_eq!(
        vm.run("gapp", "main", &[]),
        Ok(Value::I64(4)),
        "the generic dispatch tail answers the string's length"
    );
    assert!(
        !vm.objects().is_empty(),
        "the interned constant's abandoned count rides the root-tail corner"
    );
}
