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
        BUILTIN_LIST_GET, BUILTIN_LIST_LEN, BUILTIN_LIST_NEW, BUILTIN_LIST_PUSH, BUILTIN_PAR_MAP,
        BUILTIN_PAR_REDUCE, BUILTIN_STR_INT,
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

/// The fixture module: 0 main_map, 1 main_reduce, 2 main_wide,
/// 3 main_wordy, 4 main_trap, 5 main_nested, 6 main_closure,
/// 7 main_ref_capture, 8 main_empty, 9 doubles, 10 fold, 11 wide_pairs,
/// 12 wordy, 13 guard, 14 nested_fn, 15 biased, 16 string_capture.
fn fp_registry() -> ModuleRegistry {
    let mut reg = ModuleRegistry::new();
    reg.install(
        ModuleVersion::new("fp", vec![
            map_main_def("main_map", &[3, 1, 4, 1, 5, 9, 2, 6, 5, 3, 5, 8], 9),
            reduce_main_def("main_reduce", &[1, 2, 3, 4, 5, 6], 10),
            map_main_def("main_wide", &[1, 2, 3, 4, 5, 6, 7, 8], 11),
            list_main_def("main_wordy", &[1, 2, 3, 4, 5], 12),
            map_main_def("main_trap", &[4, 0, 5], 13),
            map_main_def("main_nested", &[3, 7], 14),
            closure_main_def("main_closure", &[1, 2, 3, 4, 5], 15, false),
            closure_main_def("main_ref_capture", &[1, 2, 3], 16, true),
            map_main_def("main_single", &[1], 9), /* one child: under the two-child bar, never
                                                   * pooled */
            doubles_def(),
            fold_def(),
            wide_pairs_def(),
            wordy_def(),
            guard_def(),
            nested_fn_def(),
            biased_def(),
            string_capture_def(),
        ])
        .expect("valid fixture module"),
    );
    reg
}

/// The differential core: the same entry under pool-off and pool-on
/// answers identically (value or trap), and the pooled children are all
/// dead at the boundary.
fn differential(entry: &str) -> (Result<Value, Trap>, Result<Value, Trap>, Vm) {
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
    assert_eq!(answer, Ok(Value::I64(6 * 1000 + 16 * 10 + 12)));
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
    assert_eq!(answer, Ok(Value::I64(2 * 1000 + 16 * 10 + 8)));
    assert!(
        pool.pooled_fork_drives() > 0,
        "the scalar-gated frame pooled"
    );
    assert_children_dead(&pool, 8);
}

#[test]
fn par_reduce_stays_sequential_under_the_pool() {
    let (answer, _, pool) = differential("main_reduce");
    assert_eq!(answer, Ok(Value::I64(123_456)));
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
        let Value::Ref(list) = vm
            .run("fp", "main_wordy", &[])
            .expect("the wordy main answers")
        else {
            panic!("the wordy main answers the result list word");
        };
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
    assert_eq!(answer, Ok(Value::I64(6 * 1000 + 14 * 10 + 2)));
    // the outer frame pooled; the inner ran on the worker's own
    // sequential interlock (workers never pool)
    assert!(pool.pooled_fork_drives() > 0);
    assert_children_dead(&pool, 2);
}

#[test]
fn an_i64_capture_closure_pools() {
    let (answer, _, pool) = differential("main_closure");
    // [1..5] through the closure capturing bias 10: x*2 + 10
    // -> first 12, last 20, len 5
    assert_eq!(answer, Ok(Value::I64(12 * 1000 + 20 * 10 + 5)));
    assert!(pool.pooled_fork_drives() > 0);
    assert_children_dead(&pool, 5);
}

#[test]
fn a_ref_capture_falls_back_to_the_sequential_drive() {
    let (answer, _, pool) = differential("main_ref_capture");
    // [1, 2, 3] through the string-capturing closure: x*2
    // -> first 2, last 6, len 3
    assert_eq!(answer, Ok(Value::I64(2 * 1000 + 6 * 10 + 3)));
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
            Ok(Value::I64(6 * 1000 + 16 * 10 + 12))
        );
        assert!(pool.pooled_fork_drives() > 0);
        assert_children_dead(&pool, 12);
    }
}
