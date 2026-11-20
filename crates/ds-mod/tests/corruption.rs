//! Reader rejection: every corruption class fails with the right
//! diagnostic `[G3.5 acceptance]`.

use ds_diag::{Diagnostic, ErrorCode, FileId, Severity};
use ds_mod::{
    Constant, FORMAT_VERSION, FuncRefKey, Hash128, MAGIC, Module, ReadError, SectionKind,
    TargetClass, read, read_checked, write,
};

const SOURCE: &[u8] = b"const x = 1";

fn module_bytes(with_spans: bool) -> Vec<u8> {
    let module = Module::new("m", TargetClass::Bytecode, Hash128::compute(SOURCE))
        .with_constant(Constant::I64(7));
    let module = if with_spans { module.with_debug_span(0, 12) } else { module };
    write(&module)
}

/// The first directory entry's kind byte: fixed header (32) + name "m" (2)
/// + dep count (1) + source hash (16) + directory count (1). The framing is locked, so this
///   position is stable for these fixtures.
const FIRST_KIND_BYTE: usize = 32 + 2 + 1 + 16 + 1;

fn assert_diagnostic(error: &ReadError, code: ErrorCode) {
    assert_eq!(error.code(), code, "error {error:?}");
    let diagnostic = error.diagnostic(FileId::new(0));
    let Diagnostic {
        severity,
        code: got,
        message,
        ..
    } = &diagnostic;
    assert_eq!(*got, code);
    assert_eq!(*severity, Severity::Error);
    assert_ne!(message, "");
}

#[test]
fn bad_magic_rejected() {
    let mut bytes = module_bytes(true);
    bytes[0] = b'X';
    let error = read(&bytes).expect_err("bad magic");
    assert_eq!(error, ReadError::BadMagic);
    assert_diagnostic(&error, ErrorCode::ModuleBadMagic);
    assert_eq!(error.offset(), Some(0));
    // Empty input is a magic failure, not an EOF.
    assert_eq!(read(&[]), Err(ReadError::BadMagic));
}

#[test]
fn bad_version_rejected() {
    let mut bytes = module_bytes(true);
    bytes[4] = 9;
    let error = read(&bytes).expect_err("bad version");
    assert_eq!(error, ReadError::UnsupportedVersion { found: 9 });
    assert_diagnostic(&error, ErrorCode::ModuleVersionUnsupported);
    // Format versions 1 (pre BL-7 header fields), 2 (pre FuncRef
    // constants kind), and 3 (pre selector-led FuncRef keys) are
    // rejected too.
    for version in 1..=3 {
        bytes[4] = version;
        assert_eq!(
            read(&bytes).expect_err(&format!("version {version} rejected")),
            ReadError::UnsupportedVersion { found: version }
        );
    }
    // A bare magic runs out of bytes at the version position.
    assert!(matches!(
        read(b"\0DSM"),
        Err(ReadError::UnexpectedEof { at: 4 })
    ));
}

#[test]
fn bad_target_class_rejected() {
    let mut bytes = module_bytes(true);
    bytes[5] = 9;
    let error = read(&bytes).expect_err("bad target class");
    assert_eq!(error, ReadError::BadTargetClass { tag: 9 });
    assert_diagnostic(&error, ErrorCode::ModuleBadTargetClass);
}

#[test]
fn unknown_flag_bits_rejected() {
    let mut bytes = module_bytes(true);
    bytes[6] |= 0x80;
    let error = read(&bytes).expect_err("unknown flag bits");
    assert_eq!(error, ReadError::UnknownFlags { unknown: 0x80 });
    assert_diagnostic(&error, ErrorCode::BytecodeMalformed);
}

#[test]
fn every_truncation_is_rejected() {
    let bytes = module_bytes(true);
    for cut in 0..bytes.len() {
        let Err(error) = read(&bytes[..cut]) else {
            panic!("prefix of len {cut} must not read as valid")
        };
        assert_diagnostic(&error, error.code());
    }
}

