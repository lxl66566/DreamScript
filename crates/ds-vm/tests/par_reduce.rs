//! The `par_reduce` fork-join fold pins (the parked-CALLG slice):
//! the fold's left data dependency chains one spawned child per element
//! (the callable's own function entered with the running accumulator and
//! the element, the seam's chain state carrying the fold between
//! steps), so the reduction applies strictly in input order. What the
//! fixtures pin:
//!
//! - order determinism: the non-commutative fold `acc * 10 + b` over `[1, 2, 3, 4, 5]` answers
//!   `12345`, and a callable that parks mid-element bounces back in and still folds in place — ten
//!   fresh runs answer the identical fold.
//! - the input list is untouched: its elements read back identical after the chain.
//! - the accumulator's `Ref` counts follow `reduce`'s owned discipline (a string fold through the
//!   native `concat`): the init stays borrowed until the first delivered answer adopts, each
//!   replaced word dec'd at adoption, and the heap drains at the end.
//! - first failure: the failing element's trap re-raises at the par_reduce call site with the owned
//!   accumulator released (the step's nursery consumed by its join — the chain holds no live
//!   sibling at a failure).
//! - the nested shape: a par_reduce inside a par_reduce element (and an inner par_map inside a
//!   par_reduce element — the parked CALLG seam is the shared mechanism, not a per-combinator fact)
//!   parks the element's fiber at its own CALLG site, the drive frames stacking on the interlock's
//!   heap stack, and the inner fold lands identically to the hosted equivalent.
//! - the wider fold: the init's whole word span stages between the list and the callable at any
//!   width, each element callable answering the next accumulator whole — a capturing closure
//!   folding pairs in input order, a mixed (int, string) fold balancing each replaced string's
//!   count at adoption, and a failing step re-raising at the call site with the owned span
//!   released.
//! - the callable backstop: a staged `Ref` word against the callable slot refuses (the fork spawns
//!   a closure body); an answer width disagreeing with the staged init's refuses at the adoption;
//!   an empty input answers the init.
//!
//! Layout conventions (as everywhere): overlay slots that stage arguments and then receive
//! results declare `Raw`; children spawn during the CALLG, their pool ids numbering up from 1.

