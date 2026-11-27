//! Per-function lowering: slots, the symbolic uop buffer, and the
//! expression walk.
//!
//! The walk consumes the checked tables: every expression's solved type
//! (flow-refined inside branches) comes from `expr_types`, every
//! binder's declared type from `binders`, and every name use from the
//! file's resolutions. Tail positions lower to RET/RETSPAN/CALLT
//! directly; every other position lowers to a place — a span of body
//! slots — that callers read or widen into their own spans.

use std::{
    borrow::Cow,
    collections::{HashMap, HashSet},
    rc::Rc,
};

use ds_abi::{
    BoundRet, BuiltinInstance, ConstIdx, RcOp, SlotType, Staging, Uop, uop::MAX_FRAME_SLOTS,
};
use ds_check::{
    Analyzed, AtomKind, BinderFact, ConstFact, DefId, DotRes, Extraction, GenericCallBounds,
    InherentCallBounds, InstanceId, MethodId, MethodOwner, PlaceStep, PlaceStore, Res, Resolutions,
    StoreIndex, TraitCall, TraitId, Ty, TypeExpr, TypeId, Val,
};
use ds_diag::{FileId, Span};
use ds_mod::{Constant, FuncRefKey};
use ds_parse::{
    BinaryOp, BlockExpr, CallArg, EnumPayload, Expr, Ident, Literal, LocalDecl, MatchArm, Param,
    Path, Pattern, Stmt, StmtKind, StrPart, Type, UnaryOp,
};

use crate::{
    PlacedOp,
    error::LowerError,
    native::{
        GENERIC_NATIVES, GenericNative, NURSERY_BOUNDARY_ERR, Native, NativeShape, PLATFORM_IO_OK,
    },
    place::{AffineFacts, Hook, Owned, Placement, State},
    shape::{
        MemberField, MemberShape, Shape, ShapeKey, Shapes, SpanWords, StructShape, UnionShape,
        expr_members, shape_words,
    },
    types::Subst,
};

mod block;
mod callable;
mod calls;
mod data;
mod emit;
mod extraction;
mod facts;
mod finalize;
mod instance;
mod r#loop;
mod member;
mod op;
mod ownership;
mod params;
mod path;
mod pattern;
mod place;
mod plan;
mod sort;
mod store;
mod tail;
mod text;
mod value;

use emit::*;
use extraction::AmbientExtern;
use facts::*;
pub(crate) use instance::GenericFn;
use instance::{Instance, InstanceKey};
pub(crate) use member::GenericMember;
use op::*;
use path::*;
use place::*;
use plan::*;
pub(crate) use plan::{
    BodyId, Demands, Discovery, EvidenceEnv, Plan, discover_item, runtime_pairs,
};
use value::*;

/// One symbolic frame slot: a body word, or an outgoing-overlay word that
/// resolves against the frame tail at finalization.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Slot {
    index: u8,
    overlay: bool,
}

impl Slot {
    pub(crate) const fn body(index: u8) -> Self {
        Self {
            index,
            overlay: false,
        }
    }

    pub(crate) const fn overlay(index: u8) -> Self {
        Self {
            index,
            overlay: true,
        }
    }
}

/// One-word data operations the buffer carries symbolically.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum DataOp {
    AddI64,
    SubI64,
    MulI64,
    DivI64,
    ModI64,
    WrapAddI64,
    WrapSubI64,
    WrapMulI64,
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
    LtI64,
    GeI64,
    LeI64,
    GtI64,
    EqF64,
    LtF64,
    GeF64,
    LeF64,
    GtF64,
    EqBool,
    NeqBool,
    EqStr,
    StrCat,
    StrCatArena,
}