#[test]
fn hash_mismatch_reported_by_checked_read() {
    let bytes = module_bytes(true);
    read_checked(&bytes, SOURCE).expect("matching source");
    let error = read_checked(&bytes, b"const x = 2").expect_err("edited source");
    let ReadError::HashMismatch { stored, computed } = &error else {
        panic!("expected HashMismatch, got {error:?}");
    };
    assert_eq!(*stored, Hash128::compute(SOURCE));
    assert_eq!(*computed, Hash128::compute(b"const x = 2"));
    assert_diagnostic(&error, ErrorCode::ModuleHashMismatch);
    assert_eq!(error.offset(), None);
}

#[test]
fn section_overrun_rejected() {
    let bytes = module_bytes(true);
    // Debug spans are the last section; cutting the file's final byte
    // leaves the directory claiming a length past end of file.
    let error = read(&bytes[..bytes.len() - 1]).expect_err("payload truncated");
    let ReadError::SectionOverrun { kind, .. } = &error else {
        panic!("expected SectionOverrun, got {error:?}");
    };
    assert_eq!(*kind, SectionKind::DebugSpans);
    assert_diagnostic(&error, ErrorCode::ModuleSectionMalformed);
}

#[test]
fn duplicate_section_rejected() {
    let mut bytes = module_bytes(true);
    // Point the blocks entry at the constants kind: constants twice.
    bytes[FIRST_KIND_BYTE] = SectionKind::Constants.tag();
    let error = read(&bytes).expect_err("duplicate section");
    assert_eq!(error, ReadError::DuplicateSection {
        kind: SectionKind::Constants
    });
    assert_diagnostic(&error, ErrorCode::ModuleSectionMalformed);
}

#[test]
fn missing_section_rejected() {
    let mut bytes = module_bytes(false);
    // Four-section module (no debug spans): relabel the blocks entry as
    // debug spans, so blocks is the missing required section.
    bytes[FIRST_KIND_BYTE] = SectionKind::DebugSpans.tag();
    let error = read(&bytes).expect_err("missing section");
    assert_eq!(error, ReadError::MissingSection {
        kind: SectionKind::Blocks
    });
    assert_diagnostic(&error, ErrorCode::ModuleSectionMalformed);
}

#[test]
fn unknown_section_kind_rejected() {
    let mut bytes = module_bytes(true);
    bytes[FIRST_KIND_BYTE] = 7;
    let error = read(&bytes).expect_err("unknown section kind");
    assert_eq!(error, ReadError::UnknownSectionKind {
        tag: 7,
        at: FIRST_KIND_BYTE
    });
    assert_diagnostic(&error, ErrorCode::ModuleSectionMalformed);
}

#[test]
fn non_canonical_section_order_rejected() {
    // Swap the kind bytes of directory rows 2 and 3 (type-metadata and
    // effect-rows): identical two-byte payloads, so the file still tiles
    // and both payloads parse — only the order differs from the writer's,
    // and re-serialization would produce different bytes.
    let mut bytes = module_bytes(false);
    let (row_a, row_b) = (FIRST_KIND_BYTE + 6, FIRST_KIND_BYTE + 9);
    bytes.swap(row_a, row_b);
    let error = read(&bytes).expect_err("transposed directory rows");
    assert_eq!(error, ReadError::NonCanonicalSectionOrder {
        kind: SectionKind::EffectRows,
        expected: SectionKind::TypeMetadata,
        at: row_a,
    });
    assert_diagnostic(&error, ErrorCode::ModuleSectionMalformed);
    assert_eq!(error.offset(), Some(row_a));
    // Non-adjacent transposition (rows 0 and 3): unequal payload lengths,
    // so tiling would also fail, but the kind order is checked first.
    let mut bytes = module_bytes(false);
    bytes.swap(FIRST_KIND_BYTE, FIRST_KIND_BYTE + 9);
    let error = read(&bytes).expect_err("transposed end rows");
    assert_eq!(error, ReadError::NonCanonicalSectionOrder {
        kind: SectionKind::EffectRows,
        expected: SectionKind::Blocks,
        at: FIRST_KIND_BYTE,
    });
    // A five-section module (debug spans present): moving the strippable
    // section off the last position is rejected the same way.
    let mut bytes = module_bytes(true);
    let row_c = FIRST_KIND_BYTE + 9;
    bytes.swap(row_c, row_c + 3);
    let error = read(&bytes).expect_err("debug spans out of last position");
    assert_eq!(error, ReadError::NonCanonicalSectionOrder {
        kind: SectionKind::DebugSpans,
        expected: SectionKind::EffectRows,
        at: row_c,
    });
    assert_diagnostic(&error, ErrorCode::ModuleSectionMalformed);
}

