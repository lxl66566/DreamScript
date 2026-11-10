//! The M4 RC activation under the diag layer (C4): the 0xE words execute
//! over counted heap blocks, and the balance tripwires (hard rc
//! underflow/overflow asserts, the ds-rt diag discipline) guard a fixture
//! exercising refcounts above one — a call-staging dup hands the callee a
//! second owner, the callee's drop returns it, and the heap drains on a
//! scalar-returning program. Only compiled with `--features diag`.

#![cfg(feature = "diag")]

use ds_vm::{
    Const, FunctionDef, ModuleRegistry, ModuleVersion, Trap, Value, Vm,
    abi::{BuiltinInstance, ConstIdx, FrameLayout, Inst, RcOp, SlotType, uop::Uop},
    assemble,
};

fn k(idx: u16) -> ConstIdx {
    ConstIdx(idx)
}
/// A pool FuncRef naming a builtin core native by its contract index.
fn builtin(index: u32) -> Const {
    Const::FuncRef {
        module: "builtin".into(),
        func: index,
    }
}

fn def(name: &str, slots: &[SlotType], params: usize, args: usize, body: Vec<Inst>) -> FunctionDef {
    FunctionDef {
        name: name.into(),
        layout: FrameLayout::new(slots.to_vec(), params, args).expect("valid fixture layout"),
        pool: Vec::new(),
        code: assemble(&body).expect("fixture body encodes"),
    }
}

/// The fixture module: `main` dups a string into a call, `borrow` drops
/// its handed word at its own exit, `witness` raises a host-staged count
/// above one for direct observation, `killer` decs past one.
fn registry() -> ModuleRegistry {
    let main = def(
        "main",
        &[SlotType::Ref, SlotType::I64, SlotType::Raw],
        0,
        1,
        vec![
            Uop::Funcf { frame_slots: 3 }.into(),
            Uop::Kstr { dst: 0, kidx: k(0) }.into(),
            Uop::Mov { dst: 2, src: 0 }.into(),
            // the call-staging dup: the callee owns one count, the frame
            // keeps its own
            RcOp::Inc { src: 0 }.into(),
            Uop::Call {
                callee: k(2),
                nargs: 1,
            }
            .into(),
            Uop::Kimm { dst: 1, kidx: k(1) }.into(),
            RcOp::Dec { src: 0 }.into(),
            Uop::Ret { src: 1 }.into(),
        ],
    );
    let mut main = main;
    main.pool = vec![Const::Str("shared".into()), Const::I64(0), Const::FuncRef {
        module: "rc".into(),
        func: 1,
    }];
    let borrow = def("borrow", &[SlotType::Ref, SlotType::I64], 1, 1, vec![
        Uop::Funcf { frame_slots: 2 }.into(),
        Uop::Kimm { dst: 1, kidx: k(0) }.into(),
        // the borrow parameter drops the handed count at its exit
        RcOp::Dec { src: 0 }.into(),
        Uop::Ret { src: 1 }.into(),
    ]);
    let mut borrow = borrow;
    borrow.pool = vec![Const::I64(0)];
    let witness = def("witness", &[SlotType::Ref, SlotType::I64], 1, 0, vec![
        Uop::Funcf { frame_slots: 2 }.into(),
        RcOp::Inc { src: 0 }.into(),
        RcOp::Inc { src: 0 }.into(),
        RcOp::Dec { src: 0 }.into(),
        Uop::Kimm { dst: 1, kidx: k(0) }.into(),
        Uop::Ret { src: 1 }.into(),
    ]);
    let mut witness = witness;
    witness.pool = vec![Const::I64(0)];
    let killer = def("killer", &[SlotType::Ref, SlotType::I64], 0, 0, vec![
        Uop::Funcf { frame_slots: 2 }.into(),
        Uop::Kstr { dst: 0, kidx: k(0) }.into(),
        RcOp::Dec { src: 0 }.into(),
        Uop::Kimm { dst: 1, kidx: k(1) }.into(),
        // the second dec names the freed slot
        RcOp::Dec { src: 0 }.into(),
        Uop::Ret { src: 1 }.into(),
    ]);
    let mut killer = killer;
    killer.pool = vec![Const::Str("doomed".into()), Const::I64(0)];
    let mut reg = ModuleRegistry::new();
    reg.install(
        ModuleVersion::new("rc", vec![main, borrow, witness, killer])
            .expect("valid fixture module"),
    );
    reg
}

