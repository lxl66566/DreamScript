//! The 32-bit fixed encoding (BL-1): `encode`/`decode` with typed errors.
//!
//! Field packing per the module-doc bit layout: opcode byte \[31:24] =
//! `major << 4 | minor`, slot/`a` \[23:16], then either two u8 operands
//! `b` \[15:8] / `c` \[7:0] or one u16 `d` \[15:0]. Encodings are canonical:
//! every unused field bit is zero, and decode rejects violations, so the
//! word <-> uop mapping is bijective on accepted inputs.

use super::{
    ConstIdx, MAX_FRAME_SLOTS,
    Major::{
        self, BoolCmp, Call, Const, Ctl, Cvt, FloatArith, FloatCmp, Frame, Halt, IntArith, IntCmp,
        Mov, Ref, Tag,
    },
    Uop::{
        self, AddF64, AddI64, ArenaMark, ArenaRestore, Call as UCall, Callg as UCallg,
        Calls as UCalls, Callt, Calltg as UCalltg, ClosureNew as UClosureNew, CvtF2I, CvtI2F,
        DivF64, DivI64, EqBool, EqF64, EqI64, EqStr, Funcf, GeF64, GeI64, GtF64, GtI64,
        Halt as UHalt, Jmp, Jmpif, JmpifNot, Kconst as UKconst, Kimm as UKimm, Kstr as UKstr,
        KstrBorrow as UKstrBorrow, LeF64, LeI64, LtF64, LtI64, ModI64, Mov as UMov, MulF64, MulI64,
        NeqBool, NeqF64, NeqI64, NeqStr, Ret, RetSpan, StrCat, StrCatArena, SubF64, SubI64, TagNew,
        TagTest, TagTestNot,
    },
};

/// Why a [`Uop`] cannot be encoded at a pc.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EncodeError {
    /// The jump displacement exceeds the biased u16 window
    /// `[-32768, 32767]` around the next pc.
    JumpOutOfRange { pc: u32, target: u32 },
    /// FUNCF declares more than [`super::MAX_FRAME_SLOTS`] slots: slot
    /// operands are u8, so higher counts are unaddressable.
    FrameSlotsExceedCap { frame_slots: u16 },
    /// RETSPAN declares a zero-word span. A span return moves at least the
    /// head word; a unit return is the single-word RET (whose word the
    /// caller of a unit callee never reads).
    RetSpanEmpty,
}

/// Why a 32-bit word cannot be decoded at a pc.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecodeError {
    /// The opcode byte names no assigned major.minor pair.
    UnknownOpcode { major: u8, minor: u8 },
    /// An unused field bit is nonzero: the word is not a canonical encoding.
    ReservedField,
    /// The biased jump offset cannot name a pc (below zero or past u32
    /// range); only reachable in a corrupted stream.
    JumpOutOfRange { pc: u32, field: u16 },
    /// The FUNCF frame slot count exceeds [`super::MAX_FRAME_SLOTS`]; only
    /// reachable in a corrupted stream, since encode validates.
    FrameSlotsExceedCap { frame_slots: u16 },
}

impl Uop {
    /// The opcode byte: `major << 4 | minor`. The low minor bit carries
    /// comparison negation (`opcode ^ 1`), so assembling a negated pair must
    /// never disturb it.
    #[must_use]
    pub fn opcode(&self) -> u8 {
        (self.major() as u8) << 4 | self.minor()
    }

    /// Encodes `self` located at instruction index `pc` into the fixed
    /// 32-bit word. Jump targets resolve against `pc + 1`.
    pub fn encode(&self, pc: u32) -> Result<u32, EncodeError> {
        let op = u32::from(self.opcode());
        let a = u32::from(self.a_field());
        let bc = match self.bc_field(pc)? {
            Bc::Reg { b, c } => u32::from(b) << 8 | u32::from(c),
            Bc::Wide(d) => u32::from(d),
        };
        Ok(op << 24 | a << 16 | bc)
    }