use ds_vm::{
    Const, ConstFuncRef, FiberId, FiberState, FrameAnswer, FunctionDef, ModuleRegistry,
    ModuleVersion, Trap, Value, Vm,
    abi::{
        BuiltinInstance, ConstIdx, FrameLayout, SlotType,
        SlotType::{Bool, I64, Raw, Ref},
        uop::Uop,
    },
    assemble,
    native::{
        BUILTIN_CONCAT, BUILTIN_LIST_GET, BUILTIN_LIST_NEW, BUILTIN_LIST_PUSH, BUILTIN_PAR_MAP,
        BUILTIN_PAR_REDUCE,
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

/// A same-module callee by function-table index (the fixture module "pr").
fn pr_ref(index: u32) -> Const {
    Const::FuncRef(ConstFuncRef::Named {
        module: "pr".into(),
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
const CALLABLE: u16 = 4;
/// The List instance evidence word's pool index.
const LIST_EV: u16 = 5;
/// The overlay base of the generated mains: body slots 0..7, overlay
/// words 7..11.
const OV: u8 = 7;

/// One par_reduce main: build `elems` by `list_push`, fold them through
/// `callable` from `init`, then answer the fold (with `recheck_input`,
/// the input list's elements read back after the chain and the answer
/// becomes `input_sum * 100000 + fold` — the untouched-input pin).
fn pr_main_def(
    name: &str,
    elems: &[i64],
    init: i64,
    callable: u32,
    recheck_input: bool,
) -> FunctionDef {
    // k0 list_new, k1 list_push, k2 par_reduce, k3 list_get, k4 the
    // callable, k5 the List evidence; element words, the init, read
    // indexes, and weights follow
    let mut pool = vec![
        builtin(BUILTIN_LIST_NEW),
        builtin(BUILTIN_LIST_PUSH),
        builtin(BUILTIN_PAR_REDUCE),
        builtin(BUILTIN_LIST_GET),
        pr_ref(callable),
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
    // par_reduce: (list, init, callable) plus the one List evidence word
    let init_word = word_at(&mut pool, init);
    body.push(Uop::Mov { dst: OV, src: 0 });
    body.push(Uop::Kimm {
        dst: OV + 1,
        kidx: init_word,
    });
    body.push(Uop::Kconst {
        dst: OV + 2,
        kidx: k(CALLABLE),
    });
    body.push(Uop::Kimm {
        dst: OV + 3,
        kidx: k(LIST_EV),
    });
    body.push(Uop::Callg {
        callee: k(2),
        nargs: 3,
    });
    body.push(Uop::Mov { dst: 1, src: OV }); // the fold
    if !recheck_input {
        body.push(Uop::Ret { src: 1 });
        return def(
            name,
            &[Ref, I64, Raw, Raw, I64, I64, I64, Raw, Raw, Raw, Raw],
            0,
            4,
            pool,
            &body,
        );
    }
    // the input list reads back whole: every element again via
    // list_get, summed into body slot 5 — final = input_sum * 100000 + fold
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
            callee: k(3),
            nargs: 2,
        });
        body.push(Uop::AddI64 {
            dst: 5,
            lhs: 5,
            rhs: OV + 1,
        });
    }
    body.push(Uop::Kimm {
        dst: 6,
        kidx: hundred_thousand,
    });
    body.push(Uop::MulI64 {
        dst: 5,
        lhs: 5,
        rhs: 6,
    });
    body.push(Uop::AddI64 {
        dst: 6,
        lhs: 5,
        rhs: 1,
    });
    body.push(Uop::Ret { src: 6 });
    def(
        name,
        &[Ref, I64, Raw, Raw, I64, I64, I64, Raw, Raw, Raw, Raw],
        0,
        4,
        pool,
        &body,
    )
}

/// `(acc: int, b: int) -> int`: acc * 10 + b — the non-commutative fold
/// whose answer witnesses the input order.
fn shift10_def() -> FunctionDef {
    def(
        "shift10",
        &[I64, I64, I64, I64],
        2,
        0,
        vec![Const::I64(10)],
        &[
            Uop::Funcf { frame_slots: 4 },
            Uop::Kimm { dst: 2, kidx: k(0) },
            Uop::MulI64 {
                dst: 3,
                lhs: 0,
                rhs: 2,
            },
            Uop::AddI64 {
                dst: 0,
                lhs: 3,
                rhs: 1,
            },
            Uop::Ret { src: 0 },
        ],
    )
}

/// `(acc: int, b: int) -> int`: parks at its one-shot backward branch
/// mid-element (the bounce answer is dropped — no call site stands), then
/// answers acc * 10 + b — the out-of-order-completion stand-in the hosted
/// `reduce` loop refuses.
fn parking_shift10_def() -> FunctionDef {
    // pool k0=one, k1=two, k2=zero, k3=10
    def(
        "parking_shift10",
        &[I64, I64, Bool, I64, I64],
        2,
        0,
        vec![Const::I64(1), Const::I64(2), Const::I64(0), Const::I64(10)],
        &[
            Uop::Funcf { frame_slots: 5 },
            Uop::Kimm { dst: 4, kidx: k(2) }, // zero the counter
            Uop::Kimm { dst: 3, kidx: k(0) }, // one
            Uop::AddI64 {
                dst: 4,
                lhs: 4,
                rhs: 3,
            },
            Uop::Kimm { dst: 3, kidx: k(1) }, // two
            Uop::LtI64 {
                dst: 2,
                lhs: 4,
                rhs: 3,
            },
            Uop::Jmpif { cond: 2, target: 3 }, // the one-shot park point
            Uop::Kimm { dst: 3, kidx: k(3) },
            Uop::MulI64 {
                dst: 4,
                lhs: 0,
                rhs: 3,
            },
            Uop::AddI64 {
                dst: 0,
                lhs: 4,
                rhs: 1,
            },
            Uop::Ret { src: 0 },
        ],
    )
}

/// `(acc: int, b: int) -> int`: b == 0 dies trapped at the division;
/// otherwise it counts b down through a taken backward branch — the
/// preemption point a cancelled drive would observe.
fn guard2_def() -> FunctionDef {
    def(
        "guard2",
        &[I64, I64, I64, I64, Bool, I64],
        2,
        0,
        vec![Const::I64(0), Const::I64(1)],
        &[
            Uop::Funcf { frame_slots: 6 },
            Uop::Kimm { dst: 2, kidx: k(0) }, // 0
            Uop::Kimm { dst: 3, kidx: k(1) }, // 1
            Uop::EqI64 {
                dst: 4,
                lhs: 1,
                rhs: 2,
            },
            Uop::Jmpif {
                cond: 4,
                target: 11,
            }, // b == 0 -> the trap site
            Uop::GtI64 {
                dst: 4,
                lhs: 1,
                rhs: 2,
            },
            Uop::JmpifNot {
                cond: 4,
                target: 10,
            },
            Uop::SubI64 {
                dst: 5,
                lhs: 1,
                rhs: 3,
            },
            Uop::Mov { dst: 1, src: 5 },
            Uop::Jmp { target: 5 }, // the taken backward branch
            Uop::Ret { src: 1 },    // 0
            Uop::DivI64 {
                dst: 5,
                lhs: 3,
                rhs: 2,
            }, // 1 / 0
            Uop::Ret { src: 5 },
        ],
    )
}

/// `(acc: int, b: int) -> int`: acc + b — the inner callable the nested
/// fixture's own par_reduce folds with.
fn add2_def() -> FunctionDef {
    def("add2", &[I64, I64], 2, 0, vec![], &[
        Uop::Funcf { frame_slots: 2 },
        Uop::AddI64 {
            dst: 0,
            lhs: 0,
            rhs: 1,
        },
        Uop::Ret { src: 0 },
    ])
}

/// `(acc: int, b: int) -> int`: folds `[b]` through `add2` — the nested
/// par_reduce whose join the interlock refuses.
fn nested_fn_def() -> FunctionDef {
    // body slots 0..3, overlay words 4..9
    def(
        "nested_fn",
        &[I64, I64, Ref, I64, Raw, Raw, Raw, Raw, Raw, Raw],
        2,
        6,
        vec![
            builtin(BUILTIN_LIST_NEW),                // k0
            builtin(BUILTIN_LIST_PUSH),               // k1
            builtin(BUILTIN_PAR_REDUCE),              // k2
            pr_ref(13),                               // k3 add2
            Const::I64(0),                            // k4 the inner init
            Const::I64(BuiltinInstance::List as i64), // k5
        ],
        &[
            Uop::Funcf { frame_slots: 10 },
            Uop::Call {
                callee: k(0),
                nargs: 0,
            },
            Uop::Mov { dst: 2, src: 4 }, // the fresh inner list
            Uop::Mov { dst: 4, src: 2 },
            Uop::Mov { dst: 5, src: 1 },
            Uop::Call {
                callee: k(1),
                nargs: 2,
            },
            Uop::Mov { dst: 4, src: 2 },        // list
            Uop::Kimm { dst: 5, kidx: k(4) },   // init 0
            Uop::Kconst { dst: 6, kidx: k(3) }, // add2
            Uop::Kimm { dst: 7, kidx: k(5) },   // evidence
            Uop::Callg {
                callee: k(2),
                nargs: 3,
            }, // the refused join traps here
            Uop::Mov { dst: 3, src: 4 },
            Uop::Ret { src: 3 },
        ],
    )
}

/// `(a: string, b: string) -> string`: the native `concat` — the `Ref`
/// fold whose accumulator counts follow `reduce`'s owned discipline.
fn strcat2_def() -> FunctionDef {
    // body slots 0..3, overlay words 2..4
    def(
        "strcat2",
        &[Ref, Ref, Raw, Raw],
        2,
        2,
        vec![builtin(BUILTIN_CONCAT)],
        &[
            Uop::Funcf { frame_slots: 4 },
            Uop::Mov { dst: 2, src: 0 },
            Uop::Mov { dst: 3, src: 1 },
            Uop::Call {
                callee: k(0),
                nargs: 2,
            },
            Uop::Ret { src: 2 }, // the answer rides overlay word 0
        ],
    )
}

/// `() -> int`: par_reduce over `["1", "2", "3"]` from the empty string
/// through `strcat2` — the string chain answers "123".
fn ref_chain_def() -> FunctionDef {
    // body slots 0..2, overlay words 2..7
    def(
        "main_ref_chain",
        &[Ref, Ref, Raw, Raw, Raw, Raw, Raw],
        0,
        5,
        vec![
            builtin(BUILTIN_LIST_NEW),                // k0
            builtin(BUILTIN_LIST_PUSH),               // k1
            builtin(BUILTIN_PAR_REDUCE),              // k2
            pr_ref(14),                               // k3 strcat2
            Const::Str("1".into()),                   // k4
            Const::Str("2".into()),                   // k5
            Const::Str("3".into()),                   // k6
            Const::Str(String::new()),                // k7 the init
            Const::I64(BuiltinInstance::List as i64), // k8
        ],
        &[
            Uop::Funcf { frame_slots: 7 },
            Uop::Call {
                callee: k(0),
                nargs: 0,
            },
            Uop::Mov { dst: 0, src: 2 },
            Uop::Mov { dst: 2, src: 0 },
            Uop::Kstr { dst: 3, kidx: k(4) },
            Uop::Call {
                callee: k(1),
                nargs: 2,
            },
            Uop::Mov { dst: 2, src: 0 },
            Uop::Kstr { dst: 3, kidx: k(5) },
            Uop::Call {
                callee: k(1),
                nargs: 2,
            },
            Uop::Mov { dst: 2, src: 0 },
            Uop::Kstr { dst: 3, kidx: k(6) },
            Uop::Call {
                callee: k(1),
                nargs: 2,
            },
            Uop::Mov { dst: 2, src: 0 },        // list
            Uop::Kstr { dst: 3, kidx: k(7) },   // init ""
            Uop::Kconst { dst: 4, kidx: k(3) }, // strcat2
            Uop::Kimm { dst: 5, kidx: k(8) },   // evidence
            Uop::Callg {
                callee: k(2),
                nargs: 3,
            },
            Uop::Mov { dst: 1, src: 2 },
            Uop::Ret { src: 1 },
        ],
    )
}

/// `() -> int`: par_reduce over `[1]` staging the list word against the
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
            builtin(BUILTIN_PAR_REDUCE),              // k2
            Const::I64(1),                            // k3 the element
            Const::I64(7),                            // k4 the init
            Const::I64(BuiltinInstance::List as i64), // k5
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
            Uop::Mov { dst: 1, src: 0 },      // ov0 = the list
            Uop::Kimm { dst: 2, kidx: k(4) }, // ov1 = the init
            Uop::Mov { dst: 3, src: 0 },      // ov2 = the list word as the callable
            Uop::Kimm { dst: 4, kidx: k(5) },
            Uop::Callg {
                callee: k(2),
                nargs: 3,
            },
            Uop::Ret { src: 1 },
        ],
    )
}