/// The map retain contract under the diag tripwires: `map_set` acquires one
/// count per stored `Ref` word (keys and values both), the overwrite drops
/// the replaced value's count, a checked-read hit hands the payload out
/// with its own count, and the map's unique death drops exactly the counts
/// it stored — the heap drains on a scalar-returning program.
#[test]
fn a_map_of_strings_retains_and_frees_dec_per_element() {
    let mut reg = ModuleRegistry::new();
    let body = def(
        "map_cycle",
        &[
            SlotType::Ref,
            SlotType::Ref,
            SlotType::Ref,
            SlotType::I64,
            SlotType::Raw,
            SlotType::Raw,
            SlotType::Raw,
            SlotType::Raw,
        ],
        0,
        4,
        vec![
            Uop::Funcf { frame_slots: 8 }.into(),
            // the empty map, owned by this frame
            Uop::Call {
                callee: k(0),
                nargs: 0,
            }
            .into(),
            Uop::Mov { dst: 0, src: 4 }.into(),
            // the first pair: map_set acquires the key and value counts
            Uop::Kstr { dst: 1, kidx: k(1) }.into(),
            Uop::Kstr { dst: 2, kidx: k(2) }.into(),
            Uop::Mov { dst: 4, src: 0 }.into(),
            Uop::Mov { dst: 5, src: 1 }.into(),
            Uop::Mov { dst: 6, src: 2 }.into(),
            Uop::Call {
                callee: k(3),
                nargs: 3,
            }
            .into(),
            RcOp::Dec { src: 1 }.into(),
            RcOp::Dec { src: 2 }.into(),
            // a checked-read hit: the payload rides out with its own
            // count; the generic body stages two evidence words
            Uop::Kstr { dst: 1, kidx: k(1) }.into(),
            Uop::Mov { dst: 4, src: 0 }.into(),
            Uop::Mov { dst: 5, src: 1 }.into(),
            Uop::Kimm { dst: 6, kidx: k(9) }.into(),
            Uop::Kimm { dst: 7, kidx: k(9) }.into(),
            Uop::Callg {
                callee: k(4),
                nargs: 2,
            }
            .into(),
            RcOp::Dec { src: 1 }.into(),
            Uop::Mov { dst: 1, src: 5 }.into(),
            RcOp::Dec { src: 1 }.into(),
            // the second pair, then an overwrite under the same key: the
            // replaced value's stored count drops to zero and frees
            Uop::Kstr { dst: 1, kidx: k(5) }.into(),
            Uop::Kstr { dst: 2, kidx: k(6) }.into(),
            Uop::Mov { dst: 4, src: 0 }.into(),
            Uop::Mov { dst: 5, src: 1 }.into(),
            Uop::Mov { dst: 6, src: 2 }.into(),
            Uop::Call {
                callee: k(3),
                nargs: 3,
            }
            .into(),
            RcOp::Dec { src: 1 }.into(),
            RcOp::Dec { src: 2 }.into(),
            Uop::Kstr { dst: 1, kidx: k(5) }.into(),
            Uop::Kstr { dst: 2, kidx: k(7) }.into(),
            Uop::Mov { dst: 4, src: 0 }.into(),
            Uop::Mov { dst: 5, src: 1 }.into(),
            Uop::Mov { dst: 6, src: 2 }.into(),
            Uop::Call {
                callee: k(3),
                nargs: 3,
            }
            .into(),
            RcOp::Dec { src: 1 }.into(),
            RcOp::Dec { src: 2 }.into(),
            // the entry count rides out, then the map's unique death;
            // map_len is the generic body with two evidence words
            Uop::Mov { dst: 4, src: 0 }.into(),
            Uop::Kimm { dst: 5, kidx: k(9) }.into(),
            Uop::Kimm { dst: 6, kidx: k(9) }.into(),
            Uop::Callg {
                callee: k(8),
                nargs: 1,
            }
            .into(),
            Uop::Mov { dst: 3, src: 4 }.into(),
            RcOp::Dec { src: 0 }.into(),
            Uop::Ret { src: 3 }.into(),
        ],
    );
    let mut body = body;
    body.pool = vec![
        builtin(ds_vm::native::BUILTIN_MAP_NEW),
        Const::Str("alpha".into()),
        Const::Str("one".into()),
        builtin(ds_vm::native::BUILTIN_MAP_SET),
        builtin(ds_vm::native::BUILTIN_MAP_GET),
        Const::Str("beta".into()),
        Const::Str("two".into()),
        Const::Str("replaced".into()),
        builtin(ds_vm::native::BUILTIN_MAP_LEN),
        Const::I64(BuiltinInstance::Map as i64), // k9: the evidence word
    ];
    reg.install(ModuleVersion::new("rcm", vec![body]).expect("valid fixture module"));
    let mut vm = Vm::new(reg);
    assert_eq!(
        vm.run("rcm", "map_cycle", &[]),
        Ok(Value::I64(2)),
        "the entry count answers two after the overwrite"
    );
    // every stored count dropped at the map's death: the heap holds
    // nothing live
    assert!(vm.objects().is_empty(), "the heap drains when the run ends");
}

