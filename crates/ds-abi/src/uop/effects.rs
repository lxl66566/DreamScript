//! Slot effects and purity metadata (BL-5): the tables that make uop fusion
//! safe (BL-4) and that drive stencil generation (one stencil per uop, the
//! read/write sets become the holes' dependencies).

use super::{
    Uop,
    Uop::{
        AddF64, AddI64, Call, Callg, Calls, Callt, ClosureNew, CvtF2I, CvtI2F, DivF64, DivI64,
        EqBool, EqF64, EqI64, EqStr, Funcf, GeF64, GeI64, GtF64, GtI64, Halt, Jmp, Jmpif, JmpifNot,
        Kconst, Kimm, Kstr, LeF64, LeI64, LtF64, LtI64, ModI64, Mov, MulF64, MulI64, NeqBool,
        NeqF64, NeqI64, NeqStr, Ret, RetSpan, StrCat, SubF64, SubI64, TagNew, TagTest, TagTestNot,
    },
};

/// An operation class: the opcode byte's major nibble. The class fixes the
/// operand shape (which fields mean what); the minor selects the variant.
/// 14 of 16 codes are assigned. Major 0xE belongs to the M4
/// reference-counting pair RCINC/RCDEC, which lives beside `Uop` as
/// [`super::RcOp`] (body words of both majors decode through
/// [`super::Inst`]); 0xF stays reserved.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Major {
    /// 0x0 — constant loads: KIMM, KCONST (`a` = dst, `d` = pool index).
    Const = 0x0,
    /// 0x1 — i64 arithmetic: ADDI64, SUBI64, MULI64, DIVI64, MODI64.
    IntArith = 0x1,
    /// 0x2 — f64 arithmetic: ADDF64, SUBF64, MULF64, DIVF64.
    FloatArith = 0x2,
    /// 0x3 — i64 comparisons: EQI64 .. GTI64 (XOR-negated minor pairs).
    IntCmp = 0x3,
    /// 0x4 — f64 comparisons: EQF64 .. GTF64 (XOR-negated minor pairs).
    FloatCmp = 0x4,
    /// 0x5 — bool comparisons: EQBOOL, NEQBOOL (XOR-negated pair).
    BoolCmp = 0x5,
    /// 0xD — heap-reference data ops: EQSTR, NEQSTR (XOR-negated pair),
    /// STRCAT over string contents, CLOSURE_NEW building a closure object.
    Ref = 0xd,
    /// 0x6 — tag words (BL-2): TAG_TEST(_NOT), TAG_NEW.
    Tag = 0x6,
    /// 0x7 — conversions: CVTI2F, CVTF2I (B7 explicit conversions).
    Cvt = 0x7,
    /// 0x8 — slot moves: MOV.
    Mov = 0x8,
    /// 0x9 — control: JMPIF, JMPIF_NOT, JMP (biased offsets; the two
    /// conditional jumps are the XOR-adjacent minor pair 0/1).
    Ctl = 0x9,
    /// 0xA — calls: CALL, CALLT, RET, RETSPAN, CALLG (CALLTG, minor 5,
    /// reserved for the generic tail call), CALLS (minor 6, the frame-slot
    /// callee form).
    Call = 0xa,
    /// 0xB — frame protocol: FUNCF.
    Frame = 0xb,
    /// 0xC — run termination: HALT.
    Halt = 0xc,
}

impl Major {
    /// The assigned code, or `None` for reserved majors.
    #[must_use]
    pub fn from_byte(byte: u8) -> Option<Self> {
        Some(match byte {
            0x0 => Self::Const,
            0x1 => Self::IntArith,
            0x2 => Self::FloatArith,
            0x3 => Self::IntCmp,
            0x4 => Self::FloatCmp,
            0x5 => Self::BoolCmp,
            0x6 => Self::Tag,
            0x7 => Self::Cvt,
            0x8 => Self::Mov,
            0x9 => Self::Ctl,
            0xa => Self::Call,
            0xb => Self::Frame,
            0xc => Self::Halt,
            0xd => Self::Ref,
            _ => return None,
        })
    }
}

/// A by-value list of at most three slot indices: effect queries never
/// allocate (BL-5). Constructed only by [`Uop::reads`] and [`Uop::writes`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SlotList {
    slots: [u8; 3],
    len: u8,
}

impl SlotList {
    /// The listed slot indices.
    #[must_use]
    pub fn as_slice(&self) -> &[u8] {
        &self.slots[..usize::from(self.len)]
    }