    /// Decodes the 32-bit word at instruction index `pc`. Inverse of
    /// [`Uop::encode`]: every canonical word decodes, and every accepted
    /// word re-encodes to itself.
    pub fn decode(pc: u32, word: u32) -> Result<Self, DecodeError> {
        let [byte, a, b, c] = word.to_be_bytes();
        let major = byte >> 4;
        let minor = byte & 0x0f;
        let d = u16::from_be_bytes([b, c]);
        let cx = |ok: bool| ok.then_some(()).ok_or(DecodeError::ReservedField);
        // every three-register data op fills a, b, c identically
        macro_rules! r3 {
            ($v:ident) => {
                Ok($v {
                    dst: a,
                    lhs: b,
                    rhs: c,
                })
            };
        }
        match Major::from_byte(major).ok_or(DecodeError::UnknownOpcode { major, minor })? {
            Const => match minor {
                0 => Ok(UKimm {
                    dst: a,
                    kidx: ConstIdx(d),
                }),
                1 => Ok(UKconst {
                    dst: a,
                    kidx: ConstIdx(d),
                }),
                2 => Ok(UKstr {
                    dst: a,
                    kidx: ConstIdx(d),
                }),
                3 => Ok(UKstrBorrow {
                    dst: a,
                    kidx: ConstIdx(d),
                }),
                _ => Err(DecodeError::UnknownOpcode { major, minor }),
            },
            IntArith => match minor {
                0 => r3!(AddI64),
                1 => r3!(SubI64),
                2 => r3!(MulI64),
                3 => r3!(DivI64),
                4 => r3!(ModI64),
                _ => Err(DecodeError::UnknownOpcode { major, minor }),
            },
            FloatArith => match minor {
                0 => r3!(AddF64),
                1 => r3!(SubF64),
                2 => r3!(MulF64),
                3 => r3!(DivF64),
                _ => Err(DecodeError::UnknownOpcode { major, minor }),
            },
            IntCmp => match minor {
                0 => r3!(EqI64),
                1 => r3!(NeqI64),
                2 => r3!(LtI64),
                3 => r3!(GeI64),
                4 => r3!(LeI64),
                5 => r3!(GtI64),
                _ => Err(DecodeError::UnknownOpcode { major, minor }),
            },
            FloatCmp => match minor {
                0 => r3!(EqF64),
                1 => r3!(NeqF64),
                2 => r3!(LtF64),
                3 => r3!(GeF64),
                4 => r3!(LeF64),
                5 => r3!(GtF64),
                _ => Err(DecodeError::UnknownOpcode { major, minor }),
            },
            BoolCmp => match minor {
                0 => r3!(EqBool),
                1 => r3!(NeqBool),
                _ => Err(DecodeError::UnknownOpcode { major, minor }),
            },
            Ref => match minor {
                0 => r3!(EqStr),
                1 => r3!(NeqStr),
                2 => r3!(StrCat),
                // a = dst, b = the Fn word slot, c = the capture count
                3 => Ok(UClosureNew {
                    dst: a,
                    func: b,
                    n: c,
                }),
                4 => r3!(StrCatArena),
                _ => Err(DecodeError::UnknownOpcode { major, minor }),
            },
            Tag => match minor {
                0 => Ok(TagTest {
                    dst: a,
                    src: b,
                    tag: c,
                }),
                1 => Ok(TagTestNot {
                    dst: a,
                    src: b,
                    tag: c,
                }),
                2 => {
                    cx(c == 0)?;
                    Ok(TagNew { dst: a, tag: b })
                },
                _ => Err(DecodeError::UnknownOpcode { major, minor }),
            },
            Cvt => match minor {
                0 => {
                    cx(c == 0)?;
                    Ok(CvtI2F { dst: a, src: b })
                },
                1 => {
                    cx(c == 0)?;
                    Ok(CvtF2I { dst: a, src: b })
                },
                _ => Err(DecodeError::UnknownOpcode { major, minor }),
            },
            Mov => match minor {
                0 => {
                    cx(c == 0)?;
                    Ok(UMov { dst: a, src: b })
                },
                _ => Err(DecodeError::UnknownOpcode { major, minor }),
            },
            Ctl => match minor {
                // JMPIF at 0 and JMPIF_NOT at 1: the XOR-adjacent pair
                0 => Ok(Jmpif {
                    cond: a,
                    target: super::jump_target(d, pc)?,
                }),
                1 => Ok(JmpifNot {
                    cond: a,
                    target: super::jump_target(d, pc)?,
                }),
                2 => {
                    cx(a == 0)?;
                    Ok(Jmp {
                        target: super::jump_target(d, pc)?,
                    })
                },
                3 => {
                    cx(b == 0)?;
                    cx(c == 0)?;
                    Ok(ArenaMark { dst: a })
                },
                4 => {
                    cx(b == 0)?;
                    cx(c == 0)?;
                    Ok(ArenaRestore { src: a })
                },
                _ => Err(DecodeError::UnknownOpcode { major, minor }),
            },
            Call => match minor {
                0 => Ok(UCall {
                    callee: ConstIdx(d),
                    nargs: a,
                }),
                1 => Ok(Callt {
                    callee: ConstIdx(d),
                    nargs: a,
                }),
                4 => Ok(UCallg {
                    callee: ConstIdx(d),
                    nargs: a,
                }),
                5 => Ok(UCalltg {
                    callee: ConstIdx(d),
                    nargs: a,
                }),
                6 => {
                    cx(c == 0)?;
                    Ok(UCalls {
                        callee_slot: b,
                        nargs: a,
                    })
                },
                2 => {
                    cx(d == 0)?;
                    Ok(Ret { src: a })
                },
                3 => {
                    // a = src, c = span length, b reserved zero; n = 0 is
                    // not canonical (encode rejects it), so decode must too
                    cx(b == 0)?;
                    cx(c != 0)?;
                    Ok(RetSpan { src: a, n: c })
                },
                _ => Err(DecodeError::UnknownOpcode { major, minor }),
            },
            Frame => {
                cx(minor == 0)?;
                cx(a == 0)?;
                if d > MAX_FRAME_SLOTS {
                    return Err(DecodeError::FrameSlotsExceedCap { frame_slots: d });
                }
                Ok(Funcf { frame_slots: d })
            },
            Halt => {
                cx(minor == 0)?;
                cx(d == 0)?;
                Ok(UHalt { slot: a })
            },
        }
    }

