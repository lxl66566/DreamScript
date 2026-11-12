//! Encoded programs: the constant pool [BL-1, BL-7] and assembled bodies.

use ds_abi::{EncodeError, Inst};

/// What kind of entry a pool index holds; [`Const::kind`] is its witness.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConstKind {
    /// An i64 loaded by KIMM.
    I64,
    /// An f64 loaded by KCONST.
    F64,
    /// A bool loaded by KCONST.
    Bool,
    /// A function reference loaded by CALL/CALLT (a callee) and by KCONST
    /// (the scalar fn word a callable binding holds, F2).
    FuncRef,
    /// A string constant loaded by KSTR: the VM materializes it as an
    /// interned heap string reference.
    Str,
}

/// One constant-pool entry [BL-1]: the single immediate channel. KIMM loads
/// the i64 entries, KCONST the f64/bool/fn-word entries (a FuncRef entry
/// materializes as the scalar callable word, F2), and CALL/CALLT load
/// [`Const::FuncRef`] callees [H6 item 5].
#[derive(Clone, Debug, PartialEq)]
pub enum Const {
    /// An i64 immediate.
    I64(i64),
    /// An f64 immediate.
    F64(f64),
    /// A bool immediate.
    Bool(bool),
    /// A fully-qualified callee: the target module plus the function's index
    /// in that module's function table. This is the skeleton-registry form
    /// of the contract's "module index + function index" FuncRef: modules
    /// are name-keyed until the M7 loading round's link table assigns
    /// module indices [BL-7], and the function index is the entry-descriptor word-0
    /// convention. Resolution goes through the target module's current slot,
    /// so a hot-reload swap redirects subsequent calls [H6 item 5].
    FuncRef {
        /// The callee module's name.
        module: String,
        /// The callee's function-table index.
        func: u32,
    },
    /// A string constant riding the pool; KSTR loads it as an interned
    /// heap string reference.
    Str(String),
}

impl Const {
    /// The entry's kind, for pool-mismatch traps.
    #[must_use]
    pub fn kind(&self) -> ConstKind {
        match self {
            Self::I64(_) => ConstKind::I64,
            Self::F64(_) => ConstKind::F64,
            Self::Bool(_) => ConstKind::Bool,
            Self::FuncRef { .. } => ConstKind::FuncRef,
            Self::Str(_) => ConstKind::Str,
        }
    }
}

/// A function as handed to the module loader: everything except the entry
/// descriptor, whose word 0 the loader writes once from the function's table
/// index at load time (word 0 is immutable after load [H2]). The body is the
/// canonical 32-bit word stream [BL-1] — see [`assemble`] — and the pool is
/// the function's own, matching the dsmod blocks payload (one pool per
/// function; uop `ConstIdx` operands index it).
#[derive(Clone, Debug)]
pub struct FunctionDef {
    /// The function's name within its module.
    pub name: String,
    /// The function's static frame layout [BL-6].
    pub layout: ds_abi::FrameLayout,
    /// The function's constant pool.
    pub pool: Vec<Const>,
    /// The encoded body: one u32 per uop, per the locked bit layout.
    pub code: Vec<u32>,
}

/// Assembles an instruction body (uops and the 0xE RC pair alike) into its
/// canonical 32-bit word stream [BL-1], resolving jump targets against each
/// instruction's pc. The inverse — [`Inst::decode`] — runs in the dispatcher
/// and at module load.
///
/// # Errors
///
/// Propagates [`EncodeError`] when a jump displacement exceeds the biased
/// u16 window (the only operand an assembler can get wrong; all others are
/// typed fields).
pub fn assemble<I: Into<Inst> + Copy>(body: &[I]) -> Result<Vec<u32>, EncodeError> {
    body.iter()
        .enumerate()
        .map(|(pc, inst)| Into::<Inst>::into(*inst).encode(u32::try_from(pc).expect("pc fits u32")))
        .collect()
}
