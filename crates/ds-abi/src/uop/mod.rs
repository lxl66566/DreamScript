//! The BL-5 uop set: the one instruction contract for every tier [BL-1, BL-5].
//!
//! This module defines the micro-operation set once, as a Rust enum. Every
//! tier consumes exactly this set: the tier-1 dispatch loop (`ds-vm`), the
//! copy-and-patch stencil source (one stencil per uop, one-to-one by
//! construction, BL-5), and the bytecode-to-SSA lowering (H1). The tier-1
//! reachable core is complete here by construction: constants, typed
//! arithmetic, typed comparisons, control flow, calls, frame protocol, slot
//! moves, ADT tag words, heap string loads and data ops, and multi-word
//! returns. There are no dynamic "V" variants — every operand's type is a
//! compile-time fact (BL-1).
//!
//! # Encoding: 32-bit fixed format (BL-1)
//!
//! ```text
//!  31         24 23         16 15                              0
//! +-------------+-------------+---------------------------------+
//! | opcode byte |      a      |                bc               |
//! | major | min |   u8 slot   |  b (u8) | c (u8)  or  d (u16)   |
//! +-------------+-------------+---------------------------------+
//! ```
//!
//! - The opcode byte \[31:24] is a major/minor pair: major \[31:28] selects the operation class
//!   (operand shape), minor \[27:24] the variant within it. [`Major`] lists the assigned majors; 14
//!   of 16 are used. Major 0xE is the M4 RC pair RCINC/RCDEC, carried by [`rc::RcOp`] beside `Uop`
//!   ([`Inst`] is the one-body-word union both majors decode into); 0xF stays reserved.
//! - `a` \[23:16] is the primary slot operand: destination for data ops, the condition for
//!   conditional jumps, the argument count for calls.
//! - `bc` \[15:0] is either two slot operands `b` \[15:8] / `c` \[7:0] (the three-register data ops
//!   fill `a`, `b`, `c`) or one `d` \[15:0] field: a constant-pool index ([`ConstIdx`]), a biased
//!   jump offset, or the frame slot count (FUNCF).
//! - Every unused field bit must be zero ([`DecodeError::ReservedField`] on violation), so `decode`
//!   is canonical: an encoding decodes to exactly one uop, and that uop re-encodes to the same
//!   word.
//!
//! Field map by class:
//!
//! | class (major)        | variants                | a        | b          | c          | d
//! |----------------------|-------------------------|----------|------------|------------|----------
//! | Const (0x0)          | KIMM, KCONST, KSTR     | dst slot | —          | —          | const idx
//! | IntArith (0x1)       | ADDI64 .. MODI64        | dst slot | lhs slot   | rhs slot   | —
//! | FloatArith (0x2)     | ADDF64 .. DIVF64        | dst slot | lhs slot   | rhs slot   | —
//! | IntCmp (0x3)         | EQI64 .. GTI64          | dst slot | lhs slot   | rhs slot   | —
//! | FloatCmp (0x4)       | EQF64 .. GTF64          | dst slot | lhs slot   | rhs slot   | —
//! | BoolCmp (0x5)        | EQBOOL, NEQBOOL         | dst slot | lhs slot   | rhs slot   | —
//! | Ref (0xD)            | EQSTR, NEQSTR, STRCAT   | dst slot | lhs slot   | rhs slot   | —
//! | Tag (0x6)            | TAG_TEST(_NOT), TAG_NEW | dst slot | src slot   | tag id     | —
//! |                      |                         |          | (TEST) or  | (TEST only)|
//! |                      |                         |          | tag (NEW)  |            |
//! | Cvt (0x7)            | CVTI2F, CVTF2I          | dst slot | src slot   | —          | —
//! | Mov (0x8)            | MOV                     | dst slot | src slot   | —          | —
//! | Ctl (0x9)            | JMPIF, JMPIF_NOT, JMP   | cond / — | —          | —          | jump bias
//! | Call (0xA)           | CALL, CALLT, RET,       | nargs /  | —          | span       | const idx
//! |                      | RETSPAN                 | src slot |            | (RETSPAN) |            |
//! | Frame (0xB)          | FUNCF                   | —        | —          | —          | frame slots
//! | Halt (0xC)           | HALT                    | slot     | —          | —          | —
//!
//! # Immediates go through the constant pool (BL-1)
//!
//! The fixed format leaves 16 operand bits after the opcode and one slot
//! operand. An i64 immediate needs 64 bits, an f64 needs 64, and even an
//! i64 truncated to the available 16 bits would split the integer domain in
//! two representations (inline small, pooled large), so no immediate fits
//! inline except by compromising the domain. Per BL-1 the constant pool is
//! the single immediate channel: KIMM loads an i64 pool entry, KCONST loads
//! any other constant (f64, bool, reference, unit), KSTR materializes a pool
//! string as a heap string reference, and CALL/CALLT reference
//! callees as pool entries (a FuncRef: module index + function index for
//! fully-qualified cross-module calls [H6 item 5], the entry hot reload
//! swaps). The uop contract fixes only the index shape: pool indices are u16
//! ([`ConstIdx`]); the pool CONTAINER is a dsmod-format concern, not part of
//! the ISA — the shipped format carries one pool per function (BL-7), and a
//! module-level pool remains representable at the format layer if a future
//! revision chooses it.
//!
//! # Biased jump offsets (BL-1)
//!
//! Conditional and unconditional jumps carry a signed displacement in the
//! unsigned u16 `d` field, biased by [`BIAS_J`] in the LuaJIT `BCBIAS_J`
//! style: `field = offset + 0x8000` with `offset = target - next_pc` in
//! `[-32768, 32767]` (32 KiB each way — per-function code is far shorter).
//! The bias exists so the hot loop needs no sign logic: the branch-free
//! decode is `offset = (field ^ BIAS_J) as i16`, i.e. flipping the top bit
//! of the field *is* the subtraction. A field of exactly [`BIAS_J`] is the
//! zero offset (a jump to the next instruction).
//!
//! # XOR-negated pairs (BL-1)
//!
//! Comparison variants sit at adjacent minors within their major with the
//! negation bit in the minor's lowest bit, so `opcode ^ 1` yields the
//! negated comparison: EQI64^1 == NEQI64, LTI64^1 == GEI64, LEI64^1 ==
//! GTI64, and likewise for EQF64 and EQBOOL; TAG_TEST^1 == TAG_TEST_NOT.
//! [`Uop::negate`] is the semantic counterpart. The same free adjacency
//! holds for the conditional jumps (JMPIF^1 == JMPIF_NOT): a branch-sense
//! flip is a one-bit rewrite with no operand re-encoding, which is the
//! foundation of the compare-and-branch fusion (BL-4).
//!
//! # CALLT operands
//!
//! CALLT carries the callee ([`ConstIdx`]) and the argument count; the slide
//! source span is not an instruction operand because the locked call ABI
//! already fixes it: the staged arguments are the first `nargs` slots of the
//! caller's outgoing-argument area ([`crate::slide_plan`]), so `src` is
//! derived as `caller.arg_area().start`. The record's H2 spelling
//! `CALLT odd, nargs=1, src=r3` names that derived slot (r3 is the arg-area
//! slot of even's frame). RET likewise needs no extra convention: the return
//! word overlays slot 0 (H2).
//!
//! # Budgets and caps
//!
//! Slot operands are u8 by type: an out-of-range slot index is unrepresentable
//! rather than a runtime error, and frames address at most 256 value slots
//! (LuaJIT register parity), the cap [`MAX_FRAME_SLOTS`] enforces at both
//! encode and decode. Pools address at most 65536 constants, tag ids at most
//! 256 per module. FUNCF declares the frame slot count for the VM's entry
//! cross-check against the function's [`crate::FrameLayout`].