    /// The minor opcode nibble; every variant is pinned in its doc comment.
    #[must_use]
    pub fn minor(&self) -> u8 {
        match self {
            UKimm { .. }
            | AddI64 { .. }
            | AddF64 { .. }
            | EqI64 { .. }
            | EqF64 { .. }
            | EqBool { .. }
            | EqStr { .. }
            | TagTest { .. }
            | CvtI2F { .. }
            | UMov { .. }
            | Jmpif { .. }
            | UCall { .. }
            | Funcf { .. }
            | UHalt { .. } => 0,
            UKconst { .. }
            | SubI64 { .. }
            | SubF64 { .. }
            | NeqI64 { .. }
            | NeqF64 { .. }
            | NeqBool { .. }
            | NeqStr { .. }
            | TagTestNot { .. }
            | CvtF2I { .. }
            | JmpifNot { .. }
            | Callt { .. } => 1,
            MulI64 { .. }
            | MulF64 { .. }
            | LtI64 { .. }
            | LtF64 { .. }
            | UKstr { .. }
            | StrCat { .. }
            | TagNew { .. }
            | Jmp { .. }
            | Ret { .. } => 2,
            // minor 3 spans majors: KSTR_BORROW, DIVI64/GEI64/RETSPAN
            // and CLOSURE_NEW
            UKstrBorrow { .. }
            | DivI64 { .. }
            | DivF64 { .. }
            | GeI64 { .. }
            | GeF64 { .. }
            | RetSpan { .. }
            | UClosureNew { .. }
            | ArenaMark { .. } => 3,
            // minor 4 spans majors: MODI64/LEI64/LEF64, CALLG, and
            // STRCAT_ARENA
            ModI64 { .. }
            | LeI64 { .. }
            | LeF64 { .. }
            | UCallg { .. }
            | ArenaRestore { .. }
            | StrCatArena { .. } => 4,
            // minor 5 spans majors: GTI64/GTF64 and CALLTG
            GtI64 { .. } | GtF64 { .. } | UCalltg { .. } => 5,
            UCalls { .. } => 6,
        }
    }