    /// How many slot indices are listed.
    #[must_use]
    pub fn len(&self) -> usize {
        usize::from(self.len)
    }

    /// Whether no slot is listed.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The empty list.
    fn none() -> Self {
        Self {
            slots: [0; 3],
            len: 0,
        }
    }

    /// Lists `first` plus `rest` (at most two more).
    fn new(first: u8, rest: &[u8]) -> Self {
        debug_assert!(rest.len() <= 2, "capacity is three slots");
        let mut this = Self {
            slots: [first, 0, 0],
            len: 1,
        };
        for &slot in rest {
            this.slots[usize::from(this.len)] = slot;
            this.len += 1;
        }
        this
    }
}

/// Lists only slot *operands* of the uop — layout-relative effects that no
/// instruction operand names (CALL/CALLT read the caller's staged argument
/// span and RET writes the caller's overlay slot 0) are documented in the
/// module docs, not listed here.
impl Uop {
    /// The operation class (opcode major nibble).
    #[must_use]
    pub fn major(&self) -> Major {
        match self {
            Kimm { .. } | Kconst { .. } | Kstr { .. } => Major::Const,
            AddI64 { .. } | SubI64 { .. } | MulI64 { .. } | DivI64 { .. } | ModI64 { .. } => {
                Major::IntArith
            },
            AddF64 { .. } | SubF64 { .. } | MulF64 { .. } | DivF64 { .. } => Major::FloatArith,
            EqI64 { .. }
            | NeqI64 { .. }
            | LtI64 { .. }
            | GeI64 { .. }
            | LeI64 { .. }
            | GtI64 { .. } => Major::IntCmp,
            EqF64 { .. }
            | NeqF64 { .. }
            | LtF64 { .. }
            | GeF64 { .. }
            | LeF64 { .. }
            | GtF64 { .. } => Major::FloatCmp,
            EqBool { .. } | NeqBool { .. } => Major::BoolCmp,
            EqStr { .. } | NeqStr { .. } | StrCat { .. } | ClosureNew { .. } => Major::Ref,
            TagTest { .. } | TagTestNot { .. } | TagNew { .. } => Major::Tag,
            CvtI2F { .. } | CvtF2I { .. } => Major::Cvt,
            Mov { .. } => Major::Mov,
            Jmp { .. } | Jmpif { .. } | JmpifNot { .. } => Major::Ctl,
            Call { .. }
            | Callg { .. }
            | Calls { .. }
            | Callt { .. }
            | Ret { .. }
            | RetSpan { .. } => Major::Call,
            Funcf { .. } => Major::Frame,
            Halt { .. } => Major::Halt,
        }
    }

    /// The frame slots this uop reads (slot operands only; see the module
    /// docs for CALL/CALLT/RET layout-relative effects).
    #[must_use]
    pub fn reads(&self) -> SlotList {
        match self {
            Kimm { .. }
            | Kconst { .. }
            | Kstr { .. }
            | TagNew { .. }
            | Jmp { .. }
            | Funcf { .. }
            | Call { .. }
            | Callg { .. }
            | Callt { .. }
            | Halt { .. } => SlotList::none(),
            Jmpif { cond, .. } | JmpifNot { cond, .. } => SlotList::new(*cond, &[]),
            Calls { callee_slot, .. } => SlotList::new(*callee_slot, &[]),
            // the capture span reads overlay positions 0..n, a
            // layout-relative effect documented in the module docs
            ClosureNew { func, .. } => SlotList::new(*func, &[]),
            CvtI2F { src, .. }
            | CvtF2I { src, .. }
            | Mov { src, .. }
            | TagTest { src, .. }
            | TagTestNot { src, .. }
            | Ret { src }
            | RetSpan { src, .. } => SlotList::new(*src, &[]),
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
            | StrCat { lhs, rhs, .. } => SlotList::new(*lhs, &[*rhs]),
        }
    }

    /// The frame slots this uop writes (slot operands only; RET's overlay
    /// write belongs to the popped frame, see the module docs).
    #[must_use]
    pub fn writes(&self) -> SlotList {
        match self {
            Kimm { dst, .. }
            | Kconst { dst, .. }
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
            | Kstr { dst, .. }
            | TagTest { dst, .. }
            | TagTestNot { dst, .. }
            | TagNew { dst, .. }
            | CvtI2F { dst, .. }
            | CvtF2I { dst, .. }
            | ClosureNew { dst, .. }
            | Mov { dst, .. } => SlotList::new(*dst, &[]),
            Jmp { .. }
            | Jmpif { .. }
            | JmpifNot { .. }
            | Call { .. }
            | Callg { .. }
            | Calls { .. }
            | Callt { .. }
            | Ret { .. }
            | RetSpan { .. }
            | Funcf { .. }
            | Halt { .. } => SlotList::none(),
        }
    }