impl DataOp {
    /// The uop this buffer op materializes as.
    fn uop(self, dst: u8, lhs: u8, rhs: u8) -> Uop {
        match self {
            Self::AddI64 => Uop::AddI64 { dst, lhs, rhs },
            Self::SubI64 => Uop::SubI64 { dst, lhs, rhs },
            Self::MulI64 => Uop::MulI64 { dst, lhs, rhs },
            Self::DivI64 => Uop::DivI64 { dst, lhs, rhs },
            Self::ModI64 => Uop::ModI64 { dst, lhs, rhs },
            Self::WrapAddI64 => Uop::WrapAddI64 { dst, lhs, rhs },
            Self::WrapSubI64 => Uop::WrapSubI64 { dst, lhs, rhs },
            Self::WrapMulI64 => Uop::WrapMulI64 { dst, lhs, rhs },
            Self::ShlI64 => Uop::ShlI64 { dst, lhs, rhs },
            Self::ShrI64 => Uop::ShrI64 { dst, lhs, rhs },
            Self::BitAndI64 => Uop::BitAndI64 { dst, lhs, rhs },
            Self::BitXorI64 => Uop::BitXorI64 { dst, lhs, rhs },
            Self::BitOrI64 => Uop::BitOrI64 { dst, lhs, rhs },
            Self::AddF64 => Uop::AddF64 { dst, lhs, rhs },
            Self::SubF64 => Uop::SubF64 { dst, lhs, rhs },
            Self::MulF64 => Uop::MulF64 { dst, lhs, rhs },
            Self::DivF64 => Uop::DivF64 { dst, lhs, rhs },
            Self::EqI64 => Uop::EqI64 { dst, lhs, rhs },
            Self::LtI64 => Uop::LtI64 { dst, lhs, rhs },
            Self::GeI64 => Uop::GeI64 { dst, lhs, rhs },
            Self::LeI64 => Uop::LeI64 { dst, lhs, rhs },
            Self::GtI64 => Uop::GtI64 { dst, lhs, rhs },
            Self::EqF64 => Uop::EqF64 { dst, lhs, rhs },
            Self::LtF64 => Uop::LtF64 { dst, lhs, rhs },
            Self::GeF64 => Uop::GeF64 { dst, lhs, rhs },
            Self::LeF64 => Uop::LeF64 { dst, lhs, rhs },
            Self::GtF64 => Uop::GtF64 { dst, lhs, rhs },
            Self::EqBool => Uop::EqBool { dst, lhs, rhs },
            Self::NeqBool => Uop::NeqBool { dst, lhs, rhs },
            Self::EqStr => Uop::EqStr { dst, lhs, rhs },
            Self::StrCat => Uop::StrCat { dst, lhs, rhs },
            Self::StrCatArena => Uop::StrCatArena { dst, lhs, rhs },
        }
    }

    /// The result word type of this op.
    fn result_slot(self, lhs: SlotType) -> SlotType {
        match self {
            Self::StrCat | Self::StrCatArena => SlotType::Ref,
            Self::AddI64
            | Self::SubI64
            | Self::MulI64
            | Self::DivI64
            | Self::ModI64
            | Self::WrapAddI64
            | Self::WrapSubI64
            | Self::WrapMulI64
            | Self::ShlI64
            | Self::ShrI64
            | Self::BitAndI64
            | Self::BitXorI64
            | Self::BitOrI64
            | Self::AddF64
            | Self::SubF64
            | Self::MulF64
            | Self::DivF64 => lhs,
            _ => SlotType::Bool,
        }
    }
}