/// `() -> int`: par_reduce staging a two-word init (nargs 4) with the
/// one-word-answerer `pick3` as the callable — its entry parameters
/// cover the two-word init plus the element, so the spawn passes and
/// the delivered answer's width is the hand-assembled seam.
fn wide_interior_def() -> FunctionDef {
    // body slot 0, overlay words 1..6
    def(
        "main_wide_interior",
        &[Ref, Raw, Raw, Raw, Raw, Raw],
        0,
        5,
        vec![
            builtin(BUILTIN_LIST_NEW),                // k0
            builtin(BUILTIN_LIST_PUSH),               // k1
            builtin(BUILTIN_PAR_REDUCE),              // k2
            Const::I64(1),                            // k3 the element
            Const::I64(7),                            // k4 interior word 1
            Const::I64(8),                            // k5 interior word 2
            Const::I64(BuiltinInstance::List as i64), // k6
            pr_ref(22),                               // k7 pick3
        ],
        &[
            Uop::Funcf { frame_slots: 6 },
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
            Uop::Mov { dst: 1, src: 0 },        // ov0 = the list
            Uop::Kimm { dst: 2, kidx: k(4) },   // ov1, ov2 = the two
            Uop::Kimm { dst: 3, kidx: k(5) },   // interior words
            Uop::Kconst { dst: 4, kidx: k(7) }, // ov3 = shift10 as the callable
            Uop::Kimm { dst: 5, kidx: k(6) },
            Uop::Callg {
                callee: k(2),
                nargs: 4,
            },
            Uop::Ret { src: 1 },
        ],
    )
}

