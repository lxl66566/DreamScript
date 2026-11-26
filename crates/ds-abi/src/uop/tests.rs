//! Locked-contract tests: exhaustive round-trips, bias arithmetic, XOR
//! adjacency, effect-table consistency, malformed rejection, and the two
//! seed program fixtures.

use std::{collections::HashSet, mem::discriminant};

use super::{
    BIAS_J, ConstIdx, DecodeError, EncodeError, MAX_FRAME_SLOTS, Major, Uop, Uop::*, jump_field,
    jump_target,
};

const K0: ConstIdx = ConstIdx(0);
const KMAX: ConstIdx = ConstIdx(0xffff);

/// Three boundary-valued instances of a three-register variant.
macro_rules! r3_samples {
    ($($v:ident),* $(,)?) => { vec![
        $( $v { dst: 0, lhs: 0, rhs: 0 }, $v { dst: 255, lhs: 255, rhs: 255 }, $v { dst: 1, lhs: 255, rhs: 2 } ),*
    ] };
}

/// One or more boundary-valued instances per variant (slot 0/255, pool
/// index 0/0xFFFF, tag 0/255), so the round-trip and consistency tests
/// below are exhaustive over the set.
fn samples() -> Vec<Uop> {
    let mut s = vec![
        Kimm { dst: 0, kidx: K0 },
        Kimm {
            dst: 255,
            kidx: KMAX,
        },
        Kconst { dst: 0, kidx: KMAX },
        Kconst { dst: 255, kidx: K0 },
        Kstr { dst: 0, kidx: K0 },
        Kstr {
            dst: 255,
            kidx: KMAX,
        },
        KstrBorrow { dst: 0, kidx: K0 },
        KstrBorrow {
            dst: 255,
            kidx: KMAX,
        },
    ];
    s.extend(r3_samples![
        AddI64,
        SubI64,
        MulI64,
        DivI64,
        ModI64,
        ShlI64,
        ShrI64,
        BitAndI64,
        BitXorI64,
        BitOrI64,
        AddF64,
        SubF64,
        MulF64,
        DivF64,
        EqI64,
        NeqI64,
        LtI64,
        GeI64,
        LeI64,
        GtI64,
        EqF64,
        NeqF64,
        LtF64,
        GeF64,
        LeF64,
        GtF64,
        EqBool,
        NeqBool,
        EqStr,
        NeqStr,
        StrCat,
        StrCatArena,
    ]);
    s.extend(
        [ArenaMark { dst: 0 }, ArenaMark { dst: 255 }, ArenaRestore {
            src: 255,
        }],
    );
    s.extend([
        TagTest {
            dst: 0,
            src: 255,
            tag: 0,
        },
        TagTestNot {
            dst: 255,
            src: 0,
            tag: 255,
        },
        TagNew { dst: 0, tag: 255 },
        TagNew { dst: 255, tag: 0 },
        CvtI2F { dst: 0, src: 255 },
        CvtF2I { dst: 255, src: 0 },
        Mov { dst: 0, src: 255 },
        Mov { dst: 255, src: 0 },
        Funcf { frame_slots: 0 },
        Funcf {
            frame_slots: MAX_FRAME_SLOTS,
        },
        Halt { slot: 255 },
        Halt { slot: 0 },
        Ret { src: 0 },
        Ret { src: 255 },
        RetSpan { src: 0, n: 1 },
        RetSpan { src: 255, n: 255 },
        ClosureNew {
            dst: 0,
            func: 0,
            n: 0,
        },
        ClosureNew {
            dst: 255,
            func: 255,
            n: 255,
        },
        ClosureNew {
            dst: 1,
            func: 255,
            n: 2,
        },
        CaptureStore {
            src: 0,
            obj: 255,
            idx: 0,
        },
        CaptureStore {
            src: 255,
            obj: 1,
            idx: 255,
        },
    ]);
    s.extend(call_samples());
    s.extend(jump_samples());
    s
}

/// The jump samples: absolute targets spanning both bias edges relative
/// to pc = 100 (window `[0, 32868]`); other pcs in the round-trip test
/// cover the rest.
fn jump_samples() -> Vec<Uop> {
    let mut s = Vec::new();
    for target in [0, 1, 99, 100, 101, 32868] {
        s.push(Jmp { target });
        s.push(Jmpif { cond: 0, target });
        s.push(Jmpif { cond: 255, target });
        s.push(JmpifNot { cond: 255, target });
        s.push(JmpifNot { cond: 0, target });
    }
    s
}

/// The call-family samples: CALL, CALLT, CALLG at both pool-index and
/// argument-count boundaries (CALLG carries CALL's operand layout), CALLS at
/// both callee-slot and argument-count boundaries.
fn call_samples() -> Vec<Uop> {
    vec![
        Call {
            callee: K0,
            nargs: 0,
        },
        Call {
            callee: KMAX,
            nargs: 255,
        },
        Callt {
            callee: K0,
            nargs: 255,
        },
        Callt {
            callee: KMAX,
            nargs: 1,
        },
        Callg {
            callee: K0,
            nargs: 0,
        },
        Callg {
            callee: KMAX,
            nargs: 255,
        },
        Calltg {
            callee: K0,
            nargs: 0,
        },
        Calltg {
            callee: KMAX,
            nargs: 255,
        },
        Calls {
            callee_slot: 0,
            nargs: 0,
        },
        Calls {
            callee_slot: 255,
            nargs: 255,
        },
    ]
}

