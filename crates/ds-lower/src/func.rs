//! Per-function lowering: slots, the symbolic uop buffer, and the
//! expression walk.
//!
//! The walk consumes the checked tables: every expression's solved type
//! (flow-refined inside branches [B2]) comes from `expr_types`, every
//! binder's declared type from `binders`, and every name use from the
//! file's resolutions. Tail positions lower to RET/RETSPAN/CALLT
//! directly [H2]; every other position lowers to a place — a span of body
//! slots — that callers read or widen into their own spans.

use std::{
    borrow::Cow,
    collections::{HashMap, HashSet},
    rc::Rc,
};

use ds_abi::{BoundRet, BuiltinInstance, ConstIdx, RcOp, SlotType, Staging, Uop};
use ds_check::{
    Analyzed, AtomKind, BinderFact, ConstFact, DefId, DotRes, Extraction, GenericCallBounds,
    InherentCallBounds, InstanceId, MethodId, MethodOwner, Res, Resolutions, TraitCall, TraitId,
    Ty, TypeExpr, TypeId, Val,
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
        GENERIC_NATIVES, GenericNative, NET_CLASSES, NURSERY_BOUNDARY_ERR, Native, NativeShape,
        PLATFORM_IO_OK,
    },
    place::{AffineFacts, Hook, Owned, Placement, State},
    shape::{MemberField, Shape, Shapes, SpanWords, StructShape, expr_members, shape_words},
};

mod calls;
mod extraction;
mod pattern;
mod tail;

use extraction::AmbientExtern;

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
    /// `rc_inc` on the reference in a body word (the placed dup, C4).
    RcInc {
        src: Slot,
    },
    /// `rc_dec` (the placed drop, C4); a unique death runs the deinit hook
    /// (C5) and pools the block for reuse inside the `ds-vm/src/object.rs` store.
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
    /// A generic call site's dispatch [H3]: the callee is the generic's
    /// ONE body and the staged words end in the evidence words; `nargs`
    /// names the real arguments only.
    Callg {
        callee: u16,
        nargs: u8,
    },
    /// The dispatch tail call (CALLTG, the CALLG tail form): the same
    /// dispatch resolution and staged shape as `Callg`, transferring
    /// through the CALLT frame-reuse slide [H2's guarantee closure].
    Calltg {
        callee: u16,
        nargs: u8,
    },
    /// CALLS (the callable-word call form, F2): the callee operand is a
    /// frame slot holding the fn word; `nargs` arguments are staged on
    /// the overlay like every call form.
    Calls {
        callee_slot: u8,
        nargs: u8,
    },
    /// CLOSURE_NEW (F2): the header is the Fn word at `func`, the capture
    /// block the `n` words staged on the overlay; the fresh closure Ref
    /// lands in `dst`.
    ClosureNew {
        dst: u8,
        func: u8,
        n: u8,
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
/// use's static type (possibly refined [B2]) rides separately, from
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

/// One planned function: its table name plus the lambda that is its
/// body — a named top-level const's init, or a value-position lambda.
/// The lambda is an owned clone preserving the node's spans, so the
/// checker's span-keyed tables resolve against it unchanged and the
/// walk's borrows never outlive one lowering.
#[derive(Clone)]
pub(crate) struct PlannedFn {
    pub(crate) name: String,
    pub(crate) lambda: Expr,
    /// The package definition the function names; `None` for a
    /// synthesized lambda value (no package def exists, and nothing
    /// resolves to it by name — its word calls) and for an impl-member
    /// function (a member declares no package def — its identity is the
    /// mangled table name).
    pub(crate) def: Option<DefId>,
    /// The deinit body's block span when this is a struct's synthesized
    /// deinit function; the checked type recorded there is the unit
    /// return, and the one `self` parameter binds the struct's atom.
    /// `None` for every other planned function.
    pub(crate) deinit: Option<Span>,
    /// The enclosing bindings a synthesized lambda captures, in
    /// first-use order: binder span plus declared type (F2). Always empty
    /// for named functions — a named body resolves its own names.
    pub(crate) captures: Rc<Vec<(Span, Ty)>>,
    /// A declared impl-member body, when this entry is one: the walk
    /// lowers the declaration's params (the `Self` receiver among them)
    /// and block body instead of a lambda's shape. `lambda` still
    /// carries the body block, so its span names the lowering's file.
    pub(crate) decl: Option<Rc<DeclFn>>,
    /// The checker-solved return type, set for declared bodies: a member
    /// function has no def-typed entry the signature path reads.
    pub(crate) ret: Option<Ty>,
    /// The runtime-evidence pairs this frame's body demands, in body
    /// order — a generic body's own sites, every demand lexically
    /// inside its lambdas and spawned subtrees, and every generic
    /// callee's own demands its call sites' records answer in the
    /// runtime form (a lambda's or spawned child's entry carries its own
    /// body's collection when the entry is one). Empty outside generic
    /// bodies.
    pub(crate) evidence: Rc<Vec<RuntimePair>>,
    /// The declared bounds of the generic impl whose member body this
    /// entry lowers (empty elsewhere): a lambda or spawned child
    /// lexically inside the body inherits them, so its own demands
    /// collect at its construction or spawn site.
    pub(crate) bounds: Rc<HashMap<String, HashSet<TraitId>>>,
    /// Whether this entry is a `spawn` expression's child [E2]: the
    /// capture block is the entry's own argument area — the spawned
    /// fiber's entry frame writes it exactly where a run entry writes
    /// its parameters, evidence words trailing the captures.
    pub(crate) spawn_child: bool,
    /// Whether this entry is a free generic fn's ONE body [H3]: its
    /// declared parameters may sit over the fn's own type variables
    /// (one Raw word each — the runtime kind is the caller's fact), and
    /// every call site entering it stages the body's runtime-evidence
    /// demands as trailing words after the true arguments. False for
    /// impl-member declared bodies, whose parameters stay concrete.
    pub(crate) free_generic: bool,
}

/// One declared body's lowering shape (an impl member's or a free
/// generic fn's): its parameters (binder facts live in the checked
/// tables, keyed by name span) and its body wrapped as the block
/// expression the tail walk lowers.
pub(crate) struct DeclFn {
    pub(crate) params: Rc<Vec<Param>>,
    pub(crate) body: Rc<Expr>,
}

/// One runtime-evidence demand inside a generic body (a generic impl
/// member's or a free generic fn's): the bound type variable a nested
/// trait-method call receives on, plus the member it calls. The body's
/// plan carries the pairs in body order; the call site that enters the
/// body stages one function-table word per pair — the serving member
/// body under its own substitution — and the nested site dispatches
/// through the word at its frame slot.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct RuntimePair {
    /// The declaration's generic parameter name (`t` in `[t: shows]`):
    /// the instance or fn declaration and the body's checking mint their
    /// own variable atoms, so the demand keys by the written name they
    /// share.
    pub(crate) var: String,
    /// The trait member the body calls on it.
    pub(crate) method: MethodId,
}

/// A generic body's runtime-evidence environment: the demand
/// pairs the body's frame stages slots for, plus the declared bounds
/// they collected against — the bounds ride along so a lambda lexically
/// inside the body inherits them and collects its own demands at its
/// construction.
pub(crate) struct EvidenceEnv {
    pub(crate) pairs: Rc<Vec<RuntimePair>>,
    pub(crate) bounds: Rc<HashMap<String, HashSet<TraitId>>>,
}

/// One generic body the package's demand table keys: a free generic fn
/// by its definition, a generic impl's member by the impl item's span
/// and member name, a trait's declared default body by the trait's
/// definition and member name.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub(crate) enum BodyId {
    Free(DefId),
    Member { impl_site: Span, member: String },
    Default { trait_def: DefId, member: String },
}

/// Every generic body's runtime-evidence demands after the package-wide
/// propagation: a body's walk collects its own dot-calls plus, at every
/// call entering another generic body (a free generic fn or a generic
/// inherent member), that callee's own demands whose per-call record
/// answers in the runtime form — keyed onto the calling frame's
/// variables, the same peel the staging's slot lookup runs. A callee's
/// demands may themselves carry callee demands, so the pass iterates
/// until no body's list grows; the walk always reads a callee's list
/// through this table, keyed by the callee's identity.
#[derive(Default, Clone)]
pub(crate) struct Demands {
    pairs: HashMap<BodyId, Rc<Vec<RuntimePair>>>,
}

impl Demands {
    pub(crate) fn insert(&mut self, id: BodyId, pairs: Rc<Vec<RuntimePair>>) {
        self.pairs.insert(id, pairs);
    }

    pub(crate) fn get(&self, id: &BodyId) -> Option<&[RuntimePair]> {
        self.pairs.get(id).map(|pairs| pairs.as_slice())
    }

    /// One body's list as an `Rc` clone (the planning pushes move it
    /// into their `EvidenceEnv`).
    pub(crate) fn rc(&self, id: &BodyId) -> Option<Rc<Vec<RuntimePair>>> {
        self.pairs.get(id).cloned()
    }
}

/// One staged evidence word's source: an evidence-chain object's
/// table index (the entering site's substitution is concrete), or a
/// frame slot read (a nested site inside a generic body whose
/// substitution is that frame's own — the word rides the frame's
/// evidence area).
#[derive(Clone, Copy)]
enum EvidenceSrc {
    /// The evidence-chain object's index in the module's chain table.
    Const(u32),
    /// This frame's evidence-area word (a `Mov` into the overlay).
    Slot(u8),
    /// The dead sentinel: a demand the entering branch's head never
    /// mentions, staged as a word naming no chain object (the nested
    /// dispatch that reads it can only run under a narrowing that branch
    /// cannot satisfy).
    Dead,
}

/// A solved trait-method call site's dispatch [B6, H3].
pub(crate) enum TraitDispatch {
    /// The impl body the checker's solved instance names: the body's
    /// function-table index plus the checker-recorded call (the declared
    /// instance id rides as the final staged word, the per-bound evidence
    /// words the body's runtime pairs demand stage ahead of it).
    Static { index: u32, call: TraitCall },
    /// A bound-variable receiver inside a generic impl body: dispatch
    /// resolves at runtime from the frame's evidence slot the call site
    /// entering the body staged. The trait member the evidence pair
    /// names rides along so omitted defaulted parameters stage the
    /// trait's declared defaults (the call-site contract).
    Runtime { slot: u8, method: MethodId },
    /// A solved inherent-method call site: the impl body's
    /// function-table index plus the checker-recorded bounds when the
    /// body is a generic impl's (empty for a monomorphic impl's), the
    /// method id naming the impl item the transitive chain starts from.
    /// The receiver's own type picked the body at check time, so the call
    /// dispatches like a plain local callee — no instance word and no
    /// dispatch uop; a generic body's evidence words stage trailing the
    /// arguments and the body claims them as its trailing parameters.
    Inherent {
        index: u32,
        bounds: InherentCallBounds,
        method: MethodId,
    },
}

/// The module plan: lowered functions in function-table order.
pub(crate) struct Plan {
    functions: Vec<PlannedFn>,
    index: HashMap<DefId, u32>,
    /// Impl-member bodies keyed by (impl item span, member name): a
    /// trait-method call site reaches its body through the solved
    /// instance's impl item, never through a package def.
    impl_index: HashMap<(Span, String), u32>,
    /// The trait's declared default bodies keyed by (trait definition,
    /// member name): a dispatch whose solving impl spells no member
    /// reaches the trait's default body through this tie.
    default_index: HashMap<(DefId, String), u32>,
    /// The function-table indexes of every planned trait default body.
    default_bodies: HashSet<u32>,
    /// The function-table index of every planned struct deinit, keyed
    /// by the struct's definition.
    deinits: HashMap<DefId, u32>,
    /// Every struct whose declaration carries a `deinit` member,
    /// planned or not (a generic struct's deinit declares but cannot
    /// plan — monomorphic instantiation is the CALL_GENERIC seam).
    declared_deinits: HashSet<DefId>,
    /// Every generic body's final demand table: the planning pushes read
    /// their own body's list here, and the lambda/spawn walks at
    /// lowering time read callee bodies' lists through it.
    demands: Demands,
    /// The module's evidence-chain objects [H3], built during lowering
    /// as the entering sites walk their chains: entry `i` is what a
    /// staged word carrying `i` names.
    chains: Vec<ds_mod::EvidenceChain>,
    /// The memo the chain walk keys its states by (member, instance,
    /// substitution): a repeated state shares its entry, which is also
    /// the walk's termination — a cyclic demand graph builds a cyclic
    /// table the dispatches walk lazily, one hop per call.
    chain_memo: HashMap<(MethodId, InstanceId, Option<Vec<TypeId>>), u32>,
}

impl Plan {
    pub(crate) fn new() -> Self {
        Self {
            functions: Vec::new(),
            index: HashMap::new(),
            impl_index: HashMap::new(),
            default_index: HashMap::new(),
            default_bodies: HashSet::new(),
            deinits: HashMap::new(),
            declared_deinits: HashSet::new(),
            demands: Demands::default(),
            chains: Vec::new(),
            chain_memo: HashMap::new(),
        }
    }

    /// Installs the package's propagated demand table before any body
    /// plans.
    pub(crate) fn set_demands(&mut self, demands: Demands) {
        self.demands = demands;
    }

    /// One generic body's final demands: a body outside the table (a
    /// boundless one) demands nothing.
    pub(crate) fn demand(&self, id: &BodyId) -> Rc<Vec<RuntimePair>> {
        self.demands.rc(id).unwrap_or_else(|| Rc::new(Vec::new()))
    }

    pub(crate) fn push(&mut self, name: String, def: DefId, local: &LocalDecl) {
        self.index.insert(
            def,
            u32::try_from(self.functions.len()).expect("function index fits u32"),
        );
        self.functions.push(PlannedFn {
            name,
            lambda: local.init.clone(),
            def: Some(def),
            deinit: None,
            captures: Rc::new(Vec::new()),
            decl: None,
            ret: None,
            evidence: Rc::new(Vec::new()),
            bounds: Rc::new(HashMap::new()),
            spawn_child: false,
            free_generic: false,
        });
    }

    /// Registers one free generic fn's ONE body [H3]: the fn lowers
    /// once, never per instantiation, under its own name (a package def
    /// is unique, and the impl members' `#` mangles sit outside the
    /// identifier charset). `local.init` is the fn's lambda shape; its
    /// parameters and block become the declared body the walk lowers
    /// like an impl member's, and the entering call sites stage the
    /// evidence words its runtime pairs demand after the true
    /// arguments. `None` return: the checker's def type carries the
    /// fn's signature over its minted variables.
    pub(crate) fn push_free_generic(
        &mut self,
        name: String,
        def: DefId,
        local: &LocalDecl,
        evidence: EvidenceEnv,
    ) {
        let Expr::Lambda { params, body, .. } = &local.init else {
            unreachable!("push_free_generic receives a lambda-armed local");
        };
        self.index.insert(
            def,
            u32::try_from(self.functions.len()).expect("function index fits u32"),
        );
        self.functions.push(PlannedFn {
            name,
            lambda: local.init.clone(),
            def: Some(def),
            deinit: None,
            captures: Rc::new(Vec::new()),
            decl: Some(Rc::new(DeclFn {
                params: Rc::new(params.clone()),
                body: Rc::new((**body).clone()),
            })),
            ret: None,
            evidence: evidence.pairs,
            bounds: evidence.bounds,
            spawn_child: false,
            free_generic: true,
        });
    }

    /// Registers one value-position lambda as a synthesized function and
    /// returns its table index plus the captures its body will bind (the
    /// construction and the body share the one list, so the staged block
    /// and the frame layout cannot drift). The minted name keys off the
    /// enclosing function and the table index — `$` is outside the
    /// identifier charset, so the name never shadows a user-declared one.
    pub(crate) fn push_lambda(
        &mut self,
        enclosing: &str,
        lambda: &Expr,
        res: &Resolutions,
        binders: &HashMap<Span, BinderFact>,
        evidence: Rc<Vec<RuntimePair>>,
        bounds: Rc<HashMap<String, HashSet<TraitId>>>,
    ) -> (u32, Rc<Vec<(Span, Ty)>>) {
        let index = u32::try_from(self.functions.len()).expect("function index fits u32");
        let captures = Rc::new(collect_captures(lambda, res, binders));
        self.functions.push(PlannedFn {
            name: format!("{enclosing}${index}"),
            lambda: lambda.clone(),
            def: None,
            deinit: None,
            captures: captures.clone(),
            decl: None,
            ret: None,
            evidence,
            bounds,
            spawn_child: false,
            free_generic: false,
        });
        (index, captures)
    }

    /// Registers one `spawn` expression's block as a synthesized child
    /// function [E2] and returns its table index plus the captures its
    /// body will bind: the block becomes a zero-parameter lambda over the
    /// same capture list the lambda construction uses. The child's
    /// checked unit type rides the spawn expression's own span, so the
    /// plan entry carries the return directly. A child lexically inside a
    /// generic impl body carries its own runtime-evidence pairs (staged
    /// from the spawning frame's slots into the entry argument area) and
    /// inherits the impl's bounds so its nested lambdas and spawns
    /// collect their demands the same way.
    pub(crate) fn push_spawn(
        &mut self,
        enclosing: &str,
        spawn: &Expr,
        res: &Resolutions,
        binders: &HashMap<Span, BinderFact>,
        ret: Ty,
        evidence: Rc<Vec<RuntimePair>>,
        bounds: Rc<HashMap<String, HashSet<TraitId>>>,
    ) -> (u32, Rc<Vec<(Span, Ty)>>) {
        let Expr::Spawn { span, body, .. } = spawn else {
            unreachable!("push_spawn receives a spawn expression");
        };
        let index = u32::try_from(self.functions.len()).expect("function index fits u32");
        let lambda = Expr::Lambda {
            span: *span,
            generics: Vec::new(),
            params: Vec::new(),
            ret: None,
            arrow: false,
            body: Box::new(Expr::Block(body.clone())),
        };
        let captures = Rc::new(collect_captures(&lambda, res, binders));
        self.functions.push(PlannedFn {
            name: format!("{enclosing}${index}"),
            lambda,
            def: None,
            deinit: None,
            captures: captures.clone(),
            decl: None,
            ret: Some(ret),
            evidence,
            bounds,
            spawn_child: true,
            free_generic: false,
        });
        (index, captures)
    }

    /// Registers one impl-member function under its mangled table name
    /// and returns its index. The mangle `<trait>#<target>#<member>` is
    /// deterministic, and `#` sits outside the identifier charset, so a
    /// user-declared name can never collide; two impls of one trait over
    /// intersecting heads cannot both check clean (B6), so the mangle is
    /// unique per body.
    pub(crate) fn push_declared(
        &mut self,
        name: String,
        impl_site: Span,
        member: String,
        params: Rc<Vec<Param>>,
        body: Rc<Expr>,
        ret: Ty,
        evidence: EvidenceEnv,
    ) -> u32 {
        let index = u32::try_from(self.functions.len()).expect("function index fits u32");
        self.impl_index.insert((impl_site, member), index);
        self.functions.push(PlannedFn {
            name,
            lambda: (*body).clone(),
            def: None,
            captures: Rc::new(Vec::new()),
            decl: Some(Rc::new(DeclFn { params, body })),
            ret: Some(ret),
            deinit: None,
            evidence: evidence.pairs,
            bounds: evidence.bounds,
            spawn_child: false,
            free_generic: false,
        });
        index
    }

    /// Registers one trait's declared default body under its mangled
    /// table name and returns its index. The mangle `<trait>#<member>`
    /// cannot collide with an impl member's three-part mangle or an
    /// inherent impl's two-part one (a trait and a type share one
    /// namespace, and `#` sits outside the identifier charset). The
    /// body is the ONE generic shape over the trait's own variables:
    /// its declared parameters may sit over them (`self` typed `Self`
    /// among them — one Raw word each, the runtime kind the caller's
    /// fact), and the dispatch sites entering it stage the body's
    /// runtime-evidence demands as trailing words after the true
    /// arguments, the same shape a free generic fn's entering sites
    /// stage.
    pub(crate) fn push_default_body(
        &mut self,
        name: String,
        trait_def: DefId,
        member: String,
        params: Rc<Vec<Param>>,
        body: Rc<Expr>,
        ret: Option<Ty>,
        evidence: EvidenceEnv,
    ) -> u32 {
        let index = u32::try_from(self.functions.len()).expect("function index fits u32");
        self.default_index.insert((trait_def, member), index);
        self.default_bodies.insert(index);
        self.functions.push(PlannedFn {
            name,
            lambda: (*body).clone(),
            def: None,
            deinit: None,
            captures: Rc::new(Vec::new()),
            decl: Some(Rc::new(DeclFn { params, body })),
            ret,
            evidence: evidence.pairs,
            bounds: evidence.bounds,
            spawn_child: false,
            free_generic: true,
        });
        index
    }

    /// The impl-member body a solved instance plus member name address.
    pub(crate) fn impl_body(&self, impl_site: Span, member: &str) -> Option<u32> {
        self.impl_index
            .get(&(impl_site, member.to_owned()))
            .copied()
    }

    /// The trait's declared default body for one member.
    pub(crate) fn default_body(&self, trait_def: DefId, member: &str) -> Option<u32> {
        self.default_index
            .get(&(trait_def, member.to_owned()))
            .copied()
    }

    /// Whether one function-table entry is a trait's declared default
    /// body.
    pub(crate) fn is_default_body(&self, index: u32) -> bool {
        self.default_bodies.contains(&index)
    }

    /// Registers one struct's `deinit` member as a synthesized module
    /// function: one `self` parameter (borrowed, the dying instance)
    /// whose body is the declared block, named after the struct — `$`
    /// sits outside the identifier charset, so the name never shadows a
    /// user one. Every declared deinit registers here, planned or not:
    /// a struct whose deinit carries no plan (a generic struct's) makes
    /// binding its values refuse instead of silently skipping the hook.
    pub(crate) fn push_deinit(&mut self, struct_def: DefId, name: &str, lambda: Expr, body: Span) {
        let index = u32::try_from(self.functions.len()).expect("function index fits u32");
        self.declared_deinits.insert(struct_def);
        self.deinits.insert(struct_def, index);
        self.functions.push(PlannedFn {
            name: format!("{name}$deinit"),
            lambda,
            def: None,
            deinit: Some(body),
            captures: Rc::new(Vec::new()),
            decl: None,
            ret: None,
            evidence: Rc::new(Vec::new()),
            bounds: Rc::new(HashMap::new()),
            spawn_child: false,
            free_generic: false,
        });
    }

    /// Records a struct whose declaration carries a `deinit` member but
    /// whose body has no synthesized function (a generic struct's).
    pub(crate) fn declare_deinit(&mut self, struct_def: DefId) {
        self.declared_deinits.insert(struct_def);
    }

    /// The function-table index of a struct's planned deinit.
    pub(crate) fn deinit_index(&self, def: DefId) -> Option<u32> {
        self.deinits.get(&def).copied()
    }

    /// Whether a struct's declaration carries a `deinit` member.
    pub(crate) fn declares_deinit(&self, def: DefId) -> bool {
        self.declared_deinits.contains(&def)
    }

    /// Every struct whose declaration carries a `deinit` member.
    pub(crate) fn declared_deinits(&self) -> HashSet<DefId> {
        self.declared_deinits.clone()
    }

    /// The planned function at one table index (the worklist read; the
    /// queue grows while function bodies lower).
    pub(crate) fn function(&self, index: usize) -> Option<&PlannedFn> {
        self.functions.get(index)
    }

    /// The function-table index of a planned DefId.
    pub(crate) fn index_of(&self, def: DefId) -> Option<u32> {
        self.index.get(&def).copied()
    }

    pub(crate) fn functions(&self) -> &[PlannedFn] {
        &self.functions
    }

    /// The module's finished evidence-chain table, drained by the module
    /// build after every body lowered.
    pub(crate) fn take_chains(&mut self) -> Vec<ds_mod::EvidenceChain> {
        std::mem::take(&mut self.chains)
    }

    /// The widest evidence-pair count any planned body claims: a nested
    /// runtime dispatch site cannot know its runtime-resolved serving
    /// body's demand count, so its overlay reserves this bound (the
    /// dispatch itself stages exactly what the chain object carries).
    pub(crate) fn max_evidence_pairs(&self) -> usize {
        self.functions
            .iter()
            .map(|planned| planned.evidence.len())
            .max()
            .unwrap_or(0)
    }

    pub(crate) fn names(&self) -> Vec<String> {
        self.functions.iter().map(|f| f.name.clone()).collect()
    }

    pub(crate) fn has_main(&self) -> bool {
        self.functions.iter().any(|f| f.name == "main")
    }
}

/// The enclosing bindings one lambda body captures, in first-use order
/// (F2): every single-segment path or interpolation name inside the
/// subtree that resolves to a binder whose bind span starts strictly
/// before the lambda — the resolver binds in flow order, so every binder
/// of the subtree itself (the lambda's params, its body's locals, a
/// nested lambda's) binds inside it, and a nested lambda's outer use is
/// this one's capture transitively. The walk covers every expression
/// variant; a missed shape degrades to the missing-binder refusal at its
/// use, never a miscompile.
fn collect_captures(
    lambda: &Expr,
    res: &Resolutions,
    binders: &HashMap<Span, BinderFact>,
) -> Vec<(Span, Ty)> {
    let start = lambda.span().start;
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    visit_uses(lambda, &mut |span| {
        capture_use(span, start, res, binders, &mut seen, &mut out);
    });
    out
}

/// The earliest `return` or `?` sitting directly inside a block's
/// statements [E2]: never one nested inside a lambda or a spawned block —
/// their bodies are separate frames with their own exits. The nursery
/// emission refuses strays (the block's scope exit must join), and the
/// spawn emission refuses them (a spawned body completes with its own
/// value, never the enclosing function's).
fn stray_exit(block: &BlockExpr) -> Option<Span> {
    block.stmts.iter().find_map(|stmt| match &stmt.kind {
        StmtKind::Local(local) => stray_exit_expr(&local.init),
        StmtKind::Expr(expr) => stray_exit_expr(expr),
    })
}

/// Every `cancel` call's operand span inside one block's statements [E2]:
/// the payload sites a spawned body hands the boundary, collected for the
/// spawn site's containment proof. Lambdas and nested spawned blocks are
/// separate frames and stay skipped (their own sites answer their own
/// boundaries).
fn cancel_operands(block: &BlockExpr, out: &mut Vec<Span>) {
    cancel_operands_walk(block, out, false);
}

/// The spawn-site containment proof's walk: a spawned block's own cancel
/// sites, recursing through nested nursery blocks (an over-conservative
/// proof is legal — a false site only adds a redundant check) but not
/// through nested spawns (their cancels answer their own boundary).
fn cancel_operands_walk(block: &BlockExpr, out: &mut Vec<Span>, through_spawns: bool) {
    for stmt in &block.stmts {
        match &stmt.kind {
            StmtKind::Local(local) => {
                cancel_operands_expr(&local.init, out, through_spawns);
            },
            StmtKind::Expr(expr) => cancel_operands_expr(expr, out, through_spawns),
        }
    }
}

/// The boundary join's walk: the cancel sites whose payloads can answer
/// this block's own boundary — recursing through the spawned bodies (a
/// spawned child cancels into this nursery) but not into nested nursery
/// blocks (their cancels answer their own boundary, and their failures
/// re-raise as the child's completion value, never as a payload).
fn spawned_cancel_operands(block: &BlockExpr, out: &mut Vec<Span>) {
    cancel_operands_walk(block, out, true);
}

fn cancel_operands_expr(e: &Expr, out: &mut Vec<Span>, through_spawns: bool) {
    if let Expr::Call { callee, args, .. } = e
        && let Expr::Path { path, .. } = &**callee
        && path.segments.len() == 1
        && path.segments[0].name.as_str() == "cancel"
        && let Some(arg) = args.first()
    {
        // the ambient spelling is generic with exactly one parameter, and
        // a same-named user function plans (so the def check in the
        // emission refuses it); the collection is name-keyed on purpose —
        // a false site only adds a redundant containment proof
        out.push(arg.value.span());
    }
    match e {
        Expr::Paren { inner, .. }
        | Expr::Unary { expr: inner, .. }
        | Expr::Is { expr: inner, .. }
        | Expr::Try { expr: inner, .. } => cancel_operands_expr(inner, out, through_spawns),
        Expr::Binary { left, right, .. }
        | Expr::Range {
            start: left,
            end: right,
            ..
        }
        | Expr::Assign { left, right, .. } => {
            cancel_operands_expr(left, out, through_spawns);
            cancel_operands_expr(right, out, through_spawns);
        },
        Expr::Str { parts, .. } => {
            for part in parts {
                if let StrPart::Expr { expr, .. } = part {
                    cancel_operands_expr(expr, out, through_spawns);
                }
            }
        },
        Expr::Call { callee, args, .. } => {
            cancel_operands_expr(callee, out, through_spawns);
            for arg in args {
                cancel_operands_expr(&arg.value, out, through_spawns);
            }
        },
        Expr::Field { recv, .. } | Expr::Index { recv, .. } => {
            cancel_operands_expr(recv, out, through_spawns);
        },
        Expr::MapApply { key, value, .. } => {
            cancel_operands_expr(key, out, through_spawns);
            cancel_operands_expr(value, out, through_spawns);
        },
        Expr::Struct { fields, .. } => {
            for (_, value) in fields {
                if let Some(value) = value {
                    cancel_operands_expr(value, out, through_spawns);
                }
            }
        },
        Expr::Tuple { items, .. } | Expr::List { items, .. } => {
            for item in items {
                cancel_operands_expr(item, out, through_spawns);
            }
        },
        Expr::If {
            cond, then, else_, ..
        } => {
            cancel_operands_expr(cond, out, through_spawns);
            cancel_operands_walk(then, out, through_spawns);
            if let Some(else_) = else_ {
                cancel_operands_expr(else_, out, through_spawns);
            }
        },
        Expr::Match {
            scrutinee, arms, ..
        } => {
            cancel_operands_expr(scrutinee, out, through_spawns);
            for arm in arms {
                if let Some(guard) = &arm.guard {
                    cancel_operands_expr(guard, out, through_spawns);
                }
                cancel_operands_expr(&arm.body, out, through_spawns);
            }
        },
        Expr::While { cond, body, .. } => {
            cancel_operands_expr(cond, out, through_spawns);
            cancel_operands_walk(body, out, through_spawns);
        },
        Expr::For { iter, body, .. } => {
            cancel_operands_expr(iter, out, through_spawns);
            cancel_operands_expr(body, out, through_spawns);
        },
        // the two traversal modes split exactly here: the proof walk
        // skips nested spawns and recurses into nurseries, the boundary
        // walk recurses into spawns and skips nurseries
        Expr::Nursery { body, .. } => {
            if !through_spawns {
                cancel_operands_walk(body, out, through_spawns);
            }
        },
        Expr::Spawn { body, .. } => {
            if through_spawns {
                cancel_operands_walk(body, out, through_spawns);
            }
        },
        Expr::Block(block) => cancel_operands_walk(block, out, through_spawns),
        Expr::Literal { .. }
        | Expr::RawStr { .. }
        | Expr::Path { .. }
        | Expr::FnType { .. }
        | Expr::Return { .. }
        | Expr::Break(_)
        | Expr::Continue(_)
        | Expr::Lambda { .. }
        | Expr::Defer { .. } => {},
    }
}

/// One expression subtree's earliest direct exit, walking every container
/// kind; lambdas and spawned blocks are separate frames and stay skipped.
fn stray_exit_expr(e: &Expr) -> Option<Span> {
    match e {
        Expr::Return { span, .. } | Expr::Try { span, .. } => Some(*span),
        Expr::Paren { inner, .. }
        | Expr::Unary { expr: inner, .. }
        | Expr::Is { expr: inner, .. } => stray_exit_expr(inner),
        Expr::Binary { left, right, .. } => {
            stray_exit_expr(left).or_else(|| stray_exit_expr(right))
        },
        Expr::Range { start, end, .. } => stray_exit_expr(start).or_else(|| stray_exit_expr(end)),
        Expr::Assign { left, right, .. } => {
            stray_exit_expr(left).or_else(|| stray_exit_expr(right))
        },
        Expr::Str { parts, .. } => parts.iter().find_map(|part| match part {
            StrPart::Expr { expr, .. } => stray_exit_expr(expr),
            StrPart::Text { .. } | StrPart::Ident { .. } => None,
        }),
        Expr::Call { callee, args, .. } => stray_exit_expr(callee)
            .or_else(|| args.iter().find_map(|arg| stray_exit_expr(&arg.value))),
        Expr::Field { recv, .. } | Expr::Index { recv, .. } => stray_exit_expr(recv),
        Expr::MapApply { key, value, .. } => {
            stray_exit_expr(key).or_else(|| stray_exit_expr(value))
        },
        Expr::Struct { fields, .. } => fields
            .iter()
            .find_map(|(_, value)| value.as_ref().and_then(stray_exit_expr)),
        Expr::Tuple { items, .. } | Expr::List { items, .. } => {
            items.iter().find_map(stray_exit_expr)
        },
        Expr::If {
            cond, then, else_, ..
        } => stray_exit_expr(cond)
            .or_else(|| stray_exit(then))
            .or_else(|| else_.as_deref().and_then(stray_exit_expr)),
        Expr::Match {
            scrutinee, arms, ..
        } => stray_exit_expr(scrutinee).or_else(|| {
            arms.iter().find_map(|arm| {
                arm.guard
                    .as_ref()
                    .and_then(stray_exit_expr)
                    .or_else(|| stray_exit_expr(&arm.body))
            })
        }),
        Expr::While { cond, body, .. } => stray_exit_expr(cond).or_else(|| stray_exit(body)),
        Expr::For { iter, body, .. } => stray_exit_expr(iter).or_else(|| stray_exit_expr(body)),
        Expr::Nursery { body, .. } => stray_exit(body),
        Expr::Block(block) => stray_exit(block),
        Expr::Literal { .. }
        | Expr::RawStr { .. }
        | Expr::Path { .. }
        | Expr::FnType { .. }
        | Expr::Lambda { .. }
        | Expr::Spawn { .. }
        | Expr::Defer { .. }
        | Expr::Break(_)
        | Expr::Continue(_) => None,
    }
}

/// One visited use span: an enclosing local's bind becomes one capture
/// entry, once, with its checked type.
fn capture_use(
    span: Span,
    start: u32,
    res: &Resolutions,
    binders: &HashMap<Span, BinderFact>,
    seen: &mut HashSet<Span>,
    out: &mut Vec<(Span, Ty)>,
) {
    let Some(Res::Local { bind }) = res.get(span) else {
        return;
    };
    if bind.start >= start || !seen.insert(*bind) {
        return;
    }
    if let Some(fact) = binders.get(bind) {
        out.push((*bind, fact.ty));
    }
}

