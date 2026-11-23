//! Byte-stable write-then-read round trips `[G3.5 acceptance]`.

use ds_abi::{
    FrameLayout, Inst, SlotType,
    uop::{ConstIdx, Uop},
};
use ds_mod::{
    Constant, Flags, FuncRefKey, Function, FunctionError, Hash128, Module, TargetClass, read,
    read_checked, read_mmap, write,
};

const SOURCE: &[u8] = b"fn even(n: int) -> Bool {\n  n == 0 || odd(n - 1)\n}\n";

/// The canonical word of one instruction at `pc`.
fn encoded(uop: Uop, pc: u32) -> u32 {
    Inst::Uop(uop)
        .encode(pc)
        .expect("fixture instruction encodes")
}

/// The canonical words of a body, each encoded at its own pc.
fn words(body: Vec<Uop>) -> Vec<u32> {
    body.into_iter()
        .enumerate()
        .map(|(pc, uop)| encoded(uop, u32::try_from(pc).expect("pc fits u32")))
        .collect()
}

fn sample_module() -> Module {
    Module::new(
        "pipeline",
        TargetClass::Triple("x86_64-pc-windows-msvc".to_owned()),
        Hash128::compute(SOURCE),
    )
    .with_dep("std", Hash128::compute(b"std package content"))
    .with_dep("csv", Hash128::compute(b"csv package content"))
    .with_constant(Constant::I64(-42))
    .with_constant(Constant::F64(f64::from_bits(0x7ff8_0000_dead_beef))) // NaN payload
    .with_constant(Constant::F64(3.25))
    .with_constant(Constant::Bool(true))
    .with_constant(Constant::Bool(false))
    .with_constant(Constant::Str("h\u{e9}llo \u{0} world".to_owned()))
    .with_constant(Constant::FuncRef {
        // "csv" is the second dependency entry, so position 2
        key: FuncRefKey::Dep(2),
        func: 2,
    })
    .with_constant(Constant::FuncRef {
        key: FuncRefKey::Native("builtin".to_owned()),
        func: 7,
    })
    .with_debug_span(0, 24)
    .with_debug_span(24, 25)
}

#[test]
fn write_read_write_is_byte_identical() {
    let first = write(&sample_module());
    let module = read(&first).expect("writer output must read back");
    let second = write(&module);
    assert_eq!(first, second, "round trip must be byte-stable");
    // Re-reading the second serialization is stable too (fixpoint).
    let third = write(&read(&second).expect("second serialization must read back"));
    assert_eq!(second, third);
}

#[test]
fn round_trip_preserves_every_field() {
    let bytes = write(&sample_module());
    let module = read(&bytes).expect("valid module");
    assert_eq!(module.name(), "pipeline");
    assert_eq!(
        module.target(),
        &TargetClass::Triple("x86_64-pc-windows-msvc".to_owned())
    );
    assert_eq!(module.deps().len(), 2);
    assert_eq!(module.deps()[0].name, "std");
    assert_eq!(module.deps()[1].name, "csv");
    assert_eq!(
        module.deps()[1].content_hash,
        Hash128::compute(b"csv package content")
    );
    assert_eq!(module.source_hash(), Hash128::compute(SOURCE));
    assert_eq!(module.constants().len(), 8);
    assert_eq!(module.constants()[0], Constant::I64(-42));
    assert_eq!(module.constants()[2], Constant::F64(3.25));
    assert_eq!(module.constants()[3], Constant::Bool(true));
    assert_eq!(module.constants()[4], Constant::Bool(false));
    assert_eq!(
        module.constants()[5],
        Constant::Str("h\u{e9}llo \u{0} world".to_owned())
    );
    assert_eq!(module.constants()[6], Constant::FuncRef {
        key: FuncRefKey::Dep(2),
        func: 2
    });
    assert_eq!(module.constants()[7], Constant::FuncRef {
        key: FuncRefKey::Native("builtin".to_owned()),
        func: 7
    });
    // NaN payloads survive bit-exactly (NaN != NaN under derived equality).
    match &module.constants()[1] {
        Constant::F64(value) => assert_eq!(value.to_bits(), 0x7ff8_0000_dead_beef),
        other => panic!("expected the NaN constant, got {other:?}"),
    }
    assert_eq!(
        module.debug_spans(),
        Some(
            &[
                ds_mod::DebugSpan::new(0, 24),
                ds_mod::DebugSpan::new(24, 25)
            ][..]
        )
    );
}

