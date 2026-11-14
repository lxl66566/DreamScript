//! The `par_map` fork-join combinator pins [E3, the parked-CALLG
//! slice]: fork is one spawned child per element through the nursery
//! substrate, the CALLG site parks through the interlock drive (the
//! spawn-only body answering the park request, the seam assembling),
//! and the joined body keeps driving on its own fiber afterward. What
//! the fixtures pin:
//!
//! - order determinism: the answers assemble in input order (spawn order is the drive order), and
//!   an element that parks at a yield mid-flight bounces back in and still lands in place — ten
//!   fresh runs answer the identical checksum, where the hosted `map` loop refuses the same
//!   yielding lambda outright.
//! - first failure: the failing element cancels the looping sibling at its next backward branch
//!   (both children dead, no hang) and the trap re-raises at the par_map call site.
//! - the input list is untouched: its elements read back identical after the join.
//! - the nested shape: a par_map inside a par_map element parks the element's fiber at its own
//!   CALLG site — the drive frames stack on the interlock's heap stack, never the Rust stack — the
//!   inner answers assembling identically to the hosted equivalent; a nested failure re-raises at
//!   the outer boundary like any element failure.
//! - the deep chain: a generated 2048-level nesting lands (past the in-language joins' own
//!   depth-2000 bar) with every spawned child dead.
//! - the tail-site backstop: a par_map staged at a tail dispatch site (CALLTG) refuses with the
//!   structured `TailForkJoin` trap before its spawn phase runs — the join parks its site, and the
//!   tail call would return the frame at the call.
//! - the callable backstop: a staged `Ref` word naming no closure object refuses at the spawn (the
//!   callable-defaulted spelling is a closure word now, so a value-site `Ref` is the corruption
//!   shape); an empty input answers an empty result list.
//! - the closure callable: a closure object's word forks through its own body, the capture block
//!   staged per spawn into the child's entry frame past the element's arguments (the CALLS
//!   copy-in), one acquired count per `Ref` capture — the capturing answers assemble in input order
//!   (wide spans included), the captured `Ref` words balance at the join (each child's copy-in
//!   dec'd once at its own exit, the heap draining to the host-held count), and a first failure
//!   with captured words outstanding releases both dead children's copies (the death walk and the
//!   cancel door's drain) — the named-fn callables keep running unchanged beside them.
//! - the whole-span completion channel: a struct-typed callable answer (a RETSPAN) rides the
//!   spawned child's completion whole, the join assembling each answer's words as one element of
//!   the result list (the first push fixing the stride) — the wide answers assembling in input
//!   order, a yielding element landing its wide answer in place, the completion span's `Ref` words
//!   balancing at the join (one count per word, the heap drains through the joiner's single dec), a
//!   wider outstanding answer surviving the first-failure cancel, and a hand-assembled ragged
//!   answering width trapping at the list's stride check (a width the checker never proves).
//!
//! Layout conventions (as everywhere): overlay slots that stage arguments and then receive
//! results declare `Raw`; children spawn during the CALLG, so their pool ids number up from 1.

use ds_vm::{
    Const, ConstFuncRef, FiberId, FiberState, FunctionDef, ModuleRegistry, ModuleVersion, Trap,
    Value, Vm,
    abi::{
        BuiltinInstance, ConstIdx, FrameLayout, Inst, RcOp, SlotType,
        SlotType::{Bool, I64, Raw, Ref},
        uop::Uop,
    },
    assemble,
    native::{
        BUILTIN_FIBER_YIELD, BUILTIN_LIST_GET, BUILTIN_LIST_LEN, BUILTIN_LIST_NEW,
        BUILTIN_LIST_PUSH, BUILTIN_PAR_MAP, BUILTIN_STR_INT, PLATFORM_IO_OK,
    },
};

fn k(idx: u16) -> ConstIdx {
    ConstIdx(idx)
}

fn builtin(index: u32) -> Const {
    Const::FuncRef(ConstFuncRef::Named {
        module: "builtin".into(),
        func: index,
    })
}