/// Visits every local-name use span inside one expression subtree: path
/// segments, interpolation idents, and punned struct-field names are the
/// use sites; every other variant recurses into its subexpressions.
fn visit_uses(e: &Expr, visit: &mut dyn FnMut(Span)) {
    let block = |block: &BlockExpr, visit: &mut dyn FnMut(Span)| {
        for stmt in &block.stmts {
            match &stmt.kind {
                StmtKind::Local(local) => visit_uses(&local.init, visit),
                StmtKind::Expr(expr) => visit_uses(expr, visit),
            }
        }
    };
    match e {
        // a fn type in the constructor brackets is a type spelling; it
        // names no value
        Expr::Literal { .. }
        | Expr::RawStr { .. }
        | Expr::Break(_)
        | Expr::Continue(_)
        | Expr::FnType { .. } => {},
        Expr::Str { parts, .. } => {
            for part in parts {
                match part {
                    StrPart::Text { .. } => {},
                    StrPart::Expr { expr, .. } => visit_uses(expr, visit),
                    // `$x` is a name use like a path segment
                    StrPart::Ident { name, .. } => visit(name.span),
                }
            }
        },
        Expr::Path { path, .. } => {
            if let [only] = path.segments.as_slice() {
                visit(only.span);
            }
        },
        // a nested lambda's body is walked here: its own outer uses are
        // captures of this lambda when they bind before it
        Expr::Lambda { body, .. } => visit_uses(body, visit),
        Expr::Paren { inner, .. }
        | Expr::Try { expr: inner, .. }
        | Expr::Unary { expr: inner, .. } => visit_uses(inner, visit),
        Expr::Struct { fields, .. } => {
            for (name, value) in fields {
                // a punned field names its local; an explicit value is
                // walked below either way
                visit(name.span);
                if let Some(value) = value {
                    visit_uses(value, visit);
                }
            }
        },
        Expr::Tuple { items, .. } | Expr::List { items, .. } => {
            for item in items {
                visit_uses(item, visit);
            }
        },
        Expr::Call {
            callee,
            args,
            trailing,
            ..
        } => {
            visit_uses(callee, visit);
            for arg in args {
                visit_uses(&arg.value, visit);
            }
            if let Some(block_arg) = trailing {
                visit_uses(block_arg, visit);
            }
        },
        Expr::Field { recv, name, .. } => {
            visit_uses(recv, visit);
            visit(name.span);
        },
        Expr::Index { recv, index, .. } => {
            visit_uses(recv, visit);
            visit_uses(index, visit);
        },
        Expr::MapApply { key, value, .. } => {
            visit_uses(key, visit);
            visit_uses(value, visit);
        },
        Expr::Is { expr, .. } => visit_uses(expr, visit),
        Expr::Binary { left, right, .. } | Expr::Assign { left, right, .. } => {
            visit_uses(left, visit);
            visit_uses(right, visit);
        },
        Expr::Range { start, end, .. } => {
            visit_uses(start, visit);
            visit_uses(end, visit);
        },
        Expr::If {
            cond, then, else_, ..
        } => {
            visit_uses(cond, visit);
            block(then, visit);
            if let Some(else_expr) = else_ {
                visit_uses(else_expr, visit);
            }
        },
        Expr::Match {
            scrutinee, arms, ..
        } => {
            visit_uses(scrutinee, visit);
            for arm in arms {
                if let Some(guard) = &arm.guard {
                    visit_uses(guard, visit);
                }
                visit_uses(&arm.body, visit);
            }
        },
        Expr::While { cond, body, .. } => {
            visit_uses(cond, visit);
            block(body, visit);
        },
        Expr::For { iter, body, .. } => {
            visit_uses(iter, visit);
            visit_uses(body, visit);
        },
        Expr::Return { expr, .. } => {
            if let Some(expr) = expr {
                visit_uses(expr, visit);
            }
        },
        Expr::Nursery { body, .. } | Expr::Defer { body, .. } | Expr::Spawn { body, .. } => {
            block(body, visit);
        },
        Expr::Block(inner) => block(inner, visit),
    }
}

/// Collects one frame's runtime-evidence demands, in body order: every
/// trait-method call whose receiver's checked type is the body's own
/// bound type variable (the checker satisfied it through the body's
/// in-scope bound, so no solved instance records there), plus every
/// generic callee's own demands the site's record answers in the
/// runtime form (below). Value-site lambda and spawned subtrees stay
/// in — their demands ride the child's own entry words (a lambda's
/// capture block, a spawned child's entry argument area), staged from
/// this frame's evidence slots at the construction or spawn site.
pub(crate) fn runtime_pairs(
    analyzed: &Analyzed,
    res: &Resolutions,
    bounds: &HashMap<String, HashSet<TraitId>>,
    body: &Expr,
    demands: &Demands,
) -> Vec<RuntimePair> {
    let mut walk = PairWalk {
        analyzed,
        res,
        bounds,
        demands,
        seen: HashSet::new(),
        out: Vec::new(),
    };
    walk.expr(body);
    walk.out
}

/// Whether an atom names a minted type variable anywhere in its shape —
/// any variable (`None`), or one of the given written base name: the
/// variable itself, or one nested in an application argument. A
/// trait-reference argument carrying one names a compound substitution
/// this seam resolves per variable alone; a branch head carrying the
/// named one keeps its per-branch condition on it.
fn carries_a_var(universe: &ds_check::Universe, atom: TypeId, want: Option<&str>) -> bool {
    if let Some(name) = crate::shape::var_param_name(universe, atom) {
        return want.is_none_or(|want| name == want);
    }
    let Ok(decl) = universe.atom_decl(atom) else {
        return false;
    };
    match &decl.kind {
        AtomKind::Apply(_, args) => args.iter().any(|arg| {
            universe
                .members(*arg)
                .iter()
                .any(|&a| carries_a_var(universe, a, want))
        }),
        _ => false,
    }
}

/// The staged chain word for a demand whose variable the entering
/// branch's head never mentions: no chain object, a marker a nested
/// dispatch can read only under a narrowing that branch cannot satisfy,
/// trapping loudly there (a checker gap) rather than calling a wrong
/// body.
pub(crate) const DEAD_CHAIN: i64 = -1;

/// The generic parameter one single-atom receiver covers: the atom itself
/// when it is a minted variable, else the variable at the deepest
/// single-argument constructor chain inside it (`list[t]` and
/// `list[list[t]]` both demand on `t`). Union receivers and variable-free
/// atoms cover nothing — the demand walk and the nested-site lookup key
/// by this one name.
fn peeled_param(universe: &ds_check::Universe, atom: TypeId) -> Option<String> {
    if let Some(param) = crate::shape::var_param_name(universe, atom) {
        return Some(param);
    }
    let decl = universe.atom_decl(atom).ok()?;
    let AtomKind::Apply(_, args) = &decl.kind else {
        return None;
    };
    let [only] = args.as_ref() else {
        return None;
    };
    let [member] = universe.members(*only) else {
        return None;
    };
    peeled_param(universe, *member)
}

/// The pair-collecting walk over one member body.
struct PairWalk<'a> {
    analyzed: &'a Analyzed,
    res: &'a Resolutions,
    bounds: &'a HashMap<String, HashSet<TraitId>>,
    demands: &'a Demands,
    seen: HashSet<(String, MethodId)>,
    out: Vec<RuntimePair>,
}