/// `() -> int`: par_reduce over the empty list answers the init; `7`
/// reads back.
fn empty_def() -> FunctionDef {
    // body slots 0..1, overlay words 1..5
    def(
        "main_empty",
        &[Ref, I64, Raw, Raw, Raw, Raw],
        0,
        4,
        vec![
            builtin(BUILTIN_LIST_NEW),                // k0
            builtin(BUILTIN_PAR_REDUCE),              // k1
            pr_ref(9),                                // k2 shift10
            Const::I64(7),                            // k3 the init
            Const::I64(BuiltinInstance::List as i64), // k4
        ],
        &[
            Uop::Funcf { frame_slots: 6 },
            Uop::Call {
                callee: k(0),
                nargs: 0,
            },
            Uop::Mov { dst: 0, src: 2 },
            Uop::Mov { dst: 2, src: 0 },      // the list
            Uop::Kimm { dst: 3, kidx: k(3) }, // the init
            Uop::Kconst { dst: 4, kidx: k(2) },
            Uop::Kimm { dst: 5, kidx: k(4) },
            Uop::Callg {
                callee: k(1),
                nargs: 3,
            },
            Uop::Mov { dst: 1, src: 2 },
            Uop::Ret { src: 1 },
        ],
    )
}

/// `(a: int, b: int) -> int` with one i64 capture at slot 2 (past the
/// two parameters): a * 10 + b + bias — the lambda form a fork stages
/// captures into.
fn biased_shift_def() -> FunctionDef {
    def(
        "biased_shift",
        &[I64, I64, I64, I64, I64],
        2,
        0,
        vec![Const::I64(10)],
        &[
            Uop::Funcf { frame_slots: 5 },
            Uop::Kimm { dst: 3, kidx: k(0) },
            Uop::MulI64 {
                dst: 4,
                lhs: 0,
                rhs: 3,
            },
            Uop::AddI64 {
                dst: 4,
                lhs: 4,
                rhs: 1,
            },
            Uop::AddI64 {
                dst: 4,
                lhs: 4,
                rhs: 2,
            },
            Uop::Ret { src: 4 },
        ],
    )
}

/// `() -> int`: folds `[1, 2, 3]` through the closure capturing bias 5
/// — each spawned child entered with the running accumulator and the
/// element, the capture block staged past them — so the fold applies
/// strictly in input order: 6, then 67, then 678.
fn closure_fold_def() -> FunctionDef {
    def(
        "main_closure_fold",
        &[Ref, Raw, Raw, I64, Raw, Raw, Raw, Raw, Raw],
        0,
        4,
        vec![
            builtin(BUILTIN_LIST_NEW),                // k0
            builtin(BUILTIN_LIST_PUSH),               // k1
            builtin(BUILTIN_PAR_REDUCE),              // k2
            pr_ref(15),                               // k3 biased_shift's header
            Const::I64(BuiltinInstance::List as i64), // k4
            Const::I64(1),                            // k5
            Const::I64(2),                            // k6
            Const::I64(3),                            // k7
            Const::I64(0),                            // k8 the init
            Const::I64(5),                            // k9 the bias capture
        ],
        &[
            Uop::Funcf { frame_slots: 9 },
            Uop::Call {
                callee: k(0),
                nargs: 0,
            },
            Uop::Mov { dst: 0, src: 5 },
            // the input list [1, 2, 3]
            Uop::Mov { dst: 5, src: 0 },
            Uop::Kimm { dst: 6, kidx: k(5) },
            Uop::Call {
                callee: k(1),
                nargs: 2,
            },
            Uop::Mov { dst: 5, src: 0 },
            Uop::Kimm { dst: 6, kidx: k(6) },
            Uop::Call {
                callee: k(1),
                nargs: 2,
            },
            Uop::Mov { dst: 5, src: 0 },
            Uop::Kimm { dst: 6, kidx: k(7) },
            Uop::Call {
                callee: k(1),
                nargs: 2,
            },
            // the closure: the bias capture staged at overlay word 0
            Uop::Kimm { dst: 5, kidx: k(9) },
            Uop::Kconst { dst: 1, kidx: k(3) },
            Uop::ClosureNew {
                dst: 2,
                func: 1,
                n: 1,
            },
            // par_reduce: (list, init, callable) plus the evidence word
            Uop::Mov { dst: 5, src: 0 },
            Uop::Kimm { dst: 6, kidx: k(8) },
            Uop::Mov { dst: 7, src: 2 },
            Uop::Kimm { dst: 8, kidx: k(4) },
            Uop::Callg {
                callee: k(2),
                nargs: 3,
            },
            Uop::Mov { dst: 3, src: 5 },
            Uop::Ret { src: 3 },
        ],
    )
}

