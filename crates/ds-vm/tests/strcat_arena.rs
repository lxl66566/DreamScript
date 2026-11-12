//! STRCAT_ARENA (stage three's temporary slice): the count-free
//! concatenation serving the placement pass emits for designated
//! temporaries, with ARENA_MARK/ARENA_RESTORE reclaiming each loop
//! iteration's servings at the back edge. The pins here are: the
//! serving equates its content against an acquiring heap word across
//! the two domains and leaves no count behind, every count-bearing
//! funnel refuses the arena word, the 24-bit length boundary traps by
//! name before the allocation, a hand-packed restore cursor names its
//! misuse class, and the back-edge restore keeps the arena bounded
//! where a restore-less spelling of the same loop grows it
//! unboundedly.

use ds_vm::{
    ArenaMisuse, ArenaPackError, Const, ConstKind, FunctionDef, ModuleBuildError, ModuleRegistry,
    ModuleVersion, Value, Vm,
    abi::{RcOp, SlotType, uop::Uop},
    assemble,
};

fn k(idx: u16) -> ds_vm::abi::ConstIdx {
    ds_vm::abi::ConstIdx(idx)
}

fn def(name: &str, slots: &[SlotType], body: &[ds_vm::abi::Inst]) -> FunctionDef {
    FunctionDef {
        name: name.into(),
        layout: ds_vm::abi::FrameLayout::new(slots.to_vec(), 0, 0).expect("valid fixture layout"),
        pool: Vec::new(),
        code: assemble(body).expect("fixture body encodes"),
    }
}

/// The serving pin: two borrowed operands concatenate into an arena
/// word, the arena word equates its content against an acquiring heap
/// word of the same text (the comparison reads both domains), and the
/// frame drains at return - the arena word never held a count, and the
/// heap word's count dies with the frame.
#[test]
fn a_designated_concat_serves_and_equates_across_domains() {
    let mut main = def(
        "main",
        &[
            SlotType::Ref,
            SlotType::Ref,
            SlotType::Ref,
            SlotType::Ref,
            SlotType::Bool,
            SlotType::Ref,
            SlotType::Bool,
            SlotType::I64,
        ],
        &[
            Uop::Funcf { frame_slots: 8 }.into(),
            Uop::KstrBorrow { dst: 0, kidx: k(0) }.into(),
            Uop::KstrBorrow { dst: 1, kidx: k(1) }.into(),
            Uop::StrCatArena {
                dst: 2,
                lhs: 0,
                rhs: 1,
            }
            .into(),
            Uop::Kstr { dst: 3, kidx: k(2) }.into(),
            Uop::EqStr {
                dst: 4,
                lhs: 2,
                rhs: 3,
            }
            .into(),
            Uop::Kstr { dst: 5, kidx: k(3) }.into(),
            Uop::NeqStr {
                dst: 6,
                lhs: 2,
                rhs: 5,
            }
            .into(),
            // the heap words dec to zero while the arena word serves on
            RcOp::Dec { src: 3 }.into(),
            RcOp::Dec { src: 5 }.into(),
            // both comparisons must hold for the witness to land
            Uop::JmpifNot {
                cond: 4,
                target: 14,
            }
            .into(),
            Uop::JmpifNot {
                cond: 6,
                target: 14,
            }
            .into(),
            Uop::Kimm { dst: 7, kidx: k(4) }.into(),
            Uop::Ret { src: 7 }.into(),
            Uop::Kimm { dst: 7, kidx: k(5) }.into(),
            Uop::Ret { src: 7 }.into(),
            // both comparisons must hold for the witness to land
            Uop::JmpifNot {
                cond: 4,
                target: 14,
            }
            .into(),
            Uop::JmpifNot {
                cond: 6,
                target: 14,
            }
            .into(),
            Uop::Kimm { dst: 7, kidx: k(4) }.into(),
            Uop::Ret { src: 7 }.into(),
            Uop::Kimm { dst: 7, kidx: k(5) }.into(),
            Uop::Ret { src: 7 }.into(),
        ],
    );
    main.pool = vec![
        Const::Str("ab".into()),
        Const::Str("cd".into()),
        Const::Str("abcd".into()),
        Const::Str("other".into()),
        Const::I64(1),
        Const::I64(0),
    ];
    let mut reg = ModuleRegistry::new();
    reg.install(ModuleVersion::new("strcat_arena", vec![main]).expect("valid fixture module"));
    let mut vm = Vm::new(reg);
    assert_eq!(
        vm.run("strcat_arena", "main", &[]),
        Ok(Value::I64(1)),
        "the arena word equates across domains and differs from other text"
    );
    assert!(
        vm.objects().is_empty(),
        "the arena word held no count and the heap words' counts died at their decs: {} live",
        vm.objects().len()
    );
}