impl PairWalk<'_> {
    /// One dot-call whose receiver covers a bounded type variable demands
    /// one evidence word: the member it calls, recorded once. The receiver
    /// is the variable itself (a nested runtime site) or a constructor
    /// over it (a transitive site whose serving body is itself a generic
    /// impl member — its rider words ride this frame's area for the same
    /// pair, so the walk collects the demand here too).
    fn call(&mut self, callee: &Expr, recv: &Expr) {
        let Expr::Field { name, .. } = callee else {
            return;
        };
        // A type-keyed member call carries no receiver value, so no frame
        // variable can demand an evidence word through it.
        if matches!(
            self.res.get(name.span),
            Some(Res::Dot(DotRes::Static { .. }))
        ) {
            return;
        }
        let Some(Res::Dot(DotRes::Methods { candidates })) = self.res.get(name.span) else {
            return;
        };
        let Some(val) = self.analyzed.checked.expr_types.get(&recv.span()) else {
            return;
        };
        let [only] = self.analyzed.decls.universe.members(val.ty) else {
            return;
        };
        // the instance declaration and the body's checking mint their own
        // variable atoms; the written parameter name bridges them
        let Some(param) = peeled_param(&self.analyzed.decls.universe, *only) else {
            return;
        };
        let Some(traits) = self.bounds.get(&param) else {
            return;
        };
        for &method in candidates {
            let MethodOwner::Trait(def) = &self.analyzed.package.method(method).owner else {
                continue;
            };
            let Some(&trait_id) = self.analyzed.decls.traits.get(def) else {
                continue;
            };
            if traits.contains(&trait_id) && self.seen.insert((param.clone(), method)) {
                self.out.push(RuntimePair {
                    var: param.clone(),
                    method,
                });
            }
        }
    }

    /// One interpolation hole whose implicit `show` dispatch resolves
    /// through this frame's runtime evidence [F6, H3]: the checker's
    /// hole record names the trait member and the operand's checked
    /// type peels the frame variable the dispatch's slot lookup reads —
    /// the same demand a written bound dot-call collects. A solved
    /// instance or an inherent owner records beside the hole record and
    /// stages its own words, so those demand nothing.
    fn hole_demand(&mut self, span: Span, recv: Span) {
        let Some(&method) = self.analyzed.checked.hole_show_dispatches.get(&span) else {
            return;
        };
        if self.analyzed.checked.trait_calls.contains_key(&span)
            || self.analyzed.checked.inherent_calls.contains_key(&span)
        {
            return;
        }
        let Some(val) = self.analyzed.checked.expr_types.get(&recv) else {
            return;
        };
        let [only] = self.analyzed.decls.universe.members(val.ty) else {
            return;
        };
        let Some(param) = peeled_param(&self.analyzed.decls.universe, *only) else {
            return;
        };
        let MethodOwner::Trait(def) = &self.analyzed.package.method(method).owner else {
            return;
        };
        let Some(&trait_id) = self.analyzed.decls.traits.get(def) else {
            return;
        };
        if self
            .bounds
            .get(&param)
            .is_some_and(|traits| traits.contains(&trait_id))
            && self.seen.insert((param.clone(), method))
        {
            self.out.push(RuntimePair { var: param, method });
        }
    }

    /// A call site entering a generic body — a free generic fn (named,
    /// or a UFCS dot-call feeding its first parameter) or a generic
    /// inherent impl's member — stages the callee's own runtime-evidence
    /// demands as the callee's trailing words. The checker's per-call
    /// record answers each demanded bound under this site's
    /// substitution, keyed by the callee's own variable name: a solved
    /// answer stages constants at the site, while a `None` answer (only
    /// an in-scope assumption covers it, so the substitution is this
    /// frame's own variable) reads this frame's evidence slots — the
    /// walk collects that demand here, keyed by the frame variable the
    /// staging's slot lookup peels. A trait dispatch site collects
    /// nothing here: the follow chain its entering site carries serves
    /// the transitive level.
    fn callee_demands(&mut self, callee: &Expr, call: Span, recv: Option<&Expr>) {
        let id = match callee {
            Expr::Field { name, .. } => match self.res.get(name.span) {
                Some(Res::Dot(DotRes::Free { func, .. })) => BodyId::Free(*func),
                // the checker-selected inherent member, when one won — the
                // type-keyed static call selects through the same record;
                // its receiver is no value, so a runtime-demand answer
                // (only a generic impl body has one) finds no variable and
                // contributes nothing, matching the checker's refusal
                Some(Res::Dot(DotRes::Methods { .. } | DotRes::Static { .. })) => {
                    let Some(&method) = self.analyzed.checked.inherent_calls.get(&call) else {
                        return;
                    };
                    let Some(&impl_site) = self.analyzed.decls.inherent_impls.get(&method) else {
                        return;
                    };
                    BodyId::Member {
                        impl_site,
                        member: self.analyzed.package.method(method).name.clone(),
                    }
                },
                _ => return,
            },
            Expr::Path { span, .. } => match self.res.get(*span) {
                Some(Res::Def(def)) => BodyId::Free(*def),
                _ => return,
            },
            _ => return,
        };
        let Some(pairs) = self.demands.get(&id) else {
            return;
        };
        for pair in pairs {
            let MethodOwner::Trait(def) = self.analyzed.package.method(pair.method).owner else {
                continue;
            };
            let Some(&trait_id) = self.analyzed.decls.traits.get(&def) else {
                continue;
            };
            let universe = &self.analyzed.decls.universe;
            // the runtime form's substituted type: the free fn's record
            // carries the solved type itself; the inherent record carries
            // none, and the receiver's checked type peels the same frame
            // variable the staging's slot lookup reads
            let solved = match &id {
                BodyId::Free(_) => {
                    let Some((_, _, served, solved)) = self
                        .analyzed
                        .checked
                        .generic_call_bounds
                        .get(&call)
                        .and_then(|record| {
                            record.iter().find(|(var, bound, ..)| {
                                crate::shape::var_param_name(universe, *var).as_deref()
                                    == Some(pair.var.as_str())
                                    && *bound == trait_id
                            })
                        })
                    else {
                        continue;
                    };
                    if served.is_some() {
                        continue;
                    }
                    *solved
                },
                BodyId::Member { .. } => {
                    let Some((_, _, served)) = self
                        .analyzed
                        .checked
                        .inherent_call_bounds
                        .get(&call)
                        .and_then(|record| {
                            record.iter().find(|(var, bound, _)| {
                                crate::shape::var_param_name(universe, *var).as_deref()
                                    == Some(pair.var.as_str())
                                    && *bound == trait_id
                            })
                        })
                    else {
                        continue;
                    };
                    if served.is_some() {
                        continue;
                    }
                    let Some(recv) = recv else {
                        continue;
                    };
                    let Some(val) = self.analyzed.checked.expr_types.get(&recv.span()) else {
                        continue;
                    };
                    val.ty
                },
                // a dispatch target reaches its default body through the
                // trait path, never a name
                BodyId::Default { .. } => return,
            };
            let [only] = universe.members(solved) else {
                continue;
            };
            let Some(param) = peeled_param(universe, *only) else {
                continue;
            };
            // the frame's declared bounds carry the trait (the checker
            // satisfied the callee's bound through one), mirroring the
            // walk's own dot-call collection
            if !self
                .bounds
                .get(&param)
                .is_some_and(|traits| traits.contains(&trait_id))
            {
                continue;
            }
            if self.seen.insert((param.clone(), pair.method)) {
                self.out.push(RuntimePair {
                    var: param,
                    method: pair.method,
                });
            }
        }
    }

    fn expr(&mut self, e: &Expr) {
        let block = |walk: &mut Self, block: &BlockExpr| {
            for stmt in &block.stmts {
                match &stmt.kind {
                    StmtKind::Local(local) => walk.expr(&local.init),
                    StmtKind::Expr(expr) => walk.expr(expr),
                }
            }
        };
        match e {
            Expr::Literal { .. }
            | Expr::RawStr { .. }
            | Expr::Break(_)
            | Expr::Continue(_)
            | Expr::FnType { .. }
            | Expr::Path { .. } => {},
            // a spawned block's demands ride its entry argument area,
            // staged from this frame's evidence slots at the spawn site,
            // so this frame must carry a slot for every demand lexically
            // inside it, transitively
            Expr::Spawn { body, .. } => block(self, body),
            // a value-site lambda's body sites demand evidence too: the
            // construction stages the words from this frame's slots into
            // the capture block, so this frame must carry a slot for
            // every demand lexically inside the lambda, transitively
            Expr::Lambda { body, .. } => self.expr(body),
            Expr::Str { parts, .. } => {
                for part in parts {
                    match part {
                        StrPart::Text { .. } => {},
                        StrPart::Expr { span, expr } => {
                            self.hole_demand(*span, expr.span());
                            self.expr(expr);
                        },
                        StrPart::Ident { span, name } => self.hole_demand(*span, name.span),
                    }
                }
            },
            Expr::Paren { inner, .. }
            | Expr::Try { expr: inner, .. }
            | Expr::Unary { expr: inner, .. } => self.expr(inner),
            Expr::Block(inner) => block(self, inner),
            Expr::Struct { fields, .. } => {
                for (_, value) in fields {
                    if let Some(value) = value {
                        self.expr(value);
                    }
                }
            },
            Expr::Tuple { items, .. } | Expr::List { items, .. } => {
                for item in items {
                    self.expr(item);
                }
            },
            Expr::Call {
                span,
                callee,
                args,
                trailing,
                ..
            } => {
                if let Expr::Field { recv, .. } = &**callee {
                    self.call(callee, recv);
                    self.callee_demands(callee, *span, Some(recv));
                    self.expr(recv);
                } else {
                    self.callee_demands(callee, *span, None);
                    self.expr(callee);
                }
                for arg in args {
                    self.expr(&arg.value);
                }
                if let Some(block_arg) = trailing {
                    self.expr(block_arg);
                }
            },
            Expr::Field { recv, .. } => self.expr(recv),
            Expr::Index { recv, index, .. } => {
                self.expr(recv);
                self.expr(index);
            },
            Expr::MapApply { key, value, .. } => {
                self.expr(key);
                self.expr(value);
            },
            Expr::Is { expr, .. } => self.expr(expr),
            Expr::Binary { left, right, .. } | Expr::Assign { left, right, .. } => {
                self.expr(left);
                self.expr(right);
            },
            Expr::Range { start, end, .. } => {
                self.expr(start);
                self.expr(end);
            },
            Expr::If {
                cond, then, else_, ..
            } => {
                self.expr(cond);
                block(self, then);
                if let Some(else_expr) = else_ {
                    self.expr(else_expr);
                }
            },
            Expr::Match {
                scrutinee, arms, ..
            } => {
                self.expr(scrutinee);
                for arm in arms {
                    if let Some(guard) = &arm.guard {
                        self.expr(guard);
                    }
                    self.expr(&arm.body);
                }
            },
            Expr::While { cond, body, .. } => {
                self.expr(cond);
                block(self, body);
            },
            Expr::For { iter, body, .. } => {
                self.expr(iter);
                self.expr(body);
            },
            Expr::Return { expr, .. } => {
                if let Some(expr) = expr {
                    self.expr(expr);
                }
            },
            Expr::Nursery { body, .. } | Expr::Defer { body, .. } => {
                block(self, body);
            },
        }
    }
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
    /// arguments [H3].
    Generic(&'static GenericNative),
    /// This module's function-table index.
    Local(u32),
    /// A linked package's function (G4): the def carries the import
    /// surface entry — its checked fn type serves the parameter reads
    /// exactly like a planned def's — and the FuncRef addresses the
    /// linked dep position and the linked module's function index. The
    /// call stages like a local's and a tail site takes the same CALLT
    /// frame-reuse transfer (H2: the module boundary rides the pool
    /// constant alone, the slide a pure fact of the two layouts).
    Foreign {
        /// The import-bound def: the signature's checker facts.
        def: DefId,
        /// The linked dep entry's position in the recorded dep list.
        position: u32,
        /// The function's index inside the linked module's table.
        func: u32,
    },
    /// A runtime-resolved trait dispatch inside a generic impl body: the
    /// frame's evidence slot whose word names the serving member body in
    /// the enclosing module (the sentinel FuncRef the CALLG carries).
    Evidence(u8),
    /// A bound extern's C symbol (H5): the def carries the declaration's
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
    /// the positions that must prove scalar — collection elements, struct
    /// fields, lambda captures — refuse those.
    scalar_fn_words: HashSet<u8>,
    /// Name scopes, innermost last: name -> binder span.
    scopes: Vec<HashMap<String, Span>>,
    /// Overlay word usage: index -> word types seen there.
    overlay: Vec<HashSet<SlotType>>,
    overlay_len: usize,
    /// The scalar destination offer live for one expression evaluation.
    scalar_dst: Option<ScalarDst>,
    /// The Perceus dup/drop placement over this function's walk (C4).
    rc: Placement,
    /// This body's runtime-evidence pairs to their frame slots: a nested
    /// trait-method call on a bound type variable dispatches through the
    /// word at its slot (empty outside generic bodies and their
    /// lambdas).
    evidence: HashMap<(String, MethodId), u8>,
    /// The declared bounds of the generic body this frame lowers (an
    /// impl member's or a free generic fn's), or the ones the enclosing
    /// generic body carried when this frame is one of its lambdas: a
    /// lambda's own evidence demands collect against them at its
    /// construction.
    bounds: Rc<HashMap<String, HashSet<TraitId>>>,
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
    /// The innermost open nurseries' id words [E2], innermost last: the
    /// `spawn` sites of one `nursery` block stage the top word.
    nurseries: Vec<u8>,
    /// Whether this frame is a `spawn` expression's child [E2]: the
    /// parent's spawn site owns the `cancel`-payload containment checks
    /// against the nursery-owning function's return union, so the child's
    /// own lowering skips them.
    spawn_child: bool,
    /// The frame's canonical unit word: allocated at its first use, its
    /// single `Kimm 0` spliced at the frame top in `finalize`. Every unit
    /// value in the frame is this one place.
    unit_slot: Option<u8>,
    /// `main`'s per-statement value ends (the resumed-frame seam): the
    /// code length after each direct body statement's value code, before
    /// its discards, plus the tail's exit boundary — recorded only for
    /// `main`, rebased through `finalize`'s movements.
    value_ends: Vec<u32>,
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
    ) -> Result<(ds_mod::Function, Vec<PlacedOp>, crate::FnFacts), LowerError> {
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
        // typed parameters — so those members refuse here, honestly,
        // until the representation ruling that names their word shape.
        // A free generic fn's body is the exception: its parameters and
        // returns over the fn's own type variables are the Raw-word
        // shape the entering call sites guard at their one-word
        // instantiation boundary.
        if let Some(decl) = &planned.decl
            && !planned.free_generic
        {
            for param in decl.params.iter() {
                let Some(fact) = analyzed.checked.binders.get(&param.name.span) else {
                    continue;
                };
                if let [only] = analyzed.decls.universe.members(fact.ty)
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
            let Some((_, ret)) = fn_sig_of(&analyzed.decls.universe, fn_ty) else {
                return Err(LowerError::unsupported(
                    planned.lambda.span(),
                    "a non-function binding",
                    "planned functions must be const-arrow lambdas",
                ));
            };
            ret
        };
        if planned.decl.is_some()
            && !planned.free_generic
            && let [only] = analyzed.decls.universe.members(ret)
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
            bounds: planned.bounds.clone(),
            index,
            loops: Vec::new(),
            loop_spans: Vec::new(),
            nurseries: Vec::new(),
            spawn_child: planned.spawn_child,
            unit_slot: None,
            value_ends: Vec::new(),
        };
        if planned.deinit.is_some() {
            this.bind_deinit_param(params)?;
        } else if planned.spawn_child {
            // a spawned child has no declared parameters: its capture
            // block IS the entry's argument area, laid out from the frame
            // start exactly where the spawned fiber's entry frame writes
            // it — the spawn site staged the evidence words into that
            // same block, so they bind as the entry area's trailing
            // parameters
            this.bind_captures(&planned.captures)?;
            this.params_len = this.slots.len();
            this.bind_evidence(&planned.evidence)?;
        } else {
            this.bind_params(params)?;
            this.bind_captures(&planned.captures)?;
            if planned.decl.is_some() {
                this.bind_evidence(&planned.evidence)?;
            } else {
                // a lambda's evidence words ride its capture block: their
                // slots bind right after the real captures, exactly where
                // the CALLS copy-in writes them
                this.bind_capture_evidence(&planned.evidence)?;
            }
        }
        this.tail(body)?;
        this.finalize()
    }

    // ---- frame and buffer --------------------------------------------------

    /// The checked `Val` recorded at one span.
    fn val(&self, span: Span) -> Result<Val, LowerError> {
        self.analyzed
            .checked
            .expr_types
            .get(&span)
            .copied()
            .ok_or_else(|| LowerError::unsupported(span, "an expression", "it has no checked type"))
    }

    fn members(&self, ty: Ty) -> &'a [TypeId] {
        self.analyzed.decls.universe.members(ty)
    }

    /// Allocates body slots, returning the base.
    fn alloc(&mut self, words: &[SlotType]) -> Result<u8, LowerError> {
        let base = self.slots.len();
        self.slots.extend_from_slice(words);
        if self.slots.len() > usize::from(u8::MAX) {
            return Err(LowerError::FrameTooWide {
                name: self.name.clone(),
                need: self.slots.len(),
            });
        }
        Ok(u8::try_from(base).expect("guarded by the FrameTooWide check"))
    }

    fn temp_word(&mut self, slot: SlotType) -> Result<u8, LowerError> {
        self.alloc(&[slot])
    }

    /// A span temp holding one solved type.
    fn temp_span(&mut self, ty: Ty) -> Result<Place, LowerError> {
        let words = self.shapes.words(ty)?;
        let base = self.alloc(&words)?;
        Ok(Place {
            base,
            shape: PlaceShape::Span(ty),
        })
    }

    /// The canonical place shape of one solved type at `base`: scalars are
    /// bare words, everything else a span (the same normalization params
    /// and call results get).
    fn place_of_ty(&mut self, base: u8, ty: Ty) -> Result<Place, LowerError> {
        let shape = match &*self.shapes.shape(ty)? {
            Shape::Word(slot) => PlaceShape::Word(*slot),
            _ => PlaceShape::Span(ty),
        };
        Ok(Place { base, shape })
    }

    fn emit(&mut self, op: Op) {
        self.code.push(op);
    }

    /// One KSTR load into the body word `dst`: the word starts in the
    /// borrow classification and keeps it unless a count-moving site
    /// transits it before finalization (the placement pass owns the
    /// proof; the rewrite happens in `finalize`).
    fn kstr(&mut self, dst: u8, kidx: u16) {
        self.emit(Op::Kstr {
            dst: Slot::body(dst),
            kidx,
        });
        self.rc.produced_kstr(dst);
    }

    fn label(&mut self) -> usize {
        self.labels.push(usize::MAX);
        self.labels.len() - 1
    }

    fn bind_label(&mut self, label: usize) {
        self.labels[label] = self.code.len();
    }

    /// Splices `ops` into the buffer at `at`: branch arms lower before
    /// the join's dying sets are known, so an arm's epilogue decs move in
    /// afterwards, right before the arm's jump to the join point. Labels
    /// bound past the splice shift with it; a label entering at the
    /// splice position runs the epilogue first — its path stays on the
    /// same arm.
    fn splice(&mut self, ops: Vec<Op>, at: usize) {
        let len = ops.len();
        if len == 0 {
            return;
        }
        self.code.splice(at..at, ops);
        for pos in &mut self.labels {
            if *pos != usize::MAX && *pos > at {
                *pos += len;
            }
        }
        for span in &mut self.loop_spans {
            if span.head > at {
                span.head += len;
            }
            if span.back != usize::MAX && span.back > at {
                span.back += len;
            }
        }
    }

    /// Records the innermost open loop's back edge at the next emission
    /// position. The spans persist through the walk (finalize pairs marks
    /// into the loops enclosing designated concatenations), so the
    /// innermost open one is the last span whose back edge is not yet
    /// recorded - inner loops close before their enclosing back edge
    /// runs.
    fn loop_back_edge(&mut self) {
        let at = self.code.len();
        self.loop_spans
            .iter_mut()
            .rev()
            .find(|span| span.back == usize::MAX)
            .expect("a loop is open")
            .back = at;
    }

    /// One fall-through arm's epilogue: the words the join does not keep
    /// die inside their arm, spliced at the arm's code end (the other
    /// arm jumped past it).
    fn arm_epilogue(&mut self, dying: &[(u8, Owned)]) {
        let mut epilogue = Vec::new();
        Placement::emit_drops(&mut epilogue, dying);
        let at = self.code.len();
        self.splice(epilogue, at);
    }

    /// Records one call site's overlay usage: the staged argument word
    /// types and the result words that land there.
    fn overlay_site(&mut self, staged: &[SlotType], result: &[SlotType]) {
        self.overlay_len = self.overlay_len.max(staged.len()).max(result.len());
        if self.overlay.len() < self.overlay_len {
            self.overlay.resize(self.overlay_len, HashSet::new());
        }
        for (index, word) in staged.iter().enumerate().chain(result.iter().enumerate()) {
            self.overlay[index].insert(*word);
        }
    }

    /// Adds one pool constant (deduplicated by value), returning its index.
    fn pool(&mut self, constant: Constant) -> u16 {
        if let Constant::Str(text) = constant {
            return self.pool_str(&text);
        }
        if let Some(&index) = self.pool_index.get(&constant) {
            return index;
        }
        let index = self.pool_len();
        self.pool_index.insert(constant.clone(), index);
        self.pool.push(constant);
        index
    }

    /// Adds one pool string constant (deduplicated by contents), returning
    /// its index.
    fn pool_str(&mut self, text: &str) -> u16 {
        if let Some(&index) = self.pool_strings.get(text) {
            return index;
        }
        let index = self.pool_len();
        self.pool_strings.insert(text.to_owned(), index);
        self.pool.push(Constant::Str(text.to_owned()));
        index
    }

    fn pool_len(&self) -> u16 {
        u16::try_from(self.pool.len()).expect("pool index fits u16")
    }

    /// The Ref-typed body words of one value: where a value of `ty` at
    /// `base` carries heap references (the words the RC pass tracks).
    fn ref_words(&mut self, base: u8, ty: Ty) -> Result<Vec<u8>, LowerError> {
        let words = self.shapes.words(ty)?;
        Ok(words
            .iter()
            .enumerate()
            .filter(|(_, word)| **word == SlotType::Ref)
            .map(|(index, _)| base + u8::try_from(index).expect("span width fits u8"))
            .collect())
    }

    /// The Ref words the placement tracks for one value: the place's own
    /// word classification leads, so a closure word (a fn-typed value in
    /// a Ref-classified word) is tracked where the type-derived list
    /// would miss it.
    fn tracked_words(&mut self, place: &Place, ty: Ty) -> Result<Vec<u8>, LowerError> {
        Ok(match place.shape {
            PlaceShape::Word(SlotType::Ref) => vec![place.base],
            _ if self.is_var_ty(ty) => vec![place.base],
            _ => self.count_words(place.base, ty)?,
        })
    }

    /// Whether a type is exactly one of the checker's minted type
    /// variables: its single value word is Raw and its runtime kind is
    /// the caller's fact.
    fn is_var_ty(&self, ty: Ty) -> bool {
        match self.members(ty) {
            [only] => self.shapes.is_var_atom(*only),
            _ => false,
        }
    }

    /// The words the RC pass tracks for one value at `base`: its Ref
    /// words plus every Raw word — a type variable's own single word or
    /// a variable-keyed union member's region claim, whose runtime kind
    /// is the caller's fact. The VM's RC ops check the runtime kind, so
    /// a scalar instantiation decs nothing and a reference
    /// instantiation stays balanced.
    fn count_words(&mut self, base: u8, ty: Ty) -> Result<Vec<u8>, LowerError> {
        let words = self.shapes.words(ty)?;
        Ok(words
            .iter()
            .enumerate()
            .filter(|(_, word)| **word == SlotType::Ref || **word == SlotType::Raw)
            .map(|(index, _)| base + u8::try_from(index).expect("span width fits u8"))
            .collect())
    }

    /// The establishment band word of one claim site: a fresh Tag body
    /// word at the first registration, the site's own at a
    /// re-registration (a literal's temp re-bound at its binder writes
    /// the same band once).
    fn claim_band_word(&mut self, base: u8) -> Result<u8, LowerError> {
        match self.rc.claim_band(base) {
            Some(band) => Ok(band),
            None => self.temp_word(SlotType::Tag),
        }
    }

    /// The words a discarded value must account for: its tracked Ref
    /// words plus the deinit claim's base when the value carries one —
    /// a deinit value dying unnamed at a statement still runs its hook.
    fn discard_words(&mut self, place: &Place, ty: Ty, span: Span) -> Result<Vec<u8>, LowerError> {
        let mut words = self.tracked_words(place, ty)?;
        if let Some(claim) = self.deinit_claim(ty, span)?
            && self.rc.anchors_claim(place.base)
        {
            self.overlay_site(&claim.staged, &[SlotType::I64]);
            let band = self.claim_band_word(place.base)?;
            self.rc
                .claimed(&mut self.code, place.base, claim.hook, band, &words);
            if !words.contains(&place.base) {
                words.insert(0, place.base);
            }
        }
        Ok(words)
    }

    /// Registers an owning binder with the RC pass: its tracked words
    /// (the place's own classification leads, so a closure word behind a
    /// fn-typed binding is owned by its binder — its dec fires at the
    /// checker-recorded last use, and a never-consumed word drops at its
    /// scope's end) plus the deinit claim its concrete type carries (C5)
    /// — the value's base word anchors the hook, and the frame's overlay
    /// grows to hold the hook's staging.
    fn rc_bind(&mut self, binder: Span, place: &Place, ty: Ty) -> Result<(), LowerError> {
        let mut words = self.tracked_words(place, ty)?;
        if let Some(claim) = self.deinit_claim(ty, binder)?
            && self.rc.anchors_claim(place.base)
        {
            self.overlay_site(&claim.staged, &[SlotType::I64]);
            let band = self.claim_band_word(place.base)?;
            self.rc
                .claimed(&mut self.code, place.base, claim.hook, band, &words);
            if !words.contains(&place.base) {
                words.insert(0, place.base);
            }
        }
        // a binder aliasing a borrowed word (a parameter, a member
        // alias) keeps no claim here: the word never anchors one, the
        // owner's own drop site firing the hook once
        self.rc.bound(binder, &words);
        Ok(())
    }

    /// Registers a borrowing binder (a parameter, a handler's member
    /// alias, a pattern field): its Ref words only. A borrow never owns
    /// the value's deinit — the owning frame's drop site fires the
    /// hook — so the base word joins the borrowed set and a later
    /// binder aliasing it (a const alias, a match binder) never
    /// re-establishes one.
    fn rc_bind_borrowed(&mut self, binder: Span, base: u8, ty: Ty) -> Result<(), LowerError> {
        let words = self.count_words(base, ty)?;
        self.rc.bound_borrowed(binder, base, &words);
        Ok(())
    }

    /// The deinit claim one value of `ty` carries: the synthesized
    /// deinit's pool constant, the word span it stages at the death, and
    /// the span's `Ref` word offsets (the claim's decs). `None` for
    /// types without a declared deinit.
    fn deinit_claim(&mut self, ty: Ty, span: Span) -> Result<Option<DeinitClaim>, LowerError> {
        let Some(hook) = self.deinit_hook(ty, span)? else {
            return Ok(None);
        };
        let staged = self.shapes.words(ty)?;
        Ok(Some(DeinitClaim { hook, staged }))
    }

    /// The deinit hook of a value's concrete type (C5): a single struct
    /// atom whose declaration carries a `deinit` member names its
    /// synthesized function through the pool. A declared deinit with no
    /// plan (a generic struct's) refuses; unions, scalars, and
    /// deinit-less structs carry none.
    fn deinit_hook(&mut self, ty: Ty, span: Span) -> Result<Option<Hook>, LowerError> {
        let members = self.members(ty);
        let [atom] = members else {
            return Ok(None);
        };
        let Some(def) = self.analyzed.decls.defs_of_atoms.get(atom).copied() else {
            return Ok(None);
        };
        if !self.plan.declares_deinit(def) {
            return Ok(None);
        }
        let Some(index) = self.plan.deinit_index(def) else {
            return Err(LowerError::unsupported(
                span,
                "a deinit-carrying value",
                "the struct's deinit is generic; monomorphic instantiation is the CALL_GENERIC \
                 seam",
            ));
        };
        let pool = self.pool(Constant::FuncRef {
            key: FuncRefKey::Dep(0), // the enclosing module
            func: index,
        });
        let staged = self.shapes.words(ty)?;
        let width = u8::try_from(staged.len()).expect("span width fits u8");
        // the claim decs every Ref word of the span at the value's death
        let refs: Vec<u8> = staged
            .iter()
            .enumerate()
            .filter(|(_, slot)| **slot == SlotType::Ref)
            .map(|(offset, _)| u8::try_from(offset).expect("span offset fits u8"))
            .collect();
        Ok(Some(Hook {
            pool,
            width,
            refs: refs.into(),
        }))
    }

    // ---- params and scopes ---------------------------------------------------

    fn bind_params(&mut self, params: &[Param]) -> Result<(), LowerError> {
        for param in params {
            use ds_parse::ParamMode;
            match param.mode {
                // a comptime parameter binds only inside the evaluator's
                // slice: the fenced run stages one reflection-index Tag
                // word per parameter and the body reads it like any
                // borrowed word. The runtime module never plans a comptime
                // function (its calls embed their answers), so this arm
                // refusing at the runtime seam means a comptime parameter
                // reached a surface the round does not lower — an impl or
                // trait member, the checker's refusal class.
                ParamMode::Comptime(_) if matches!(self.comptime, ComptimeRun::Slice) => {},
                ParamMode::Comptime(_) => {
                    return Err(LowerError::unsupported(
                        param.span,
                        "a comptime parameter",
                        "a comptime parameter is a free-fn surface; the evaluator compiles the \
                         free fn's body, so spell the member without one",
                    ));
                },
                // a mut parameter is a re-storable slot over the borrowed
                // staging: the assignment machinery re-stores its words
                // (store_over decs the incoming count the caller handed),
                // so the only seam is the var-typed word whose count the
                // overwrite contract cannot see yet
                ParamMode::Mut(_) if self.param_is_var_ty(param) => {
                    return Err(LowerError::unsupported(
                        param.span,
                        "a mut parameter over a type variable",
                        "a reassignment would drop the previous word's count by runtime kind, and \
                         the placement has no overwrite contract for it yet",
                    ));
                },
                _ => {},
            }
            let Some(fact) = self.analyzed.checked.binders.get(&param.name.span).copied() else {
                return Err(LowerError::unsupported(
                    param.name.span,
                    "a parameter",
                    "it has no checked type",
                ));
            };
            let words = self
                .shapes
                .words(fact.ty)
                .map_err(|err| err.respanned(param.name.span))?;
            let base = self.alloc(&words)?;
            let shape = match &*self.shapes.shape(fact.ty)? {
                Shape::Word(slot) => PlaceShape::Word(*slot),
                _ => PlaceShape::Span(fact.ty),
            };
            let place = Place { base, shape };
            self.binders.insert(param.name.span, place.clone());
            // every parameter owns its incoming reference: the caller
            // handed a count (dup or forward at its staging) and this
            // frame's drops balance it (C2/C4)
            for word in self.count_words(base, fact.ty)? {
                self.rc.produced(word);
            }
            match param.mode {
                // a consuming parameter owns the value's death: the
                // deinit claim its type carries registers here, so the
                // hook fires at this frame's own death sites (the scope
                // end, an assignment's replaced content, a re-consuming
                // move) — the caller's staging spent its claim riding
                // the call
                ParamMode::Consume(_) => self.rc_bind(param.name.span, &place, fact.ty)?,
                _ => self.rc_bind_borrowed(param.name.span, base, fact.ty)?,
            }
            self.scopes
                .last_mut()
                .expect("the root scope exists")
                .insert(param.name.name.clone(), param.name.span);
            self.params_len += words.len();
        }
        Ok(())
    }

    /// Whether one parameter's checked type is a generic impl's type
    /// variable (the mut-window seam the var-binding refusal mirrors).
    fn param_is_var_ty(&self, param: &Param) -> bool {
        self.analyzed
            .checked
            .binders
            .get(&param.name.span)
            .copied()
            .is_some_and(|fact| self.is_var_ty(fact.ty))
    }

    /// Binds one generic impl member body's runtime-evidence slots: one
    /// I64 parameter word per recorded pair, appended after the declared
    /// parameters. The call site entering this body stages the words —
    /// one evidence-chain object index per pair, under its own
    /// substitution; the nested sites dispatch through them.
    fn bind_evidence(&mut self, pairs: &[RuntimePair]) -> Result<(), LowerError> {
        for pair in pairs {
            let slot = self.temp_word(SlotType::I64)?;
            self.params_len += 1;
            self.evidence.insert((pair.var.clone(), pair.method), slot);
        }
        Ok(())
    }

    /// Binds one lambda's runtime-evidence words as the trailing
    /// capture words: the construction site staged them from the
    /// enclosing frame's evidence slots, and the CALLS copy-in writes
    /// them right after the real captures — one word per pair, read back
    /// by the pair's dispatch. They are plain I64 words — never counted,
    /// never a binder.
    fn bind_capture_evidence(&mut self, pairs: &[RuntimePair]) -> Result<(), LowerError> {
        for pair in pairs {
            let slot = self.alloc(&[SlotType::I64; 1])?;
            self.evidence.insert((pair.var.clone(), pair.method), slot);
        }
        Ok(())
    }

    /// Binds a deinit function's `self` parameter: the dying instance,
    /// borrowed. The caller owns the count and the hook — the drop site
    /// stages the value and decs after the call — so this frame tracks
    /// no ownership on the parameter's words: borrows inside the body
    /// read them and nothing decs at this frame's exit.
    fn bind_deinit_param(&mut self, params: &[Param]) -> Result<(), LowerError> {
        let [param] = params else {
            return Err(mismatch("a deinit's parameter"));
        };
        let Some(fact) = self.analyzed.checked.binders.get(&param.name.span).copied() else {
            return Err(LowerError::unsupported(
                param.name.span,
                "a deinit's `self`",
                "it has no checked type",
            ));
        };
        let words = self
            .shapes
            .words(fact.ty)
            .map_err(|err| err.respanned(param.name.span))?;
        let base = self.alloc(&words)?;
        let shape = match &*self.shapes.shape(fact.ty)? {
            Shape::Word(slot) => PlaceShape::Word(*slot),
            _ => PlaceShape::Span(fact.ty),
        };
        self.binders.insert(param.name.span, Place { base, shape });
        self.params_len = words.len();
        self.scopes
            .last_mut()
            .expect("the root scope exists")
            .insert(param.name.name.clone(), param.name.span);
        Ok(())
    }

    /// Binds one synthesized lambda's capture block right after the
    /// params — exactly where the VM's CALLS writes it — one binder per
    /// captured enclosing binding, in the construction's capture order
    /// (F2). Each Ref word is a count the call acquired at its copy-in;
    /// the frame owns it for the call's duration and its exit drops
    /// balance the acquire, precisely a borrow parameter's story.
    fn bind_captures(&mut self, captures: &[(Span, Ty)]) -> Result<(), LowerError> {
        for &(bind, ty) in captures {
            let words = self.shapes.words(ty).map_err(|err| err.respanned(bind))?;
            let base = self.alloc(&words)?;
            let shape = match &*self.shapes.shape(ty)? {
                Shape::Word(slot) => PlaceShape::Word(*slot),
                _ => PlaceShape::Span(ty),
            };
            self.binders.insert(bind, Place { base, shape });
            for word in self.count_words(base, ty)? {
                self.rc.produced(word);
            }
            self.rc_bind_borrowed(bind, base, ty)?;
        }
        Ok(())
    }

    fn scope_push(&mut self) {
        self.scopes.push(HashMap::new());
    }

    /// Pops one name scope: the leaving binders' still-owned references
    /// drop here, in source order (the checker's deinit discipline).
    fn scope_pop(&mut self) {
        let Some(leaving) = self.scopes.pop() else {
            return;
        };
        let mut spans: Vec<Span> = leaving.into_values().collect();
        spans.sort_by(|a, b| {
            (a.file.as_u32(), a.start, a.end).cmp(&(b.file.as_u32(), b.start, b.end))
        });
        for span in spans {
            self.rc.scope_end(&mut self.code, span);
        }
    }

    fn scope_insert(&mut self, name: &str, span: Span) {
        self.scopes
            .last_mut()
            .expect("the root scope exists")
            .insert(name.to_owned(), span);
    }

    fn scope_lookup(&self, name: &str) -> Option<Span> {
        self.scopes.iter().rev().find_map(|s| s.get(name).copied())
    }

    // ---- word addressing ------------------------------------------------------

    /// The atom of a single-atom type.
    fn only_atom(&self, ty: Ty, span: Span) -> Result<TypeId, LowerError> {
        match self.members(ty) {
            [only] => Ok(*only),
            _ => Err(LowerError::unsupported(
                span,
                "a value of a multi-atom type",
                "this lowering point needs one member; split the value with a match",
            )),
        }
    }

    /// The runtime words of `ty`'s value inside `place`. A refined
    /// single-atom use of a union span addresses only that member's
    /// payload words.
    fn word_addrs(&mut self, place: &Place, ty: Ty) -> Result<Vec<WordAddr>, LowerError> {
        let use_shape = self.shapes.shape(ty)?;
        match (&*use_shape, &place.shape) {
            (Shape::Word(slot), PlaceShape::Word(classified)) => Ok(vec![WordAddr {
                name: None,
                slot: place.base,
                // the place's own classification leads when the two
                // disagree: a closure word is Ref-classified behind a
                // fn-typed use (F2), and the staged word's kind must tell
                // the truth for the call slide and the borrow placement
                slot_ty: if *slot == SlotType::Fn && *classified == SlotType::Ref {
                    SlotType::Ref
                } else {
                    *slot
                },
            }]),
            (Shape::Word(slot), PlaceShape::Span(underlying)) => {
                let atom = self.only_atom(ty, no_span())?;
                // a single-member span holds the member's own words, so
                // a word-shaped member sits at the span base itself
                if self.members(*underlying) == [atom] {
                    return Ok(vec![WordAddr {
                        name: None,
                        slot: place.base,
                        slot_ty: *slot,
                    }]);
                }
                let layout = self.shapes.shape(*underlying)?;
                let fields = layout
                    .member_fields(atom)
                    .ok_or_else(|| mismatch("a union member read"))?;
                match fields {
                    [] => Ok(vec![WordAddr {
                        name: None,
                        slot: place.base,
                        slot_ty: *slot,
                    }]),
                    [only] => Ok(vec![WordAddr {
                        name: None,
                        slot: place.base + only.word,
                        slot_ty: *slot,
                    }]),
                    _ => Err(mismatch("a multi-word member payload")),
                }
            },
            (Shape::Struct(s), PlaceShape::Span(underlying)) => {
                let place_shape = self.shapes.shape(*underlying)?;
                if matches!(&*place_shape, Shape::Struct(_)) {
                    // the same bare struct: words in declaration order,
                    // one address per word of each field's span
                    return Ok(s
                        .fields
                        .iter()
                        .flat_map(|f| field_addrs(place.base, f))
                        .collect());
                }
                let atom = self.only_atom(ty, no_span())?;
                let fields = place_shape
                    .member_fields(atom)
                    .ok_or_else(|| mismatch("a union member field"))?;
                let mut out = Vec::new();
                for (index, field) in s.fields.iter().enumerate() {
                    // a positional field (a tuple-payload case's) matches
                    // by position — the None names all compare equal, so
                    // the name-find would return the first field for
                    // every element
                    let member = if field.name.is_some() {
                        fields
                            .iter()
                            .find(|f| f.name == field.name)
                            .ok_or_else(|| mismatch("a union member field"))?
                    } else {
                        fields
                            .get(index)
                            .ok_or_else(|| mismatch("a union member field"))?
                    };
                    out.extend(field_addrs(place.base, member));
                }
                Ok(out)
            },
            (Shape::Tuple(s), PlaceShape::Span(underlying)) => {
                let place_shape = self.shapes.shape(*underlying)?;
                if matches!(&*place_shape, Shape::Tuple(_)) {
                    return Ok(s
                        .fields
                        .iter()
                        .flat_map(|f| field_addrs(place.base, f))
                        .collect());
                }
                // a tuple member inside a union region: the elements
                // gather from the member's claimed, possibly scattered
                // offsets — positionally, the tuple's own field map has
                // no names to match by
                let atom = self.only_atom(ty, no_span())?;
                let fields = place_shape
                    .member_fields(atom)
                    .ok_or_else(|| mismatch("a tuple member read"))?;
                if fields.len() != s.fields.len() {
                    return Err(mismatch("a tuple member's arity"));
                }
                Ok(s.fields
                    .iter()
                    .zip(fields.iter())
                    .flat_map(|(_, member)| field_addrs(place.base, member))
                    .collect())
            },
            (Shape::Union(u), PlaceShape::Span(underlying)) if *underlying == ty => Ok(u
                .words
                .iter()
                .enumerate()
                .map(|(index, slot)| WordAddr {
                    name: None,
                    slot: place.base + u8::try_from(index).expect("span width fits u8"),
                    slot_ty: *slot,
                })
                .collect()),
            (Shape::Union(_), PlaceShape::Span(underlying)) => {
                // a subset union: the value keeps the span's full layout
                let words = self.shapes.words(*underlying)?;
                Ok(words
                    .iter()
                    .enumerate()
                    .map(|(index, slot)| WordAddr {
                        name: None,
                        slot: place.base + u8::try_from(index).expect("span width fits u8"),
                        slot_ty: *slot,
                    })
                    .collect())
            },
            _ => Err(mismatch("this value's representation")),
        }
    }

    /// Moves one field's whole word span from `srcs` (the source value's
    /// word addresses, in span order) into the words at `base +
    /// field.word`: a Mov per word, an RC copy per counted word (Ref, or
    /// Raw — a variable-keyed member's claim, whose runtime kind the RC
    /// ops check).
    fn store_field_span(&mut self, base: u8, field: &MemberField, srcs: &[WordAddr]) {
        for (offset, addr) in srcs.iter().enumerate() {
            let word = base + field.word + u8::try_from(offset).expect("field span offset fits u8");
            self.emit(Op::Mov {
                dst: Slot::body(word),
                src: Slot::body(addr.slot),
            });
            if matches!(addr.slot_ty, SlotType::Ref | SlotType::Raw) {
                self.rc.copied(&mut self.code, word, addr.slot);
            }
        }
    }

    /// The tag word of a union place.
    fn tag_addr(&self, place: &Place) -> Result<u8, LowerError> {
        match &place.shape {
            PlaceShape::Span(ty) if self.is_span_ty(*ty) => Ok(place.base),
            PlaceShape::Word(SlotType::Tag) => Ok(place.base),
            _ => Err(mismatch("a tag word read")),
        }
    }

    fn is_span_ty(&self, ty: Ty) -> bool {
        self.members(ty).len() > 1
    }

    /// Whether a checked binder's type is a bare struct span: its
    /// runtime representation is its own field words, however many — a
    /// one-word struct still binds and reads as a span, never as a
    /// scalar word.
    fn struct_typed(&mut self, ty: Ty) -> bool {
        match self.shapes.shape(ty) {
            Ok(shape) => matches!(&*shape, Shape::Struct(_)),
            Err(_) => false,
        }
    }

    // ---- statements ------------------------------------------------------------

    /// Lowers one block's statements, returning the block's value.
    fn block_value(&mut self, block: &BlockExpr) -> Result<TypedPlace, LowerError> {
        let bracket = self.rc.staging_bracket();
        self.scope_push();
        let result = self.block_inner(block);
        // The tail answer's words escape to the consumer outside the
        // block, so they stage past the scope end: the block's tail read
        // of a local binder answers the binder's own words, and the
        // leaving scope would release their counts — and fire a
        // deinit value's claim — before any consumer claims them. The
        // staged-word computation is fallible, so its error waits past
        // the pop: every exit runs scope_pop and retires the staging
        // bracket, matching block_inner's own failure path.
        let staged = match &result {
            Ok(tp) => self.stage_tail_words(tp, block.span),
            Err(_) => Ok(()),
        };
        self.scope_pop();
        self.rc.unstage_tail_answer(bracket);
        staged?;
        result
    }

    /// Computes and stages the words the block's tail answer carries
    /// past its scope end: the tracked words plus, for a deinit-carrying
    /// answer whose base anchors the frame's claim, the claim's own
    /// base word.
    fn stage_tail_words(&mut self, tp: &TypedPlace, span: Span) -> Result<(), LowerError> {
        let mut words = self.tracked_words(&tp.place, tp.ty)?;
        if self.deinit_claim(tp.ty, span)?.is_some()
            && self.rc.anchors_claim(tp.place.base)
            && !words.contains(&tp.place.base)
        {
            words.push(tp.place.base);
        }
        self.rc.stage_tail_answer(&words);
        Ok(())
    }

    fn block_inner(&mut self, block: &BlockExpr) -> Result<TypedPlace, LowerError> {
        let (head, last) = match block.stmts.split_last() {
            Some((last, head)) => (head, Some(last)),
            None => (&block.stmts[..], None),
        };
        for stmt in head {
            self.stmt(stmt)?;
        }
        match last {
            Some(Stmt {
                kind: StmtKind::Expr(expr),
                ..
            }) => self.value(expr),
            Some(Stmt {
                kind: StmtKind::Local(local),
                ..
            }) => self.local_stmt(local),
            None => self.unit_value(block.span),
        }
    }

    /// The unit value: the frame's canonical unit word, one I64 slot
    /// matching the native unit convention. Unit words are idempotent
    /// and carry no counts, so every unit value in the frame shares the
    /// one place instead of materializing a fresh zero per use.
    fn unit_value(&mut self, span: Span) -> Result<TypedPlace, LowerError> {
        let ty = self.val(span).map_or(self.ret_ty, |v| v.ty);
        Ok(TypedPlace {
            place: self.canonical_unit()?,
            ty,
        })
    }

    /// The frame's one unit word: allocated at first use, zero-written by
    /// a single `Kimm 0` `finalize` splices at the frame top — the write
    /// dominates every later read (branch arms read the word on paths
    /// that never ran the first use, and frame words carry the previous
    /// frame's garbage), and no site ever writes the word afterwards, so
    /// one entry store serves the whole frame.
    fn canonical_unit(&mut self) -> Result<Place, LowerError> {
        if let Some(base) = self.unit_slot {
            return Ok(Place {
                base,
                shape: PlaceShape::Word(SlotType::I64),
            });
        }
        let base = self.temp_word(SlotType::I64)?;
        self.unit_slot = Some(base);
        Ok(Place {
            base,
            shape: PlaceShape::Word(SlotType::I64),
        })
    }

    /// The unit type `()`: the solved member set is exactly the arity-0
    /// tuple atom [B3] — distinct from the unit tags (`none`, `io_err`),
    /// which are Tag words, and from `int`, which shares the I64 slot.
    fn is_unit_ty(&self, ty: Ty) -> bool {
        let [atom] = self.members(ty) else {
            return false;
        };
        matches!(
            self.analyzed.decls.universe.atom_decl(*atom),
            Ok(decl)
                if matches!(&decl.kind, AtomKind::Tuple(elements) if elements.is_empty())
        )
    }

    /// The join span a value-position if/match widens its arms into: a
    /// fresh temp span, except a unit join reuses the canonical unit
    /// word — a statement if pays no result span and no per-arm Mov,
    /// because every arm's unit value is that same word, so each
    /// widen degenerates to the in-place no-op and the expression's
    /// place is the unit word itself.
    fn join_span(&mut self, ty: Ty) -> Result<Place, LowerError> {
        if self.is_unit_ty(ty) { self.canonical_unit() } else { self.temp_span(ty) }
    }

    fn stmt(&mut self, stmt: &Stmt) -> Result<(), LowerError> {
        match &stmt.kind {
            StmtKind::Local(local) => {
                self.local_stmt(local)?;
                self.note_value_end();
                Ok(())
            },
            StmtKind::Expr(expr) => {
                let tp = self.value(expr)?;
                self.note_value_end();
                // a statement-position value is discarded: its references
                // die at this use when nothing else keeps them
                let words = self.discard_words(&tp.place, tp.ty, expr.span())?;
                self.rc.discard(&mut self.code, &words);
                Ok(())
            },
        }
    }

    /// Records one body-level statement's value end (the resumed-frame
    /// seam): the resume target the REPL computes against the current
    /// round's own lowering, so the discarded statement's decs run at the
    /// next resume and its value never re-computes.
    fn note_value_end(&mut self) {
        if self.root && self.scopes.len() == 2 {
            self.value_ends
                .push(u32::try_from(self.code.len()).expect("code length fits u32"));
        }
    }

    /// One `const`/`var` binding statement.
    ///
    /// A `const` binder aliases its initializer's place (immutable, every
    /// use reads live slots). A `var` binder [C3] owns a dedicated span
    /// sized by its declared type; the initializer widens into it and every
    /// later assignment re-stores the same slots — the rebindable local
    /// keeps its slot span for the function's whole body, so the M4
    /// placement pass will see one stable storage per var.
    /// A deinit value stored in a union payload refuses at its own span
    /// (C5): the union's death decs the member's payload words and frees
    /// nested objects, but dispatches no user deinit — the member
    /// value's hook would never run.
    fn guard_deinit_union_member(
        &mut self,
        src_ty: Ty,
        dst_ty: Ty,
        span: Span,
    ) -> Result<(), LowerError> {
        if self.deinit_claim(src_ty, span)?.is_some() && self.members(dst_ty).len() > 1 {
            return Err(LowerError::unsupported(
                span,
                "a deinit-carrying union member",
                "the union's death decs the member's payload words and frees nested objects, but \
                 dispatches no user deinit; a deinit value's death is its own binding's",
            ));
        }
        Ok(())
    }

    fn local_stmt(&mut self, local: &LocalDecl) -> Result<TypedPlace, LowerError> {
        // A one-element paren pattern `(a)` is the inner pattern; the
        // parser keeps the tuple node and both layers collapse it.
        let pat = match &local.pat {
            Pattern::Tuple {
                pats,
                trailing_comma: false,
                ..
            } if pats.len() == 1 => &pats[0],
            other => other,
        };
        // A scalar var's int/float literal init writes the var's own
        // word directly: the constant has no other consumer, a scalar
        // word carries no count, and a literal trips neither of the
        // general path's closure/deinit guards, so its temp + re-store
        // reduces to the one constant load.
        if local.mutable
            && let Some(landed) = self.scalar_var_init(local, pat)?
        {
            return Ok(landed);
        }
        let init = self.value(&local.init)?;
        let declared = match pat {
            Pattern::Binding { span, .. } => self
                .analyzed
                .checked
                .binders
                .get(span)
                .map_or(init.ty, |f| f.ty),
            _ => init.ty,
        };
        let place = if local.mutable {
            if self.is_var_ty(declared) {
                return Err(LowerError::unsupported(
                    local.span,
                    "a var binding over a type variable",
                    "a reassignment would drop the previous word's count by runtime kind, and the \
                     placement has no overwrite contract for it yet",
                ));
            }
            self.refuse_closure_widen(&init, declared, local.span)?;
            self.guard_deinit_union_member(init.ty, declared, local.span)?;
            // A destructuring var owns one re-storable slot per
            // element: each element word copies into its own var
            // slot at the binding, so an assignment through an
            // element re-stores its slot alone — the const
            // convention aliases the tuple's words instead.
            if let Pattern::Tuple { pats, .. } = pat {
                self.var_tuple_slots(pats, &init, declared)?;
                init.place.clone()
            } else {
                let target = self.temp_span(declared)?;
                self.widen_into(&target, declared, &init, local.span)?;
                self.place_of_ty(target.base, declared)?
            }
        } else if declared != init.ty && self.is_union_ty(declared)? {
            // An annotated const over a narrower source claims the
            // destination union's region under its source member: the
            // binding must physically carry the union's tag, so a bare
            // member literal re-spans through the same construction the
            // if/match join uses instead of aliasing one word.
            let target = self.temp_span(declared)?;
            self.refuse_closure_init(&init, local.span)?;
            self.widen_into(&target, declared, &init, local.span)?;
            self.place_of_ty(target.base, declared)?
        } else {
            init.place.clone()
        };
        match pat {
            Pattern::Binding { span, name } => {
                self.guard_deinit_union_member(init.ty, declared, local.span)?;
                self.binders.insert(*span, place.clone());
                // the place's own word classification leads: a const
                // aliasing a closure word binds the Ref word the declared
                // fn type does not name, and the binder's death drops it
                self.rc_bind(*span, &place, declared)?;
                self.scope_insert(&name.name, *span);
            },
            Pattern::Tuple { pats, .. } => {
                // a mutable tuple already bound its per-element var
                // slots; the const convention aliases in place
                if !local.mutable {
                    self.bind_tuple_pattern(
                        pats.as_slice(),
                        &TypedPlace {
                            place: place.clone(),
                            ty: declared,
                        },
                        false,
                    )?;
                }
            },
            Pattern::Wildcard(_) => {
                // `const _ = e` discards: no binder names the value, so
                // its references die here when nothing else keeps them
                let words = self.discard_words(&place, declared, local.span)?;
                self.rc.discard(&mut self.code, &words);
            },
            _ => {
                return Err(LowerError::unsupported(
                    local.span,
                    "a destructuring binding pattern",
                    "plain names, tuple patterns, and `_` lower this round",
                ))
            },
        }
        Ok(TypedPlace {
            place,
            ty: declared,
        })
    }

    /// `var x = <int/float literal>` over a scalar word: allocates the
    /// var's span and writes the constant straight into it, so the
    /// binding answers its own word with no temp and no re-store.
    /// Declines for every other shape (an annotated non-scalar, a
    /// type-variable word, a suffixed or unparseable spelling), where
    /// the general path keeps its refusals and staging — the probe
    /// precedes the allocation, so a declined literal leaves no dead
    /// frame word behind.
    fn scalar_var_init(
        &mut self,
        local: &LocalDecl,
        pat: &Pattern,
    ) -> Result<Option<TypedPlace>, LowerError> {
        let Pattern::Binding { span, name } = pat else {
            return Ok(None);
        };
        let Expr::Literal { lit, .. } = peel_parens(&local.init) else {
            return Ok(None);
        };
        let Some(fact) = self.analyzed.checked.binders.get(span).copied() else {
            return Ok(None);
        };
        if self.is_var_ty(fact.ty) {
            return Ok(None);
        }
        let Ok(shape) = self.shapes.shape(fact.ty) else {
            return Ok(None);
        };
        let Shape::Word(slot @ (SlotType::I64 | SlotType::F64)) = &*shape else {
            return Ok(None);
        };
        let Some(constant) = scalar_const(lit, *slot) else {
            return Ok(None);
        };
        let target = self.temp_span(fact.ty)?;
        self.scalar_const_into(constant, Slot::body(target.base));
        let place = self.place_of_ty(target.base, fact.ty)?;
        self.guard_deinit_union_member(fact.ty, fact.ty, local.span)?;
        self.binders.insert(*span, place.clone());
        self.rc_bind(*span, &place, fact.ty)?;
        self.scope_insert(&name.name, *span);
        Ok(Some(TypedPlace { place, ty: fact.ty }))
    }

    /// Loads one probed scalar constant into the slot `dst` with the
    /// literal arm's own op spelling (Kimm for I64, Kconst for F64).
    fn scalar_const_into(&mut self, constant: ScalarConst, dst: Slot) {
        match constant {
            ScalarConst::I64(value) => {
                let kidx = self.pool(Constant::I64(value));
                self.emit(Op::Kimm { dst, kidx });
            },
            ScalarConst::F64(value) => {
                let kidx = self.pool(Constant::F64(value));
                self.emit(Op::Kconst { dst, kidx });
            },
        }
    }

    /// Binds one destructuring `var`'s elements to fresh per-element
    /// slots: each width-1 element word copies into its own var slot (a
    /// Ref word inc's at the copy), so an assignment through an element
    /// re-stores its slot alone and the source tuple keeps its words.
    /// Wider elements refuse: a multi-word element would need a
    /// span-shaped slot, which the span rework's width slices open.
    fn var_tuple_slots(
        &mut self,
        pats: &[Pattern],
        init: &TypedPlace,
        declared: Ty,
    ) -> Result<(), LowerError> {
        let shape = self.shapes.shape(declared)?;
        let Shape::Tuple(s) = &*shape else {
            return Err(mismatch("a destructuring var binding"));
        };
        if pats.len() != s.fields.len() {
            return Err(mismatch("a tuple pattern's arity"));
        }
        let addrs = self.word_addrs(&init.place, declared)?;
        for ((sub, field), addr) in pats.iter().zip(s.fields.iter()).zip(addrs.iter()) {
            let Pattern::Binding { span, name } = sub else {
                match sub {
                    Pattern::Wildcard(_) => continue,
                    _ => {
                        return Err(LowerError::unsupported(
                            pattern_span(sub),
                            "a var destructuring element pattern",
                            "elements bind as plain names or skip as `_`; literals test at match \
                             time and never bind",
                        ))
                    },
                }
            };
            let Some(fact) = self.analyzed.checked.binders.get(span).copied() else {
                return Err(mismatch("a var destructuring binder"));
            };
            if self.is_var_ty(fact.ty) {
                return Err(LowerError::unsupported(
                    *span,
                    "a var element over a type variable",
                    "a reassignment would drop the previous word's count by runtime kind, and the \
                     placement has no overwrite contract for it yet",
                ));
            }
            let Shape::Word(slot) = &*self.shapes.shape(fact.ty)? else {
                return Err(LowerError::unsupported(
                    *span,
                    "a var destructuring element wider than one word",
                    "a struct, tuple, or union element would need a span-shaped var slot; the \
                     span rework's width slices open it",
                ));
            };
            if *slot != field.slot() {
                return Err(mismatch("a var destructuring element word"));
            }
            let target = self.temp_word(*slot)?;
            self.emit(Op::Mov {
                dst: Slot::body(target),
                src: Slot::body(addr.slot),
            });
            if addr.slot_ty == SlotType::Ref {
                self.rc.copied(&mut self.code, target, addr.slot);
            }
            let place = Place {
                base: target,
                shape: PlaceShape::Word(*slot),
            };
            self.binders.insert(*span, place.clone());
            self.rc_bind(*span, &place, fact.ty)?;
            self.scope_insert(&name.name, *span);
        }
        Ok(())
    }

    /// One assignment `x = e` / `x op= e` [C3]: the place is a single-segment
    /// path to a `var` local (the checker rejects every other place), the
    /// right side evaluates first, then re-stores the binder's slots. A
    /// reassignment kills the old value: the replacement drop threads right
    /// here, before the store Movs overwrite the old reference.
    fn assign_value(&mut self, e: &Expr) -> Result<Place, LowerError> {
        let Expr::Assign {
            op, left, right, ..
        } = e
        else {
            return Err(mismatch("an assignment"));
        };
        let Expr::Path { path, .. } = &**left else {
            return Err(LowerError::unsupported(
                left.span(),
                "an assignment place",
                "places are plain name bindings; field assignment is undecided surface [D2]",
            ));
        };
        let [only] = path.segments.as_slice() else {
            return Err(LowerError::unsupported(
                left.span(),
                "an assignment place",
                "qualified places resolve through modules [G4, M7]",
            ));
        };
        let binder = match self.res.get(only.span) {
            Some(Res::Local { bind: binder }) => *binder,
            Some(Res::Def(_)) => {
                return Err(LowerError::unsupported(
                    only.span,
                    "an assignment to a top-level binding",
                    "module-level storage arrives with cross-module state [G4, M7]",
                ))
            },
            _ => {
                return Err(LowerError::unsupported(
                    only.span,
                    "an assignment place",
                    "the name does not address a local binding",
                ))
            },
        };
        let place = self
            .binders
            .get(&binder)
            .cloned()
            .ok_or_else(|| self.missing_binder(only.span))?;
        let declared = self
            .analyzed
            .checked
            .binders
            .get(&binder)
            .copied()
            .ok_or_else(|| mismatch("an assignment place"))?
            .ty;
        let assigned = match op {
            ds_parse::AssignOp::Plain => {
                // An int/float literal stores straight into a scalar
                // var word: the constant has no other consumer and a
                // scalar word carries no count, so the temp + re-store
                // reduces to the one constant load (a declined spelling
                // keeps the temp path and its refusals)
                if let PlaceShape::Word(slot @ (SlotType::I64 | SlotType::F64)) = place.shape
                    && let Expr::Literal { lit, .. } = peel_parens(right)
                    && let Some(constant) = scalar_const(lit, slot)
                {
                    self.scalar_const_into(constant, Slot::body(place.base));
                    TypedPlace {
                        place: place.clone(),
                        ty: declared,
                    }
                } else if let PlaceShape::Word(slot @ (SlotType::I64 | SlotType::F64)) = place.shape
                    && arith_shape(right)
                {
                    // A computed scalar stores straight into the var
                    // word: the assignment offers the word to the
                    // right-hand side's outermost data op (the scalar
                    // destination offer), whose produced temp is
                    // single-use by construction — this store is its
                    // only consumer. A shape that declines the offer
                    // (a call result, a path read) keeps the temp +
                    // re-store.
                    self.scalar_dst = Some(ScalarDst::Var {
                        word: place.base,
                        slot,
                    });
                    let assigned_val = self.value(right)?;
                    let _ = self.scalar_dst.take();
                    assigned_val
                } else {
                    self.value(right)?
                }
            },
            ds_parse::AssignOp::Add
            | ds_parse::AssignOp::Sub
            | ds_parse::AssignOp::Mul
            | ds_parse::AssignOp::Div
            | ds_parse::AssignOp::Rem
            | ds_parse::AssignOp::Shl
            | ds_parse::AssignOp::Shr => {
                // `x op= e` reads the current slots, then re-stores the
                // result: the same word moves the binary path emits.
                let current = TypedPlace {
                    place: place.clone(),
                    ty: declared,
                };
                match op {
                    ds_parse::AssignOp::Rem => self.reject_float_rem(&current, left.span())?,
                    ds_parse::AssignOp::Shl | ds_parse::AssignOp::Shr => {
                        self.reject_float_shift_bitwise(&current, left.span())?;
                    },
                    _ => {},
                }
                let (int_op, float_op) = match op {
                    ds_parse::AssignOp::Add => (DataOp::AddI64, DataOp::AddF64),
                    ds_parse::AssignOp::Sub => (DataOp::SubI64, DataOp::SubF64),
                    ds_parse::AssignOp::Mul => (DataOp::MulI64, DataOp::MulF64),
                    ds_parse::AssignOp::Div => (DataOp::DivI64, DataOp::DivF64),
                    ds_parse::AssignOp::Shl => (DataOp::ShlI64, DataOp::ShlI64),
                    ds_parse::AssignOp::Shr => (DataOp::ShrI64, DataOp::ShrI64),
                    _ => (DataOp::ModI64, DataOp::ModI64),
                };
                // string `+=` opens the concat window before the rhs
                // evaluation and pins the replaced word: a last use
                // inside the rhs defers the replaced content's dec to
                // the flush (after the CALL), and the store below then
                // finds the word unowned and skips its own dec
                let str_add = matches!(op, ds_parse::AssignOp::Add)
                    && self.is_string_add_ty(declared, self.val(right.span())?.ty);
                if str_add {
                    self.rc.native_open();
                    let words = self
                        .word_addrs(&place, declared)
                        .map_err(|err| err.respanned(right.span()))?;
                    self.rc.native_reads(words.iter().map(|addr| addr.slot));
                }
                let rhs = self.value(right)?;
                let result = if str_add {
                    // string `+=` desugars like the binary arm: the
                    // compound store re-stores the concat result, a
                    // fresh Ref word
                    self.concat_value(&current, &rhs, declared, left.span())?
                } else if let PlaceShape::Word(SlotType::I64 | SlotType::F64) = place.shape {
                    // a scalar compound store writes the var word
                    // itself: the VM reads both operands before writing
                    // dst, so the word safely aliases its own read, and
                    // the re-store below degenerates to the in-place
                    // no-op (a scalar word carries no count)
                    self.arith_at(int_op, float_op, &current, &rhs, place.base, left.span())?
                } else {
                    self.arith(int_op, float_op, &current, &rhs, left.span())?
                };
                TypedPlace {
                    place: result,
                    ty: declared,
                }
            },
        };
        // the replaced reference dies before the store overwrites its word
        // (compound stores re-store arithmetic results or the string
        // concat's fresh Ref word, never a borrowed operand). A deinit
        // value's claim fires first (C5): the hook runs while the old
        // content's counts are still live, and the span claim's drop decs
        // every `Ref` word of the value — the folded words carry no entry
        // of their own
        if self.deinit_claim(declared, e.span())?.is_some() {
            self.rc.store_over(&mut self.code, &[place.base]);
        } else {
            for word in self.count_words(place.base, declared)? {
                self.rc.store_over(&mut self.code, &[word]);
            }
        }
        self.refuse_closure_widen(&assigned, declared, e.span())?;
        self.guard_deinit_union_member(assigned.ty, declared, e.span())?;
        self.widen_into(&place, declared, &assigned, e.span())?;
        // The replaced value's claim re-registers at its slot: the store
        // fired the old content's drop (spending the site's band) and the
        // incoming claim's transfer spent its own, so without this the
        // teardown doors read the slot dead and the value the frame still
        // holds never fires its hook — the same re-registration a binder
        // performs over a moved-in claim (C5).
        if let Some(claim) = self.deinit_claim(declared, e.span())?
            && self.rc.has_claim(place.base)
            && self.rc.anchors_claim(place.base)
        {
            self.overlay_site(&claim.staged, &[SlotType::I64]);
            let words = self.tracked_words(&place, declared)?;
            let band = self.claim_band_word(place.base)?;
            self.rc
                .claimed(&mut self.code, place.base, claim.hook, band, &words);
        }
        Ok(self.unit_value(e.span())?.place)
    }

    /// One `while cond { body }`: a backward jump over the re-tested
    /// condition [F1]. The body lowers as statements (its value is unit);
    /// every body slot is frame-static, so an iteration re-writes the same
    /// words. Refinements harvested inside an iteration never escape it
    /// (the checker restores its snapshot at loop exit; the lowerer reads
    /// only per-expression checked types, which already respect that).
    /// The loop forks the ownership map before the head: the cond replays
    /// per iteration, so a Ref operand born outside the loop is
    /// loop-carried (its count never decs at the in-head use; it dies at
    /// the owning scope's end, past the loop), while words the cond
    /// itself produces die per iteration at their uses.
    fn while_value(&mut self, e: &Expr) -> Result<Place, LowerError> {
        let Expr::While { cond, body, span } = e else {
            return Err(mismatch("a while loop"));
        };
        let head = self.label();
        let exit = self.label();
        // continue targets the head: the cond re-evaluates per iteration
        // (body_depth is the scope count before the body scope pushes,
        // so a jump sweeps every scope from the body scope inwards)
        self.loops.push(LoopCtx {
            exit,
            cont: head,
            body_depth: self.scopes.len(),
        });
        // loop-fork the ownership map: words owned on entry AND at the
        // back edge survive the loop; body-only words died inside it, and
        // uses one depth deeper never kill outer productions
        let entry = self.rc.save();
        let carried: Vec<u8> = entry.keys().copied().collect();
        self.rc.loop_enter();
        self.loop_spans.push(LoopSpan {
            head: self.code.len(),
            back: usize::MAX,
        });
        self.bind_label(head);
        let cond_val = self.value(cond)?;
        let cond_word = single(&self.word_addrs(&cond_val.place, cond_val.ty)?, cond.span())?;
        self.emit(Op::JmpifNot {
            cond: Slot::body(cond_word.slot),
            target: exit,
        });
        self.scope_push();
        for stmt in &body.stmts {
            self.stmt(stmt)?;
        }
        self.scope_pop();
        self.rc.loop_exit();
        let body_end = self.rc.take();
        self.rc.join(&[entry, body_end]);
        self.loop_back_edge();
        self.emit(Op::Jmp { target: head });
        self.bind_label(exit);
        // an anonymous cond operand consumed in-loop has no scope of its
        // own: its carried count dies here, once per evaluation
        self.rc.drop_unnamed(&mut self.code, &carried);
        self.loops.pop();
        Ok(self.unit_value(*span)?.place)
    }

    /// One `for x in it { body }`: the iterable is an int range or one
    /// list [F7]; every other iterable refuses.
    fn for_value(&mut self, e: &Expr) -> Result<Place, LowerError> {
        let Expr::For {
            pat,
            iter,
            body,
            span,
        } = e
        else {
            return Err(mismatch("a for loop"));
        };
        match peel_parens(iter) {
            Expr::Range {
                inclusive,
                start,
                end,
                ..
            } => self.for_range_value(pat, start, end, *inclusive, body, *span),
            _ => self.for_list_value(pat, iter, body, *span),
        }
    }

    /// A `for x in a..b` over an int range: a counted loop with one
    /// induction slot. The endpoints evaluate once; the step test
    /// re-runs per iteration against the frozen end. An inclusive back
    /// edge parked at i64::MAX exits instead of stepping (the add has
    /// no successor word). The pattern binder
    /// names the induction word, so every body read sees the current
    /// step [F7].
    fn for_range_value(
        &mut self,
        pat: &Pattern,
        start: &Expr,
        end: &Expr,
        inclusive: bool,
        body: &Expr,
        span: Span,
    ) -> Result<Place, LowerError> {
        let start_tp = self.value(start)?;
        let start_w = single(
            &self.word_addrs(&start_tp.place, start_tp.ty)?,
            start.span(),
        )?;
        let end_tp = self.value(end)?;
        let end_w = single(&self.word_addrs(&end_tp.place, end_tp.ty)?, end.span())?;
        if start_w.slot_ty != SlotType::I64 || end_w.slot_ty != SlotType::I64 {
            return Err(LowerError::unsupported(
                span,
                "a range over non-`int` widths",
                "only the ambient `int` width lowers this round; conversions are explicit [B7]",
            ));
        }
        let ind = self.temp_word(SlotType::I64)?;
        self.emit(Op::Mov {
            dst: Slot::body(ind),
            src: Slot::body(start_w.slot),
        });
        let step = self.temp_word(SlotType::I64)?;
        let one = self.pool(Constant::I64(1));
        self.emit(Op::Kimm {
            dst: Slot::body(step),
            kidx: one,
        });
        let ind_place = Place {
            base: ind,
            shape: PlaceShape::Word(SlotType::I64),
        };
        let ind_tp = TypedPlace {
            place: ind_place.clone(),
            ty: start_tp.ty,
        };
        let head = self.label();
        let exit = self.label();
        let back = self.label();
        // continue lands at the backedge start so the induction step
        // still runs before the head re-test (body_depth is the scope
        // count before the body scope pushes, so a jump sweeps every
        // scope from the body scope inwards)
        self.loops.push(LoopCtx {
            exit,
            cont: back,
            body_depth: self.scopes.len(),
        });
        self.bind_label(head);
        let bound_op = if inclusive { DataOp::LeI64 } else { DataOp::LtI64 };
        let cond = self.emit_data(bound_op, &ind_tp, &end_tp, span)?;
        let cond_w = single(&self.word_addrs(&cond, start_tp.ty)?, span)?;
        self.emit(Op::JmpifNot {
            cond: Slot::body(cond_w.slot),
            target: exit,
        });
        // loop-fork the ownership map exactly like the while shape
        let entry = self.rc.save();
        self.rc.loop_enter();
        self.loop_spans.push(LoopSpan {
            head: self.code.len(),
            back: usize::MAX,
        });
        self.scope_push();
        match pat {
            Pattern::Binding {
                span: bind_span,
                name,
            } => {
                self.binders.insert(*bind_span, ind_place.clone());
                self.rc_bind(*bind_span, &ind_place, start_tp.ty)?;
                self.scope_insert(&name.name, *bind_span);
            },
            Pattern::Wildcard(_) => {},
            _ => {
                return Err(LowerError::unsupported(
                    span,
                    "a destructuring for pattern",
                    "only plain name and wildcard patterns lower this round",
                ))
            },
        }
        match body {
            Expr::Block(block) => {
                for stmt in &block.stmts {
                    self.stmt(stmt)?;
                }
            },
            other => {
                let tp = self.value(other)?;
                let words = self.discard_words(&tp.place, tp.ty, other.span())?;
                self.rc.discard(&mut self.code, &words);
            },
        }
        self.scope_pop();
        self.rc.loop_exit();
        let body_end = self.rc.take();
        self.rc.join(&[entry, body_end]);
        self.bind_label(back);
        if inclusive {
            // an inclusive count parked at i64::MAX has no successor
            // step (the add would trap), so the back edge exits there;
            // continue lands here too, so the guard covers every exit
            // from the final iteration
            let ceiling = self.temp_word(SlotType::I64)?;
            let max = self.pool(Constant::I64(i64::MAX));
            self.emit(Op::Kimm {
                dst: Slot::body(ceiling),
                kidx: max,
            });
            let at_max = self.temp_word(SlotType::Bool)?;
            self.emit(Op::Data {
                op: DataOp::EqI64,
                dst: Slot::body(at_max),
                lhs: Slot::body(ind),
                rhs: Slot::body(ceiling),
            });
            self.emit(Op::Jmpif {
                cond: Slot::body(at_max),
                target: exit,
            });
        }
        self.emit(Op::Data {
            op: DataOp::AddI64,
            dst: Slot::body(ind),
            lhs: Slot::body(ind),
            rhs: Slot::body(step),
        });
        self.loop_back_edge();
        self.emit(Op::Jmp { target: head });
        self.bind_label(exit);
        self.loops.pop();
        Ok(self.unit_value(span)?.place)
    }

    /// A `for x in xs` over one list [F7]: the iterable evaluates once
    /// and its element count freezes at loop entry (one `list_len` call
    /// into a hidden count word) — elements the body appends are not
    /// visited this loop — then an index-counted fetch loop: one hidden
    /// induction word, `list_get` per iteration over its ok/none span
    /// (one payload word per element word at the list's stride), the
    /// frozen count bounds the loop while the none span stays the
    /// fetch's fail-closed guard. The pattern binder names the
    /// gathered element — one word or a whole
    /// struct span: the native's handed-out counts (one per `Ref` word)
    /// die at the binding's scope end each rollover, never carried
    /// across the back edge, while the iterable's count survives the
    /// whole loop (a loop-carried borrow the fetch re-stages per
    /// iteration) and dies past it.
    fn for_list_value(
        &mut self,
        pat: &Pattern,
        iter: &Expr,
        body: &Expr,
        span: Span,
    ) -> Result<Place, LowerError> {
        let iter_tp = self.value(iter)?;
        let Some(element) = self.shapes.list_element(iter_tp.ty) else {
            return Err(LowerError::unsupported(
                span,
                "a for over a non-list collection",
                "iteration lowers over int ranges and `list[t]`; map iteration waits on an \
                 ordered-entries native",
            ));
        };
        let elem_words = self.element_words(element, span)?;
        let list_w = single(&self.word_addrs(&iter_tp.place, iter_tp.ty)?, iter.span())?;
        let ind = self.temp_word(SlotType::I64)?;
        let zero = self.pool(Constant::I64(0));
        self.emit(Op::Kimm {
            dst: Slot::body(ind),
            kidx: zero,
        });
        let step = self.temp_word(SlotType::I64)?;
        let one = self.pool(Constant::I64(1));
        self.emit(Op::Kimm {
            dst: Slot::body(step),
            kidx: one,
        });
        // loop-fork the ownership map before the head: the iterable is
        // a loop-carried word, so the per-iteration fetch staging never
        // decs it in-body
        let entry = self.rc.save();
        self.rc.loop_enter();
        // the frozen element count: one length read here bounds the
        // whole loop, so a body appending to the iterable never extends
        // it. The read sits after loop_enter (these pre-head ops run
        // once) so the iterable is already loop-carried: an anonymous
        // iterable dies at first consumption, and the length read would
        // otherwise consume its count before the fetch runs.
        let len = self.temp_word(SlotType::I64)?;
        self.list_len_word(list_w.slot, len)?;
        self.loop_spans.push(LoopSpan {
            head: self.code.len(),
            back: usize::MAX,
        });
        let head = self.label();
        let exit = self.label();
        let back = self.label();
        // continue lands at the backedge start so the index step still
        // runs before the head re-fetch (body_depth is the scope count
        // before the body scope pushes, so a jump sweeps every scope
        // from the body scope inwards)
        self.loops.push(LoopCtx {
            exit,
            cont: back,
            body_depth: self.scopes.len(),
        });
        self.bind_label(head);
        // the frozen count is the loop's exit test: the index check runs
        // before the fetch, so indices past the entry count never stage
        // a borrow or hand out counts
        let bound = self.temp_word(SlotType::Bool)?;
        self.emit(Op::Data {
            op: DataOp::LtI64,
            dst: Slot::body(bound),
            lhs: Slot::body(ind),
            rhs: Slot::body(len),
        });
        self.emit(Op::JmpifNot {
            cond: Slot::body(bound),
            target: exit,
        });
        self.fetch_element(list_w.slot, ind, &elem_words)?;
        let cond = self.temp_word(SlotType::Bool)?;
        self.emit(Op::TagTest {
            dst: Slot::body(cond),
            src: Slot::overlay(0),
            tag: PLATFORM_IO_OK,
        });
        self.emit(Op::JmpifNot {
            cond: Slot::body(cond),
            target: exit,
        });
        self.scope_push();
        let elem_place = self.gather_element(&elem_words, element)?;
        match pat {
            Pattern::Binding {
                span: bind_span,
                name,
            } => {
                self.binders.insert(*bind_span, elem_place.clone());
                self.rc_bind(*bind_span, &elem_place, element)?;
                self.scope_insert(&name.name, *bind_span);
            },
            Pattern::Wildcard(_) => {
                let words = self.tracked_words(&elem_place, element)?;
                self.rc.discard(&mut self.code, &words);
            },
            _ => {
                return Err(LowerError::unsupported(
                    span,
                    "a destructuring for pattern",
                    "only plain name and wildcard patterns lower this round",
                ))
            },
        }
        match body {
            Expr::Block(block) => {
                for stmt in &block.stmts {
                    self.stmt(stmt)?;
                }
            },
            other => {
                let tp = self.value(other)?;
                let words = self.discard_words(&tp.place, tp.ty, other.span())?;
                self.rc.discard(&mut self.code, &words);
            },
        }
        self.scope_pop();
        self.rc.loop_exit();
        let body_end = self.rc.take();
        self.rc.join(&[entry, body_end]);
        self.bind_label(back);
        self.emit(Op::Data {
            op: DataOp::AddI64,
            dst: Slot::body(ind),
            lhs: Slot::body(ind),
            rhs: Slot::body(step),
        });
        self.loop_back_edge();
        self.emit(Op::Jmp { target: head });
        self.bind_label(exit);
        // an anonymous iterable (a literal, a call result) has no scope
        // of its own: its consumed count dies here, once per evaluation
        self.rc.drop_unnamed(&mut self.code, &[list_w.slot]);
        self.loops.pop();
        Ok(self.unit_value(span)?.place)
    }

    /// One `break`: the binders of the loop body's scopes inwards die at
    /// the jump — their words drop here, once per path, while the
    /// fall-through path drops the same words at the scope pops the jump
    /// skips. Carried and outer words stay untouched: the exit label's
    /// carried cleanup and the outer scope pops run past the loop on
    /// every exit path.
    fn break_value(&mut self, span: Span) -> Result<Place, LowerError> {
        let Some(ctx) = self.loops.last().copied() else {
            return Err(LowerError::unsupported(
                span,
                "a break outside a loop",
                "loop exits never cross a function or lambda frame boundary",
            ));
        };
        self.loop_exit_drops(ctx.body_depth);
        self.emit(Op::Jmp { target: ctx.exit });
        Ok(self.unit_value(span)?.place)
    }

    /// One `continue`: the same scope accounting as `break`, jumping to
    /// the backedge start so the induction step and the head re-test
    /// still run.
    fn continue_value(&mut self, span: Span) -> Result<Place, LowerError> {
        let Some(ctx) = self.loops.last().copied() else {
            return Err(LowerError::unsupported(
                span,
                "a continue outside a loop",
                "loop exits never cross a function or lambda frame boundary",
            ));
        };
        self.loop_exit_drops(ctx.body_depth);
        self.emit(Op::Jmp { target: ctx.cont });
        Ok(self.unit_value(span)?.place)
    }

    /// The words dying at a loop-exit jump: every binder of the scopes
    /// from the loop's body scope inwards, innermost scope first and
    /// binders in source order inside one scope, matching the scope-pop
    /// drop order the fall-through path takes.
    fn loop_exit_drops(&mut self, body_depth: usize) {
        let mut doomed: Vec<Span> = Vec::new();
        for scope in self.scopes[body_depth..].iter().rev() {
            let mut spans: Vec<Span> = scope.values().copied().collect();
            spans.sort_by(|a, b| {
                (a.file.as_u32(), a.start, a.end).cmp(&(b.file.as_u32(), b.start, b.end))
            });
            doomed.extend(spans);
        }
        let dying = self.rc.loop_exit_drops(&doomed);
        Placement::emit_drops(&mut self.code, &dying);
    }

    /// Stages one `list_len` call through CALLG [H3]: the list word as
    /// the single real argument, the list instance as the one evidence
    /// word behind it, the total i64 result moved into the caller's
    /// word. The list word stages as the native's transient borrow
    /// window.
    fn list_len_word(&mut self, list: u8, dst: u8) -> Result<(), LowerError> {
        let generic = GENERIC_NATIVES
            .iter()
            .find(|native| native.name == "list_len")
            .expect("the list_len body is tabled");
        self.guard_staged_count(2)?;
        self.rc.native_open();
        self.rc.native_reads([list]);
        self.overlay_site(&[SlotType::Ref, SlotType::I64], &[SlotType::I64]);
        self.emit(Op::Mov {
            dst: Slot::overlay(0),
            src: Slot::body(list),
        });
        self.rc.staged_native(list, false);
        let evidence = self.pool(Constant::I64(BuiltinInstance::List as i64));
        self.emit(Op::Kimm {
            dst: Slot::overlay(1),
            kidx: evidence,
        });
        let callee = self.pool(self.funcref_const(Callee::Generic(generic)));
        self.emit(Op::Callg { callee, nargs: 1 });
        self.rc.native_done(&mut self.code);
        self.emit(Op::Mov {
            dst: Slot::body(dst),
            src: Slot::overlay(0),
        });
        Ok(())
    }

    /// Stages one `list_get` fetch through CALLG [H3]: the list word and
    /// the index as the real arguments, the list instance as the one
    /// evidence word behind them, and the ok/none span answering on the
    /// overlay — word 0 the discriminant, then one payload word per
    /// element word (`elem_words`, the native channel's span). The list
    /// word stages as the native's transient borrow window.
    fn fetch_element(
        &mut self,
        list: u8,
        index: u8,
        elem_words: &[SlotType],
    ) -> Result<(), LowerError> {
        let generic = GENERIC_NATIVES
            .iter()
            .find(|native| native.name == "list_get")
            .expect("the list_get body is tabled");
        let mut result_words = vec![SlotType::Tag];
        result_words.extend_from_slice(elem_words);
        self.guard_staged_count(3.max(result_words.len()))?;
        self.rc.native_open();
        self.rc.native_reads([list]);
        self.overlay_site(
            &[SlotType::Ref, SlotType::I64, SlotType::I64],
            &result_words,
        );
        self.emit(Op::Mov {
            dst: Slot::overlay(0),
            src: Slot::body(list),
        });
        self.rc.staged_native(list, false);
        self.emit(Op::Mov {
            dst: Slot::overlay(1),
            src: Slot::body(index),
        });
        let evidence = self.pool(Constant::I64(BuiltinInstance::List as i64));
        self.emit(Op::Kimm {
            dst: Slot::overlay(2),
            kidx: evidence,
        });
        let callee = self.pool(self.funcref_const(Callee::Generic(generic)));
        self.emit(Op::Callg { callee, nargs: 2 });
        self.rc.native_done(&mut self.code);
        Ok(())
    }

    /// Gathers one fetched element out of the overlay span into a fresh
    /// element place: one word per element word moved in declaration
    /// order (a struct's flattened words are contiguous), each counted
    /// word produced for the frame — Ref, or Raw (a type variable's
    /// runtime word under a counted instantiation) — because the fetch
    /// handed one count per counted word, which this place owns until
    /// its death. The one-word shape stays a bare word, owned and
    /// kind-checked at its drop.
    fn gather_element(
        &mut self,
        elem_words: &[SlotType],
        element: Ty,
    ) -> Result<Place, LowerError> {
        if let [only] = elem_words {
            let elem = self.temp_word(*only)?;
            self.emit(Op::Mov {
                dst: Slot::body(elem),
                src: Slot::overlay(1),
            });
            if *only == SlotType::Ref || *only == SlotType::Raw {
                self.rc.produced(elem);
            }
            return Ok(Place {
                base: elem,
                shape: PlaceShape::Word(*only),
            });
        }
        let base = self.alloc(elem_words)?;
        for (index, slot) in elem_words.iter().enumerate() {
            let word = base + u8::try_from(index).expect("span width fits u8");
            self.emit(Op::Mov {
                dst: Slot::body(word),
                src: Slot::overlay(u8::try_from(1 + index).expect("overlay word fits u8")),
            });
            // a Raw element word is a type variable's runtime word under a
            // counted instantiation, so the production (the fetch's handed
            // count) registers like a Ref — the RC ops check the runtime
            // kind, matching the one-word shape above
            if matches!(*slot, SlotType::Ref | SlotType::Raw) {
                self.rc.produced(word);
            }
        }
        Ok(Place {
            base,
            shape: PlaceShape::Span(element),
        })
    }

    /// The word kinds of one list element type for the iteration and
    /// membership fetches: one scalar word stays the one-word shape,
    /// a struct's flattened words ride the native channel's span, and
    /// every other shape (tuple, union) keeps the checked-read refusal
    /// family's name — the element cannot form its optional or compare
    /// atomically.
    fn element_words(&mut self, element: Ty, span: Span) -> Result<SpanWords, LowerError> {
        let shape = self.shapes.shape(element)?;
        match &*shape {
            Shape::Word(
                SlotType::I64
                | SlotType::F64
                | SlotType::Bool
                | SlotType::Tag
                | SlotType::Ref
                | SlotType::Raw
                | SlotType::Fn,
            ) => self.shapes.words(element),
            Shape::Struct(_) => {
                // the list's edge retention owns counts, never claims
                // (the element guard's own refusal)
                if self.deinit_hook(element, span)?.is_some() {
                    return Err(LowerError::unsupported(
                        span,
                        "a deinit-carrying list element",
                        "the iteration binder would own a claim the list's edge retention cannot \
                         honor; per-span deinit claims own the seam",
                    ));
                }
                self.shapes
                    .words(element)
                    .map_err(|err| err.respanned(span))
            },
            Shape::Tuple(_) => {
                // one positional word per element — the same flat span a
                // struct's flattened words ride, gathered positionally
                // (a tuple element can carry no deinit: a multi-word
                // element refuses its own shape)
                self.shapes
                    .words(element)
                    .map_err(|err| err.respanned(span))
            },
            Shape::Union(_) => {
                // a union element's whole tag-plus-region span, gathered
                // verbatim (the checked read's own multi-atom hand-back
                // shape — the element's own tag word already names the
                // member the binder narrows by); a union member cannot
                // carry a deinit, so the struct arm's guard is vacuous
                self.shapes
                    .words(element)
                    .map_err(|err| err.respanned(span))
            },
            Shape::Word(_) => Err(LowerError::unsupported(
                span,
                "a list element of this shape",
                "iteration and membership bind one scalar word, one struct's flattened span, a \
                 tuple's positional words, or a union's whole span",
            )),
        }
    }

    // ---- the expression walk ---------------------------------------------------

    fn value(&mut self, e: &Expr) -> Result<TypedPlace, LowerError> {
        self.value_node(e).map_err(|err| err.respanned(e.span()))
    }

    /// The expression walk. Type-keyed refusals (shapes, word
    /// addressing) observe no node, so [`fn value`](Self::value)
    /// re-spans them at the expression whose lowering hit them.
    fn value_node(&mut self, e: &Expr) -> Result<TypedPlace, LowerError> {
        let val = self.val(e.span())?;
        let place = match e {
            Expr::Literal { lit, .. } => self.literal(lit, e.span())?,
            Expr::Str { parts, .. } => self.string_expr(parts)?,
            Expr::RawStr { text, .. } => {
                let dst = self.temp_word(SlotType::Ref)?;
                let kidx = self.pool_str(text);
                self.kstr(dst, kidx);
                Place {
                    base: dst,
                    shape: PlaceShape::Word(SlotType::Ref),
                }
            },
            Expr::List { .. } => self.list_literal(e)?,
            Expr::Path { path, .. } => self.path_value(path, e.span(), val.ty)?,
            Expr::Paren { inner, .. } => return self.value(inner),
            Expr::Block(block) => return self.block_value(block),
            Expr::If { .. } => self.if_value(e, val.ty)?,
            Expr::Match { .. } => self.match_value(e, val.ty)?,
            Expr::Struct { .. } => self.struct_literal(e, val.ty)?,
            Expr::Call { .. } => self.call_value(e, val.ty)?,
            Expr::Field { .. } => self.field_access(e, val.ty)?,
            Expr::Try { .. } => self.try_value(e, val.ty)?,
            Expr::Unary { op, expr, span } => self.unary(*op, expr, *span, val.ty)?,
            Expr::Binary { .. } => self.binary_value(e, val.ty)?,
            Expr::Is { expr, ty, span } => self.is_value(expr, ty, *span)?,
            Expr::Assign { .. } => self.assign_value(e)?,
            Expr::While { .. } => self.while_value(e)?,
            Expr::Return { .. } => return self.return_value(e),
            Expr::For { .. } => self.for_value(e)?,
            Expr::Break(span) => self.break_value(*span)?,
            Expr::Continue(span) => self.continue_value(*span)?,
            Expr::Nursery { .. } => self.nursery_value(e)?,
            Expr::Spawn { .. } => self.spawn_value(e)?,
            Expr::Defer { .. } => {
                return Err(LowerError::unsupported(
                    e.span(),
                    "a control statement",
                    "defer statements have no lowering yet",
                ))
            },
            Expr::Lambda { .. } => self.lambda_value(e)?,
            Expr::Tuple { items, span, .. } => {
                if items.is_empty() {
                    self.unit_value(*span)?.place
                } else {
                    self.tuple_literal(items, val.ty, *span)?
                }
            },
            Expr::MapApply { .. } => {
                return Err(LowerError::unsupported(
                    e.span(),
                    "a map type application",
                    "the application `map[k, v]` constructs only in call position — `map[k, v]()`",
                ))
            },
            Expr::FnType { .. } => {
                return Err(LowerError::unsupported(
                    e.span(),
                    "a fn type in value position",
                    "a fn type is a type spelling for the constructor brackets — `list[fn(int) -> \
                     int]()`",
                ))
            },
            Expr::Index { .. } | Expr::Range { .. } => {
                return Err(LowerError::unsupported(
                    e.span(),
                    "an index or range expression",
                    "indexing and ranges arrive with the collection surface",
                ))
            },
        };
        Ok(TypedPlace { place, ty: val.ty })
    }

    fn literal(&mut self, lit: &Literal, span: Span) -> Result<Place, LowerError> {
        match lit {
            Literal::Int { suffix, text } => {
                if !suffix.is_empty() {
                    return Err(LowerError::unsupported(
                        span,
                        "a suffixed integer literal",
                        "explicit widths are not in the M3 contract",
                    ));
                }
                let value = parse_int(text).ok_or_else(|| {
                    LowerError::unsupported(span, "an integer literal", "it does not parse as i64")
                })?;
                let dst = self.temp_word(SlotType::I64)?;
                let kidx = self.pool(Constant::I64(value));
                self.emit(Op::Kimm {
                    dst: Slot::body(dst),
                    kidx,
                });
                Ok(Place {
                    base: dst,
                    shape: PlaceShape::Word(SlotType::I64),
                })
            },
            Literal::Float { suffix, text } => {
                if !suffix.is_empty() {
                    return Err(LowerError::unsupported(
                        span,
                        "a suffixed float literal",
                        "explicit widths are not in the M3 contract",
                    ));
                }
                let Ok(value) = text.parse::<f64>() else {
                    return Err(LowerError::unsupported(
                        span,
                        "a float literal",
                        "it does not parse as f64",
                    ));
                };
                let dst = self.temp_word(SlotType::F64)?;
                let kidx = self.pool(Constant::F64(value));
                self.emit(Op::Kconst {
                    dst: Slot::body(dst),
                    kidx,
                });
                Ok(Place {
                    base: dst,
                    shape: PlaceShape::Word(SlotType::F64),
                })
            },
            Literal::Bool(value) => {
                let dst = self.temp_word(SlotType::Bool)?;
                let kidx = self.pool(Constant::Bool(*value));
                self.emit(Op::Kconst {
                    dst: Slot::body(dst),
                    kidx,
                });
                Ok(Place {
                    base: dst,
                    shape: PlaceShape::Word(SlotType::Bool),
                })
            },
            Literal::Char(c) => {
                // a char rides the string Ref as its one-char string (the
                // runtime convention the list natives' separator pins)
                let dst = self.temp_word(SlotType::Ref)?;
                let kidx = self.pool_str(c.encode_utf8(&mut [0u8; 4]));
                self.kstr(dst, kidx);
                Ok(Place {
                    base: dst,
                    shape: PlaceShape::Word(SlotType::Ref),
                })
            },
            Literal::Byte(b) => {
                // A byte stages as its 0-255 integer value [F6]: the atom
                // is its own u8 width class, but the word rides the
                // integer slot encoding — the checker keeps byte outside
                // the int conversion family, so no arithmetic reaches here
                let dst = self.temp_word(SlotType::I64)?;
                let kidx = self.pool(Constant::I64(i64::from(*b)));
                self.emit(Op::Kimm {
                    dst: Slot::body(dst),
                    kidx,
                });
                Ok(Place {
                    base: dst,
                    shape: PlaceShape::Word(SlotType::I64),
                })
            },
        }
    }

    /// One linked const's value fact as a place: the literal embeds at the
    /// use site over the same word shapes and RC treatment the owning
    /// module's own literal lowering gives it — a string or char through
    /// the produced-Kstr channel (an arena temporary), a list through the
    /// `list_new`/`list_push` seam a local list literal lowers through
    /// (the element type read immutably from the use's checked type,
    /// `ty`), everything else a scalar constant word.
    fn embed_const_fact(
        &mut self,
        fact: ConstFact,
        ty: Ty,
        span: Span,
    ) -> Result<Place, LowerError> {
        let text = match fact {
            ConstFact::Int(value) => {
                let dst = self.temp_word(SlotType::I64)?;
                let kidx = self.pool(Constant::I64(value));
                self.emit(Op::Kimm {
                    dst: Slot::body(dst),
                    kidx,
                });
                return Ok(Place {
                    base: dst,
                    shape: PlaceShape::Word(SlotType::I64),
                });
            },
            ConstFact::Float(value) => {
                let dst = self.temp_word(SlotType::F64)?;
                let kidx = self.pool(Constant::F64(value));
                self.emit(Op::Kconst {
                    dst: Slot::body(dst),
                    kidx,
                });
                return Ok(Place {
                    base: dst,
                    shape: PlaceShape::Word(SlotType::F64),
                });
            },
            ConstFact::Bool(value) => {
                let dst = self.temp_word(SlotType::Bool)?;
                let kidx = self.pool(Constant::Bool(value));
                self.emit(Op::Kconst {
                    dst: Slot::body(dst),
                    kidx,
                });
                return Ok(Place {
                    base: dst,
                    shape: PlaceShape::Word(SlotType::Bool),
                });
            },
            ConstFact::Byte(value) => {
                // a byte stages as its 0-255 integer value, the literal
                // lowering's own convention
                let dst = self.temp_word(SlotType::I64)?;
                let kidx = self.pool(Constant::I64(i64::from(value)));
                self.emit(Op::Kimm {
                    dst: Slot::body(dst),
                    kidx,
                });
                return Ok(Place {
                    base: dst,
                    shape: PlaceShape::Word(SlotType::I64),
                });
            },
            ConstFact::Str(text) => text,
            ConstFact::Char(c) => c.encode_utf8(&mut [0u8; 4]).to_owned(),
            ConstFact::List(items) => return self.embed_list_fact(items, ty, span),
        };
        let dst = self.temp_word(SlotType::Ref)?;
        let kidx = self.pool_str(&text);
        self.kstr(dst, kidx);
        Ok(Place {
            base: dst,
            shape: PlaceShape::Word(SlotType::Ref),
        })
    }

    /// One linked list-const's elements onto a fresh list word: the same
    /// construction a local list literal lowers through — `list_new`,
    /// then each element's literal embed staged through `list_push`'s
    /// overlay — so the RC discipline (the frame-owned list borrowing
    /// itself per push, each Ref element staged for the native's own
    /// count) is the local spelling's.
    fn embed_list_fact(
        &mut self,
        items: Vec<ConstFact>,
        ty: Ty,
        span: Span,
    ) -> Result<Place, LowerError> {
        let elem = self.shapes.list_element(ty).ok_or_else(|| {
            LowerError::unsupported(
                span,
                "a linked list const",
                "the use's checked type is not the applied list constructor the fact's elements \
                 stage over",
            )
        })?;
        let list = self.list_new_word()?;
        self.list_push_each(list, items, |this, fact| {
            let place = this.embed_const_fact(fact, elem, span)?;
            Ok((TypedPlace { place, ty: elem }, span))
        })?;
        Ok(Place {
            base: list,
            shape: PlaceShape::Word(SlotType::Ref),
        })
    }

    /// A lambda value (F2): the synthesized module function plus its word.
    /// A non-capturing lambda mints the copyable scalar FuncRef into a Fn
    /// slot (KCONST over the pool's same-module FuncRef, resolved through
    /// the registry id space at issue time). A capturing lambda builds a
    /// heap closure object instead: the KCONST-minted FuncRef word heads
    /// it, the capture words stage on the overlay, and CLOSURE_NEW
    /// allocates the object — acquiring one count per `Ref` capture (the
    /// staged sources keep their own counts and die their ordinary
    /// deaths: copyable bindings copy in, an affine capture's outer
    /// binding is dead to the program by the checker's move) — leaving
    /// one `Ref` word in a Ref-classified slot. That word is the value:
    /// every fn-typed slot stays scalar, so the closure flows only
    /// through value-site places (its construction word, const aliases of
    /// it, a provenance-known call result).
    fn lambda_value(&mut self, e: &Expr) -> Result<Place, LowerError> {
        let Expr::Lambda {
            generics,
            span,
            body,
            ..
        } = e
        else {
            return Err(mismatch("a lambda value"));
        };
        if !generics.is_empty() {
            return Err(LowerError::unsupported(
                *span,
                "a generic lambda value",
                "the word carries no instantiation and a call through it has no site to stage the \
                 body's evidence words; free generic fns carry the generic call surface",
            ));
        }
        // a lambda lexically inside a generic impl body carries its own
        // evidence demands: the pairs its body calls (through its own
        // nested lambdas), plus the callee demands its call sites read
        // from the plan's table, staged from this frame's evidence slots
        // into the capture block
        let evidence = if self.bounds.is_empty() {
            Rc::new(Vec::new())
        } else {
            Rc::new(runtime_pairs(
                self.analyzed,
                self.res,
                &self.bounds,
                body,
                &self.plan.demands,
            ))
        };
        let (index, captures) = self.plan.push_lambda(
            &self.name,
            e,
            self.res,
            &self.analyzed.checked.binders,
            evidence.clone(),
            self.bounds.clone(),
        );
        let (word, slot) = self.child_callable(*span, index, &captures, &evidence)?;
        Ok(Place {
            base: word,
            shape: PlaceShape::Word(slot),
        })
    }

    /// The callable word for a synthesized block function (a lambda value
    /// or a spawned child [E2]): a scalar `Fn` word when nothing
    /// captures, a closure object's `Ref` staging the captures otherwise.
    /// The capture refusals are the lambda construction's own — a spawned
    /// child is the same shape.
    fn child_callable(
        &mut self,
        span: Span,
        index: u32,
        captures: &[(Span, Ty)],
        evidence: &[RuntimePair],
    ) -> Result<(u8, SlotType), LowerError> {
        let kidx = self.pool(Constant::FuncRef {
            key: FuncRefKey::Dep(0), // the enclosing module
            func: index,
        });
        let func_word = self.temp_word(SlotType::Fn)?;
        self.emit(Op::Kconst {
            dst: Slot::body(func_word),
            kidx,
        });
        if captures.is_empty() && evidence.is_empty() {
            self.fn_words.insert(func_word, index);
            self.scalar_fn_words.insert(func_word);
            return Ok((func_word, SlotType::Fn));
        }
        // stage the capture block on the overlay, in the order the
        // synthesized body binds it; a capture whose place is a closure
        // word refuses (the callable-classification seam: the body's
        // capture slot would classify Fn and the Ref word cannot sit there)
        let mut staged: Vec<SlotType> = Vec::new();
        let mut moves: Vec<u8> = Vec::new();
        for &(bind, ty) in captures {
            let place = self
                .binders
                .get(&bind)
                .cloned()
                .ok_or_else(|| self.missing_binder(span))?;
            if self.is_closure_word(&place, ty)? {
                return Err(LowerError::unsupported(
                    span,
                    "a lambda capturing a closure",
                    "the capture block would carry a Ref word behind a Fn-classified capture \
                     slot; pass the callable as a parameter instead",
                ));
            }
            // a captured deinit value's hook has no death site: the
            // closure object outlives the frame, but its death frees the
            // captured counts through the object cascade — the VM side
            // dispatches no user code there
            if self.deinit_claim(ty, span)?.is_some() {
                return Err(LowerError::unsupported(
                    span,
                    "a lambda capturing a `~Copyable` value with a deinit",
                    "the closure object's death cannot dispatch the hook yet; pass the value as a \
                     parameter instead",
                ));
            }
            // a fn-typed capture must prove scalar provenance: a
            // callable-defaulted parameter (or an alias of one) may hold a
            // closure's Ref word at runtime, and the copy-in would write it
            // into the Fn-classified capture slot
            if matches!(&*self.shapes.shape(ty)?, Shape::Word(SlotType::Fn)) {
                let addrs = self
                    .word_addrs(&place, ty)
                    .map_err(|err| err.respanned(span))?;
                let proven = addrs.len() == 1 && self.scalar_fn_words.contains(&addrs[0].slot);
                if !proven {
                    return Err(LowerError::unsupported(
                        span,
                        "a lambda capturing a callable binding",
                        "the capture cannot prove a non-capturing scalar; a callable-defaulted \
                         parameter may hold a closure at runtime — pass the callable as a \
                         parameter instead",
                    ));
                }
            }
            for addr in self
                .word_addrs(&place, ty)
                .map_err(|err| err.respanned(span))?
            {
                // the capture is a migration point: the construction
                // acquires its own count per Ref capture inside the op,
                // so a produced temporary (an arena StrCat result, a
                // borrow KSTR) transits here and keeps its counted
                // spelling — the count-bearing capture site refuses the
                // arena and borrow servings
                self.rc.captured(addr.slot);
                moves.push(addr.slot);
                staged.push(addr.slot_ty);
            }
        }
        // the lambda's evidence words ride the capture block as its
        // trailing words — one per pair: this frame staged them at its
        // own entry, so the moves read the evidence slots and the
        // closure carries the words into every frame the callable enters
        for pair in evidence {
            let slot = self
                .evidence
                .get(&(pair.var.clone(), pair.method))
                .copied()
                .ok_or_else(|| mismatch("a lambda's runtime-evidence demand"))?;
            moves.push(slot);
            staged.push(SlotType::I64);
        }
        self.guard_staged_count(staged.len())?;
        for (word, slot) in moves.iter().enumerate() {
            self.emit(Op::Mov {
                dst: Slot::overlay(u8::try_from(word).expect("overlay word fits u8")),
                src: Slot::body(*slot),
            });
            // the construction acquires its own count per Ref capture
            // inside the op: a staged capture is a borrow, never a dup or
            // a forward, and the captured word's own placement migrated
            // at the collection above
        }
        self.overlay_site(&staged, &[]);
        let dst = self.temp_word(SlotType::Ref)?;
        self.emit(Op::ClosureNew {
            dst,
            func: func_word,
            n: u8::try_from(staged.len()).expect("capture count fits u8"),
        });
        self.rc.produced(dst);
        self.fn_words.insert(dst, index);
        Ok((dst, SlotType::Ref))
    }

    /// One internal seam call to a builtin control native [E2]: the
    /// staged words are body slots with their word kinds, the result
    /// words land on the overlay for the caller to read. No seam site is
    /// ever in tail position — the emissions below keep them inside their
    /// own blocks.
    fn seam_call(
        &mut self,
        native: &Native,
        staged: &[(u8, SlotType)],
        result: &[SlotType],
    ) -> Result<(), LowerError> {
        self.guard_staged_count(staged.len())?;
        self.rc.native_open();
        let kinds: Vec<SlotType> = staged.iter().map(|(_, kind)| *kind).collect();
        for (index, &(slot, _)) in staged.iter().enumerate() {
            self.emit(Op::Mov {
                dst: Slot::overlay(u8::try_from(index).expect("overlay word fits u8")),
                src: Slot::body(slot),
            });
            // a staged Ref is a transient borrow window, the native call
            // convention's own discipline
            if kinds[index] == SlotType::Ref {
                self.rc.staged_native(slot, false);
            }
        }
        self.overlay_site(&kinds, result);
        let kidx = self.pool(Constant::FuncRef {
            key: self.native_key(native.module),
            func: native.index,
        });
        self.emit(Op::Call {
            callee: kidx,
            nargs: u8::try_from(staged.len()).expect("argument count fits u8"),
        });
        self.rc.native_done(&mut self.code);
        Ok(())
    }

    /// A `nursery` block [E2]: the seam opens a fresh nursery, the body
    /// runs with its `spawn` sites bound to the id word, and scope exit
    /// joins — the boundary answers a clean join or the first failure's
    /// error payload, and the payload re-spans into the enclosing
    /// function's return union exactly like a `?` exit: child errors
    /// union out at the boundary. A child that died by a real trap
    /// re-raises on the joiner (let-it-crash surfaces at the boundary);
    /// a payload outside the boundary's containment mask refuses in the
    /// seam instead of corrupting the union's tag space.
    fn nursery_value(&mut self, e: &Expr) -> Result<Place, LowerError> {
        let Expr::Nursery { span, body } = e else {
            return Err(mismatch("a nursery block"));
        };
        if let Some(exit) = stray_exit(body) {
            return Err(LowerError::unsupported(
                exit,
                "an early exit inside a nursery block",
                "the block's scope exit must join its children; route `return` and `?` around it",
            ));
        }
        let open = crate::native::lookup("nursery_open")
            .ok_or_else(|| mismatch("the nursery open seam"))?;
        let join = crate::native::lookup("nursery_boundary")
            .ok_or_else(|| mismatch("the nursery boundary seam"))?;
        self.scope_push();
        let id_word = self.temp_word(SlotType::I64)?;
        self.seam_call(open, &[], &[SlotType::I64])?;
        self.emit(Op::Mov {
            dst: Slot::body(id_word),
            src: Slot::overlay(0),
        });
        self.nurseries.push(id_word);
        let body_val = self.block_inner(body);
        self.nurseries.pop();
        let _ = body_val?;
        // The payloaded members this block's spawned bodies can cancel:
        // the operands' member sets over the spawned children's cancel
        // sites (the containment proof's own walk), kept to the return
        // union's members that carry payload words — exactly the members
        // the mask must admit beyond its bare set. Each member's flat
        // word span is the boundary answer's tail (one overlay word per
        // payload word in declaration order), so the joiner's overlay
        // widens per member too.
        let layout = self.shapes.shape(self.ret_ty)?;
        let mut payloaded: Vec<(TypeId, u8, &[MemberField])> = Vec::new();
        if let Shape::Union(u) = &*layout {
            let mut operand_spans = Vec::new();
            spawned_cancel_operands(body, &mut operand_spans);
            let mut reachable: Vec<TypeId> = Vec::new();
            for operand in &operand_spans {
                let ty = self.val(*operand)?.ty;
                for member in self.members(ty) {
                    if !reachable.contains(member) {
                        reachable.push(*member);
                    }
                }
            }
            for member in &reachable {
                let Some(entry) = u.member(*member) else {
                    continue;
                };
                if entry.fields.is_empty() {
                    continue;
                }
                let tag = self.shapes.tag_id(*member)?;
                payloaded.push((*member, tag, entry.fields.as_slice()));
            }
            for (_, _, fields) in &payloaded {
                let mut result = vec![SlotType::Tag, SlotType::Tag];
                result.extend(fields.iter().flat_map(|f| f.slots.iter().copied()));
                self.overlay_site(&[], &result);
            }
        }
        let mask = self.boundary_mask(*span, &payloaded)?;
        // the mask word always stages (the native's parameter count is
        // fixed); a zero mask answers nothing but clean joins — every
        // member a spawned cancel site could deliver was refused
        // statically
        let mask_word = self.temp_word(SlotType::I64)?;
        let kidx = self.pool(Constant::I64(mask));
        self.emit(Op::Kimm {
            dst: Slot::body(mask_word),
            kidx,
        });
        self.seam_call(
            join,
            &[(id_word, SlotType::I64), (mask_word, SlotType::I64)],
            &[SlotType::Tag, SlotType::Tag],
        )?;
        let answer = if mask != 0 {
            let words = self.shapes.words(self.ret_ty)?.clone();
            let fail = self.label();
            let disc = self.temp_word(SlotType::Bool)?;
            self.emit(Op::TagTest {
                dst: Slot::body(disc),
                src: Slot::overlay(0),
                tag: NURSERY_BOUNDARY_ERR,
            });
            self.emit(Op::Jmpif {
                cond: Slot::body(disc),
                target: fail,
            });
            // the exit consumes the placement state; the continuation
            // forks from before it
            let ok_state = self.rc.save();
            let unit = self.unit_value(*span)?.place;
            let done = self.label();
            self.emit(Op::Jmp { target: done });
            self.bind_label(fail);
            let target = self.temp_span(self.ret_ty)?;
            if !payloaded.is_empty() {
                // The delivered member is dynamic: dispatch on the
                // channel's tag word (overlay word 1) per payloaded
                // member, gathering the flat payload words into the
                // member's claimed, possibly scattered region offsets —
                // the checked read's ok-arm gather — with the delivered
                // `Ref` words' claims registering as the frame's own
                // (the joiner's ledger acquired them at the death
                // handoff). A bare member falls through to the
                // whole-tag path below.
                let mut labels = Vec::with_capacity(payloaded.len());
                for (_, tag, _) in &payloaded {
                    let cond = self.temp_word(SlotType::Bool)?;
                    self.emit(Op::TagTest {
                        dst: Slot::body(cond),
                        src: Slot::overlay(1),
                        tag: *tag,
                    });
                    let label = self.label();
                    self.emit(Op::Jmpif {
                        cond: Slot::body(cond),
                        target: label,
                    });
                    labels.push(label);
                }
                let n = u8::try_from(words.len()).expect("span width fits u8");
                let transferred: Vec<u8> = (0..n).map(|index| target.base + index).collect();
                for ((_, _, fields), label) in payloaded.iter().zip(labels) {
                    self.bind_label(label);
                    self.emit(Op::Mov {
                        dst: Slot::body(target.base),
                        src: Slot::overlay(1),
                    });
                    self.zero_init_member(target.base, &words, fields)?;
                    let mut flat = 0usize;
                    for field in *fields {
                        for offset in 0..field.width() {
                            let word = target.base
                                + field.word
                                + u8::try_from(offset).expect("field span offset fits u8");
                            self.emit(Op::Mov {
                                dst: Slot::body(word),
                                src: Slot::overlay(
                                    u8::try_from(2 + flat + offset).expect("overlay word fits u8"),
                                ),
                            });
                            if field.slots[offset] == SlotType::Ref {
                                self.rc.produced(word);
                            }
                        }
                        flat += field.width();
                    }
                    self.rc.exit(&mut self.code, &transferred);
                    self.emit(Op::RetSpan {
                        src: Slot::body(target.base),
                        n,
                    });
                    self.rc.restore(ok_state.clone());
                }
            }
            // a bare member's own tag word re-spans whole: bare members'
            // tag ids are module-wide, so the union's tag slot takes it
            // and the dead payload words zero-init (the construction
            // convention)
            self.emit(Op::Mov {
                dst: Slot::body(target.base),
                src: Slot::overlay(1),
            });
            self.zero_init_member(target.base, &words, &[])?;
            let n = u8::try_from(words.len()).expect("span width fits u8");
            let transferred: Vec<u8> = (0..n).map(|index| target.base + index).collect();
            self.rc.exit(&mut self.code, &transferred);
            self.emit(Op::RetSpan {
                src: Slot::body(target.base),
                n,
            });
            self.rc.restore(ok_state);
            self.bind_label(done);
            unit
        } else {
            // no contained error member can answer, so the span is
            // always the clean-join word; bug traps ride the call's own
            // trap path
            self.unit_value(*span)?.place
        };
        self.scope_pop();
        Ok(answer)
    }

    /// The join's containment mask [E2]: one bit per bare member of the
    /// function's return union plus one per payloaded member a spawned
    /// body can cancel — exactly the members a `cancel` payload can be
    /// through this boundary. A non-union return keeps the zero mask
    /// (every cancel site refused statically at its own span).
    fn boundary_mask(
        &mut self,
        span: Span,
        payloaded: &[(TypeId, u8, &[MemberField])],
    ) -> Result<i64, LowerError> {
        let mut mask = 0i64;
        let shape = self.shapes.shape(self.ret_ty)?;
        let Shape::Union(u) = &*shape else {
            return Ok(mask);
        };
        for member in self.members(self.ret_ty) {
            let Some(entry) = u.member(*member) else {
                continue;
            };
            if !entry.fields.is_empty() {
                continue;
            }
            let tag = self.shapes.tag_id(*member)?;
            if tag >= 64 {
                return Err(LowerError::unsupported(
                    span,
                    "a nursery boundary",
                    "the return union's error-member tags pass the one-word containment mask; \
                     keep the module's tag space under 64",
                ));
            }
            mask |= 1i64 << tag;
        }
        for (_, tag, _) in payloaded {
            let tag = *tag;
            if tag >= 64 {
                return Err(LowerError::unsupported(
                    span,
                    "a nursery boundary",
                    "the return union's error-member tags pass the one-word containment mask; \
                     keep the module's tag space under 64",
                ));
            }
            mask |= 1i64 << tag;
        }
        Ok(mask)
    }

    /// A `spawn` expression [E2]: the block becomes a synthesized child
    /// function (the lambda construction's own capture list) and the seam
    /// spawns it into the innermost open nursery as a ready fiber — the
    /// block's join drives it. The expression's value is the unit word.
    fn spawn_value(&mut self, e: &Expr) -> Result<Place, LowerError> {
        let Expr::Spawn { span, body } = e else {
            return Err(mismatch("a spawn block"));
        };
        let Some(&id_word) = self.nurseries.last() else {
            return Err(LowerError::unsupported(
                *span,
                "a spawn outside a nursery block",
                "spawn exists only inside a nursery block; bare detach requires an explicit root \
                 nursery or supervisor",
            ));
        };
        if let Some(exit) = stray_exit(body) {
            return Err(LowerError::unsupported(
                exit,
                "an early exit inside a spawned block",
                "a spawned body runs as its own fiber and completes with its own value; route \
                 `return` and `?` through the enclosing function",
            ));
        }
        let ret = self.val(*span)?.ty;
        // the body's `cancel` payloads union out through THIS function's
        // return at the enclosing nursery's join: prove the containment
        // at the spawn site, where the return union is in scope
        let mut operands = Vec::new();
        cancel_operands(body, &mut operands);
        for operand in operands {
            let ty = self.val(operand)?.ty;
            let members = self.members(ty);
            if !set_diff(members, self.members(self.ret_ty)).is_empty() {
                return Err(LowerError::unsupported(
                    operand,
                    "a `cancel` payload",
                    "the error member is not contained in the enclosing function's return union; \
                     the nursery boundary unions child errors out through the return",
                ));
            }
        }
        // a spawned child lexically inside a generic impl body carries its
        // own evidence demands: the pairs its body calls (through its own
        // nested lambdas and spawns), plus the callee demands its call
        // sites read from the plan's table, staged from this frame's
        // evidence slots into the child's entry argument area beside the
        // captures — the closure object's capture block becomes the
        // spawned fiber's entry arguments verbatim, so the words ride
        // exactly like a lambda's capture-block evidence
        let evidence = if self.bounds.is_empty() {
            Rc::new(Vec::new())
        } else {
            Rc::new(runtime_pairs(
                self.analyzed,
                self.res,
                &self.bounds,
                &Expr::Block(body.clone()),
                &self.plan.demands,
            ))
        };
        let (index, captures) = self.plan.push_spawn(
            &self.name,
            e,
            self.res,
            &self.analyzed.checked.binders,
            ret,
            evidence.clone(),
            self.bounds.clone(),
        );
        let native = crate::native::lookup("nursery_spawn")
            .ok_or_else(|| mismatch("the nursery spawn seam"))?;
        let (callable, kind) = self.child_callable(*span, index, &captures, &evidence)?;
        self.seam_call(native, &[(id_word, SlotType::I64), (callable, kind)], &[
            SlotType::I64,
        ])?;
        Ok(self.unit_value(*span)?.place)
    }

    /// Whether a place holds a capturing lambda's closure word: a
    /// fn-typed value in a Ref-classified word (F2's classification —
    /// every fn-typed slot reads the scalar FuncRef directly).
    fn is_closure_word(&mut self, place: &Place, ty: Ty) -> Result<bool, LowerError> {
        Ok(matches!(place.shape, PlaceShape::Word(SlotType::Ref))
            && matches!(&*self.shapes.shape(ty)?, Shape::Word(SlotType::Fn)))
    }

    /// The callee's declared parameter word kinds, when statically known:
    /// a local callee's checked signature or a generic builtin's table
    /// entry. A closure word staged against a `Fn` entry lands in the
    /// callable-defaulted parameter as a borrow; a callee whose parameter
    /// words are unknown keeps the closure-argument refusal.
    fn expected_param_words(&mut self, target: Callee) -> Option<Vec<SlotType>> {
        match target {
            Callee::Builtin(_) | Callee::Evidence(_) => None,
            Callee::Generic(generic) => Some(generic.params.to_vec()),
            // a bound extern's staged words are its written signature's
            // (H5): the scalar slice validated at resolution, the same
            // derivation the linked arm takes
            Callee::Bound { def, .. } | Callee::Foreign { def, .. } => self
                .analyzed
                .checked
                .def_types
                .get(&def)
                .copied()
                .and_then(|fn_ty| fn_sig_of(&self.analyzed.decls.universe, fn_ty))
                .and_then(|(params, _)| {
                    let mut words = Vec::with_capacity(params.len());
                    for ty in params {
                        words.extend_from_slice(&self.shapes.words(*ty).ok()?);
                    }
                    Some(words)
                }),
            Callee::Local(index) => {
                let planned = self.plan.functions().get(index as usize)?;
                if planned.decl.is_some() {
                    return self.decl_param_words(index);
                }
                planned
                    .def
                    .and_then(|def| self.analyzed.checked.def_types.get(&def).copied())
                    .and_then(|fn_ty| fn_sig_of(&self.analyzed.decls.universe, fn_ty))
                    .and_then(|(params, _)| {
                        let mut words = Vec::with_capacity(params.len());
                        for ty in params {
                            words.extend_from_slice(&self.shapes.words(*ty).ok()?);
                        }
                        Some(words)
                    })
            },
        }
    }

    /// A closure word staged at position `at` of a call's staged span: it
    /// lands legally only in a callable-defaulted `Fn` parameter. A callee
    /// whose parameter words are not statically known, or a non-fn word at
    /// the position, keeps the closure-argument refusal.
    fn guard_callable_staged(
        expected: Option<&[SlotType]>,
        at: usize,
        span: Span,
    ) -> Result<(), LowerError> {
        if !expected.is_some_and(|words| words.get(at) == Some(&SlotType::Fn)) {
            return Err(LowerError::unsupported(
                span,
                "a capturing closure as a call argument",
                "the parameter is not a callable-defaulted fn slot; a closure is a Ref word and \
                 only a fn-typed parameter admits it",
            ));
        }
        Ok(())
    }

    /// A closure word against an interior-span generic: the dynamic span
    /// (a fold's init) shifts every word behind it, so the static
    /// expected list names the leading and trailing named words alone.
    /// A closure admits the trailing callable slot — the final named
    /// word — never the span's own words, which are the folded value.
    fn guard_interior_closure_slot(
        expected: Option<&[SlotType]>,
        trailing: bool,
        span: Span,
    ) -> Result<(), LowerError> {
        if !trailing || !expected.is_some_and(|words| words.last() == Some(&SlotType::Fn)) {
            return Err(LowerError::unsupported(
                span,
                "a capturing closure as a call argument",
                "the parameter is not a callable-defaulted fn slot; a closure is a Ref word and \
                 only a fn-typed parameter admits it",
            ));
        }
        Ok(())
    }

    /// A callable value widened into a var span refuses here: the span
    /// classifies its fn-typed word Fn, so the re-storable slot takes
    /// only the proven scalar words (a named fn, a non-capturing
    /// lambda). A capturing closure is Ref-classified behind the use,
    /// and an unproven callable word (a callable-defaulted binding, an
    /// alias of one) may hold a closure's Ref at runtime — either lands
    /// a Ref word in the re-storable Fn slot and traps at the store.
    fn refuse_closure_widen(
        &mut self,
        val: &TypedPlace,
        declared: Ty,
        span: Span,
    ) -> Result<(), LowerError> {
        if self.is_closure_word(&val.place, declared)? {
            return Err(LowerError::unsupported(
                span,
                "a capturing closure in a var binding",
                "a var's fn-typed slot is re-storable and stays the scalar Fn word; construct a \
                 fresh closure for each binding instead",
            ));
        }
        if matches!(&*self.shapes.shape(declared)?, Shape::Word(SlotType::Fn))
            && let [addr] = self.word_addrs(&val.place, val.ty)?.as_slice()
            && addr.slot_ty == SlotType::Fn
            && !self.scalar_fn_words.contains(&addr.slot)
        {
            return Err(LowerError::unsupported(
                span,
                "an unproven callable in a var binding",
                "the word may hold a closure's Ref at runtime; only a named fn or a non-capturing \
                 lambda inhabits a re-storable fn-typed var slot",
            ));
        }
        Ok(())
    }

    /// A callable word widened into a union region's fn-typed payload
    /// slot keeps the var slot's provenance rule: the payload word is
    /// Fn-classified and every extraction re-reads it, so a capturing
    /// closure (a Ref-classified word) or an unproven callable word (a
    /// callable-defaulted binding's, an alias of one — the word may hold
    /// a closure's Ref at runtime) lands a Ref word there and traps;
    /// only the proven scalar words (a named fn, a non-capturing
    /// lambda) inhabit the slot.
    fn refuse_union_fn_payload(&mut self, src: &TypedPlace, span: Span) -> Result<(), LowerError> {
        // a word addressed inside a span place entered through its own
        // provenance guard (a union region's construction here, a
        // collection element's or a tuple element's staging), so a
        // read-back re-widen trusts it
        if matches!(src.place.shape, PlaceShape::Span(_)) {
            return Ok(());
        }
        if self.is_closure_word(&src.place, src.ty)? {
            return Err(LowerError::unsupported(
                span,
                "a capturing closure in a union payload slot",
                "the union's fn-typed payload slot is Fn-classified and the closure word is a Ref \
                 word; store the closure in a plain fn-typed binding instead",
            ));
        }
        if let [addr] = self.word_addrs(&src.place, src.ty)?.as_slice()
            && addr.slot_ty == SlotType::Fn
            && !self.scalar_fn_words.contains(&addr.slot)
        {
            return Err(LowerError::unsupported(
                span,
                "an unproven callable in a union payload slot",
                "the word may hold a closure's Ref at runtime; only a named fn or a non-capturing \
                 lambda inhabits a union's fn-typed payload slot",
            ));
        }
        Ok(())
    }

    /// A fn-typed word answer keeps the var slot's provenance rule at
    /// every staging site — the join arms, the extraction family's
    /// fallbacks and handler bodies: the answer word is Fn-classified
    /// and every later re-read (the call, the binding, the re-storable
    /// slot) trusts it scalar, so a capturing closure (a Ref-classified
    /// word) or an unproven callable word (a callable-defaulted
    /// binding's, an alias of one) lands a Ref word there, traps, and
    /// strands the count; only the proven scalar words answer. A word
    /// addressed inside a span place passed its own guard and stays
    /// trusted.
    fn refuse_fn_word_answer(
        &mut self,
        dst_ty: Ty,
        src: &TypedPlace,
        span: Span,
        closure_what: &'static str,
        unproven_what: &'static str,
    ) -> Result<(), LowerError> {
        if !matches!(&*self.shapes.shape(dst_ty)?, Shape::Word(SlotType::Fn)) {
            return Ok(());
        }
        if matches!(src.place.shape, PlaceShape::Span(_)) {
            return Ok(());
        }
        if self.is_closure_word(&src.place, src.ty)? {
            return Err(LowerError::unsupported(
                span,
                closure_what,
                "the answer word is Fn-classified and the closure word is a Ref word; keep the \
                 closure behind a callable-defaulted parameter instead",
            ));
        }
        if let [addr] = self.word_addrs(&src.place, src.ty)?.as_slice()
            && addr.slot_ty == SlotType::Fn
            && !self.scalar_fn_words.contains(&addr.slot)
        {
            return Err(LowerError::unsupported(
                span,
                unproven_what,
                "the word may hold a closure's Ref at runtime; only a named fn, a non-capturing \
                 lambda, or a proven call result answers here",
            ));
        }
        Ok(())
    }

    /// Whether a solved type's value region is a union span.
    fn is_union_ty(&mut self, ty: Ty) -> Result<bool, LowerError> {
        Ok(matches!(&*self.shapes.shape(ty)?, Shape::Union(_)))
    }

    /// A capturing closure never enters a widened union region: the
    /// member's Fn slot and the closure word's Ref classification
    /// disagree (F2), and no payload read-back contract covers that
    /// pairing yet.
    fn refuse_closure_init(&mut self, val: &TypedPlace, span: Span) -> Result<(), LowerError> {
        if self.is_closure_word(&val.place, val.ty)? {
            return Err(LowerError::unsupported(
                span,
                "a capturing closure in a union binding",
                "the member's Fn slot and the closure word's Ref classification disagree; store \
                 the closure in a plain fn-typed binding instead",
            ));
        }
        Ok(())
    }

    /// Why a resolved local has no lowered place. Inside a synthesized
    /// lambda body the missing binder is an enclosing one the capture
    /// walk did not collect — a capture shape the collector missed, so
    /// the walk reports it at its use span instead of misbinding it.
    fn missing_binder(&self, span: Span) -> LowerError {
        if self.synth {
            LowerError::unsupported(
                span,
                "a captured binding",
                "the capture collector missed this use's shape; report the construct",
            )
        } else {
            LowerError::unsupported(span, "a local", "it has no lowered place")
        }
    }

    /// One interpolating string: a STRCAT chain over its parts; a hole
    /// is `str(e)` [F6] — printable primitives route through their str
    /// natives, a string or char passes its Ref word through, and an
    /// operand whose `show` the checker solved at the hole's own span
    /// dispatches through the ordinary member-call machinery.
    fn string_expr(&mut self, parts: &[StrPart]) -> Result<Place, LowerError> {
        let mut acc: Option<u8> = None;
        for part in parts {
            let word = match part {
                StrPart::Text { text, .. } => {
                    let dst = self.temp_word(SlotType::Ref)?;
                    let kidx = self.pool_str(text);
                    self.kstr(dst, kidx);
                    dst
                },
                StrPart::Expr { span, expr } => {
                    // A derive-synthesized body's non-printable part hole
                    // mints one span shared by the hole and the written
                    // `.show()` call it wraps: that record belongs to the
                    // call, so the expression lowers as written, not as an
                    // implicit dispatch over its own result
                    let written_show = matches!(
                        &**expr,
                        Expr::Call {
                            span: call_span,
                            callee,
                            ..
                        } if call_span == span
                            && matches!(
                                &**callee,
                                Expr::Field { name, .. } if name.name == "show"
                            )
                    );
                    if !written_show && self.hole_show_dispatch(*span) {
                        self.show_dispatch_word(*span, (**expr).clone())?
                    } else {
                        let val = self.value(expr)?;
                        self.hole_word(expr.span(), &val)?
                    }
                },
                StrPart::Ident { span, name } => {
                    if self.hole_show_dispatch(*span) {
                        let recv = Expr::Path {
                            span: name.span,
                            path: Path {
                                span: name.span,
                                segments: vec![name.clone()],
                            },
                        };
                        self.show_dispatch_word(*span, recv)?
                    } else {
                        let ty = self.val(name.span)?.ty;
                        let path = Path {
                            span: name.span,
                            segments: vec![name.clone()],
                        };
                        let place = self.path_value(&path, name.span, ty)?;
                        self.hole_word(name.span, &TypedPlace { place, ty })?
                    }
                },
            };
            acc = Some(match acc {
                None => word,
                Some(left) => {
                    let dst = self.temp_word(SlotType::Ref)?;
                    self.emit(Op::Data {
                        op: DataOp::StrCat,
                        dst: Slot::body(dst),
                        lhs: Slot::body(left),
                        rhs: Slot::body(word),
                    });
                    // the operands are reads; the result is a fresh owner
                    self.rc.read(&mut self.code, &[left, word]);
                    self.rc.produced_strcat(dst);
                    dst
                },
            });
        }
        let base = if let Some(base) = acc {
            base
        } else {
            // the empty string literal: a plain constant, not a chain
            let dst = self.temp_word(SlotType::Ref)?;
            let kidx = self.pool_str("");
            self.kstr(dst, kidx);
            dst
        };
        Ok(Place {
            base,
            shape: PlaceShape::Word(SlotType::Ref),
        })
    }

    /// `str(e)` of one hole [F6]: the operand's checked type routes the
    /// conversion. Anything the checker did not prove printable lands as
    /// a lowering error (a `show`-carrying operand never reaches here —
    /// the checker's hole record routed it through the dispatch above).
    fn hole_word(&mut self, span: Span, val: &TypedPlace) -> Result<u8, LowerError> {
        let members = self.members(val.ty);
        let [atom] = members else {
            return Err(LowerError::unsupported(
                span,
                "an interpolation hole",
                "the operand's type is not a printable primitive and carries no solved `show` \
                 dispatch; report the construct",
            ));
        };
        let name = self
            .analyzed
            .decls
            .universe
            .atom_decl(*atom)
            .map_err(|_| mismatch("a printable atom"))?
            .name
            .clone();
        match name.as_str() {
            "string" | "char" => {
                let addrs = self.word_addrs(&val.place, val.ty)?;
                match addrs.as_slice() {
                    [only] if only.slot_ty == SlotType::Ref => Ok(only.slot),
                    _ => Err(LowerError::unsupported(
                        span,
                        "an interpolation hole",
                        "the string value does not occupy one reference word",
                    )),
                }
            },
            // a byte prints its 0-255 value [F6]: the word IS the integer
            // slot staging, so the int native renders it; interpolation
            // holes stay checker-gated to the five printable primitives,
            // so only an explicit `str(b)` reaches this arm
            "int" | "byte" => self.str_native_word("str_int", SlotType::I64, val, span),
            "float" => self.str_native_word("str_float", SlotType::F64, val, span),
            "bool" => self.str_native_word("str_bool", SlotType::Bool, val, span),
            _ => Err(LowerError::unsupported(
                span,
                "an interpolation hole",
                "the operand's type is not a printable primitive and carries no solved `show` \
                 dispatch; report the construct",
            )),
        }
    }

    /// Whether the checker solved one hole's `show` at its own span [F6]:
    /// the hole's dispatch record (the trait-dispatch or inherent table
    /// beside the solved instance, the runtime-evidence table alone for
    /// an assumption-answered hole) the synthetic member call below
    /// resolves through.
    fn hole_show_dispatch(&self, span: Span) -> bool {
        self.analyzed.checked.trait_calls.contains_key(&span)
            || self.analyzed.checked.inherent_calls.contains_key(&span)
            || self
                .analyzed
                .checked
                .hole_show_dispatches
                .contains_key(&span)
    }

    /// One hole's `show` dispatch [F6]: the receiver wrapped as the
    /// written `recv.show()` call — no surface name exists, so the
    /// checker's span-keyed record is the resolution — staging through
    /// the ordinary member-call machinery (receiver argument, evidence
    /// words, the CALLG dispatch) and answering the hole's one string
    /// word. The receiver expression is the hole's own operand, so it
    /// evaluates exactly once.
    fn show_dispatch_word(&mut self, span: Span, recv: Expr) -> Result<u8, LowerError> {
        let call = Expr::Call {
            span,
            callee: Box::new(Expr::Field {
                span,
                recv: Box::new(recv),
                name: Ident {
                    span,
                    name: "show".to_owned(),
                },
            }),
            args: Vec::new(),
            trailing: None,
        };
        let val = self.value(&call)?;
        let addrs = self.word_addrs(&val.place, val.ty)?;
        match addrs.as_slice() {
            [only] if only.slot_ty == SlotType::Ref => Ok(only.slot),
            _ => Err(LowerError::unsupported(
                span,
                "an interpolation hole",
                "the `show` dispatch did not answer one reference word",
            )),
        }
    }

    /// One total str native call over a scalar hole word: stage the word
    /// onto the overlay, call, and land the fresh string on a body temp.
    fn str_native_word(
        &mut self,
        native_name: &str,
        arg_slot: SlotType,
        val: &TypedPlace,
        span: Span,
    ) -> Result<u8, LowerError> {
        let addrs = self.word_addrs(&val.place, val.ty)?;
        let [only] = addrs.as_slice() else {
            return Err(LowerError::unsupported(
                span,
                "an interpolation hole",
                "the scalar value does not occupy one word",
            ));
        };
        if only.slot_ty != arg_slot {
            return Err(LowerError::unsupported(
                span,
                "an interpolation hole",
                "the scalar value's word and the str native's parameter disagree",
            ));
        }
        let native = crate::native::lookup(native_name)
            .unwrap_or_else(|| panic!("the str native `{native_name}` is covered"));
        self.overlay_site(&[arg_slot], &[SlotType::Ref]);
        self.rc.native_open();
        self.emit(Op::Mov {
            dst: Slot::overlay(0),
            src: Slot::body(only.slot),
        });
        let kidx = self.pool(self.funcref_const(Callee::Builtin(native)));
        self.emit(Op::Call {
            callee: kidx,
            nargs: 1,
        });
        self.rc.native_done(&mut self.code);
        let dst = self.temp_word(SlotType::Ref)?;
        self.emit(Op::Mov {
            dst: Slot::body(dst),
            src: Slot::overlay(0),
        });
        self.rc.produced(dst);
        Ok(dst)
    }

    fn path_value(&mut self, path: &Path, span: Span, ty: Ty) -> Result<Place, LowerError> {
        let [only] = path.segments.as_slice() else {
            return Err(LowerError::unsupported(
                span,
                "a multi-segment path expression",
                "qualified paths are not in the M3 contract",
            ));
        };
        match self.res.get(span) {
            Some(Res::Local { bind }) => {
                // the checker's use site: the RC death test compares
                // against the binder's last-use facts at exactly this span
                self.rc.use_of(*bind, only.span);
                self.binders
                    .get(bind)
                    .cloned()
                    .ok_or_else(|| self.missing_binder(span))
            },
            Some(Res::Def(def)) => {
                let kind = self.analyzed.package.def(*def).kind;
                match kind {
                    ds_check::DefKind::Tag | ds_check::DefKind::Case => {
                        let atom = self
                            .analyzed
                            .decls
                            .atoms
                            .get(def)
                            .copied()
                            .ok_or_else(|| mismatch("a tag construction"))?;
                        let dst = self.temp_word(SlotType::Tag)?;
                        let tag = self.shapes.tag_id(atom)?;
                        self.emit(Op::TagNew {
                            dst: Slot::body(dst),
                            tag,
                        });
                        Ok(Place {
                            base: dst,
                            shape: PlaceShape::Word(SlotType::Tag),
                        })
                    },
                    _ => {
                        // A linked const's value fact (G4): the literal is
                        // the whole fact, so the use embeds it exactly as
                        // the literal a user would have written — the same
                        // word shapes and RC treatment the owning module's
                        // own literal lowering gives it.
                        if let Some(fact) = self.analyzed.decls.foreign_consts.get(def).cloned() {
                            return self.embed_const_fact(fact, ty, span);
                        }
                        // A named fn used as a value (F2, functions are
                        // values): the same pool FuncRef const channel a
                        // non-capturing lambda mints — one copyable scalar
                        // word, resolved through the two-slot module table
                        // at call time. A bodied `fn` declaration plans as
                        // a Local binding (DefKind::Const), so planned
                        // membership — not the def kind — identifies the
                        // function value. Natives run inside their CALL
                        // (never through a frame push) and generics stage
                        // evidence words per call site, so neither has a
                        // callable word.
                        if let Some(index) = self.plan.index_of(*def) {
                            if self.plan.functions()[index as usize].free_generic {
                                return Err(LowerError::unsupported(
                                    span,
                                    "a generic function value",
                                    "the word carries no instantiation, and a call through it has \
                                     no site to stage the body's evidence words; generic calls \
                                     name their callee directly",
                                ));
                            }
                            let kidx = self.pool(Constant::FuncRef {
                                key: FuncRefKey::Dep(0), // the enclosing module
                                func: index,
                            });
                            let dst = self.temp_word(SlotType::Fn)?;
                            self.emit(Op::Kconst {
                                dst: Slot::body(dst),
                                kidx,
                            });
                            self.scalar_fn_words.insert(dst);
                            self.fn_words.insert(dst, index);
                            Ok(Place {
                                base: dst,
                                shape: PlaceShape::Word(SlotType::Fn),
                            })
                        } else if kind == ds_check::DefKind::Fn
                            || self
                                .analyzed
                                .checked
                                .def_types
                                .get(def)
                                .copied()
                                .is_some_and(|ty| {
                                    fn_sig_of(&self.analyzed.decls.universe, ty).is_some()
                                })
                        {
                            // a bodied fn that did not plan (a `var`-bound
                            // generic fn) still carries a checked fn type,
                            // so the fn type — not the def kind — routes it
                            // here
                            Err(LowerError::unsupported(
                                span,
                                "a function value",
                                "only planned module functions mint callable words; native and \
                                 unplanned generic callees have no word",
                            ))
                        } else {
                            Err(LowerError::unsupported(
                                span,
                                "a top-level value",
                                "only functions and unit tags are lowerable at top level",
                            ))
                        }
                    },
                }
            },
            None if only.name == "none" => {
                // the predeclared unit atom: no package DefId exists
                let dst = self.temp_word(SlotType::Tag)?;
                let tag = self.shapes.tag_id(TypeId::NONE)?;
                self.emit(Op::TagNew {
                    dst: Slot::body(dst),
                    tag,
                });
                Ok(Place {
                    base: dst,
                    shape: PlaceShape::Word(SlotType::Tag),
                })
            },
            _ => Err(LowerError::unsupported(
                span,
                "a name use",
                "it resolved to a method or handler name the lowerer cannot address",
            )),
        }
        .inspect(|_place| {
            let _ = ty;
        })
    }

    /// `x is T`: TAG_TESTs against the tested atoms, or a constant when
    /// the checked types already decide it (legal specialization). A
    /// multi-atom test (a written union spelling) joins one TAG_TEST
    /// per atom by jumps — the first hit answers true, the fall-through
    /// answers false.
    fn is_value(&mut self, expr: &Expr, ty: &Type, span: Span) -> Result<Place, LowerError> {
        let operand = self.value(expr)?;
        let operand_ty = self.val(expr.span())?.ty;
        // A nominal written type tests its one atom; any other written
        // type tests the members the checker solved for the test.
        let atom = self.type_atom(ty);
        let tested: &[TypeId] = match &atom {
            Ok(atom) => std::slice::from_ref(atom),
            Err(path_err) => self
                .solved_is_members(span)
                .ok_or_else(|| path_err.clone())?,
        };
        let members = self.members(operand_ty);
        // the operand's member set decides the test: its whole coverage
        // folds to true, a disjoint test to false, and the rest tests the
        // members the two sets share
        let live: Vec<TypeId> = members
            .iter()
            .copied()
            .filter(|m| tested.contains(m))
            .collect();
        let dst = self.temp_word(SlotType::Bool)?;
        // the covered fold is instantiation-stable even over type
        // variables (the variable's atom covers whatever it becomes);
        // every other shape reads a tag the variable's raw word cannot
        // answer — or folds against a member set an instantiation may
        // re-solve — so a type variable on either side of the test
        // refuses here
        if !members.is_empty() && live.len() == members.len() {
            let kidx = self.pool(Constant::Bool(true));
            self.emit(Op::Kconst {
                dst: Slot::body(dst),
                kidx,
            });
        } else if tested
            .iter()
            .chain(members.iter())
            .any(|a| self.shapes.is_var_atom(*a))
        {
            return Err(LowerError::unsupported(
                span,
                "an `is` test over a type variable",
                "a type-variable value rides one raw word, so no tag discriminates it and no \
                 member set survives instantiation; testing it needs runtime reflection",
            ));
        } else if live.is_empty() {
            let kidx = self.pool(Constant::Bool(false));
            self.emit(Op::Kconst {
                dst: Slot::body(dst),
                kidx,
            });
        } else if let [atom] = live.as_slice() {
            let src = self.tag_addr(&operand.place)?;
            let tag = self.shapes.tag_id(*atom)?;
            self.emit(Op::TagTest {
                dst: Slot::body(dst),
                src: Slot::body(src),
                tag,
            });
        } else {
            let src = self.tag_addr(&operand.place)?;
            let hit = self.label();
            let join = self.label();
            for atom in &live {
                let probe = self.temp_word(SlotType::Bool)?;
                let tag = self.shapes.tag_id(*atom)?;
                self.emit(Op::TagTest {
                    dst: Slot::body(probe),
                    src: Slot::body(src),
                    tag,
                });
                self.emit(Op::Jmpif {
                    cond: Slot::body(probe),
                    target: hit,
                });
            }
            let k_false = self.pool(Constant::Bool(false));
            self.emit(Op::Kconst {
                dst: Slot::body(dst),
                kidx: k_false,
            });
            self.emit(Op::Jmp { target: join });
            self.bind_label(hit);
            let k_true = self.pool(Constant::Bool(true));
            self.emit(Op::Kconst {
                dst: Slot::body(dst),
                kidx: k_true,
            });
            self.bind_label(join);
        }
        Ok(Place {
            base: dst,
            shape: PlaceShape::Word(SlotType::Bool),
        })
    }

    /// The tested members of a written type the nominal path cannot name
    /// (a written fn type, a union spelling): the checker solved the
    /// `is` test and recorded it keyed by the expression's span, so the
    /// solution's members are what the TAG_TESTs discriminate by.
    /// Anything else keeps the path spelling's refusal.
    fn solved_is_members(&self, span: Span) -> Option<&'a [TypeId]> {
        let &solved = self.analyzed.checked.is_tests.get(&span)?;
        Some(self.members(solved))
    }

    /// The atom a written type names; nominal single-segment paths only.
    fn type_atom(&self, ty: &Type) -> Result<TypeId, LowerError> {
        let Type::Path { path, args, .. } = ty else {
            return Err(LowerError::unsupported(
                ty.span(),
                "a written type",
                "only nominal single-segment type names resolve here",
            ));
        };
        if !args.is_empty() || path.segments.len() != 1 {
            return Err(LowerError::unsupported(
                ty.span(),
                "a written type",
                "only nominal single-segment type names resolve here",
            ));
        }
        self.path_atom(&path.segments[0].name)
            .ok_or_else(|| mismatch("a type atom"))
    }

    fn path_atom(&self, name: &str) -> Option<TypeId> {
        // `none` is the universe's predeclared unit atom [B4], never a
        // package declaration
        if name == "none" {
            return Some(TypeId::NONE);
        }
        let def = self
            .analyzed
            .package
            .lookup(name)
            .or_else(|| self.analyzed.decls.foreign_type(name))?;
        self.analyzed.decls.atoms.get(&def).copied()
    }

    /// `x.f`: the member payload word, addressed through the receiver's
    /// checked (possibly refined) type.
    fn field_access(&mut self, e: &Expr, static_ty: Ty) -> Result<Place, LowerError> {
        let Expr::Field { recv, name, .. } = e else {
            return Err(mismatch("a field access"));
        };
        // A module alias's qualified const (G4): the checker bound the
        // linked binding's def at the member's span, so the value embeds
        // through the same literal fact the selection spelling lowers
        // — no receiver word exists.
        if let Some(&Res::Def(def)) = self.res.get(name.span)
            && let Some(fact) = self.analyzed.decls.foreign_consts.get(&def).cloned()
        {
            return self.embed_const_fact(fact, static_ty, name.span);
        }
        let recv_val = self.value(recv)?;
        // the receiver's recorded type carries the refinement: a member
        // name inside an `is`/match arm [B2]
        let recv_ty = self.val(recv.span())?.ty;
        let PlaceShape::Span(underlying) = recv_val.place.shape else {
            return Err(mismatch("a field access on a scalar"));
        };
        let place_shape = self.shapes.shape(underlying)?;
        let field: &MemberField = match &*place_shape {
            Shape::Union(_) => {
                let atom = self.only_atom(recv_ty, e.span())?;
                let fields = place_shape
                    .member_fields(atom)
                    .ok_or_else(|| mismatch("a union member field"))?;
                fields
                    .iter()
                    .find(|f| f.name.as_deref() == Some(name.name.as_str()))
                    .ok_or_else(|| mismatch("a union member field"))?
            },
            Shape::Struct(s) => s
                .fields
                .iter()
                .find(|f| f.name.as_deref() == Some(name.name.as_str()))
                .ok_or_else(|| mismatch("a struct field"))?,
            Shape::Word(_) | Shape::Tuple(_) => return Err(mismatch("a field access")),
        };
        // A word-shaped field reads as its word; every wider-shaped
        // field (a struct, a tuple, a union) reads as the span the
        // expression's own checked type names, nested access resolving
        // through it. The choice keys on the field's checked shape,
        // never its raw width: a one-word struct field still spans, so
        // receiver staging and a nested access address its words.
        let use_ty = self.val(e.span())?.ty;
        let shape = if matches!(&*self.shapes.shape(use_ty)?, Shape::Word(_)) {
            PlaceShape::Word(field.slot())
        } else {
            PlaceShape::Span(use_ty)
        };
        Ok(Place {
            base: recv_val.place.base + field.word,
            shape,
        })
    }

    /// A struct literal: a fresh span, fields written by name [B3].
    fn struct_literal(&mut self, e: &Expr, static_ty: Ty) -> Result<Place, LowerError> {
        let Expr::Struct { fields, .. } = e else {
            return Err(mismatch("a struct literal"));
        };
        // an annotated union destination names its member here (the
        // checker propagated the annotation into the literal): a
        // deinit-carrying member refuses at the literal's own span
        if self.members(static_ty).len() > 1 {
            for atom in self.members(static_ty) {
                if let Some(def) = self.analyzed.decls.defs_of_atoms.get(atom).copied()
                    && self.plan.declares_deinit(def)
                {
                    return Err(LowerError::unsupported(
                        e.span(),
                        "a deinit-carrying union member",
                        "the union's death decs the member's payload words and frees nested \
                         objects, but dispatches no user deinit; a deinit value's death is its \
                         own binding's",
                    ));
                }
            }
        }
        let shape = self.shapes.shape(static_ty)?;
        let Shape::Struct(s) = &*shape else {
            return Err(mismatch("a struct literal"));
        };
        let target = self.temp_span(static_ty)?;
        for (name, value) in fields {
            let index = s
                .fields
                .iter()
                .position(|f| f.name.as_deref() == Some(name.name.as_str()))
                .ok_or_else(|| {
                    LowerError::unsupported(
                        name.span,
                        "a struct literal field",
                        "the field's type stayed outside the core's span vocabulary (a callable- \
                         or foreign-typed field), so the planned shape carries no such field",
                    )
                })?;
            let field = &s.fields[index];
            let (v, value_span) = if let Some(expr) = value {
                let v = self.value(expr)?;
                (v, expr.span())
            } else {
                // punning: the field name is the value's name
                let bind = self.scope_lookup(&name.name).ok_or_else(|| {
                    LowerError::unsupported(
                        name.span,
                        "a punned struct field",
                        "the name does not name an in-scope binding",
                    )
                })?;
                self.rc.use_of(bind, name.span);
                let place = self
                    .binders
                    .get(&bind)
                    .cloned()
                    .ok_or_else(|| mismatch("a punned field's binding"))?;
                let ty = self
                    .analyzed
                    .checked
                    .binders
                    .get(&bind)
                    .map_or(static_ty, |f| f.ty);
                (TypedPlace { place, ty }, name.span)
            };
            let addrs = self
                .word_addrs(&v.place, v.ty)
                .map_err(|err| err.respanned(value_span))?;
            if addrs.len() != field.width() {
                // A member value into a union-typed field re-spans
                // through the field's own region; anything else is the
                // honest mismatch.
                let decl_expr = self.field_decl_expr(static_ty, index);
                self.store_union_member_field(target.base + field.word, decl_expr, &v, name.span)?;
                continue;
            }
            // a fn-typed field must prove scalar provenance: a closure word
            // (or a callable-defaulted binding that may hold one at
            // runtime) would write a Ref word into the Fn-classified field
            // slot and trap at the store
            if field.slot() == SlotType::Fn && !self.scalar_fn_words.contains(&addrs[0].slot) {
                return Err(LowerError::unsupported(
                    name.span,
                    "a fn-typed struct field",
                    "the value cannot prove a non-capturing scalar word; a closure or an unproven \
                     callable binding cannot inhabit a struct field",
                ));
            }
            self.store_field_span(target.base, field, &addrs);
        }
        // the constructed value's deinit claim anchors its base word
        // (C5); the value dies unnamed unless a binder aliases it
        if let Some(claim) = self.deinit_claim(static_ty, e.span())? {
            self.overlay_site(&claim.staged, &[SlotType::I64]);
            let words = self.ref_words(target.base, static_ty)?;
            let band = self.claim_band_word(target.base)?;
            self.rc
                .claimed(&mut self.code, target.base, claim.hook, band, &words);
        }
        Ok(target)
    }

    /// The declared type expression of one field of a struct atom, by
    /// position: the shape's fields pair with the declaration's, so the
    /// union store reads the field's own union sides.
    fn field_decl_expr(&self, ty: Ty, index: usize) -> Option<&'a TypeExpr> {
        let atom = self.only_atom(ty, no_span()).ok()?;
        let decl = self.analyzed.decls.universe.atom_decl(atom).ok()?;
        match &decl.kind {
            AtomKind::Struct(fields) => fields.get(index).map(|f| &f.ty),
            _ => None,
        }
    }

    /// Stores one member value into a union-typed literal field's
    /// region: the field's words are the union's tag-plus-payload span,
    /// and a bare member value re-spans through the same
    /// tag/zero/write construction the annotated-binding widen uses, so
    /// the field physically carries its union's tag and zeroed dead
    /// payload words. A wider union value refuses — narrow it first.
    fn store_union_member_field(
        &mut self,
        region_base: u8,
        decl_expr: Option<&'a TypeExpr>,
        v: &TypedPlace,
        span: Span,
    ) -> Result<(), LowerError> {
        let mismatched = || mismatch("a struct literal field's span");
        let Some(expr) = decl_expr else {
            return Err(mismatched());
        };
        let Some(members) = expr_members(expr) else {
            return Err(mismatched());
        };
        let shape = self.shapes.members_shape(&members, 1)?;
        let Shape::Union(u) = &shape else {
            return Err(mismatched());
        };
        if self.members(v.ty).len() > 1 {
            return Err(LowerError::unsupported(
                span,
                "a union-typed struct field",
                "the field stores one member value; narrow a union value through a match first",
            ));
        }
        let atom = self.only_atom(v.ty, span)?;
        let member = u.member(atom).ok_or_else(mismatched)?;
        // a callable word entering the union's fn-typed payload slot
        // keeps the var slot's provenance rule: a closure's Ref word
        // would trap there the same way it traps a re-storable var slot
        self.refuse_union_fn_payload(v, span)?;
        let tag = self.shapes.tag_id(atom)?;
        self.emit(Op::TagNew {
            dst: Slot::body(region_base),
            tag,
        });
        self.zero_init_member(region_base, &u.words, &member.fields)?;
        let addrs = self.word_addrs(&v.place, v.ty)?;
        let srcs = ordered_field_srcs(&addrs, &member.fields)?;
        for (field, srcs) in member.fields.iter().zip(srcs) {
            self.store_field_span(region_base, field, srcs);
        }
        Ok(())
    }

    /// One anonymous tuple `(e0, e1, ...)`: a fresh frame span, one word
    /// per element, moved in evaluation order (B3). Each element lowers
    /// as a single word (the shape's one-word-per-element rule); a
    /// union-typed element distributed the product into several tuple
    /// atoms, which the boundary rejects — narrow the element first.
    fn tuple_literal(&mut self, items: &[Expr], ty: Ty, span: Span) -> Result<Place, LowerError> {
        if self.members(ty).len() != 1 {
            return Err(LowerError::unsupported(
                span,
                "a tuple with a union-typed element",
                "the product distributed into several tuple atoms; narrow the element through a \
                 match before the tuple",
            ));
        }
        let shape = self.shapes.shape(ty)?;
        let Shape::Tuple(s) = &*shape else {
            return Err(mismatch("a tuple construction"));
        };
        if s.fields.len() != items.len() {
            return Err(mismatch("a tuple construction's arity"));
        }
        let target = self.temp_span(ty)?;
        for (index, item) in items.iter().enumerate() {
            let field = &s.fields[index];
            let v = self.value(item)?;
            let addr = single(&self.word_addrs(&v.place, v.ty)?, item.span())?;
            // a callable element proves a non-capturing scalar word: the
            // span's count traffic copies only Ref-classified fields, so a
            // closure's count rides untracked and the read-back hands a Ref
            // word to a Fn-classified element slot
            if field.slot() == SlotType::Fn && !self.scalar_fn_words.contains(&addr.slot) {
                return Err(LowerError::unsupported(
                    item.span(),
                    "a callable tuple element",
                    "the element must prove a non-capturing scalar word (a named fn, a \
                     non-capturing lambda); a closure or an unproven callable binding cannot \
                     inhabit a tuple element",
                ));
            }
            self.emit(Op::Mov {
                dst: Slot::body(target.base + field.word),
                src: Slot::body(addr.slot),
            });
            // a variable-typed element's declared word is Raw under a
            // counted instantiation, so the copy registration covers both
            // classifications (the RC ops check the runtime kind)
            if matches!(field.slot(), SlotType::Ref | SlotType::Raw) {
                self.rc
                    .copied(&mut self.code, target.base + field.word, addr.slot);
            }
        }
        Ok(target)
    }

    /// Binary operators: typed uops for scalars, short-circuit chains for
    /// `&&`/`||`, memberwise chains for structural `==` [B9], and the
    /// concat call for string `+` [B8].
    fn binary_value(&mut self, e: &Expr, static_ty: Ty) -> Result<Place, LowerError> {
        let Expr::Binary {
            op,
            left,
            right,
            span,
        } = e
        else {
            return Err(mismatch("a binary expression"));
        };
        if matches!(op, BinaryOp::And | BinaryOp::Or) {
            return self.bool_chain(*op == BinaryOp::And, left, right);
        }
        if *op == BinaryOp::In
            && let Expr::Range {
                inclusive,
                start,
                end,
                ..
            } = peel_parens(right)
        {
            return self.in_range_value(left, start, end, *inclusive, *span);
        }
        if *op == BinaryOp::In {
            let rhs_ty = self.val(right.span())?.ty;
            if self.shapes.list_element(rhs_ty).is_some() {
                return self.in_list_value(left, right, *span);
            }
            return Err(LowerError::unsupported(
                *span,
                "a membership over a non-list collection",
                "`in` lowers over int ranges and one list; membership over other collections is \
                 not ruled yet",
            ));
        }
        // string `+` is the concat native's call shape: its borrow
        // window opens before operand evaluation, so a later operand's
        // own last-use dec cannot free a word the CALL still reads
        let str_add = *op == BinaryOp::Add
            && self.is_string_add_ty(self.val(left.span())?.ty, self.val(right.span())?.ty);
        if str_add {
            self.rc.native_open();
        }
        // The scalar destination offer: the assignment store or the
        // call staging that owns this expression may have offered its
        // word to this node. The offer is taken here, before operand
        // evaluation, so no nested node can write the offered word; an
        // arm that declines restores it for the offering site to read
        // back as unconsumed.
        let offered = match op {
            BinaryOp::Add if !str_add => self.scalar_dst.take(),
            BinaryOp::Sub
            | BinaryOp::Mul
            | BinaryOp::Div
            | BinaryOp::Rem
            | BinaryOp::WrapAdd
            | BinaryOp::WrapSub
            | BinaryOp::WrapMul
            | BinaryOp::Shl
            | BinaryOp::Shr
            | BinaryOp::BitAnd
            | BinaryOp::BitXor
            | BinaryOp::BitOr => self.scalar_dst.take(),
            _ => None,
        };
        let lhs = self.value(left)?;
        if str_add {
            let words = self
                .word_addrs(&lhs.place, lhs.ty)
                .map_err(|err| err.respanned(*span))?;
            self.rc.native_reads(words.iter().map(|addr| addr.slot));
        }
        let rhs = self.value(right)?;
        let (place, landed) = match op {
            BinaryOp::Eq | BinaryOp::Ne => (
                self.eq_chain(&lhs, &rhs, e.span())
                    .map_err(|err| err.respanned(e.span()))?,
                false,
            ),
            BinaryOp::Add if str_add => (self.concat_value(&lhs, &rhs, static_ty, *span)?, false),
            BinaryOp::Add => {
                self.arith_dst(DataOp::AddI64, DataOp::AddF64, &lhs, &rhs, offered, *span)?
            },
            BinaryOp::Sub => {
                self.arith_dst(DataOp::SubI64, DataOp::SubF64, &lhs, &rhs, offered, *span)?
            },
            BinaryOp::Mul => {
                self.arith_dst(DataOp::MulI64, DataOp::MulF64, &lhs, &rhs, offered, *span)?
            },
            BinaryOp::Div => {
                self.arith_dst(DataOp::DivI64, DataOp::DivF64, &lhs, &rhs, offered, *span)?
            },
            BinaryOp::Rem => {
                self.reject_float_rem(&lhs, *span)?;
                self.arith_dst(DataOp::ModI64, DataOp::ModI64, &lhs, &rhs, offered, *span)?
            },
            BinaryOp::Lt => (
                self.compare(DataOp::LtI64, DataOp::LtF64, &lhs, &rhs, *span)?,
                false,
            ),
            BinaryOp::Le => (
                self.compare(DataOp::LeI64, DataOp::LeF64, &lhs, &rhs, *span)?,
                false,
            ),
            BinaryOp::Gt => (
                self.compare(DataOp::GtI64, DataOp::GtF64, &lhs, &rhs, *span)?,
                false,
            ),
            BinaryOp::Ge => (
                self.compare(DataOp::GeI64, DataOp::GeF64, &lhs, &rhs, *span)?,
                false,
            ),
            BinaryOp::WrapAdd => {
                self.reject_float_wrap(&lhs, *span)?;
                self.arith_dst(
                    DataOp::WrapAddI64,
                    DataOp::WrapAddI64,
                    &lhs,
                    &rhs,
                    offered,
                    *span,
                )?
            },
            BinaryOp::WrapSub => {
                self.reject_float_wrap(&lhs, *span)?;
                self.arith_dst(
                    DataOp::WrapSubI64,
                    DataOp::WrapSubI64,
                    &lhs,
                    &rhs,
                    offered,
                    *span,
                )?
            },
            BinaryOp::WrapMul => {
                self.reject_float_wrap(&lhs, *span)?;
                self.arith_dst(
                    DataOp::WrapMulI64,
                    DataOp::WrapMulI64,
                    &lhs,
                    &rhs,
                    offered,
                    *span,
                )?
            },
            BinaryOp::Shl
            | BinaryOp::Shr
            | BinaryOp::BitAnd
            | BinaryOp::BitOr
            | BinaryOp::BitXor => {
                self.reject_float_shift_bitwise(&lhs, *span)?;
                let op = match op {
                    BinaryOp::Shl => DataOp::ShlI64,
                    BinaryOp::Shr => DataOp::ShrI64,
                    BinaryOp::BitAnd => DataOp::BitAndI64,
                    BinaryOp::BitOr => DataOp::BitOrI64,
                    BinaryOp::BitXor => DataOp::BitXorI64,
                    _ => unreachable!("the outer arm partitions the operators"),
                };
                self.arith_dst(op, op, &lhs, &rhs, offered, *span)?
            },
            BinaryOp::And | BinaryOp::Or | BinaryOp::In => unreachable!("handled above"),
        };
        if !landed {
            self.scalar_dst = offered;
        }
        if *op == BinaryOp::Ne {
            // `!=` over spans: invert the eq chain's bool
            let true_word = self.temp_word(SlotType::Bool)?;
            let kidx = self.pool(Constant::Bool(true));
            self.emit(Op::Kconst {
                dst: Slot::body(true_word),
                kidx,
            });
            let dst = self.temp_word(SlotType::Bool)?;
            self.emit(Op::Data {
                op: DataOp::NeqBool,
                dst: Slot::body(dst),
                lhs: Slot::body(place.base),
                rhs: Slot::body(true_word),
            });
            return Ok(Place {
                base: dst,
                shape: PlaceShape::Word(SlotType::Bool),
            });
        }
        Ok(place)
    }

    fn arith(
        &mut self,
        int_op: DataOp,
        float_op: DataOp,
        lhs: &TypedPlace,
        rhs: &TypedPlace,
        span: Span,
    ) -> Result<Place, LowerError> {
        let op = self.pick_int_float(int_op, float_op, lhs, span)?;
        self.emit_data(op, lhs, rhs, span)
    }

    /// arith over the offered scalar destination: the data op writes
    /// the offered word directly when its result kind matches the
    /// destination's own, and the fresh temp otherwise. The VM reads
    /// both operands before writing dst, so the offered word may alias
    /// an operand read — the same guarantee the compound store relies
    /// on.
    fn arith_dst(
        &mut self,
        int_op: DataOp,
        float_op: DataOp,
        lhs: &TypedPlace,
        rhs: &TypedPlace,
        dst: Option<ScalarDst>,
        span: Span,
    ) -> Result<(Place, bool), LowerError> {
        let op = self.pick_int_float(int_op, float_op, lhs, span)?;
        let Some(dst) = dst else {
            return Ok((self.emit_data(op, lhs, rhs, span)?, false));
        };
        let l = single(&self.word_addrs(&lhs.place, lhs.ty)?, span)?;
        let slot = op.result_slot(l.slot_ty);
        if slot != dst.slot() {
            return Ok((self.emit_data(op, lhs, rhs, span)?, false));
        }
        let r = single(&self.word_addrs(&rhs.place, rhs.ty)?, span)?;
        let word = dst.word();
        self.emit(Op::Data {
            op,
            dst: dst.slot_word(),
            lhs: Slot::body(l.slot),
            rhs: Slot::body(r.slot),
        });
        Ok((
            Place {
                base: word,
                shape: PlaceShape::Word(slot),
            },
            true,
        ))
    }

    /// arith writing the pinned word `dst` — the compound assignment's
    /// direct scalar store over the var's own word.
    fn arith_at(
        &mut self,
        int_op: DataOp,
        float_op: DataOp,
        lhs: &TypedPlace,
        rhs: &TypedPlace,
        dst: u8,
        span: Span,
    ) -> Result<Place, LowerError> {
        let op = self.pick_int_float(int_op, float_op, lhs, span)?;
        self.emit_data_at(op, lhs, rhs, dst, span)
    }

    /// Both operands solve to the string atom: `+` is the concat
    /// call's spelling [B8]. The checked-type-only probe answers before
    /// operand evaluation, so the call's borrow window opens in time to
    /// pin the words the CALL reads.
    fn is_string_add_ty(&self, lhs: Ty, rhs: Ty) -> bool {
        self.shapes.builtin_instance(lhs) == Some(BuiltinInstance::Str)
            && self.shapes.builtin_instance(rhs) == Some(BuiltinInstance::Str)
    }

    /// String `+` lowers to the concat native's own call shape: the two
    /// Ref words stage as borrow windows, the CALL dispatches the
    /// native, and the fresh result lands on a body temp — the same
    /// staging and RC contract an explicit `a.concat(b)` lowers
    /// through, so the operator is a spelling, never a second
    /// operation.
    fn concat_value(
        &mut self,
        lhs: &TypedPlace,
        rhs: &TypedPlace,
        ty: Ty,
        span: Span,
    ) -> Result<Place, LowerError> {
        let l = single(&self.word_addrs(&lhs.place, lhs.ty)?, span)?;
        let r = single(&self.word_addrs(&rhs.place, rhs.ty)?, span)?;
        let native = crate::native::lookup("concat").unwrap_or_else(|| panic!("concat is tabled"));
        for (index, word) in [l.slot, r.slot].into_iter().enumerate() {
            self.emit(Op::Mov {
                dst: Slot::overlay(u8::try_from(index).expect("two words fit u8")),
                src: Slot::body(word),
            });
            self.rc.staged_native(word, false);
        }
        self.overlay_site(&[SlotType::Ref, SlotType::Ref], &[SlotType::Ref]);
        let kidx = self.pool(self.funcref_const(Callee::Builtin(native)));
        self.emit(Op::Call {
            callee: kidx,
            nargs: 2,
        });
        self.rc.native_done(&mut self.code);
        self.result_place(ty, &[SlotType::Ref], span)
    }

    /// Float remainder is a boundary, not a trap: the numeric table admits
    /// float rem [B7], but the locked uop set carries ModI64 only — both
    /// Rem sites (the binary operator and the compound assignment) check
    /// the left operand's word and reject F64 at compile time.
    fn reject_float_operand(
        &mut self,
        lhs: &TypedPlace,
        span: Span,
        subject: &'static str,
        reason: &'static str,
    ) -> Result<(), LowerError> {
        if let [only] = self.word_addrs(&lhs.place, lhs.ty)?.as_slice()
            && only.slot_ty == SlotType::F64
        {
            return Err(LowerError::unsupported(span, subject, reason));
        }
        Ok(())
    }

    fn reject_float_rem(&mut self, lhs: &TypedPlace, span: Span) -> Result<(), LowerError> {
        self.reject_float_operand(
            lhs,
            span,
            "a float remainder",
            "no ModF64 uop exists in the locked set; the numeric table admits float rem — library \
             rem arrives with @std",
        )
    }

    /// The defensive float refusal behind the checker's own [B7] rule: the
    /// wrap uops are i64-only, so a float operand that slips the check
    /// refuses here instead of mis-lowering as an i64 word.
    fn reject_float_wrap(&mut self, lhs: &TypedPlace, span: Span) -> Result<(), LowerError> {
        self.reject_float_operand(
            lhs,
            span,
            "a float wrapping operand",
            "wrapping operators are integer-only; floats are IEEE [B7]",
        )
    }

    /// The defensive float refusal behind the checker's own [B7] rule for
    /// the shift and bitwise operators: no f64 variant of any of them
    /// exists, so a float operand that slips the check refuses here
    /// instead of mis-lowering as an i64 word.
    fn reject_float_shift_bitwise(
        &mut self,
        lhs: &TypedPlace,
        span: Span,
    ) -> Result<(), LowerError> {
        self.reject_float_operand(
            lhs,
            span,
            "a float shift or bitwise operand",
            "shift and bitwise operators are integer-only [B7]",
        )
    }

    fn compare(
        &mut self,
        int_op: DataOp,
        float_op: DataOp,
        lhs: &TypedPlace,
        rhs: &TypedPlace,
        span: Span,
    ) -> Result<Place, LowerError> {
        let op = self.pick_int_float(int_op, float_op, lhs, span)?;
        self.emit_data(op, lhs, rhs, span)
    }

    fn pick_int_float(
        &mut self,
        int_op: DataOp,
        float_op: DataOp,
        lhs: &TypedPlace,
        span: Span,
    ) -> Result<DataOp, LowerError> {
        let addrs = self.word_addrs(&lhs.place, lhs.ty)?;
        let [only] = addrs.as_slice() else {
            return Err(LowerError::unsupported(
                span,
                "an arithmetic operand",
                "operands must be single-word scalars this round",
            ));
        };
        match only.slot_ty {
            SlotType::I64 => Ok(int_op),
            SlotType::F64 => Ok(float_op),
            _ => Err(LowerError::unsupported(
                span,
                "an arithmetic operand",
                "int and float operands only in the M3 uop contract",
            )),
        }
    }

    fn emit_data(
        &mut self,
        op: DataOp,
        lhs: &TypedPlace,
        rhs: &TypedPlace,
        span: Span,
    ) -> Result<Place, LowerError> {
        let l = single(&self.word_addrs(&lhs.place, lhs.ty)?, span)?;
        let dst = self.temp_word(op.result_slot(l.slot_ty))?;
        self.emit_data_at(op, lhs, rhs, dst, span)
    }

    /// The data op writing the pinned word `dst` — the direct scalar
    /// store. The VM reads both operands before writing dst, so `dst`
    /// may alias an operand word (the compound store reads the var's
    /// current value and overwrites it in one op).
    fn emit_data_at(
        &mut self,
        op: DataOp,
        lhs: &TypedPlace,
        rhs: &TypedPlace,
        dst: u8,
        span: Span,
    ) -> Result<Place, LowerError> {
        let l = single(&self.word_addrs(&lhs.place, lhs.ty)?, span)?;
        let r = single(&self.word_addrs(&rhs.place, rhs.ty)?, span)?;
        self.emit(Op::Data {
            op,
            dst: Slot::body(dst),
            lhs: Slot::body(l.slot),
            rhs: Slot::body(r.slot),
        });
        Ok(Place {
            base: dst,
            shape: PlaceShape::Word(op.result_slot(l.slot_ty)),
        })
    }

    /// `&&`/`||` as short-circuit jump chains; the uop set has no boolean
    /// arithmetic opcode.
    fn bool_chain(&mut self, is_and: bool, left: &Expr, right: &Expr) -> Result<Place, LowerError> {
        let short = self.label();
        let lie_lbl = self.label();
        let join = self.label();
        let lhs = self.value(left)?;
        let lcond = single(&self.word_addrs(&lhs.place, lhs.ty)?, left.span())?;
        self.emit(if is_and {
            Op::JmpifNot {
                cond: Slot::body(lcond.slot),
                target: short,
            }
        } else {
            Op::Jmpif {
                cond: Slot::body(lcond.slot),
                target: short,
            }
        });
        // path-fork: the short path skips the right operand entirely
        let base = self.rc.save();
        let rhs = self.value(right)?;
        let rcond = single(&self.word_addrs(&rhs.place, rhs.ty)?, right.span())?;
        self.emit(if is_and {
            Op::JmpifNot {
                cond: Slot::body(rcond.slot),
                target: lie_lbl,
            }
        } else {
            Op::Jmpif {
                cond: Slot::body(rcond.slot),
                target: lie_lbl,
            }
        });
        let truth = self.pool(Constant::Bool(is_and));
        let lie = self.pool(Constant::Bool(!is_and));
        let dst = self.temp_word(SlotType::Bool)?;
        self.emit(Op::Kconst {
            dst: Slot::body(dst),
            kidx: truth,
        });
        let arm_full = self.rc.take();
        let full_div = self.rc.diverged();
        let truth_jmp = self.code.len();
        self.emit(Op::Jmp { target: join });
        // The right operand's own shortcut answer: this block runs with
        // the post-right-operand ownership state, so its drops differ
        // from the left shortcut's — a block shared with the first
        // jump would dec the words the right operand's call already
        // handed to its callee (a double drop, the trap the `||`
        // right-arm probes pinned). Each of the full arm's two exits
        // carries its own copy of the full arm's dying drops, and each
        // runtime path decs once.
        self.bind_label(lie_lbl);
        self.emit(Op::Kconst {
            dst: Slot::body(dst),
            kidx: lie,
        });
        let lie_jmp = self.code.len();
        self.emit(Op::Jmp { target: join });
        self.rc.restore(base);
        self.bind_label(short);
        self.emit(Op::Kconst {
            dst: Slot::body(dst),
            kidx: lie,
        });
        let arm_short = self.rc.take();
        let short_div = self.rc.diverged();
        let dying = self
            .rc
            .reconcile(&[(arm_full, full_div), (arm_short, short_div)]);
        if let [full, short] = &dying[..] {
            let mut epilogue = Vec::new();
            Placement::emit_drops(&mut epilogue, full);
            // the later site splices first: the earlier site's splice
            // would shift it
            self.splice(epilogue.clone(), lie_jmp);
            self.splice(epilogue, truth_jmp);
            self.arm_epilogue(short);
        }
        self.bind_label(join);
        Ok(Place {
            base: dst,
            shape: PlaceShape::Word(SlotType::Bool),
        })
    }

    /// `x in a..b` / `x in a..=b` [F7]: the operand and both bounds
    /// evaluate once; two comparisons join by jumps (the uop set has no
    /// boolean arithmetic).
    fn in_range_value(
        &mut self,
        left: &Expr,
        start: &Expr,
        end: &Expr,
        inclusive: bool,
        span: Span,
    ) -> Result<Place, LowerError> {
        let x_tp = self.value(left)?;
        let x_w = single(&self.word_addrs(&x_tp.place, x_tp.ty)?, left.span())?;
        let start_tp = self.value(start)?;
        let start_w = single(
            &self.word_addrs(&start_tp.place, start_tp.ty)?,
            start.span(),
        )?;
        let end_tp = self.value(end)?;
        let end_w = single(&self.word_addrs(&end_tp.place, end_tp.ty)?, end.span())?;
        if x_w.slot_ty != SlotType::I64
            || start_w.slot_ty != SlotType::I64
            || end_w.slot_ty != SlotType::I64
        {
            return Err(LowerError::unsupported(
                span,
                "a range membership over non-`int` widths",
                "only the ambient `int` width lowers this round; conversions are explicit [B7]",
            ));
        }
        let miss = self.label();
        let join = self.label();
        let low = self.emit_data(DataOp::GeI64, &x_tp, &start_tp, span)?;
        let low_w = single(&self.word_addrs(&low, x_tp.ty)?, span)?;
        self.emit(Op::JmpifNot {
            cond: Slot::body(low_w.slot),
            target: miss,
        });
        let bound_op = if inclusive { DataOp::LeI64 } else { DataOp::LtI64 };
        let hi = self.emit_data(bound_op, &x_tp, &end_tp, span)?;
        let hi_w = single(&self.word_addrs(&hi, x_tp.ty)?, span)?;
        self.emit(Op::JmpifNot {
            cond: Slot::body(hi_w.slot),
            target: miss,
        });
        let truth = self.pool(Constant::Bool(true));
        let dst = self.temp_word(SlotType::Bool)?;
        self.emit(Op::Kconst {
            dst: Slot::body(dst),
            kidx: truth,
        });
        self.emit(Op::Jmp { target: join });
        self.bind_label(miss);
        let lie = self.pool(Constant::Bool(false));
        self.emit(Op::Kconst {
            dst: Slot::body(dst),
            kidx: lie,
        });
        self.bind_label(join);
        Ok(Place {
            base: dst,
            shape: PlaceShape::Word(SlotType::Bool),
        })
    }

    /// `x in xs` over one list [F7]: a scan over once-evaluated
    /// operands — one hidden induction word, `list_get` per iteration
    /// over its ok/none span (the none span answers false), one
    /// comparison per element (a wider element compares through the
    /// structural `==` chain, field words pairwise), the first match
    /// answers true. The gathered element's handed-out counts die right
    /// at the gather — the values stay readable through the list's own
    /// edge counts, so the comparison's reads never own and every exit
    /// path balances — while the operands' counts are loop-carried and
    /// die past the loop.
    fn in_list_value(
        &mut self,
        left: &Expr,
        right: &Expr,
        span: Span,
    ) -> Result<Place, LowerError> {
        let x_tp = self.value(left)?;
        let list_tp = self.value(right)?;
        let Some(element) = self.shapes.list_element(list_tp.ty) else {
            return Err(mismatch("a list membership's list"));
        };
        let elem_words = self.element_words(element, span)?;
        let wide = elem_words.len() > 1;
        if !wide
            && !matches!(
                elem_words[0],
                SlotType::I64 | SlotType::F64 | SlotType::Bool | SlotType::Ref
            )
        {
            return Err(LowerError::unsupported(
                span,
                "a membership over tag or callable elements",
                "membership compares int, float, bool, string, bytes, and collection elements",
            ));
        }
        // the one-word element compares by its type, never its slot kind
        // alone: the element's own word plan names the compare — a
        // collection element (bytes, list, map, or a one-word struct
        // whose field is one) dispatches its content-compare native, the
        // EqStr uop reading string objects alone
        let kind = (!wide)
            .then(|| {
                let plan = self.eq_word_plan(element)?;
                let [word] = plan.as_slice() else {
                    return Err(mismatch("a membership comparison"));
                };
                Ok(*word)
            })
            .transpose()
            .map_err(|err: LowerError| err.respanned(span))?;
        let mut operand_words: Vec<u8> = self
            .word_addrs(&x_tp.place, x_tp.ty)
            .map_err(|err| err.respanned(span))?
            .iter()
            .map(|addr| addr.slot)
            .collect();
        let x_w = if wide { None } else { Some(operand_words[0]) };
        let list_w = single(&self.word_addrs(&list_tp.place, list_tp.ty)?, right.span())?;
        operand_words.push(list_w.slot);
        let ind = self.temp_word(SlotType::I64)?;
        let zero = self.pool(Constant::I64(0));
        self.emit(Op::Kimm {
            dst: Slot::body(ind),
            kidx: zero,
        });
        let step = self.temp_word(SlotType::I64)?;
        let one = self.pool(Constant::I64(1));
        self.emit(Op::Kimm {
            dst: Slot::body(step),
            kidx: one,
        });
        let dst = self.temp_word(SlotType::Bool)?;
        let entry = self.rc.save();
        self.rc.loop_enter();
        self.loop_spans.push(LoopSpan {
            head: self.code.len(),
            back: usize::MAX,
        });
        let head = self.label();
        let next = self.label();
        let exhausted = self.label();
        let found = self.label();
        self.bind_label(head);
        self.fetch_element(list_w.slot, ind, &elem_words)?;
        let cond = self.temp_word(SlotType::Bool)?;
        self.emit(Op::TagTest {
            dst: Slot::body(cond),
            src: Slot::overlay(0),
            tag: PLATFORM_IO_OK,
        });
        self.emit(Op::JmpifNot {
            cond: Slot::body(cond),
            target: exhausted,
        });
        let elem_place = self.gather_element(&elem_words, element)?;
        let cmp_place = if wide {
            // the handed-out counts die here, straight-line on every
            // path: the values stay readable through the list's own edge
            // counts, so the equality chain's reads never own and a
            // fail-fast field mismatch cannot strand a later field's
            // count
            let dying = self.tracked_words(&elem_place, element)?;
            self.rc.discard(&mut self.code, &dying);
            self.eq_chain(
                &x_tp,
                &TypedPlace {
                    place: elem_place,
                    ty: element,
                },
                span,
            )
            .map_err(|err| err.respanned(span))?
        } else {
            let (Some(word), Some(kind)) = (x_w, kind) else {
                return Err(mismatch("a membership comparison"));
            };
            let cmp = match kind {
                // the one-word shape keeps its emission exactly (the
                // word's count dies at the comparison read)
                WordEq::Scalar(op) => {
                    let cmp = self.temp_word(SlotType::Bool)?;
                    self.emit(Op::Data {
                        op,
                        dst: Slot::body(cmp),
                        lhs: Slot::body(word),
                        rhs: Slot::body(elem_place.base),
                    });
                    if op == DataOp::EqStr {
                        self.rc.read(&mut self.code, &[word, elem_place.base]);
                    }
                    cmp
                },
                // a collection element compares through its content
                // compare, the operands staged as borrows
                WordEq::Bytes | WordEq::List | WordEq::Map => {
                    self.content_eq_slots(kind, word, elem_place.base, true)?
                },
            };
            Place {
                base: cmp,
                shape: PlaceShape::Word(SlotType::Bool),
            }
        };
        self.emit(Op::JmpifNot {
            cond: Slot::body(cmp_place.base),
            target: next,
        });
        let truth = self.pool(Constant::Bool(true));
        self.emit(Op::Kconst {
            dst: Slot::body(dst),
            kidx: truth,
        });
        self.emit(Op::Jmp { target: found });
        self.bind_label(next);
        self.emit(Op::Data {
            op: DataOp::AddI64,
            dst: Slot::body(ind),
            lhs: Slot::body(ind),
            rhs: Slot::body(step),
        });
        self.loop_back_edge();
        self.emit(Op::Jmp { target: head });
        self.rc.loop_exit();
        let back = self.rc.take();
        self.rc.join(&[entry, back]);
        self.bind_label(exhausted);
        let lie = self.pool(Constant::Bool(false));
        self.emit(Op::Kconst {
            dst: Slot::body(dst),
            kidx: lie,
        });
        self.bind_label(found);
        // an anonymous operand (a literal, a call result) has no scope of
        // its own: its consumed count dies here, once per evaluation
        self.rc.drop_unnamed(&mut self.code, &operand_words);
        Ok(Place {
            base: dst,
            shape: PlaceShape::Word(SlotType::Bool),
        })
    }

    /// Structural `==`: one uop per scalar, a fail-fast chain per struct,
    /// a tag dispatch plus payload compare per union [B9].
    fn eq_chain(
        &mut self,
        lhs: &TypedPlace,
        rhs: &TypedPlace,
        span: Span,
    ) -> Result<Place, LowerError> {
        // A multi-atom value never compares against one member's payload
        // value: the per-member tag dispatch needs a tag word on both
        // sides, and a narrowed value of one member carries none — the
        // union rides the dispatch side (a unit-tag value against it
        // compares from either side), and every other mixed spelling
        // refuses naming the narrowing seam.
        let l_union = matches!(&*self.shapes.shape(lhs.ty)?, Shape::Union(_));
        let r_union = matches!(&*self.shapes.shape(rhs.ty)?, Shape::Union(_));
        let tag_word = |place: &Place| matches!(place.shape, PlaceShape::Word(SlotType::Tag));
        if l_union != r_union && !tag_word(&lhs.place) && !tag_word(&rhs.place) {
            return Err(LowerError::unsupported(
                span,
                "an equality over a multi-atom value against one member's value",
                "the per-member tag dispatch needs a tag word on both sides and one member's \
                 payload value carries none; narrow the union with `is` or a match arm before \
                 comparing",
            ));
        }
        if r_union && !l_union {
            return self.eq_chain(rhs, lhs, span);
        }
        let lty = lhs.ty;
        let shape = self.shapes.shape(lty)?;
        let dst = match &*shape {
            Shape::Word(slot) => {
                // the heap collections share the string's one Ref word
                // shape but the EqStr uop reads string objects [B9]: each
                // collection's content-compare native serves the chain's
                // word
                if *slot == SlotType::Ref {
                    if let Some(word) = self.collection_ty_word(lty)? {
                        self.collection_eq_word(word, lhs, rhs, span)?
                    } else {
                        self.scalar_eq_word(lhs, rhs, span, *slot)?
                    }
                } else {
                    self.scalar_eq_word(lhs, rhs, span, *slot)?
                }
            },
            Shape::Struct(_) => {
                // the flattened word lists compare pairwise, a field's
                // span word-by-word in declaration order; the compares
                // borrow, so both exits release the operands' dying
                // counts at the join — a fail-fast field mismatch
                // cannot strand a later Ref word's staged count
                let fail = self.label();
                let join = self.label();
                let l = self.word_addrs(&lhs.place, lhs.ty)?;
                let r = self.word_addrs(&rhs.place, rhs.ty)?;
                let plan = self.eq_word_plan(lty)?;
                self.emit_word_compares(&l, &r, &plan, fail)?;
                let truth = self.pool(Constant::Bool(true));
                let dst = self.temp_word(SlotType::Bool)?;
                self.emit(Op::Kconst {
                    dst: Slot::body(dst),
                    kidx: truth,
                });
                self.emit(Op::Jmp { target: join });
                self.bind_label(fail);
                let lie = self.pool(Constant::Bool(false));
                self.emit(Op::Kconst {
                    dst: Slot::body(dst),
                    kidx: lie,
                });
                self.bind_label(join);
                self.release_eq_words(&l, &r);
                dst
            },
            Shape::Tuple(s) => {
                let fail = self.label();
                let join = self.label();
                let l = self.word_addrs(&lhs.place, lhs.ty)?;
                let r = self.word_addrs(&rhs.place, rhs.ty)?;
                if l.len() != s.fields.len() || r.len() != s.fields.len() {
                    return Err(mismatch("a tuple equality's arity"));
                }
                let plan = self.eq_word_plan(lty)?;
                self.emit_word_compares(&l, &r, &plan, fail)?;
                let truth = self.pool(Constant::Bool(true));
                let dst = self.temp_word(SlotType::Bool)?;
                self.emit(Op::Kconst {
                    dst: Slot::body(dst),
                    kidx: truth,
                });
                self.emit(Op::Jmp { target: join });
                self.bind_label(fail);
                let lie = self.pool(Constant::Bool(false));
                self.emit(Op::Kconst {
                    dst: Slot::body(dst),
                    kidx: lie,
                });
                self.bind_label(join);
                self.release_eq_words(&l, &r);
                dst
            },
            Shape::Union(_) => self.union_eq(lhs, rhs, lty)?,
        };
        Ok(Place {
            base: dst,
            shape: PlaceShape::Word(SlotType::Bool),
        })
    }

    /// The scalar `==`/`!=` word: the one uop the word's slot kind
    /// selects (the string Ref's EqStr reads string objects), its reads
    /// owning no word's death.
    fn scalar_eq_word(
        &mut self,
        lhs: &TypedPlace,
        rhs: &TypedPlace,
        span: Span,
        slot: SlotType,
    ) -> Result<u8, LowerError> {
        let op = eq_op(slot)?;
        let l = single(&self.word_addrs(&lhs.place, lhs.ty)?, span)?;
        let r = single(&self.word_addrs(&rhs.place, rhs.ty)?, span)?;
        let dst = self.temp_word(SlotType::Bool)?;
        self.emit(Op::Data {
            op,
            dst: Slot::body(dst),
            lhs: Slot::body(l.slot),
            rhs: Slot::body(r.slot),
        });
        if op == DataOp::EqStr {
            self.rc.read(&mut self.code, &[l.slot, r.slot]);
        }
        Ok(dst)
    }

    /// The collection `==`/`!=` seam [B9]: structural content equality
    /// through the builtin core's content-compare native the word's
    /// collection names — one CALL in the chain's word, the operands
    /// staged as borrows exactly the concat call shape's (a staged word's
    /// count dies at the window's flush, never owning).
    fn collection_eq_word(
        &mut self,
        word: WordEq,
        lhs: &TypedPlace,
        rhs: &TypedPlace,
        span: Span,
    ) -> Result<u8, LowerError> {
        let l = single(&self.word_addrs(&lhs.place, lhs.ty)?, span)?;
        let r = single(&self.word_addrs(&rhs.place, rhs.ty)?, span)?;
        self.content_eq_slots(word, l.slot, r.slot, true)
    }

    /// The content compare over two operand words: one CALL, the
    /// operands staged as borrows exactly the concat call shape's, the
    /// bool answer landed on a fresh word — the direct-operand seam and
    /// the per-field chain word share it. `own` marks whether the window's
    /// flush owns a dying operand's dec (a chain word stages keep-mode:
    /// its fail-fast exit skips the flush site, so the chain's merged
    /// exits release instead).
    fn content_eq_slots(
        &mut self,
        kind: WordEq,
        l: u8,
        r: u8,
        own: bool,
    ) -> Result<u8, LowerError> {
        let name = match kind {
            WordEq::Bytes => "bytes_eq",
            WordEq::List => "list_eq",
            WordEq::Map => "map_eq",
            WordEq::Scalar(_) => return Err(mismatch("a content compare")),
        };
        let native = crate::native::lookup(name).ok_or_else(|| mismatch("the content compare"))?;
        self.rc.native_open();
        for (index, word) in [l, r].into_iter().enumerate() {
            self.emit(Op::Mov {
                dst: Slot::overlay(u8::try_from(index).expect("two staged words fit u8")),
                src: Slot::body(word),
            });
            if own {
                self.rc.staged_native(word, false);
            } else {
                self.rc.staged_native_keep(word);
            }
        }
        self.overlay_site(&[SlotType::Ref, SlotType::Ref], &[SlotType::Bool]);
        let kidx = self.pool(self.funcref_const(Callee::Builtin(native)));
        self.emit(Op::Call {
            callee: kidx,
            nargs: 2,
        });
        self.rc.native_done(&mut self.code);
        let dst = self.temp_word(SlotType::Bool)?;
        self.emit(Op::Mov {
            dst: Slot::body(dst),
            src: Slot::overlay(0),
        });
        Ok(dst)
    }

    /// The per-word compare plan of one type's flattened value span
    /// [B9]: the chain compares fields by their declared type, never by
    /// their slot kind alone, so the plan pairs with `word_addrs` word
    /// for word. A collection-typed Ref word plans that collection's
    /// content compare (`builtin_instance` names the collection the way
    /// the call sites do); every other word plans the scalar uop its kind
    /// selects — a kind with no uop keeps the refusal the slot-kind
    /// chain gave.
    fn eq_word_plan(&mut self, ty: Ty) -> Result<Vec<WordEq>, LowerError> {
        let shape = self.shapes.shape(ty)?;
        match &*shape {
            Shape::Word(slot) => {
                if *slot == SlotType::Ref
                    && let Some(word) = self.collection_ty_word(ty)?
                {
                    return Ok(vec![word]);
                }
                Ok(vec![WordEq::Scalar(eq_op(*slot)?)])
            },
            // a bare struct or tuple flattens its declared fields' plans
            // in span order; a union value never reaches a word chain
            // (its equality rides the tag dispatch), and a union-typed
            // field's span words each plan their own kind — the tag word
            // refusing exactly as the slot-kind chain refused
            Shape::Struct(_) | Shape::Tuple(_) => {
                let [atom] = self.members(ty) else {
                    return Err(mismatch("a span equality plan"));
                };
                self.atom_word_plan(*atom)
            },
            Shape::Union(u) => u
                .words
                .iter()
                .map(|slot| Ok(WordEq::Scalar(eq_op(*slot)?)))
                .collect(),
        }
    }

    /// The per-word compare plan of one atom's own value span: a
    /// collection atom (the bytes word, a list/map heap application)
    /// plans that collection's content compare; a struct or tuple atom
    /// flattens its declared fields' plans; every other atom is its one
    /// word's kind (the uop, or its refusal).
    fn atom_word_plan(&mut self, atom: TypeId) -> Result<Vec<WordEq>, LowerError> {
        match &self
            .analyzed
            .decls
            .universe
            .atom_decl(atom)
            .map_err(|_| mismatch("an equality plan"))?
            .kind
        {
            AtomKind::Struct(_) | AtomKind::Tuple(_) => self.payload_word_plan(atom),
            _ => {
                if let Some(word) = self.collection_atom_word(atom)? {
                    return Ok(vec![word]);
                }
                let shape = self.shapes.atom_shape(atom)?;
                let Shape::Word(slot) = shape else {
                    return Err(mismatch("an equality plan"));
                };
                Ok(vec![WordEq::Scalar(eq_op(slot)?)])
            },
        }
    }

    /// The content-compare word one solved collection type plans [B9]:
    /// the native its builtin instance names — the by-type entry the
    /// direct operand pair, a membership's element, and the flattened
    /// plan's word arm share.
    ///
    /// `None` for every non-collection type (the string's EqStr serves
    /// its own Ref word).
    fn collection_ty_word(&mut self, ty: Ty) -> Result<Option<WordEq>, LowerError> {
        match self.shapes.builtin_instance(ty) {
            Some(BuiltinInstance::Bytes) => Ok(Some(WordEq::Bytes)),
            Some(BuiltinInstance::List) => {
                let element = self
                    .shapes
                    .list_element(ty)
                    .ok_or_else(|| mismatch("a list equality plan"))?;
                self.eq_word_plan(element).map_err(|_| list_refusal())?;
                Ok(Some(WordEq::List))
            },
            Some(BuiltinInstance::Map) => {
                let value = self
                    .shapes
                    .map_value(ty)
                    .ok_or_else(|| mismatch("a map equality plan"))?;
                self.eq_word_plan(value).map_err(|_| map_refusal())?;
                Ok(Some(WordEq::Map))
            },
            _ => Ok(None),
        }
    }

    /// The atom-keyed twin of [`Self::collection_ty_word`]: the content
    /// compare one declared collection atom plans — the entry the field
    /// atoms of the flattened plan ride.
    fn collection_atom_word(&mut self, atom: TypeId) -> Result<Option<WordEq>, LowerError> {
        let Some(instance) = self.shapes.atom_builtin_instance(atom) else {
            return Ok(None);
        };
        match instance {
            BuiltinInstance::Bytes => Ok(Some(WordEq::Bytes)),
            BuiltinInstance::List => {
                let [element] = self.shapes.apply_args(atom).unwrap_or(&[]) else {
                    return Err(mismatch("a list equality plan"));
                };
                self.eq_word_plan(*element).map_err(|_| list_refusal())?;
                Ok(Some(WordEq::List))
            },
            BuiltinInstance::Map => {
                let [_, value] = self.shapes.apply_args(atom).unwrap_or(&[]) else {
                    return Err(mismatch("a map equality plan"));
                };
                self.eq_word_plan(*value).map_err(|_| map_refusal())?;
                Ok(Some(WordEq::Map))
            },
            _ => Ok(None),
        }
    }

    /// The per-word compare plan of one atom's declared payload fields
    /// (a bare struct's fields, or a union member's), in declaration
    /// order — the typed half of the shape walk `payload_fields` laid
    /// out, so the plan pairs with the laid-out field spans.
    fn payload_word_plan(&mut self, atom: TypeId) -> Result<Vec<WordEq>, LowerError> {
        let decl = self
            .analyzed
            .decls
            .universe
            .atom_decl(atom)
            .map_err(|_| mismatch("an equality plan"))?;
        match &decl.kind {
            AtomKind::Struct(fields) => {
                // a part-less struct's payload is its one reserved I64
                // word, the frame's inert zero — no declared field backs
                // it, so the plan mirrors the reserved word itself
                if fields.is_empty() {
                    return Ok(vec![WordEq::Scalar(eq_op(SlotType::I64)?)]);
                }
                let mut plan = Vec::new();
                for field in fields {
                    plan.extend(self.type_word_plan(&field.ty)?);
                }
                Ok(plan)
            },
            AtomKind::Tuple(elements) => {
                let mut plan = Vec::new();
                for element in elements {
                    plan.extend(self.atom_word_plan(*element)?);
                }
                Ok(plan)
            },
            // a bare tag carries no payload; a callable, a primitive,
            // or a heap application is its own one word
            _ => {
                if let Some(word) = self.collection_atom_word(atom)? {
                    return Ok(vec![word]);
                }
                let shape = self.shapes.atom_shape(atom)?;
                let Shape::Word(slot) = shape else {
                    return Err(mismatch("an equality plan"));
                };
                Ok(vec![WordEq::Scalar(eq_op(slot)?)])
            },
        }
    }

    /// The per-word compare plan of one written field type: a bytes
    /// atom plans the content compare; a nested struct or tuple atom
    /// flattens its own fields' plans; a union flattens its whole span's
    /// word kinds; a tuple type flattens its elements' plans (the
    /// layout's one-word-per-element rule already refused a wider
    /// element).
    fn type_word_plan(&mut self, expr: &TypeExpr) -> Result<Vec<WordEq>, LowerError> {
        match expr {
            TypeExpr::Atom(id) => self.atom_word_plan(*id),
            TypeExpr::Union(..) => {
                let members = expr_members(expr).ok_or_else(|| mismatch("an equality plan"))?;
                let shape = self
                    .shapes
                    .members_shape(&members, 0)
                    .map_err(|_| mismatch("an equality plan"))?;
                shape_words(&shape)
                    .iter()
                    .map(|slot| Ok(WordEq::Scalar(eq_op(*slot)?)))
                    .collect()
            },
            TypeExpr::Tuple(elements) => {
                let mut plan = Vec::new();
                for element in elements {
                    plan.extend(self.type_word_plan(element)?);
                }
                Ok(plan)
            },
            // a trait, negation, intersection, or difference never laid
            // out as an inline field span
            _ => Err(mismatch("an equality plan")),
        }
    }

    /// Emits the pairwise word compares of two flattened address lists,
    /// every mismatched word jumping to `fail`; the last word's result
    /// flows on. A struct's whole span compares word-by-word this way —
    /// inline spans are flat words, so per-word equality is the value's,
    /// each word compared by its field's planned kind: a bytes word
    /// through the content-compare native, every other through its
    /// scalar uop. The compares borrow their operands — no read owns its
    /// word's death — because the chain's fail-fast exit skips the later
    /// compares' own release sites; the caller releases every dying
    /// operand word where the chain's exits merge.
    fn emit_word_compares(
        &mut self,
        l: &[WordAddr],
        r: &[WordAddr],
        plan: &[WordEq],
        fail: usize,
    ) -> Result<(), LowerError> {
        if l.len() != r.len() || l.len() != plan.len() {
            return Err(mismatch("a span equality's width"));
        }
        for (pair, kind) in l.iter().zip(r.iter()).zip(plan.iter().copied()) {
            let cond = match kind {
                WordEq::Scalar(op) => {
                    if op != eq_op(pair.0.slot_ty)? {
                        return Err(mismatch("a span equality plan"));
                    }
                    let cond = self.temp_word(SlotType::Bool)?;
                    self.emit(Op::Data {
                        op,
                        dst: Slot::body(cond),
                        lhs: Slot::body(pair.0.slot),
                        rhs: Slot::body(pair.1.slot),
                    });
                    cond
                },
                // a collection word dispatches its content compare,
                // staged keep-mode (the chain's merged exits release)
                WordEq::Bytes | WordEq::List | WordEq::Map => {
                    self.content_eq_slots(kind, pair.0.slot, pair.1.slot, false)?
                },
            };
            self.emit(Op::JmpifNot {
                cond: Slot::body(cond),
                target: fail,
            });
        }
        Ok(())
    }

    /// Releases the equality chain's operand counts where its exits merge:
    /// every dying operand word drops here, on the equal path and the
    /// fail-fast path alike (`drop_word` itself defers into an enclosing
    /// window that still reads the word, so a chain nested in a call's
    /// operand staging stays sound).
    fn release_eq_words(&mut self, l: &[WordAddr], r: &[WordAddr]) {
        let words: Vec<u8> = l.iter().chain(r.iter()).map(|addr| addr.slot).collect();
        self.rc.read(&mut self.code, &words);
    }

    /// Union equality: per-member tag tests, then the member's payload
    /// compare; distinct members never compare equal [B9].
    fn union_eq(&mut self, lhs: &TypedPlace, rhs: &TypedPlace, ty: Ty) -> Result<u8, LowerError> {
        let shape = self.shapes.shape(ty)?;
        let Shape::Union(u) = &*shape else {
            return Err(mismatch("a union equality"));
        };
        let members = &u.members;
        let ltag = self.tag_addr(&lhs.place)?;
        let rtag = self.tag_addr(&rhs.place)?;
        let false_lbl = self.label();
        let done = self.label();
        let dst = self.temp_word(SlotType::Bool)?;
        for member in members {
            let next = self.label();
            self.emit(Op::TagTest {
                dst: Slot::body(dst),
                src: Slot::body(ltag),
                tag: member.tag,
            });
            self.emit(Op::JmpifNot {
                cond: Slot::body(dst),
                target: next,
            });
            self.emit(Op::TagTest {
                dst: Slot::body(dst),
                src: Slot::body(rtag),
                tag: member.tag,
            });
            self.emit(Op::JmpifNot {
                cond: Slot::body(dst),
                target: false_lbl,
            });
            if member.fields.is_empty() {
                let truth = self.pool(Constant::Bool(true));
                self.emit(Op::Kconst {
                    dst: Slot::body(dst),
                    kidx: truth,
                });
                self.emit(Op::Jmp { target: done });
            } else {
                // the whole payload span compares field by field, the
                // same per-word chain a bare struct's equality rides —
                // the compares borrow, and both of the member's exits
                // release the payload counts from the fork's own state,
                // so a fail-fast field mismatch cannot strand a later
                // field's count
                let l = member
                    .fields
                    .iter()
                    .flat_map(|field| field_addrs(lhs.place.base, field))
                    .collect::<Vec<_>>();
                let r = member
                    .fields
                    .iter()
                    .flat_map(|field| field_addrs(rhs.place.base, field))
                    .collect::<Vec<_>>();
                let plan = self.payload_word_plan(member.atom)?;
                let member_fail = self.label();
                let fork = self.rc.save();
                self.emit_word_compares(&l, &r, &plan, member_fail)?;
                self.release_eq_words(&l, &r);
                self.emit(Op::Jmp { target: done });
                self.bind_label(member_fail);
                self.rc.rewind(fork);
                self.release_eq_words(&l, &r);
                self.emit(Op::Jmp { target: false_lbl });
            }
            self.bind_label(next);
        }
        self.bind_label(false_lbl);
        let lie = self.pool(Constant::Bool(false));
        self.emit(Op::Kconst {
            dst: Slot::body(dst),
            kidx: lie,
        });
        self.bind_label(done);
        Ok(dst)
    }

    fn unary(&mut self, op: UnaryOp, expr: &Expr, span: Span, ty: Ty) -> Result<Place, LowerError> {
        // `-9223372036854775808` writes i64::MIN: the checker folds the
        // sign into the literal's range check, and the exact min
        // magnitude lowers as the min constant (every other negative
        // keeps the `0 - a` lowering below).
        if op == UnaryOp::Neg
            && let Expr::Literal {
                lit: Literal::Int { suffix, text },
                ..
            } = expr
            && suffix.is_empty()
            && negated_int_min(text)
        {
            let dst = self.temp_word(SlotType::I64)?;
            let kidx = self.pool(Constant::I64(i64::MIN));
            self.emit(Op::Kimm {
                dst: Slot::body(dst),
                kidx,
            });
            return Ok(Place {
                base: dst,
                shape: PlaceShape::Word(SlotType::I64),
            });
        }
        let operand = self.value(expr)?;
        let addrs = self.word_addrs(&operand.place, operand.ty)?;
        let [only] = addrs.as_slice() else {
            return Err(mismatch("a unary operand"));
        };
        let place = match op {
            UnaryOp::Neg if only.slot_ty == SlotType::I64 => {
                let zero = self.temp_word(SlotType::I64)?;
                let kidx = self.pool(Constant::I64(0));
                self.emit(Op::Kimm {
                    dst: Slot::body(zero),
                    kidx,
                });
                let dst = self.temp_word(SlotType::I64)?;
                self.emit(Op::Data {
                    op: DataOp::SubI64,
                    dst: Slot::body(dst),
                    lhs: Slot::body(zero),
                    rhs: Slot::body(only.slot),
                });
                Place {
                    base: dst,
                    shape: PlaceShape::Word(SlotType::I64),
                }
            },
            UnaryOp::Not if only.slot_ty == SlotType::Bool => {
                let lie = self.temp_word(SlotType::Bool)?;
                let kidx = self.pool(Constant::Bool(false));
                self.emit(Op::Kconst {
                    dst: Slot::body(lie),
                    kidx,
                });
                let dst = self.temp_word(SlotType::Bool)?;
                self.emit(Op::Data {
                    op: DataOp::EqBool,
                    dst: Slot::body(dst),
                    lhs: Slot::body(only.slot),
                    rhs: Slot::body(lie),
                });
                Place {
                    base: dst,
                    shape: PlaceShape::Word(SlotType::Bool),
                }
            },
            UnaryOp::Neg | UnaryOp::Not => {
                return Err(LowerError::unsupported(
                    span,
                    "a unary operation",
                    "only int negation and boolean not are in the M3 contract",
                ))
            },
            UnaryOp::WrapNeg if only.slot_ty == SlotType::I64 => {
                // `-%a` lowers as `0 -% a`: the KIMM zero mirrors int
                // negation and the wrapping sub never traps, so negating
                // int-min wraps to int-min (B7's wrap set)
                let zero = self.temp_word(SlotType::I64)?;
                let kidx = self.pool(Constant::I64(0));
                self.emit(Op::Kimm {
                    dst: Slot::body(zero),
                    kidx,
                });
                let dst = self.temp_word(SlotType::I64)?;
                self.emit(Op::Data {
                    op: DataOp::WrapSubI64,
                    dst: Slot::body(dst),
                    lhs: Slot::body(zero),
                    rhs: Slot::body(only.slot),
                });
                Place {
                    base: dst,
                    shape: PlaceShape::Word(SlotType::I64),
                }
            },
            UnaryOp::WrapNeg => {
                return Err(LowerError::unsupported(
                    span,
                    "a wrapping negation",
                    "wrapping operators are integer-only; floats are IEEE [B7]",
                ))
            },
        };
        let _ = ty;
        Ok(place)
    }

    // ---- finalization ---------------------------------------------------------

    /// Renders the symbolic buffer: the placed sequence (uops plus the RC
    /// ops at their walk positions) and the module body carry the same
    /// words — the RC ops ride the 0xE major beside the uops, and the VM
    /// dispatches both. FUNCF occupies word 0 and every op occupies one
    /// word, so a label's stream position is its op index plus one; jump
    /// targets cannot shift.
    #[allow(clippy::too_many_lines)]
    fn finalize(mut self) -> Result<(ds_mod::Function, Vec<PlacedOp>, crate::FnFacts), LowerError> {
        // The canonical unit word's one `Kimm 0` enters at the frame top:
        // a use inside a branch arm must not own the only write (a path
        // through another arm would read the previous frame's garbage),
        // and a loop head bound at stream position 0 must not pick the
        // store up per back edge — so every label and loop span shifts
        // past it, keeping each jump target on its own op.
        if let Some(word) = self.unit_slot {
            let kidx = self.pool(Constant::I64(0));
            self.code.insert(0, Op::Kimm {
                dst: Slot::body(word),
                kidx,
            });
            for pos in &mut self.labels {
                if *pos != usize::MAX {
                    *pos += 1;
                }
            }
            for span in &mut self.loop_spans {
                span.head += 1;
                if span.back != usize::MAX {
                    span.back += 1;
                }
            }
            for end in &mut self.value_ends {
                *end += 1;
            }
        }
        // Stage three's temporary slices: every slot the placement pass
        // still classifies count-free takes its count-free spelling — a
        // KSTR load becomes a borrow load (KSTR -> KSTR_BORROW), a
        // STRCAT result becomes an arena serving (STRCAT ->
        // STRCAT_ARENA) — and its terminal decs leave the stream. Slots
        // bump-allocate, so each dec belongs to a word the pass hosted
        // there, and the classification survived the whole walk, so no
        // path moves a deleted count. A loop body carrying a designated
        // arena word gains a mark/restore pair at its head and back
        // edge, one fresh body word per marked loop (appended after the
        // walk, so only qualifying frames grow). Deleting and inserting
        // words shift the stream, so bound labels rebase against both
        // movements: a label bound at a removed dec lands on the op
        // that followed it, and a loop's head label stays on the
        // inserted mark (a jump lands on the re-mark).
        let borrow = self.rc.take_kstr_slots();
        let mut arena = self.rc.take_strcat_slots();
        let mut insertions: Vec<(usize, Op)> = Vec::new();
        if !arena.is_empty() {
            // a marked loop's body holds a designated concatenation
            // somewhere between its head and back edge
            let designated = |index: usize, arena: &HashSet<u8>| match self.code[index] {
                Op::Data {
                    op: DataOp::StrCat,
                    dst:
                        Slot {
                            index: word,
                            overlay: false,
                        },
                    ..
                } => arena.contains(&word),
                _ => false,
            };
            let marked: Vec<usize> = self
                .loop_spans
                .iter()
                .enumerate()
                .filter(|(_, span)| (span.head..span.back).any(|index| designated(index, &arena)))
                .map(|(span_index, _)| span_index)
                .collect();
            // the mark words must fit beside the body and overlay words;
            // a frame that cannot host them keeps the counted spelling
            if u8::try_from(self.slots.len() + self.overlay_len + marked.len()).is_ok() {
                for &span_index in &marked {
                    let word = u8::try_from(self.slots.len()).expect("mark word fits u8");
                    self.slots.push(SlotType::I64);
                    let span = self.loop_spans[span_index];
                    insertions.push((span.head, Op::ArenaMark {
                        dst: Slot::body(word),
                    }));
                    insertions.push((span.back, Op::ArenaRestore {
                        src: Slot::body(word),
                    }));
                }
            } else {
                arena.clear();
            }
        }
        if !borrow.is_empty() || !arena.is_empty() {
            let mut dead = Vec::new();
            for index in 0..self.code.len() {
                match self.code[index] {
                    Op::Kstr {
                        dst:
                            Slot {
                                index: word,
                                overlay: false,
                            },
                        kidx,
                    } if borrow.contains(&word) => {
                        self.code[index] = Op::KstrBorrow {
                            dst: Slot::body(word),
                            kidx,
                        };
                    },
                    Op::Data {
                        op: DataOp::StrCat,
                        dst:
                            Slot {
                                index: word,
                                overlay: false,
                            },
                        lhs,
                        rhs,
                    } if arena.contains(&word) => {
                        self.code[index] = Op::Data {
                            op: DataOp::StrCatArena,
                            dst: Slot::body(word),
                            lhs,
                            rhs,
                        };
                    },
                    Op::RcDec {
                        src:
                            Slot {
                                index: word,
                                overlay: false,
                            },
                    } if borrow.contains(&word) || arena.contains(&word) => {
                        dead.push(index);
                    },
                    _ => {},
                }
            }
            // `dead` rises with the forward walk and `insertions` sorts
            // into code order, so one forward pass splices the survivors
            // and the mark ops and a second answers every label's shift
            insertions.sort_by_key(|&(at, _)| at);
            let mut rebuilt = Vec::with_capacity(self.code.len() + insertions.len() - dead.len());
            let mut at = 0;
            let mut dead_at = 0;
            for (index, op) in self.code.iter().enumerate() {
                while at < insertions.len() && insertions[at].0 == index {
                    rebuilt.push(insertions[at].1);
                    at += 1;
                }
                if dead_at < dead.len() && dead[dead_at] == index {
                    dead_at += 1;
                    continue;
                }
                rebuilt.push(*op);
            }
            self.code = rebuilt;
            let mut moved: Vec<usize> = (0..self.labels.len())
                .filter(|&label| self.labels[label] != usize::MAX)
                .collect();
            moved.sort_unstable_by_key(|&label| self.labels[label]);
            let (mut removed, mut added) = (0usize, 0usize);
            for label in moved {
                // both movements count against the original position
                // (a deletion before the label removes one op, an
                // insertion before it adds one; the insertions cannot
                // underflow because each removal sits strictly below)
                let base = self.labels[label];
                while removed < dead.len() && dead[removed] < base {
                    removed += 1;
                }
                while added < insertions.len() && insertions[added].0 < base {
                    added += 1;
                }
                self.labels[label] = base - removed + added;
            }
            // the value ends ride the same movements (the seam's resume
            // targets stay on the ops they named)
            for end in &mut self.value_ends {
                let base = usize::try_from(*end).expect("code length fits usize");
                let (mut removed, mut added) = (0usize, 0usize);
                while removed < dead.len() && dead[removed] < base {
                    removed += 1;
                }
                while added < insertions.len() && insertions[added].0 < base {
                    added += 1;
                }
                *end = u32::try_from(base - removed + added).expect("code length fits u32");
            }
        }
        let body_len = self.slots.len();
        // one word type per overlay slot, or Raw when shapes mix there
        let mut overlay_words = Vec::with_capacity(self.overlay_len);
        for index in 0..self.overlay_len {
            let slot = self
                .overlay
                .get(index)
                .filter(|set| set.len() == 1)
                .and_then(|set| set.iter().next().copied())
                .unwrap_or(SlotType::Raw);
            overlay_words.push(slot);
        }
        if body_len + overlay_words.len() > usize::from(u8::MAX) {
            return Err(LowerError::FrameTooWide {
                name: self.name.clone(),
                need: body_len + overlay_words.len(),
            });
        }
        let total = body_len + overlay_words.len();
        let mut slots = self.slots.clone();
        slots.extend(overlay_words);
        let layout =
            ds_abi::FrameLayout::new(slots, self.params_len, self.overlay_len).map_err(|_| {
                LowerError::FrameTooWide {
                    name: self.name.clone(),
                    need: total,
                }
            })?;
        let len = self.code.len();
        let mut placed = Vec::with_capacity(len + 1);
        let mut code = Vec::with_capacity(len + 1);
        let funcf = Uop::Funcf {
            frame_slots: u16::try_from(total).expect("frame slots fit u16"),
        };
        placed.push(PlacedOp::Uop(funcf));
        code.push(ds_abi::Inst::Uop(funcf));
        for op in &self.code {
            let body = |slot: Slot| resolve(slot, body_len);
            // FUNCF occupies stream word 0, so op i sits at word i + 1
            let target =
                |label: usize| u32::try_from(self.labels[label] + 1).expect("jump target fits u32");
            let rendered = match *op {
                Op::Kimm { dst, kidx } => PlacedOp::Uop(Uop::Kimm {
                    dst: body(dst),
                    kidx: ConstIdx(kidx),
                }),
                Op::Kconst { dst, kidx } => PlacedOp::Uop(Uop::Kconst {
                    dst: body(dst),
                    kidx: ConstIdx(kidx),
                }),
                Op::Kstr { dst, kidx } => PlacedOp::Uop(Uop::Kstr {
                    dst: body(dst),
                    kidx: ConstIdx(kidx),
                }),
                Op::KstrBorrow { dst, kidx } => PlacedOp::Uop(Uop::KstrBorrow {
                    dst: body(dst),
                    kidx: ConstIdx(kidx),
                }),
                Op::Data { op, dst, lhs, rhs } => {
                    PlacedOp::Uop(op.uop(body(dst), body(lhs), body(rhs)))
                },
                Op::TagTest { dst, src, tag } => PlacedOp::Uop(Uop::TagTest {
                    dst: body(dst),
                    src: body(src),
                    tag,
                }),
                Op::TagNew { dst, tag } => PlacedOp::Uop(Uop::TagNew {
                    dst: body(dst),
                    tag,
                }),
                Op::Mov { dst, src } => PlacedOp::Uop(Uop::Mov {
                    dst: body(dst),
                    src: body(src),
                }),
                Op::RcInc { src } => PlacedOp::Rc(RcOp::Inc { src: body(src) }),
                Op::RcDec { src } => PlacedOp::Rc(RcOp::Dec { src: body(src) }),
                Op::Jmp { target: at } => PlacedOp::Uop(Uop::Jmp { target: target(at) }),
                Op::ArenaMark { dst } => PlacedOp::Uop(Uop::ArenaMark { dst: body(dst) }),
                Op::ArenaRestore { src } => PlacedOp::Uop(Uop::ArenaRestore { src: body(src) }),
                Op::Jmpif { cond, target: at } => PlacedOp::Uop(Uop::Jmpif {
                    cond: body(cond),
                    target: target(at),
                }),
                Op::JmpifNot { cond, target: at } => PlacedOp::Uop(Uop::JmpifNot {
                    cond: body(cond),
                    target: target(at),
                }),
                Op::Call { callee, nargs } => PlacedOp::Uop(Uop::Call {
                    callee: ConstIdx(callee),
                    nargs,
                }),
                Op::Callt { callee, nargs } => PlacedOp::Uop(Uop::Callt {
                    callee: ConstIdx(callee),
                    nargs,
                }),
                Op::Callg { callee, nargs } => PlacedOp::Uop(Uop::Callg {
                    callee: ConstIdx(callee),
                    nargs,
                }),
                Op::Calltg { callee, nargs } => PlacedOp::Uop(Uop::Calltg {
                    callee: ConstIdx(callee),
                    nargs,
                }),
                Op::Calls { callee_slot, nargs } => {
                    PlacedOp::Uop(Uop::Calls { callee_slot, nargs })
                },
                Op::ClosureNew { dst, func, n } => PlacedOp::Uop(Uop::ClosureNew { dst, func, n }),
                Op::Ret { src } => PlacedOp::Uop(Uop::Ret { src: body(src) }),
                Op::RetSpan { src, n } => PlacedOp::Uop(Uop::RetSpan { src: body(src), n }),
                Op::Halt { slot } => PlacedOp::Uop(Uop::Halt { slot: body(slot) }),
            };
            code.push(match rendered {
                PlacedOp::Uop(uop) => ds_abi::Inst::Uop(uop),
                PlacedOp::Rc(rc) => ds_abi::Inst::Rc(rc),
            });
            placed.push(rendered);
        }
        let function = ds_mod::Function::new(self.name.clone(), layout, self.pool, code)
            .map_err(|error| LowerError::Build(error.to_string()))?;
        // FUNCF occupies stream word 0, so every recorded value end sits
        // one word above its pre-finalize index; the leading zero is the
        // body's start — the target that runs everything, standing for
        // the value end of the zeroth statement
        let facts = crate::FnFacts {
            value_ends: std::iter::once(0)
                .chain(self.value_ends.iter().map(|end| end + 1))
                .collect(),
            registered: self.rc.take_registered(),
        };
        Ok((function, placed, facts))
    }
}