/// `(acc: int, b: int) -> int`: builds `[b]`, forks it through the inner
/// `par_map` (doubles), reads the first answer back — the inner par_map
/// whose join the interlock refuses inside a par_reduce element (the
/// cross-combinator nesting: the boundary is the drive seam's shared
/// flag, not a per-combinator fact).
fn nested_map_fn_def() -> FunctionDef {
    // body slots 0..3, overlay words 4..9
    def(
        "nested_map_fn",
        &[I64, I64, Ref, I64, Raw, Raw, Raw, Raw, Raw, Raw],
        2,
        6,
        vec![
            builtin(BUILTIN_LIST_NEW),                // k0
            builtin(BUILTIN_LIST_PUSH),               // k1
            builtin(BUILTIN_PAR_MAP),                 // k2
            builtin(BUILTIN_LIST_GET),                // k3
            pr_ref(19),                               // k4 double1
            Const::I64(0),                            // k5 the read index
            Const::I64(BuiltinInstance::List as i64), // k6
        ],
        &[
            Uop::Funcf { frame_slots: 10 },
            Uop::Call {
                callee: k(0),
                nargs: 0,
            },
            Uop::Mov { dst: 2, src: 4 }, // the fresh inner list
            Uop::Mov { dst: 4, src: 2 },
            Uop::Mov { dst: 5, src: 1 }, // the element b
            Uop::Call {
                callee: k(1),
                nargs: 2,
            },
            Uop::Mov { dst: 4, src: 2 },        // the list
            Uop::Kconst { dst: 5, kidx: k(4) }, // the callable
            Uop::Kimm { dst: 6, kidx: k(6) },   // evidence
            Uop::Kimm { dst: 7, kidx: k(6) },   // evidence
            Uop::Callg {
                callee: k(2),
                nargs: 2,
            }, // the refused join traps here
            Uop::Mov { dst: 2, src: 4 },        // the result list
            Uop::Mov { dst: 4, src: 2 },
            Uop::Kimm { dst: 5, kidx: k(5) }, // index 0
            Uop::Kimm { dst: 6, kidx: k(6) },
            Uop::Callg {
                callee: k(3),
                nargs: 2,
            },
            Uop::Mov { dst: 3, src: 5 }, // the ok payload
            Uop::AddI64 {
                dst: 0,
                lhs: 0,
                rhs: 3,
            },
            Uop::Ret { src: 0 },
        ],
    )
}