#[test]
fn round_trip_every_uop_at_boundary_values() {
    for pc in [0, 1, 100] {
        for u in samples() {
            let word = match u.encode(pc) {
                Ok(word) => word,
                // only a jump can fall outside this pc's bias window, and
                // every sample FUNCF and RETSPAN is inside its caps
                Err(EncodeError::JumpOutOfRange { .. }) => {
                    assert!(matches!(u, Jmp { .. } | Jmpif { .. } | JmpifNot { .. }));
                    continue;
                },
                Err(EncodeError::FrameSlotsExceedCap { .. }) => {
                    panic!("samples stay at or below the frame cap: {u:?}")
                },
                Err(EncodeError::RetSpanEmpty) => {
                    panic!("samples never encode an empty span: {u:?}")
                },
            };
            let back = Uop::decode(pc, word).expect("a canonical word decodes");
            assert_eq!(back, u, "at pc {pc}");
            assert_eq!(
                back.encode(pc),
                Ok(word),
                "decode then encode is the identity"
            );
        }
    }
}

/// The documented opcode byte for every variant (the doc comments pin them
/// per variant; this table is the executable cross-check).
// a data table over every documented opcode, one entry each
#[allow(clippy::too_many_lines)]
fn documented_opcodes() -> Vec<(Uop, u8)> {
    vec![
        (Kimm { dst: 0, kidx: K0 }, 0x00),
        (Kconst { dst: 0, kidx: K0 }, 0x01),
        (
            AddI64 {
                dst: 0,
                lhs: 0,
                rhs: 0,
            },
            0x10,
        ),
        (
            SubI64 {
                dst: 0,
                lhs: 0,
                rhs: 0,
            },
            0x11,
        ),
        (
            MulI64 {
                dst: 0,
                lhs: 0,
                rhs: 0,
            },
            0x12,
        ),
        (
            DivI64 {
                dst: 0,
                lhs: 0,
                rhs: 0,
            },
            0x13,
        ),
        (
            ModI64 {
                dst: 0,
                lhs: 0,
                rhs: 0,
            },
            0x14,
        ),
        (
            WrapAddI64 {
                dst: 0,
                lhs: 0,
                rhs: 0,
            },
            0x15,
        ),
        (
            WrapSubI64 {
                dst: 0,
                lhs: 0,
                rhs: 0,
            },
            0x16,
        ),
        (
            WrapMulI64 {
                dst: 0,
                lhs: 0,
                rhs: 0,
            },
            0x17,
        ),
        (
            ShlI64 {
                dst: 0,
                lhs: 0,
                rhs: 0,
            },
            0x18,
        ),
        (
            ShrI64 {
                dst: 0,
                lhs: 0,
                rhs: 0,
            },
            0x19,
        ),
        (
            BitAndI64 {
                dst: 0,
                lhs: 0,
                rhs: 0,
            },
            0x1a,
        ),
        (
            BitXorI64 {
                dst: 0,
                lhs: 0,
                rhs: 0,
            },
            0x1b,
        ),
        (
            BitOrI64 {
                dst: 0,
                lhs: 0,
                rhs: 0,
            },
            0x1c,
        ),
        (
            AddF64 {
                dst: 0,
                lhs: 0,
                rhs: 0,
            },
            0x20,
        ),
        (
            SubF64 {
                dst: 0,
                lhs: 0,
                rhs: 0,
            },
            0x21,
        ),
        (
            MulF64 {
                dst: 0,
                lhs: 0,
                rhs: 0,
            },
            0x22,
        ),
        (
            DivF64 {
                dst: 0,
                lhs: 0,
                rhs: 0,
            },
            0x23,
        ),
        (
            EqI64 {
                dst: 0,
                lhs: 0,
                rhs: 0,
            },
            0x30,
        ),
        (
            NeqI64 {
                dst: 0,
                lhs: 0,
                rhs: 0,
            },
            0x31,
        ),
        (
            LtI64 {
                dst: 0,
                lhs: 0,
                rhs: 0,
            },
            0x32,
        ),
        (
            GeI64 {
                dst: 0,
                lhs: 0,
                rhs: 0,
            },
            0x33,
        ),
        (
            LeI64 {
                dst: 0,
                lhs: 0,
                rhs: 0,
            },
            0x34,
        ),
        (
            GtI64 {
                dst: 0,
                lhs: 0,
                rhs: 0,
            },
            0x35,
        ),
        (
            EqF64 {
                dst: 0,
                lhs: 0,
                rhs: 0,
            },
            0x40,
        ),
        (
            NeqF64 {
                dst: 0,
                lhs: 0,
                rhs: 0,
            },
            0x41,
        ),
        (
            LtF64 {
                dst: 0,
                lhs: 0,
                rhs: 0,
            },
            0x42,
        ),
        (
            GeF64 {
                dst: 0,
                lhs: 0,
                rhs: 0,
            },
            0x43,
        ),
        (
            LeF64 {
                dst: 0,
                lhs: 0,
                rhs: 0,
            },
            0x44,
        ),
        (
            GtF64 {
                dst: 0,
                lhs: 0,
                rhs: 0,
            },
            0x45,
        ),
        (
            EqBool {
                dst: 0,
                lhs: 0,
                rhs: 0,
            },
            0x50,
        ),
        (
            NeqBool {
                dst: 0,
                lhs: 0,
                rhs: 0,
            },
            0x51,
        ),
        (
            TagTest {
                dst: 0,
                src: 0,
                tag: 0,
            },
            0x60,
        ),
        (
            TagTestNot {
                dst: 0,
                src: 0,
                tag: 0,
            },
            0x61,
        ),
        (TagNew { dst: 0, tag: 0 }, 0x62),
        (CvtI2F { dst: 0, src: 0 }, 0x70),
        (CvtF2I { dst: 0, src: 0 }, 0x71),
        (Mov { dst: 0, src: 0 }, 0x80),
        (Jmp { target: 0 }, 0x92),
        (Jmpif { cond: 0, target: 0 }, 0x90),
        (JmpifNot { cond: 0, target: 0 }, 0x91),
        (Ret { src: 0 }, 0xa2),
        (RetSpan { src: 0, n: 1 }, 0xa3),
        (Funcf { frame_slots: 0 }, 0xb0),
        (Halt { slot: 0 }, 0xc0),
        (Kstr { dst: 0, kidx: K0 }, 0x02),
        (KstrBorrow { dst: 0, kidx: K0 }, 0x03),
        (
            EqStr {
                dst: 0,
                lhs: 0,
                rhs: 0,
            },
            0xd0,
        ),
        (
            NeqStr {
                dst: 0,
                lhs: 0,
                rhs: 0,
            },
            0xd1,
        ),
        (
            StrCat {
                dst: 0,
                lhs: 0,
                rhs: 0,
            },
            0xd2,
        ),
        (
            ClosureNew {
                dst: 0,
                func: 0,
                n: 0,
            },
            0xd3,
        ),
        (
            StrCatArena {
                dst: 0,
                lhs: 0,
                rhs: 0,
            },
            0xd4,
        ),
        (
            CaptureStore {
                src: 0,
                obj: 0,
                idx: 0,
            },
            0xd5,
        ),
        (ArenaMark { dst: 0 }, 0x93),
        (ArenaRestore { src: 0 }, 0x94),
    ]
    .into_iter()
    .chain(documented_call_opcodes())
    .collect()
}