/// The handoff tripwire: a dec reads its operand through the counted
/// funnel, and the funnel refuses an arena word by name - the placement
/// proof keeps every designated temporary away from such sites, so only
/// a misclassified lowering ever trips it.
#[test]
fn a_count_bearing_dec_refuses_an_arena_word() {
    let mut main = def("main", &[SlotType::Ref, SlotType::Ref, SlotType::Ref], &[
        Uop::Funcf { frame_slots: 3 }.into(),
        Uop::KstrBorrow { dst: 0, kidx: k(0) }.into(),
        Uop::KstrBorrow { dst: 1, kidx: k(1) }.into(),
        Uop::StrCatArena {
            dst: 2,
            lhs: 0,
            rhs: 1,
        }
        .into(),
        RcOp::Dec { src: 2 }.into(),
        Uop::Ret { src: 2 }.into(),
    ]);
    main.pool = vec![Const::Str("ab".into()), Const::Str("cd".into())];
    let mut reg = ModuleRegistry::new();
    reg.install(ModuleVersion::new("strcat_arena", vec![main]).expect("valid fixture module"));
    let mut vm = Vm::new(reg);
    assert_eq!(
        vm.run("strcat_arena", "main", &[]),
        Err(ds_vm::Trap::ArenaHandoff { slot: 2 }),
        "the counted funnel refuses the arena word instead of walking its handle"
    );
}

/// The length boundary: a concatenation at or past the 24-bit length
/// field traps by name before the allocation, instead of truncating
/// into a handle that would misreport its own extent.
#[test]
fn an_oversized_concat_traps_at_the_length_field() {
    let half = (1 << 23) + 16;
    let mut main = def("main", &[SlotType::Ref, SlotType::Ref, SlotType::Ref], &[
        Uop::Funcf { frame_slots: 3 }.into(),
        Uop::KstrBorrow { dst: 0, kidx: k(0) }.into(),
        Uop::KstrBorrow { dst: 1, kidx: k(1) }.into(),
        Uop::StrCatArena {
            dst: 2,
            lhs: 0,
            rhs: 1,
        }
        .into(),
        Uop::Ret { src: 2 }.into(),
    ]);
    main.pool = vec![Const::Str("a".repeat(half)), Const::Str("b".repeat(half))];
    let mut reg = ModuleRegistry::new();
    reg.install(ModuleVersion::new("strcat_arena", vec![main]).expect("valid fixture module"));
    let mut vm = Vm::new(reg);
    assert_eq!(
        vm.run("strcat_arena", "main", &[]),
        Err(ds_vm::Trap::ArenaExhausted {
            field: ArenaPackError::Length
        }),
        "the pair fills the 24-bit length field and refuses to pack"
    );
}

/// The hand-packed cursor: a restore cursor whose chunk field sits past
/// the arena's growth names the chunk misuse class - the packed layout
/// refuses lies structurally instead of walking a fabricated mark.
#[test]
fn a_restore_cursor_past_the_growth_names_the_chunk_class() {
    let mut main = def(
        "main",
        &[
            SlotType::Ref,
            SlotType::Ref,
            SlotType::Ref,
            SlotType::I64,
            SlotType::I64,
        ],
        &[
            Uop::Funcf { frame_slots: 5 }.into(),
            Uop::KstrBorrow { dst: 0, kidx: k(0) }.into(),
            Uop::KstrBorrow { dst: 1, kidx: k(1) }.into(),
            Uop::StrCatArena {
                dst: 2,
                lhs: 0,
                rhs: 1,
            }
            .into(),
            Uop::ArenaMark { dst: 3 }.into(),
            // add one to the cursor's chunk field (bits 48..64)
            Uop::Kimm { dst: 4, kidx: k(2) }.into(),
            Uop::AddI64 {
                dst: 3,
                lhs: 3,
                rhs: 4,
            }
            .into(),
            Uop::ArenaRestore { src: 3 }.into(),
            Uop::Ret { src: 3 }.into(),
        ],
    );
    main.pool = vec![
        Const::Str("ab".into()),
        Const::Str("cd".into()),
        Const::I64(1 << 48),
    ];
    let mut reg = ModuleRegistry::new();
    reg.install(ModuleVersion::new("strcat_arena", vec![main]).expect("valid fixture module"));
    let mut vm = Vm::new(reg);
    assert_eq!(
        vm.run("strcat_arena", "main", &[]),
        Err(ds_vm::Trap::ArenaHandle {
            class: ArenaMisuse::Chunk
        }),
        "the restore cursor names a chunk the arena never grew"
    );
}