/// `(x: int) -> int`: x * 2 — the inner par_map's per-element callable.
fn double1_def() -> FunctionDef {
    def("double1", &[I64, I64], 1, 0, vec![Const::I64(2)], &[
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

/// `(a0: int, a1: int, b: int) -> (int, int)` with one i64 capture at
/// slot 3 (past the three parameters): answers [a0 * 10 + b + bias,
/// a1 + b] — the two-word fold a fork chains, both words order
/// witnesses.
fn biased_pair_def() -> FunctionDef {
    def(
        "biased_pair",
        &[I64, I64, I64, I64, I64, I64, I64],
        3,
        0,
        vec![Const::I64(10)],
        &[
            Uop::Funcf { frame_slots: 7 },
            Uop::Kimm { dst: 4, kidx: k(0) },
            Uop::MulI64 {
                dst: 5,
                lhs: 0,
                rhs: 4,
            },
            Uop::AddI64 {
                dst: 5,
                lhs: 5,
                rhs: 2,
            },
            Uop::AddI64 {
                dst: 5,
                lhs: 5,
                rhs: 3,
            }, // + the capture's bias
            Uop::AddI64 {
                dst: 6,
                lhs: 1,
                rhs: 2,
            },
            Uop::RetSpan { src: 5, n: 2 },
        ],
    )
}

/// `(n: int, acc: string, s: string) -> (int, string)`: s == "" dies
/// trapped at the division; otherwise answers [n * 10 + 1, acc ++ s]
/// through the native concat — the mixed fold whose `Ref` word adopts
/// per step (each replaced string dec'd at adoption), the failure
/// shape after two adoptions.
fn wide_strcat_guard_def() -> FunctionDef {
    // body slots 0..6, overlay words 7..8
    def(
        "wide_strcat_guard",
        &[I64, Ref, Ref, I64, Ref, Ref, Bool, Raw, Raw],
        3,
        2,
        vec![
            builtin(BUILTIN_CONCAT),   // k0
            Const::Str(String::new()), // k1 the guard's right side
            Const::I64(10),            // k2
            Const::I64(1),             // k3
            Const::I64(0),             // k4
        ],
        &[
            Uop::Funcf { frame_slots: 9 },    // pc0
            Uop::Kstr { dst: 5, kidx: k(1) }, // pc1
            Uop::EqStr {
                dst: 6,
                lhs: 2,
                rhs: 5,
            }, // pc2 s == ""
            Uop::Jmpif {
                cond: 6,
                target: 13,
            }, // pc3 -> the trap site
            Uop::Mov { dst: 7, src: 1 },      // pc4 stage acc
            Uop::Mov { dst: 8, src: 2 },      // pc5 stage s
            Uop::Call {
                callee: k(0),
                nargs: 2,
            }, // pc6 the fresh string on ov0
            Uop::Mov { dst: 4, src: 7 },      // pc7
            Uop::Kimm { dst: 3, kidx: k(2) }, // pc8
            Uop::MulI64 {
                dst: 3,
                lhs: 0,
                rhs: 3,
            }, // pc9 n * 10
            Uop::Kimm { dst: 7, kidx: k(3) }, // pc10 scratch (staging done)
            Uop::AddI64 {
                dst: 3,
                lhs: 3,
                rhs: 7,
            }, // pc11 n * 10 + 1
            Uop::RetSpan { src: 3, n: 2 },    // pc12 [n * 10 + 1, the string]
            Uop::Kimm { dst: 7, kidx: k(4) }, // pc13 the trap site
            Uop::DivI64 {
                dst: 3,
                lhs: 7,
                rhs: 7,
            }, // pc14 0 / 0
            Uop::Ret { src: 3 },
        ],
    )
}

/// `(a0: int, a1: int, b: int) -> int`: a0 — the one-word answerer for
/// the width backstop (its entry parameters cover the two-word init
/// plus the element, its answer narrower than the fold).
fn pick3_def() -> FunctionDef {
    def("pick3", &[I64, I64, I64], 3, 0, vec![], &[
        Uop::Funcf { frame_slots: 3 },
        Uop::Ret { src: 0 },
    ])
}

/// `() -> int`: folds `[1, 2, 3]` from the pair init (0, 0) through the
/// closure capturing bias 5 and answering pairs — each spawned child
/// entered with the running accumulator's two words and the element,
/// the capture block staged past them — so both fold words apply
/// strictly in input order: (6, 1), then (67, 3), then (678, 6).
fn wide_closure_main_def() -> FunctionDef {
    // body slots 0..4, overlay words 5..10
    def(
        "main_wide_closure",
        &[Ref, Raw, Ref, I64, I64, Raw, Raw, Raw, Raw, Raw, Raw],
        0,
        6,
        vec![
            builtin(BUILTIN_LIST_NEW),                // k0
            builtin(BUILTIN_LIST_PUSH),               // k1
            builtin(BUILTIN_PAR_REDUCE),              // k2
            pr_ref(20),                               // k3 biased_pair's header
            Const::I64(BuiltinInstance::List as i64), // k4
            Const::I64(1),                            // k5
            Const::I64(2),                            // k6
            Const::I64(3),                            // k7
            Const::I64(0),                            // k8 the init (both words)
            Const::I64(5),                            // k9 the bias capture
            Const::I64(1000),                         // k10
        ],
        &[
            Uop::Funcf { frame_slots: 11 },
            Uop::Call {
                callee: k(0),
                nargs: 0,
            },
            Uop::Mov { dst: 0, src: 5 },
            Uop::Mov { dst: 5, src: 0 },
            Uop::Kimm { dst: 6, kidx: k(5) },
            Uop::Call {
                callee: k(1),
                nargs: 2,
            },
            Uop::Mov { dst: 5, src: 0 },
            Uop::Kimm { dst: 6, kidx: k(6) },
            Uop::Call {
                callee: k(1),
                nargs: 2,
            },
            Uop::Mov { dst: 5, src: 0 },
            Uop::Kimm { dst: 6, kidx: k(7) },
            Uop::Call {
                callee: k(1),
                nargs: 2,
            },
            // the closure: the bias capture staged at overlay word 0
            Uop::Kimm { dst: 5, kidx: k(9) },
            Uop::Kconst { dst: 1, kidx: k(3) },
            Uop::ClosureNew {
                dst: 2,
                func: 1,
                n: 1,
            },
            // par_reduce: (list, init 0, init 0, callable) plus the evidence word
            Uop::Mov { dst: 5, src: 0 },
            Uop::Kimm { dst: 6, kidx: k(8) },
            Uop::Kimm { dst: 7, kidx: k(8) },
            Uop::Mov { dst: 8, src: 2 },
            Uop::Kimm { dst: 9, kidx: k(4) },
            Uop::Callg {
                callee: k(2),
                nargs: 4,
            },
            // the two-word answer combines: word0 * 1000 + word1
            Uop::Mov { dst: 3, src: 5 },
            Uop::Mov { dst: 4, src: 6 },
            Uop::Kimm {
                dst: 5,
                kidx: k(10),
            },
            Uop::MulI64 {
                dst: 3,
                lhs: 3,
                rhs: 5,
            },
            Uop::AddI64 {
                dst: 3,
                lhs: 3,
                rhs: 4,
            },
            Uop::Ret { src: 3 },
        ],
    )
}

/// One par_reduce main over string elements from the pair init (0, ""):
/// the fold's two words stage as nargs 4 (list, init count, init
/// string, callable) plus the List evidence word, the mixed answer's
/// `Ref` word returning.
fn wide_ref_main_def(name: &str, elems: &[&str]) -> FunctionDef {
    // body slots 0..1, overlay words 2..7
    let mut pool = vec![
        builtin(BUILTIN_LIST_NEW),                // k0
        builtin(BUILTIN_LIST_PUSH),               // k1
        builtin(BUILTIN_PAR_REDUCE),              // k2
        pr_ref(21),                               // k3 wide_strcat_guard
        Const::I64(BuiltinInstance::List as i64), // k4
        Const::I64(0),                            // k5 the init's count word
        Const::Str(String::new()),                // k6 the init's string word
    ];
    let str_at = |pool: &mut Vec<Const>, text: &str| {
        pool.push(Const::Str(text.into()));
        k(u16::try_from(pool.len() - 1).expect("pool index fits u16"))
    };
    let mut body = vec![
        Uop::Funcf { frame_slots: 8 },
        Uop::Call {
            callee: k(0),
            nargs: 0,
        },
        Uop::Mov { dst: 0, src: 2 },
    ];
    for &text in elems {
        let word = str_at(&mut pool, text);
        body.push(Uop::Mov { dst: 2, src: 0 });
        body.push(Uop::Kstr { dst: 3, kidx: word });
        body.push(Uop::Call {
            callee: k(1),
            nargs: 2,
        });
    }
    body.extend([
        Uop::Mov { dst: 2, src: 0 },        // the list
        Uop::Kimm { dst: 3, kidx: k(5) },   // the init's count word
        Uop::Kstr { dst: 4, kidx: k(6) },   // the init's string word
        Uop::Kconst { dst: 5, kidx: k(3) }, // the callable
        Uop::Kimm { dst: 6, kidx: k(4) },   // the evidence
        Uop::Callg {
            callee: k(2),
            nargs: 4,
        },
        Uop::Mov { dst: 1, src: 3 }, // the answer's Ref word
        Uop::Ret { src: 1 },
    ]);
    def(
        name,
        &[Ref, Ref, Raw, Raw, Raw, Raw, Raw, Raw],
        0,
        6,
        pool,
        &body,
    )
}

/// The fixture module. Function indices are pool-FuncRef contract:
/// 0 main_order, 1 main_park, 2 main_untouched, 3 main_first_failure,
/// 4 main_nested, 5 main_ref_chain, 6 main_bad_callable,
/// 7 main_wide_interior, 8 main_empty, 9 shift10, 10 parking_shift10,
/// 11 guard2, 12 nested_fn, 13 add2, 14 strcat2, 15 biased_shift,
/// 16 main_closure_fold, 17 main_nested_map, 18 nested_map_fn,
/// 19 double1, 20 biased_pair, 21 wide_strcat_guard, 22 pick3,
/// 23 main_wide_closure, 24 main_wide_ref, 25 main_wide_ref_failure.
fn pr_registry() -> ModuleRegistry {
    let mut reg = ModuleRegistry::new();
    reg.install(
        ModuleVersion::new("pr", vec![
            pr_main_def("main_order", &[1, 2, 3, 4, 5], 0, 9, false),
            pr_main_def("main_park", &[1, 2, 3, 4, 5], 0, 10, false),
            pr_main_def("main_untouched", &[10, 20, 30], 0, 9, true),
            pr_main_def("main_first_failure", &[5, 0], 0, 11, false),
            pr_main_def("main_nested", &[7], 0, 12, false),
            ref_chain_def(),
            bad_callable_def(),
            wide_interior_def(),
            empty_def(),
            shift10_def(),
            parking_shift10_def(),
            guard2_def(),
            nested_fn_def(),
            add2_def(),
            strcat2_def(),
            biased_shift_def(),
            closure_fold_def(),
            pr_main_def("main_nested_map", &[7], 0, 18, false),
            nested_map_fn_def(),
            double1_def(),
            biased_pair_def(),
            wide_strcat_guard_def(),
            pick3_def(),
            wide_closure_main_def(),
            wide_ref_main_def("main_wide_ref", &["1", "2", "3"]),
            wide_ref_main_def("main_wide_ref_failure", &["1", ""]),
        ])
        .expect("valid fixture module"),
    );
    reg
}

/// No spawned child outlives the boundary — the chain's step joins
/// consume each child whatever the outcome (a step's id may recycle,
/// so an id past the spawn count is simply absent).
fn assert_children_dead(vm: &Vm, up_to: u32) {
    for id in 1..=up_to {
        if let Some(state) = vm.fiber_state(FiberId(id)) {
            assert_eq!(
                state,
                FiberState::Dead,
                "child {id} must be dead at the boundary"
            );
        }
    }
}

#[test]
fn the_fold_applies_in_input_order() {
    let mut vm = Vm::new(pr_registry());
    // [1, 2, 3, 4, 5] folded by acc * 10 + b: the non-commutative
    // witness — 12345, not any permutation
    assert_eq!(
        vm.run("pr", "main_order", &[]),
        Ok(FrameAnswer::one(Value::I64(12_345)))
    );
    assert_children_dead(&vm, 5);
}

#[test]
fn a_parking_element_folds_in_place_across_ten_runs() {
    // each element parks mid-flight and is bounced back in
    // by the worklist; the fold still applies strictly in input order
    for _ in 0..10 {
        let mut vm = Vm::new(pr_registry());
        assert_eq!(
            vm.run("pr", "main_park", &[]),
            Ok(FrameAnswer::one(Value::I64(12_345)))
        );
        assert_children_dead(&vm, 5);
    }
}

#[test]
fn the_input_list_reads_back_untouched() {
    let mut vm = Vm::new(pr_registry());
    // input sum 60 * 100000 + the fold 1230
    assert_eq!(
        vm.run("pr", "main_untouched", &[]),
        Ok(FrameAnswer::one(Value::I64(6_001_230)))
    );
    assert_children_dead(&vm, 3);
}

#[test]
fn a_string_fold_balances_the_accumulator_counts() {
    let mut vm = Vm::new(pr_registry());
    // "1" ++ "2" ++ "3" through the native concat: each delivered
    // answer adopts (the child's completion count landing on the
    // joiner), each replaced word dec'd — the drain at teardown pins
    // the balance
    let answer = vm
        .run("pr", "main_ref_chain", &[])
        .expect("the string fold answers a Ref");
    let [Value::Ref(id)] = answer.as_slice() else {
        panic!("the string fold answers a Ref word");
    };
    let id = *id;
    assert_eq!(vm.objects_of(id).str_at(id.id()), Ok("123"));
    assert_children_dead(&vm, 3);
}

#[test]
fn the_failing_element_re_raises_at_the_call_site() {
    let mut vm = Vm::new(pr_registry());
    // element 5 counts down through its backward branch and completes;
    // element 0 dies trapped at the division — the chain holds no live
    // sibling at the failure, the owned accumulator is released, and
    // the trap re-raises at the par_reduce call site
    assert_eq!(
        vm.run("pr", "main_first_failure", &[]),
        Err(Trap::DivideByZero)
    );
    assert_children_dead(&vm, 2);
}

#[test]
fn a_nested_par_reduce_folds_through_the_parked_call_site() {
    let mut vm = Vm::new(pr_registry());
    // the element's own par_reduce parks its fiber at its CALLG site —
    // the drive frames stacking on the interlock's heap stack, the
    // seam's chain state folding inside — and the inner fold lands
    // identically to the hosted equivalent: add2(0, 7) = 7
    assert_eq!(
        vm.run("pr", "main_nested", &[]),
        Ok(FrameAnswer::one(Value::I64(7)))
    );
    // the outer child plus the inner child are both dead at the boundary
    assert_children_dead(&vm, 2);
}

#[test]
fn an_inner_par_map_lands_inside_a_par_reduce_element() {
    let mut vm = Vm::new(pr_registry());
    // the cross-combinator nesting: the inner par_map parks exactly
    // like the same-combinator nesting (the parked CALLG seam is the
    // shared mechanism), the inner answers assembling in order —
    // double1(7) = 14, folded onto 0
    assert_eq!(
        vm.run("pr", "main_nested_map", &[]),
        Ok(FrameAnswer::one(Value::I64(14)))
    );
    // the outer child plus the inner child are both dead at the boundary
    assert_children_dead(&vm, 2);
}

#[test]
fn a_non_closure_ref_word_refuses_at_the_spawn() {
    let mut vm = Vm::new(pr_registry());
    // the callable-defaulted `Ref` spelling is a closure word now, so a
    // value-site `Ref` (the input list's word, hand-staged against the
    // callable slot) names no closure object and refuses at the spawn
    assert_eq!(
        vm.run("pr", "main_bad_callable", &[]),
        Err(Trap::UnknownObject { obj: 0 })
    );
}

#[test]
fn a_capturing_closure_folds_in_input_order() {
    let mut vm = Vm::new(pr_registry());
    // the closure capturing bias 5 folds [1, 2, 3] strictly in input
    // order: (0*10 + 1 + 5) = 6, then (6*10 + 2 + 5) = 67, then
    // (67*10 + 3 + 5) = 678
    assert_eq!(
        vm.run("pr", "main_closure_fold", &[]),
        Ok(FrameAnswer::one(Value::I64(678)))
    );
    assert_children_dead(&vm, 3);
}

#[test]
fn an_answer_width_disagreeing_with_the_init_span_refuses() {
    let mut vm = Vm::new(pr_registry());
    // the callable's entry parameters cover the two-word init plus the
    // element, but its answer is one word — a width the checker never
    // proves: the adoption refuses instead of re-chaining at a
    // different width
    assert_eq!(
        vm.run("pr", "main_wide_interior", &[]),
        Err(Trap::ArityMismatch {
            expected: 2,
            given: 1,
        })
    );
}

#[test]
fn a_capturing_closure_folds_a_wide_pair_in_input_order() {
    let mut vm = Vm::new(pr_registry());
    // [1, 2, 3] from the pair init (0, 0) through the closure
    // capturing bias 5: each child answers the next accumulator's two
    // words whole, both order witnesses — (6, 1), (67, 3), (678, 6)
    assert_eq!(
        vm.run("pr", "main_wide_closure", &[]),
        Ok(FrameAnswer::one(Value::I64(678_006)))
    );
    assert_children_dead(&vm, 3);
}

#[test]
fn a_wide_fold_balances_the_accumulator_s_ref_counts() {
    let mut vm = Vm::new(pr_registry());
    // the mixed (int, string) fold: each step's fresh string adopts
    // with the child's completion count on the joiner, each replaced
    // string dec'd at adoption — the final word carries exactly the
    // transferred count to the host, "1" -> "12" -> "123"
    let answer = vm
        .run("pr", "main_wide_ref", &[])
        .expect("the wide ref fold answers a Ref");
    let [Value::Ref(id)] = answer.as_slice() else {
        panic!("the wide ref fold answers a Ref word");
    };
    let id = *id;
    let store = vm.objects_of(id);
    assert_eq!(
        store.rc_at(id.id()),
        Ok(1),
        "the count transferred to the host"
    );
    assert_eq!(store.str_at(id.id()), Ok("123"));
    assert_children_dead(&vm, 3);
}

#[test]
fn a_failing_wide_step_re_raises_with_the_owned_span_released() {
    let mut vm = Vm::new(pr_registry());
    // ["1", ""] from the pair init (0, ""): step one adopts (1, "1"),
    // step two's element dies trapped at the division — the owned wide
    // accumulator (both words) is released and the trap re-raises at
    // the par_reduce call site
    assert_eq!(
        vm.run("pr", "main_wide_ref_failure", &[]),
        Err(Trap::DivideByZero)
    );
    assert_children_dead(&vm, 2);
}

#[test]
fn an_empty_input_answers_the_init() {
    let mut vm = Vm::new(pr_registry());
    assert_eq!(
        vm.run("pr", "main_empty", &[]),
        Ok(FrameAnswer::one(Value::I64(7)))
    );
}