#[test]
fn a_call_staging_dup_balances_and_the_heap_drains() {
    let mut vm = Vm::new(registry());
    assert_eq!(
        vm.run("rc", "main", &[]),
        Ok(Value::I64(0)),
        "the dup/drop pair balances through the call"
    );
    // main returns a scalar and every claimed count dropped: the freed
    // slot waits for reuse, the heap holds nothing live
    assert!(vm.objects().is_empty(), "the heap drains when the run ends");
}

#[test]
fn a_raised_count_is_observable_and_returns_to_one() {
    let mut vm = Vm::new(registry());
    let text = vm.intern_str("x"); // the host's claim: rc 1
    assert_eq!(
        vm.run("rc", "witness", &[text]),
        Ok(Value::I64(0)),
        "the raised counts execute"
    );
    let Value::Ref(id) = text else {
        panic!("an interned string ref");
    };
    // two incs and one dec over the host's single claim: 3 - 1 = 2, and
    // the object is alive to tell about it
    assert_eq!(
        vm.objects().rc_at(id),
        Ok(2),
        "the count rose above one and settled at two"
    );
}

#[test]
fn a_dec_naming_a_freed_object_traps() {
    let mut vm = Vm::new(registry());
    assert_eq!(
        vm.run("rc", "killer", &[]),
        Err(Trap::UnknownObject { obj: 0 }),
        "the second dec names the freed slot"
    );
}