/// The reclamation witness: one loop, 20k iterations, a 200-byte
/// arena serving per iteration. The `restoring` spelling carries the
/// mark/restore pair (head and back edge); the `leaking` spelling is
/// identical minus the restore. Both trap on the same deliberate type
/// mismatch after the loop, so the arena snapshot read at the trap is
/// the loop's high-water state: the restore keeps the arena at one
/// iteration's footprint, while the restore-less spelling grows past
/// every reasonable bound.
#[test]
fn the_back_edge_restore_keeps_the_loop_footprint_bounded() {
    fn registry(restoring: bool) -> ModuleRegistry {
        let mut body: Vec<ds_vm::abi::Inst> = vec![Uop::Funcf { frame_slots: 8 }.into()];
        body.push(Uop::Kimm { dst: 3, kidx: k(0) }.into()); // i = 0
        body.push(Uop::Kimm { dst: 5, kidx: k(1) }.into()); // one
        body.push(Uop::Kimm { dst: 6, kidx: k(2) }.into()); // limit
        let head = body.len();
        body.push(Uop::ArenaMark { dst: 7 }.into()); // head mark
        body.push(Uop::KstrBorrow { dst: 1, kidx: k(3) }.into());
        body.push(Uop::KstrBorrow { dst: 2, kidx: k(4) }.into());
        body.push(
            Uop::StrCatArena {
                dst: 0,
                lhs: 1,
                rhs: 2,
            }
            .into(),
        );
        if restoring {
            body.push(Uop::ArenaRestore { src: 7 }.into()); // back edge
        }
        body.push(
            Uop::AddI64 {
                dst: 3,
                lhs: 3,
                rhs: 5,
            }
            .into(),
        );
        body.push(
            Uop::LtI64 {
                dst: 4,
                lhs: 3,
                rhs: 6,
            }
            .into(),
        );
        body.push(
            Uop::Jmpif {
                cond: 4,
                target: u32::try_from(head).expect("loop head fits the u32 target"),
            }
            .into(),
        );
        // deliberate mismatch: freeze the machine with the loop's arena
        // state still in place so the snapshot witnesses the footprint
        body.push(
            Uop::EqStr {
                dst: 4,
                lhs: 3,
                rhs: 3,
            }
            .into(),
        );
        body.push(Uop::Ret { src: 3 }.into());
        let mut main = FunctionDef {
            name: "main".into(),
            layout: ds_vm::abi::FrameLayout::new(
                vec![
                    SlotType::Ref,
                    SlotType::Ref,
                    SlotType::Ref,
                    SlotType::I64,
                    SlotType::Bool,
                    SlotType::I64,
                    SlotType::I64,
                    SlotType::I64,
                ],
                0,
                0,
            )
            .expect("valid fixture layout"),
            pool: Vec::new(),
            code: assemble(&body).expect("fixture body encodes"),
        };
        main.pool = vec![
            Const::I64(0),
            Const::I64(1),
            Const::I64(20_000),
            Const::Str("a".repeat(100)),
            Const::Str("b".repeat(100)),
        ];
        let mut reg = ModuleRegistry::new();
        let name = if restoring { "restore" } else { "leak" };
        reg.install(ModuleVersion::new(name, vec![main]).expect("valid fixture module"));
        reg
    }
    for (restoring, want_bounded) in [(true, true), (false, false)] {
        let mut vm = Vm::new(registry(restoring));
        let name = if restoring { "restore" } else { "leak" };
        assert!(
            matches!(
                vm.run(name, "main", &[]),
                Err(ds_vm::Trap::SlotTypeMismatch { slot: 3, .. })
            ),
            "both spellings freeze at the deliberate mismatch"
        );
        let used = vm.arena_stats().used_bytes;
        if want_bounded {
            assert!(
                used < 100_000,
                "the restore holds one iteration's footprint: {used} bytes in use"
            );
        } else {
            assert!(
                used > 1_000_000,
                "the restore-less spelling grows past every bound: {used} bytes in use"
            );
        }
    }
}

/// The pool-kind pin: the operand borrow loads behind a STRCAT_ARENA
/// read their pool entries through the same loader check an acquiring
/// KSTR load applies, so a non-string entry fails validation, not the
/// run.
#[test]
fn an_operand_borrow_load_validates_the_pool_kind() {
    let mut main = def("main", &[SlotType::Ref, SlotType::I64], &[
        Uop::Funcf { frame_slots: 2 }.into(),
        Uop::KstrBorrow { dst: 0, kidx: k(0) }.into(),
        Uop::Kimm { dst: 1, kidx: k(1) }.into(),
        Uop::Ret { src: 1 }.into(),
    ]);
    main.pool = vec![Const::I64(0), Const::I64(0)];
    match ModuleVersion::new("bad_pool", vec![main]) {
        Err(ModuleBuildError::PoolKind {
            kidx: 0,
            expected: ConstKind::Str,
            found: ConstKind::I64,
            ..
        }) => {},
        other => panic!("the borrow load refuses a non-string pool entry at validation: {other:?}"),
    }
}