/// The call-family opcode entries (CALL, CALLT, CALLG, CALLTG, CALLS): the
/// multi-line struct literals are the bulk of the table, so they live in
/// their own helper to keep each function readable.
fn documented_call_opcodes() -> Vec<(Uop, u8)> {
    vec![
        (
            Call {
                callee: K0,
                nargs: 0,
            },
            0xa0,
        ),
        (
            Callt {
                callee: K0,
                nargs: 0,
            },
            0xa1,
        ),
        (
            Callg {
                callee: K0,
                nargs: 0,
            },
            0xa4,
        ),
        (
            Calltg {
                callee: K0,
                nargs: 0,
            },
            0xa5,
        ),
        (
            Calls {
                callee_slot: 0,
                nargs: 0,
            },
            0xa6,
        ),
    ]
}

#[test]
fn opcodes_match_documented_codes_and_are_unique() {
    let expect = documented_opcodes();
    for (u, code) in &expect {
        assert_eq!(u.opcode(), *code);
        assert_eq!(u.major() as u8, code >> 4);
    }
    let codes: HashSet<_> = expect.iter().map(|(u, _)| u.opcode()).collect();
    assert_eq!(codes.len(), expect.len(), "opcode assignment is unique");
}

#[test]
fn every_sample_lands_on_a_documented_code() {
    let codes: HashSet<_> = documented_opcodes()
        .iter()
        .map(|(u, _)| u.opcode())
        .collect();
    for u in samples() {
        assert!(
            codes.contains(&u.opcode()),
            "opcode {:02X} not documented",
            u.opcode()
        );
    }
}