/// The checked-read span stays count-balanced when the hit and miss
/// paths run repeatedly: 25 loop iterations each drive one list hit,
/// one list miss, one map hit, and one map miss, every path dec'ing
/// exactly the count its arm claimed — the miss side's interned zero-init
/// claim dies inside its arm (the balanced branch join the placement
/// pass lowers), the hit side releases the native's handed-out count.
/// The diag tripwires guard every dec, and the scalar-returning frame
/// leaves the heap drained.
#[test]
fn checked_read_spans_balance_hit_and_miss_counts_across_iterations() {
    let mut b: Vec<Inst> = Vec::new();
    macro_rules! op {
        ($i:expr) => {
            b.push($i.into());
        };
    }
    op!(Uop::Funcf { frame_slots: 13 });
    // the list: list_new, one pushed element
    op!(Uop::Call {
        callee: k(0),
        nargs: 0
    });
    op!(Uop::Mov { dst: 0, src: 9 });
    op!(Uop::Kstr { dst: 2, kidx: k(1) });
    op!(Uop::Mov { dst: 9, src: 0 });
    op!(Uop::Mov { dst: 10, src: 2 });
    op!(Uop::Call {
        callee: k(2),
        nargs: 2
    });
    op!(RcOp::Dec { src: 2 });
    // the map: one stored pair
    op!(Uop::Call {
        callee: k(6),
        nargs: 0
    });
    op!(Uop::Mov { dst: 1, src: 9 });
    op!(Uop::Kstr { dst: 5, kidx: k(7) });
    op!(Uop::Kstr { dst: 2, kidx: k(8) });
    op!(Uop::Mov { dst: 9, src: 1 });
    op!(Uop::Mov { dst: 10, src: 5 });
    op!(Uop::Mov { dst: 11, src: 2 });
    op!(Uop::Call {
        callee: k(9),
        nargs: 3
    });
    op!(RcOp::Dec { src: 5 });
    op!(RcOp::Dec { src: 2 });
    // the counters: i = 0, bound = 25, zero = 0
    op!(Uop::Kimm { dst: 3, kidx: k(4) });
    op!(Uop::Kimm { dst: 7, kidx: k(5) });
    op!(Uop::Kimm { dst: 8, kidx: k(4) });
    let head = b.len() as u32;
    op!(Uop::LtI64 {
        dst: 4,
        lhs: 3,
        rhs: 7
    });
    let end_jmp = b.len();
    op!(Uop::JmpifNot { cond: 4, target: 0 });
    // one checked-read leg: stage, call, tag-test, then each side decs
    // the count its own path claimed before the join
    macro_rules! read_leg {
        ($stage:expr, $hit_mov:expr) => {{
            $stage
            op!(Uop::TagTest { dst: 4, src: 9, tag: ds_vm::native::PLATFORM_IO_OK });
            let hit = b.len();
            op!(Uop::Jmpif { cond: 4, target: 0 });
            // the miss side: the none arm's zero-init claim
            op!(Uop::Kstr { dst: 2, kidx: k(11) });
            let join_jmp = b.len();
            op!(Uop::Jmp { target: 0 });
            let hit_at = b.len() as u32;
            b[hit] = Uop::Jmpif { cond: 4, target: hit_at }.into();
            $hit_mov
            let join_at = b.len() as u32;
            b[join_jmp] = Uop::Jmp { target: join_at }.into();
            op!(RcOp::Dec { src: 2 });
        }};
    }
    // the list hit (index 0)
    read_leg!(
        {
            op!(Uop::Mov { dst: 9, src: 0 });
            op!(Uop::Mov { dst: 10, src: 8 });
            op!(Uop::Kimm {
                dst: 11,
                kidx: k(15)
            });
            op!(Uop::Callg {
                callee: k(3),
                nargs: 2
            });
        },
        {
            op!(Uop::Mov { dst: 2, src: 10 });
        }
    );
    // the list miss (index 99)
    op!(Uop::Mov { dst: 9, src: 0 });
    op!(Uop::Kimm {
        dst: 8,
        kidx: k(12)
    });
    op!(Uop::Mov { dst: 10, src: 8 });
    op!(Uop::Kimm {
        dst: 11,
        kidx: k(15)
    });
    op!(Uop::Callg {
        callee: k(3),
        nargs: 2
    });
    op!(Uop::TagTest {
        dst: 4,
        src: 9,
        tag: ds_vm::native::PLATFORM_IO_OK
    });
    let hit2 = b.len();
    op!(Uop::Jmpif { cond: 4, target: 0 });
    op!(Uop::Kstr {
        dst: 2,
        kidx: k(11)
    });
    let join2_jmp = b.len();
    op!(Uop::Jmp { target: 0 });
    let hit2_at = b.len() as u32;
    b[hit2] = Uop::Jmpif {
        cond: 4,
        target: hit2_at,
    }
    .into();
    op!(Uop::Mov { dst: 2, src: 10 });
    let join2_at = b.len() as u32;
    b[join2_jmp] = Uop::Jmp { target: join2_at }.into();
    op!(RcOp::Dec { src: 2 });
    op!(Uop::Kimm { dst: 8, kidx: k(4) });
    // the map hit ("host"): the key borrow decs after the read too
    read_leg!(
        {
            op!(Uop::Kstr { dst: 5, kidx: k(7) });
            op!(Uop::Mov { dst: 9, src: 1 });
            op!(Uop::Mov { dst: 10, src: 5 });
            op!(Uop::Kimm {
                dst: 11,
                kidx: k(16)
            });
            op!(Uop::Kimm {
                dst: 12,
                kidx: k(16)
            });
            op!(Uop::Callg {
                callee: k(10),
                nargs: 2
            });
        },
        {
            op!(Uop::Mov { dst: 2, src: 10 });
        }
    );
    op!(RcOp::Dec { src: 5 });
    // the map miss ("user")
    read_leg!(
        {
            op!(Uop::Kstr {
                dst: 5,
                kidx: k(13)
            });
            op!(Uop::Mov { dst: 9, src: 1 });
            op!(Uop::Mov { dst: 10, src: 5 });
            op!(Uop::Kimm {
                dst: 11,
                kidx: k(16)
            });
            op!(Uop::Kimm {
                dst: 12,
                kidx: k(16)
            });
            op!(Uop::Callg {
                callee: k(10),
                nargs: 2
            });
        },
        {
            op!(Uop::Mov { dst: 2, src: 10 });
        }
    );
    op!(RcOp::Dec { src: 5 });
    // i += 1, then the back-edge
    op!(Uop::Kimm {
        dst: 8,
        kidx: k(14)
    });
    op!(Uop::AddI64 {
        dst: 3,
        lhs: 3,
        rhs: 8
    });
    op!(Uop::Jmp { target: head });
    let end_at = b.len() as u32;
    b[end_jmp] = Uop::JmpifNot {
        cond: 4,
        target: end_at,
    }
    .into();
    // teardown: the scalar return, then the two collection deaths
    op!(Uop::Kimm { dst: 6, kidx: k(4) });
    op!(RcOp::Dec { src: 0 });
    op!(RcOp::Dec { src: 1 });
    op!(Uop::Ret { src: 6 });
    let reads = def(
        "reads",
        &[
            SlotType::Ref,
            SlotType::Ref,
            SlotType::Ref,
            SlotType::I64,
            SlotType::Bool,
            SlotType::Ref,
            SlotType::I64,
            SlotType::I64,
            SlotType::I64,
            SlotType::Raw,
            SlotType::Raw,
            SlotType::Raw,
            SlotType::Raw,
        ],
        0,
        4,
        b,
    );
    let mut reads = reads;
    reads.pool = vec![
        builtin(ds_vm::native::BUILTIN_LIST_NEW),
        Const::Str("alpha".into()),
        builtin(ds_vm::native::BUILTIN_LIST_PUSH),
        builtin(ds_vm::native::BUILTIN_LIST_GET),
        Const::I64(0),
        Const::I64(25),
        builtin(ds_vm::native::BUILTIN_MAP_NEW),
        Const::Str("host".into()),
        Const::Str("localhost".into()),
        builtin(ds_vm::native::BUILTIN_MAP_SET),
        builtin(ds_vm::native::BUILTIN_MAP_GET),
        Const::Str(String::new()),
        Const::I64(99),
        Const::Str("user".into()),
        Const::I64(1),
        Const::I64(BuiltinInstance::List as i64), // k15: the list evidence word
        Const::I64(BuiltinInstance::Map as i64),  // k16: the map evidence word
    ];
    let mut reg = ModuleRegistry::new();
    reg.install(ModuleVersion::new("rcr", vec![reads]).expect("valid fixture module"));
    let mut vm = Vm::new(reg);
    assert_eq!(
        vm.run("rcr", "reads", &[]),
        Ok(Value::I64(0)),
        "100 checked reads across 25 iterations ran clean under the tripwires"
    );
    assert!(
        vm.objects().is_empty(),
        "every claimed count dropped across the hit and miss iterations"
    );
}