/// A same-module callee by function-table index (the fixture module "pm").
fn pm_ref(index: u32) -> Const {
    Const::FuncRef(ConstFuncRef::Named {
        module: "pm".into(),
        func: index,
    })
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

/// The per-element callable's pool index in the generated mains (the
/// pool contract below).
const CALLABLE: u16 = 5;
/// The List instance evidence word's pool index.
const LIST_EV: u16 = 6;
/// The overlay base of the generated mains: body slots 0..7, overlay
/// words 7..11.
const OV: u8 = 7;

/// One par_map main: build `elems` by `list_push`, fork-join them through
/// `callable`, then read the result back (`list_len`, first and last
/// `list_get`) and answer `first * 1000 + last * 10 + len`. With
/// `recheck_input`, the input list's elements read back after the join
/// and the answer becomes `input_sum * 100000 + checksum` — the
/// untouched-input pin.
fn par_main_def(name: &str, elems: &[i64], callable: u32, recheck_input: bool) -> FunctionDef {
    // k0 list_new, k1 list_push, k2 par_map, k3 list_len, k4 list_get,
    // k5 the callable, k6 the List evidence; element words, read indexes,
    // and weights follow
    let mut pool = vec![
        builtin(BUILTIN_LIST_NEW),
        builtin(BUILTIN_LIST_PUSH),
        builtin(BUILTIN_PAR_MAP),
        builtin(BUILTIN_LIST_LEN),
        builtin(BUILTIN_LIST_GET),
        pm_ref(callable),
        Const::I64(BuiltinInstance::List as i64),
    ];
    let word_at = |pool: &mut Vec<Const>, word: i64| {
        pool.push(Const::I64(word));
        k(u16::try_from(pool.len() - 1).expect("pool index fits u16"))
    };
    let mut body = vec![Uop::Funcf { frame_slots: 11 }];
    // the input list
    body.push(Uop::Call {
        callee: k(0),
        nargs: 0,
    });
    body.push(Uop::Mov { dst: 0, src: OV });
    for &elem in elems {
        let word = word_at(&mut pool, elem);
        body.push(Uop::Mov { dst: OV, src: 0 });
        body.push(Uop::Kimm {
            dst: OV + 1,
            kidx: word,
        });
        body.push(Uop::Call {
            callee: k(1),
            nargs: 2,
        });
    }
    // par_map: (list, callable) plus the two List evidence words
    body.push(Uop::Mov { dst: OV, src: 0 });
    body.push(Uop::Kconst {
        dst: OV + 1,
        kidx: k(CALLABLE),
    });
    body.push(Uop::Kimm {
        dst: OV + 2,
        kidx: k(LIST_EV),
    });
    body.push(Uop::Kimm {
        dst: OV + 3,
        kidx: k(LIST_EV),
    });
    body.push(Uop::Callg {
        callee: k(2),
        nargs: 2,
    });
    body.push(Uop::Mov { dst: 1, src: OV }); // the result list
    // list_len(result)
    body.push(Uop::Mov { dst: OV, src: 1 });
    body.push(Uop::Kimm {
        dst: OV + 1,
        kidx: k(LIST_EV),
    });
    body.push(Uop::Callg {
        callee: k(3),
        nargs: 1,
    });
    body.push(Uop::Mov { dst: 4, src: OV });
    // list_get(result, 0) and list_get(result, last): the ok payload
    // rides overlay word 1
    for (slot, index) in [(2u8, 0usize), (3, elems.len().saturating_sub(1))] {
        let word = word_at(&mut pool, i64::try_from(index).expect("index fits i64"));
        body.push(Uop::Mov { dst: OV, src: 1 });
        body.push(Uop::Kimm {
            dst: OV + 1,
            kidx: word,
        });
        body.push(Uop::Kimm {
            dst: OV + 2,
            kidx: k(LIST_EV),
        });
        body.push(Uop::Callg {
            callee: k(4),
            nargs: 2,
        });
        body.push(Uop::Mov {
            dst: slot,
            src: OV + 1,
        });
    }
    // checksum = first * 1000 + last * 10 + len (body slots 5, 6 scratch)
    let thousand = word_at(&mut pool, 1000);
    let ten = word_at(&mut pool, 10);
    body.push(Uop::Kimm {
        dst: 5,
        kidx: thousand,
    });
    body.push(Uop::MulI64 {
        dst: 5,
        lhs: 2,
        rhs: 5,
    });
    body.push(Uop::Mov { dst: 6, src: 5 });
    body.push(Uop::Kimm { dst: 5, kidx: ten });
    body.push(Uop::MulI64 {
        dst: 5,
        lhs: 3,
        rhs: 5,
    });
    body.push(Uop::AddI64 {
        dst: 6,
        lhs: 6,
        rhs: 5,
    });
    body.push(Uop::AddI64 {
        dst: 6,
        lhs: 6,
        rhs: 4,
    });
    if recheck_input {
        // the input list reads back whole: every element again via
        // list_get, summed into body slot 5
        let zero = word_at(&mut pool, 0);
        let hundred_thousand = word_at(&mut pool, 100_000);
        body.push(Uop::Kimm { dst: 5, kidx: zero });
        for index in 0..elems.len() {
            let word = word_at(&mut pool, i64::try_from(index).expect("index fits i64"));
            body.push(Uop::Mov { dst: OV, src: 0 });
            body.push(Uop::Kimm {
                dst: OV + 1,
                kidx: word,
            });
            body.push(Uop::Kimm {
                dst: OV + 2,
                kidx: k(LIST_EV),
            });
            body.push(Uop::Callg {
                callee: k(4),
                nargs: 2,
            });
            body.push(Uop::AddI64 {
                dst: 5,
                lhs: 5,
                rhs: OV + 1,
            });
        }
        // final = input_sum * 100000 + checksum
        body.push(Uop::Kimm {
            dst: 2,
            kidx: hundred_thousand,
        });
        body.push(Uop::MulI64 {
            dst: 5,
            lhs: 5,
            rhs: 2,
        });
        body.push(Uop::AddI64 {
            dst: 6,
            lhs: 6,
            rhs: 5,
        });
    }
    body.push(Uop::Ret { src: 6 });
    def(
        name,
        &[Ref, Ref, Raw, Raw, I64, I64, I64, Raw, Raw, Raw, Raw],
        0,
        4,
        pool,
        &body,
    )
}

/// `(x: int) -> int`: x * 2 — the plain per-element callable.
fn doubles_def() -> FunctionDef {
    def("doubles", &[I64, I64], 1, 0, vec![Const::I64(2)], &[
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

/// `(x: int) -> int`: parks at `fiber_yield` mid-element (the bounce
/// answer is ignored), then answers x * 2 — the out-of-order-completion
/// stand-in the hosted `map` loop refuses.
fn yielding_doubles_def() -> FunctionDef {
    // body slots 0..2, overlay word 2
    def(
        "yielding_doubles",
        &[I64, I64, Raw],
        1,
        1,
        vec![builtin(BUILTIN_FIBER_YIELD), Const::I64(2)],
        &[
            Uop::Funcf { frame_slots: 3 },
            Uop::Mov { dst: 1, src: 0 },
            Uop::Mov { dst: 2, src: 1 },
            Uop::Call {
                callee: k(0),
                nargs: 1,
            },
            Uop::Kimm { dst: 1, kidx: k(1) },
            Uop::MulI64 {
                dst: 0,
                lhs: 0,
                rhs: 1,
            },
            Uop::Ret { src: 0 },
        ],
    )
}

/// `(x: int) -> int`: x == 0 dies trapped at the division; otherwise it
/// counts x down through a taken backward branch — the preemption point
/// a cancelled sibling observes [E5].
fn guard_def() -> FunctionDef {
    def(
        "guard",
        &[I64, I64, I64, Bool, I64],
        1,
        0,
        vec![Const::I64(0), Const::I64(1)],
        &[
            Uop::Funcf { frame_slots: 5 },
            Uop::Kimm { dst: 1, kidx: k(0) }, // 0
            Uop::Kimm { dst: 2, kidx: k(1) }, // 1
            Uop::EqI64 {
                dst: 3,
                lhs: 0,
                rhs: 1,
            },
            Uop::Jmpif {
                cond: 3,
                target: 11,
            }, // x == 0 -> the trap site
            Uop::GtI64 {
                dst: 3,
                lhs: 0,
                rhs: 1,
            },
            Uop::JmpifNot {
                cond: 3,
                target: 10,
            },
            Uop::SubI64 {
                dst: 4,
                lhs: 0,
                rhs: 2,
            },
            Uop::Mov { dst: 0, src: 4 },
            Uop::Jmp { target: 5 }, // the taken backward branch
            Uop::Ret { src: 1 },    // 0
            Uop::DivI64 {
                dst: 4,
                lhs: 2,
                rhs: 1,
            }, // 1 / 0
            Uop::Ret { src: 4 },
        ],
    )
}

/// `(x: int) -> int`: forks `doubles` over the one-element list `[x]`
/// — the nested par_map whose join the interlock refuses.
fn nested_fn_def() -> FunctionDef {
    // body slots 0..4, overlay words 4..8
    def(
        "nested_fn",
        &[I64, Ref, Ref, Bool, Raw, Raw, Raw, Raw],
        1,
        4,
        vec![
            builtin(BUILTIN_LIST_NEW),                // k0
            builtin(BUILTIN_LIST_PUSH),               // k1
            builtin(BUILTIN_PAR_MAP),                 // k2
            builtin(BUILTIN_LIST_GET),                // k3
            pm_ref(7),                                // k4 doubles
            Const::I64(0),                            // k5 the read index
            Const::I64(BuiltinInstance::List as i64), // k6
        ],
        &[
            Uop::Funcf { frame_slots: 8 },
            Uop::Call {
                callee: k(0),
                nargs: 0,
            },
            Uop::Mov { dst: 1, src: 4 }, // the fresh list
            Uop::Mov { dst: 4, src: 1 },
            Uop::Mov { dst: 5, src: 0 },
            Uop::Call {
                callee: k(1),
                nargs: 2,
            },
            Uop::Mov { dst: 4, src: 1 },
            Uop::Kconst { dst: 5, kidx: k(4) },
            Uop::Kimm { dst: 6, kidx: k(6) },
            Uop::Kimm { dst: 7, kidx: k(6) },
            Uop::Callg {
                callee: k(2),
                nargs: 2,
            }, // the refused join traps here
            Uop::Mov { dst: 2, src: 4 },
            Uop::Mov { dst: 4, src: 2 },
            Uop::Kimm { dst: 5, kidx: k(5) },
            Uop::Kimm { dst: 6, kidx: k(6) },
            Uop::Callg {
                callee: k(3),
                nargs: 2,
            },
            Uop::TagTest {
                dst: 3,
                src: 4,
                tag: PLATFORM_IO_OK,
            },
            Uop::Jmpif {
                cond: 3,
                target: 20,
            },
            Uop::Kimm { dst: 0, kidx: k(5) },
            Uop::Ret { src: 0 },
            Uop::Mov { dst: 0, src: 5 },
            Uop::Ret { src: 0 },
        ],
    )
}

/// `(x: int) -> int`: forks `guard` over the one-element list `[0]` —
/// the nested par_map whose inner element traps, the failure raising
/// through the inner parked seam and out at the outer boundary.
fn nested_fail_fn_def() -> FunctionDef {
    // body slots 0..2, overlay words 2..6
    def(
        "nested_fail_fn",
        &[I64, Ref, Raw, Raw, Raw, Raw],
        1,
        4,
        vec![
            builtin(BUILTIN_LIST_NEW),                // k0
            builtin(BUILTIN_LIST_PUSH),               // k1
            builtin(BUILTIN_PAR_MAP),                 // k2
            pm_ref(9),                                // k3 guard
            Const::I64(0),                            // k4 the inner element
            Const::I64(BuiltinInstance::List as i64), // k5
        ],
        &[
            Uop::Funcf { frame_slots: 6 },
            Uop::Call {
                callee: k(0),
                nargs: 0,
            },
            Uop::Mov { dst: 1, src: 2 }, // the fresh list
            Uop::Mov { dst: 2, src: 1 },
            Uop::Kimm { dst: 3, kidx: k(4) },
            Uop::Call {
                callee: k(1),
                nargs: 2,
            },
            Uop::Mov { dst: 2, src: 1 },        // list
            Uop::Kconst { dst: 3, kidx: k(3) }, // the callable
            Uop::Kimm { dst: 4, kidx: k(5) },   // evidence
            Uop::Kimm { dst: 5, kidx: k(5) },   // evidence
            Uop::Callg {
                callee: k(2),
                nargs: 2,
            }, // the inner failure raises here
            Uop::Mov { dst: 0, src: 2 },        // never reached
            Uop::Ret { src: 0 },
        ],
    )
}

/// `() -> int`: par_map over `[1]` staged at a tail dispatch site —
/// the hand-assembled backstop for the tail-position fork (the join
/// parks its site, the tail call would return the frame at the call).
fn tail_fork_def() -> FunctionDef {
    // body slot 0, overlay words 1..5
    def(
        "main_tail_fork",
        &[Ref, Raw, Raw, Raw, Raw],
        0,
        4,
        vec![
            builtin(BUILTIN_LIST_NEW),                // k0
            builtin(BUILTIN_LIST_PUSH),               // k1
            builtin(BUILTIN_PAR_MAP),                 // k2
            pm_ref(7),                                // k3 doubles
            Const::I64(1),                            // k4 the element
            Const::I64(BuiltinInstance::List as i64), // k5
        ],
        &[
            Uop::Funcf { frame_slots: 5 },
            Uop::Call {
                callee: k(0),
                nargs: 0,
            },
            Uop::Mov { dst: 0, src: 1 }, // the fresh list
            Uop::Mov { dst: 1, src: 0 },
            Uop::Kimm { dst: 2, kidx: k(4) },
            Uop::Call {
                callee: k(1),
                nargs: 2,
            },
            Uop::Mov { dst: 1, src: 0 },        // list
            Uop::Kconst { dst: 2, kidx: k(3) }, // the callable
            Uop::Kimm { dst: 3, kidx: k(5) },   // evidence
            Uop::Kimm { dst: 4, kidx: k(5) },   // evidence
            Uop::Calltg {
                callee: k(2),
                nargs: 2,
            }, // the tail-site refusal traps here
        ],
    )
}

/// One deep-chain level: `level_i(x)` forks `callable` (the level
/// below) over `[x]`, reads the answer's element back, and answers it
/// plus one — the answer witnesses the nesting depth (`level_i(0)` is
/// `i` when the base is the identity).
fn deep_level_def(name: &str, callable: Const) -> FunctionDef {
    // body slots 0..4, overlay words 4..8
    def(
        name,
        &[I64, Ref, Ref, Bool, Raw, Raw, Raw, Raw],
        1,
        4,
        vec![
            builtin(BUILTIN_LIST_NEW),                // k0
            builtin(BUILTIN_LIST_PUSH),               // k1
            builtin(BUILTIN_PAR_MAP),                 // k2
            builtin(BUILTIN_LIST_GET),                // k3
            callable,                                 // k4
            Const::I64(0),                            // k5 the read index
            Const::I64(1),                            // k6 the level addend
            Const::I64(BuiltinInstance::List as i64), // k7
        ],
        &[
            Uop::Funcf { frame_slots: 8 },
            Uop::Call {
                callee: k(0),
                nargs: 0,
            },
            Uop::Mov { dst: 1, src: 4 }, // the fresh list
            Uop::Mov { dst: 4, src: 1 },
            Uop::Mov { dst: 5, src: 0 }, // the element x
            Uop::Call {
                callee: k(1),
                nargs: 2,
            },
            Uop::Mov { dst: 4, src: 1 },        // list
            Uop::Kconst { dst: 5, kidx: k(4) }, // the callable
            Uop::Kimm { dst: 6, kidx: k(7) },   // evidence
            Uop::Kimm { dst: 7, kidx: k(7) },   // evidence
            Uop::Callg {
                callee: k(2),
                nargs: 2,
            },
            Uop::Mov { dst: 2, src: 4 }, // the result list
            Uop::Mov { dst: 4, src: 2 },
            Uop::Kimm { dst: 5, kidx: k(5) }, // the read index
            Uop::Kimm { dst: 6, kidx: k(7) }, // evidence
            Uop::Callg {
                callee: k(3),
                nargs: 2,
            },
            Uop::TagTest {
                dst: 3,
                src: 4,
                tag: PLATFORM_IO_OK,
            },
            Uop::Jmpif {
                cond: 3,
                target: 20,
            },
            Uop::Kimm { dst: 0, kidx: k(5) },
            Uop::Ret { src: 0 },
            Uop::Mov { dst: 0, src: 5 }, // the ok payload
            Uop::Kimm { dst: 6, kidx: k(6) },
            Uop::AddI64 {
                dst: 0,
                lhs: 0,
                rhs: 6,
            },
            Uop::Ret { src: 0 },
        ],
    )
}

/// `level_0(x: int) -> int`: the identity — the deep chain's base.
fn deep_base_def() -> FunctionDef {
    def("level_0", &[I64], 1, 0, vec![], &[
        Uop::Funcf { frame_slots: 1 },
        Uop::Ret { src: 0 },
    ])
}

/// `() -> int`: par_map over `[1]` staging the LIST word against the
/// callable slot — the hand-assembled backstop for the callable word.
fn bad_callable_def() -> FunctionDef {
    // body slot 0, overlay words 1..5
    def(
        "main_bad_callable",
        &[Ref, Raw, Raw, Raw, Raw],
        0,
        4,
        vec![
            builtin(BUILTIN_LIST_NEW),                // k0
            builtin(BUILTIN_LIST_PUSH),               // k1
            builtin(BUILTIN_PAR_MAP),                 // k2
            Const::I64(1),                            // k3 the element
            Const::I64(BuiltinInstance::List as i64), // k4
        ],
        &[
            Uop::Funcf { frame_slots: 5 },
            Uop::Call {
                callee: k(0),
                nargs: 0,
            },
            Uop::Mov { dst: 0, src: 1 },
            Uop::Mov { dst: 1, src: 0 },
            Uop::Kimm { dst: 2, kidx: k(3) },
            Uop::Call {
                callee: k(1),
                nargs: 2,
            },
            Uop::Mov { dst: 1, src: 0 }, // ov0 = the list
            Uop::Mov { dst: 2, src: 0 }, // ov1 = the list word as the callable
            Uop::Kimm { dst: 3, kidx: k(4) },
            Uop::Kimm { dst: 4, kidx: k(4) },
            Uop::Callg {
                callee: k(2),
                nargs: 2,
            },
            Uop::Ret { src: 1 },
        ],
    )
}

/// `() -> int`: par_map over the empty list answers the empty list;
/// `list_len` reads 0 back.
fn empty_def() -> FunctionDef {
    // body slots 0..2, overlay words 2..6
    def(
        "main_empty",
        &[Ref, I64, Raw, Raw, Raw, Raw],
        0,
        4,
        vec![
            builtin(BUILTIN_LIST_NEW),                // k0
            builtin(BUILTIN_PAR_MAP),                 // k1
            builtin(BUILTIN_LIST_LEN),                // k2
            pm_ref(7),                                // k3 doubles
            Const::I64(BuiltinInstance::List as i64), // k4
        ],
        &[
            Uop::Funcf { frame_slots: 6 },
            Uop::Call {
                callee: k(0),
                nargs: 0,
            },
            Uop::Mov { dst: 0, src: 2 },
            Uop::Mov { dst: 2, src: 0 },
            Uop::Kconst { dst: 3, kidx: k(3) },
            Uop::Kimm { dst: 4, kidx: k(4) },
            Uop::Kimm { dst: 5, kidx: k(4) },
            Uop::Callg {
                callee: k(1),
                nargs: 2,
            },
            Uop::Mov { dst: 2, src: 2 }, // the empty result stays on ov0
            Uop::Kimm { dst: 3, kidx: k(4) },
            Uop::Callg {
                callee: k(2),
                nargs: 1,
            },
            Uop::Mov { dst: 1, src: 2 },
            Uop::Ret { src: 1 },
        ],
    )
}

/// `(x: int) -> (int, int)`: answers the span [x*2, x*3] through
/// RETSPAN — the struct-typed answer shape the completion channel
/// carries whole.
fn wide_pairs_def() -> FunctionDef {
    def(
        "wide_pairs",
        &[I64, I64, I64],
        1,
        0,
        vec![Const::I64(2), Const::I64(3)],
        &[
            Uop::Funcf { frame_slots: 3 },
            Uop::Kimm { dst: 1, kidx: k(0) },
            Uop::MulI64 {
                dst: 1,
                lhs: 0,
                rhs: 1,
            },
            Uop::Kimm { dst: 2, kidx: k(1) },
            Uop::MulI64 {
                dst: 2,
                lhs: 0,
                rhs: 2,
            },
            Uop::RetSpan { src: 1, n: 2 },
        ],
    )
}

/// `(x: int) -> (int, int)`: parks at `fiber_yield` mid-element (the
/// bounce answer ignored), then answers [x*2, x*3] — the
/// out-of-order-completion stand-in over wide answers.
fn yielding_wide_pairs_def() -> FunctionDef {
    def(
        "yielding_wide_pairs",
        &[I64, I64, I64, Raw],
        1,
        1,
        vec![builtin(BUILTIN_FIBER_YIELD), Const::I64(2), Const::I64(3)],
        &[
            Uop::Funcf { frame_slots: 4 },
            Uop::Mov { dst: 2, src: 0 },
            Uop::Mov { dst: 3, src: 2 },
            Uop::Call {
                callee: k(0),
                nargs: 1,
            },
            Uop::Kimm { dst: 1, kidx: k(1) },
            Uop::MulI64 {
                dst: 1,
                lhs: 0,
                rhs: 1,
            },
            Uop::Kimm { dst: 2, kidx: k(2) },
            Uop::MulI64 {
                dst: 2,
                lhs: 0,
                rhs: 2,
            },
            Uop::RetSpan { src: 1, n: 2 },
        ],
    )
}

/// `(x: int) -> (int, string)`: answers [x*10, str_int(x)] — the
/// completion span's `Ref` word (a fresh object per child), the
/// balance witness over the join.
fn wordy_def() -> FunctionDef {
    def(
        "wordy",
        &[I64, I64, Ref, Raw],
        1,
        1,
        vec![builtin(BUILTIN_STR_INT), Const::I64(10)],
        &[
            Uop::Funcf { frame_slots: 4 },
            Uop::Mov { dst: 3, src: 0 },
            Uop::Call {
                callee: k(0),
                nargs: 1,
            },
            Uop::Mov { dst: 2, src: 3 },
            Uop::Kimm { dst: 1, kidx: k(1) },
            Uop::MulI64 {
                dst: 1,
                lhs: 0,
                rhs: 1,
            },
            Uop::RetSpan { src: 1, n: 2 },
        ],
    )
}

/// `(x: int) -> (int, string)`: x == 0 dies trapped at the division
/// (the entry check); otherwise it counts x down through a taken
/// backward branch and answers [0, "0"] — the wide-answer
/// first-failure shape.
fn wide_guard_def() -> FunctionDef {
    def(
        "wide_guard",
        &[I64, I64, Ref, Bool, I64, Raw],
        1,
        1,
        vec![
            Const::I64(0),
            Const::I64(1),
            builtin(BUILTIN_STR_INT),
            Const::I64(2),
        ],
        &[
            Uop::Funcf { frame_slots: 6 },    // pc0
            Uop::Kimm { dst: 1, kidx: k(0) }, // pc1  0
            Uop::Kimm { dst: 4, kidx: k(1) }, // pc2  1
            Uop::EqI64 {
                dst: 3,
                lhs: 0,
                rhs: 1,
            }, // pc3  x == 0
            Uop::Jmpif {
                cond: 3,
                target: 17,
            }, // pc4  -> the trap site
            Uop::GtI64 {
                dst: 3,
                lhs: 0,
                rhs: 4,
            }, // pc5  x > 0
            Uop::JmpifNot {
                cond: 3,
                target: 10,
            }, // pc6  -> the answer block
            Uop::SubI64 {
                dst: 4,
                lhs: 0,
                rhs: 4,
            }, // pc7  x - 1
            Uop::Mov { dst: 0, src: 4 },      // pc8
            Uop::Jmp { target: 5 },           // pc9  the taken backward branch
            Uop::Kimm { dst: 4, kidx: k(3) }, // pc10 2
            Uop::MulI64 {
                dst: 4,
                lhs: 0,
                rhs: 4,
            }, // pc11 x*2
            Uop::Mov { dst: 5, src: 0 },      // pc12 stage x
            Uop::Call {
                callee: k(2),
                nargs: 1,
            }, // pc13 str_int(x)
            Uop::Mov { dst: 2, src: 5 },      // pc14 the fresh string
            Uop::Mov { dst: 1, src: 4 },      // pc15
            Uop::RetSpan { src: 1, n: 2 },    // pc16 [x*2, str(x)]
            Uop::DivI64 {
                dst: 4,
                lhs: 4,
                rhs: 1,
            }, // pc17 1 / 0
            Uop::Ret { src: 4 },
        ],
    )
}

/// `(x: int) -> int | (int, int)`: x > 3 answers the wide span, else
/// one word — the hand-assembled ragged answering width the stride
/// check traps (a width the checker never proves).
fn ragged_def() -> FunctionDef {
    def(
        "ragged",
        &[I64, I64, I64, Bool],
        1,
        0,
        vec![Const::I64(3), Const::I64(2)],
        &[
            Uop::Funcf { frame_slots: 4 },    // pc0
            Uop::Kimm { dst: 2, kidx: k(0) }, // pc1  3
            Uop::GtI64 {
                dst: 3,
                lhs: 0,
                rhs: 2,
            }, // pc2  x > 3
            Uop::Jmpif { cond: 3, target: 7 }, // pc3  -> the wide block
            Uop::Kimm { dst: 1, kidx: k(1) }, // pc4  2
            Uop::MulI64 {
                dst: 1,
                lhs: 0,
                rhs: 1,
            }, // pc5  x*2
            Uop::Ret { src: 1 },              // pc6  one word
            Uop::Kimm { dst: 1, kidx: k(1) }, // pc7  2
            Uop::MulI64 {
                dst: 1,
                lhs: 0,
                rhs: 1,
            }, // pc8  x*2
            Uop::Kimm { dst: 2, kidx: k(0) }, // pc9  3
            Uop::MulI64 {
                dst: 2,
                lhs: 0,
                rhs: 2,
            }, // pc10 x*3
            Uop::RetSpan { src: 1, n: 2 },
        ],
    )
}

/// One par_map main over a wide-answer callable: build `elems`, fork
/// them through `callable`, then read the result back — `list_len`,
/// then the first and last elements' whole spans through `list_get`
/// (the ok payload riding overlay words 1 and 2) — and answer
/// `first0 * 100000 + first1 * 10000 + last0 * 100 + last1 * 10 + len`.
/// With `recheck_input`, the input list's elements read back after the
/// join and the answer becomes `input_sum * 1000000 + checksum` — the
/// untouched-input pin.
fn wide_main_def(name: &str, elems: &[i64], callable: u32, recheck_input: bool) -> FunctionDef {
    // k0 list_new, k1 list_push, k2 par_map, k3 list_len, k4 list_get,
    // k5 the callable, k6 the List evidence; element words, read
    // indexes, and weights follow
    let mut pool = vec![
        builtin(BUILTIN_LIST_NEW),
        builtin(BUILTIN_LIST_PUSH),
        builtin(BUILTIN_PAR_MAP),
        builtin(BUILTIN_LIST_LEN),
        builtin(BUILTIN_LIST_GET),
        pm_ref(callable),
        Const::I64(BuiltinInstance::List as i64),
    ];
    let word_at = |pool: &mut Vec<Const>, word: i64| {
        pool.push(Const::I64(word));
        k(u16::try_from(pool.len() - 1).expect("pool index fits u16"))
    };
    // body slots 0..9: 0 the input, 1 the result, 2/3 the first
    // element's words, 4/5 the last's, 6 the len, 7 the checksum, 8
    // scratch; overlay words 9..13
    let mut body = vec![Uop::Funcf { frame_slots: 13 }];
    body.push(Uop::Call {
        callee: k(0),
        nargs: 0,
    });
    body.push(Uop::Mov { dst: 0, src: 9 });
    for &elem in elems {
        let word = word_at(&mut pool, elem);
        body.push(Uop::Mov { dst: 9, src: 0 });
        body.push(Uop::Kimm {
            dst: 10,
            kidx: word,
        });
        body.push(Uop::Call {
            callee: k(1),
            nargs: 2,
        });
    }
    // par_map: (list, callable) plus the two List evidence words
    body.push(Uop::Mov { dst: 9, src: 0 });
    body.push(Uop::Kconst {
        dst: 10,
        kidx: k(5),
    });
    body.push(Uop::Kimm {
        dst: 11,
        kidx: k(6),
    });
    body.push(Uop::Kimm {
        dst: 12,
        kidx: k(6),
    });
    body.push(Uop::Callg {
        callee: k(2),
        nargs: 2,
    });
    body.push(Uop::Mov { dst: 1, src: 9 });
    // list_len(result)
    body.push(Uop::Mov { dst: 9, src: 1 });
    body.push(Uop::Kimm {
        dst: 10,
        kidx: k(6),
    });
    body.push(Uop::Callg {
        callee: k(3),
        nargs: 1,
    });
    body.push(Uop::Mov { dst: 6, src: 9 });
    // list_get(result, i): the ok payload spans overlay words 1 and 2
    for (a, b, index) in [(2u8, 3u8, 0usize), (4, 5, elems.len().saturating_sub(1))] {
        let word = word_at(&mut pool, i64::try_from(index).expect("index fits i64"));
        body.push(Uop::Mov { dst: 9, src: 1 });
        body.push(Uop::Kimm {
            dst: 10,
            kidx: word,
        });
        body.push(Uop::Kimm {
            dst: 11,
            kidx: k(6),
        });
        body.push(Uop::Callg {
            callee: k(4),
            nargs: 2,
        });
        body.push(Uop::Mov { dst: a, src: 10 });
        body.push(Uop::Mov { dst: b, src: 11 });
    }
    // checksum = a0*100000 + a1*10000 + b0*100 + b1*10 + len
    body.push(Uop::Kimm {
        dst: 7,
        kidx: word_at(&mut pool, 0),
    });
    for (slot, weight) in [(2u8, 100_000i64), (3, 10_000), (4, 100), (5, 10)] {
        body.push(Uop::Kimm {
            dst: 8,
            kidx: word_at(&mut pool, weight),
        });
        body.push(Uop::MulI64 {
            dst: slot,
            lhs: slot,
            rhs: 8,
        });
        body.push(Uop::AddI64 {
            dst: 7,
            lhs: 7,
            rhs: slot,
        });
    }
    body.push(Uop::AddI64 {
        dst: 7,
        lhs: 7,
        rhs: 6,
    });
    if recheck_input {
        // the input list reads back whole: every element again via
        // list_get, summed into body slot 8
        body.push(Uop::Kimm {
            dst: 8,
            kidx: word_at(&mut pool, 0),
        });
        for index in 0..elems.len() {
            let word = word_at(&mut pool, i64::try_from(index).expect("index fits i64"));
            body.push(Uop::Mov { dst: 9, src: 0 });
            body.push(Uop::Kimm {
                dst: 10,
                kidx: word,
            });
            body.push(Uop::Kimm {
                dst: 11,
                kidx: k(6),
            });
            body.push(Uop::Callg {
                callee: k(4),
                nargs: 2,
            });
            body.push(Uop::AddI64 {
                dst: 8,
                lhs: 8,
                rhs: 10,
            });
        }
        // final = input_sum * 1000000 + checksum
        body.push(Uop::Kimm {
            dst: 2,
            kidx: word_at(&mut pool, 1_000_000),
        });
        body.push(Uop::MulI64 {
            dst: 8,
            lhs: 8,
            rhs: 2,
        });
        body.push(Uop::AddI64 {
            dst: 7,
            lhs: 7,
            rhs: 8,
        });
    }
    body.push(Uop::Ret { src: 7 });
    def(
        name,
        &[
            Ref, Ref, I64, I64, I64, I64, I64, I64, I64, Raw, Raw, Raw, Raw,
        ],
        0,
        4,
        pool,
        &body,
    )
}

/// One bare par_map main: build `elems`, fork them through `callable`,
/// answer the result list `Ref` — the balance and failure mains whose
/// value or trap surfaces at the CALLG site.
fn fork_main_def(name: &str, elems: &[i64], callable: u32) -> FunctionDef {
    // k0 list_new, k1 list_push, k2 par_map, k3 the callable, k4 the
    // List evidence; element words follow
    let mut pool = vec![
        builtin(BUILTIN_LIST_NEW),
        builtin(BUILTIN_LIST_PUSH),
        builtin(BUILTIN_PAR_MAP),
        pm_ref(callable),
        Const::I64(BuiltinInstance::List as i64),
    ];
    let word_at = |pool: &mut Vec<Const>, word: i64| {
        pool.push(Const::I64(word));
        k(u16::try_from(pool.len() - 1).expect("pool index fits u16"))
    };
    // body slots 0..5: 0 the input, 1 the result; overlay words 5..9
    let mut body = vec![Uop::Funcf { frame_slots: 9 }];
    body.push(Uop::Call {
        callee: k(0),
        nargs: 0,
    });
    body.push(Uop::Mov { dst: 0, src: 5 });
    for &elem in elems {
        let word = word_at(&mut pool, elem);
        body.push(Uop::Mov { dst: 5, src: 0 });
        body.push(Uop::Kimm { dst: 6, kidx: word });
        body.push(Uop::Call {
            callee: k(1),
            nargs: 2,
        });
    }
    body.push(Uop::Mov { dst: 5, src: 0 });
    body.push(Uop::Kconst { dst: 6, kidx: k(3) });
    body.push(Uop::Kimm { dst: 7, kidx: k(4) });
    body.push(Uop::Kimm { dst: 8, kidx: k(4) });
    body.push(Uop::Callg {
        callee: k(2),
        nargs: 2,
    });
    body.push(Uop::Mov { dst: 1, src: 5 });
    body.push(Uop::Ret { src: 1 });
    def(
        name,
        &[Ref, Ref, Raw, Raw, Raw, Raw, Raw, Raw, Raw],
        0,
        4,
        pool,
        &body,
    )
}

/// The lambda-form callables a fork stages captures into: the capture
/// block lands in the frame's local slots past the parameters (the
/// CALLS copy-in coordinates), and the body emits its own capture exit
/// dec (the placement pass's emission, hand-written here) — so these
/// need `RcOp` words beside the uops and assemble from `Inst`.
fn def_inst(
    name: &str,
    slots: &[SlotType],
    params: usize,
    args: usize,
    pool: Vec<Const>,
    body: &[Inst],
) -> FunctionDef {
    FunctionDef {
        name: name.into(),
        layout: FrameLayout::new(slots.to_vec(), params, args).expect("valid fixture layout"),
        pool,
        code: assemble(body).expect("fixture body encodes"),
    }
}

/// `(x: int) -> int` with one i64 capture at slot 1: x * 2 + bias.
fn biased_def() -> FunctionDef {
    def("biased", &[I64, I64, I64], 1, 0, vec![Const::I64(2)], &[
        Uop::Funcf { frame_slots: 3 },
        Uop::Kimm { dst: 2, kidx: k(0) },
        Uop::MulI64 {
            dst: 2,
            lhs: 0,
            rhs: 2,
        },
        Uop::AddI64 {
            dst: 2,
            lhs: 2,
            rhs: 1,
        },
        Uop::Ret { src: 2 },
    ])
}

/// `(x: int) -> (int, int)` with one i64 capture at slot 1: the span
/// [x*2 + bias, x*3] — the wide answer of a capturing fork.
fn wide_capture_def() -> FunctionDef {
    def(
        "wide_capture",
        &[I64, I64, I64, I64],
        1,
        0,
        vec![Const::I64(2), Const::I64(3)],
        &[
            Uop::Funcf { frame_slots: 4 },
            Uop::Kimm { dst: 2, kidx: k(0) },
            Uop::MulI64 {
                dst: 2,
                lhs: 0,
                rhs: 2,
            },
            Uop::AddI64 {
                dst: 2,
                lhs: 2,
                rhs: 1,
            },
            Uop::Kimm { dst: 3, kidx: k(1) },
            Uop::MulI64 {
                dst: 3,
                lhs: 0,
                rhs: 3,
            },
            Uop::RetSpan { src: 2, n: 2 },
        ],
    )
}

/// `(x: int) -> int` with one string capture at slot 1 (unread by the
/// body): the capture's copy-in/exit-dec accounting is the balance
/// witness, the answer x * 2.
fn string_capture_def() -> FunctionDef {
    def_inst(
        "string_capture",
        &[I64, Ref, I64],
        1,
        0,
        vec![Const::I64(2)],
        &[
            Uop::Funcf { frame_slots: 3 }.into(),
            Uop::Kimm { dst: 2, kidx: k(0) }.into(),
            Uop::MulI64 {
                dst: 2,
                lhs: 0,
                rhs: 2,
            }
            .into(),
            // the capture's exit dec — the placement pass's emission
            RcOp::Dec { src: 1 }.into(),
            Uop::Ret { src: 2 }.into(),
        ],
    )
}

/// `(x: int) -> int` with one string capture at slot 1: x == 0 dies
/// trapped at the division (the trap path never decs the capture — the
/// death walk owns it); otherwise it counts x down through a taken
/// backward branch and answers 0, dec-ing the capture at the landing
/// (a cut sibling's copy releases at the cancel door's drain).
fn capturing_guard_def() -> FunctionDef {
    def_inst(
        "capturing_guard",
        &[I64, Ref, I64, Bool, I64, I64],
        1,
        0,
        vec![Const::I64(0), Const::I64(1)],
        &[
            Uop::Funcf { frame_slots: 6 }.into(),    // pc0
            Uop::Kimm { dst: 2, kidx: k(0) }.into(), // pc1  0
            Uop::Kimm { dst: 4, kidx: k(1) }.into(), // pc2  1
            Uop::EqI64 {
                dst: 3,
                lhs: 0,
                rhs: 2,
            }
            .into(), // pc3  x == 0
            Uop::Jmpif {
                cond: 3,
                target: 12,
            }
            .into(), // pc4  -> the trap site
            Uop::GtI64 {
                dst: 3,
                lhs: 0,
                rhs: 4,
            }
            .into(), // pc5  x > 0
            Uop::JmpifNot {
                cond: 3,
                target: 10,
            }
            .into(), // pc6  -> the landing
            Uop::SubI64 {
                dst: 5,
                lhs: 0,
                rhs: 4,
            }
            .into(), // pc7  x - 1
            Uop::Mov { dst: 0, src: 5 }.into(),      // pc8
            Uop::Jmp { target: 5 }.into(),           // pc9  the taken backward branch
            RcOp::Dec { src: 1 }.into(),             // pc10 the capture's exit dec
            Uop::Ret { src: 2 }.into(),              // pc11 answer 0
            Uop::DivI64 {
                dst: 5,
                lhs: 4,
                rhs: 2,
            }
            .into(), // pc12 1 / 0
            Uop::Ret { src: 5 }.into(),
        ],
    )
}

/// One par_map main through a closure callable: build `elems`,
/// construct the closure (the header word at slot 7, the object's word
/// at slot 6, the one capture staged at overlay word 0), fork-join
/// through it, then read the result back — `list_len`, then the first
/// and last elements through `list_get` — and answer the weighted
/// checksum. With `wide`, the callable answers a two-word span and each
/// readback takes the payload's two overlay words.
fn closure_main_def(name: &str, elems: &[i64], callee: u32, bias: i64, wide: bool) -> FunctionDef {
    // k0 list_new, k1 list_push, k2 par_map, k3 list_len, k4 list_get,
    // k5 the callable's header, k6 the List evidence; element words,
    // the bias, read indexes, and weights follow
    let mut pool = vec![
        builtin(BUILTIN_LIST_NEW),
        builtin(BUILTIN_LIST_PUSH),
        builtin(BUILTIN_PAR_MAP),
        builtin(BUILTIN_LIST_LEN),
        builtin(BUILTIN_LIST_GET),
        pm_ref(callee),
        Const::I64(BuiltinInstance::List as i64),
    ];
    let word_at = |pool: &mut Vec<Const>, word: i64| {
        pool.push(Const::I64(word));
        k(u16::try_from(pool.len() - 1).expect("pool index fits u16"))
    };
    // body slots 0..9: 0 the input, 1 the result, 2..5 the readback,
    // 6 the closure word, 7 the header, 8 the len, 9 the checksum;
    // overlay words 10..13
    let mut body = vec![Uop::Funcf { frame_slots: 14 }];
    body.push(Uop::Call {
        callee: k(0),
        nargs: 0,
    });
    body.push(Uop::Mov { dst: 0, src: 10 });
    for &elem in elems {
        let word = word_at(&mut pool, elem);
        body.push(Uop::Mov { dst: 10, src: 0 });
        body.push(Uop::Kimm {
            dst: 11,
            kidx: word,
        });
        body.push(Uop::Call {
            callee: k(1),
            nargs: 2,
        });
    }
    // the closure: the bias capture staged at overlay word 0
    body.push(Uop::Kimm {
        dst: 10,
        kidx: word_at(&mut pool, bias),
    });
    body.push(Uop::Kconst { dst: 7, kidx: k(5) });
    body.push(Uop::ClosureNew {
        dst: 6,
        func: 7,
        n: 1,
    });
    // par_map: (list, callable) plus the two List evidence words
    body.push(Uop::Mov { dst: 10, src: 0 });
    body.push(Uop::Mov { dst: 11, src: 6 });
    body.push(Uop::Kimm {
        dst: 12,
        kidx: k(6),
    });
    body.push(Uop::Kimm {
        dst: 13,
        kidx: k(6),
    });
    body.push(Uop::Callg {
        callee: k(2),
        nargs: 2,
    });
    body.push(Uop::Mov { dst: 1, src: 10 });
    // list_len(result)
    body.push(Uop::Mov { dst: 10, src: 1 });
    body.push(Uop::Kimm {
        dst: 11,
        kidx: k(6),
    });
    body.push(Uop::Callg {
        callee: k(3),
        nargs: 1,
    });
    body.push(Uop::Mov { dst: 8, src: 10 });
    // list_get(result, i): the ok payload rides overlay word 1, the
    // wide answer's second word overlay word 2
    // (first readback slot, second for the wide answer, the index)
    let reads: &[(u8, u8, usize)] = if wide {
        &[(2, 3, 0), (4, 5, elems.len() - 1)]
    } else {
        &[(2, 2, 0), (3, 3, elems.len() - 1)]
    };
    for &(a, b, index) in reads {
        let word = word_at(&mut pool, i64::try_from(index).expect("index fits i64"));
        body.push(Uop::Mov { dst: 10, src: 1 });
        body.push(Uop::Kimm {
            dst: 11,
            kidx: word,
        });
        body.push(Uop::Kimm {
            dst: 12,
            kidx: k(6),
        });
        body.push(Uop::Callg {
            callee: k(4),
            nargs: 2,
        });
        body.push(Uop::Mov { dst: a, src: 11 });
        if wide {
            body.push(Uop::Mov { dst: b, src: 12 });
        }
    }
    // the checksum: each readback word scaled by its weight (the
    // weight rides overlay word 0), then the len added
    body.push(Uop::Kimm {
        dst: 9,
        kidx: word_at(&mut pool, 0),
    });
    let weights: &[(u8, i64)] =
        if wide { &[(2, 100_000), (3, 10_000), (4, 100), (5, 10)] } else { &[(2, 1000), (3, 10)] };
    for (slot, weight) in weights {
        body.push(Uop::Kimm {
            dst: 10,
            kidx: word_at(&mut pool, *weight),
        });
        body.push(Uop::MulI64 {
            dst: *slot,
            lhs: *slot,
            rhs: 10,
        });
        body.push(Uop::AddI64 {
            dst: 9,
            lhs: 9,
            rhs: *slot,
        });
    }
    body.push(Uop::AddI64 {
        dst: 9,
        lhs: 9,
        rhs: 8,
    });
    body.push(Uop::Ret { src: 9 });
    def(
        name,
        &[
            Ref, Ref, I64, I64, I64, I64, Raw, Raw, I64, I64, Raw, Raw, Raw, Raw,
        ],
        0,
        4,
        pool,
        &body,
    )
}

/// `() -> string-ref`: hands the interned "shared" word out to the host
/// (count one, host-held) — the probe the balance tests read counts
/// through after later fixtures capture the same text.
fn kstr_probe_def() -> FunctionDef {
    def(
        "main_kstr_probe",
        &[Ref],
        0,
        0,
        vec![Const::Str("shared".into())],
        &[
            Uop::Funcf { frame_slots: 1 },
            Uop::Kstr { dst: 0, kidx: k(0) },
            Uop::Ret { src: 0 },
        ],
    )
}

/// `() -> string-ref`: forks `[1, 2, 3]` through a string-capturing
/// closure, releases the closure word and the result list, and answers
/// the captured word — the balance main (the count must read exactly
/// the host-held one after the join: each child's copy-in dec'd once at
/// its own exit).
fn ref_balance_def() -> FunctionDef {
    def_inst(
        "main_ref_balance",
        &[
            Ref, Ref, Raw, Raw, Raw, Raw, Raw, Raw, Ref, Raw, Raw, Raw, Raw,
        ],
        0,
        4,
        vec![
            builtin(BUILTIN_LIST_NEW),                // k0
            builtin(BUILTIN_LIST_PUSH),               // k1
            builtin(BUILTIN_PAR_MAP),                 // k2
            pm_ref(24),                               // k3 string_capture's header
            Const::I64(BuiltinInstance::List as i64), // k4
            Const::Str("shared".into()),              // k5
            Const::I64(1),                            // k6
            Const::I64(2),                            // k7
            Const::I64(3),                            // k8
        ],
        &[
            Uop::Funcf { frame_slots: 13 }.into(),
            Uop::Call {
                callee: k(0),
                nargs: 0,
            }
            .into(),
            Uop::Mov { dst: 0, src: 9 }.into(),
            // the input list [1, 2, 3]
            Uop::Mov { dst: 9, src: 0 }.into(),
            Uop::Kimm {
                dst: 10,
                kidx: k(6),
            }
            .into(),
            Uop::Call {
                callee: k(1),
                nargs: 2,
            }
            .into(),
            Uop::Mov { dst: 9, src: 0 }.into(),
            Uop::Kimm {
                dst: 10,
                kidx: k(7),
            }
            .into(),
            Uop::Call {
                callee: k(1),
                nargs: 2,
            }
            .into(),
            Uop::Mov { dst: 9, src: 0 }.into(),
            Uop::Kimm {
                dst: 10,
                kidx: k(8),
            }
            .into(),
            Uop::Call {
                callee: k(1),
                nargs: 2,
            }
            .into(),
            // the closure: the string capture staged at overlay word 0
            Uop::Kstr { dst: 8, kidx: k(5) }.into(),
            Uop::Mov { dst: 9, src: 8 }.into(),
            Uop::Kconst { dst: 7, kidx: k(3) }.into(),
            Uop::ClosureNew {
                dst: 6,
                func: 7,
                n: 1,
            }
            .into(),
            // par_map: (list, callable) plus the two evidence words
            Uop::Mov { dst: 9, src: 0 }.into(),
            Uop::Mov { dst: 10, src: 6 }.into(),
            Uop::Kimm {
                dst: 11,
                kidx: k(4),
            }
            .into(),
            Uop::Kimm {
                dst: 12,
                kidx: k(4),
            }
            .into(),
            Uop::Callg {
                callee: k(2),
                nargs: 2,
            }
            .into(),
            Uop::Mov { dst: 1, src: 9 }.into(),
            // the closure word and the result list release before the
            // answer rides out
            RcOp::Dec { src: 6 }.into(),
            RcOp::Dec { src: 1 }.into(),
            Uop::Ret { src: 8 }.into(),
        ],
    )
}

/// `() -> int`: forks `[7, 0, 5]` through the string-capturing guard
/// closure — element 0 lands, element 1 dies trapped at the division
/// raising the flag, element 2 is cut at its first taken backward
/// branch; the trap surfaces at the boundary with the two children's
/// captured copies released (the death walk and the cancel door's
/// drain).
fn closure_failure_def() -> FunctionDef {
    def_inst(
        "main_closure_failure",
        &[
            Ref, Raw, Raw, Raw, Raw, Raw, Raw, Raw, Ref, Raw, Raw, Raw, Raw,
        ],
        0,
        4,
        vec![
            builtin(BUILTIN_LIST_NEW),                // k0
            builtin(BUILTIN_LIST_PUSH),               // k1
            builtin(BUILTIN_PAR_MAP),                 // k2
            pm_ref(25),                               // k3 capturing_guard's header
            Const::I64(BuiltinInstance::List as i64), // k4
            Const::Str("shared".into()),              // k5
            Const::I64(7),                            // k6
            Const::I64(0),                            // k7
            Const::I64(5),                            // k8
        ],
        &[
            Uop::Funcf { frame_slots: 13 }.into(),
            Uop::Call {
                callee: k(0),
                nargs: 0,
            }
            .into(),
            Uop::Mov { dst: 0, src: 9 }.into(),
            // the input list [7, 0, 5]
            Uop::Mov { dst: 9, src: 0 }.into(),
            Uop::Kimm {
                dst: 10,
                kidx: k(6),
            }
            .into(),
            Uop::Call {
                callee: k(1),
                nargs: 2,
            }
            .into(),
            Uop::Mov { dst: 9, src: 0 }.into(),
            Uop::Kimm {
                dst: 10,
                kidx: k(7),
            }
            .into(),
            Uop::Call {
                callee: k(1),
                nargs: 2,
            }
            .into(),
            Uop::Mov { dst: 9, src: 0 }.into(),
            Uop::Kimm {
                dst: 10,
                kidx: k(8),
            }
            .into(),
            Uop::Call {
                callee: k(1),
                nargs: 2,
            }
            .into(),
            // the closure over the same interned text
            Uop::Kstr { dst: 8, kidx: k(5) }.into(),
            Uop::Mov { dst: 9, src: 8 }.into(),
            Uop::Kconst { dst: 7, kidx: k(3) }.into(),
            Uop::ClosureNew {
                dst: 6,
                func: 7,
                n: 1,
            }
            .into(),
            Uop::Mov { dst: 9, src: 0 }.into(),
            Uop::Mov { dst: 10, src: 6 }.into(),
            Uop::Kimm {
                dst: 11,
                kidx: k(4),
            }
            .into(),
            Uop::Kimm {
                dst: 12,
                kidx: k(4),
            }
            .into(),
            Uop::Callg {
                callee: k(2),
                nargs: 2,
            }
            .into(),
            Uop::Kimm { dst: 0, kidx: k(7) }.into(),
            Uop::Ret { src: 0 }.into(),
        ],
    )
}

/// The fixture module. Function indices are pool-FuncRef contract:
/// 0 main_order, 1 main_yield, 2 main_untouched, 3 main_first_failure,
/// 4 main_nested, 5 main_bad_callable, 6 main_empty, 7 doubles,
/// 8 yielding_doubles, 9 guard, 10 nested_fn, 11 main_wide_order,
/// 12 main_wide_yield, 13 main_wide_untouched, 14 main_ref_span,
/// 15 main_wide_failure, 16 main_ragged, 17 wide_pairs,
/// 18 yielding_wide_pairs, 19 wordy, 20 wide_guard, 21 ragged,
/// 22 biased, 23 wide_capture, 24 string_capture, 25 capturing_guard,
/// 26 main_closure_order, 27 main_wide_closure, 28 main_ref_balance,
/// 29 main_kstr_probe, 30 main_closure_failure.
fn pm_registry() -> ModuleRegistry {
    let mut reg = ModuleRegistry::new();
    // indices 31 nested_fail_fn, 32 main_nested_fail, 33 main_tail_fork
    reg.install(
        ModuleVersion::new("pm", vec![
            par_main_def("main_order", &[1, 2, 3, 4, 5], 7, false),
            par_main_def("main_yield", &[1, 2, 3, 4, 5], 8, false),
            par_main_def("main_untouched", &[10, 20, 30], 7, true),
            par_main_def("main_first_failure", &[0, 5], 9, false),
            par_main_def("main_nested", &[7], 10, false),
            bad_callable_def(),
            empty_def(),
            doubles_def(),
            yielding_doubles_def(),
            guard_def(),
            nested_fn_def(),
            wide_main_def("main_wide_order", &[1, 2, 3, 4, 5], 17, false),
            wide_main_def("main_wide_yield", &[1, 2, 3, 4, 5], 18, false),
            wide_main_def("main_wide_untouched", &[1, 2, 3, 4, 5], 17, true),
            fork_main_def("main_ref_span", &[1, 2, 3], 19),
            fork_main_def("main_wide_failure", &[7, 0, 5], 20),
            fork_main_def("main_ragged", &[4, 1, 4], 21),
            wide_pairs_def(),
            yielding_wide_pairs_def(),
            wordy_def(),
            wide_guard_def(),
            ragged_def(),
            biased_def(),
            wide_capture_def(),
            string_capture_def(),
            capturing_guard_def(),
            closure_main_def("main_closure_order", &[1, 2, 3, 4, 5], 22, 10, false),
            closure_main_def("main_wide_closure", &[1, 2, 3, 4, 5], 23, 10, true),
            ref_balance_def(),
            kstr_probe_def(),
            closure_failure_def(),
            nested_fail_fn_def(),
            par_main_def("main_nested_fail", &[5], 31, false),
            tail_fork_def(),
        ])
        .expect("valid fixture module"),
    );
    reg
}

/// Every child the par_map spawned is dead after the boundary — the
/// drive consumed them all whatever the outcome.
fn assert_children_dead(vm: &Vm, count: u32) {
    for id in 1..=count {
        assert_eq!(
            vm.fiber_state(FiberId(id)),
            Some(FiberState::Dead),
            "child {id} must be dead at the boundary"
        );
    }
}

#[test]
fn answers_assemble_in_input_order() {
    let mut vm = Vm::new(pm_registry());
    // [1, 2, 3, 4, 5] doubled: first 2, last 10, len 5 -> 2 * 1000 + 10 * 10 + 5
    assert_eq!(vm.run("pm", "main_order", &[]), Ok(Value::I64(2105)));
    assert_children_dead(&vm, 5);
}

#[test]
fn a_yielding_element_lands_in_place_across_ten_runs() {
    // each element parks at its yield mid-flight and is bounced back in
    // by the worklist; the assembly order is spawn order regardless
    for _ in 0..10 {
        let mut vm = Vm::new(pm_registry());
        assert_eq!(vm.run("pm", "main_yield", &[]), Ok(Value::I64(2105)));
        assert_children_dead(&vm, 5);
    }
}

#[test]
fn the_input_list_reads_back_untouched() {
    let mut vm = Vm::new(pm_registry());
    // input sum 60 * 100000 + (20 * 1000 + 60 * 10 + 3)
    assert_eq!(
        vm.run("pm", "main_untouched", &[]),
        Ok(Value::I64(6_020_603))
    );
    assert_children_dead(&vm, 3);
}

#[test]
fn first_failure_cancels_the_sibling_and_surfaces() {
    let mut vm = Vm::new(pm_registry());
    // element 0 dies trapped at the division; element 1 counts down and
    // is cut at its first taken backward branch under the raised flag
    assert_eq!(
        vm.run("pm", "main_first_failure", &[]),
        Err(Trap::DivideByZero)
    );
    assert_children_dead(&vm, 2);
}

#[test]
fn a_nested_par_map_lands_through_the_parked_call_site() {
    let mut vm = Vm::new(pm_registry());
    // the element's own par_map parks its fiber at its CALLG site — the
    // drive frames stacking on the interlock's heap stack — and the
    // inner answers assemble identically to the hosted equivalent:
    // doubles applied inside (7 -> 14), the outer checksum reading it
    assert_eq!(vm.run("pm", "main_nested", &[]), Ok(Value::I64(14_141)));
    // the outer child plus the inner child are both dead at the boundary
    assert_children_dead(&vm, 2);
}

#[test]
fn a_nested_par_map_failure_re_raises_at_the_outer_boundary() {
    let mut vm = Vm::new(pm_registry());
    // the inner element traps at its division; the inner seam raises at
    // the element's parked CALLG site, the element fiber's own trap
    // ledgers into the outer nursery and raises its flag, and the outer
    // seam re-raises at the main call site exactly like any element
    // failure — both children dead at the boundary, no live stragglers
    assert_eq!(
        vm.run("pm", "main_nested_fail", &[]),
        Err(Trap::DivideByZero)
    );
    assert_children_dead(&vm, 2);
}

#[test]
fn nested_fork_joins_run_past_the_interlocks_own_depth_bar() {
    // the previously-refused shape at scale: 2048 lexically nested
    // par_map levels, each element forking the level below — every
    // level parks its CALLG site, the drive frames stacking on the
    // interlock's heap stack (the naive unlock aborted the process
    // stack near lexical depth 800), and the depth-2000 bar the
    // in-language joins hold covers the fork seam too
    let depth = 2048u32;
    let mut fns = vec![deep_base_def()];
    for i in 1..=depth {
        fns.push(deep_level_def(
            &format!("level_{i}"),
            Const::FuncRef(ConstFuncRef::Named {
                module: "dp".into(),
                func: i - 1,
            }),
        ));
    }
    let mut reg = ModuleRegistry::new();
    reg.install(ModuleVersion::new("dp", fns).expect("valid deep fixture"));
    let mut vm = Vm::new(reg);
    let entry = format!("level_{depth}");
    // level_i(0) == i: every level's plus-one landed in nesting order
    assert_eq!(
        vm.run("dp", &entry, &[Value::I64(0)]),
        Ok(Value::I64(i64::from(depth)))
    );
    for id in 1..=depth {
        assert_eq!(
            vm.fiber_state(FiberId(id)),
            Some(FiberState::Dead),
            "child {id} must be dead at the boundary"
        );
    }
}

#[test]
fn a_fork_join_at_a_tail_dispatch_site_refuses() {
    let mut vm = Vm::new(pm_registry());
    // the tail call would return the reused frame at the call, but the
    // join's answer arrives through the drive's delivery onto a parked
    // site — the fork refuses at the dispatch, before its spawn phase
    // runs (no child ever stands)
    assert_eq!(vm.run("pm", "main_tail_fork", &[]), Err(Trap::TailForkJoin));
    assert_children_dead(&vm, 0);
}

#[test]
fn a_non_closure_ref_word_refuses_at_the_spawn() {
    let mut vm = Vm::new(pm_registry());
    // the callable-defaulted `Ref` spelling is a closure word now, so a
    // value-site `Ref` (the input list's word, hand-staged against the
    // callable slot) names no closure object and refuses at the spawn —
    // the honest corruption trap, the callable's own arity or type
    // checks never reached
    assert_eq!(
        vm.run("pm", "main_bad_callable", &[]),
        Err(Trap::UnknownObject { obj: 0 })
    );
}

#[test]
fn an_empty_input_answers_the_empty_list() {
    let mut vm = Vm::new(pm_registry());
    assert_eq!(vm.run("pm", "main_empty", &[]), Ok(Value::I64(0)));
}

#[test]
fn wide_answers_assemble_in_input_order() {
    let mut vm = Vm::new(pm_registry());
    // [1, 2, 3, 4, 5] through the span-answering callable: first the
    // pair (2, 3), last the pair (10, 15), len 5
    // -> 2 * 100000 + 3 * 10000 + 10 * 100 + 15 * 10 + 5
    assert_eq!(
        vm.run("pm", "main_wide_order", &[]),
        Ok(Value::I64(231_155))
    );
    assert_children_dead(&vm, 5);
}

#[test]
fn a_yielding_element_lands_its_wide_answer_in_place_across_ten_runs() {
    // each element parks at its yield mid-flight and is bounced back in
    // by the worklist; the wide assembly order is spawn order regardless
    for _ in 0..10 {
        let mut vm = Vm::new(pm_registry());
        assert_eq!(
            vm.run("pm", "main_wide_yield", &[]),
            Ok(Value::I64(231_155))
        );
        assert_children_dead(&vm, 5);
    }
}

#[test]
fn the_wide_input_list_reads_back_untouched() {
    let mut vm = Vm::new(pm_registry());
    // input sum 15 * 1000000 + 231155
    assert_eq!(
        vm.run("pm", "main_wide_untouched", &[]),
        Ok(Value::I64(15_231_155))
    );
    assert_children_dead(&vm, 5);
}

#[test]
fn the_completion_span_ref_words_balance_at_the_join() {
    let mut vm = Vm::new(pm_registry());
    // each child answers [x*10, str_int(x)]: the span's `Ref` word
    // carries its birth count across the completion, the join's push
    // takes the list's own edge count, and the joiner's single dec
    // lands each string at exactly the result list's edge — the host
    // holds the list, so every count is observable here
    let answer = vm.run("pm", "main_ref_span", &[]);
    let Ok(Value::Ref(res)) = answer else {
        panic!("the ref fork answers the result list word");
    };
    let store = vm.objects_of(res);
    let elems = store.list_at(res.id()).expect("the result list reads");
    assert_eq!(elems.len(), 6, "three two-word answers");
    // the elements assembled in input order, each pair an int word then
    // the fresh string's Ref word; the strings live on the dead
    // children's own stores, so each probe routes through its word
    for (at, text, int_half) in [(1usize, "1", 10i64), (3, "2", 20), (5, "3", 30)] {
        let (Value::I64(seen_int), Value::Ref(id)) = (&elems[at - 1], &elems[at]) else {
            panic!("the pair ending at word {at} is an int then a Ref word");
        };
        assert_eq!(*seen_int, int_half);
        let child_store = vm.objects_of(*id);
        assert_eq!(
            child_store.rc_at(id.id()),
            Ok(1),
            "one count: the list's edge"
        );
        assert_eq!(child_store.str_at(id.id()), Ok(text));
    }
    assert_eq!(store.rc_at(res.id()), Ok(1), "the host-held list");
    assert_children_dead(&vm, 3);
}

#[test]
fn a_wider_outstanding_answer_survives_the_first_failure_cancel() {
    let mut vm = Vm::new(pm_registry());
    // element 0 (x=7) lands its (int, string) answer before the failure;
    // element 1 (x=0) dies trapped at the division raising the flag;
    // element 2 (x=5) counts down and is cut at its first taken backward
    // branch; the outstanding answer's words release with the result
    // list's dec, and the trap re-raises at the call site
    assert_eq!(
        vm.run("pm", "main_wide_failure", &[]),
        Err(Trap::DivideByZero)
    );
    assert_children_dead(&vm, 3);
}

#[test]
fn a_ragged_answering_width_traps_at_the_stride_check() {
    let mut vm = Vm::new(pm_registry());
    // the callable answers width 2 for x > 3 and one word otherwise — a
    // width the checker never proves; the second push's stride check
    // traps instead of misaligning the payload
    let Err(trap) = vm.run("pm", "main_ragged", &[]) else {
        panic!("the ragged fork must trap");
    };
    assert!(matches!(trap, Trap::ElementStride {
        stride: 2,
        given: 1,
        ..
    }));
    assert_children_dead(&vm, 3);
}

#[test]
fn a_capturing_closure_forks_and_assembles_in_input_order() {
    let mut vm = Vm::new(pm_registry());
    // [1, 2, 3, 4, 5] through the closure capturing bias 10: the
    // answers x*2 + 10 assemble in input order — first 12, last 20,
    // len 5 -> 12 * 1000 + 20 * 10 + 5
    assert_eq!(
        vm.run("pm", "main_closure_order", &[]),
        Ok(Value::I64(12_205))
    );
    assert_children_dead(&vm, 5);
}

#[test]
fn a_capturing_closure_answers_wide_spans_in_input_order() {
    let mut vm = Vm::new(pm_registry());
    // the closure's body answers the two-word span [x*2 + 10, x*3]:
    // first the pair (12, 3), last the pair (20, 15), len 5
    // -> 12 * 100000 + 3 * 10000 + 20 * 100 + 15 * 10 + 5
    assert_eq!(
        vm.run("pm", "main_wide_closure", &[]),
        Ok(Value::I64(1_232_155))
    );
    assert_children_dead(&vm, 5);
}

#[test]
fn the_captured_ref_words_balance_at_the_join() {
    let mut vm = Vm::new(pm_registry());
    // the probe hands the interned word out first: count one, host-held
    let Ok(Value::Ref(id)) = vm.run("pm", "main_kstr_probe", &[]) else {
        panic!("the probe answers the interned word");
    };
    assert_eq!(vm.objects_of(id).rc_at(id.id()), Ok(1));
    // the balance main: the closure captures the word (one edge count),
    // three children fork through it — each copy-in acquires one count
    // claimed for the child, each child's own exit dec releases it —
    // and the closure word and result list release before the answer
    // rides out. The count reads exactly the host-held one
    let Ok(Value::Ref(seen)) = vm.run("pm", "main_ref_balance", &[]) else {
        panic!("the balance main answers the captured word");
    };
    assert_eq!(seen, id, "the interned text names one word");
    // exactly two counts: the probe's host-held one plus the balance
    // main's delivered word (the host never releases) — a stranded
    // child copy would read three, an over-release one
    assert_eq!(vm.objects_of(id).rc_at(id.id()), Ok(2));
    assert_children_dead(&vm, 3);
}

#[test]
fn a_first_failure_releases_the_outstanding_captured_words() {
    let mut vm = Vm::new(pm_registry());
    // the probe holds the word: count one
    let Ok(Value::Ref(id)) = vm.run("pm", "main_kstr_probe", &[]) else {
        panic!("the probe answers the interned word");
    };
    assert_eq!(vm.objects_of(id).rc_at(id.id()), Ok(1));
    // element 0 lands (its exit dec ran); element 1 dies trapped at the
    // division — its captured copy releases at the death walk; element 2
    // is cut at its first taken backward branch — its copy releases at
    // the cancel door's drain; the trap re-raises at the call site
    assert_eq!(
        vm.run("pm", "main_closure_failure", &[]),
        Err(Trap::DivideByZero)
    );
    assert_children_dead(&vm, 3);
    // the trapped root drive kept its frames (the Kstr claim and the
    // closure word's birth count among them); a fresh entry's reset
    // drains them — the closure frees, its capture edge decs, and the
    // count reads exactly the host-held one: no child copy stranded
    assert_eq!(vm.run("pm", "main_empty", &[]), Ok(Value::I64(0)));
    assert_eq!(vm.objects_of(id).rc_at(id.id()), Ok(1));
}