/// The module doc's field-map table as exact words: one sentinel instance
/// per variant — 0x11 in `a` (dst/cond/src/slot/nargs), 0x22 in `b`
/// (lhs/src/func/callee_slot, TAG_NEW's tag), 0x33 in `c` (rhs/tag/n),
/// 0x0203/0x00CC in `d` (pool index, frame slot count) — so a field
/// extraction that disagrees with the documented mapping fails here.
#[test]
fn field_map_matches_the_module_doc_table() {
    // every three-register variant: dst/lhs/rhs fill a/b/c
    macro_rules! r3_words {
        ($($op:literal $v:ident),* $(,)?) => { vec![
            $( ($v { dst: 0x11, lhs: 0x22, rhs: 0x33 },
                u32::from_be_bytes([$op, 0x11, 0x22, 0x33])) ),*
        ] };
    }
    let kidx = ConstIdx(0x0203);
    let mut cases: Vec<(Uop, u32)> = vec![
        (Kimm { dst: 0x11, kidx }, 0x0011_0203),
        (Kconst { dst: 0x11, kidx }, 0x0111_0203),
        (Kstr { dst: 0x11, kidx }, 0x0211_0203),
        (KstrBorrow { dst: 0x11, kidx }, 0x0311_0203),
        (
            ClosureNew {
                dst: 0x11,
                func: 0x22,
                n: 0x33,
            },
            0xd311_2233,
        ),
        (
            CaptureStore {
                src: 0x11,
                obj: 0x22,
                idx: 0x33,
            },
            0xd511_2233,
        ),
        (
            TagTest {
                dst: 0x11,
                src: 0x22,
                tag: 0x33,
            },
            0x6011_2233,
        ),
        (
            TagTestNot {
                dst: 0x11,
                src: 0x22,
                tag: 0x33,
            },
            0x6111_2233,
        ),
        (
            TagNew {
                dst: 0x11,
                tag: 0x22,
            },
            0x6211_2200,
        ),
        (
            CvtI2F {
                dst: 0x11,
                src: 0x22,
            },
            0x7011_2200,
        ),
        (
            CvtF2I {
                dst: 0x11,
                src: 0x22,
            },
            0x7111_2200,
        ),
        (
            Mov {
                dst: 0x11,
                src: 0x22,
            },
            0x8011_2200,
        ),
        // target 0x205 at pc 0: offset 0x204, biased field 0x8204
        (
            Jmpif {
                cond: 0x11,
                target: 0x205,
            },
            0x9011_8204,
        ),
        (
            JmpifNot {
                cond: 0x11,
                target: 0x205,
            },
            0x9111_8204,
        ),
        (Jmp { target: 0x205 }, 0x9200_8204),
        (ArenaMark { dst: 0x11 }, 0x9311_0000),
        (ArenaRestore { src: 0x11 }, 0x9411_0000),
        (
            Call {
                callee: kidx,
                nargs: 0x11,
            },
            0xa011_0203,
        ),
        (
            Callt {
                callee: kidx,
                nargs: 0x11,
            },
            0xa111_0203,
        ),
        (
            Callg {
                callee: kidx,
                nargs: 0x11,
            },
            0xa411_0203,
        ),
        (
            Calltg {
                callee: kidx,
                nargs: 0x11,
            },
            0xa511_0203,
        ),
        (
            Calls {
                callee_slot: 0x22,
                nargs: 0x11,
            },
            0xa611_2200,
        ),
        (Ret { src: 0x11 }, 0xa211_0000),
        (RetSpan { src: 0x11, n: 0x33 }, 0xa311_0033),
        (
            Funcf {
                frame_slots: 0x00cc,
            },
            0xb000_00cc,
        ),
        (Halt { slot: 0x11 }, 0xc011_0000),
    ];
    cases.extend(r3_words![
        0x10 AddI64, 0x11 SubI64, 0x12 MulI64, 0x13 DivI64, 0x14 ModI64, 0x20 AddF64, 0x21 SubF64,
        0x22 MulF64, 0x23 DivF64, 0x30 EqI64, 0x31 NeqI64, 0x32 LtI64, 0x33 GeI64, 0x34 LeI64,
        0x35 GtI64, 0x40 EqF64, 0x41 NeqF64, 0x42 LtF64, 0x43 GeF64, 0x44 LeF64, 0x45 GtF64,
        0x50 EqBool, 0x51 NeqBool, 0xD0 EqStr, 0xD1 NeqStr, 0xD2 StrCat, 0xD4 StrCatArena,
        0x15 WrapAddI64, 0x16 WrapSubI64, 0x17 WrapMulI64, 0x18 ShlI64, 0x19 ShrI64,
        0x1A BitAndI64, 0x1B BitXorI64, 0x1C BitOrI64,
    ]);
    assert_eq!(cases.len(), 61, "one sentinel per variant");
    for (uop, word) in cases {
        assert_eq!(uop.encode(0), Ok(word), "field map drift: {uop:?}");
    }
}

#[test]
fn major_from_byte_covers_assigned_and_rejects_reserved() {
    for byte in 0x0..=0xd {
        let m = Major::from_byte(byte).expect("assigned major");
        assert_eq!(m as u8, byte);
    }
    for byte in 0xe..=0xf {
        assert_eq!(Major::from_byte(byte), None, "reserved major {byte:X}");
    }
}

#[test]
fn xor_negation_adjacency_holds_for_every_comparison() {
    // 16 comparison constructors x 3 instances, plus one instance of each
    // tag test: everything negate() accepts must flip exactly one opcode bit
    let comparisons: Vec<_> = samples()
        .into_iter()
        .filter(|u| u.negate().is_some())
        .collect();
    assert_eq!(comparisons.len(), 16 * 3 + 2);
    for u in comparisons {
        let n = u.negate().expect("filtered");
        assert_eq!(n.opcode(), u.opcode() ^ 1, "{u:?}");
        assert_eq!(n.negate().expect("negation is an involution"), u);
        // operand words are untouched: only the opcode bit flips
        assert_eq!(
            n.encode(7),
            Ok(u.encode(7).expect("sample encodable") ^ (1 << 24))
        );
    }
    for u in [
        Kimm { dst: 0, kidx: K0 },
        Kstr { dst: 0, kidx: K0 },
        AddI64 {
            dst: 0,
            lhs: 0,
            rhs: 0,
        },
        StrCat {
            dst: 0,
            lhs: 0,
            rhs: 0,
        },
        Mov { dst: 0, src: 0 },
        Jmp { target: 0 },
        Jmpif { cond: 0, target: 0 },
        Call {
            callee: K0,
            nargs: 0,
        },
        RetSpan { src: 0, n: 2 },
        Halt { slot: 0 },
    ] {
        assert_eq!(u.negate(), None);
    }
}