/// One buffered instruction: slots stay symbolic (overlay words resolve
/// against the frame tail) and jumps carry label ids until finalization.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Op {
    Kimm {
        dst: Slot,
        kidx: u16,
    },
    Kconst {
        dst: Slot,
        kidx: u16,
    },
    Kstr {
        dst: Slot,
        kidx: u16,
    },
    /// The claim-free KSTR form (stage three's temporary slice): the
    /// word borrows the intern map's canonical copy, so the load claims
    /// no count and the frame's terminal dec is deleted. The placement
    /// pass emits this only for slots it proved count-free.
    KstrBorrow {
        dst: Slot,
        kidx: u16,
    },
    Data {
        op: DataOp,
        dst: Slot,
        lhs: Slot,
        rhs: Slot,
    },
    TagTest {
        dst: Slot,
        src: Slot,
        tag: u8,
    },
    TagNew {
        dst: Slot,
        tag: u8,
    },
    Mov {
        dst: Slot,
        src: Slot,
    },
    /// `rc_inc` on the reference in a body word (the placed dup).
    RcInc {
        src: Slot,
    },
    /// `rc_dec` (the placed drop); a unique death runs the deinit hook
    /// and pools the block for reuse inside the `ds-vm/src/object.rs` store.
    RcDec {
        src: Slot,
    },
    Jmp {
        target: usize,
    },
    /// The loop-head mark: packs the arena cursor into a body word the
    /// paired restore reads at the back edge.
    ArenaMark {
        dst: Slot,
    },
    /// The back-edge restore: rewinds the arena to the packed cursor.
    ArenaRestore {
        src: Slot,
    },
    Jmpif {
        cond: Slot,
        target: usize,
    },
    JmpifNot {
        cond: Slot,
        target: usize,
    },
    Call {
        callee: u16,
        nargs: u8,
    },
    Callt {
        callee: u16,
        nargs: u8,
    },
    /// A generic call site's dispatch: the callee is the generic's
    /// ONE body and the staged words end in the evidence words; `nargs`
    /// names the real arguments only.
    Callg {
        callee: u16,
        nargs: u8,
    },
    /// The dispatch tail call (CALLTG, the CALLG tail form): the same
    /// dispatch resolution and staged shape as `Callg`, transferring
    /// through the CALLT frame-reuse slide.
    Calltg {
        callee: u16,
        nargs: u8,
    },
    /// CALLS (the callable-word call form): the callee operand is a
    /// frame slot holding the fn word; `nargs` arguments are staged on
    /// the overlay like every call form.
    Calls {
        callee_slot: u8,
        nargs: u8,
    },
    /// CLOSURE_NEW: the header is the Fn word at `func`, the capture
    /// block the `n` words staged on the overlay; the fresh closure Ref
    /// lands in `dst`.
    ClosureNew {
        dst: u8,
        func: u8,
        n: u8,
    },
    /// CAPTURE_STORE: `src` is the incoming word, `obj` the
    /// closure word CALLS wrote into this frame, `idx` the capture
    /// block's word offset — the mut window over the closure's own
    /// uniquely-held capture slot. The op owns the count traffic
    /// (one edge acquire for the incoming Ref, one release for the
    /// replaced word), so the frame's own copy stays untouched.
    CaptureStore {
        src: u8,
        obj: u8,
        idx: u8,
    },
    Ret {
        src: Slot,
    },
    RetSpan {
        src: Slot,
        n: u8,
    },
    Halt {
        slot: Slot,
    },
    /// The `?` error leg's propagation mark (ERR_MARK, 0xB.1): no
    /// operands, no slot effects. The VM records the frame's position
    /// into the running fiber's ring under its error-trace switch, and
    /// falls through otherwise.
    ErrMark,
}

/// How the words of a place are shaped: a solved type's span, or a single
/// bare word (scalars and extracted payload words).
#[derive(Clone, Debug)]
enum PlaceShape {
    /// The span of this solved type.
    Span(Ty),
    /// One bare word of the given type.
    Word(SlotType),
}

/// Where one value lives: the base slot of its words plus their shape. A
/// use's static type (possibly refined) rides separately, from
/// `expr_types`.
#[derive(Clone, Debug)]
struct Place {
    base: u8,
    shape: PlaceShape,
}

/// One enclosing loop's exit targets: `break` jumps to `exit` (the
/// label past the backedge, where the loop's carried cleanups run),
/// `continue` jumps to `cont` (the backedge start, before the
/// induction step), and `body_depth` is the name-scope count below the
/// loop's own body scope — a jump leaving the loop must account for
/// every binder of the body scope inwards.
#[derive(Clone, Copy)]
struct LoopCtx {
    exit: usize,
    cont: usize,
    body_depth: usize,
}