mod effects;
mod encode;
pub mod inst;
pub mod rc;

#[cfg(test)]
mod tests;

pub use effects::{Major, SlotList};
pub use encode::{DecodeError, EncodeError};
pub use inst::{Inst, InstDecodeError};
pub use rc::{RC_MAJOR, RcDecodeError, RcOp};

/// The jump-offset bias (BL-1): the additive origin of the u16 jump field,
/// in the LuaJIT `BCBIAS_J` style. Biased fields decode branch-free:
/// `(field ^ BIAS_J) as i16` is the signed offset from the next pc.
pub const BIAS_J: u16 = 0x8000;

/// The frame-slot cap: slot operands are u8, so a frame addressing more than
/// 256 value slots is unrepresentable (LuaJIT register parity). FUNCF counts
/// above it are rejected by encode and decode with a typed error.
pub const MAX_FRAME_SLOTS: u16 = 256;

/// Index into a constant pool. The uop contract fixes the index shape (u16),
/// not the container: the dsmod format carries one pool per function (BL-7).
/// Pool entries are: i64 (KIMM), any other constant word (KCONST: f64, bool,
/// reference, unit), or a FuncRef (CALL/CALLT: module index + function index
/// [H6 item 5]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ConstIdx(pub u16);

/// One micro-operation (BL-5). Slot operands are frame-slot indices (slot 0
/// is the first parameter); jump targets are absolute instruction indices —
/// encoding resolves them to biased offsets. Each variant documents its
/// major.minor opcode.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Uop {
    /// `0x0.0` — `slots[dst] =` the i64 at pool entry `kidx`.
    Kimm { dst: u8, kidx: ConstIdx },
    /// `0x0.1` — `slots[dst] =` the constant at pool entry `kidx` (f64, bool,
    /// reference, or unit).
    Kconst { dst: u8, kidx: ConstIdx },
    /// `0x0.2` — `slots[dst] =` a heap string reference materialized from
    /// the pool `Str` entry `kidx`. Interned: one object per distinct
    /// constant content, so a constant string is one heap object however
    /// often it loads. M3 extension, justified: KCONST is a pure constant
    /// load while string materialization is a heap-interning effect — a
    /// separate opcode keeps KCONST's purity-table entry true.
    Kstr { dst: u8, kidx: ConstIdx },
    /// `0x1.0` — `slots[dst] = slots[lhs] + slots[rhs]`, i64, traps on
    /// overflow (B7).
    AddI64 { dst: u8, lhs: u8, rhs: u8 },
    /// `0x1.1` — `slots[dst] = slots[lhs] - slots[rhs]`, i64, traps on
    /// overflow (B7).
    SubI64 { dst: u8, lhs: u8, rhs: u8 },
    /// `0x1.2` — `slots[dst] = slots[lhs] * slots[rhs]`, i64, traps on
    /// overflow (B7).
    MulI64 { dst: u8, lhs: u8, rhs: u8 },
    /// `0x1.3` — `slots[dst] = slots[lhs] / slots[rhs]`, i64, traps on
    /// zero divide and on `MIN / -1` overflow (B7).
    DivI64 { dst: u8, lhs: u8, rhs: u8 },
    /// `0x1.4` — `slots[dst] = slots[lhs] % slots[rhs]`, i64, traps on zero
    /// divide and on `MIN % -1` overflow (B7).
    ModI64 { dst: u8, lhs: u8, rhs: u8 },
    /// `0x2.0` — `slots[dst] = slots[lhs] + slots[rhs]`, f64.
    AddF64 { dst: u8, lhs: u8, rhs: u8 },
    /// `0x2.1` — `slots[dst] = slots[lhs] - slots[rhs]`, f64.
    SubF64 { dst: u8, lhs: u8, rhs: u8 },
    /// `0x2.2` — `slots[dst] = slots[lhs] * slots[rhs]`, f64.
    MulF64 { dst: u8, lhs: u8, rhs: u8 },
    /// `0x2.3` — `slots[dst] = slots[lhs] / slots[rhs]`, f64.
    DivF64 { dst: u8, lhs: u8, rhs: u8 },
    /// `0x3.0` — `slots[dst] = slots[lhs] == slots[rhs]`, unboxed bool;
    /// `0x3.0 ^ 1 == 0x3.1` (NEQI64).
    EqI64 { dst: u8, lhs: u8, rhs: u8 },
    /// `0x3.1` — `slots[dst] = slots[lhs] != slots[rhs]`.
    NeqI64 { dst: u8, lhs: u8, rhs: u8 },
    /// `0x3.2` — `slots[dst] = slots[lhs] < slots[rhs]`; `^ 1` is GEI64.
    LtI64 { dst: u8, lhs: u8, rhs: u8 },
    /// `0x3.3` — `slots[dst] = slots[lhs] >= slots[rhs]`.
    GeI64 { dst: u8, lhs: u8, rhs: u8 },
    /// `0x3.4` — `slots[dst] = slots[lhs] <= slots[rhs]`; `^ 1` is GTI64.
    LeI64 { dst: u8, lhs: u8, rhs: u8 },
    /// `0x3.5` — `slots[dst] = slots[lhs] > slots[rhs]`.
    GtI64 { dst: u8, lhs: u8, rhs: u8 },
    /// `0x4.0` — `slots[dst] = slots[lhs] == slots[rhs]`, f64 bit-structural
    /// equality (B9); `0x4.0 ^ 1 == 0x4.1` (NEQF64).
    EqF64 { dst: u8, lhs: u8, rhs: u8 },
    /// `0x4.1` — `slots[dst] = slots[lhs] != slots[rhs]`, f64.
    NeqF64 { dst: u8, lhs: u8, rhs: u8 },
    /// `0x4.2` — `slots[dst] = slots[lhs] < slots[rhs]`, f64; `^ 1` is GEF64.
    LtF64 { dst: u8, lhs: u8, rhs: u8 },
    /// `0x4.3` — `slots[dst] = slots[lhs] >= slots[rhs]`, f64.
    GeF64 { dst: u8, lhs: u8, rhs: u8 },
    /// `0x4.4` — `slots[dst] = slots[lhs] <= slots[rhs]`, f64; `^ 1` is GTF64.
    LeF64 { dst: u8, lhs: u8, rhs: u8 },
    /// `0x4.5` — `slots[dst] = slots[lhs] > slots[rhs]`, f64.
    GtF64 { dst: u8, lhs: u8, rhs: u8 },
    /// `0x5.0` — `slots[dst] = slots[lhs] == slots[rhs]`, bool; `0x5.0 ^ 1 ==
    /// 0x5.1` (NEQBOOL).
    EqBool { dst: u8, lhs: u8, rhs: u8 },
    /// `0x5.1` — `slots[dst] = slots[lhs] != slots[rhs]`, bool.
    NeqBool { dst: u8, lhs: u8, rhs: u8 },
    /// `0xD.0` — `slots[dst] =` `slots[lhs] ==` `slots[rhs]`, strings:
    /// structural byte equality of the referenced contents, not object
    /// identity; `0xD.0 ^ 1 == 0xD.1` (NEQSTR). M3 extension, justified:
    /// string equality is content equality, which no existing comparison
    /// expresses (EQI64 compares the reference words).
    EqStr { dst: u8, lhs: u8, rhs: u8 },
    /// `0xD.1` — `slots[dst] =` `slots[lhs] !=` `slots[rhs]`, strings.
    NeqStr { dst: u8, lhs: u8, rhs: u8 },
    /// `0xD.2` — `slots[dst] =` a fresh heap string
    /// `concat(slots[lhs], slots[rhs])`. Allocating, so impure for the
    /// same reason as KSTR: reordering or duplicating an allocation breaks
    /// M4's dup/drop placement. M3 extension, justified: concatenation is
    /// the primitive interpolation lowers onto (F6), and no existing uop
    /// builds a new string.
    StrCat { dst: u8, lhs: u8, rhs: u8 },
    /// `0x6.0` — `slots[dst] = (slots[src] == tag)`, unboxed bool: reads the
    /// tag word of a tag+payload union (BL-2) and compares its discriminant
    /// against immediate tag id `tag` (in the `c` field). `0x6.0 ^ 1 ==
    /// 0x6.1`.
    TagTest { dst: u8, src: u8, tag: u8 },
    /// `0x6.1` — `slots[dst] = (slots[src] != tag)`: the negated tag test.
    TagTestNot { dst: u8, src: u8, tag: u8 },
    /// `0x6.2` — `slots[dst] = tag`: writes the discriminant word of a
    /// tag+payload union (BL-2). Payload words are the immediately following
    /// slots, filled by ordinary slot ops; fusing construction into a
    /// payloaded TAG_NEW is a BL-4 superinstruction decision from profile
    /// data, not a new uop.
    TagNew { dst: u8, tag: u8 },
    /// `0x7.0` — `slots[dst] = slots[src] as f64`, i64 to f64.
    CvtI2F { dst: u8, src: u8 },
    /// `0x7.1` — `slots[dst] = slots[src] as i64`, f64 to i64, traps when
    /// the value is out of i64 range [B7 explicit conversions].
    CvtF2I { dst: u8, src: u8 },
    /// `0x8.0` — `slots[dst] = slots[src]`, one raw word.
    Mov { dst: u8, src: u8 },
    /// `0x9.2` — jump to `target` (biased offset in the `d` field).
    Jmp { target: u32 },
    /// `0x9.0` — jump to `target` when `slots[cond]` is `true`;
    /// `0x9.0 ^ 1 == 0x9.1`.
    Jmpif { cond: u8, target: u32 },
    /// `0x9.1` — jump to `target` when `slots[cond]` is `false`. Polarity
    /// is literal: exiting a `while cond` loop takes JMPIF on the negated
    /// condition (jump when done), not JMPIF_NOT on `cond` (which would
    /// jump straight out whenever `cond` holds).
    JmpifNot { cond: u8, target: u32 },
    /// `0xA.0` — call pool FuncRef `callee` with `nargs` arguments staged in
    /// the caller's outgoing-argument area; the callee frame overlays that
    /// area [BL-6, H2]. The VM re-checks `nargs` against the callee's
    /// layout.
    Call { callee: ConstIdx, nargs: u8 },
    /// `0xA.1` — tail call (H2): slide the staged arguments into slots
    /// `0..nargs` per [`crate::slide_plan`], rewrite the metadata word in
    /// place, and enter the callee. The stack never grows at any depth. The
    /// VM re-checks `nargs` against the callee's layout. The slide source is
    /// derived from the caller layout (see module docs).
    Callt { callee: ConstIdx, nargs: u8 },
    /// `0xA.2` — return `slots[src]`; the word overlays slot 0 of the
    /// caller's outgoing-argument area (H2).
    Ret { src: u8 },
    /// `0xA.3` — return the `n` consecutive slots starting at `src`: the
    /// span is placed at overlay words `0..n` (the caller's
    /// outgoing-argument slots, which the callee's own slots already alias
    /// below its call word), then the frame returns exactly like RET with
    /// the span's head as the single-word result. The caller's
    /// outgoing-argument area must be at least `n` wide (the VM checks;
    /// the lowering sizes it). M3 extension, justified: RET moves one word
    /// while BL-2 values (payloaded unions, structs) span consecutive
    /// slots, and moves cannot carry a span across the popped frame
    /// boundary — the callee's words above the caller's frame are
    /// truncated at the pop.
    RetSpan { src: u8, n: u8 },
    /// `0xB.0` — function entry: declares `frame_slots` value slots, at most
    /// [`MAX_FRAME_SLOTS`] (encode and decode reject larger counts with a
    /// typed error); the VM cross-checks it against the function's
    /// [`crate::FrameLayout`] slot count. FUNCF belongs to this set because
    /// it is part of the tier-1 entry protocol every tier must recognize
    /// (tier 2 entry stubs keep the check); it is not a data op.
    Funcf { frame_slots: u16 },
    /// `0xC.0` — stop the VM and report `slots[slot]` as the run result.
    Halt { slot: u8 },
}