/// The campaign-found reproducer: a committed corpus seed with the
/// type-metadata and effect-rows directory rows transposed over identical
/// payloads. The seed must stay rejected; accepting it would break the
/// byte-stable reader contract.
#[test]
fn fuzz_reproducer_section_row_swap_rejected() {
    let path = "../../fuzz/corpus/fuzz_read_dsmod/section-row-swap.dsmod";
    let bytes = std::fs::read(path).unwrap_or_else(|err| panic!("{path}: {err}"));
    let error = read(&bytes).expect_err("row-swapped directory must be rejected");
    assert_eq!(error, ReadError::NonCanonicalSectionOrder {
        kind: SectionKind::EffectRows,
        expected: SectionKind::TypeMetadata,
        at: 57,
    });
    assert_diagnostic(&error, ErrorCode::ModuleSectionMalformed);
    assert_eq!(error.offset(), Some(57));
}

#[test]
fn gapped_section_layout_rejected() {
    let mut bytes = module_bytes(true);
    // Open a one-byte gap between the blocks and constants payloads by
    // bumping the second entry's offset. All section ranges stay in bounds
    // and non-overlapping, but the layout is not the writer's compact
    // tiling, so the reader must reject it (strict canonical framing).
    // Fixture offsets and lengths are below 128, so every directory entry
    // is exactly three bytes: kind, offset uleb, length uleb.
    let constants_offset_byte = FIRST_KIND_BYTE + 3 + 1;
    assert!(
        bytes[constants_offset_byte] < 0x7f && bytes[constants_offset_byte] & 0x80 == 0,
        "fixture offset must be a single-byte uleb"
    );
    bytes[constants_offset_byte] += 1;
    let error = read(&bytes).expect_err("gapped layout");
    assert!(matches!(error, ReadError::Malformed {
        reason: "non-contiguous section layout",
        ..
    }));
    assert_diagnostic(&error, ErrorCode::BytecodeMalformed);
}

#[test]
fn constant_payload_corruption_rejected() {
    let module = Module::new("m", TargetClass::Bytecode, Hash128::ZERO)
        .with_constant(Constant::Str("ZQXZQX".to_owned()));
    let mut bytes = write(&module);
    let needle = b"ZQXZQX";
    let pos = bytes
        .windows(needle.len())
        .position(|w| w == needle)
        .expect("constant bytes present");
    // The uleb length and the Str tag (3) sit directly before the bytes.
    bytes[pos - 2] = 9;
    let error = read(&bytes).expect_err("bad constant tag");
    assert!(matches!(error, ReadError::Malformed {
        reason: "unknown constant tag",
        ..
    }));
    assert_diagnostic(&error, ErrorCode::BytecodeMalformed);
}

#[test]
fn funcref_dep_position_past_the_dep_list_rejected() {
    // One dependency entry, so position 2 addresses no module: the
    // builder accepts the value (the writer is not a validator), the
    // reader refuses it.
    let module = Module::new("m", TargetClass::Bytecode, Hash128::ZERO)
        .with_dep("std", Hash128::ZERO)
        .with_constant(Constant::FuncRef {
            key: FuncRefKey::Dep(2),
            func: 0,
        });
    let error = read(&write(&module)).expect_err("dangling dep position");
    assert!(matches!(error, ReadError::Malformed {
        reason: "FuncRef dep position past the dependency list",
        ..
    }));
    assert_diagnostic(&error, ErrorCode::BytecodeMalformed);
}

#[test]
fn unknown_funcref_selector_rejected() {
    let module =
        Module::new("m", TargetClass::Bytecode, Hash128::ZERO).with_constant(Constant::FuncRef {
            key: FuncRefKey::Dep(0),
            func: 7,
        });
    let mut bytes = write(&module);
    // The lone FuncRef constant is tag 4, selector 0, position 0, func 7:
    // the selector is the byte after the tag.
    let needle: &[u8] = &[4, 0, 0, 7];
    let pos = bytes
        .windows(needle.len())
        .position(|w| w == needle)
        .expect("funcref constant present");
    bytes[pos + 1] = 2;
    let error = read(&bytes).expect_err("unknown selector");
    assert!(matches!(error, ReadError::Malformed {
        reason: "unknown FuncRef key selector",
        ..
    }));
    assert_diagnostic(&error, ErrorCode::BytecodeMalformed);
}

