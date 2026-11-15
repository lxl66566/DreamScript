//! The fork-join worker pool differential pins [E3, the 2026-11-15
//! sendability ruling]: a pool-enabled VM answers the same program the
//! sequential interlock drive answers — byte-identical values, traps,
//! and observable counts — while the scalar-gated frames demonstrably
//! run on workers and the gated-out shapes (a `Ref` word in an entry
//! frame, the `par_reduce` fold chain) keep the sequential drive. What
//! the fixtures pin:
//!
//! - the differential itself: every main's answer and trap equal under pool-off and pool-on, the
//!   pooled children dead at every boundary, and ten fresh pool runs answering identically.
//! - the non-vacuity witness: the scalar-gated maps record pooled drives while the `Ref`-capture
//!   fallback records none (the sequential drive, identical answers and counts).
//! - `Ref` answers minted on pooled children's own heaps: the words come home with the fiber, the
//!   join assembling them exactly as sequentially, each string at the result list's single edge.
//! - `par_reduce` under a pool-enabled VM: the fold chain stays sequential (the left data
//!   dependency) and answers the sequential fold.
//! - the hardened boundary: a mid-flight mint then trap and a cancel carrying its own minted `Ref`
//!   word (the death walk's held-back payload and its claim routing to the joiner), deep
//!   in-language recursion and a 2048-level nested combinator chain inside a pooled child (the
//!   worker's own interlock stack), a `fiber_yield` bounce and the own-nursery join refusal
//!   (identical between drives), counted `Ref` elements in the entry span (the gate's fallback),
//!   and a nested `par_reduce` on a worker.
//! - the out-of-contract divergences pinned honestly per drive: a `fiber_resume` of a host-parked
//!   fiber and a join of a foreign nursery both answer differently worker-side, because the worker
//!   holds no sibling targets — the source surface now refuses both spellings inside par_* element
//!   bodies statically (ds-check's pooled-body check, [E3]), so these pins document the
//!   embedder-only seam: hand-assembled bytecode never passes that check, and a host driving one
//!   owns the divergence.