// zero loads for the dead payload words of a constructed member: a
// union span's every word carries a value of its declared kind, so
// whole-span moves and typed callee parameters never see a foreign kind
// (the live member's words are the only ones ever read [BL-2])
impl FnLower<'_, '_, '_> {
    /// The native-by-name key a native surface's pool FuncRef addresses:
    /// the builtin core's reserved name, or the platform declaration's
    /// name for the world surface.
    fn native_key(&self, module: crate::native::NativeModule) -> FuncRefKey {
        match module {
            crate::native::NativeModule::Builtin => {
                FuncRefKey::Native(crate::native::BUILTIN_MODULE_NAME.to_owned())
            },
            crate::native::NativeModule::Platform => FuncRefKey::Native(self.platform.clone()),
            crate::native::NativeModule::Comptime => {
                FuncRefKey::Native(crate::native::COMPTIME_MODULE_NAME.to_owned())
            },
        }
    }

    /// The pool FuncRef of a callee: a builtin or platform native's
    /// reserved index under its native-by-name key, the enclosing
    /// module's dep-position key for a local function, the
    /// runtime-evidence sentinel for a staged dispatch site.
    fn funcref_const(&self, target: Callee) -> Constant {
        match target {
            Callee::Builtin(native) => Constant::FuncRef {
                key: self.native_key(native.module),
                func: native.index,
            },
            Callee::Bound { def, index } => {
                // the pool constant carries the return's marshaling word
                // (H5): the parameters marshal per the staged words' own
                // kinds, so the return alone needs a payload — the scalar
                // word read off the signature the resolution already
                // validated, an absent return the unit word
                let sig = self
                    .analyzed
                    .decls
                    .fns
                    .get(&def)
                    .expect("a bound callee resolved against its signature");
                Constant::Bound {
                    module: sig
                        .bind
                        .clone()
                        .expect("the resolution validated the bind target"),
                    index,
                    ret: sig
                        .ret
                        .as_ref()
                        .and_then(calls::bound_ret_word)
                        .unwrap_or(BoundRet::Unit),
                }
            },
            Callee::Generic(generic) => Constant::FuncRef {
                key: self.native_key(generic.module),
                func: generic.index,
            },
            Callee::Local(index) => Constant::FuncRef {
                key: FuncRefKey::Dep(0), // the enclosing module
                func: index,
            },
            Callee::Foreign { position, func, .. } => Constant::FuncRef {
                // position 0 is the enclosing module, so the dep entry's
                // list position shifts by one (G3.5's position key)
                key: FuncRefKey::Dep(position + 1),
                func,
            },
            Callee::Evidence(slot) => Constant::FuncRef {
                key: FuncRefKey::Native(crate::native::EVIDENCE_MODULE.to_owned()),
                func: u32::from(slot),
            },
        }
    }

    fn zero_word(&mut self, dst: u8, slot: SlotType) {
        let kidx = match slot {
            SlotType::I64 => {
                let k = self.pool(Constant::I64(0));
                self.emit(Op::Kimm {
                    dst: Slot::body(dst),
                    kidx: k,
                });
                return;
            },
            SlotType::F64 => self.pool(Constant::F64(0.0)),
            SlotType::Bool => self.pool(Constant::Bool(false)),
            SlotType::Ref | SlotType::Raw => {
                // the dead payload word owns its interned empty-string
                // reference like any production — a borrow candidate like
                // every KSTR load. A Raw dead word (a variable-keyed
                // member's claim another member's write kills) rides the
                // same discipline: its count is a runtime fact, and the
                // kind-checked dec reads a valid reference whatever the
                // instantiation
                let k = self.pool_str("");
                self.kstr(dst, k);
                return;
            },
            // Fn words need kind-true content too: the call slide
            // validates staged words against typed callee params, so
            // the zero is the frame's own scalar FuncRef — a copyable
            // word the dead member never calls
            SlotType::Fn => {
                let k = self.pool(Constant::FuncRef {
                    key: FuncRefKey::Dep(0), // the enclosing module
                    func: self.index,
                });
                self.emit(Op::Kconst {
                    dst: Slot::body(dst),
                    kidx: k,
                });
                return;
            },
            // A dead Tag word still needs kind-true content: RETSPAN
            // hands the whole span to the caller, whose layout types
            // every word of the union's span, so moving the frame's
            // inert I64 zero into a Tag-classified slot would trap the
            // caller's typed write. Tag id 0 is the module's first
            // minted user tag — a Tag region word only exists once some
            // union field's shape minted one — and a dead word is never
            // read (only the live member's words are), so the
            // discriminant's identity is inert; only its kind matters.
            SlotType::Tag => {
                self.emit(Op::TagNew {
                    dst: Slot::body(dst),
                    tag: 0,
                });
                return;
            },
            // no shape mints an I32 region word (explicit-width locals
            // only), and no uop writes one; unreachable until that
            // vocabulary exists
            SlotType::I32 => return,
        };
        self.emit(Op::Kconst {
            dst: Slot::body(dst),
            kidx,
        });
    }

    /// Writes kind-correct zeros into every payload word the constructed
    /// member does not fill: a field's whole claimed span stays live.
    fn zero_init_member(
        &mut self,
        base: u8,
        words: &[SlotType],
        member_fields: &[MemberField],
    ) -> Result<(), LowerError> {
        for (index, slot) in words.iter().enumerate().skip(1) {
            let word = u8::try_from(index).map_err(|_| mismatch("a union span"))?;
            let live = member_fields.iter().any(|field| {
                let start = usize::from(field.word);
                (start..start + field.width()).contains(&usize::from(word))
            });
            if !live {
                self.zero_word(base + word, *slot);
            }
        }
        Ok(())
    }
}