#[test]
fn conditional_jump_sense_flip_is_one_bit() {
    // JMPIF^1 == JMPIF_NOT adjacency: a branch-sense rewrite is one opcode
    // bit, the foundation of compare-and-branch fusion [BL-4]
    let jf = Jmpif { cond: 9, target: 4 };
    let jfn = JmpifNot { cond: 9, target: 4 };
    assert_eq!(jf.opcode() ^ 1, jfn.opcode());
    assert_eq!(
        jf.encode(0),
        Ok(jfn.encode(0).expect("sample encodable") ^ (1 << 24))
    );
}

#[test]
fn bias_arithmetic_forward_backward_and_edges() {
    // zero offset: a jump to the next instruction
    assert_eq!(jump_field(5, 4), Ok(BIAS_J));
    assert_eq!(jump_target(BIAS_J, 4), Ok(5));
    // forward extreme: +32767
    assert_eq!(jump_field(0xffff, 0x7fff), Ok(0xffff));
    assert_eq!(jump_target(0xffff, 0x7fff), Ok(0xffff));
    // backward extreme: -32768
    assert_eq!(jump_field(0, 0x7fff), Ok(0x0000));
    assert_eq!(jump_target(0x0000, 0x7fff), Ok(0));
    // a jump to itself is one back (offset -1)
    assert_eq!(jump_field(3, 3), Ok(BIAS_J - 1));
    // one past either edge is a typed error
    assert_eq!(
        jump_field(0x1_0000, 0x7fff),
        Err(EncodeError::JumpOutOfRange {
            pc: 0x7fff,
            target: 0x1_0000
        })
    );
    assert_eq!(
        jump_field(0, 0x8001),
        Err(EncodeError::JumpOutOfRange {
            pc: 0x8001,
            target: 0
        })
    );
    // the branch-free decode identity: the top-bit flip IS the subtraction,
    // so target - next_pc equals field - BIAS_J over the full signed window
    for field in [0u16, 1, BIAS_J, BIAS_J + 1, 0xffff] {
        let pc = 40_000u32;
        let target = i64::from(jump_target(field, pc).expect("in range by construction"));
        assert_eq!(
            target - (i64::from(pc) + 1),
            i64::from(field) - i64::from(BIAS_J)
        );
    }
}

#[test]
fn malformed_encodings_are_rejected() {
    // reserved major
    assert_eq!(
        Uop::decode(0, 0xe0_00_00_00),
        Err(DecodeError::UnknownOpcode {
            major: 0xe,
            minor: 0x0
        })
    );
    // unassigned minor inside a valid major
    assert_eq!(
        Uop::decode(0, 0x36_00_00_00),
        Err(DecodeError::UnknownOpcode {
            major: 0x3,
            minor: 0x6
        })
    );
    assert_eq!(
        Uop::decode(0, 0x8f_00_00_00),
        Err(DecodeError::UnknownOpcode {
            major: 0x8,
            minor: 0xf
        })
    );
    // nonzero unused fields break canonicity
    for word in [
        0x92_01_80_00, // JMP with a != 0
        0xc0_00_00_01, // HALT with d != 0
        0x80_00_01_01, // MOV with c != 0
        0xa2_00_00_01, // RET with d != 0
        0x62_00_00_01, // TAG_NEW with c != 0
        0x70_00_00_01, // CVTI2F with c != 0
        0xb0_01_00_04, // FUNCF with a != 0
    ] {
        assert_eq!(
            Uop::decode(0, word),
            Err(DecodeError::ReservedField),
            "{word:08X}"
        );
    }
    // RETSPAN with a zero span or a nonzero reserved b field
    for word in [0xa3_00_00_00, 0xa3_00_02_01] {
        assert_eq!(
            Uop::decode(0, word),
            Err(DecodeError::ReservedField),
            "{word:08X}"
        );
    }
    // unassigned minor inside IntArith (the five new variants stop at
    // minor 0xC)
    assert_eq!(
        Uop::decode(0, 0x1d_00_00_00),
        Err(DecodeError::UnknownOpcode {
            major: 0x1,
            minor: 0xd
        })
    );
    // unassigned minor inside the Ref major (CAPTURE_STORE took 0xD.5,
    // leaving 0xD.6 the next free)
    assert_eq!(
        Uop::decode(0, 0xd6_00_00_00),
        Err(DecodeError::UnknownOpcode {
            major: 0xd,
            minor: 0x6
        })
    );
    // CALLTG (0xA.5) decodes with CALL's operand layout and re-encodes to
    // itself (the reserved slot became the dispatch tail call)
    assert_eq!(
        Uop::decode(0, 0xa5_02_ab_cd),
        Ok(Calltg {
            callee: ConstIdx(0xab_cd),
            nargs: 2,
        })
    );
    assert_eq!(
        Calltg {
            callee: ConstIdx(0xab_cd),
            nargs: 2,
        }
        .encode(0),
        Ok(0xa5_02_ab_cd)
    );
    // CALLG decodes with CALL's operand layout and re-encodes to itself
    assert_eq!(
        Uop::decode(0, 0xa4_01_ab_cd),
        Ok(Callg {
            callee: ConstIdx(0xab_cd),
            nargs: 1,
        })
    );
    assert_eq!(
        Callg {
            callee: ConstIdx(0xab_cd),
            nargs: 1,
        }
        .encode(0),
        Ok(0xa4_01_ab_cd)
    );
    // CALLS keeps the call-family `a` = nargs with the callee moved to the
    // `b` slot field and `c` reserved zero; it re-encodes to itself
    assert_eq!(
        Uop::decode(0, 0xa6_01_02_00),
        Ok(Calls {
            callee_slot: 2,
            nargs: 1,
        })
    );
    assert_eq!(
        Calls {
            callee_slot: 2,
            nargs: 1,
        }
        .encode(0),
        Ok(0xa6_01_02_00)
    );
    // a nonzero `c` is not canonical
    assert_eq!(
        Uop::decode(0, 0xa6_01_02_01),
        Err(DecodeError::ReservedField)
    );
    // biased field 0x0000 at pc 0 points below the code stream
    assert_eq!(
        Uop::decode(0, 0x90_00_00_00),
        Err(DecodeError::JumpOutOfRange {
            pc: 0,
            field: 0x0000
        })
    );
}