/// The biased u16 field for a jump from pc to `target` (BL-1):
/// `field = (target - next_pc) + BIAS_J`. Errors when the displacement
/// exceeds the signed 16-bit window.
// the bias trick is a guarded bit-level reinterpretation: the range check
// above makes the narrowing exact
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
#[must_use = "the bias arithmetic is the contract; use or test the field"]
pub fn jump_field(target: u32, pc: u32) -> Result<u16, EncodeError> {
    let offset = i64::from(target) - (i64::from(pc) + 1);
    if !(-0x8000..=0x7fff).contains(&offset) {
        return Err(EncodeError::JumpOutOfRange { pc, target });
    }
    Ok((i64::from(BIAS_J) + offset) as u16)
}

/// The jump target of a biased field at pc (BL-1):
/// `target = pc + 1 + (field ^ BIAS_J) as i16`. Errors when the biased
/// offset cannot be a valid pc (below zero or past u32 range; only
/// reachable for a corrupted stream, since encode validates).
// the bias trick is a bit-level reinterpretation by design: the XOR flips
// the top bit, and the i16 read is the signed offset
#[allow(clippy::cast_possible_wrap)]
#[must_use = "the bias arithmetic is the contract; use or test the target"]
pub fn jump_target(field: u16, pc: u32) -> Result<u32, DecodeError> {
    let target = i64::from(pc) + 1 + i64::from((field ^ BIAS_J) as i16);
    u32::try_from(target).map_err(|_| DecodeError::JumpOutOfRange { pc, field })
}
