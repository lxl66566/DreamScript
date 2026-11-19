//! The bytes collection's native face (indices 61-68) plus the `len`
//! evidence arm (the `Bytes` instance): `bytes_of` folds a `list[byte]`
//! into one counted block, `byte_list` bridges back out one word per
//! byte, `byte_at` answers the checked ok/none span, `bytes_eq` answers
//! the structural content compare the `==`/`!=` chain dispatches,
//! `bytes_compare`/`bytes_hash` answer the sort key and content hash
//! the derives ride, the two UTF-8 bridges take the string's own bytes
//! each way (the decode validating on the way back), and `len` counts
//! the payload — each driven through hand-assembled fixtures the way
//! the generic container tests drive the list surface, so the staging
//! conventions (evidence staged behind the real arguments, the result
//! overlaying argument slot 0, the checked span's discriminant at the
//! same slot) stay pinned without a compile in the loop.

use ds_vm::{
    Const, ConstFuncRef, FunctionDef, ModuleRegistry, ModuleVersion, Value, Vm,
    abi::{
        BuiltinInstance, ConstIdx, FrameLayout, Inst, RcOp, SlotType,
        SlotType::{I64, Raw, Ref},
        uop::Uop,
    },
    assemble,
    native::{
        BUILTIN_BYTE_AT, BUILTIN_BYTE_LIST, BUILTIN_BYTES_COMPARE, BUILTIN_BYTES_EQ,
        BUILTIN_BYTES_HASH, BUILTIN_BYTES_OF, BUILTIN_LEN, BUILTIN_LIST_GET, BUILTIN_LIST_NEW,
        BUILTIN_LIST_PUSH, BUILTIN_STR_HASH, BUILTIN_UTF8_DECODE, BUILTIN_UTF8_ENCODE,
        PLATFORM_IO_OK, PLATFORM_NONE, PLATFORM_UTF8_ERR,
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

/// The shared pool, copied per function: the two evidence words (bytes
/// and list), the bytes face, and the list construction surface the
/// fixtures build their inputs through.
fn pool() -> Vec<Const> {
    vec![
        Const::I64(BuiltinInstance::Bytes as i64), // k0: the bytes evidence
        Const::I64(BuiltinInstance::List as i64),  // k1: the list evidence
        builtin(BUILTIN_LIST_NEW),                 // k2
        builtin(BUILTIN_LIST_PUSH),                // k3
        builtin(BUILTIN_BYTES_OF),                 // k4
        builtin(BUILTIN_BYTE_LIST),                // k5
        builtin(BUILTIN_BYTE_AT),                  // k6
        builtin(BUILTIN_LIST_GET),                 // k7
        builtin(BUILTIN_LEN),                      // k8
        Const::I64(65),                            // k9: the pushed byte
        Const::I64(0),                             // k10: the unit word
        builtin(BUILTIN_BYTES_EQ),                 // k11
        builtin(BUILTIN_BYTES_COMPARE),            // k12
        builtin(BUILTIN_BYTES_HASH),               // k13
        builtin(BUILTIN_UTF8_ENCODE),              // k14
        builtin(BUILTIN_UTF8_DECODE),              // k15
        builtin(BUILTIN_STR_HASH),                 // k16
    ]
}

fn def<I>(name: &str, slots: &[SlotType], params: usize, args: usize, body: &[I]) -> FunctionDef
where
    I: Into<Inst> + Copy,
{
    FunctionDef {
        name: name.into(),
        layout: FrameLayout::new(slots.to_vec(), params, args).expect("valid fixture layout"),
        pool: pool(),
        code: assemble(body).expect("fixture body encodes"),
    }
}

/// `(a, b, c: i64) -> bytes`: the three bytes folded into one block —
/// the empty list, three construction appends, then `bytes_of` over the
/// finished list.
fn mk() -> FunctionDef {
    def("mk", &[I64, I64, I64, Ref, Ref, Ref, I64], 3, 2, &[
        Uop::Funcf { frame_slots: 7 },
        // the fresh empty list lands in the argument area's first slot
        Uop::Call {
            callee: k(2),
            nargs: 0,
        },
        Uop::Mov { dst: 3, src: 5 },
        Uop::Mov { dst: 5, src: 3 },
        Uop::Mov { dst: 6, src: 0 },
        Uop::Call {
            callee: k(3),
            nargs: 2,
        },
        Uop::Mov { dst: 5, src: 3 },
        Uop::Mov { dst: 6, src: 1 },
        Uop::Call {
            callee: k(3),
            nargs: 2,
        },
        Uop::Mov { dst: 5, src: 3 },
        Uop::Mov { dst: 6, src: 2 },
        Uop::Call {
            callee: k(3),
            nargs: 2,
        },
        Uop::Mov { dst: 5, src: 3 },
        Uop::Call {
            callee: k(4),
            nargs: 1,
        },
        Uop::Mov { dst: 4, src: 5 },
        Uop::Ret { src: 4 },
    ])
}

/// `(b: bytes) -> i64`: `len` through CALLG, the bytes evidence staged
/// behind the receiver.
fn len_bytes() -> FunctionDef {
    def("len_bytes", &[Ref, I64, Ref, I64], 1, 2, &[
        Uop::Funcf { frame_slots: 4 },
        Uop::Mov { dst: 2, src: 0 },
        Uop::Kimm { dst: 3, kidx: k(0) },
        Uop::Callg {
            callee: k(8),
            nargs: 1,
        },
        Uop::Mov { dst: 1, src: 2 },
        Uop::Ret { src: 1 },
    ])
}

/// `(b: bytes, i: i64) -> word`: `byte_at`'s raw discriminant word.
fn at_tag() -> FunctionDef {
    def("at_tag", &[Ref, I64, Raw, Ref, I64], 2, 2, &[
        Uop::Funcf { frame_slots: 5 },
        Uop::Mov { dst: 3, src: 0 },
        Uop::Mov { dst: 4, src: 1 },
        Uop::Call {
            callee: k(6),
            nargs: 2,
        },
        Uop::Mov { dst: 2, src: 3 },
        Uop::Ret { src: 2 },
    ])
}

/// `(b: bytes, i: i64) -> i64`: `byte_at`'s payload word — the byte on
/// the ok side, the rejected index on the none side.
fn at_payload() -> FunctionDef {
    def("at_payload", &[Ref, I64, I64, Ref, I64], 2, 2, &[
        Uop::Funcf { frame_slots: 5 },
        Uop::Mov { dst: 3, src: 0 },
        Uop::Mov { dst: 4, src: 1 },
        Uop::Call {
            callee: k(6),
            nargs: 2,
        },
        Uop::Mov { dst: 2, src: 4 },
        Uop::Ret { src: 2 },
    ])
}

/// `(b: bytes) -> list[byte]`: the bridge back out.
fn bridge() -> FunctionDef {
    def("bridge", &[Ref, Ref, Ref], 1, 1, &[
        Uop::Funcf { frame_slots: 3 },
        Uop::Mov { dst: 2, src: 0 },
        Uop::Call {
            callee: k(5),
            nargs: 1,
        },
        Uop::Mov { dst: 1, src: 2 },
        Uop::Ret { src: 1 },
    ])
}

/// `(xs: list, i: i64) -> i64`: `list_get`'s payload over the bridged
/// list, the list evidence staged behind the two arguments.
fn get() -> FunctionDef {
    def("get", &[Ref, I64, I64, Ref, I64, I64], 2, 3, &[
        Uop::Funcf { frame_slots: 6 },
        Uop::Mov { dst: 3, src: 0 },
        Uop::Mov { dst: 4, src: 1 },
        Uop::Kimm { dst: 5, kidx: k(1) },
        Uop::Callg {
            callee: k(7),
            nargs: 2,
        },
        Uop::Mov { dst: 2, src: 4 },
        Uop::Ret { src: 2 },
    ])
}

/// `(xs: list) -> bytes`: `bytes_of` over a handed-in list.
fn fold() -> FunctionDef {
    def("fold", &[Ref, Ref, Ref], 1, 1, &[
        Uop::Funcf { frame_slots: 3 },
        Uop::Mov { dst: 2, src: 0 },
        Uop::Call {
            callee: k(4),
            nargs: 1,
        },
        Uop::Mov { dst: 1, src: 2 },
        Uop::Ret { src: 1 },
    ])
}

/// `(a, b: i64) -> bytes`: the two-byte constructor, `mk`'s shape
/// minus one append — the length-mismatch probe's shorter payload.
fn mk2() -> FunctionDef {
    def("mk2", &[I64, I64, Ref, Ref, I64], 2, 2, &[
        Uop::Funcf { frame_slots: 5 },
        // the fresh empty list lands in the argument area's first slot
        Uop::Call {
            callee: k(2),
            nargs: 0,
        },
        Uop::Mov { dst: 2, src: 3 },
        Uop::Mov { dst: 3, src: 2 },
        Uop::Mov { dst: 4, src: 0 },
        Uop::Call {
            callee: k(3),
            nargs: 2,
        },
        Uop::Mov { dst: 3, src: 2 },
        Uop::Mov { dst: 4, src: 1 },
        Uop::Call {
            callee: k(3),
            nargs: 2,
        },
        Uop::Mov { dst: 3, src: 2 },
        Uop::Call {
            callee: k(4),
            nargs: 1,
        },
        Uop::Mov { dst: 2, src: 3 },
        Uop::Ret { src: 2 },
    ])
}

/// `(a, b: bytes) -> bool`: `bytes_eq`'s content compare — equal
/// payloads true, a different length or content false.
fn eq() -> FunctionDef {
    def("eq", &[Ref, Ref, Raw, Ref, Ref], 2, 2, &[
        Uop::Funcf { frame_slots: 5 },
        Uop::Mov { dst: 3, src: 0 },
        Uop::Mov { dst: 4, src: 1 },
        Uop::Call {
            callee: k(11),
            nargs: 2,
        },
        Uop::Mov { dst: 2, src: 3 },
        Uop::Ret { src: 2 },
    ])
}

/// `(a, b: bytes) -> i64`: `bytes_compare`'s sort key.
fn cmp() -> FunctionDef {
    def("cmp", &[Ref, Ref, I64, Ref, Ref], 2, 2, &[
        Uop::Funcf { frame_slots: 5 },
        Uop::Mov { dst: 3, src: 0 },
        Uop::Mov { dst: 4, src: 1 },
        Uop::Call {
            callee: k(12),
            nargs: 2,
        },
        Uop::Mov { dst: 2, src: 3 },
        Uop::Ret { src: 2 },
    ])
}

/// `(b: bytes) -> i64`: `bytes_hash`'s content fold.
fn hash() -> FunctionDef {
    def("hash", &[Ref, I64, Ref], 1, 1, &[
        Uop::Funcf { frame_slots: 3 },
        Uop::Mov { dst: 2, src: 0 },
        Uop::Call {
            callee: k(13),
            nargs: 1,
        },
        Uop::Mov { dst: 1, src: 2 },
        Uop::Ret { src: 1 },
    ])
}

/// `(s: string) -> i64`: `str_hash` over the same bytes, the
/// cross-check `bytes_hash`'s fold agrees with.
fn shash() -> FunctionDef {
    def("shash", &[Ref, I64, Ref], 1, 1, &[
        Uop::Funcf { frame_slots: 3 },
        Uop::Mov { dst: 2, src: 0 },
        Uop::Call {
            callee: k(16),
            nargs: 1,
        },
        Uop::Mov { dst: 1, src: 2 },
        Uop::Ret { src: 1 },
    ])
}

/// `(s: string) -> bytes`: `utf8_encode`, the string's own bytes.
fn enc() -> FunctionDef {
    def("enc", &[Ref, Ref, Ref], 1, 1, &[
        Uop::Funcf { frame_slots: 3 },
        Uop::Mov { dst: 2, src: 0 },
        Uop::Call {
            callee: k(14),
            nargs: 1,
        },
        Uop::Mov { dst: 1, src: 2 },
        Uop::Ret { src: 1 },
    ])
}

/// `(b: bytes) -> word`: `utf8_decode`'s raw discriminant word.
fn dec_tag() -> FunctionDef {
    def("dec_tag", &[Ref, Raw, Raw, Raw], 1, 2, &[
        Uop::Funcf { frame_slots: 4 },
        Uop::Mov { dst: 2, src: 0 },
        Uop::Call {
            callee: k(15),
            nargs: 1,
        },
        Uop::Mov { dst: 1, src: 2 },
        Uop::Ret { src: 1 },
    ])
}

/// `(b: bytes) -> string`: `utf8_decode`'s payload word.
fn dec_str() -> FunctionDef {
    def("dec_str", &[Ref, Ref, Raw, Raw], 1, 2, &[
        Uop::Funcf { frame_slots: 4 },
        Uop::Mov { dst: 2, src: 0 },
        Uop::Call {
            callee: k(15),
            nargs: 1,
        },
        Uop::Mov { dst: 1, src: 3 },
        Uop::Ret { src: 1 },
    ])
}

/// `() -> ()`: the construction-and-drop cycle — one byte pushed onto a
/// fresh list, folded into a bytes value, then both words dec'd in the
/// body (hand-assembled frames own their counts explicitly), so the
/// object table holds no survivor.
fn churn() -> FunctionDef {
    let body: Vec<Inst> = vec![
        Uop::Funcf { frame_slots: 4 }.into(),
        Uop::Call {
            callee: k(2),
            nargs: 0,
        }
        .into(),
        Uop::Mov { dst: 0, src: 2 }.into(),
        Uop::Mov { dst: 2, src: 0 }.into(),
        Uop::Kimm { dst: 3, kidx: k(9) }.into(),
        Uop::Call {
            callee: k(3),
            nargs: 2,
        }
        .into(),
        Uop::Mov { dst: 2, src: 0 }.into(),
        Uop::Call {
            callee: k(4),
            nargs: 1,
        }
        .into(),
        RcOp::Dec { src: 2 }.into(),
        RcOp::Dec { src: 0 }.into(),
        Uop::Kimm {
            dst: 1,
            kidx: k(10),
        }
        .into(),
        Uop::Ret { src: 1 }.into(),
    ];
    def("churn", &[Ref, I64, Ref, I64], 0, 2, &body)
}

fn registry() -> ModuleRegistry {
    let mut reg = ModuleRegistry::new();
    reg.install(
        ModuleVersion::new("b", vec![
            mk(),
            len_bytes(),
            at_tag(),
            at_payload(),
            bridge(),
            get(),
            fold(),
            mk2(),
            eq(),
            churn(),
            cmp(),
            hash(),
            shash(),
            enc(),
            dec_tag(),
            dec_str(),
        ])
        .expect("valid fixture module"),
    );
    reg
}

fn run_value(vm: &mut Vm, func: &str, args: &[Value]) -> Value {
    let answer = vm.run("b", func, args).expect("the fixture runs");
    let [word] = answer.as_slice() else {
        panic!("the fixture answers one word")
    };
    *word
}

/// `bytes_of` folds the byte list into one block: `len` counts the
/// payload, `byte_at` answers the ok span in range and the none span
/// out of it.
#[test]
fn bytes_of_folds_the_list_and_reads_check_the_range() {
    let mut vm = Vm::new(registry());
    let b = run_value(&mut vm, "mk", &[
        Value::I64(97),
        Value::I64(98),
        Value::I64(99),
    ]);
    let Value::Ref(id) = b else {
        panic!("bytes_of answers a reference word: {b:?}")
    };
    assert_eq!(
        id.fiber(),
        vm.objects().owner(),
        "the bytes ride this store"
    );
    assert_eq!(run_value(&mut vm, "len_bytes", &[b]), Value::I64(3));
    assert_eq!(
        run_value(&mut vm, "at_payload", &[b, Value::I64(0)]),
        Value::I64(97)
    );
    assert_eq!(
        run_value(&mut vm, "at_payload", &[b, Value::I64(2)]),
        Value::I64(99)
    );
    assert_eq!(
        run_value(&mut vm, "at_tag", &[b, Value::I64(1)]),
        Value::Tag(PLATFORM_IO_OK)
    );
    assert_eq!(
        run_value(&mut vm, "at_tag", &[b, Value::I64(3)]),
        Value::Tag(PLATFORM_NONE),
        "an out-of-range read answers the none member"
    );
    assert_eq!(
        run_value(&mut vm, "at_payload", &[b, Value::I64(7)]),
        Value::I64(7),
        "the none side carries the rejected index"
    );
}

/// `byte_list` bridges back out: the list holds one word per byte, and
/// the round trip through `bytes_of` answers the same payload count.
#[test]
fn byte_list_bridges_back_into_a_list_of_words() {
    let mut vm = Vm::new(registry());
    let b = run_value(&mut vm, "mk", &[
        Value::I64(10),
        Value::I64(20),
        Value::I64(30),
    ]);
    let list = run_value(&mut vm, "bridge", &[b]);
    assert_eq!(
        run_value(&mut vm, "get", &[list, Value::I64(0)]),
        Value::I64(10)
    );
    assert_eq!(
        run_value(&mut vm, "get", &[list, Value::I64(2)]),
        Value::I64(30)
    );
    let round = run_value(&mut vm, "fold", &[list]);
    assert_eq!(run_value(&mut vm, "len_bytes", &[round]), Value::I64(3));
}

/// A bytes value and its source list free their blocks when their
/// counts die: the explicit decs leave the object table at its baseline.
#[test]
fn a_dropped_bytes_and_its_list_free_their_blocks() {
    let mut vm = Vm::new(registry());
    let before = vm.objects().len();
    let unit = run_value(&mut vm, "churn", &[]);
    assert_eq!(unit, Value::I64(0), "the unit word rides the unit slot");
    assert_eq!(vm.objects().len(), before, "both blocks freed at the decs");
}

/// `bytes_eq` answers the structural content compare [B9]: equal
/// payloads compare equal whatever their object identity, a different
/// length or a single differing byte unequal.
#[test]
fn bytes_eq_compares_payload_content_not_identity() {
    let mut vm = Vm::new(registry());
    let abc = run_value(&mut vm, "mk", &[
        Value::I64(97),
        Value::I64(98),
        Value::I64(99),
    ]);
    let again = run_value(&mut vm, "mk", &[
        Value::I64(97),
        Value::I64(98),
        Value::I64(99),
    ]);
    assert_eq!(run_value(&mut vm, "eq", &[abc, again]), Value::Bool(true));
    // the same content across the bridge compares equal to the source
    let list = run_value(&mut vm, "bridge", &[abc]);
    let round = run_value(&mut vm, "fold", &[list]);
    assert_eq!(run_value(&mut vm, "eq", &[abc, round]), Value::Bool(true));
    // one differing byte answers false
    let swapped = run_value(&mut vm, "mk", &[
        Value::I64(97),
        Value::I64(98),
        Value::I64(100),
    ]);
    assert_eq!(
        run_value(&mut vm, "eq", &[abc, swapped]),
        Value::Bool(false)
    );
    // a different payload length answers false: a two-byte payload
    // never equals a three-byte one with the same head
    let pair = run_value(&mut vm, "mk2", &[Value::I64(97), Value::I64(98)]);
    assert_eq!(run_value(&mut vm, "eq", &[abc, pair]), Value::Bool(false));
}

/// `bytes_compare` answers the lexicographic order over the payloads,
/// a shorter prefix ordering first.
#[test]
fn bytes_compare_orders_lexicographically_prefix_first() {
    let mut vm = Vm::new(registry());
    let text = |vm: &mut Vm, s: &str| {
        let word = vm.intern_str(s);
        run_value(vm, "enc", &[word])
    };
    let ab = text(&mut vm, "ab");
    let abc = text(&mut vm, "abc");
    let ab2 = text(&mut vm, "ab");
    let ad = text(&mut vm, "ad");
    assert_eq!(run_value(&mut vm, "cmp", &[ab, abc]), Value::I64(-1));
    assert_eq!(
        run_value(&mut vm, "cmp", &[abc, ab]),
        Value::I64(1),
        "a shorter prefix orders first"
    );
    assert_eq!(run_value(&mut vm, "cmp", &[ab, ab2]), Value::I64(0));
    assert_eq!(run_value(&mut vm, "cmp", &[ab, ad]), Value::I64(-1));
    assert_eq!(run_value(&mut vm, "cmp", &[ad, ab]), Value::I64(1));
}

/// `bytes_hash` folds the payload under the hash family's one scheme:
/// it agrees with `str_hash` over the same UTF-8 bytes, so an encoded
/// string and its own bytes hash identically.
#[test]
fn bytes_hash_folds_the_payload_under_the_one_scheme() {
    let mut vm = Vm::new(registry());
    for text in ["héllo", "", "bytes face"] {
        let word = vm.intern_str(text);
        let encoded = run_value(&mut vm, "enc", &[word]);
        assert_eq!(
            run_value(&mut vm, "hash", &[encoded]),
            run_value(&mut vm, "shash", &[word]),
            "{text:?}: the encoded bytes hash like their string"
        );
    }
}

/// The UTF-8 bridges take the string's own bytes each way: `utf8_encode`
/// copies the UTF-8 payload (multi-byte scalars included), `utf8_decode`
/// validates on the way back — a valid payload answering the string,
/// an invalid sequence the `utf8_err` discriminant.
#[test]
fn the_utf8_bridges_take_the_strings_own_bytes_each_way() {
    let mut vm = Vm::new(registry());
    let word = vm.intern_str("héllo");
    let encoded = run_value(&mut vm, "enc", &[word]);
    assert_eq!(run_value(&mut vm, "len_bytes", &[encoded]), Value::I64(6));
    assert_eq!(
        run_value(&mut vm, "at_payload", &[encoded, Value::I64(1)]),
        Value::I64(0xc3),
        "the two-byte é lands in the payload verbatim"
    );
    assert_eq!(
        run_value(&mut vm, "at_payload", &[encoded, Value::I64(2)]),
        Value::I64(0xa9)
    );
    assert_eq!(
        run_value(&mut vm, "dec_tag", &[encoded]),
        Value::Tag(PLATFORM_IO_OK)
    );
    let decoded = run_value(&mut vm, "dec_str", &[encoded]);
    let Value::Ref(id) = decoded else {
        panic!("the decoded string rides a reference word: {decoded:?}")
    };
    assert_eq!(
        vm.objects().str_at(id.id()).map(str::to_owned),
        Ok("héllo".to_owned()),
        "the round trip answers the same text"
    );
    // an invalid sequence refuses: 0xc3 wants a continuation byte
    let bad = run_value(&mut vm, "mk", &[
        Value::I64(0x68),
        Value::I64(0xc3),
        Value::I64(0x28),
    ]);
    assert_eq!(
        run_value(&mut vm, "dec_tag", &[bad]),
        Value::Tag(PLATFORM_UTF8_ERR),
        "an invalid payload answers the taxonomy's utf8_err tag"
    );
}