#[test]
fn flags_round_trip() {
    for flags in [
        Flags::default(),
        Flags::default().with_ffi_used(true),
        Flags::default().with_jit_metadata(true).with_strip(true),
    ] {
        let module = sample_module().with_flags(flags);
        let bytes = write(&module);
        let read_back = read(&bytes).expect("valid module");
        assert_eq!(read_back.flags(), flags);
    }
}

#[test]
fn collector_hooks_round_trip_dormant() {
    let bytes = write(&sample_module());
    let module = read(&bytes).expect("valid module");
    let hooks = module.collector();
    assert!(hooks.is_dormant(), "hooks must stay dormant");
    assert_eq!(hooks.slots(), [0, 0]);
    assert!(!hooks.hooks_active());
    // The dormant collector block plus the pending BL-7 scalars are exactly
    // the reserved bytes 7..32.
    assert_eq!(&bytes[7..32], &[0u8; 25]);
}

#[test]
fn bl7_fields_default_pending_and_round_trip() {
    // New modules carry pending values: both BL-7 scalars zero.
    let bytes = write(&sample_module());
    let module = read(&bytes).expect("valid module");
    assert_eq!(module.max_call_depth(), 0);
    assert_eq!(module.ir_checksum(), 0);
    // A file with non-zero committed values is accepted and round-tripped
    // byte-identically (values are preserved, not validated, until M3).
    let mut patched = bytes.clone();
    patched[24..28].copy_from_slice(&1_000u32.to_le_bytes());
    patched[28..32].copy_from_slice(&0xdead_beefu32.to_le_bytes());
    let module = read(&patched).expect("non-zero pending values accepted");
    assert_eq!(module.max_call_depth(), 1_000);
    assert_eq!(module.ir_checksum(), 0xdead_beef);
    assert_eq!(write(&module), patched);
}

#[test]
fn the_tag_table_round_trips() {
    // The blocks payload's tag table: the name side of the tag ids.
    let module = sample_module().with_tag("none", 0).with_tag("int", 1);
    let bytes = write(&module);
    let read_back = read(&bytes).expect("the table round trips");
    assert_eq!(write(&read_back), bytes, "byte-stable across the read");
    assert_eq!(read_back.tags().len(), 2);
    assert_eq!(read_back.tag_id("none"), Some(0));
    assert_eq!(read_back.tag_id("int"), Some(1));
    assert_eq!(read_back.tag_id("zzz"), None);
}

#[test]
fn strip_removes_debug_spans_section() {
    let plain = write(&sample_module());
    let stripped_bytes = write(&sample_module().stripped());
    let stripped = read(&stripped_bytes).expect("stripped module must read");
    assert!(stripped.flags().strips_debug_spans());
    assert_eq!(stripped.debug_spans(), None);
    assert!(
        stripped_bytes.len() < plain.len(),
        "stripping must shrink the artifact"
    );
    // Stripping is byte-stable too.
    assert_eq!(write(&stripped), stripped_bytes);
}

#[test]
fn bytecode_target_and_empty_module_round_trip() {
    let module = Module::new("a", TargetClass::Bytecode, Hash128::ZERO);
    let bytes = write(&module);
    let read_back = read(&bytes).expect("minimal module");
    assert_eq!(read_back.target(), &TargetClass::Bytecode);
    assert_eq!(read_back.deps(), []);
    assert_eq!(read_back.constants(), []);
    assert_eq!(read_back.tags(), []);
    assert_eq!(read_back.debug_spans(), None);
    assert_eq!(write(&read_back), bytes);

    let module = Module::new("b", TargetClass::Wasm, Hash128::ZERO);
    let bytes = write(&module);
    assert_eq!(
        read(&bytes).expect("wasm module").target(),
        &TargetClass::Wasm
    );
}

#[test]
fn checked_hash_validation_accepts_and_rejects() {
    let bytes = write(&sample_module());
    read_checked(&bytes, SOURCE).expect("matching source");
    let error = read_checked(&bytes, b"edited source").expect_err("hash must mismatch");
    assert_eq!(error, ds_mod::ReadError::HashMismatch {
        stored: Hash128::compute(SOURCE),
        computed: Hash128::compute(b"edited source"),
    });
}

#[test]
fn mmap_loader_round_trips_from_disk() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("pipeline.dsmod");
    std::fs::write(&path, write(&sample_module())).expect("write artifact");
    // SAFETY: the test wrote the file above and nothing rewrites or
    // truncates it while the mapping lives.
    let mapped = unsafe { read_mmap(&path) }.expect("mmap valid module");
    assert_eq!(mapped.module().name(), "pipeline");
    assert_eq!(mapped.bytes(), &write(&sample_module())[..]);
}