#[test]
fn funcf_frame_slot_cap_is_enforced_at_both_edges() {
    // The cap itself (256 slots, addressed by u8 slot operands) round-trips.
    let edge = Funcf {
        frame_slots: MAX_FRAME_SLOTS,
    };
    let word = edge.encode(0).expect("the cap itself is addressable");
    assert_eq!(Uop::decode(0, word), Ok(edge));
    // One past the cap is a typed encode error, not an unaddressable frame.
    let over = Funcf {
        frame_slots: MAX_FRAME_SLOTS + 1,
    };
    assert_eq!(
        over.encode(0),
        Err(EncodeError::FrameSlotsExceedCap { frame_slots: 257 })
    );
    // A word carrying an over-cap count never came from encode (0xB0, a = 0,
    // d = 0x0101): decode rejects it too.
    assert_eq!(
        Uop::decode(0, 0xb0_00_01_01),
        Err(DecodeError::FrameSlotsExceedCap { frame_slots: 257 })
    );
}

#[test]
fn ret_span_requires_at_least_one_word() {
    assert_eq!(
        RetSpan { src: 0, n: 0 }.encode(0),
        Err(EncodeError::RetSpanEmpty)
    );
    let one = RetSpan { src: 3, n: 1 };
    let word = one.encode(0).expect("the unit-word span encodes");
    assert_eq!(Uop::decode(0, word), Ok(one));
    let two = RetSpan { src: 3, n: 2 };
    let second = two.encode(0).expect("a nonempty span encodes");
    assert_eq!(Uop::decode(0, second), Ok(two));
}

#[test]
fn effects_are_consistent_with_slot_operands() {
    for u in samples() {
        let operands: Vec<u8> = match u {
            Kimm { dst, .. }
            | Kconst { dst, .. }
            | Kstr { dst, .. }
            | KstrBorrow { dst, .. }
            | TagNew { dst, .. }
            | ArenaMark { dst } => vec![dst],
            CvtI2F { dst, src }
            | CvtF2I { dst, src }
            | Mov { dst, src }
            | TagTest { dst, src, .. }
            | TagTestNot { dst, src, .. } => vec![dst, src],
            Jmpif { cond, .. } | JmpifNot { cond, .. } => vec![cond],
            ClosureNew { dst, func, .. } => vec![dst, func],
            CaptureStore { src, obj, .. } => vec![src, obj],
            Ret { src } | RetSpan { src, .. } | ArenaRestore { src } => vec![src],
            Halt { slot } => vec![slot],
            Calls { callee_slot, .. } => vec![callee_slot],
            Jmp { .. }
            | Funcf { .. }
            | Call { .. }
            | Callg { .. }
            | Callt { .. }
            | Calltg { .. } => vec![],
            AddI64 { dst, lhs, rhs }
            | SubI64 { dst, lhs, rhs }
            | MulI64 { dst, lhs, rhs }
            | DivI64 { dst, lhs, rhs }
            | ModI64 { dst, lhs, rhs }
            | WrapAddI64 { dst, lhs, rhs }
            | WrapSubI64 { dst, lhs, rhs }
            | WrapMulI64 { dst, lhs, rhs }
            | ShlI64 { dst, lhs, rhs }
            | ShrI64 { dst, lhs, rhs }
            | BitAndI64 { dst, lhs, rhs }
            | BitXorI64 { dst, lhs, rhs }
            | BitOrI64 { dst, lhs, rhs }
            | AddF64 { dst, lhs, rhs }
            | SubF64 { dst, lhs, rhs }
            | MulF64 { dst, lhs, rhs }
            | DivF64 { dst, lhs, rhs }
            | EqI64 { dst, lhs, rhs }
            | NeqI64 { dst, lhs, rhs }
            | LtI64 { dst, lhs, rhs }
            | GeI64 { dst, lhs, rhs }
            | LeI64 { dst, lhs, rhs }
            | GtI64 { dst, lhs, rhs }
            | EqF64 { dst, lhs, rhs }
            | NeqF64 { dst, lhs, rhs }
            | LtF64 { dst, lhs, rhs }
            | GeF64 { dst, lhs, rhs }
            | LeF64 { dst, lhs, rhs }
            | GtF64 { dst, lhs, rhs }
            | EqBool { dst, lhs, rhs }
            | NeqBool { dst, lhs, rhs }
            | EqStr { dst, lhs, rhs }
            | NeqStr { dst, lhs, rhs }
            | StrCat { dst, lhs, rhs }
            | StrCatArena { dst, lhs, rhs } => vec![dst, lhs, rhs],
        };
        for i in u.reads().as_slice() {
            assert!(
                operands.contains(i),
                "read {i} of {u:?} is not a slot operand"
            );
        }
        for i in u.writes().as_slice() {
            assert!(
                operands.contains(i),
                "write {i} of {u:?} is not a slot operand"
            );
        }
        // ops with no slot operand have no listed effects
        if operands.is_empty() {
            assert!(u.reads().is_empty() && u.writes().is_empty(), "{u:?}");
        }
        // every data op writes exactly its dst slot; control and frame ops
        // write none of their own frame's slots
        let writes_one = !matches!(
            u,
            Jmp { .. }
                | Jmpif { .. }
                | JmpifNot { .. }
                | ArenaRestore { .. }
                | Call { .. }
                | Callg { .. }
                | Calls { .. }
                | Callt { .. }
                | Calltg { .. }
                | CaptureStore { .. }
                | Ret { .. }
                | RetSpan { .. }
                | Funcf { .. }
                | Halt { .. }
        );
        assert_eq!(u.writes().len(), usize::from(writes_one), "{u:?}");
    }
}