#[test]
fn bad_code_word_rejected() {
    use ds_abi::{FrameLayout, Uop, uop::ConstIdx};
    use ds_mod::Function;
    // One function whose code is [KIMM r0 <- k0; RET r0]: the RET word is
    // the distinctive needle; corrupt its opcode byte to the reserved
    // major 0xF (0xE decodes: it is the RC pair).
    let module = Module::new("m", TargetClass::Bytecode, Hash128::ZERO).with_function(
        Function::new(
            "f",
            FrameLayout::new(vec![ds_abi::SlotType::I64], 1, 0).expect("layout"),
            vec![Constant::I64(0)],
            vec![
                Uop::Kimm {
                    dst: 0,
                    kidx: ConstIdx(0),
                },
                Uop::Ret { src: 0 },
            ],
        )
        .expect("fixture validates"),
    );
    let mut bytes = write(&module);
    let needle = 0xa2u32.to_le_bytes(); // RET { src: 0 }
    let pos = bytes
        .windows(4)
        .position(|w| w == needle)
        .expect("code word present");
    bytes[pos] = 0xf0; // reserved major: no instruction decodes
    let error = read(&bytes).expect_err("bad code word");
    assert!(matches!(error, ReadError::Malformed {
        reason: "code word does not decode to an instruction",
        ..
    }));
    assert_diagnostic(&error, ErrorCode::BytecodeMalformed);
}

/// The single decode pass owns the jump-target bound: a target one word
/// past the body surfaces as the spanned `Malformed` reason at the jump
/// word, in the same error class every other malformed body word uses.
#[test]
fn jump_out_of_body_rejected() {
    use ds_abi::{FrameLayout, Uop};
    use ds_mod::Function;
    // Code [FUNCF 1; JMP -> 1] (a legal self-jump, field BIAS_J - 1); patch
    // the field to BIAS_J + 0 so the target becomes 2, one past the body.
    let module = Module::new("m", TargetClass::Bytecode, Hash128::ZERO).with_function(
        Function::new(
            "j",
            FrameLayout::new(vec![ds_abi::SlotType::I64], 1, 0).expect("layout"),
            Vec::new(),
            vec![Uop::Funcf { frame_slots: 1 }, Uop::Jmp { target: 1 }],
        )
        .expect("fixture validates"),
    );
    let mut bytes = write(&module);
    let needle = Uop::Jmp { target: 1 }
        .encode(1)
        .expect("encodable")
        .to_le_bytes();
    let pos = bytes
        .windows(4)
        .position(|w| w == needle)
        .expect("jump word present");
    // Retarget to 2: offset 0 relative to next_pc, field exactly BIAS_J.
    // The word is computed with the encoder itself (it is un-buildable as a
    // Function, which is the point of the corruption).
    let patched = Uop::Jmp { target: 2 }
        .encode(1)
        .expect("biased field encodable at pc 1");
    bytes[pos..pos + 4].copy_from_slice(&patched.to_le_bytes());
    let error = read(&bytes).expect_err("jump escapes body");
    assert!(matches!(&error, ReadError::Malformed {
        reason: "jump target escapes the function body",
        at,
    } if *at == pos));
    assert_eq!(error.offset(), Some(pos));
    assert_diagnostic(&error, ErrorCode::BytecodeMalformed);
    assert!(
        error
            .to_string()
            .contains("jump target escapes the function body"),
        "the error must name the escaping jump: {error}"
    );
}

#[test]
fn unknown_entry_plan_rejected() {
    // One recorded entry "zebra" with one Int plan: the name, the plan
    // count 1, and the plan byte 0 sit contiguously in the blocks
    // payload's entry table; patch the plan byte past the vocabulary.
    let module = Module::new("m", TargetClass::Bytecode, Hash128::ZERO)
        .with_entry("zebra", vec![ds_mod::EntryArg::Int]);
    let mut bytes = write(&module);
    let needle: &[u8] = b"zebra\x01\x00";
    let pos = bytes
        .windows(needle.len())
        .position(|w| w == needle)
        .expect("entry record present");
    bytes[pos + needle.len() - 1] = 5;
    let error = read(&bytes).expect_err("unknown plan byte");
    assert!(matches!(error, ReadError::Malformed {
        reason: "unknown entry plan",
        ..
    }));
    assert_diagnostic(&error, ErrorCode::BytecodeMalformed);
}