    fn a_field(self) -> u8 {
        match self {
            UKimm { dst, .. }
            | UKconst { dst, .. }
            | AddI64 { dst, .. }
            | SubI64 { dst, .. }
            | MulI64 { dst, .. }
            | DivI64 { dst, .. }
            | ModI64 { dst, .. }
            | AddF64 { dst, .. }
            | SubF64 { dst, .. }
            | MulF64 { dst, .. }
            | DivF64 { dst, .. }
            | EqI64 { dst, .. }
            | NeqI64 { dst, .. }
            | LtI64 { dst, .. }
            | GeI64 { dst, .. }
            | LeI64 { dst, .. }
            | GtI64 { dst, .. }
            | EqF64 { dst, .. }
            | NeqF64 { dst, .. }
            | LtF64 { dst, .. }
            | GeF64 { dst, .. }
            | LeF64 { dst, .. }
            | GtF64 { dst, .. }
            | EqBool { dst, .. }
            | NeqBool { dst, .. }
            | EqStr { dst, .. }
            | NeqStr { dst, .. }
            | StrCat { dst, .. }
            | StrCatArena { dst, .. }
            | ArenaMark { dst }
            | UKstr { dst, .. }
            | UKstrBorrow { dst, .. }
            | TagTest { dst, .. }
            | TagTestNot { dst, .. }
            | TagNew { dst, .. }
            | CvtI2F { dst, .. }
            | CvtF2I { dst, .. }
            | UClosureNew { dst, .. }
            | UMov { dst, .. } => dst,
            Jmpif { cond, .. } | JmpifNot { cond, .. } => cond,
            UCall { nargs, .. }
            | Callt { nargs, .. }
            | UCallg { nargs, .. }
            | UCalltg { nargs, .. }
            | UCalls { nargs, .. } => nargs,
            Ret { src } | RetSpan { src, .. } | ArenaRestore { src } => src,
            UHalt { slot } => slot,
            Jmp { .. } | Funcf { .. } => 0,
        }
    }

    fn bc_field(self, pc: u32) -> Result<Bc, EncodeError> {
        Ok(match self {
            UKimm { kidx, .. }
            | UKconst { kidx, .. }
            | UKstr { kidx, .. }
            | UKstrBorrow { kidx, .. } => Bc::Wide(kidx.0),
            UCall { callee, .. }
            | Callt { callee, .. }
            | UCallg { callee, .. }
            | UCalltg { callee, .. } => Bc::Wide(callee.0),
            UCalls { callee_slot, .. } => Bc::Reg {
                b: callee_slot,
                c: 0,
            },
            Funcf { frame_slots } => {
                if frame_slots > MAX_FRAME_SLOTS {
                    return Err(EncodeError::FrameSlotsExceedCap { frame_slots });
                }
                Bc::Wide(frame_slots)
            },
            Jmp { target } | Jmpif { target, .. } | JmpifNot { target, .. } => {
                Bc::Wide(super::jump_field(target, pc)?)
            },
            RetSpan { n, .. } => {
                if n == 0 {
                    return Err(EncodeError::RetSpanEmpty);
                }
                Bc::Reg { b: 0, c: n }
            },
            TagNew { tag, .. } => Bc::Reg { b: tag, c: 0 },
            UClosureNew { func, n, .. } => Bc::Reg { b: func, c: n },
            TagTest { src, tag, .. } | TagTestNot { src, tag, .. } => Bc::Reg { b: src, c: tag },
            CvtI2F { src, .. } | CvtF2I { src, .. } | UMov { src, .. } => Bc::Reg { b: src, c: 0 },
            AddI64 { lhs, rhs, .. }
            | SubI64 { lhs, rhs, .. }
            | MulI64 { lhs, rhs, .. }
            | DivI64 { lhs, rhs, .. }
            | ModI64 { lhs, rhs, .. }
            | AddF64 { lhs, rhs, .. }
            | SubF64 { lhs, rhs, .. }
            | MulF64 { lhs, rhs, .. }
            | DivF64 { lhs, rhs, .. }
            | EqI64 { lhs, rhs, .. }
            | NeqI64 { lhs, rhs, .. }
            | LtI64 { lhs, rhs, .. }
            | GeI64 { lhs, rhs, .. }
            | LeI64 { lhs, rhs, .. }
            | GtI64 { lhs, rhs, .. }
            | EqF64 { lhs, rhs, .. }
            | NeqF64 { lhs, rhs, .. }
            | LtF64 { lhs, rhs, .. }
            | GeF64 { lhs, rhs, .. }
            | LeF64 { lhs, rhs, .. }
            | GtF64 { lhs, rhs, .. }
            | EqBool { lhs, rhs, .. }
            | NeqBool { lhs, rhs, .. }
            | EqStr { lhs, rhs, .. }
            | NeqStr { lhs, rhs, .. }
            | StrCat { lhs, rhs, .. }
            | StrCatArena { lhs, rhs, .. } => Bc::Reg { b: lhs, c: rhs },
            Ret { .. } | UHalt { .. } | ArenaMark { .. } | ArenaRestore { .. } => Bc::Wide(0),
        })
    }
}

enum Bc {
    Reg { b: u8, c: u8 },
    Wide(u16),
}