/// The generic container closes' retain contract under the diag
/// tripwires: `cons` adopts its prepend and copies each element with
/// one acquired count (construction copies), the checked first-element
/// body (`head` and `first`, two spellings of the one body) hands the
/// payload out with its own count, and every frame-owned word drops at
/// its exit — the heap drains on a scalar-returning program.
#[test]
fn cons_copies_and_the_checked_first_element_hands_out_its_count() {
    let mut reg = ModuleRegistry::new();
    let body = def(
        "container_cycle",
        &[
            SlotType::Ref,
            SlotType::Ref,
            SlotType::Ref,
            SlotType::I64,
            SlotType::Raw,
            SlotType::Raw,
            SlotType::Raw,
        ],
        0,
        3,
        vec![
            Uop::Funcf { frame_slots: 7 }.into(),
            // the string element, one claim owned by this frame
            Uop::Kstr { dst: 0, kidx: k(0) }.into(),
            // the empty list, owned by this frame
            Uop::Call {
                callee: k(2),
                nargs: 0,
            }
            .into(),
            Uop::Mov { dst: 1, src: 4 }.into(),
            // cons("a", empty): the fresh list adopts the prepend and
            // copies the source's element words with acquired counts
            Uop::Mov { dst: 4, src: 0 }.into(),
            Uop::Mov { dst: 5, src: 1 }.into(),
            Uop::Kimm { dst: 6, kidx: k(1) }.into(),
            Uop::Callg {
                callee: k(3),
                nargs: 2,
            }
            .into(),
            Uop::Mov { dst: 2, src: 4 }.into(),
            // the checked first element: the payload rides out with its
            // own count, and the frame drops the handed word
            Uop::Mov { dst: 4, src: 2 }.into(),
            Uop::Kimm { dst: 5, kidx: k(1) }.into(),
            Uop::Callg {
                callee: k(4),
                nargs: 1,
            }
            .into(),
            RcOp::Dec { src: 5 }.into(),
            // `first` over the same list: the other spelling of the one
            // body, the same handed-out count to drop
            Uop::Mov { dst: 4, src: 2 }.into(),
            Uop::Kimm { dst: 5, kidx: k(1) }.into(),
            Uop::Callg {
                callee: k(5),
                nargs: 1,
            }
            .into(),
            RcOp::Dec { src: 5 }.into(),
            // the scalar exit, then every frame-owned word drops: the
            // cons result's death frees its element claim, then the
            // empty list and the birth claim
            Uop::Kimm { dst: 3, kidx: k(6) }.into(),
            RcOp::Dec { src: 2 }.into(),
            RcOp::Dec { src: 1 }.into(),
            RcOp::Dec { src: 0 }.into(),
            Uop::Ret { src: 3 }.into(),
        ],
    );
    let mut body = body;
    body.pool = vec![
        Const::Str("a".into()),
        Const::I64(BuiltinInstance::List as i64),
        builtin(ds_vm::native::BUILTIN_LIST_NEW),
        builtin(ds_vm::native::BUILTIN_CONS),
        builtin(ds_vm::native::BUILTIN_HEAD),
        builtin(ds_vm::native::BUILTIN_FIRST),
        Const::I64(0),
    ];
    reg.install(ModuleVersion::new("rcc", vec![body]).expect("valid fixture module"));
    let mut vm = Vm::new(reg);
    assert_eq!(
        vm.run("rcc", "container_cycle", &[]),
        Ok(Value::I64(0)),
        "the cons/read/drop cycle ran clean under the tripwires"
    );
    assert!(
        vm.objects().is_empty(),
        "the copied element claim, both handed-out counts, and the lists all dropped"
    );
}