#[test]
fn duplicate_entry_name_rejected() {
    // The builder accepts the duplicate (the writer is not a validator);
    // the reader refuses the entry table's second row.
    let module = Module::new("m", TargetClass::Bytecode, Hash128::ZERO)
        .with_entry("dup", Vec::new())
        .with_entry("dup", vec![ds_mod::EntryArg::Int]);
    let error = read(&write(&module)).expect_err("duplicate entry name");
    assert!(matches!(error, ReadError::Malformed {
        reason: "duplicate entry-table name",
        ..
    }));
    assert_diagnostic(&error, ErrorCode::BytecodeMalformed);
}

#[test]
fn duplicate_tag_rows_rejected() {
    for (first, second) in [
        (("a", 0u8), ("a", 1u8)), // same name, different ids
        (("a", 3), ("b", 3)),     // same id, different names
    ] {
        let module = Module::new("m", TargetClass::Bytecode, Hash128::ZERO)
            .with_tag(first.0, first.1)
            .with_tag(second.0, second.1);
        let error = read(&write(&module)).expect_err("duplicate tag row");
        assert!(
            matches!(&error, ReadError::Malformed { reason, .. } if reason.contains("tag-table"))
        );
        assert_diagnostic(&error, ErrorCode::BytecodeMalformed);
    }
}

/// A blocks payload declaring 2^32 code words: the count itself must
/// reject as malformed data — a pc beyond u32 is unrepresentable, and the
/// reader never panics on any input. No writer can emit this file, so it
/// is hand-assembled: the minimal four-section module.
#[test]
fn code_word_count_beyond_u32_rejected() {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&MAGIC);
    bytes.extend_from_slice(&[FORMAT_VERSION, 0, 0, 0]); // version, target tag, flags, collector flags
    bytes.extend_from_slice(&[0u8; 16]); // collector hook slots
    bytes.extend_from_slice(&0u32.to_le_bytes()); // max call depth
    bytes.extend_from_slice(&0u32.to_le_bytes()); // IR-version checksum
    bytes.extend_from_slice(&[1, b'm']); // module name
    bytes.push(0); // dependency count
    bytes.extend_from_slice(Hash128::ZERO.as_bytes()); // source hash
    bytes.push(4); // section directory count
    let dir_entries_at = bytes.len();
    bytes.extend_from_slice(&[0u8; 12]); // four 3-byte entries, patched below
    let blocks_at = bytes.len();
    // blocks payload v3: one function "f" with no params, slots, or
    // constants, then the over-range code count uleb (2^32) — the count
    // rejects before the trailing tables are reached
    bytes.extend_from_slice(&[3, 1, 1, b'f', 0, 0, 0, 0]);
    bytes.extend_from_slice(&[0x80, 0x80, 0x80, 0x80, 0x10]);
    let blocks_len = bytes.len() - blocks_at;
    let constants_at = bytes.len();
    bytes.push(0); // zero constants
    let type_metadata_at = bytes.len();
    bytes.extend_from_slice(&[0, 0]); // placeholder payload v0 + reserved 0
    let effect_rows_at = bytes.len();
    bytes.extend_from_slice(&[0, 0]);
    // Directory rows in canonical order: kind byte, offset uleb, length
    // uleb — every value stays under 128, so each entry is exactly three
    // bytes as reserved above.
    let rows = [
        (SectionKind::Blocks.tag(), blocks_at, blocks_len),
        (SectionKind::Constants.tag(), constants_at, 1),
        (SectionKind::TypeMetadata.tag(), type_metadata_at, 2),
        (SectionKind::EffectRows.tag(), effect_rows_at, 2),
    ];
    let mut at = dir_entries_at;
    for (kind, offset, len) in rows {
        bytes[at] = kind;
        bytes[at + 1] = u8::try_from(offset).expect("offset under 128");
        bytes[at + 2] = u8::try_from(len).expect("length under 128");
        at += 3;
    }
    let error = read(&bytes).expect_err("declared count beyond u32 must be rejected");
    // 64-bit hosts reject the count as malformed; 32-bit hosts reject the
    // same uleb one step earlier, as a count too large to address memory
    assert!(matches!(
        error,
        ReadError::Malformed {
            reason: "code word count exceeds the u32 pc range",
            ..
        } | ReadError::MalformedUleb { .. }
    ));
    assert_diagnostic(&error, ErrorCode::BytecodeMalformed);
}