#[test]
fn effect_lists_report_expected_slots() {
    // dst may double as an operand (acc += n): reads and writes may overlap
    let add = AddI64 {
        dst: 1,
        lhs: 1,
        rhs: 0,
    };
    assert_eq!(add.reads().as_slice(), &[1, 0]);
    assert_eq!(add.writes().as_slice(), &[1]);
    let jf = Jmpif { cond: 2, target: 5 };
    assert_eq!(jf.reads().as_slice(), &[2]);
    assert!(jf.writes().is_empty());
    let k = Kimm { dst: 7, kidx: K0 };
    assert!(k.reads().is_empty());
    assert_eq!(k.writes().as_slice(), &[7]);
    let eq = EqStr {
        dst: 1,
        lhs: 2,
        rhs: 3,
    };
    assert_eq!(eq.reads().as_slice(), &[2, 3]);
    assert_eq!(eq.writes().as_slice(), &[1]);
    let rs = RetSpan { src: 4, n: 2 };
    assert_eq!(rs.reads().as_slice(), &[4]);
    assert!(rs.writes().is_empty());
    // CALLS reads its callee slot operand and writes none of its own frame
    let cs = Calls {
        callee_slot: 5,
        nargs: 1,
    };
    assert_eq!(cs.reads().as_slice(), &[5]);
    assert!(cs.writes().is_empty());
}

#[test]
fn purity_table_is_exact() {
    for u in samples() {
        let pure = !matches!(
            u,
            Jmp { .. }
                | Jmpif { .. }
                | JmpifNot { .. }
                | Call { .. }
                | Callg { .. }
                | Calls { .. }
                | Callt { .. }
                | Calltg { .. }
                | Ret { .. }
                | RetSpan { .. }
                | Kstr { .. }
                | KstrBorrow { .. }
                | StrCat { .. }
                | StrCatArena { .. }
                | ArenaMark { .. }
                | ArenaRestore { .. }
                | ClosureNew { .. }
                | CaptureStore { .. }
                | Funcf { .. }
                | Halt { .. }
        );
        assert_eq!(u.is_pure(), pure, "{u:?}");
    }
}

/// `fn sum_to(n: i64) -> i64 { let mut acc = 0i64; while n > 0 { acc += n; n -= 1 }; acc }`
/// from the archive seed corpus, ported to the locked encoding: immediates ride
/// the constant pool (k0 = 0i64, k1 = 1i64) because comparisons take slot
/// operands only (BL-1). Slots: r0 = n, r1 = acc, r2 = compare bool, r3 =
/// the zero constant, r4 = the one constant.
// archive sketch erratum: its `JMPIF_NOT` exit polarity (jumps when the
// condition is false) would leave the loop on the first iteration, and its
// single scratch aliases the 0/1 constants into `n <= 1`; corrected here to
// JMPIF on `n <= 0` with separate scratch slots, matching the executable
// ds-vm fixture
fn sum_to() -> Vec<Uop> {
    vec![
        Funcf { frame_slots: 5 }, // 0
        Kimm {
            dst: 1,
            kidx: ConstIdx(0),
        }, // 1: acc = 0
        Kimm {
            dst: 3,
            kidx: ConstIdx(0),
        }, // 2: r3 = 0
        LeI64 {
            dst: 2,
            lhs: 0,
            rhs: 3,
        }, // 3: r2 = n <= r3
        Jmpif { cond: 2, target: 9 }, // 4: exit while n > 0 is false, i.e. n <= 0
        AddI64 {
            dst: 1,
            lhs: 1,
            rhs: 0,
        }, // 5: acc += n
        Kimm {
            dst: 4,
            kidx: ConstIdx(1),
        }, // 6: r4 = 1
        SubI64 {
            dst: 0,
            lhs: 0,
            rhs: 4,
        }, // 7: n -= 1
        Jmp { target: 3 },        // 8: loop
        Ret { src: 1 },           // 9
    ]
}