/// One loop's stream brackets: the head position (an ARENA_MARK inserts
/// there, the label stays pointing at it) and the back-edge jump's
/// position (an ARENA_RESTORE inserts before it).
#[derive(Clone, Copy)]
struct LoopSpan {
    head: usize,
    back: usize,
}

/// A walked expression's result: its place plus the use's static type.
#[derive(Clone)]
struct TypedPlace {
    place: Place,
    ty: Ty,
}

/// One word of a value inside a place.
/// How one arm's pattern tests a non-union scrutinee.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ValueArmKind {
    /// No runtime test: a plain binding, `_`, an atom naming the
    /// scrutinee's sole member, or a destructuring tuple without
    /// literal elements.
    Catchall,
    /// Literal words tested in place.
    Tested,
}

#[derive(Clone, Debug)]
struct WordAddr {
    /// The field name where one exists; `None` for scalars.
    name: Option<String>,
    /// The absolute body slot.
    slot: u8,
    /// The word type.
    slot_ty: SlotType,
}

/// The word addresses of one field's whole span at `base`: one address
/// per word, the field's name on its base word alone so grouped lists
/// stay name-addressable.
fn field_addrs(base: u8, field: &MemberField) -> Vec<WordAddr> {
    field
        .slots
        .iter()
        .enumerate()
        .map(|(offset, slot_ty)| WordAddr {
            name: (offset == 0).then(|| field.name.clone()).flatten(),
            slot: base + field.word + u8::try_from(offset).expect("field span offset fits u8"),
            slot_ty: *slot_ty,
        })
        .collect()
}

/// One value's declared deinit claim: what the drop site stages.
struct DeinitClaim {
    /// The synthesized deinit's pool constant, staged width, and `Ref`
    /// word offsets (the decs fire per span word).
    hook: Hook,
    /// The value's word types, in span order — the hook's staging.
    staged: SpanWords,
}

/// Which run one lowering drives: the runtime module embeds the
/// evaluator's answers at comptime call sites and skips the comptime
/// functions themselves, while the comptime slice lowers exactly those
/// functions as ordinary locals (their nested comptime calls are plain
/// intra-module calls there).
/// The per-run context one lowering carries: the platform name the
/// world natives address and which comptime run this body lowers for.
pub(crate) struct RunContext<'a> {
    /// The platform name for world-call pool FuncRefs.
    pub platform: String,
    /// The comptime run mode.
    pub comptime: ComptimeRun<'a>,
    /// The lowering mode: the REPL's resumed-frame session facts ride
    /// it.
    pub mode: crate::LowerMode,
}

/// Which run one lowering drives: the runtime module embeds the
/// evaluator's answers at comptime call sites and skips the comptime
/// functions themselves, while the comptime slice lowers exactly those
/// functions as ordinary locals (their nested comptime calls are plain
/// intra-module calls there).
#[derive(Clone, Copy)]
pub(crate) enum ComptimeRun<'a> {
    /// The runtime module: the evaluator's recorded answers, keyed by
    /// each call's span.
    Runtime(&'a crate::ComptimeValues),
    /// The comptime slice the evaluator compiles for its fenced run.
    Slice,
}