/// A dependency count far past the payload: the capacity hint clamps to the
/// bytes left, so the reader rejects the stream at its first absent byte
/// instead of reserving room for the declared count.
#[test]
fn huge_declared_count_rejected() {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&MAGIC);
    bytes.extend_from_slice(&[FORMAT_VERSION, 0, 0, 0]); // version, target tag, flags, collector flags
    bytes.extend_from_slice(&[0u8; 16]); // collector hook slots
    bytes.extend_from_slice(&0u32.to_le_bytes()); // max call depth
    bytes.extend_from_slice(&0u32.to_le_bytes()); // IR-version checksum
    bytes.extend_from_slice(&[1, b'm']); // module name
    bytes.extend_from_slice(&[0x80, 0x80, 0x80, 0x80, 0x80, 0x20]); // dep count 2^40
    let error = read(&bytes).expect_err("the first dependency is absent");
    assert_eq!(error, ReadError::UnexpectedEof { at: bytes.len() });
    assert_diagnostic(&error, ErrorCode::BytecodeMalformed);
}

/// A chain-table count far past the payload: the capacity hint clamps to
/// the bytes left, so the reader rejects the stream at its first absent
/// chain record instead of reserving room for the declared table.
#[test]
fn huge_chain_count_rejected() {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&MAGIC);
    bytes.extend_from_slice(&[FORMAT_VERSION, 0, 0, 0]); // version, target tag, flags, collector flags
    bytes.extend_from_slice(&[0u8; 16]); // collector hook slots
    bytes.extend_from_slice(&0u32.to_le_bytes()); // max call depth
    bytes.extend_from_slice(&0u32.to_le_bytes()); // IR-version checksum
    bytes.extend_from_slice(&[1, b'm']); // module name
    bytes.push(0); // dependency count
    bytes.extend_from_slice(Hash128::ZERO.as_bytes()); // source hash
    bytes.push(4); // section directory count
    let dir_entries_at = bytes.len();
    bytes.extend_from_slice(&[0u8; 12]); // four 3-byte entries, patched below
    let blocks_at = bytes.len();
    // blocks payload v3: zero functions, then the chain count uleb (2^40)
    // — the first chain record is absent, so the stream ends there
    bytes.extend_from_slice(&[3, 0]);
    bytes.extend_from_slice(&[0x80, 0x80, 0x80, 0x80, 0x80, 0x20]);
    let blocks_len = bytes.len() - blocks_at;
    let constants_at = bytes.len();
    bytes.push(0); // zero constants
    let type_metadata_at = bytes.len();
    bytes.extend_from_slice(&[0, 0]); // placeholder payload v0 + reserved 0
    let effect_rows_at = bytes.len();
    bytes.extend_from_slice(&[0, 0]);
    // Directory rows in canonical order: kind byte, offset uleb, length
    // uleb — every value stays under 128, so each entry is exactly three
    // bytes as reserved above.
    let rows = [
        (SectionKind::Blocks.tag(), blocks_at, blocks_len),
        (SectionKind::Constants.tag(), constants_at, 1),
        (SectionKind::TypeMetadata.tag(), type_metadata_at, 2),
        (SectionKind::EffectRows.tag(), effect_rows_at, 2),
    ];
    let mut at = dir_entries_at;
    for (kind, offset, len) in rows {
        bytes[at] = kind;
        bytes[at + 1] = u8::try_from(offset).expect("offset under 128");
        bytes[at + 2] = u8::try_from(len).expect("length under 128");
        at += 3;
    }
    let error = read(&bytes).expect_err("the first chain record is absent");
    assert_eq!(error, ReadError::UnexpectedEof { at: constants_at });
    assert_diagnostic(&error, ErrorCode::BytecodeMalformed);
}