#[test]
fn sum_to_fixture_round_trips_with_biased_jumps() {
    let prog = sum_to();
    let words: Vec<_> = prog
        .iter()
        .enumerate()
        .map(|(pc, u)| {
            u.encode(u32::try_from(pc).expect("pc fits u32"))
                .expect("fixture encodes")
        })
        .collect();
    let back: Vec<_> = words
        .iter()
        .enumerate()
        .map(|(pc, w)| {
            Uop::decode(u32::try_from(pc).expect("pc fits u32"), *w).expect("fixture decodes")
        })
        .collect();
    assert_eq!(back, prog, "decode returns the exact written vector");
    // forward conditional exit at 4 -> 9 (JMPIF on `n <= 0`): offset +4
    // (target - next_pc)
    assert_eq!(jump_field(9, 4), Ok(BIAS_J + 4));
    assert_eq!(words[4] & 0xffff, u32::from(BIAS_J + 4));
    assert_eq!(Uop::decode(4, words[4]), Ok(Jmpif { cond: 2, target: 9 }));
    // backward loop at 8 -> 3: offset -6, below the bias origin
    assert_eq!(jump_field(3, 8), Ok(BIAS_J - 6));
    assert!(
        words[8] & 0xffff < u32::from(BIAS_J),
        "backward jump field is below the bias origin"
    );
}

/// The mutual `even`/`odd` (H2's example). Slots:
/// r0 = n, r1 = compare bool, r2 = constant scratch, r3 = the outgoing
/// argument (the slide source the ABI derives as the arg-area slot). Pool:
/// k0 = 0i64, k1 = 1i64, k2 = true, k3 = FuncRef odd, k4 = FuncRef even,
/// k5 = false.
fn even() -> Vec<Uop> {
    vec![
        Funcf { frame_slots: 4 }, // 0
        Kimm {
            dst: 2,
            kidx: ConstIdx(0),
        }, // 1: stage 0
        EqI64 {
            dst: 1,
            lhs: 0,
            rhs: 2,
        }, // 2: r1 = n == 0
        Jmpif { cond: 1, target: 7 }, // 3
        Kimm {
            dst: 2,
            kidx: ConstIdx(1),
        }, // 4: stage 1
        SubI64 {
            dst: 3,
            lhs: 0,
            rhs: 2,
        }, // 5: r3 = n - 1 (arg area)
        Callt {
            callee: ConstIdx(3),
            nargs: 1,
        }, // 6: slide r3 into slot0
        Kconst {
            dst: 1,
            kidx: ConstIdx(2),
        }, // 7: r1 = true
        Ret { src: 1 },           // 8
    ]
}

fn odd() -> Vec<Uop> {
    vec![
        Funcf { frame_slots: 4 }, // 0
        Kimm {
            dst: 2,
            kidx: ConstIdx(0),
        }, // 1
        EqI64 {
            dst: 1,
            lhs: 0,
            rhs: 2,
        }, // 2: r1 = n == 0
        Jmpif { cond: 1, target: 7 }, // 3
        Kimm {
            dst: 2,
            kidx: ConstIdx(1),
        }, // 4
        SubI64 {
            dst: 3,
            lhs: 0,
            rhs: 2,
        }, // 5
        Callt {
            callee: ConstIdx(4),
            nargs: 1,
        }, // 6: tail-call even
        Kconst {
            dst: 1,
            kidx: ConstIdx(5),
        }, // 7: r1 = false
        Ret { src: 1 },           // 8
    ]
}

#[test]
fn even_odd_fixtures_round_trip_and_slide_from_r3() {
    for (prog, callee_kidx) in [(even(), 3u32), (odd(), 4)] {
        let words: Vec<_> = prog
            .iter()
            .enumerate()
            .map(|(pc, u)| {
                u.encode(u32::try_from(pc).expect("pc fits u32"))
                    .expect("fixture encodes")
            })
            .collect();
        let back: Vec<_> = words
            .iter()
            .enumerate()
            .map(|(pc, w)| {
                Uop::decode(u32::try_from(pc).expect("pc fits u32"), *w).expect("fixture decodes")
            })
            .collect();
        assert_eq!(back, prog, "decode returns the exact written vector");
        // JMPIF at 3 -> 7 is the +3 bias, exactly the record's shape
        assert_eq!(words[3] & 0xffff, u32::from(BIAS_J + 3));
        // CALLT's d field is the callee pool index, its a field the nargs
        assert_eq!(words[6] & 0xffff, callee_kidx);
        assert_eq!((words[6] >> 16) & 0xff, 1);
    }
    // the record's "src=r3" is the arg-area slot the locked ABI derives:
    // frame = [n:i64 param, bool, i64 scratch, i64 outgoing], arg area = 1
    let even_layout = crate::layout::FrameLayout::new(
        [
            crate::layout::SlotType::I64,
            crate::layout::SlotType::Bool,
            crate::layout::SlotType::I64,
            crate::layout::SlotType::I64,
        ]
        .to_vec(),
        1,
        1,
    )
    .expect("valid layout");
    let plan = crate::slide_plan(&even_layout, &even_layout).expect("layouts agree");
    assert_eq!(plan.src(), 3..4, "the derived slide source is r3");
    assert_eq!(plan.dst(), 0..1);
}

#[test]
fn fixtures_exercise_the_uops_the_record_spells() {
    // the two seed encodings alone cover the record's spelled instruction
    // set (sum_to in its executable form): FUNCF, LDIMM->KIMM/KCONST,
    // ADDI64, SUBI64, LEI64, EQI64, JMPIF, JMP, CALLT, RET
    let mut variants = Vec::new();
    for u in sum_to().into_iter().chain(even()).chain(odd()) {
        let d = discriminant(&u);
        if !variants.contains(&d) {
            variants.push(d);
        }
    }
    assert_eq!(variants.len(), 11);
}