use ds_vm::{
    Const, ConstFuncRef, FiberId, FiberState, FrameAnswer, FunctionDef, ModuleRegistry,
    ModuleVersion, Trap, Value, Vm,
    abi::{
        BuiltinInstance, ConstIdx, FrameLayout, Inst, RcOp, SlotType,
        SlotType::{Bool, I64, Raw, Ref, Tag},
        uop::Uop,
    },
    assemble,
    native::{
        BUILTIN_FIBER_RESUME, BUILTIN_FIBER_YIELD, BUILTIN_LIST_GET, BUILTIN_LIST_LEN,
        BUILTIN_LIST_NEW, BUILTIN_LIST_PUSH, BUILTIN_NURSERY_CANCEL, BUILTIN_NURSERY_JOIN,
        BUILTIN_NURSERY_OPEN, BUILTIN_PAR_MAP, BUILTIN_PAR_REDUCE, BUILTIN_STR_INT, PLATFORM_NONE,
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

/// A same-module callee by function-table index (the fixture module "fp").
fn fp_ref(index: u32) -> Const {
    Const::FuncRef(ConstFuncRef::Named {
        module: "fp".into(),
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

/// One par_map checksum main: build `elems`, fork them through
/// `callable`, read the result back (`list_len`, first and last
/// `list_get`), and answer `first * 1000 + last * 10 + len`.
fn map_main_def(name: &str, elems: &[i64], callable: u32) -> FunctionDef {
    let mut pool = vec![
        builtin(BUILTIN_LIST_NEW),                // k0
        builtin(BUILTIN_LIST_PUSH),               // k1
        builtin(BUILTIN_PAR_MAP),                 // k2
        builtin(BUILTIN_LIST_LEN),                // k3
        builtin(BUILTIN_LIST_GET),                // k4
        fp_ref(callable),                         // k5
        Const::I64(BuiltinInstance::List as i64), // k6
    ];
    let word_at = |pool: &mut Vec<Const>, word: i64| {
        pool.push(Const::I64(word));
        k(u16::try_from(pool.len() - 1).expect("pool index fits u16"))
    };
    let mut body = vec![Uop::Funcf { frame_slots: 11 }];
    body.push(Uop::Call {
        callee: k(0),
        nargs: 0,
    });
    body.push(Uop::Mov { dst: 0, src: 7 });
    for &elem in elems {
        let word = word_at(&mut pool, elem);
        body.push(Uop::Mov { dst: 7, src: 0 });
        body.push(Uop::Kimm { dst: 8, kidx: word });
        body.push(Uop::Call {
            callee: k(1),
            nargs: 2,
        });
    }
    body.push(Uop::Mov { dst: 7, src: 0 });
    body.push(Uop::Kconst { dst: 8, kidx: k(5) });
    body.push(Uop::Kimm { dst: 9, kidx: k(6) });
    body.push(Uop::Kimm {
        dst: 10,
        kidx: k(6),
    });
    body.push(Uop::Callg {
        callee: k(2),
        nargs: 2,
    });
    body.push(Uop::Mov { dst: 1, src: 7 });
    body.push(Uop::Mov { dst: 7, src: 1 });
    body.push(Uop::Kimm { dst: 8, kidx: k(6) });
    body.push(Uop::Callg {
        callee: k(3),
        nargs: 1,
    });
    body.push(Uop::Mov { dst: 4, src: 7 });
    for (slot, index) in [(2u8, 0usize), (3, elems.len() - 1)] {
        let word = word_at(&mut pool, i64::try_from(index).expect("index fits i64"));
        body.push(Uop::Mov { dst: 7, src: 1 });
        body.push(Uop::Kimm { dst: 8, kidx: word });
        body.push(Uop::Kimm { dst: 9, kidx: k(6) });
        body.push(Uop::Callg {
            callee: k(4),
            nargs: 2,
        });
        body.push(Uop::Mov { dst: slot, src: 8 });
    }
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

/// One bare par_map main: build `elems`, fork them through `callable`,
/// answer the result list `Ref` — the readback main whose elements and
/// counts the differential inspects.
fn list_main_def(name: &str, elems: &[i64], callable: u32) -> FunctionDef {
    let mut pool = vec![
        builtin(BUILTIN_LIST_NEW),                // k0
        builtin(BUILTIN_LIST_PUSH),               // k1
        builtin(BUILTIN_PAR_MAP),                 // k2
        fp_ref(callable),                         // k3
        Const::I64(BuiltinInstance::List as i64), // k4
    ];
    let word_at = |pool: &mut Vec<Const>, word: i64| {
        pool.push(Const::I64(word));
        k(u16::try_from(pool.len() - 1).expect("pool index fits u16"))
    };
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

/// One par_reduce main: build `elems`, fold them through `callable`
/// (`acc * 10 + b`) from 0, answer the fold.
fn reduce_main_def(name: &str, elems: &[i64], callable: u32) -> FunctionDef {
    let mut pool = vec![
        builtin(BUILTIN_LIST_NEW),                // k0
        builtin(BUILTIN_LIST_PUSH),               // k1
        builtin(BUILTIN_PAR_REDUCE),              // k2
        fp_ref(callable),                         // k3
        Const::I64(BuiltinInstance::List as i64), // k4
    ];
    let word_at = |pool: &mut Vec<Const>, word: i64| {
        pool.push(Const::I64(word));
        k(u16::try_from(pool.len() - 1).expect("pool index fits u16"))
    };
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
    let init = word_at(&mut pool, 0);
    body.push(Uop::Mov { dst: 5, src: 0 });
    body.push(Uop::Kimm { dst: 6, kidx: init });
    body.push(Uop::Kconst { dst: 7, kidx: k(3) });
    body.push(Uop::Kimm { dst: 8, kidx: k(4) });
    body.push(Uop::Callg {
        callee: k(2),
        nargs: 3,
    });
    body.push(Uop::Mov { dst: 1, src: 5 });
    body.push(Uop::Ret { src: 1 });
    def(
        name,
        &[Ref, Raw, Raw, Raw, Raw, Raw, Raw, Raw, Raw],
        0,
        4,
        pool,
        &body,
    )
}

/// One closure-callable main: build `elems`, fork them through a
/// closure over `callee` with the staged capture, read the result back,
/// answer the checksum. With a `Ref` capture the frames hold a foreign
/// word — the pool's gate refuses and the sequential drive runs.
fn closure_main_def(name: &str, elems: &[i64], callee: u32, capture_is_word: bool) -> FunctionDef {
    let mut pool = vec![
        builtin(BUILTIN_LIST_NEW),                // k0
        builtin(BUILTIN_LIST_PUSH),               // k1
        builtin(BUILTIN_PAR_MAP),                 // k2
        builtin(BUILTIN_LIST_LEN),                // k3
        builtin(BUILTIN_LIST_GET),                // k4
        fp_ref(callee),                           // k5 the callable's header
        Const::I64(BuiltinInstance::List as i64), // k6
    ];
    let word_at = |pool: &mut Vec<Const>, word: i64| {
        pool.push(Const::I64(word));
        k(u16::try_from(pool.len() - 1).expect("pool index fits u16"))
    };
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
    // the closure: one capture staged at overlay word 0 (an i64 bias, or
    // a fresh string word for the Ref-capture shape)
    if capture_is_word {
        let word = k(u16::try_from(pool.len()).expect("pool index fits u16"));
        body.push(Uop::Kstr {
            dst: 10,
            kidx: word,
        });
        pool.push(Const::Str("shared".into()));
    } else {
        let bias = word_at(&mut pool, 10);
        body.push(Uop::Kimm {
            dst: 10,
            kidx: bias,
        });
    }
    body.push(Uop::Kconst { dst: 7, kidx: k(5) });
    body.push(Uop::ClosureNew {
        dst: 6,
        func: 7,
        n: 1,
    });
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
    for (slot, index) in [(2u8, 0usize), (3, elems.len() - 1)] {
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
        body.push(Uop::Mov { dst: slot, src: 11 });
    }
    let thousand = word_at(&mut pool, 1000);
    let ten = word_at(&mut pool, 10);
    body.push(Uop::Kimm {
        dst: 9,
        kidx: word_at(&mut pool, 0),
    });
    body.push(Uop::Kimm {
        dst: 5,
        kidx: thousand,
    });
    body.push(Uop::MulI64 {
        dst: 2,
        lhs: 2,
        rhs: 5,
    });
    body.push(Uop::AddI64 {
        dst: 9,
        lhs: 9,
        rhs: 2,
    });
    body.push(Uop::Kimm { dst: 5, kidx: ten });
    body.push(Uop::MulI64 {
        dst: 3,
        lhs: 3,
        rhs: 5,
    });
    body.push(Uop::AddI64 {
        dst: 9,
        lhs: 9,
        rhs: 3,
    });
    body.push(Uop::AddI64 {
        dst: 9,
        lhs: 9,
        rhs: 8,
    });
    body.push(Uop::Ret { src: 9 });
    def(
        name,
        &[
            Ref, Ref, I64, I64, Raw, I64, Raw, Raw, I64, I64, Raw, Raw, Raw, Raw,
        ],
        0,
        4,
        pool,
        &body,
    )
}

/// `(x: int) -> int`: x * 2.
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

/// `(acc: int, b: int) -> int`: acc * 10 + b — the non-commutative fold.
fn fold_def() -> FunctionDef {
    def(
        "fold",
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
                dst: 3,
                lhs: 3,
                rhs: 1,
            },
            Uop::Ret { src: 3 },
        ],
    )
}

/// `(x: int) -> (int, int)`: the span [x*2, x*3] through RETSPAN.
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

/// `(x: int) -> (int, string)`: the span [x*10, str_int(x)] — the
/// `Ref`-word answers minted on each pooled child's own heap.
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

/// `(x: int) -> int`: x == 0 dies trapped at the division; otherwise it
/// counts x down through a taken backward branch.
fn guard_def() -> FunctionDef {
    def(
        "guard",
        &[I64, I64, I64, Bool, I64],
        1,
        0,
        vec![Const::I64(0), Const::I64(1)],
        &[
            Uop::Funcf { frame_slots: 5 },
            Uop::Kimm { dst: 1, kidx: k(0) },
            Uop::Kimm { dst: 2, kidx: k(1) },
            Uop::EqI64 {
                dst: 3,
                lhs: 0,
                rhs: 1,
            },
            Uop::Jmpif {
                cond: 3,
                target: 11,
            },
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
            Uop::Jmp { target: 5 },
            Uop::Ret { src: 1 },
            Uop::DivI64 {
                dst: 4,
                lhs: 2,
                rhs: 1,
            },
            Uop::Ret { src: 4 },
        ],
    )
}

/// `(x: int) -> int`: forks `doubles` over the one-element list `[x]`
/// and answers the inner element — the nested par_map a pooled child
/// runs worker-side through its own sequential interlock.
fn nested_fn_def() -> FunctionDef {
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
            fp_ref(9),                                // k4 doubles
            Const::I64(0),                            // k5 the read index
            Const::I64(BuiltinInstance::List as i64), // k6
        ],
        &[
            Uop::Funcf { frame_slots: 8 },
            Uop::Call {
                callee: k(0),
                nargs: 0,
            },
            Uop::Mov { dst: 1, src: 4 },
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
            },
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
                tag: ds_vm::native::PLATFORM_IO_OK,
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

/// `(x: int, bias: int) -> int`: x * 2 + bias — the i64-capture closure
/// body (poolable frames).
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

/// `(x: int, s: string) -> int`: x * 2 with the capture's exit dec —
/// the `Ref`-capture closure body (the frames hold a foreign word, the
/// pool's gate refuses).
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
            RcOp::Dec { src: 1 }.into(),
            Uop::Ret { src: 2 }.into(),
        ],
    )
}

/// `(x: int) -> int`: mints a fresh string word on its own heap, holds
/// it in the frame, then dies at the division — the mid-flight mint a
/// trapped child's death walk must release.
fn mint_trap_def() -> FunctionDef {
    def(
        "mint_trap",
        &[I64, I64, Ref, Raw],
        1,
        1,
        vec![builtin(BUILTIN_STR_INT), Const::I64(0)],
        &[
            Uop::Funcf { frame_slots: 4 },
            Uop::Mov { dst: 3, src: 0 },
            Uop::Call {
                callee: k(0),
                nargs: 1,
            },
            Uop::Mov { dst: 2, src: 3 },
            Uop::Kimm { dst: 1, kidx: k(1) },
            Uop::DivI64 {
                dst: 3,
                lhs: 1,
                rhs: 1,
            },
            Uop::Ret { src: 3 },
        ],
    )
}

/// `(x: int) -> int`: mints a fresh string word on its own heap and
/// cancels carrying it as the payload — the trapped child whose death
/// walk must hold the payload word back, route its claim to the joiner,
/// and hand a live word to the host with the trap.
fn cancel_wordy_def() -> FunctionDef {
    def(
        "cancel_wordy",
        &[I64, I64, Ref, Raw],
        1,
        1,
        vec![builtin(BUILTIN_STR_INT), builtin(BUILTIN_NURSERY_CANCEL)],
        &[
            Uop::Funcf { frame_slots: 4 },
            Uop::Mov { dst: 3, src: 0 },
            Uop::Call {
                callee: k(0),
                nargs: 1,
            },
            Uop::Mov { dst: 2, src: 3 },
            Uop::Mov { dst: 3, src: 2 },
            Uop::Call {
                callee: k(1),
                nargs: 1,
            },
            Uop::Ret { src: 3 },
        ],
    )
}

/// `(n: int) -> int`: n == 0 answers 0; otherwise it recurses through
/// itself at n - 1 and adds one on the way back — the in-language deep
/// recursion a pooled child's value stack carries.
fn deep_rec_def() -> FunctionDef {
    def(
        "deep_rec",
        &[I64, I64, Bool, Raw],
        1,
        1,
        vec![Const::I64(0), Const::I64(1), fp_ref(21)],
        &[
            Uop::Funcf { frame_slots: 4 },
            Uop::Kimm { dst: 1, kidx: k(0) },
            Uop::EqI64 {
                dst: 2,
                lhs: 0,
                rhs: 1,
            },
            Uop::Jmpif {
                cond: 2,
                target: 11,
            },
            Uop::Kimm { dst: 1, kidx: k(1) },
            Uop::SubI64 {
                dst: 3,
                lhs: 0,
                rhs: 1,
            },
            Uop::Call {
                callee: k(2),
                nargs: 1,
            },
            Uop::Mov { dst: 0, src: 3 },
            Uop::AddI64 {
                dst: 0,
                lhs: 0,
                rhs: 1,
            },
            Uop::Jmp { target: 12 },
            Uop::Ret { src: 1 },
            Uop::Kimm { dst: 0, kidx: k(0) },
            Uop::Ret { src: 0 },
        ],
    )
}

/// `(x: int) -> int`: parks at `fiber_yield`; the drive's bare-tag
/// bounce overlays the call's answer, and the tag test reports whether
/// the none tag arrived — 1 when it did, 0 otherwise.
fn yielder_def() -> FunctionDef {
    def(
        "yielder",
        &[I64, I64, Bool, Raw],
        1,
        1,
        vec![builtin(BUILTIN_FIBER_YIELD), Const::I64(0), Const::I64(1)],
        &[
            Uop::Funcf { frame_slots: 4 },
            Uop::Mov { dst: 3, src: 0 },
            Uop::Call {
                callee: k(0),
                nargs: 1,
            },
            Uop::TagTest {
                dst: 2,
                src: 3,
                tag: PLATFORM_NONE,
            },
            Uop::Jmpif { cond: 2, target: 8 },
            Uop::Kimm { dst: 0, kidx: k(1) },
            Uop::Jmp { target: 9 },
            Uop::Jmp { target: 9 },
            Uop::Kimm { dst: 0, kidx: k(2) },
            Uop::Ret { src: 0 },
        ],
    )
}

/// `(x: int) -> int`: joins nursery 0 — the fork's own nursery on the
/// host, the worker stub's own on a pooled drive — the own-nursery
/// refusal trap both drives must raise identically.
fn own_joiner_def() -> FunctionDef {
    def(
        "own_joiner",
        &[I64, I64, Raw],
        1,
        1,
        vec![builtin(BUILTIN_NURSERY_JOIN), Const::I64(0)],
        &[
            Uop::Funcf { frame_slots: 3 },
            Uop::Kimm { dst: 2, kidx: k(1) },
            Uop::Call {
                callee: k(0),
                nargs: 1,
            },
            Uop::Ret { src: 2 },
        ],
    )
}

/// `(x: int) -> int`: joins the nursery x names — the host's foreign
/// empty nursery answers the zero failed-children count; the pooled
/// worker holds only the child's own stub, so the same id refuses.
fn foreign_joiner_def() -> FunctionDef {
    def(
        "foreign_joiner",
        &[I64, I64, Raw],
        1,
        1,
        vec![builtin(BUILTIN_NURSERY_JOIN), Const::I64(0)],
        &[
            Uop::Funcf { frame_slots: 3 },
            Uop::Mov { dst: 2, src: 0 },
            Uop::Call {
                callee: k(0),
                nargs: 1,
            },
            Uop::AddI64 {
                dst: 0,
                lhs: 0,
                rhs: 2,
            },
            Uop::Ret { src: 0 },
        ],
    )
}

/// `(x: int) -> int`: resumes fiber 1 with payload 5 — the host-parked
/// resumee completes and answers 107; a worker holds no fiber 1, so the
/// call bounces the bare none tag and the element answers x*10 instead.
fn host_resumer_def() -> FunctionDef {
    def(
        "host_resumer",
        &[I64, I64, Bool, Raw, Raw],
        1,
        2,
        vec![
            builtin(BUILTIN_FIBER_RESUME),
            Const::I64(1),   // the target fiber
            Const::I64(5),   // the payload
            Const::I64(100), // the entered-resumee bias
            Const::I64(10),  // the bounce bias
        ],
        &[
            Uop::Funcf { frame_slots: 5 },
            Uop::Kimm { dst: 3, kidx: k(1) },
            Uop::Kimm { dst: 4, kidx: k(2) },
            Uop::Call {
                callee: k(0),
                nargs: 2,
            },
            Uop::TagTest {
                dst: 2,
                src: 3,
                tag: PLATFORM_NONE,
            },
            Uop::Jmpif {
                cond: 2,
                target: 10,
            },
            Uop::Kimm { dst: 1, kidx: k(3) },
            Uop::AddI64 {
                dst: 0,
                lhs: 0,
                rhs: 1,
            },
            Uop::Jmp { target: 12 },
            Uop::Jmp { target: 12 },
            Uop::Kimm { dst: 1, kidx: k(4) },
            Uop::AddI64 {
                dst: 0,
                lhs: 0,
                rhs: 1,
            },
            Uop::Ret { src: 0 },
        ],
    )
}

/// `(v: int) -> tag`: answers the tag 7 — the host-parked resumee's
/// body, entered by a pooled child's `fiber_resume` on the sequential
/// drive only (the resume call's answer kind is the tag).
fn parked_resumee_def() -> FunctionDef {
    def("parked_resumee", &[I64, Tag], 1, 0, vec![], &[
        Uop::Funcf { frame_slots: 2 },
        Uop::TagNew { dst: 1, tag: 7 },
        Uop::Ret { src: 1 },
    ])
}

/// `(s: string) -> int`: 1 when s is "2", 0 otherwise — the foreign
/// element word a `Ref`-element frame's callable reads.
fn word_elem_def() -> FunctionDef {
    def(
        "word_elem",
        &[Ref, Bool, Ref, I64],
        1,
        0,
        vec![Const::Str("2".into()), Const::I64(0), Const::I64(1)],
        &[
            Uop::Funcf { frame_slots: 4 },
            Uop::Kstr { dst: 2, kidx: k(0) },
            Uop::EqStr {
                dst: 1,
                lhs: 0,
                rhs: 2,
            },
            Uop::Jmpif { cond: 1, target: 7 },
            Uop::Kimm { dst: 3, kidx: k(1) },
            Uop::Jmp { target: 9 },
            Uop::Jmp { target: 9 },
            Uop::Kimm { dst: 3, kidx: k(2) },
            Uop::Jmp { target: 9 },
            Uop::Ret { src: 3 },
        ],
    )
}

/// One `Ref`-element par_map main: mint a counted string word per
/// element on the main heap, fork them through `callable`, read the
/// result back, answer the checksum — every child's entry span holds a
/// foreign word, the gate's refusal shape.
fn strings_main_def(name: &str, elems: &[i64], callable: u32) -> FunctionDef {
    let mut pool = vec![
        builtin(BUILTIN_STR_INT),                 // k0
        builtin(BUILTIN_LIST_NEW),                // k1
        builtin(BUILTIN_LIST_PUSH),               // k2
        builtin(BUILTIN_PAR_MAP),                 // k3
        builtin(BUILTIN_LIST_LEN),                // k4
        builtin(BUILTIN_LIST_GET),                // k5
        fp_ref(callable),                         // k6
        Const::I64(BuiltinInstance::List as i64), // k7
    ];
    let word_at = |pool: &mut Vec<Const>, word: i64| {
        pool.push(Const::I64(word));
        k(u16::try_from(pool.len() - 1).expect("pool index fits u16"))
    };
    let mut body = vec![Uop::Funcf { frame_slots: 11 }];
    body.push(Uop::Call {
        callee: k(1),
        nargs: 0,
    });
    body.push(Uop::Mov { dst: 0, src: 7 });
    for &elem in elems {
        let word = word_at(&mut pool, elem);
        body.push(Uop::Kimm { dst: 7, kidx: word });
        body.push(Uop::Call {
            callee: k(0),
            nargs: 1,
        });
        body.push(Uop::Mov { dst: 8, src: 7 });
        body.push(Uop::Mov { dst: 7, src: 0 });
        body.push(Uop::Call {
            callee: k(2),
            nargs: 2,
        });
    }
    body.push(Uop::Mov { dst: 7, src: 0 });
    body.push(Uop::Kconst { dst: 8, kidx: k(6) });
    body.push(Uop::Kimm { dst: 9, kidx: k(7) });
    body.push(Uop::Kimm {
        dst: 10,
        kidx: k(7),
    });
    body.push(Uop::Callg {
        callee: k(3),
        nargs: 2,
    });
    body.push(Uop::Mov { dst: 1, src: 7 });
    body.push(Uop::Mov { dst: 7, src: 1 });
    body.push(Uop::Kimm { dst: 8, kidx: k(7) });
    body.push(Uop::Callg {
        callee: k(4),
        nargs: 1,
    });
    body.push(Uop::Mov { dst: 4, src: 7 });
    for (slot, index) in [(2u8, 0usize), (3, elems.len() - 1)] {
        let word = word_at(&mut pool, i64::try_from(index).expect("index fits i64"));
        body.push(Uop::Mov { dst: 7, src: 1 });
        body.push(Uop::Kimm { dst: 8, kidx: word });
        body.push(Uop::Kimm { dst: 9, kidx: k(7) });
        body.push(Uop::Callg {
            callee: k(5),
            nargs: 2,
        });
        body.push(Uop::Mov { dst: slot, src: 8 });
    }
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

/// `(x: int) -> int`: folds `[x, 1, 2]` from 0 through `acc * 10 + b`
/// with a nested `par_reduce` — the fold chain a pooled child runs on
/// its worker's own sequential interlock; the answer is x*100 + 12.
fn nested_reduce_fn_def() -> FunctionDef {
    def(
        "nested_reduce_fn",
        &[I64, Ref, I64, Bool, Raw, Raw, Raw, Raw],
        1,
        4,
        vec![
            builtin(BUILTIN_LIST_NEW),                // k0
            builtin(BUILTIN_LIST_PUSH),               // k1
            builtin(BUILTIN_PAR_REDUCE),              // k2
            builtin(BUILTIN_LIST_GET),                // k3
            fp_ref(10),                               // k4 fold
            Const::I64(0),                            // k5 the init
            Const::I64(1),                            // k6
            Const::I64(2),                            // k7
            Const::I64(BuiltinInstance::List as i64), // k8
        ],
        &[
            Uop::Funcf { frame_slots: 8 },
            Uop::Call {
                callee: k(0),
                nargs: 0,
            },
            Uop::Mov { dst: 1, src: 4 },
            Uop::Mov { dst: 4, src: 1 },
            Uop::Mov { dst: 5, src: 0 },
            Uop::Call {
                callee: k(1),
                nargs: 2,
            },
            Uop::Mov { dst: 4, src: 1 },
            Uop::Kimm { dst: 5, kidx: k(6) },
            Uop::Call {
                callee: k(1),
                nargs: 2,
            },
            Uop::Mov { dst: 4, src: 1 },
            Uop::Kimm { dst: 5, kidx: k(7) },
            Uop::Call {
                callee: k(1),
                nargs: 2,
            },
            Uop::Mov { dst: 4, src: 1 },
            Uop::Kimm { dst: 5, kidx: k(5) },
            Uop::Kconst { dst: 6, kidx: k(4) },
            Uop::Kimm { dst: 7, kidx: k(8) },
            Uop::Callg {
                callee: k(2),
                nargs: 3,
            },
            Uop::Mov { dst: 2, src: 4 },
            Uop::Ret { src: 2 },
        ],
    )
}

/// `() -> int`: open two empty nurseries, par_map the ids `[0, 1]`
/// through the foreign joiner, answer the first element — the foreign
/// nursery join's out-of-contract main.
fn foreign_main_def(name: &str, callable: u32) -> FunctionDef {
    def(
        name,
        &[Ref, Ref, I64, Bool, Raw, Raw, Raw, Raw],
        0,
        4,
        vec![
            builtin(BUILTIN_NURSERY_OPEN),            // k0
            builtin(BUILTIN_LIST_NEW),                // k1
            builtin(BUILTIN_LIST_PUSH),               // k2
            builtin(BUILTIN_PAR_MAP),                 // k3
            fp_ref(callable),                         // k4
            builtin(BUILTIN_LIST_GET),                // k5
            Const::I64(0),                            // k6 the read index
            Const::I64(0),                            // k7 element 0
            Const::I64(1),                            // k8 element 1
            Const::I64(BuiltinInstance::List as i64), // k9
        ],
        &[
            Uop::Funcf { frame_slots: 8 },
            Uop::Call {
                callee: k(0),
                nargs: 0,
            },
            Uop::Call {
                callee: k(0),
                nargs: 0,
            },
            Uop::Call {
                callee: k(1),
                nargs: 0,
            },
            Uop::Mov { dst: 0, src: 4 },
            Uop::Mov { dst: 4, src: 0 },
            Uop::Kimm { dst: 5, kidx: k(7) },
            Uop::Call {
                callee: k(2),
                nargs: 2,
            },
            Uop::Mov { dst: 4, src: 0 },
            Uop::Kimm { dst: 5, kidx: k(8) },
            Uop::Call {
                callee: k(2),
                nargs: 2,
            },
            Uop::Mov { dst: 4, src: 0 },
            Uop::Kconst { dst: 5, kidx: k(4) },
            Uop::Kimm { dst: 6, kidx: k(9) },
            Uop::Kimm { dst: 7, kidx: k(9) },
            Uop::Callg {
                callee: k(3),
                nargs: 2,
            },
            Uop::Mov { dst: 1, src: 4 },
            Uop::Mov { dst: 4, src: 1 },
            Uop::Kimm { dst: 5, kidx: k(6) },
            Uop::Kimm { dst: 6, kidx: k(9) },
            Uop::Callg {
                callee: k(5),
                nargs: 2,
            },
            Uop::TagTest {
                dst: 3,
                src: 4,
                tag: ds_vm::native::PLATFORM_IO_OK,
            },
            Uop::Jmpif {
                cond: 3,
                target: 26,
            },
            Uop::Kimm { dst: 2, kidx: k(6) },
            Uop::Jmp { target: 27 },
            Uop::Jmp { target: 27 },
            Uop::Mov { dst: 2, src: 5 },
            Uop::Ret { src: 2 },
        ],
    )
}

/// `(x: int) -> int`: the deep chain's base — the identity.
fn deep_base_def() -> FunctionDef {
    def("deep_base", &[I64], 1, 0, vec![], &[
        Uop::Funcf { frame_slots: 1 },
        Uop::Ret { src: 0 },
    ])
}

/// `(x: int) -> int`: forks the level below over `[x]`, reads the
/// answer back, and adds one — one nested combinator level, its drive
/// frame stacking on whichever interlock runs it.
fn deep_level_def(name: &str, callable: Const) -> FunctionDef {
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
            callable,                                 // k4 the level below
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
            Uop::Mov { dst: 1, src: 4 },
            Uop::Mov { dst: 4, src: 1 },
            Uop::Mov { dst: 5, src: 0 },
            Uop::Call {
                callee: k(1),
                nargs: 2,
            },
            Uop::Mov { dst: 4, src: 1 },
            Uop::Kconst { dst: 5, kidx: k(4) },
            Uop::Kimm { dst: 6, kidx: k(7) },
            Uop::Kimm { dst: 7, kidx: k(7) },
            Uop::Callg {
                callee: k(2),
                nargs: 2,
            },
            Uop::Mov { dst: 2, src: 4 },
            Uop::Mov { dst: 4, src: 2 },
            Uop::Kimm { dst: 5, kidx: k(5) },
            Uop::Kimm { dst: 6, kidx: k(7) },
            Uop::Callg {
                callee: k(3),
                nargs: 2,
            },
            Uop::TagTest {
                dst: 3,
                src: 4,
                tag: ds_vm::native::PLATFORM_IO_OK,
            },
            Uop::Jmpif {
                cond: 3,
                target: 20,
            },
            Uop::Kimm { dst: 0, kidx: k(5) },
            Uop::Ret { src: 0 },
            Uop::Mov { dst: 0, src: 5 },
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

/// The fixture module: 0 main_map, 1 main_reduce, 2 main_wide,
/// 3 main_wordy, 4 main_trap, 5 main_nested, 6 main_closure,
/// 7 main_ref_capture, 8 main_single, 9 doubles, 10 fold, 11 wide_pairs,
/// 12 wordy, 13 guard, 14 nested_fn, 15 biased, 16 string_capture,
/// 17 mint_trap, 18 cancel_wordy, 19 main_mint_trap, 20 main_cancel_wordy,
/// 21 deep_rec, 22 main_deep_rec, 23 yielder, 24 main_yield, 25 own_joiner,
/// 26 main_own_join, 27 host_resumer, 28 main_host_resume, 29 parked_resumee,
/// 30 word_elem, 31 main_strings, 32 nested_reduce_fn, 33 main_nested_reduce,
/// 34 foreign_joiner, 35 main_foreign_join, 36 deep_base, 37.. the deep
/// levels, and main_deep_fork last.
/// How deep the nested combinator chain runs inside a pooled child: the
/// pooled top frame plus the chain reaches the drive-frame pin budget.
const DEEP_LEVELS: u32 = 2047;

fn fp_registry() -> ModuleRegistry {
    let mut fns = vec![
        map_main_def("main_map", &[3, 1, 4, 1, 5, 9, 2, 6, 5, 3, 5, 8], 9),
        reduce_main_def("main_reduce", &[1, 2, 3, 4, 5, 6], 10),
        map_main_def("main_wide", &[1, 2, 3, 4, 5, 6, 7, 8], 11),
        list_main_def("main_wordy", &[1, 2, 3, 4, 5], 12),
        map_main_def("main_trap", &[4, 0, 5], 13),
        map_main_def("main_nested", &[3, 7], 14),
        closure_main_def("main_closure", &[1, 2, 3, 4, 5], 15, false),
        closure_main_def("main_ref_capture", &[1, 2, 3], 16, true),
        map_main_def("main_single", &[1], 9),
        doubles_def(),
        fold_def(),
        wide_pairs_def(),
        wordy_def(),
        guard_def(),
        nested_fn_def(),
        biased_def(),
        string_capture_def(),
        mint_trap_def(),
        cancel_wordy_def(),
        map_main_def("main_mint_trap", &[4, 5, 6], 17),
        map_main_def("main_cancel_wordy", &[4, 5, 6], 18),
        deep_rec_def(),
        map_main_def("main_deep_rec", &[1024, 100], 21),
        yielder_def(),
        map_main_def("main_yield", &[3, 4, 5], 23),
        own_joiner_def(),
        map_main_def("main_own_join", &[1, 2, 3], 25),
        host_resumer_def(),
        map_main_def("main_host_resume", &[1, 2], 27),
        parked_resumee_def(),
        word_elem_def(),
        strings_main_def("main_strings", &[1, 2, 3], 30),
        nested_reduce_fn_def(),
        map_main_def("main_nested_reduce", &[3, 7], 32),
        foreign_joiner_def(),
        foreign_main_def("main_foreign_join", 34),
        deep_base_def(),
    ];
    // the nested combinator chain: level i forks level i-1 over its
    // one element, DEEP_LEVELS deep — the drive-frame budget a pooled
    // child's own nesting reaches worker-side
    for i in 1..=DEEP_LEVELS {
        fns.push(deep_level_def(&format!("deep_level_{i}"), fp_ref(35 + i)));
    }
    fns.push(map_main_def("main_deep_fork", &[0, 1], 36 + DEEP_LEVELS));
    fns.push(map_main_def("main_deep_boundary", &[300_000, 100], 21));
    let mut reg = ModuleRegistry::new();
    reg.install(ModuleVersion::new("fp", fns).expect("valid fixture module"));
    reg
}

/// The differential core: the same entry under pool-off and pool-on
/// answers identically (value or trap), and the pooled children are all
/// dead at the boundary.
fn differential(entry: &str) -> (Result<FrameAnswer, Trap>, Result<FrameAnswer, Trap>, Vm) {
    let mut seq = Vm::new(fp_registry());
    let mut pool = Vm::new(fp_registry());
    pool.enable_fork_pool();
    let seq_answer = seq.run("fp", entry, &[]);
    let pool_answer = pool.run("fp", entry, &[]);
    assert_eq!(
        pool_answer, seq_answer,
        "the pool answers the sequential drive"
    );
    (seq_answer, pool_answer, pool)
}

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
fn par_map_answers_identically_and_actually_pools() {
    let (answer, _, pool) = differential("main_map");
    // [3,1,4,1,5,9,2,6,5,3,5,8] doubled: first 6, last 16, len 12
    assert_eq!(
        answer,
        Ok(FrameAnswer::one(Value::I64(6 * 1000 + 16 * 10 + 12)))
    );
    assert!(
        pool.pooled_fork_drives() > 0,
        "the scalar-gated frame pooled"
    );
    assert_children_dead(&pool, 12);
}

#[test]
fn wide_answers_answer_identically_under_the_pool() {
    let (answer, _, pool) = differential("main_wide");
    // [1..8] through the span callable: first (2, 3), last (16, 24), len 8
    // -> 2 * 1000 + 16 * 10 + 8 (the checksum main reads word 0 of each)
    assert_eq!(
        answer,
        Ok(FrameAnswer::one(Value::I64(2 * 1000 + 16 * 10 + 8)))
    );
    assert!(
        pool.pooled_fork_drives() > 0,
        "the scalar-gated frame pooled"
    );
    assert_children_dead(&pool, 8);
}

#[test]
fn par_reduce_stays_sequential_under_the_pool() {
    let (answer, _, pool) = differential("main_reduce");
    assert_eq!(answer, Ok(FrameAnswer::one(Value::I64(123_456))));
    // the fold chain is the left data dependency: one child per step
    // (one pool slot, reused), never pooled — the sequential drive
    // under a pool-enabled VM
    assert_eq!(pool.pooled_fork_drives(), 0);
    assert_children_dead(&pool, 1);
}

#[test]
fn ref_answers_minted_on_pooled_children_balance_like_sequential() {
    let mut seq = Vm::new(fp_registry());
    let mut pool = Vm::new(fp_registry());
    pool.enable_fork_pool();
    let read = |vm: &mut Vm| {
        let answer = vm
            .run("fp", "main_wordy", &[])
            .expect("the wordy main answers");
        let [Value::Ref(list)] = answer.as_slice() else {
            panic!("the wordy main answers the result list word");
        };
        let list = *list;
        let store = vm.objects_of(list);
        let elems = store.list_at(list.id()).expect("the result list reads");
        assert_eq!(elems.len(), 10, "five two-word answers");
        elems
            .chunks(2)
            .enumerate()
            .map(|(at, pair)| {
                let (Value::I64(int_half), Value::Ref(word)) = (&pair[0], &pair[1]) else {
                    panic!("the pair at {at} is an int then a Ref word");
                };
                assert_eq!(*int_half, i64::try_from((at + 1) * 10).expect("x*10"));
                assert_eq!(
                    vm.objects_of(*word).rc_at(word.id()),
                    Ok(1),
                    "one count: the list's edge"
                );
                vm.objects_of(*word)
                    .str_at(word.id())
                    .expect("the fresh string reads")
                    .to_string()
            })
            .collect::<Vec<_>>()
    };
    let seq_texts = read(&mut seq);
    let pool_texts = read(&mut pool);
    assert_eq!(pool_texts, seq_texts);
    assert_eq!(pool_texts, vec!["1", "2", "3", "4", "5"]);
    assert_children_dead(&pool, 5);
    assert!(pool.pooled_fork_drives() > 0);
}

#[test]
fn a_trapped_element_raises_identically_under_the_pool() {
    let (answer, _, pool) = differential("main_trap");
    assert_eq!(answer, Err(Trap::DivideByZero));
    assert_children_dead(&pool, 3);
    assert!(pool.pooled_fork_drives() > 0);
}

#[test]
fn a_nested_par_map_inside_a_pooled_child_lands() {
    let (answer, _, pool) = differential("main_nested");
    // each element's inner par_map doubles it (3 -> 6, 7 -> 14); the
    // outer checksum reads first 6, last 14, len 2
    assert_eq!(
        answer,
        Ok(FrameAnswer::one(Value::I64(6 * 1000 + 14 * 10 + 2)))
    );
    // the outer frame pooled; the inner ran on the worker's own
    // sequential interlock (workers never pool: exactly one pooled
    // drive — the host drives no second frame while the scope stands)
    assert_eq!(pool.pooled_fork_drives(), 1);
    assert_children_dead(&pool, 2);
}

#[test]
fn an_i64_capture_closure_pools() {
    let (answer, _, pool) = differential("main_closure");
    // [1..5] through the closure capturing bias 10: x*2 + 10
    // -> first 12, last 20, len 5
    assert_eq!(
        answer,
        Ok(FrameAnswer::one(Value::I64(12 * 1000 + 20 * 10 + 5)))
    );
    assert!(pool.pooled_fork_drives() > 0);
    assert_children_dead(&pool, 5);
}

#[test]
fn a_ref_capture_falls_back_to_the_sequential_drive() {
    let (answer, _, pool) = differential("main_ref_capture");
    // [1, 2, 3] through the string-capturing closure: x*2
    // -> first 2, last 6, len 3
    assert_eq!(
        answer,
        Ok(FrameAnswer::one(Value::I64(2 * 1000 + 6 * 10 + 3)))
    );
    assert_eq!(
        pool.pooled_fork_drives(),
        0,
        "a Ref word in an entry frame never pools"
    );
    assert_children_dead(&pool, 3);
}

#[test]
fn a_single_child_frame_stays_sequential() {
    // under the two-child bar: one element pays no transfer, and the
    // gate answers the drive's own threshold honestly
    let (_, _, pool) = differential("main_single");
    assert_eq!(pool.pooled_fork_drives(), 0);
    assert_children_dead(&pool, 1);
}

#[test]
fn ten_fresh_pool_runs_answer_identically() {
    for _ in 0..10 {
        let mut pool = Vm::new(fp_registry());
        pool.enable_fork_pool();
        assert_eq!(
            pool.run("fp", "main_map", &[]),
            Ok(FrameAnswer::one(Value::I64(6 * 1000 + 16 * 10 + 12)))
        );
        assert!(pool.pooled_fork_drives() > 0);
        assert_children_dead(&pool, 12);
    }
}
#[test]
fn a_mid_flight_minting_child_traps_identically() {
    // the child mints a string on its own heap before dying at the
    // division: the trap, the flag, and the death walk's release of the
    // minted count must all match the sequential drive
    let (answer, _, pool) = differential("main_mint_trap");
    assert_eq!(answer, Err(Trap::DivideByZero));
    assert!(pool.pooled_fork_drives() > 0);
    assert_children_dead(&pool, 3);
}

#[test]
fn a_cancel_payload_word_survives_the_pooled_death_walk() {
    // the trapped child's cancel payload is a fresh `Ref` word minted on
    // the child's own heap: the death walk must hold it back, route its
    // claim to the joiner, and the trap's delivery must hand a live
    // word to the host — the pooled homecoming runs the same walk, so
    // both drives surface the same live word
    let run = |pooled: bool| {
        let mut vm = Vm::new(fp_registry());
        if pooled {
            vm.enable_fork_pool();
        }
        let answer = vm.run("fp", "main_cancel_wordy", &[]);
        let Err(Trap::NurseryCancelled { payload }) = &answer else {
            panic!("the cancel trap surfaces, not {answer:?}");
        };
        let [Value::Ref(word)] = payload.as_slice() else {
            panic!("the payload is the child's one minted word");
        };
        let word = *word;
        let store = vm.objects_of(word);
        let rc = store.rc_at(word.id());
        let text = store.str_at(word.id()).map(ToString::to_string);
        (answer, (word, rc, text))
    };
    let (seq_answer, (seq_word, seq_rc, seq_text)) = run(false);
    let (pool_answer, (pool_word, pool_rc, pool_text)) = run(true);
    assert_eq!(pool_answer, seq_answer);
    assert_eq!(pool_word, seq_word);
    assert_eq!(seq_rc, Ok(1));
    assert_eq!(pool_rc, Ok(1));
    assert_eq!(pool_text, seq_text);
    // the first spawned element (4) cancelled first in spawn order
    assert_eq!(pool_text, Ok("4".to_owned()));
}

#[test]
fn deep_recursion_inside_a_pooled_child_answers_identically() {
    // [1024, 100] recursed through the callable and counted back up:
    // first 1024, last 100 — half a million stack words ride the cargo
    let (answer, _, pool) = differential("main_deep_rec");
    assert_eq!(
        answer,
        Ok(FrameAnswer::one(Value::I64(1024 * 1000 + 100 * 10 + 2)))
    );
    assert!(pool.pooled_fork_drives() > 0);
    assert_children_dead(&pool, 2);
}

#[test]
fn recursion_at_the_stack_boundary_traps_identically() {
    // the same callable recursed far past the 1 Mi-word segment budget:
    // the first child's 300k frames cross the limit and the overflow
    // trap cancels the frame, re-raising at the call — the boundary
    // trap is byte-identical between drives (the worker VM carries the
    // same limit), the flag and the walk included
    let (answer, _, pool) = differential("main_deep_boundary");
    assert!(matches!(answer, Err(Trap::StackOverflow { .. })));
    assert!(pool.pooled_fork_drives() > 0);
    assert_children_dead(&pool, 2);
}

#[test]
fn a_nested_combinator_chain_at_the_pin_budget_pools_its_top_frame() {
    // 2048 combinator levels in total: the pooled top frame's two
    // children each nest 2047 par_map levels — every level below the
    // pooled frame rides the worker's own sequential interlock (workers
    // never pool), the drive frames stacking on the interlock's heap
    // stack, level_2047(0) == 2047 and level_2047(1) == 2048
    let expect = Ok(FrameAnswer::one(Value::I64(2047 * 1000 + 2048 * 10 + 2)));
    let mut seq = Vm::new(fp_registry());
    assert_eq!(seq.run("fp", "main_deep_fork", &[]), expect);
    let mut pool = Vm::new(fp_registry());
    pool.enable_fork_pool();
    assert_eq!(pool.run("fp", "main_deep_fork", &[]), expect);
    assert_eq!(pool.pooled_fork_drives(), 1);
    // the sequential drive's second chain reuses the first chain's
    // dead slots, so the pool caps at the two outer children plus 2047
    // nested ones — every fiber it ever spawned is dead at the boundary
    for id in 1..=(2 + 2047) {
        assert_eq!(
            seq.fiber_state(FiberId(u32::try_from(id).expect("id fits"))),
            Some(FiberState::Dead),
            "fiber {id} must be dead at the boundary"
        );
    }
    // the pooled drive's nested fibers lived and died on the worker VMs
    // — only the two pooled children ever stood in the host pool
    assert_children_dead(&pool, 2);
}

#[test]
fn a_nested_par_reduce_inside_a_pooled_child_lands() {
    // each element's inner par_reduce folds [x, 1, 2] from 0 with
    // acc*10 + b -> x*100 + 12 (3 -> 312, 7 -> 712): the fold chain runs
    // on the worker's own sequential interlock, one drive frame per
    // step, and the outer checksum reads first 312, last 712, len 2
    let (answer, _, pool) = differential("main_nested_reduce");
    assert_eq!(
        answer,
        Ok(FrameAnswer::one(Value::I64(312 * 1000 + 712 * 10 + 2)))
    );
    assert_eq!(pool.pooled_fork_drives(), 1);
    assert_children_dead(&pool, 2);
}

#[test]
fn a_fiber_yield_inside_a_pooled_child_bounces_identically() {
    // each child parks at its yield; the drive bounces the bare none
    // tag back in on its re-entry, exactly the sequential drive's
    // bounce — every element answers 1, so first 1, last 1, len 3
    let (answer, _, pool) = differential("main_yield");
    assert_eq!(answer, Ok(FrameAnswer::one(Value::I64(1013))));
    assert!(pool.pooled_fork_drives() > 0);
    assert_children_dead(&pool, 3);
}

#[test]
fn an_own_nursery_join_refusal_traps_identically() {
    // joining its own nursery would deadlock the single-threaded driver
    // on itself — the refusal trap is identical whether the child
    // stands host-side (the fork's own nursery 0) or worker-side (the
    // stub at index 0)
    let (answer, _, pool) = differential("main_own_join");
    assert_eq!(
        answer,
        Err(Trap::UnknownCallee {
            module: "nursery".into(),
            func: 0,
        })
    );
    assert!(pool.pooled_fork_drives() > 0);
    assert_children_dead(&pool, 3);
}

#[test]
fn ref_words_in_the_entry_span_fall_back_identically() {
    // the elements are counted string words minted on the main heap — a
    // foreign word in every child's entry span, so the gate sends the
    // whole frame back to the sequential drive; word_elem answers 1 only
    // for "2", so [1, 2, 3] -> [0, 1, 0] -> first 0, last 0, len 3
    let (answer, _, pool) = differential("main_strings");
    assert_eq!(answer, Ok(FrameAnswer::one(Value::I64(3))));
    assert_eq!(
        pool.pooled_fork_drives(),
        0,
        "a Ref word in an entry span never pools"
    );
    assert_children_dead(&pool, 3);
}

#[test]
fn a_resume_of_a_host_parked_fiber_bounces_worker_side() {
    // OUT OF CONTRACT (the pool's purity ruling): a pooled child has no
    // sibling targets on its worker — the host-parked fiber 1's slot is
    // an empty placeholder there, so its fiber_resume answers the
    // checked-read bounce tag while the sequential drive enters the
    // fiber and drives it to completion. The per-drive behavior pinned
    // as the embedder seam: the checker's pooled-body refusal keeps this
    // spelling out of any compiled source, so only a hand-assembled
    // module reaches it
    let run = |pooled: bool| {
        let mut vm = Vm::new(fp_registry());
        vm.open_root_nursery();
        assert_eq!(
            vm.spawn_fiber("fp", "parked_resumee", &[Value::I64(7)]),
            Ok(FiberId(1))
        );
        if pooled {
            vm.enable_fork_pool();
        }
        (
            vm.run("fp", "main_host_resume", &[]),
            vm.fiber_state(FiberId(1)),
        )
    };
    let (seq_answer, seq_state) = run(false);
    let (pool_answer, pool_state) = run(true);
    // sequential: the first child enters fiber 1 (which answers its
    // tag, x + 100); the second names the completed fiber and takes the
    // dead-target bounce (x + 10) -> [101, 12]
    assert_eq!(
        seq_answer,
        Ok(FrameAnswer::one(Value::I64(101 * 1000 + 12 * 10 + 2)))
    );
    assert_eq!(seq_state, Some(FiberState::Dead));
    // pooled: no child ever reaches the fiber (its slot is an empty
    // placeholder on the worker), so both bounce — x + 10 -> [11, 12]
    // — and the host-parked fiber never runs
    assert_eq!(
        pool_answer,
        Ok(FrameAnswer::one(Value::I64(11 * 1000 + 12 * 10 + 2)))
    );
    assert_eq!(pool_state, Some(FiberState::Ready));
    assert_ne!(pool_answer, seq_answer);
}

#[test]
fn a_foreign_nursery_join_diverges_worker_side() {
    // OUT OF CONTRACT: nursery ids name host nurseries; the worker holds
    // only the child's own stub at index 0. The sequential drive joins
    // the two empty nurseries main opened (answering the zero
    // failed-children count, so [0, 1] -> [0, 1] -> first element 0);
    // the pooled child names its own stub worker-side, the own-nursery
    // refusal trapping and re-raising at the call. Pinned as the embedder
    // seam — the checker's pooled-body refusal keeps a foreign
    // `nursery_join` out of any compiled par_* element body
    let mut seq = Vm::new(fp_registry());
    assert_eq!(
        seq.run("fp", "main_foreign_join", &[]),
        Ok(FrameAnswer::one(Value::I64(0)))
    );
    let mut pool = Vm::new(fp_registry());
    pool.enable_fork_pool();
    assert_eq!(
        pool.run("fp", "main_foreign_join", &[]),
        Err(Trap::UnknownCallee {
            module: "nursery".into(),
            func: 0,
        })
    );
    assert!(pool.pooled_fork_drives() > 0);
    assert_children_dead(&pool, 2);
}