/// A call target: a module-local function or a contract builtin.
#[derive(Clone, Copy)]
enum Callee {
    /// A native in one of the reserved native surfaces (the `builtin`
    /// core or the platform handler — the native's own routing says
    /// which).
    Builtin(&'static Native),
    /// A generic builtin's ONE body: the call routes through CALLG with
    /// the instantiated-type evidence words staged after the real
    /// arguments.
    Generic(&'static GenericNative),
    /// This module's function-table index.
    Local(u32),
    /// A linked package's function: the def carries the import
    /// surface entry — its checked fn type serves the parameter reads
    /// exactly like a planned def's — and the FuncRef addresses the
    /// linked dep position and the linked module's function index. The
    /// call stages like a local's and a tail site takes the same CALLT
    /// frame-reuse transfer: the module boundary rides the pool
    /// constant alone, the slide a pure fact of the two layouts). The
    /// def sits absent for a linked trait impl's member body — a method
    /// owns no package def, so the member's own declared signature
    /// serves the mode reads the Evidence arm mirrors.
    Foreign {
        /// The import-bound def: the signature's checker facts; `None`
        /// for a linked impl member (no def names it).
        def: Option<DefId>,
        /// The linked dep entry's position in the recorded dep list.
        position: u32,
        /// The function's index inside the linked module's table.
        func: u32,
    },
    /// A runtime-resolved trait dispatch inside a generic impl body: the
    /// frame's evidence slot whose word names the serving member body in
    /// the enclosing module (the sentinel FuncRef the CALLG carries).
    Evidence(u8),
    /// A bound extern's C symbol: the def carries the declaration's
    /// bind target and written signature, the index the symbol's
    /// position in the artifact's enumeration-ordered table — the same
    /// table the run installs as the artifact's native module under its
    /// manifest name. The call stages and borrows exactly like a native
    /// callee's (the staged words are the scalar signature's own).
    Bound {
        /// The extern's def: the bind target and signature facts.
        def: DefId,
        /// The symbol's index in the artifact's export enumeration.
        index: u32,
    },
}

/// One function's lowering state.
pub(crate) struct FnLower<'a, 'p, 's> {
    analyzed: &'a Analyzed,
    res: &'a Resolutions,
    plan: &'p mut Plan,
    shapes: &'s mut Shapes<'a>,
    name: String,
    /// The platform name the world natives' pool FuncRefs address: the
    /// package's `platform "..."` declaration's name, the default
    /// platform `std` when no declaration names one.
    platform: String,
    /// Whether this function is a synthesized lambda value's body: a use
    /// of a binder the walk never bound is a capture there.
    synth: bool,
    /// Whether this frame is the program's root: the run entry (`main`).
    /// Only a root frame may tail-transfer to a native with its staged
    /// borrows abandoned — the program ends with that call, so the heap
    /// dies whole; every other frame returns into a live program and
    /// flushes the counts it owes before the exit.
    root: bool,
    /// Which run this body lowers for: the runtime module or the
    /// evaluator's comptime slice.
    comptime: ComptimeRun<'a>,
    ret_ty: Ty,
    params_len: usize,
    slots: Vec<SlotType>,
    code: Vec<Op>,
    labels: Vec<usize>,
    pool: Vec<Constant>,
    /// Position of every non-string pool entry, keyed by value.
    pool_index: HashMap<Constant, u16>,
    /// Position of every string pool entry, keyed by contents: a hit on a
    /// repeated literal allocates nothing.
    pool_strings: HashMap<String, u16>,
    /// Binder span -> place.
    binders: HashMap<Span, Place>,
    /// Lambda-value words -> their synthesized function's table index:
    /// the frame's provenance facts for fn-typed call results (a factory's
    /// result is the closure its body constructs). Slots bump-allocate
    /// and a lambda value's word is written once, so the key is stable.
    fn_words: HashMap<u8, u32>,
    /// Body words holding provably-scalar FuncRefs (a named fn value or a
    /// non-capturing lambda value): the callable-provenance oracle. A
    /// fn-typed place outside this set may hold a closure's Ref word at
    /// runtime (a callable-defaulted parameter or an alias of one), so
    /// the re-storable frame positions and the fn-typed answers keep
    /// proving scalar before they trust a word.
    scalar_fn_words: HashSet<u8>,
    /// A fn index to the wrapped closure word standing for it: a
    /// non-capturing fn entering a Ref-classified heap position wraps
    /// into its static closure object once per live placement region,
    /// every entry in the region re-reading the same word.
    wrap_memo: HashMap<u32, u8>,
    /// Name scopes, innermost last: name -> binder span.
    scopes: Vec<HashMap<String, Span>>,
    /// Overlay word usage: index -> word types seen there.
    overlay: Vec<HashSet<SlotType>>,
    overlay_len: usize,
    /// The scalar destination offer live for one expression evaluation.
    scalar_dst: Option<ScalarDst>,
    /// The Perceus dup/drop placement over this function's walk.
    rc: Placement,
    /// This body's runtime-evidence pairs to their frame slots: a nested
    /// trait-method call on a bound type variable dispatches through the
    /// word at its slot (empty outside generic bodies and their
    /// lambdas).
    evidence: HashMap<(String, MethodId), u8>,
    /// This function's table index: a dead Fn payload word zero-writes
    /// the frame's own scalar FuncRef (a copyable word the dead member
    /// never calls).
    index: u32,
    /// The enclosing loops, innermost last.
    loops: Vec<LoopCtx>,
    /// Every lowered loop's head and back-edge stream positions: the
    /// slots finalize pairs an ARENA_MARK/ARENA_RESTORE into for the loops
    /// enclosing designated arena temporaries. Positions shift with the
    /// epilogue splices, like labels.
    loop_spans: Vec<LoopSpan>,
    /// The innermost open nurseries' id words, innermost last: the
    /// `spawn` sites of one `nursery` block stage the top word.
    nurseries: Vec<u8>,
    /// The closure word of this lambda frame's own running closure (the
    /// Ref word CALLS writes right past the capture block): the mut
    /// window's `obj` operand. `None` for every declared, deinit, and
    /// spawned-child frame (a spawned child keeps its own copy of the
    /// captured words; its captures never mutated the original).
    capture_word: Option<u8>,
    /// A mutable captured binding's base offset in the closure's
    /// capture block: the CAPTURE_STORE `idx` operand. Entries exist only
    /// in a lambda frame whose block carries at least one capture (a
    /// non-capturing lambda runs as a scalar FuncRef, never a frame with
    /// a closure word).
    capture_index: HashMap<Span, usize>,
    /// Whether this frame is a spawned child: the child's own
    /// return is its answer type, not the enclosing function's union,
    /// so the cancel-containment proof belongs to the parent's spawn
    /// site instead of this frame.
    spawn_child: bool,
    /// The active `for comptime` copy path while a copy's body lowers,
    /// outer-to-inner copy indices; empty outside one. Every checked
    /// record read routes through it: inside a copy the checker's facts
    /// for the body live in the per-copy table.
    comptime_path: Vec<u32>,
    /// The per-instance `for comptime` expansion this body lowers
    /// under: the walked fn's definition and solved arguments. The
    /// instance tables' key; `None` on every other body.
    comptime_instance: Option<Rc<ds_check::type_of::InstanceScope>>,
    /// The enclosing `for comptime` loops' (loop span, record binder
    /// span) pairs, innermost last; the copy's field name reads from the
    /// loop's projected fields through the path depth.
    comptime_stack: Vec<(Span, Span)>,
    /// Whether this frame is a `spawn` expression's child: the
    /// parent's spawn site owns the `cancel`-payload containment checks
    /// against the nursery-owning function's return union, so the child's
    /// own lowering skips them.
    /// The frame's canonical unit word: allocated at its first use, its
    /// single `Kimm 0` spliced at the frame top in `finalize`. Every unit
    /// value in the frame is this one place.
    unit_slot: Option<u8>,
    /// `main`'s per-statement value ends (the resumed-frame seam): the
    /// code length after each direct body statement's value code, before
    /// its discards, plus the tail's exit boundary — recorded only for
    /// `main`, rebased through `finalize`'s movements.
    value_ends: Vec<u32>,
    /// The source span the ops currently emit under: `value` sets it for
    /// its expression's walk and restores the enclosing one after, so
    /// every op attributes to the innermost expression lowering it.
    cur_span: Span,
    /// The span the last recorded mark carries: a mark lands only when
    /// the span changes, so consecutive ops under one expression share
    /// one region.
    last_span: Option<Span>,
    /// The region starts, in stream order: stream position plus the span
    /// the ops from there run under. Positions shift with every splice
    /// like the labels, and `finalize` rebases them onto final word pcs.
    span_marks: Vec<(usize, Span)>,
}

impl<'a, 'p, 's> FnLower<'a, 'p, 's> {
    /// Lowers one planned function to a validated ds-mod function record
    /// plus its placed (RC-bearing) op sequence.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn lower(
        analyzed: &'a Analyzed,
        res: &'a Resolutions,
        plan: &'p mut Plan,
        shapes: &'s mut Shapes<'a>,
        facts: &Rc<AffineFacts>,
        planned: &PlannedFn,
        index: u32,
        run: RunContext<'a>,
    ) -> Result<crate::FinalizedFn, LowerError> {
        // A declared impl-member body lowers its declaration's params (the
        // `Self` receiver among them) and block body; every other entry
        // is a lambda's shape.
        let (params, body): (&[Param], &Expr) = if let Some(decl) = &planned.decl {
            (&decl.params, &decl.body)
        } else {
            let Expr::Lambda { params, body, .. } = &planned.lambda else {
                return Err(LowerError::unsupported(
                    planned.lambda.span(),
                    "a non-lambda function body",
                    "planned functions are const-arrow lambdas",
                ));
            };
            (params, body)
        };
        // A declared impl member's declared shape must be concrete: a
        // parameter or return over the impl's own type variable has no
        // word kind statically (the runtime word is the caller's fact),
        // and the pool-call slide checks words against the callee's
        // typed parameters — so those members refuse here: no static
        // word shape exists for them.
        // A generic body is the exception — the ONE body of a free
        // generic fn or any of its instances: the ONE body's parameters
        // and returns over the fn's own type variables are the Raw-word
        // shape the entering call sites guard at their one-word
        // instantiation boundary, and an instance reads its types
        // substituted through the lens, so its guard cannot fire.
        if let Some(decl) = &planned.decl
            && !planned.is_generic_body()
        {
            for param in decl.params.iter() {
                let Some(fact) = analyzed.checked.binders.get(&param.name.span) else {
                    continue;
                };
                if let [only] = shapes.universe().members(fact.ty)
                    && shapes.is_var_atom(*only)
                {
                    return Err(LowerError::unsupported(
                        param.name.span,
                        "a member parameter over the impl's type variable",
                        "the runtime word's kind is the caller's fact; concrete parameter words \
                         are the covered shape this round",
                    ));
                }
            }
        }
        // A named function reads its type from the package table; a
        // synthesized lambda value carries its checked type on its own
        // expression span; a declared impl member carries its solved
        // return on the plan entry (no package def names it); a struct's
        // deinit function reads the unit return the checker recorded at
        // its declared body block.
        let ret = if let Some(ret) = planned.ret {
            ret
        } else if let Some(body_span) = planned.deinit {
            analyzed
                .checked
                .expr_types
                .get(&body_span)
                .map(|val| val.ty)
                .ok_or_else(|| {
                    LowerError::unsupported(
                        planned.lambda.span(),
                        "a deinit body",
                        "the checker recorded no type for it",
                    )
                })?
        } else {
            let fn_ty = match planned.def {
                Some(def) => analyzed.checked.def_types.get(&def).copied(),
                None => analyzed
                    .checked
                    .expr_types
                    .get(&planned.lambda.span())
                    .map(|val| val.ty),
            }
            .ok_or_else(|| {
                LowerError::unsupported(
                    planned.lambda.span(),
                    "a generic or unchecked function",
                    "only monomorphic checked functions lower this round",
                )
            })?;
            // a lambda minted inside an instance lowers under the
            // instance's substitution, so its own checked type reads
            // through the lens here
            let fn_ty = shapes
                .subst(fn_ty)
                .map_err(|err| err.respanned(planned.lambda.span()))?;
            let Some((_, ret)) = fn_sig_of(shapes.universe(), fn_ty) else {
                return Err(LowerError::unsupported(
                    planned.lambda.span(),
                    "a non-function binding",
                    "planned functions must be const-arrow lambdas",
                ));
            };
            ret
        };
        if planned.decl.is_some()
            && !planned.is_generic_body()
            && let [only] = shapes.universe().members(ret)
            && shapes.is_var_atom(*only)
        {
            return Err(LowerError::unsupported(
                planned.lambda.span(),
                "a member return over the impl's type variable",
                "the returned runtime word's kind is the body's runtime fact; concrete return \
                 words are the covered shape this round",
            ));
        }
        let mut this = Self {
            analyzed,
            res,
            plan,
            shapes,
            name: planned.name.clone(),
            platform: run.platform,
            synth: planned.def.is_none() && planned.decl.is_none(),
            root: planned.name == "main",
            comptime: run.comptime,
            ret_ty: ret,
            params_len: 0,
            slots: Vec::new(),
            code: Vec::new(),
            labels: Vec::new(),
            pool: Vec::new(),
            pool_index: HashMap::new(),
            pool_strings: HashMap::new(),
            binders: HashMap::new(),
            fn_words: HashMap::new(),
            scalar_fn_words: HashSet::new(),
            wrap_memo: HashMap::new(),
            scopes: vec![HashMap::new()],
            overlay: Vec::new(),
            overlay_len: 0,
            scalar_dst: None,
            rc: {
                let mut rc = Placement::new(Rc::clone(facts));
                if run.mode == crate::LowerMode::ResumedFrame && planned.name == "main" {
                    rc.pinning_binders();
                }
                rc
            },
            evidence: HashMap::new(),
            index,
            loops: Vec::new(),
            loop_spans: Vec::new(),
            nurseries: Vec::new(),
            capture_word: None,
            capture_index: HashMap::new(),
            spawn_child: planned.spawn_child,
            comptime_path: Vec::new(),
            comptime_stack: Vec::new(),
            comptime_instance: planned.comptime_instance.clone(),
            unit_slot: None,
            value_ends: Vec::new(),
            cur_span: body.span(),
            last_span: None,
            span_marks: Vec::new(),
        };
        if planned.deinit.is_some() {
            this.bind_deinit_param(params)?;
        } else if planned.spawn_child {
            // a spawned child has no declared parameters: its capture
            // block IS the entry's argument area, laid out from the frame
            // start exactly where the spawned fiber's entry frame writes
            // it — the spawn site staged the evidence words into that
            // same block, so they bind as the entry area's trailing
            // parameters. The child holds its own copy of each captured
            // word (the spawn seam acquires one count per Ref capture),
            // so no mutation window opens here.
            this.bind_captures(&planned.captures, false)?;
            this.params_len = this.slots.len();
            this.bind_evidence(&planned.evidence)?;
        } else {
            this.bind_params(params)?;
            this.bind_captures(&planned.captures, planned.decl.is_none())?;
            if planned.decl.is_some() {
                this.bind_evidence(&planned.evidence)?;
            } else {
                // a lambda's evidence words ride its capture block: their
                // slots bind right after the real captures, exactly where
                // the CALLS copy-in writes them
                this.bind_capture_evidence(&planned.evidence)?;
                // a closure-building lambda's frame (real captures,
                // runtime-evidence words, or both) reserves one more
                // word right past the block: CALLS writes the callee's
                // closure word there, and every assignment to a captured
                // binding addresses the object through it (the mut
                // window)
                if !planned.captures.is_empty() || !planned.evidence.is_empty() {
                    let word = this.temp_word(SlotType::Ref)?;
                    this.capture_word = Some(word);
                }
            }
        }
        this.tail(body)?;
        this.finalize()
    }

    // ---- frame and buffer --------------------------------------------------

    // ---- the type lens -------------------------------------------------------
}

/// Strips parentheses around an expression (for-in iterables and
/// `in` right sides may be parenthesized ranges).
fn peel_parens(expr: &Expr) -> &Expr {
    match expr {
        Expr::Paren { inner, .. } => peel_parens(inner),
        _ => expr,
    }
}

/// Strips parentheses around a pattern: a single-element tuple pattern
/// without a trailing comma is its inner pattern parenthesized, so an
/// or-arm binds the last alternative's innermost pattern.
fn last_alt(pat: &Pattern) -> &Pattern {
    match pat {
        Pattern::Tuple {
            pats,
            trailing_comma: false,
            ..
        } if pats.len() == 1 => last_alt(&pats[0]),
        _ => pat,
    }
}