    /// Whether the uop only reads its read-set and writes its write-set:
    /// no control transfer and no frame-boundary effect, so reordering,
    /// duplicating, or fusing it inside a dataflow window preserves
    /// semantics (BL-4). Pure uops may still trap — the B7 cold overflow
    /// edge — and traps compose under fusion. Calls, jumps, returns, the
    /// FUNCF entry protocol, and HALT are impure. So are the allocating
    /// ops (KSTR, STRCAT, CLOSURE_NEW): reordering or duplicating an
    /// allocation breaks M4's dup/drop balance, and interning touches
    /// shared heap state.
    #[must_use]
    pub fn is_pure(&self) -> bool {
        !matches!(
            self,
            Jmp { .. }
                | Jmpif { .. }
                | JmpifNot { .. }
                | Call { .. }
                | Callg { .. }
                | Calls { .. }
                | Callt { .. }
                | Ret { .. }
                | RetSpan { .. }
                | Kstr { .. }
                | StrCat { .. }
                | ClosureNew { .. }
                | Funcf { .. }
                | Halt { .. }
        )
    }

    /// The XOR-negated counterpart of a comparison, if this is one:
    /// `opcode ^ 1` is the negation's opcode (BL-1). Conditional jumps are
    /// adjacent too (JMPIF^1 == JMPIF_NOT) but negate is about
    /// value-producing comparisons; flipping branch sense is a one-bit
    /// opcode rewrite performed directly by rewriters.
    #[must_use]
    pub fn negate(&self) -> Option<Self> {
        Some(match self {
            EqI64 { dst, lhs, rhs } => NeqI64 {
                dst: *dst,
                lhs: *lhs,
                rhs: *rhs,
            },
            NeqI64 { dst, lhs, rhs } => EqI64 {
                dst: *dst,
                lhs: *lhs,
                rhs: *rhs,
            },
            LtI64 { dst, lhs, rhs } => GeI64 {
                dst: *dst,
                lhs: *lhs,
                rhs: *rhs,
            },
            GeI64 { dst, lhs, rhs } => LtI64 {
                dst: *dst,
                lhs: *lhs,
                rhs: *rhs,
            },
            LeI64 { dst, lhs, rhs } => GtI64 {
                dst: *dst,
                lhs: *lhs,
                rhs: *rhs,
            },
            GtI64 { dst, lhs, rhs } => LeI64 {
                dst: *dst,
                lhs: *lhs,
                rhs: *rhs,
            },
            EqF64 { dst, lhs, rhs } => NeqF64 {
                dst: *dst,
                lhs: *lhs,
                rhs: *rhs,
            },
            NeqF64 { dst, lhs, rhs } => EqF64 {
                dst: *dst,
                lhs: *lhs,
                rhs: *rhs,
            },
            LtF64 { dst, lhs, rhs } => GeF64 {
                dst: *dst,
                lhs: *lhs,
                rhs: *rhs,
            },
            GeF64 { dst, lhs, rhs } => LtF64 {
                dst: *dst,
                lhs: *lhs,
                rhs: *rhs,
            },
            LeF64 { dst, lhs, rhs } => GtF64 {
                dst: *dst,
                lhs: *lhs,
                rhs: *rhs,
            },
            GtF64 { dst, lhs, rhs } => LeF64 {
                dst: *dst,
                lhs: *lhs,
                rhs: *rhs,
            },
            EqBool { dst, lhs, rhs } => NeqBool {
                dst: *dst,
                lhs: *lhs,
                rhs: *rhs,
            },
            NeqBool { dst, lhs, rhs } => EqBool {
                dst: *dst,
                lhs: *lhs,
                rhs: *rhs,
            },
            TagTest { dst, src, tag } => TagTestNot {
                dst: *dst,
                src: *src,
                tag: *tag,
            },
            TagTestNot { dst, src, tag } => TagTest {
                dst: *dst,
                src: *src,
                tag: *tag,
            },
            EqStr { dst, lhs, rhs } => NeqStr {
                dst: *dst,
                lhs: *lhs,
                rhs: *rhs,
            },
            NeqStr { dst, lhs, rhs } => EqStr {
                dst: *dst,
                lhs: *lhs,
                rhs: *rhs,
            },
            _ => return None,
        })
    }
}