fn resolve(slot: Slot, body_len: usize) -> u8 {
    if slot.overlay {
        u8::try_from(body_len + usize::from(slot.index)).expect("overlay slot fits u8")
    } else {
        slot.index
    }
}

fn eq_op(slot: SlotType) -> Result<DataOp, LowerError> {
    Ok(match slot {
        SlotType::I64 => DataOp::EqI64,
        SlotType::F64 => DataOp::EqF64,
        SlotType::Bool => DataOp::EqBool,
        SlotType::Ref => DataOp::EqStr,
        _ => return Err(mismatch("a scalar equality")),
    })
}

/// The named refusal a list word's element gate answers: the element
/// kind's own plan found a word without content equality, so the chain
/// refuses naming the seam instead of reading the list's Ref word through
/// the EqStr uop (a runtime trap over a non-string object).
fn list_refusal() -> LowerError {
    LowerError::unsupported(
        no_span(),
        "a structural equality over list elements",
        "the content compare covers int, float, bool, string, bytes, struct, tuple, and nested \
         collection elements; callable and union elements have no content equality",
    )
}

/// The named refusal a map word's value gate answers, [`list_refusal`]'s
/// twin over the value channel.
fn map_refusal() -> LowerError {
    LowerError::unsupported(
        no_span(),
        "a structural equality over map values",
        "the content compare covers int, float, bool, string, bytes, struct, tuple, and nested \
         collection values; callable and union values have no content equality",
    )
}