/// The copy producers' retain contract: `tail` and `collect` each answer
/// a fresh list acquiring one count per copied `Ref` element, and each
/// frame-owned word drops at its exit — the source list's unique death
/// frees its element claims, and the heap drains.
#[test]
fn tail_and_collect_copies_balance_per_element() {
    let mut reg = ModuleRegistry::new();
    let body = def(
        "copy_cycle",
        &[
            SlotType::Ref,
            SlotType::Ref,
            SlotType::Ref,
            SlotType::Ref,
            SlotType::Ref,
            SlotType::I64,
            SlotType::Raw,
            SlotType::Raw,
            SlotType::Raw,
        ],
        0,
        3,
        vec![
            Uop::Funcf { frame_slots: 9 }.into(),
            // two string elements, each one claim owned by this frame
            Uop::Kstr { dst: 0, kidx: k(0) }.into(),
            Uop::Kstr { dst: 1, kidx: k(1) }.into(),
            // the source list: list_new plus two in-place appends, each
            // stored word acquiring its own count
            Uop::Call {
                callee: k(3),
                nargs: 0,
            }
            .into(),
            Uop::Mov { dst: 2, src: 6 }.into(),
            Uop::Mov { dst: 6, src: 2 }.into(),
            Uop::Mov { dst: 7, src: 0 }.into(),
            Uop::Call {
                callee: k(4),
                nargs: 2,
            }
            .into(),
            Uop::Mov { dst: 6, src: 2 }.into(),
            Uop::Mov { dst: 7, src: 1 }.into(),
            Uop::Call {
                callee: k(4),
                nargs: 2,
            }
            .into(),
            // tail: a fresh list over the elements after the first
            Uop::Mov { dst: 6, src: 2 }.into(),
            Uop::Kimm { dst: 7, kidx: k(2) }.into(),
            Uop::Callg {
                callee: k(5),
                nargs: 1,
            }
            .into(),
            Uop::Mov { dst: 3, src: 6 }.into(),
            // collect: a plain fresh copy of the tail
            Uop::Mov { dst: 6, src: 3 }.into(),
            Uop::Kimm { dst: 7, kidx: k(2) }.into(),
            Uop::Callg {
                callee: k(6),
                nargs: 1,
            }
            .into(),
            Uop::Mov { dst: 4, src: 6 }.into(),
            // the scalar exit, then every frame-owned word drops: both
            // copies' deaths free their element claims, then the source
            // list's death frees its own, then the birth claims
            Uop::Kimm { dst: 5, kidx: k(7) }.into(),
            RcOp::Dec { src: 4 }.into(),
            RcOp::Dec { src: 3 }.into(),
            RcOp::Dec { src: 2 }.into(),
            RcOp::Dec { src: 1 }.into(),
            RcOp::Dec { src: 0 }.into(),
            Uop::Ret { src: 5 }.into(),
        ],
    );
    let mut body = body;
    body.pool = vec![
        Const::Str("a".into()),
        Const::Str("b".into()),
        Const::I64(BuiltinInstance::List as i64),
        builtin(ds_vm::native::BUILTIN_LIST_NEW),
        builtin(ds_vm::native::BUILTIN_LIST_PUSH),
        builtin(ds_vm::native::BUILTIN_TAIL),
        builtin(ds_vm::native::BUILTIN_COLLECT),
        Const::I64(0),
    ];
    reg.install(ModuleVersion::new("rct", vec![body]).expect("valid fixture module"));
    let mut vm = Vm::new(reg);
    assert_eq!(
        vm.run("rct", "copy_cycle", &[]),
        Ok(Value::I64(0)),
        "the tail/collect copy cycle ran clean under the tripwires"
    );
    assert!(
        vm.objects().is_empty(),
        "every copied, stored, and birth-claimed count dropped"
    );
}