// The mutual `even`/`odd` of H2's example, a local
// copy of ds-abi's locked fixture encodings (its test module is not
// public). Slots: r0 = n, r1 = compare bool, r2 = constant scratch, r3 =
// the outgoing argument (the slide source the ABI derives as the arg-area
// slot). Pool: k0 = 0i64, k1 = 1i64, k2 = the result bool, k3 = the
// callee as an enclosing-module FuncRef (dependency position 0,
// function index within this module's blocks payload).
fn even_code() -> Vec<Uop> {
    vec![
        Uop::Funcf { frame_slots: 4 }, // 0
        Uop::Kimm {
            dst: 2,
            kidx: ConstIdx(0),
        }, // 1: stage 0
        Uop::EqI64 {
            dst: 1,
            lhs: 0,
            rhs: 2,
        }, // 2: r1 = n == 0
        Uop::Jmpif { cond: 1, target: 7 }, // 3
        Uop::Kimm {
            dst: 2,
            kidx: ConstIdx(1),
        }, // 4: stage 1
        Uop::SubI64 {
            dst: 3,
            lhs: 0,
            rhs: 2,
        }, // 5: r3 = n - 1 (arg area)
        Uop::Callt {
            callee: ConstIdx(3),
            nargs: 1,
        }, // 6: slide r3 into slot0
        Uop::Kconst {
            dst: 1,
            kidx: ConstIdx(2),
        }, // 7: r1 = true
        Uop::Ret { src: 1 },           // 8
    ]
}

fn odd_code() -> Vec<Uop> {
    vec![
        Uop::Funcf { frame_slots: 4 }, // 0
        Uop::Kimm {
            dst: 2,
            kidx: ConstIdx(0),
        }, // 1
        Uop::EqI64 {
            dst: 1,
            lhs: 0,
            rhs: 2,
        }, // 2
        Uop::Jmpif { cond: 1, target: 7 }, // 3
        Uop::Kimm {
            dst: 2,
            kidx: ConstIdx(1),
        }, // 4
        Uop::SubI64 {
            dst: 3,
            lhs: 0,
            rhs: 2,
        }, // 5
        Uop::Callt {
            callee: ConstIdx(3),
            nargs: 1,
        }, // 6: tail-call even
        Uop::Kconst {
            dst: 1,
            kidx: ConstIdx(2),
        }, // 7: r1 = false
        Uop::Ret { src: 1 },           // 8
    ]
}

fn even_odd_layout() -> FrameLayout {
    FrameLayout::new(
        vec![SlotType::I64, SlotType::Bool, SlotType::I64, SlotType::I64],
        1,
        1,
    )
    .expect("valid layout")
}

#[test]
fn even_odd_two_function_module_round_trips() {
    let module = Module::new("mutrec", TargetClass::Bytecode, Hash128::compute(SOURCE))
        .with_function(
            Function::new(
                "even",
                even_odd_layout(),
                vec![
                    Constant::I64(0),
                    Constant::I64(1),
                    Constant::Bool(true),
                    Constant::FuncRef {
                        key: FuncRefKey::Dep(0),
                        func: 1,
                    },
                ],
                even_code(),
            )
            .expect("even validates"),
        )
        .with_function(
            Function::new(
                "odd",
                even_odd_layout(),
                vec![
                    Constant::I64(0),
                    Constant::I64(1),
                    Constant::Bool(false),
                    Constant::FuncRef {
                        key: FuncRefKey::Dep(0),
                        func: 0,
                    },
                ],
                odd_code(),
            )
            .expect("odd validates"),
        );
    let bytes = write(&module);
    let read_back = read(&bytes).expect("two-function module reads back");
    // Byte-stable with real payload content.
    assert_eq!(write(&read_back), bytes);
    assert_eq!(read_back.functions().len(), 2);
    let (even, odd) = (&read_back.functions()[0], &read_back.functions()[1]);
    assert_eq!(even.name(), "even");
    assert_eq!(odd.name(), "odd");
    assert_eq!(even.code(), words(even_code()).as_slice());
    assert_eq!(odd.code(), words(odd_code()).as_slice());
    assert_eq!(even.layout(), &even_odd_layout());
    assert_eq!(even.constants()[0], Constant::I64(0));
    assert_eq!(even.constants()[3], Constant::FuncRef {
        key: FuncRefKey::Dep(0),
        func: 1
    });
    assert_eq!(odd.constants()[2], Constant::Bool(false));
    // The tail-call word survived: CALLT with nargs 1 and pool index 3.
    assert!(even.code().contains(&encoded(
        Uop::Callt {
            callee: ConstIdx(3),
            nargs: 1
        },
        0
    )));
}