/// How one word of a structural `==` chain compares [B9]: the word's
/// own scalar uop, or — when the word's field type is a heap collection
/// — that collection's content-compare native, because the EqStr uop
/// reads string objects alone. The chain selects per field type, never
/// per slot kind, so a bytes-, list-, or map-typed struct, tuple, or
/// union field, and a membership element over any of them, dispatch the
/// content compare.
#[derive(Clone, Copy, PartialEq, Eq)]
enum WordEq {
    /// The scalar word's equality uop.
    Scalar(DataOp),
    /// One bytes-typed word: the content compare through the native
    /// call.
    Bytes,
    /// One list-typed word: the content compare through the native call.
    List,
    /// One map-typed word: the content compare through the native call.
    Map,
}

/// The single identifier of a one-segment path expression, if it is one.
/// Whether one expression is an arithmetic binary at its outermost
/// node: the only shape that can take a scalar destination offer, so
/// the offer never survives into branch arms or nested calls, which own
/// other words.
fn arith_shape(expr: &Expr) -> bool {
    matches!(peel_parens(expr), Expr::Binary {
        op: BinaryOp::Add
            | BinaryOp::Sub
            | BinaryOp::Mul
            | BinaryOp::Div
            | BinaryOp::Rem
            | BinaryOp::WrapAdd
            | BinaryOp::WrapSub
            | BinaryOp::WrapMul
            | BinaryOp::Shl
            | BinaryOp::Shr
            | BinaryOp::BitAnd
            | BinaryOp::BitXor
            | BinaryOp::BitOr,
        ..
    })
}