#[test]
fn empty_function_round_trips() {
    let module = Module::new("m", TargetClass::Bytecode, Hash128::ZERO).with_function(
        Function::new(
            "nop",
            FrameLayout::new(Vec::new(), 0, 0).expect("empty layout"),
            Vec::new(),
            vec![Uop::Ret { src: 0 }],
        )
        .expect("RET-only body validates"),
    );
    let bytes = write(&module);
    let read_back = read(&bytes).expect("empty function reads");
    assert_eq!(write(&read_back), bytes);
    let function = &read_back.functions()[0];
    assert_eq!(function.name(), "nop");
    assert_eq!(function.code(), &[encoded(Uop::Ret { src: 0 }, 0)]);
    assert_eq!(function.layout().frame_size(), 2); // metadata + call words only
}

#[test]
fn raw_overlay_slots_round_trip() {
    // A frame whose outgoing-argument area stages more than one call shape
    // declares Raw overlay slots; wire tag 6 carries them.
    let module = Module::new("overlay", TargetClass::Bytecode, Hash128::ZERO).with_function(
        Function::new(
            "main",
            FrameLayout::new(vec![SlotType::Raw, SlotType::Raw, SlotType::Raw], 0, 3)
                .expect("layout"),
            Vec::new(),
            vec![Uop::Funcf { frame_slots: 3 }, Uop::Ret { src: 0 }],
        )
        .expect("valid function"),
    );
    let bytes = write(&module);
    let read_back = read(&bytes).expect("round trips");
    assert_eq!(read_back.functions()[0].layout().slots(), &[
        SlotType::Raw,
        SlotType::Raw,
        SlotType::Raw
    ]);
    assert_eq!(write(&read_back), bytes, "byte-stable across the read");
}

#[test]
fn builder_rejects_jump_escaping_the_body() {
    let layout = FrameLayout::new(vec![SlotType::I64], 1, 0).expect("layout");
    // A Jmp at pc 1 targeting 2 in a two-word body: one past the end.
    let error = Function::new("j", layout.clone(), Vec::new(), vec![
        Uop::Funcf { frame_slots: 1 },
        Uop::Jmp { target: 2 },
    ])
    .expect_err("target escapes");
    assert_eq!(error, FunctionError::JumpEscapesBody {
        pc: 1,
        target: 2,
        len: 2
    });
    // A target inside the body is accepted, including a jump to itself.
    Function::new("j", layout, Vec::new(), vec![
        Uop::Funcf { frame_slots: 1 },
        Uop::Jmp { target: 1 },
    ])
    .expect("in-body target validates");
}

#[test]
fn cross_module_funcref_round_trips() {
    // A function whose pool mixes two dependency-position refs (the
    // "std" and "utils" dependency entries) and an enclosing-module
    // ref (position 0); the module-level constants section carries one
    // too.
    let module = Module::new("app", TargetClass::Bytecode, Hash128::ZERO)
        .with_dep("std", Hash128::ZERO)
        .with_dep("utils", Hash128::ZERO)
        .with_constant(Constant::FuncRef {
            key: FuncRefKey::Dep(1), // the "std" entry
            func: 0,
        })
        .with_function(
            Function::new(
                "main",
                FrameLayout::new(vec![SlotType::I64, SlotType::I64], 1, 1).expect("layout"),
                vec![
                    Constant::FuncRef {
                        key: FuncRefKey::Dep(2), // the "utils" entry
                        func: 4,
                    },
                    Constant::FuncRef {
                        key: FuncRefKey::Dep(0),
                        func: 0,
                    },
                ],
                vec![
                    Uop::Funcf { frame_slots: 2 },
                    Uop::Kconst {
                        dst: 1,
                        kidx: ConstIdx(0),
                    },
                    Uop::Call {
                        callee: ConstIdx(0),
                        nargs: 0,
                    },
                    Uop::Ret { src: 0 },
                ],
            )
            .expect("funcref pool validates"),
        );
    let bytes = write(&module);
    let read_back = read(&bytes).expect("funcref module reads");
    assert_eq!(write(&read_back), bytes);
    assert_eq!(read_back.constants()[0], Constant::FuncRef {
        key: FuncRefKey::Dep(1),
        func: 0
    });
    assert_eq!(read_back.functions()[0].constants()[0], Constant::FuncRef {
        key: FuncRefKey::Dep(2),
        func: 4
    });
    assert_eq!(read_back.functions()[0].constants()[1], Constant::FuncRef {
        key: FuncRefKey::Dep(0),
        func: 0
    });
}