/// Whether one argument's evaluation provably touches no overlay word
/// and mutates nothing: plain paths, literals, and field projections
/// over such expressions. A directly staged overlay word may be written
/// only while every later argument stays one of these.
fn trivially_staged(expr: &Expr) -> bool {
    match peel_parens(expr) {
        Expr::Literal { .. } | Expr::Path { .. } => true,
        Expr::Field { recv, .. } => trivially_staged(recv),
        _ => false,
    }
}

fn single_path_ident(expr: &Expr) -> Option<&Ident> {
    match expr {
        Expr::Path { path, .. } if path.segments.len() == 1 => path.segments.first(),
        _ => None,
    }
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

fn single(addrs: &[WordAddr], span: Span) -> Result<WordAddr, LowerError> {
    let [only] = addrs else {
        return Err(LowerError::unsupported(
            span,
            "a value",
            "this lowering point needs a single word; the value spans several",
        ));
    };
    Ok(only.clone())
}

/// The source words of one field inside a flattened address list: the
/// addresses group by field in span order, each group's base word
/// carrying the field's name.
/// The per-field source slices of one member's ordered fields over a
/// value's whole word addresses: a named field name-matches anywhere in
/// the list (a bare struct's addresses carry their field names), a
/// positional field consumes the next words in order (a tuple's
/// addresses carry no names — the walk is positional exactly where the
/// fields are). The fields arrive in declaration order and the
/// addresses in span order, so both walks stay monotone.
fn ordered_field_srcs<'a>(
    addrs: &'a [WordAddr],
    fields: &[MemberField],
) -> Result<Vec<&'a [WordAddr]>, LowerError> {
    let mut out = Vec::with_capacity(fields.len());
    let mut pos = 0usize;
    for field in fields {
        let width = field.width();
        if field.name.is_some() {
            let start = addrs
                .iter()
                .position(|a| a.name.as_deref() == field.name.as_deref())
                .ok_or_else(|| mismatch("a value field"))?;
            out.push(
                addrs
                    .get(start..start + width)
                    .ok_or_else(|| mismatch("a value field span"))?,
            );
        } else {
            out.push(
                addrs
                    .get(pos..pos + width)
                    .ok_or_else(|| mismatch("a value field span"))?,
            );
            pos += width;
        }
    }
    Ok(out)
}

/// Set difference over member lists.
/// The solved signature of a function-typed value: the fn atom's TypeId
/// comes from the handle's member set (one member, by construction).
fn fn_sig_of(universe: &ds_check::Universe, fn_ty: Ty) -> Option<(&[Ty], Ty)> {
    universe
        .members(fn_ty)
        .first()
        .and_then(|atom| universe.fn_sig(*atom))
}

fn set_diff(a: &[TypeId], b: &[TypeId]) -> Vec<TypeId> {
    a.iter().copied().filter(|m| !b.contains(m)).collect()
}

fn mismatch(what: &'static str) -> LowerError {
    LowerError::unsupported(
        no_span(),
        what,
        "the value's checked type and runtime representation disagree",
    )
}

fn no_span() -> Span {
    Span::new(FileId::new(0), 0, 0)
}

/// The span of one pattern node (no `span()` method on the enum).
fn pattern_span(pat: &Pattern) -> Span {
    match pat {
        Pattern::Wildcard(span)
        | Pattern::Literal { span, .. }
        | Pattern::Str { span, .. }
        | Pattern::RawStr { span, .. }
        | Pattern::Path { span, .. }
        | Pattern::Call { span, .. }
        | Pattern::Struct { span, .. }
        | Pattern::Tuple { span, .. }
        | Pattern::Binding { span, .. }
        | Pattern::Or { span, .. } => *span,
    }
}

fn int_radix(text: &str) -> (u32, &str) {
    if let Some(rest) = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
        (16, rest)
    } else if let Some(rest) = text.strip_prefix("0b").or_else(|| text.strip_prefix("0B")) {
        (2, rest)
    } else if let Some(rest) = text.strip_prefix("0o").or_else(|| text.strip_prefix("0O")) {
        (8, rest)
    } else {
        (10, text)
    }
}

fn parse_int(text: &str) -> Option<i64> {
    let (radix, digits) = int_radix(text);
    i64::from_str_radix(digits, radix).ok()
}

/// Whether one integer literal's magnitude is exactly i64::MIN's: the
/// sign-folded range check admits the negation as the min value, while
/// the magnitude alone sits outside the positive range, so the direct
/// `-` over the literal lowers as the min constant at the unary seam.
fn negated_int_min(text: &str) -> bool {
    let (radix, digits) = int_radix(text);
    u64::from_str_radix(digits, radix).is_ok_and(|m| m == 1u64 << 63)
}

/// The constant one scalar literal writes: the direct-store fast paths
/// (a scalar var's literal init, a literal assignment over a scalar
/// var) load it straight into the destination word.
#[derive(Clone, Copy)]
enum ScalarConst {
    I64(i64),
    F64(f64),
}

/// One scalar word an expression evaluation is offered as the
/// destination of its outermost data op: an assignment's var word, or
/// the overlay staging word a call site reserves for one argument.
/// `emit_data` always allocates a fresh temp for its result, so the
/// offered word is born at the consuming op with exactly one consumer
/// — the store that reads it back, or the CALL whose overlay slot
/// holds it — which is the single-use proof the direct write rides
/// on; a shape that does not take the offer keeps the temp spelling.
#[derive(Clone, Copy)]
enum ScalarDst {
    /// A scalar var word an assignment's plain store writes.
    Var { word: u8, slot: SlotType },
    /// An overlay staging word a call's argument writes.
    Arg { index: u8, slot: SlotType },
}

impl ScalarDst {
    /// The word index the consuming op's produced place reports.
    fn word(self) -> u8 {
        match self {
            Self::Var { word, .. } | Self::Arg { index: word, .. } => word,
        }
    }

    /// The word kind the destination carries.
    fn slot(self) -> SlotType {
        match self {
            Self::Var { slot, .. } | Self::Arg { slot, .. } => slot,
        }
    }

    /// The symbolic slot the consuming data op writes.
    fn slot_word(self) -> Slot {
        match self {
            Self::Var { word, .. } => Slot::body(word),
            Self::Arg { index, .. } => Slot::overlay(index),
        }
    }
}

/// Probes one literal against a scalar word's slot: suffixed or
/// unparseable spellings and kind disagreements answer None, so the
/// fast paths decline and the temp path (`literal`) owns the refusals.
fn scalar_const(lit: &Literal, slot: SlotType) -> Option<ScalarConst> {
    match (lit, slot) {
        (Literal::Int { suffix, text }, SlotType::I64) if suffix.is_empty() => {
            Some(ScalarConst::I64(parse_int(text)?))
        },
        (Literal::Float { suffix, text }, SlotType::F64) if suffix.is_empty() => {
            Some(ScalarConst::F64(text.parse::<f64>().ok()?))
        },
        (Literal::Byte(b), SlotType::I64) => Some(ScalarConst::I64(i64::from(*b))),
        _ => None,
    }
}
