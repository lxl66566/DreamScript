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

use ds_abi::{BuiltinInstance, ConstIdx, RcOp, SlotType, Staging, Uop};
use ds_check::{
    Analyzed, AtomKind, BinderFact, DefId, DotRes, Extraction, GenericCallBounds,
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
        GENERIC_NATIVES, GenericNative, NURSERY_BOUNDARY_ERR, Native, NativeShape, PLATFORM_IO_OK,
    },
    place::{Hook, Owned, Placement},
    shape::{MemberField, Shape, Shapes, SpanWords, StructShape, expr_members},
};

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
                    if let StrPart::Expr { expr, .. } = part {
                        self.expr(expr);
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
    /// A runtime-resolved trait dispatch inside a generic impl body: the
    /// frame's evidence slot whose word names the serving member body in
    /// the enclosing module (the sentinel FuncRef the CALLG carries).
    Evidence(u8),
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
        lin: &ds_check::Linearity,
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
                let mut rc = Placement::from_linearity(lin);
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
                for field in &s.fields {
                    let member = fields
                        .iter()
                        .find(|f| f.name == field.name)
                        .ok_or_else(|| mismatch("a union member field"))?;
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
        self.scope_push();
        let result = self.block_inner(block);
        self.scope_pop();
        result
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
        let bind = match self.res.get(only.span) {
            Some(Res::Local { bind }) => *bind,
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
            .get(&bind)
            .cloned()
            .ok_or_else(|| self.missing_binder(only.span))?;
        let declared = self
            .analyzed
            .checked
            .binders
            .get(&bind)
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
            | ds_parse::AssignOp::Rem => {
                // `x op= e` reads the current slots, then re-stores the
                // result: the same word moves the binary path emits.
                let current = TypedPlace {
                    place: place.clone(),
                    ty: declared,
                };
                if matches!(op, ds_parse::AssignOp::Rem) {
                    self.reject_float_rem(&current, left.span())?;
                }
                let (int_op, float_op) = match op {
                    ds_parse::AssignOp::Add => (DataOp::AddI64, DataOp::AddF64),
                    ds_parse::AssignOp::Sub => (DataOp::SubI64, DataOp::SubF64),
                    ds_parse::AssignOp::Mul => (DataOp::MulI64, DataOp::MulF64),
                    ds_parse::AssignOp::Div => (DataOp::DivI64, DataOp::DivF64),
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
            ds_parse::AssignOp::Shl | ds_parse::AssignOp::Shr => {
                return Err(LowerError::unsupported(
                    left.span(),
                    "a shift assignment",
                    "shift uops are outside the M3 contract",
                ))
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
    /// re-runs per iteration against the frozen end. The pattern binder
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

    /// A `for x in xs` over one list [F7]: the iterable evaluates once,
    /// then an index-counted fetch loop — one hidden induction word,
    /// `list_get` per iteration over its ok/none span (one payload word
    /// per element word at the list's stride), the none span exits. The
    /// pattern binder names the gathered element — one word or a whole
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
    /// order (a struct's flattened words are contiguous), each `Ref`
    /// word produced for the frame — the fetch handed one count per
    /// `Ref` word, which this place owns until its death. The one-word
    /// shape stays a bare word (a `Raw` element word is a type
    /// variable's runtime word, owned and kind-checked at its drop).
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
            if *slot == SlotType::Ref {
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
            _ => Err(LowerError::unsupported(
                span,
                "a list element of this shape",
                "iteration and membership bind one scalar word, one struct's flattened span, or a \
                 tuple's positional words; union elements keep the checked-read refusal",
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
            // a forward, so no placement bookkeeping runs here
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
    /// the trait-dispatch record (or the inherent one) the synthetic
    /// member call below resolves through.
    fn hole_show_dispatch(&self, span: Span) -> bool {
        self.analyzed.checked.trait_calls.contains_key(&span)
            || self.analyzed.checked.inherent_calls.contains_key(&span)
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
        let def = self.analyzed.package.lookup(name)?;
        self.analyzed.decls.atoms.get(&def).copied()
    }

    /// `x.f`: the member payload word, addressed through the receiver's
    /// checked (possibly refined) type.
    fn field_access(&mut self, e: &Expr, _static_ty: Ty) -> Result<Place, LowerError> {
        let Expr::Field { recv, name, .. } = e else {
            return Err(mismatch("a field access"));
        };
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
        // A one-word field reads as its word; a multi-word field (a
        // struct-typed field) reads as the span the expression's own
        // checked type names, nested access resolving through it.
        let shape = if field.width() == 1 {
            PlaceShape::Word(field.slot())
        } else {
            PlaceShape::Span(self.val(e.span())?.ty)
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
            if field.slot() == SlotType::Ref {
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
            | BinaryOp::WrapMul => self.scalar_dst.take(),
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
            BinaryOp::Eq | BinaryOp::Ne => (self.eq_chain(&lhs, &rhs, e.span())?, false),
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
                return Err(LowerError::unsupported(
                    *span,
                    "a bitwise or shift operator",
                    "outside the M3 uop contract",
                ))
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
                target: short,
            }
        } else {
            Op::Jmpif {
                cond: Slot::body(rcond.slot),
                target: short,
            }
        });
        let truth = self.pool(Constant::Bool(is_and));
        let dst = self.temp_word(SlotType::Bool)?;
        self.emit(Op::Kconst {
            dst: Slot::body(dst),
            kidx: truth,
        });
        let arm_full = self.rc.take();
        let full_div = self.rc.diverged();
        let full_jmp = self.code.len();
        self.emit(Op::Jmp { target: join });
        self.rc.restore(base);
        self.bind_label(short);
        let lie = self.pool(Constant::Bool(!is_and));
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
            self.splice(epilogue, full_jmp);
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
                "membership compares int, float, bool, and string elements",
            ));
        }
        let op = (!wide).then(|| eq_op(elem_words[0])).transpose()?;
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
            )?
        } else {
            // the one-word shape keeps its emission exactly (the word's
            // count dies at the comparison read)
            let (Some(word), Some(op)) = (x_w, op) else {
                return Err(mismatch("a membership comparison"));
            };
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
                let op = eq_op(*slot)?;
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
                dst
            },
            Shape::Struct(_) => {
                // the flattened word lists compare pairwise, a field's
                // span word-by-word in declaration order
                let fail = self.label();
                let join = self.label();
                let l = self.word_addrs(&lhs.place, lhs.ty)?;
                let r = self.word_addrs(&rhs.place, rhs.ty)?;
                self.emit_word_compares(&l, &r, fail)?;
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
                self.emit_word_compares(&l, &r, fail)?;
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
                dst
            },
            Shape::Union(_) => self.union_eq(lhs, rhs, lty)?,
        };
        Ok(Place {
            base: dst,
            shape: PlaceShape::Word(SlotType::Bool),
        })
    }

    /// Emits the pairwise word compares of two flattened address lists,
    /// every mismatched word jumping to `fail`; the last word's result
    /// flows on. A struct's whole span compares word-by-word this way —
    /// inline spans are flat words, so per-word equality is the value's.
    fn emit_word_compares(
        &mut self,
        l: &[WordAddr],
        r: &[WordAddr],
        fail: usize,
    ) -> Result<(), LowerError> {
        if l.len() != r.len() {
            return Err(mismatch("a span equality's width"));
        }
        for pair in l.iter().zip(r.iter()) {
            let cond = self.temp_word(SlotType::Bool)?;
            let op = eq_op(pair.0.slot_ty)?;
            self.emit(Op::Data {
                op,
                dst: Slot::body(cond),
                lhs: Slot::body(pair.0.slot),
                rhs: Slot::body(pair.1.slot),
            });
            if op == DataOp::EqStr {
                self.rc.read(&mut self.code, &[pair.0.slot, pair.1.slot]);
            }
            self.emit(Op::JmpifNot {
                cond: Slot::body(cond),
                target: fail,
            });
        }
        Ok(())
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
            } else {
                // the whole payload span compares field by field, the
                // same per-word chain a bare struct's equality rides
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
                self.emit_word_compares(&l, &r, false_lbl)?;
            }
            self.emit(Op::Jmp { target: done });
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

    // ---- control forms -------------------------------------------------------

    /// An `if` in value position: a result span both arms write into.
    fn if_value(&mut self, e: &Expr, ty: Ty) -> Result<Place, LowerError> {
        let Expr::If {
            cond, then, else_, ..
        } = e
        else {
            return Err(mismatch("an if expression"));
        };
        let cond_val = self.value(cond)?;
        let cond_word = single(&self.word_addrs(&cond_val.place, cond_val.ty)?, cond.span())?;
        let result = self.join_span(ty)?;
        let else_lbl = self.label();
        let join = self.label();
        self.emit(Op::JmpifNot {
            cond: Slot::body(cond_word.slot),
            target: else_lbl,
        });
        // path-fork the ownership map: each arm writes the result span
        // with its own value; the join keeps what both arms kept
        let base = self.rc.save();
        let then_tp = self.block_value(then)?;
        self.refuse_fn_word_answer(
            ty,
            &then_tp,
            then.span,
            "a capturing closure in a fn-typed join arm",
            "an unproven callable in a fn-typed join arm",
        )?;
        self.widen_into(&result, ty, &then_tp, then.span)?;
        let arm_then = self.rc.take();
        let then_div = self.rc.diverged();
        let then_jmp = self.code.len();
        self.emit(Op::Jmp { target: join });
        self.rc.restore(base);
        self.bind_label(else_lbl);
        if let Some(else_expr) = else_ {
            let else_tp = self.value(else_expr)?;
            self.refuse_fn_word_answer(
                ty,
                &else_tp,
                else_expr.span(),
                "a capturing closure in a fn-typed join arm",
                "an unproven callable in a fn-typed join arm",
            )?;
            self.widen_into(&result, ty, &else_tp, else_expr.span())?;
        } else {
            // a missing else is the unit else [B2]: the checker already
            // types the if as the then-type united with unit
            let unit = self.unit_value(e.span())?;
            self.widen_into(&result, ty, &unit, e.span())?;
        }
        let arm_else = self.rc.take();
        let else_div = self.rc.diverged();
        let dying = self
            .rc
            .reconcile(&[(arm_then, then_div), (arm_else, else_div)]);
        if let [then, else_] = &dying[..] {
            let mut epilogue = Vec::new();
            Placement::emit_drops(&mut epilogue, then);
            self.splice(epilogue, then_jmp);
            self.arm_epilogue(else_);
        }
        self.bind_label(join);
        Ok(result)
    }

    /// A `match` in value position.
    fn match_value(&mut self, e: &Expr, ty: Ty) -> Result<Place, LowerError> {
        let Expr::Match {
            scrutinee, arms, ..
        } = e
        else {
            return Err(mismatch("a match expression"));
        };
        let scrut = self.value(scrutinee)?;
        let result = self.join_span(ty)?;
        let join = self.label();
        self.match_arms(&scrut, arms, Some((&result, ty, join)))?;
        self.bind_label(join);
        Ok(result)
    }

    /// A `match` in tail position: every arm returns directly.
    fn match_tail(&mut self, e: &Expr) -> Result<(), LowerError> {
        let Expr::Match {
            scrutinee, arms, ..
        } = e
        else {
            return Err(mismatch("a match expression"));
        };
        let scrut = self.value(scrutinee)?;
        self.match_arms(&scrut, arms, None)
    }

    /// Emits one match: the dispatch chain, then every arm's body. Member
    /// arms TAG_TEST and jump in (an or-arm tests each alternative's
    /// tag), the catchall receives the fall-through; a catchall-free
    /// match is exhaustive by the checker's proof, so its dispatch chain
    /// has no live fall-through. A member arm may carry value-level tests
    /// over its own bound payload words (a case constructor pattern's
    /// argument, a struct member pattern's field): the tests run at the
    /// arm section's top and a mismatch slides to the next arm's
    /// dispatch entry, re-running its tag tests over the full scrutinee —
    /// the B2 remainder rule's tag-dispatch form, no reordering — so a
    /// can-fail arm (value tests or a guard) needs a later arm it lands
    /// in.
    fn match_arms(
        &mut self,
        scrut: &TypedPlace,
        arms: &[MatchArm],
        value: Option<(&Place, Ty, usize)>,
    ) -> Result<(), LowerError> {
        if !self.is_span_ty(scrut.ty) {
            return self.match_single(scrut, arms, value);
        }
        let labels: Vec<usize> = arms.iter().map(|_| self.label()).collect();
        // one dispatch-entry label per arm plus the chain tail: a failed
        // value test inside a member arm's section slides to the next
        // entry, continuing the dispatch from that arm onward
        let entries: Vec<usize> = (0..=arms.len()).map(|_| self.label()).collect();
        let arm_atoms: Vec<Vec<TypeId>> = arms
            .iter()
            .map(|arm| self.pattern_atoms(&arm.pat))
            .collect::<Result<_, _>>()?;
        let mut catchall = None;
        for (index, arm) in arms.iter().enumerate() {
            let atoms = &arm_atoms[index];
            // every arm owns an entry label: a slide into it finds a
            // bound target, and a catchall's intercepts the chain at its
            // position (arm order kept) unless the tail already jumps
            // there
            self.bind_label(entries[index]);
            if atoms.is_empty() {
                if arm.guard.is_some() {
                    return Err(LowerError::unsupported(
                        arm.span,
                        "a guarded catchall arm over union dispatch",
                        "a failed catchall guard slides to the next entry, and a value no later \
                         arm holds re-enters the catchall through the tail in a loop; bind a name \
                         and test it inside the body",
                    ));
                }
                catchall = Some(index);
                if index + 1 < arms.len() {
                    self.emit(Op::Jmp {
                        target: labels[index],
                    });
                }
                continue;
            }
            // the vocabulary checks over the collected atoms: a tested
            // alternative's payload words are live only under its own
            // tag, so or-alternatives with tests must name one member;
            // and a can-fail arm's slide (value tests or a guard)
            // must land, per atom, in a later settling arm (no tests,
            // no guard) covering it, or the catchall — otherwise the
            // failed test would fall off the checker-proved-exhaustive
            // chain tail into the first section
            let tested = Self::carries_member_tests(&arm.pat);
            if let Pattern::Or { pats, .. } = &arm.pat
                && tested
            {
                let mut distinct: Vec<TypeId> = Vec::new();
                for alt in pats {
                    for atom in self.pattern_atoms(alt)? {
                        if !distinct.contains(&atom) {
                            distinct.push(atom);
                        }
                    }
                }
                if distinct.len() > 1 {
                    return Err(LowerError::unsupported(
                        arm.span,
                        "an or-pattern of member tests",
                        "a tested alternative's payload words are live only under its own tag, so \
                         alternatives with payload tests must name one member; split the \
                         alternatives into arms",
                    ));
                }
            }
            if tested || arm.guard.is_some() {
                let mut distinct = atoms.clone();
                distinct.sort_unstable();
                distinct.dedup();
                for atom in &distinct {
                    let lands = arms[index + 1..].iter().enumerate().any(|(ahead, later)| {
                        let at = index + 1 + ahead;
                        let settles =
                            !Self::carries_member_tests(&later.pat) && later.guard.is_none();
                        settles && (arm_atoms[at].is_empty() || arm_atoms[at].contains(atom))
                    });
                    if !lands {
                        return Err(LowerError::unsupported(
                            arm.span,
                            "a member arm's value test or guard",
                            "the failed test would slide past the last arm into no landing \
                             section; add a binding arm for the member or a catchall arm",
                        ));
                    }
                }
            }
            for atom in atoms {
                let cond = self.temp_word(SlotType::Bool)?;
                let src = self.tag_addr(&scrut.place)?;
                let tag = self.shapes.tag_id(*atom)?;
                self.emit(Op::TagTest {
                    dst: Slot::body(cond),
                    src: Slot::body(src),
                    tag,
                });
                self.emit(Op::Jmpif {
                    cond: Slot::body(cond),
                    target: labels[index],
                });
            }
        }
        self.bind_label(entries[arms.len()]);
        if let Some(catchall) = catchall {
            self.emit(Op::Jmp {
                target: labels[catchall],
            });
        }
        // path-fork per arm: every arm starts from the dispatch state and
        // the join keeps what every arm kept
        let base = self.rc.save();
        let mut states = Vec::with_capacity(arms.len());
        let mut jumps = Vec::with_capacity(arms.len());
        let mut divs = Vec::with_capacity(arms.len());
        for (index, arm) in arms.iter().enumerate() {
            self.bind_label(labels[index]);
            if arm.ann.is_some() {
                return Err(LowerError::unsupported(
                    arm.span,
                    "an annotated match arm over a union",
                    "annotations narrow through the member machinery; union arms stay unannotated",
                ));
            }
            self.rc.restore(base.clone());
            self.emit_member_tests(&arm.pat, scrut, entries[index + 1])?;
            // a guard runs after the member tests: their failure edge is
            // base-equivalent (borrow reads), while the guard's edge
            // drops the section's transients before the slide
            let mut cleanup = None;
            if let Some(guard) = &arm.guard {
                let copy = self.guard_scrut_copy(arm, scrut)?;
                if let Some(copy) = &copy {
                    self.bind_guarded(&arm.pat, copy, scrut)?;
                } else {
                    self.bind_arm_pattern(&arm.pat, scrut)?;
                }
                let cond = self.value(guard)?;
                let after = self.rc.save();
                // the slide re-enters the dispatch over the base state:
                // a count the guard took would dec again at a later
                // section's exit, so the guard must leave it unchanged
                if base.keys().any(|word| !self.rc.owns(*word)) {
                    return Err(LowerError::unsupported(
                        arm.span,
                        "a guard taking a count over union dispatch",
                        "the failed guard slides to the next arm's entry, which re-enters the \
                         dispatch over the base state; keep a use after the match",
                    ));
                }
                let outstanding: Vec<(u8, Owned)> = after
                    .iter()
                    .filter(|(word, _)| !base.contains_key(word))
                    .map(|(word, owned)| (*word, owned.clone()))
                    .collect();
                let word = single(&self.word_addrs(&cond.place, cond.ty)?, arm.span)?;
                let label = self.label();
                self.emit(Op::JmpifNot {
                    cond: Slot::body(word.slot),
                    target: label,
                });
                cleanup = Some((label, outstanding));
            } else {
                self.bind_arm_pattern(&arm.pat, scrut)?;
            }
            // a value arm decs at its own jump what only it kept; a tail
            // arm exits the frame, its exit path dropping from its own
            // state
            if let Some((result, ty, join)) = value {
                let body_tp = self.value(&arm.body)?;
                self.refuse_fn_word_answer(
                    ty,
                    &body_tp,
                    arm.body.span(),
                    "a capturing closure in a fn-typed join arm",
                    "an unproven callable in a fn-typed join arm",
                )?;
                self.widen_into(result, ty, &body_tp, arm.body.span())?;
                states.push(self.rc.take());
                divs.push(self.rc.diverged());
                let at = self.code.len();
                self.emit(Op::Jmp { target: join });
                jumps.push(Some(at));
            } else {
                self.tail(&arm.body)?;
                states.push(self.rc.take());
                divs.push(self.rc.diverged());
                jumps.push(None);
            }
            if let Some((label, outstanding)) = cleanup {
                self.bind_label(label);
                Placement::emit_drops(&mut self.code, &outstanding);
                self.emit(Op::Jmp {
                    target: entries[index + 1],
                });
            }
        }
        let forked: Vec<_> = states.into_iter().zip(divs).collect();
        let dying = self.rc.reconcile(&forked);
        let mut shift = 0;
        for (at, dying) in jumps.into_iter().zip(&dying) {
            if let Some(at) = at {
                let mut epilogue = Vec::new();
                Placement::emit_drops(&mut epilogue, dying);
                let len = epilogue.len();
                self.splice(epilogue, at + shift);
                shift += len;
            }
        }
        Ok(())
    }

    /// A match over a non-union scrutinee (a tuple, a scalar, a struct —
    /// one member, no tags to dispatch on): one unguarded catchall arm
    /// lowers as direct destructuring, anything else as a linear test
    /// chain — each arm tests in order, a failed test or guard falls
    /// through to the next arm. A chain that can fail needs one
    /// unguarded catchall arm to close it: literal tests and guards
    /// prove no coverage, so the checker's tuple-arity coverage leaves
    /// the fall-through open.
    fn match_single(
        &mut self,
        scrut: &TypedPlace,
        arms: &[MatchArm],
        value: Option<(&Place, Ty, usize)>,
    ) -> Result<(), LowerError> {
        let kinds = arms
            .iter()
            .map(|arm| self.value_arm_kind(&arm.pat, scrut))
            .collect::<Result<Vec<ValueArmKind>, _>>()?;
        let catchall = arms
            .iter()
            .enumerate()
            .find(|(index, arm)| kinds[*index] == ValueArmKind::Catchall && arm.guard.is_none())
            .map(|(index, _)| index);
        let can_fail =
            kinds.contains(&ValueArmKind::Tested) || arms.iter().any(|arm| arm.guard.is_some());
        if can_fail && catchall.is_none() {
            let span = arms.first().map_or_else(no_span, |arm| arm.span);
            return Err(LowerError::unsupported(
                span,
                "a value-level match without a catchall arm",
                "literal tests and guards prove no coverage, so the checker's exhaustiveness \
                 proof leaves the fall-through open; one unguarded catchall arm closes the chain",
            ));
        }
        if let [arm] = arms {
            if arm.ann.is_some() {
                return Err(LowerError::unsupported(
                    arm.span,
                    "an annotated match arm",
                    "annotations narrow through the member machinery; value arms stay unannotated \
                     this round",
                ));
            }
            match &arm.pat {
                // a catchall-classified struct pattern (no field tests)
                // destructures directly, its classification already run
                Pattern::Tuple { .. }
                | Pattern::Binding { .. }
                | Pattern::Wildcard(_)
                | Pattern::Struct { .. } => {},
                _ => {
                    return Err(LowerError::unsupported(
                        pattern_span(&arm.pat),
                        "a member-test pattern on a non-union value",
                        "a single-member scrutinee has no tags to test",
                    ))
                },
            }
            self.bind_arm_pattern(&arm.pat, scrut)?;
            match value {
                Some((result, ty, _join)) => {
                    let body = self.value(&arm.body)?;
                    self.refuse_fn_word_answer(
                        ty,
                        &body,
                        arm.body.span(),
                        "a capturing closure in a fn-typed join arm",
                        "an unproven callable in a fn-typed join arm",
                    )?;
                    self.widen_into(result, ty, &body, arm.body.span())?;
                },
                None => self.tail(&arm.body)?,
            }
            return Ok(());
        }
        self.value_match_chain(
            scrut,
            arms,
            value,
            catchall.expect("the safety check passed"),
        )
    }

    /// The linear arm chain over a non-union value: every arm's section
    /// runs on the fall-through path in arm order (its tests
    /// short-circuit to the next entry on the first mismatch, its guard
    /// jumps there on failure), and each section snapshots its
    /// post-guard state so its body forks from exactly that — the join
    /// reconciles the per-arm states, so a count one guard ate stays
    /// eaten on every later path while a count no reached guard ate
    /// decs at its own arm's join jump. A guarded arm over a
    /// Ref-carrying scrutinee binds copies of the scrutinee's words: a
    /// guard that consumes its binder at the binder's last use must not
    /// eat the scrutinee's count, which later entries still test. The
    /// copy incs at the section's top, before its tests, so every path
    /// reaching the section incs what its decs will eat.
    fn value_match_chain(
        &mut self,
        scrut: &TypedPlace,
        arms: &[MatchArm],
        value: Option<(&Place, Ty, usize)>,
        catchall: usize,
    ) -> Result<(), LowerError> {
        for arm in arms {
            if arm.ann.is_some() {
                return Err(LowerError::unsupported(
                    arm.span,
                    "an annotated match arm",
                    "annotations narrow through the member machinery; value arms stay unannotated \
                     this round",
                ));
            }
        }
        let entries: Vec<usize> = arms.iter().map(|_| self.label()).collect();
        let mut states = Vec::with_capacity(arms.len());
        let mut jumps = Vec::with_capacity(arms.len());
        let mut divs = Vec::with_capacity(arms.len());
        for (index, arm) in arms.iter().enumerate() {
            self.bind_label(entries[index]);
            let fail = entries.get(index + 1).copied().unwrap_or(entries[catchall]);
            let copy = self.guard_scrut_copy(arm, scrut)?;
            self.emit_value_tests(&arm.pat, scrut, fail)?;
            if let Some(guard) = &arm.guard {
                match &copy {
                    Some(copy) => self.bind_guarded(&arm.pat, copy, scrut)?,
                    None => self.bind_arm_pattern(&arm.pat, scrut)?,
                }
                // a guard must not take the scrutinee's own Ref word's
                // count (a direct outer-name reference; the binder copy
                // shields the bound spelling): chain paths bypass the
                // guard while a later entry or body may re-read the word,
                // so the walk refuses the moment it sees the count go — a
                // guard that only reads the word keeps its count when the
                // death sits past the chain (a use after the match)
                let before = self.scrut_owned(scrut)?;
                let cond = self.value(guard)?;
                let eaten = before
                    .iter()
                    .any(|&(word, owned)| owned && !self.rc.owns(word));
                if eaten {
                    return Err(LowerError::unsupported(
                        arm.span,
                        "a guard consuming the scrutinee",
                        "chain paths bypass the guard while a later entry may re-read the word, \
                         so the guard must not take the scrutinee's count; keep a use after the \
                         match, or bind a name for the tested arm instead",
                    ));
                }
                let word = single(&self.word_addrs(&cond.place, cond.ty)?, arm.span)?;
                self.emit(Op::JmpifNot {
                    cond: Slot::body(word.slot),
                    target: fail,
                });
            } else {
                self.bind_arm_pattern(&arm.pat, scrut)?;
            }
            let after_entry = self.rc.save();
            // a value arm decs at its own jump what only it kept; a tail
            // arm exits the frame, its exit path dropping from its own
            // state
            if let Some((result, ty, join)) = value {
                let body_tp = self.value(&arm.body)?;
                self.refuse_fn_word_answer(
                    ty,
                    &body_tp,
                    arm.body.span(),
                    "a capturing closure in a fn-typed join arm",
                    "an unproven callable in a fn-typed join arm",
                )?;
                self.widen_into(result, ty, &body_tp, arm.body.span())?;
                states.push(self.rc.take());
                divs.push(self.rc.diverged());
                let at = self.code.len();
                self.emit(Op::Jmp { target: join });
                jumps.push(Some(at));
            } else {
                self.tail(&arm.body)?;
                states.push(self.rc.take());
                divs.push(self.rc.diverged());
                jumps.push(None);
            }
            // the next entry's path executed this section's tests and
            // guard but never its body, so the chain walk resumes from
            // the post-guard state (a body that exited the frame also
            // cleared the map; the snapshot is what keeps walking)
            self.rc.restore(after_entry);
        }
        let forked: Vec<_> = states.into_iter().zip(divs).collect();
        let dying = self.rc.reconcile(&forked);
        let mut shift = 0;
        for (at, dying) in jumps.into_iter().zip(&dying) {
            if let Some(at) = at {
                let mut epilogue = Vec::new();
                Placement::emit_drops(&mut epilogue, dying);
                let len = epilogue.len();
                self.splice(epilogue, at + shift);
                shift += len;
            }
        }
        Ok(())
    }

    /// The scrutinee copy a guarded arm binds when the scrutinee carries
    /// Ref words and the pattern binds them: `None` for unguarded arms,
    /// scalar scrutinees (no count rides the words, so the guard cannot
    /// eat one), and binder-free patterns (`_`, an atom naming the sole
    /// member — a guard cannot reach the scrutinee through them). A
    /// deinit-carrying scrutinee refuses: the copy would anchor a second
    /// claim, so the value would deinit twice.
    /// Whether a guarded arm's pattern binds a plain name reading the
    /// scrutinee's words (through the binder, so a guard consuming one
    /// needs the copy): a plain binding, tuple or constructor payload
    /// binders, punned or plain-name struct fields, and an or-arm
    /// through any alternative that binds.
    fn pattern_binds_words(&self, pat: &Pattern) -> bool {
        match pat {
            Pattern::Binding { name, .. } => self.binding_atom(&name.name).is_none(),
            Pattern::Tuple { pats, .. } | Pattern::Call { args: pats, .. } => {
                pats.iter().any(|sub| self.pattern_binds_words(sub))
            },
            Pattern::Struct { fields, .. } => fields.iter().any(|(_, sub)| match sub {
                None => true,
                Some(sub) => self.pattern_binds_words(sub),
            }),
            Pattern::Or { pats, .. } => pats.iter().any(|alt| self.pattern_binds_words(alt)),
            _ => false,
        }
    }

    fn guard_scrut_copy(
        &mut self,
        arm: &MatchArm,
        scrut: &TypedPlace,
    ) -> Result<Option<TypedPlace>, LowerError> {
        let binds = self.pattern_binds_words(&arm.pat);
        if arm.guard.is_none() || !binds {
            return Ok(None);
        }
        let words = self.shapes.words(scrut.ty)?;
        if !words.contains(&SlotType::Ref) {
            return Ok(None);
        }
        if self
            .deinit_claim(scrut.ty, pattern_span(&arm.pat))?
            .is_some()
        {
            return Err(LowerError::unsupported(
                arm.span,
                "a guarded arm over a deinit-carrying value",
                "the guard's binder copy would anchor a second claim, so the value would deinit \
                 twice; the payload-pattern seam covers it",
            ));
        }
        self.scrut_copy(scrut).map(Some)
    }

    /// The scrutinee's Ref words and whether the placement currently
    /// owns each: a guard's consumption check reads the pair before and
    /// after the guard's walk.
    fn scrut_owned(&mut self, scrut: &TypedPlace) -> Result<Vec<(u8, bool)>, LowerError> {
        let addrs = self.word_addrs(&scrut.place, scrut.ty)?;
        Ok(addrs
            .iter()
            .filter(|addr| addr.slot_ty == SlotType::Ref)
            .map(|addr| (addr.slot, self.rc.owns(addr.slot)))
            .collect())
    }

    /// Binds one guarded arm's pattern over its scrutinee copy: the
    /// plain binding aliases the copy's whole span, a tuple pattern its
    /// elements (a literal element tested nothing to bind), a member
    /// arm's fields or payload binders (and an or-arm's last
    /// alternative) read the copy's words, and a wildcard or atom-named
    /// binding binds nothing at all.
    fn bind_guarded(
        &mut self,
        pat: &Pattern,
        copy: &TypedPlace,
        scrut: &TypedPlace,
    ) -> Result<(), LowerError> {
        match pat {
            Pattern::Binding { span, name } if self.binding_atom(&name.name).is_none() => {
                self.binders.insert(*span, copy.place.clone());
                self.rc_bind(*span, &copy.place, scrut.ty)?;
                self.scope_insert(&name.name, *span);
                Ok(())
            },
            Pattern::Tuple { pats, .. } => self.bind_tuple_pattern(pats, copy, true),
            Pattern::Struct { .. } | Pattern::Call { .. } | Pattern::Or { .. } => {
                self.bind_arm_pattern(pat, copy)
            },
            _ => self.bind_arm_pattern(pat, scrut),
        }
    }

    /// A fresh word-for-word copy of the scrutinee: Ref words dup
    /// unconditionally — the source's death position is the checker's,
    /// not this use's, because the fall-through keeps the scrutinee —
    /// and scalar words mov.
    fn scrut_copy(&mut self, scrut: &TypedPlace) -> Result<TypedPlace, LowerError> {
        let words = self.shapes.words(scrut.ty)?;
        let base = self.alloc(&words)?;
        let place = self.place_of_ty(base, scrut.ty)?;
        let addrs = self.word_addrs(&scrut.place, scrut.ty)?;
        for (index, addr) in addrs.iter().enumerate() {
            let word = u8::try_from(index).map_err(|_| mismatch("a scrutinee copy"))?;
            self.emit(Op::Mov {
                dst: Slot::body(base + word),
                src: Slot::body(addr.slot),
            });
            if addr.slot_ty == SlotType::Ref {
                self.rc.duped(&mut self.code, base + word, addr.slot);
            }
        }
        Ok(TypedPlace {
            place,
            ty: scrut.ty,
        })
    }

    /// Whether a member arm's pattern carries value-level tests over its
    /// own bound payload words: a case constructor pattern with a tested
    /// argument, a struct member pattern with a tested field, or an
    /// or-arm of such alternatives.
    fn carries_member_tests(pat: &Pattern) -> bool {
        match pat {
            Pattern::Call { args, .. } => args.iter().any(Self::carries_word_tests),
            Pattern::Struct { fields, .. } => fields
                .iter()
                .any(|(_, sub)| sub.as_ref().is_some_and(Self::carries_word_tests)),
            Pattern::Or { pats, .. } => pats.iter().any(Self::carries_member_tests),
            _ => false,
        }
    }

    /// Whether a one-word or nested-struct sub-pattern carries a test:
    /// a literal, an or-ladder of tests, or a nested struct sub-pattern
    /// with a tested inner field at any depth.
    fn carries_word_tests(sub: &Pattern) -> bool {
        match sub {
            Pattern::Literal { .. } | Pattern::Str { .. } | Pattern::RawStr { .. } => true,
            // a nested struct sub-pattern tests its inner fields at the
            // struct-typed field's own offsets; pure binders and skips
            // test nothing
            Pattern::Struct { fields, .. } => fields
                .iter()
                .any(|(_, sub)| sub.as_ref().is_some_and(Self::carries_word_tests)),
            Pattern::Or { pats, .. } => pats.iter().any(Self::carries_word_tests),
            _ => false,
        }
    }

    /// Emits one member arm's value tests over its own bound payload
    /// words, in payload order, a mismatch sliding to the next dispatch
    /// entry (the value chain's short-circuit discipline over the
    /// member's claimed region offsets; the tested words are borrows
    /// across every arm's tests, the tag word's own discipline).
    /// Binding-only arms and catchalls test nothing.
    fn emit_member_tests(
        &mut self,
        pat: &Pattern,
        scrut: &TypedPlace,
        fail: usize,
    ) -> Result<(), LowerError> {
        match pat {
            Pattern::Call { path, args, .. } => {
                let [only] = path.segments.as_slice() else {
                    return Ok(());
                };
                let Some(atom) = self.path_atom(&only.name) else {
                    return Ok(());
                };
                let layout = self.shapes.shape(scrut.ty)?;
                let fields = layout
                    .member_fields(atom)
                    .ok_or_else(|| mismatch("a union member read"))?;
                for (arg, field) in args.iter().zip(fields.iter()) {
                    // a struct-typed payload's nested struct sub-pattern
                    // tests its fields at the payload's own offsets
                    if let Pattern::Struct { .. } = arg {
                        self.emit_struct_sub_tests(arg, scrut.place.base + field.word, fail)?;
                        continue;
                    }
                    if Self::word_sub_kind(arg, "a case payload test")? == ValueArmKind::Tested {
                        let addr = WordAddr {
                            name: field.name.clone(),
                            slot: scrut.place.base + field.word,
                            slot_ty: field.slot(),
                        };
                        self.emit_word_tests(arg, &addr, fail)?;
                    }
                }
                Ok(())
            },
            Pattern::Struct { path, fields, .. } => {
                let [only] = path.segments.as_slice() else {
                    return Ok(());
                };
                let Some(atom) = self.path_atom(&only.name) else {
                    return Ok(());
                };
                let layout = self.shapes.shape(scrut.ty)?;
                let member_fields = layout
                    .member_fields(atom)
                    .ok_or_else(|| mismatch("a union member read"))?;
                for (name, sub) in fields {
                    let Some(field) = member_fields
                        .iter()
                        .find(|f| f.name.as_deref() == Some(name.name.as_str()))
                    else {
                        return Err(mismatch("a pattern field"));
                    };
                    if let Some(inner @ Pattern::Struct { .. }) = sub {
                        self.emit_struct_sub_tests(inner, scrut.place.base + field.word, fail)?;
                        continue;
                    }
                    if let Some(sub) = sub
                        && Self::word_sub_kind(sub, "a struct field test")? == ValueArmKind::Tested
                    {
                        let addr = WordAddr {
                            name: field.name.clone(),
                            slot: scrut.place.base + field.word,
                            slot_ty: field.slot(),
                        };
                        self.emit_word_tests(sub, &addr, fail)?;
                    }
                }
                Ok(())
            },
            Pattern::Or { pats, .. } => {
                // the alternative ladder: a non-last alternative's tests
                // slide to the next alternative on mismatch and jump past
                // the rest on success; the last one's mismatch slides to
                // the arm's own next entry
                let ok = self.label();
                for (index, sub) in pats.iter().enumerate() {
                    if index + 1 == pats.len() {
                        self.emit_member_tests(sub, scrut, fail)?;
                    } else {
                        let next = self.label();
                        self.emit_member_tests(sub, scrut, next)?;
                        self.emit(Op::Jmp { target: ok });
                        self.bind_label(next);
                    }
                }
                self.bind_label(ok);
                Ok(())
            },
            _ => Ok(()),
        }
    }

    /// How one arm's pattern tests a non-union scrutinee: `_`, a plain
    /// binding, an atom naming the scrutinee's sole member (always true,
    /// binding nothing), and a destructuring tuple without literal
    /// elements run no test; literal elements and scalar literals test
    /// the value in place, and an or-pattern tests its alternatives'
    /// tests as a disjunction (any catchall alternative makes the whole
    /// arm catch no test).
    fn value_arm_kind(
        &mut self,
        pat: &Pattern,
        scrut: &TypedPlace,
    ) -> Result<ValueArmKind, LowerError> {
        if let Pattern::Tuple {
            pats,
            trailing_comma: false,
            ..
        } = pat
        {
            if pats.len() == 1 {
                return self.value_arm_kind(&pats[0], scrut);
            }
        }
        match pat {
            Pattern::Wildcard(_) | Pattern::Binding { .. } => Ok(ValueArmKind::Catchall),
            Pattern::Literal { .. } | Pattern::Str { .. } | Pattern::RawStr { .. } => {
                Ok(ValueArmKind::Tested)
            },
            Pattern::Tuple { pats, .. } => {
                let shape = self.shapes.shape(scrut.ty)?;
                let Shape::Tuple(s) = &*shape else {
                    return Err(mismatch("a tuple destructuring"));
                };
                if pats.len() != s.fields.len() {
                    return Err(mismatch("a tuple pattern's arity"));
                }
                let mut tested = false;
                for sub in pats {
                    tested |=
                        Self::word_sub_kind(sub, "a tuple element test")? == ValueArmKind::Tested;
                }
                Ok(if tested { ValueArmKind::Tested } else { ValueArmKind::Catchall })
            },
            Pattern::Or { pats, .. } => {
                self.or_binder_agreement(pats, scrut)?;
                for sub in pats {
                    if self.value_arm_kind(sub, scrut)? == ValueArmKind::Catchall {
                        // a catchall alternative always matches, so the
                        // disjunction never fails
                        return Ok(ValueArmKind::Catchall);
                    }
                }
                Ok(ValueArmKind::Tested)
            },
            Pattern::Path { .. } | Pattern::Call { .. } => Err(LowerError::unsupported(
                pattern_span(pat),
                "a member-test pattern on a non-union value",
                "a single-member scrutinee has no tags to test",
            )),
            Pattern::Struct { path, fields, .. } => {
                let [only] = path.segments.as_slice() else {
                    return Err(mismatch("a pattern path"));
                };
                // the checker proved the pattern's atom is the
                // scrutinee's sole member, so the fields classify
                if self.path_atom(&only.name).is_none() {
                    return Err(mismatch("a pattern's member atom"));
                }
                let shape = self.shapes.shape(scrut.ty)?;
                let Shape::Struct(s) = &*shape else {
                    return Err(mismatch("a struct pattern's scrutinee"));
                };
                let mut tested = false;
                for (name, sub) in fields {
                    if s.fields
                        .iter()
                        .all(|f| f.name.as_deref() != Some(name.name.as_str()))
                    {
                        return Err(mismatch("a pattern field"));
                    }
                    if let Some(sub) = sub {
                        tested |= Self::word_sub_kind(sub, "a struct field test")?
                            == ValueArmKind::Tested;
                    }
                }
                Ok(if tested { ValueArmKind::Tested } else { ValueArmKind::Catchall })
            },
        }
    }

    /// The kind of a one-word sub-pattern (a tuple element, a struct
    /// field): `_` and bindings test nothing (bindings bind at arm
    /// entry; an atom-named sub binds nothing per the member-test
    /// predicate), literals test the word, and an or-pattern tests its
    /// alternatives' tests as a disjunction. Wider sub-patterns have no
    /// word to test against.
    fn word_sub_kind(sub: &Pattern, what: &'static str) -> Result<ValueArmKind, LowerError> {
        match sub {
            Pattern::Wildcard(_) | Pattern::Binding { .. } => Ok(ValueArmKind::Catchall),
            Pattern::Literal { .. } | Pattern::Str { .. } | Pattern::RawStr { .. } => {
                Ok(ValueArmKind::Tested)
            },
            Pattern::Or { pats, .. } => {
                for alt in pats {
                    if Self::word_sub_kind(alt, what)? == ValueArmKind::Catchall {
                        return Ok(ValueArmKind::Catchall);
                    }
                }
                Ok(ValueArmKind::Tested)
            },
            // a nested struct sub-pattern over a struct-typed field
            // tests its inner fields; pure binders and skips catch
            // nothing
            Pattern::Struct { fields, .. } => {
                let tested = fields
                    .iter()
                    .any(|(_, sub)| sub.as_ref().is_some_and(Self::carries_word_tests));
                Ok(if tested { ValueArmKind::Tested } else { ValueArmKind::Catchall })
            },
            _ => Err(LowerError::unsupported(
                pattern_span(sub),
                what,
                "one word tests as a literal or an or-ladder of literals, binds as a plain name, \
                 or skips as `_`; nested patterns need the word's whole span",
            )),
        }
    }

    /// Every alternative of a value-level or-pattern binds its binders
    /// on the same words: the arm body binds the last alternative's
    /// binders (the scope's name map keeps the last binding), so an
    /// alternative binding a name at a different word would read the
    /// wrong value on its own path. The checker keeps the alternatives'
    /// name set and its order, never the words.
    fn or_binder_agreement(
        &mut self,
        pats: &[Pattern],
        scrut: &TypedPlace,
    ) -> Result<(), LowerError> {
        let words = self.shapes.words(scrut.ty)?;
        let mut agreed: Option<Vec<(String, u8, Vec<SlotType>)>> = None;
        for sub in pats {
            let mut binds = self.alt_binder_words(sub, scrut, 0, &words)?;
            binds.sort_by(|a, b| a.0.cmp(&b.0));
            if let Some(expected) = &agreed
                && expected != &binds
            {
                return Err(LowerError::unsupported(
                    pattern_span(sub),
                    "or-pattern alternatives binding different words",
                    "the arm body binds the last alternative's words, so an alternative binding a \
                     name elsewhere would read the wrong value on its own path",
                ));
            }
            agreed = Some(binds);
        }
        Ok(())
    }

    /// One or-alternative's binder words as (name, word offset, slot
    /// span) triples: a plain binding names the walked span, tuple and
    /// struct alternatives bind through their elements and fields, and
    /// a nested or-ladder contributes every alternative's binders (the
    /// agreement check compares whole alternatives).
    fn alt_binder_words(
        &mut self,
        pat: &Pattern,
        scrut: &TypedPlace,
        off: u8,
        slots: &[SlotType],
    ) -> Result<Vec<(String, u8, Vec<SlotType>)>, LowerError> {
        if let Pattern::Tuple {
            pats,
            trailing_comma: false,
            ..
        } = pat
        {
            if pats.len() == 1 {
                return self.alt_binder_words(&pats[0], scrut, off, slots);
            }
        }
        match pat {
            Pattern::Wildcard(_)
            | Pattern::Literal { .. }
            | Pattern::Str { .. }
            | Pattern::RawStr { .. } => Ok(Vec::new()),
            Pattern::Binding { name, .. } => {
                if self.binding_atom(&name.name).is_some() {
                    return Err(LowerError::unsupported(
                        pattern_span(pat),
                        "a member-test or-pattern alternative",
                        "an or-pattern over a non-union value tests values; a tag arm needs a \
                         union scrutinee",
                    ));
                }
                Ok(vec![(name.name.clone(), off, slots.to_vec())])
            },
            Pattern::Tuple { pats, .. } => {
                let shape = self.shapes.shape(scrut.ty)?;
                let Shape::Tuple(s) = &*shape else {
                    return Err(mismatch("a tuple destructuring"));
                };
                if pats.len() != s.fields.len() {
                    return Err(mismatch("a tuple pattern's arity"));
                }
                let mut out = Vec::new();
                for (sub, field) in pats.iter().zip(s.fields.iter()) {
                    out.extend(self.alt_binder_words(
                        sub,
                        scrut,
                        off + field.word,
                        &field.slots,
                    )?);
                }
                Ok(out)
            },
            Pattern::Or { pats, .. } => {
                let mut out = Vec::new();
                for sub in pats {
                    out.extend(self.alt_binder_words(sub, scrut, off, slots)?);
                }
                Ok(out)
            },
            Pattern::Struct { .. } => {
                // the pattern's own atom owns the field offsets (the
                // checker proved it is the walked type), so a nested
                // struct sub-pattern resolves its own fields one level
                // deeper the same way
                let struct_shape = self.struct_sub_shape(pat)?;
                let fields = &struct_shape.fields;
                let Pattern::Struct { fields: pats, .. } = pat else {
                    return Err(mismatch("a struct pattern"));
                };
                let mut out = Vec::new();
                for (name, sub) in pats {
                    let Some(field) = fields
                        .iter()
                        .find(|f| f.name.as_deref() == Some(name.name.as_str()))
                    else {
                        return Err(mismatch("a pattern field"));
                    };
                    match sub {
                        // punning binds the field's name at its span
                        None => {
                            out.push((name.name.clone(), off + field.word, field.slots.clone()));
                        },
                        Some(sub) => out.extend(self.alt_binder_words(
                            sub,
                            scrut,
                            off + field.word,
                            &field.slots,
                        )?),
                    }
                }
                Ok(out)
            },
            Pattern::Path { .. } | Pattern::Call { .. } => Err(LowerError::unsupported(
                pattern_span(pat),
                "a member-test pattern on a non-union value",
                "a single-member scrutinee has no tags to test",
            )),
        }
    }

    /// Emits one arm's value tests in place, in element order: each
    /// literal word compares against its constant and jumps to `fail`
    /// on the first mismatch (short-circuit), and an or-pattern's
    /// alternatives test as a disjunction — each alternative's tests
    /// run in order, a mismatch sliding to the next alternative and
    /// the last mismatch jumping to `fail`, so the arm survives on the
    /// first alternative that matches. A catchall arm emits nothing.
    fn emit_value_tests(
        &mut self,
        pat: &Pattern,
        scrut: &TypedPlace,
        fail: usize,
    ) -> Result<(), LowerError> {
        if let Pattern::Tuple {
            pats,
            trailing_comma: false,
            ..
        } = pat
        {
            if pats.len() == 1 {
                return self.emit_value_tests(&pats[0], scrut, fail);
            }
        }
        match pat {
            Pattern::Wildcard(_) | Pattern::Binding { .. } => Ok(()),
            Pattern::Literal { .. } | Pattern::Str { .. } | Pattern::RawStr { .. } => {
                let (place, slot) = self.pattern_const(pat)?;
                let word = single(&self.word_addrs(&scrut.place, scrut.ty)?, pattern_span(pat))?;
                self.emit_value_eq(&word, &place, slot, fail)
            },
            Pattern::Tuple { pats, .. } => {
                let shape = self.shapes.shape(scrut.ty)?;
                let Shape::Tuple(s) = &*shape else {
                    return Err(mismatch("a tuple destructuring"));
                };
                if pats.len() != s.fields.len() {
                    return Err(mismatch("a tuple pattern's arity"));
                }
                let addrs = self.word_addrs(&scrut.place, scrut.ty)?;
                for (sub, addr) in pats.iter().zip(addrs.iter()) {
                    if Self::word_sub_kind(sub, "a tuple element test")? == ValueArmKind::Tested {
                        self.emit_word_tests(sub, addr, fail)?;
                    }
                }
                Ok(())
            },
            Pattern::Struct { path, fields, .. } => {
                let [only] = path.segments.as_slice() else {
                    return Err(mismatch("a pattern path"));
                };
                let Some(_atom) = self.path_atom(&only.name) else {
                    return Err(mismatch("a pattern's member atom"));
                };
                let shape = self.shapes.shape(scrut.ty)?;
                let Shape::Struct(s) = &*shape else {
                    return Err(mismatch("a struct pattern's scrutinee"));
                };
                // the field words are the scrutinee's own consecutive
                // span words; each tested field compares its word in
                // place, a mismatch jumping to the next arm entry
                let addrs = self.word_addrs(&scrut.place, scrut.ty)?;
                for (name, sub) in fields {
                    let Some(field) = s
                        .fields
                        .iter()
                        .find(|f| f.name.as_deref() == Some(name.name.as_str()))
                    else {
                        return Err(mismatch("a pattern field"));
                    };
                    if let Some(inner @ Pattern::Struct { .. }) = sub {
                        self.emit_struct_sub_tests(inner, scrut.place.base + field.word, fail)?;
                        continue;
                    }
                    let addr = addrs
                        .get(usize::from(field.word))
                        .ok_or_else(|| mismatch("a pattern field's word"))?;
                    if let Some(sub) = sub
                        && Self::word_sub_kind(sub, "a struct field test")? == ValueArmKind::Tested
                    {
                        self.emit_word_tests(sub, addr, fail)?;
                    }
                }
                Ok(())
            },
            Pattern::Or { pats, .. } => {
                // the alternative ladder: a non-last alternative's tests
                // slide to the next alternative on mismatch and jump
                // past the remaining ones on success; the last
                // alternative's mismatch is the arm's own failure
                let ok = self.label();
                for (index, sub) in pats.iter().enumerate() {
                    if index + 1 == pats.len() {
                        self.emit_value_tests(sub, scrut, fail)?;
                    } else {
                        let next = self.label();
                        self.emit_value_tests(sub, scrut, next)?;
                        self.emit(Op::Jmp { target: ok });
                        self.bind_label(next);
                    }
                }
                self.bind_label(ok);
                Ok(())
            },
            _ => Err(mismatch("a value test the classification passed")),
        }
    }

    /// Emits a one-word sub-pattern's tests (a tuple element, a struct
    /// field): a literal compares its word and jumps to `fail` on
    /// mismatch, an or-ladder tests its alternatives against the same
    /// word, and bindings and wildcards test nothing.
    fn emit_word_tests(
        &mut self,
        sub: &Pattern,
        addr: &WordAddr,
        fail: usize,
    ) -> Result<(), LowerError> {
        match sub {
            Pattern::Wildcard(_) | Pattern::Binding { .. } => Ok(()),
            Pattern::Literal { .. } | Pattern::Str { .. } | Pattern::RawStr { .. } => {
                let (place, slot) = self.pattern_const(sub)?;
                self.emit_value_eq(addr, &place, slot, fail)
            },
            Pattern::Or { pats, .. } => {
                let ok = self.label();
                for (index, alt) in pats.iter().enumerate() {
                    if index + 1 == pats.len() {
                        self.emit_word_tests(alt, addr, fail)?;
                    } else {
                        let next = self.label();
                        self.emit_word_tests(alt, addr, next)?;
                        self.emit(Op::Jmp { target: ok });
                        self.bind_label(next);
                    }
                }
                self.bind_label(ok);
                Ok(())
            },
            _ => Err(mismatch("a value test the classification passed")),
        }
    }

    /// The declared fields of the struct a nested struct sub-pattern's
    /// path names: the checker proved the pattern's atom is the
    /// struct-typed field's own type, so the atom's declaration owns the
    /// nested field offsets. The shape carries the fields by value, so
    /// the walk reads them without copying the list.
    fn struct_sub_shape(&mut self, pat: &Pattern) -> Result<StructShape, LowerError> {
        let Pattern::Struct { path, .. } = pat else {
            return Err(mismatch("a nested struct sub-pattern"));
        };
        let [only] = path.segments.as_slice() else {
            return Err(mismatch("a pattern path"));
        };
        let Some(atom) = self.path_atom(&only.name) else {
            return Err(mismatch("a struct sub-pattern's atom"));
        };
        let shape = self.shapes.atom_shape(atom)?;
        let Shape::Struct(s) = shape else {
            return Err(mismatch("a nested struct sub-pattern's field"));
        };
        Ok(s)
    }

    /// Emits a nested struct sub-pattern's field tests at `base`, the
    /// offset its struct-typed field claims: the inner fields test in
    /// declaration order at the nested span's own consecutive words, a
    /// mismatch sliding to `fail` like any member test, and a
    /// struct-typed inner field's own nested sub-pattern recurses at
    /// the shifted base. Binding-only fields test nothing here.
    fn emit_struct_sub_tests(
        &mut self,
        pat: &Pattern,
        base: u8,
        fail: usize,
    ) -> Result<(), LowerError> {
        let struct_shape = self.struct_sub_shape(pat)?;
        let fields = &struct_shape.fields;
        let Pattern::Struct { fields: pats, .. } = pat else {
            return Err(mismatch("a nested struct sub-pattern"));
        };
        for (name, sub) in pats {
            let Some(field) = fields
                .iter()
                .find(|f| f.name.as_deref() == Some(name.name.as_str()))
            else {
                return Err(mismatch("a pattern field"));
            };
            let Some(sub) = sub else { continue };
            if let Pattern::Struct { .. } = sub {
                self.emit_struct_sub_tests(sub, base + field.word, fail)?;
            } else if Self::word_sub_kind(sub, "a nested struct field test")?
                == ValueArmKind::Tested
            {
                let addr = WordAddr {
                    name: field.name.clone(),
                    slot: base + field.word,
                    slot_ty: field.slot(),
                };
                self.emit_word_tests(sub, &addr, fail)?;
            }
        }
        Ok(())
    }

    /// One equality test against a constant word: the tested word's slot
    /// picks the comparison, a string constant reads its Ref at the use
    /// (the comparison drops a dying constant there), and a mismatch
    /// jumps to `fail`.
    fn emit_value_eq(
        &mut self,
        tested: &WordAddr,
        constant: &Place,
        slot: SlotType,
        fail: usize,
    ) -> Result<(), LowerError> {
        if slot != tested.slot_ty {
            return Err(mismatch("a literal pattern's word class"));
        }
        let op = eq_op(tested.slot_ty)?;
        let cond = self.temp_word(SlotType::Bool)?;
        self.emit(Op::Data {
            op,
            dst: Slot::body(cond),
            lhs: Slot::body(tested.slot),
            rhs: Slot::body(constant.base),
        });
        if op == DataOp::EqStr {
            // the tested word is a borrow across every arm's test (the
            // union dispatch reads its tag word the same way); the
            // constant dies at its use, its count balanced per pass
            self.rc.read(&mut self.code, &[constant.base]);
        }
        self.emit(Op::JmpifNot {
            cond: Slot::body(cond),
            target: fail,
        });
        Ok(())
    }

    /// The constant word a literal pattern tests against: the scalar
    /// literal family lowers like the expression literal, a string
    /// pattern its interned constant (interpolation holes refuse).
    fn pattern_const(&mut self, pat: &Pattern) -> Result<(Place, SlotType), LowerError> {
        let intern = |me: &mut Self, text: &str| -> Result<(Place, SlotType), LowerError> {
            let dst = me.temp_word(SlotType::Ref)?;
            let kidx = me.pool_str(text);
            me.kstr(dst, kidx);
            Ok((
                Place {
                    base: dst,
                    shape: PlaceShape::Word(SlotType::Ref),
                },
                SlotType::Ref,
            ))
        };
        match pat {
            Pattern::Literal { lit, span } => {
                let place = self.literal(lit, *span)?;
                let PlaceShape::Word(slot) = place.shape else {
                    return Err(mismatch("a literal pattern's word"));
                };
                Ok((place, slot))
            },
            Pattern::RawStr { text, .. } => intern(self, text),
            Pattern::Str { parts, span, .. } => {
                let mut text = String::new();
                for part in parts {
                    match part {
                        StrPart::Text { text: run, .. } => text.push_str(run),
                        _ => {
                            return Err(LowerError::unsupported(
                                *span,
                                "a string pattern with interpolation holes",
                                "a pattern tests one constant string; holes have no pattern \
                                 lowering",
                            ))
                        },
                    }
                }
                intern(self, &text)
            },
            _ => Err(mismatch("a literal pattern")),
        }
    }

    /// The member atoms a pattern tests; empty for a catchall arm. A
    /// bare identifier names a member test when the type namespace
    /// carries it [F9] — the same resolution the checker applies, so the
    /// two layers never disagree — and an or-pattern contributes every
    /// alternative's atom.
    fn pattern_atoms(&mut self, pat: &Pattern) -> Result<Vec<TypeId>, LowerError> {
        match pat {
            Pattern::Wildcard(_) => Ok(Vec::new()),
            Pattern::Binding { name, .. } => {
                Ok(self.binding_atom(&name.name).into_iter().collect())
            },
            Pattern::Or { pats, .. } => {
                let mut atoms = Vec::with_capacity(pats.len());
                for sub in pats {
                    let sub_atoms = self.pattern_atoms(sub)?;
                    let [atom] = sub_atoms.as_slice() else {
                        return Err(LowerError::unsupported(
                            pattern_span(sub),
                            "an or-pattern alternative that is not one member test",
                            "or-arms test tags this round; binding alternatives need their own \
                             seam",
                        ));
                    };
                    atoms.push(*atom);
                }
                Ok(atoms)
            },
            Pattern::Path { path, .. } => {
                let [only] = path.segments.as_slice() else {
                    return Err(LowerError::unsupported(
                        pattern_span(pat),
                        "a multi-segment pattern path",
                        "qualified patterns are not in the M3 contract",
                    ));
                };
                self.path_atom(&only.name)
                    .map(|atom| vec![atom])
                    .ok_or_else(|| mismatch("a pattern's member atom"))
            },
            Pattern::Struct { path, .. } => {
                let [only] = path.segments.as_slice() else {
                    return Err(LowerError::unsupported(
                        pattern_span(pat),
                        "a multi-segment pattern path",
                        "qualified patterns are not in the M3 contract",
                    ));
                };
                self.path_atom(&only.name)
                    .map(|atom| vec![atom])
                    .ok_or_else(|| mismatch("a pattern's member atom"))
            },
            Pattern::Tuple { .. } => Err(LowerError::unsupported(
                pattern_span(pat),
                "a tuple pattern in a member-test position",
                "the dispatch resolves a member arm from the pattern's own shape, and a tuple \
                 pattern names no member; bind the member and destructure it in the arm body",
            )),
            // a case constructor pattern names its case's atom [B3]: the
            // dispatch jumps in on the case's tag and the section tests
            // the pattern's payload sub-patterns over the case's claimed
            // region offsets
            Pattern::Call { path, .. } => {
                let [only] = path.segments.as_slice() else {
                    return Err(LowerError::unsupported(
                        pattern_span(pat),
                        "a multi-segment pattern path",
                        "qualified patterns are not in the M3 contract",
                    ));
                };
                self.path_atom(&only.name)
                    .map(|atom| vec![atom])
                    .ok_or_else(|| mismatch("a pattern's member atom"))
            },
            Pattern::Literal { .. } | Pattern::Str { .. } | Pattern::RawStr { .. } => {
                Err(LowerError::unsupported(
                    pattern_span(pat),
                    "a literal pattern in a member-test position",
                    "a literal tests no member tag; test it inside a constructor pattern's \
                     payload or bind the member and compare in the arm body",
                ))
            },
        }
    }

    /// The member atom a bare identifier names in a pattern, mirroring
    /// the checker: the predeclared `none`, or a constructor type's
    /// atom; `None` means the identifier is a binding (a catchall).
    fn binding_atom(&self, name: &str) -> Option<TypeId> {
        if name == "none" && !self.analyzed.package.declares("none") {
            return Some(TypeId::NONE);
        }
        let def = self.analyzed.package.lookup_type(name)?;
        self.analyzed.decls.atoms.get(&def).copied()
    }

    /// Binds one arm's pattern binders to scrutinee words.
    /// Binds a nested struct sub-pattern's fields at `base`, the offset
    /// its struct-typed field claims: the same field vocabulary as an
    /// arm's own struct pattern — punning, plain names, or-ladders
    /// through their last alternative — binding borrowed aliases at the
    /// nested span's own consecutive words; a struct-typed inner
    /// field's own nested sub-pattern recurses at the shifted base, and
    /// tested fields (the section's or the chain's) bind nothing.
    /// Binds a nested struct sub-pattern's fields at `base`, the offset
    /// its struct-typed field claims: the same field vocabulary as an
    /// arm's own struct pattern — punning, plain names, or-ladders
    /// through their last alternative — binding borrowed aliases at the
    /// nested span's own consecutive words; a struct-typed inner
    /// field's own nested sub-pattern recurses at the shifted base, and
    /// tested fields (the section's or the chain's) bind nothing.
    fn bind_struct_sub_pattern(&mut self, pat: &Pattern, base: u8) -> Result<(), LowerError> {
        let struct_shape = self.struct_sub_shape(pat)?;
        let fields = &struct_shape.fields;
        let Pattern::Struct { fields: pats, .. } = pat else {
            return Err(mismatch("a nested struct sub-pattern"));
        };
        for (name, sub) in pats {
            let Some(field) = fields
                .iter()
                .find(|f| f.name.as_deref() == Some(name.name.as_str()))
            else {
                return Err(mismatch("a pattern field"));
            };
            if let Some(inner @ Pattern::Struct { .. }) = sub {
                self.bind_struct_sub_pattern(inner, base + field.word)?;
                continue;
            }
            let bind = match sub {
                None => Some((name.span, name.name.clone())),
                Some(Pattern::Binding { span, name }) => Some((*span, name.name.clone())),
                Some(Pattern::Wildcard(_)) => None,
                Some(sub) => match sub {
                    // the section or the chain tested the literals and
                    // ladders at the field's word; they bind nothing
                    Pattern::Literal { .. } | Pattern::Str { .. } | Pattern::RawStr { .. } => None,
                    Pattern::Or { pats, .. } => match pats.last().map(last_alt) {
                        None
                        | Some(
                            Pattern::Wildcard(_)
                            | Pattern::Literal { .. }
                            | Pattern::Str { .. }
                            | Pattern::RawStr { .. },
                        ) => None,
                        Some(Pattern::Binding { span, name }) => Some((*span, name.name.clone())),
                        _ => {
                            return Err(LowerError::unsupported(
                                pattern_span(sub),
                                "a nested or-ladder field",
                                "a field binds as a plain name or skips as `_`, and tests as a \
                                 literal or an or-ladder of literals",
                            ))
                        },
                    },
                    other => {
                        return Err(LowerError::unsupported(
                            pattern_span(other),
                            "a nested struct field",
                            "fields bind as plain names or skip as `_`, and test as literals at \
                             match time; tuple and member sub-patterns wait on the field's own \
                             span; bind the field and destructure it in the arm body",
                        ))
                    },
                },
            };
            let Some(bind) = bind else { continue };
            let at = base + field.word;
            // a struct-typed inner field binds the span its checked
            // type names, however many words it spans; every other
            // field binds its one word
            let fact = self.analyzed.checked.binders.get(&bind.0).copied();
            let shape = match fact {
                Some(fact) if self.struct_typed(fact.ty) => PlaceShape::Span(fact.ty),
                _ => PlaceShape::Word(field.slot()),
            };
            self.binders.insert(bind.0, Place { base: at, shape });
            if let Some(fact) = self.analyzed.checked.binders.get(&bind.0).copied() {
                self.rc_bind_borrowed(bind.0, at, fact.ty)?;
            }
            self.scope_insert(&bind.1, bind.0);
        }
        Ok(())
    }

    fn bind_arm_pattern(&mut self, pat: &Pattern, scrut: &TypedPlace) -> Result<(), LowerError> {
        match pat {
            Pattern::Binding { span, name } => {
                if self.binding_atom(&name.name).is_some() {
                    // a bare tag arm tests membership [F9]; nothing binds
                    return Ok(());
                }
                self.binders.insert(*span, scrut.place.clone());
                self.rc_bind(*span, &scrut.place, scrut.ty)?;
                self.scope_insert(&name.name, *span);
                Ok(())
            },
            Pattern::Struct { path, fields, .. } => {
                let [only] = path.segments.as_slice() else {
                    return Err(mismatch("a pattern path"));
                };
                let Some(atom) = self.path_atom(&only.name) else {
                    return Err(mismatch("a pattern's member atom"));
                };
                let PlaceShape::Span(underlying) = scrut.place.shape else {
                    return Err(mismatch("a struct pattern on a scalar"));
                };
                // a union scrutinee places the member's fields at their
                // claimed region offsets and the dispatch slide emits the
                // field tests at the section's top; a bare struct
                // scrutinee places its own consecutive words and the
                // value chain emits them — both test positions skip what
                // the chain already tested
                let layout = self.shapes.shape(underlying)?;
                let (member_fields, value_chain): (&[MemberField], bool) = match &*layout {
                    Shape::Union(_) => (
                        layout
                            .member_fields(atom)
                            .ok_or_else(|| mismatch("a union member read"))?,
                        false,
                    ),
                    Shape::Struct(s) => (s.fields.as_slice(), true),
                    _ => return Err(mismatch("a struct pattern's scrutinee")),
                };
                for (name, sub) in fields {
                    let field = member_fields
                        .iter()
                        .find(|f| f.name.as_deref() == Some(name.name.as_str()))
                        .ok_or_else(|| mismatch("a pattern field"))?;
                    // a nested struct sub-pattern over a struct-typed
                    // field binds its fields at the field's own offset,
                    // the same vocabulary one level deeper (both chains
                    // share it)
                    if let Some(inner @ Pattern::Struct { .. }) = sub {
                        self.bind_struct_sub_pattern(inner, scrut.place.base + field.word)?;
                        continue;
                    }
                    // the binder the field names: punning binds the field's
                    // own name, a plain-name sub its binder; on the value
                    // chain a tested field (a literal, or an or-ladder
                    // binding through its last alternative — the
                    // alternatives agree on the same words) binds nothing,
                    // and `_` skips outright
                    let bind = match sub {
                        None => Some((name.span, name.name.clone())),
                        Some(Pattern::Binding { span, name }) => Some((*span, name.name.clone())),
                        Some(Pattern::Wildcard(_)) => None,
                        Some(sub) if value_chain => match sub {
                            Pattern::Literal { .. }
                            | Pattern::Str { .. }
                            | Pattern::RawStr { .. } => None,
                            Pattern::Or { pats, .. } => match pats.last().map(last_alt) {
                                // the last alternative binds nothing when
                                // it tests or skips; the alternatives
                                // agreed on the same binder words
                                None
                                | Some(
                                    Pattern::Wildcard(_)
                                    | Pattern::Literal { .. }
                                    | Pattern::Str { .. }
                                    | Pattern::RawStr { .. },
                                ) => None,
                                Some(Pattern::Binding { span, name }) => {
                                    Some((*span, name.name.clone()))
                                },
                                _ => {
                                    return Err(LowerError::unsupported(
                                        pattern_span(sub),
                                        "a nested pattern field",
                                        "fields bind as plain names or skip as `_`, and test as \
                                         literals at match time; nested patterns wait on the \
                                         field's whole-span staging",
                                    ))
                                },
                            },
                            _ => {
                                return Err(LowerError::unsupported(
                                    pattern_span(sub),
                                    "a nested pattern field",
                                    "fields bind as plain names or skip as `_`, and test as \
                                     literals at match time; nested patterns wait on the field's \
                                     whole-span staging",
                                ))
                            },
                        },
                        // the union side takes the same resolution: the
                        // dispatch slide tested literals and or-ladders in
                        // the section, so they bind nothing here
                        Some(sub) => match sub {
                            Pattern::Literal { .. }
                            | Pattern::Str { .. }
                            | Pattern::RawStr { .. } => None,
                            Pattern::Or { pats, .. } => match pats.last().map(last_alt) {
                                None
                                | Some(
                                    Pattern::Wildcard(_)
                                    | Pattern::Literal { .. }
                                    | Pattern::Str { .. }
                                    | Pattern::RawStr { .. },
                                ) => None,
                                Some(Pattern::Binding { span, name }) => {
                                    Some((*span, name.name.clone()))
                                },
                                _ => {
                                    return Err(LowerError::unsupported(
                                        pattern_span(sub),
                                        "a nested pattern field",
                                        "nested patterns wait on the field's whole span; bind the \
                                         field and destructure it in the arm body",
                                    ))
                                },
                            },
                            _ => {
                                return Err(LowerError::unsupported(
                                    pattern_span(sub),
                                    "a nested pattern field",
                                    "nested patterns wait on the field's whole span; bind the \
                                     field and destructure it in the arm body",
                                ))
                            },
                        },
                    };
                    let Some(bind) = bind else {
                        continue;
                    };
                    let base = scrut.place.base + field.word;
                    // a struct-typed field binds the span its checked
                    // type names, however many words it spans; every
                    // other field binds its one word
                    let fact = self.analyzed.checked.binders.get(&bind.0).copied();
                    let shape = match fact {
                        Some(fact) if self.struct_typed(fact.ty) => PlaceShape::Span(fact.ty),
                        _ => PlaceShape::Word(field.slot()),
                    };
                    self.binders.insert(bind.0, Place { base, shape });
                    if let Some(fact) = self.analyzed.checked.binders.get(&bind.0).copied() {
                        self.rc_bind_borrowed(bind.0, base, fact.ty)?;
                    }
                    self.scope_insert(&bind.1, bind.0);
                }
                Ok(())
            },
            Pattern::Call { path, args, .. } => {
                let [only] = path.segments.as_slice() else {
                    return Err(mismatch("a pattern path"));
                };
                let Some(atom) = self.path_atom(&only.name) else {
                    return Err(mismatch("a pattern's member atom"));
                };
                let PlaceShape::Span(underlying) = scrut.place.shape else {
                    return Err(mismatch("a struct pattern on a scalar"));
                };
                if !matches!(&*self.shapes.shape(underlying)?, Shape::Union(_)) {
                    return Err(mismatch("a case pattern's scrutinee"));
                }
                let layout = self.shapes.shape(scrut.ty)?;
                let member_fields = layout
                    .member_fields(atom)
                    .ok_or_else(|| mismatch("a union member read"))?;
                if member_fields.len() != args.len() {
                    return Err(mismatch("a case pattern's arity"));
                }
                for (arg, field) in args.iter().zip(member_fields.iter()) {
                    match arg {
                        // a struct-typed payload's nested struct
                        // sub-pattern binds its fields at the payload's
                        // own offsets, the same vocabulary one level
                        // deeper
                        Pattern::Struct { .. } => {
                            self.bind_struct_sub_pattern(arg, scrut.place.base + field.word)?;
                        },
                        // the section tested literals and or-ladders at
                        // the claimed offsets already, and `_` skips;
                        // neither binds anything here
                        Pattern::Wildcard(_)
                        | Pattern::Literal { .. }
                        | Pattern::Str { .. }
                        | Pattern::RawStr { .. } => {},
                        Pattern::Or { pats, .. } => {
                            // binding alternatives bind through the last;
                            // literal ladders tested in the section
                            if let Some(Pattern::Binding { span, name }) = pats.last().map(last_alt)
                            {
                                let base = scrut.place.base + field.word;
                                let fact = self.analyzed.checked.binders.get(span).copied();
                                let shape = match fact {
                                    Some(fact) if self.struct_typed(fact.ty) => {
                                        PlaceShape::Span(fact.ty)
                                    },
                                    _ => PlaceShape::Word(field.slot()),
                                };
                                self.binders.insert(*span, Place { base, shape });
                                if let Some(fact) = self.analyzed.checked.binders.get(span).copied()
                                {
                                    self.rc_bind_borrowed(*span, base, fact.ty)?;
                                }
                                self.scope_insert(&name.name, *span);
                            }
                        },
                        Pattern::Binding { span, name } => {
                            // an atom-named payload binding resolves
                            // through the type namespace [F9], so it is a
                            // member test binding nothing — and the
                            // payload has no union of its own to test
                            // (union-typed payload fields refuse at their
                            // shape)
                            if self.binding_atom(&name.name).is_some() {
                                return Err(LowerError::unsupported(
                                    *span,
                                    "an atom-named case payload sub-pattern",
                                    "the name resolves through the type namespace as a type atom, \
                                     so it binds nothing; a payload binder needs a plain name",
                                ));
                            }
                            let base = scrut.place.base + field.word;
                            // a struct-typed payload binds the span its
                            // checked type names, however many words it
                            // spans; every other field binds its one word
                            let fact = self.analyzed.checked.binders.get(span).copied();
                            let shape = match fact {
                                Some(fact) if self.struct_typed(fact.ty) => {
                                    PlaceShape::Span(fact.ty)
                                },
                                _ => PlaceShape::Word(field.slot()),
                            };
                            self.binders.insert(*span, Place { base, shape });
                            if let Some(fact) = self.analyzed.checked.binders.get(span).copied() {
                                self.rc_bind_borrowed(*span, base, fact.ty)?;
                            }
                            self.scope_insert(&name.name, *span);
                        },
                        sub => {
                            return Err(LowerError::unsupported(
                                pattern_span(sub),
                                "a nested case payload sub-pattern",
                                "nested patterns wait on the field's whole span; bind the payload \
                                 and destructure it in the arm body",
                            ))
                        },
                    }
                }
                Ok(())
            },
            Pattern::Tuple { pats, .. } => {
                self.bind_tuple_pattern(pats, scrut, true)?;
                Ok(())
            },
            Pattern::Or { pats, .. } => {
                // the alternatives bind the same names on the same words
                // (the classification's agreement check), and the body
                // reads the last alternative's binders — the scope's
                // name map keeps the last binding
                if let Some(last) = pats.last() {
                    self.bind_arm_pattern(last_alt(last), scrut)?;
                }
                Ok(())
            },
            _ => Ok(()),
        }
    }

    /// Binds one tuple pattern's elements to the tuple span's words:
    /// one word per element, bound in place (aliases, the const
    /// convention). A test position skips what the dispatch already
    /// tested: literal elements bind nothing, an atom-named element is
    /// a member test binding nothing [F9], and an or-ladder element
    /// binds its last alternative's binders (the alternatives bind the
    /// same names on the same word) — the same resolution the checker's
    /// pattern walk applies, so the two layers never disagree.
    /// Declaration positions bind every name and refuse literals: a
    /// declaration cannot fail a test.
    fn bind_tuple_pattern(
        &mut self,
        pats: &[Pattern],
        scrut: &TypedPlace,
        arm: bool,
    ) -> Result<(), LowerError> {
        let shape = self.shapes.shape(scrut.ty)?;
        let Shape::Tuple(s) = &*shape else {
            return Err(mismatch("a tuple destructuring"));
        };
        if pats.len() != s.fields.len() {
            return Err(mismatch("a tuple pattern's arity"));
        }
        // the elements bind at their whole-word addresses — consecutive
        // words for a bare tuple, the member's claimed offsets for a
        // tuple member read out of a union region
        let addrs = self.word_addrs(&scrut.place, scrut.ty)?;
        for (index, raw) in pats.iter().enumerate() {
            let addr = addrs
                .get(index)
                .ok_or_else(|| mismatch("a tuple pattern's arity"))?;
            let base = addr.slot;
            let sub = match raw {
                Pattern::Or { pats, .. } if arm => pats.last().map_or(raw, last_alt),
                _ => raw,
            };
            match sub {
                Pattern::Wildcard(_) => {},
                Pattern::Binding { span, name } => {
                    if arm && self.binding_atom(&name.name).is_some() {
                        // an atom-named element is a member test [F9];
                        // nothing binds
                        continue;
                    }
                    self.binders.insert(*span, Place {
                        base,
                        shape: PlaceShape::Word(addr.slot_ty),
                    });
                    if let Some(fact) = self.analyzed.checked.binders.get(span).copied() {
                        self.rc_bind_borrowed(*span, base, fact.ty)?;
                    }
                    self.scope_insert(&name.name, *span);
                },
                Pattern::Literal { .. } | Pattern::Str { .. } | Pattern::RawStr { .. } if arm => {},
                _ => {
                    return Err(LowerError::unsupported(
                        pattern_span(sub),
                        "a destructuring element pattern",
                        "elements bind as plain names, skip as `_`, or test as literals at match \
                         time; a declaration cannot fail a test, so it never tests",
                    ))
                },
            }
        }
        Ok(())
    }

    // ---- calls ------------------------------------------------------------------

    /// The element guard both list construction paths share: a pushed
    /// element stages its whole inline word span — one scalar word (int,
    /// float, bool, tag, a heap Ref, a callable's scalar FuncRef) or a
    /// struct's flattened field words at the per-list element stride the
    /// first push fixes (the native channel's staged-count and read-out
    /// span seam). Tuple elements ride the same flat span, one word per
    /// element, gathered positionally. Union elements refuse (the
    /// checked read extracts one atom), and a
    /// deinit-carrying element refuses (the list's edge retention owns
    /// counts, never claims; per-span deinit claims own the seam).
    /// A callable element must prove scalar provenance: a closure word (or
    /// a callable-defaulted parameter that may hold one at runtime) would
    /// leave its count invisible to the element staging and read back as a
    /// Ref word behind a Fn-classified payload slot.
    fn guard_list_element(
        &mut self,
        val: &TypedPlace,
        span: Span,
        what: &'static str,
    ) -> Result<(), LowerError> {
        match &*self.shapes.shape(val.ty)? {
            Shape::Struct(_) => {
                if self.deinit_hook(val.ty, span)?.is_some() {
                    return Err(LowerError::unsupported(
                        span,
                        what,
                        "a deinit-carrying element's hook has no claim inside the list's edge \
                         retention; per-span deinit claims own the seam",
                    ));
                }
                Ok(())
            },
            Shape::Tuple(s) => {
                // the tuple's one-word elements ride the channel like a
                // struct's flattened span, gathered positionally; a
                // callable element word keeps the scalar-provenance rule
                // (a closure word would read back as a Ref behind a
                // Fn-classified payload slot)
                let addrs = self.word_addrs(&val.place, val.ty)?;
                for (field, addr) in s.fields.iter().zip(addrs.iter()) {
                    if field.slot() == SlotType::Fn && !self.scalar_fn_words.contains(&addr.slot) {
                        return Err(LowerError::unsupported(
                            span,
                            what,
                            "a callable element must prove a non-capturing scalar word; a closure \
                             or an unproven callable parameter cannot inhabit a collection element",
                        ));
                    }
                }
                Ok(())
            },
            Shape::Union(_) => Err(LowerError::unsupported(
                span,
                what,
                "a union element stages its whole region, and the checked read extracts one atom; \
                 narrow the element before the list",
            )),
            Shape::Word(_) => {
                if matches!(&*self.shapes.shape(val.ty)?, Shape::Word(SlotType::Fn)) {
                    let addrs = self.word_addrs(&val.place, val.ty)?;
                    let proven = addrs.len() == 1 && self.scalar_fn_words.contains(&addrs[0].slot);
                    if !proven {
                        return Err(LowerError::unsupported(
                            span,
                            what,
                            "a callable element must prove a non-capturing scalar word; a closure \
                             or an unproven callable parameter cannot inhabit a collection element",
                        ));
                    }
                }
                Ok(())
            },
        }
    }

    /// The staged-span shape guards a call refuses before dispatch.
    /// `map_set`'s key and value lower as one machine word each: a wider
    /// argument (a union span) would shift the staged words under the
    /// native's positional contract. A local callee's slide contract is
    /// its parameter span: the widening family re-spans a member or
    /// sub-union argument into the parameter's whole region before the
    /// staging moves, so a remaining width disagreement is a lowerer
    /// seam the VM's slide validation would trap on.
    fn guard_staged_width(
        &mut self,
        target: Callee,
        staged: &[SlotType],
        span: Span,
    ) -> Result<(), LowerError> {
        if let Callee::Builtin(native) = target {
            if native.name == "map_set" && staged.len() != 3 {
                return Err(LowerError::unsupported(
                    span,
                    "a map_set argument",
                    "map keys and values lower as one machine word; wider values arrive when the \
                     native channel carries whole spans",
                ));
            }
        }
        if let Callee::Local(index) = target {
            if let Some(expected) = self.callee_param_words(index) {
                if staged.len() != expected {
                    return Err(LowerError::unsupported(
                        span,
                        "a call argument",
                        "the staged words disagree with the callee's parameter span; a \
                         union-typed parameter re-spans its member argument into the whole region \
                         at the call, so a disagreement here is a lowerer seam",
                    ));
                }
            }
        }
        Ok(())
    }

    /// Guards a staged word count against the one-byte staging width:
    /// the overlay area and every call's nargs (a closure's capture
    /// count) address words by u8, so a staged span wider than 255
    /// words is a typed refusal at the point the span is built — the
    /// finalize-time frame check would come only after the staging
    /// moves lowered, and their conversions panic first.
    fn guard_staged_count(&self, count: usize) -> Result<(), LowerError> {
        if count > usize::from(u8::MAX) {
            return Err(LowerError::FrameTooWide {
                name: self.name.clone(),
                need: self.slots.len() + count,
            });
        }
        Ok(())
    }

    /// One call argument's single scalar word kind, when its checked
    /// type is one I64/F64 word and its parameter neither re-spans the
    /// value (a union parameter takes the member's whole region) nor
    /// erases the staging kind (a type variable stages Raw).
    fn scalar_arg_slot(
        &mut self,
        arg: &Expr,
        param_ty: Option<Ty>,
    ) -> Result<Option<SlotType>, LowerError> {
        let ty = self.val(arg.span())?.ty;
        if let Some(param) = param_ty
            && (param != ty && self.is_union_ty(param)? || self.is_var_ty(param))
        {
            return Ok(None);
        }
        Ok(match &*self.shapes.shape(ty)? {
            Shape::Word(slot @ (SlotType::I64 | SlotType::F64)) => Some(*slot),
            _ => None,
        })
    }

    /// A literal scalar argument's word kind and constant, when it may
    /// stage directly: the load emits at the staging position with
    /// every argument already evaluated, so a later argument's nested
    /// call cannot clobber the word, and the temp the ordinary
    /// spelling allocates is single-use by construction.
    fn direct_literal(
        &mut self,
        arg: &Expr,
        param_ty: Option<Ty>,
    ) -> Result<Option<(SlotType, ScalarConst)>, LowerError> {
        let Expr::Literal { lit, .. } = peel_parens(arg) else {
            return Ok(None);
        };
        let Some(slot) = self.scalar_arg_slot(arg, param_ty)? else {
            return Ok(None);
        };
        Ok(scalar_const(lit, slot).map(|constant| (slot, constant)))
    }

    /// A computed scalar argument's word kind, when its outermost data
    /// op may write its overlay word directly: the argument itself must
    /// be the arithmetic binary that takes the offer, and every later
    /// argument must evaluate without touching the overlay — a later
    /// argument's nested call would stage over the word first. The call
    /// site additionally keeps tail calls on the evaluated temp (a tail
    /// transfer stages its arguments past the exit claims, which stage
    /// through the overlay over them).
    fn direct_compute(
        &mut self,
        arg: &Expr,
        param_ty: Option<Ty>,
        later: &[&Expr],
    ) -> Result<Option<SlotType>, LowerError> {
        if !arith_shape(arg) || !later.iter().all(|expr| trivially_staged(expr)) {
            return Ok(None);
        }
        self.scalar_arg_slot(arg, param_ty)
    }

    /// One list literal `[a, b, c]`: the empty `list_new` call, then one
    /// `list_push` per element — the list word staged first, the
    /// element's words after it (an element expression may itself
    /// call). The list is a freshly created Ref its frame uniquely
    /// owns, so the in-place appends carry no window discipline [B3].
    /// The staged element borrows or transfers by its own death: `list_push`
    /// acquires its own count when storing, so a dying element's post-call
    /// drop hands the count to the list (the push window's flush); a live
    /// element
    /// keeps its frame count and the list holds the acquired one.
    fn list_literal(&mut self, e: &Expr) -> Result<Place, LowerError> {
        let Expr::List { items, .. } = e else {
            return Err(mismatch("a list literal"));
        };
        let list = self.list_new_word()?;
        for item in items {
            // the push's borrow window opens before the element
            // evaluation (an element expression may itself call) and
            // pins the list word plus every word the push stages
            self.rc.native_open();
            self.rc.native_reads([list]);
            let item_val = self.value(item)?;
            self.list_push_word(list, &item_val, item.span())?;
        }
        Ok(Place {
            base: list,
            shape: PlaceShape::Word(SlotType::Ref),
        })
    }

    /// One element's push onto a list under construction: the guarded
    /// element staged through the `list_push` native's overlay. The
    /// literal construction and the embedded comptime answer share the
    /// spelling; the caller owns the borrow window and the element's own
    /// lowering.
    fn list_push_word(
        &mut self,
        list: u8,
        item_val: &TypedPlace,
        span: Span,
    ) -> Result<(), LowerError> {
        self.guard_list_element(item_val, span, "a list literal element")?;
        let push = crate::native::lookup("list_push")
            .expect("the list surface is covered by the native table");
        let mut staged = vec![SlotType::Ref];
        let mut moves = vec![(0usize, list)];
        for addr in self.word_addrs(&item_val.place, item_val.ty)? {
            moves.push((staged.len(), addr.slot));
            staged.push(addr.slot_ty);
        }
        self.guard_staged_count(staged.len())?;
        for (index, slot) in moves {
            self.emit(Op::Mov {
                dst: Slot::overlay(u8::try_from(index).expect("overlay word fits u8")),
                src: Slot::body(slot),
            });
            // index 0 is the construction target itself: the list is
            // the frame's own unique word, so its staging is a borrow
            // that never drops mid-construction — the value's real
            // death (a binder's scope end or the frame exit) owns the
            // count
            if index > 0 && staged[index] == SlotType::Ref {
                self.rc.staged_native(slot, false);
            }
        }
        self.overlay_site(&staged, &[SlotType::I64]); // the unit word
        let kidx = self.pool(self.funcref_const(Callee::Builtin(push)));
        self.emit(Op::Call {
            callee: kidx,
            nargs: u8::try_from(staged.len()).expect("argument count fits u8"),
        });
        self.rc.native_done(&mut self.code);
        Ok(())
    }

    /// The empty-list word: the zero-argument `list_new` call whose one
    /// Ref result lands on the overlay (the empty `list_new` call still
    /// needs its one-word overlay area — the result must land
    /// somewhere), copied into a fresh body word the frame owns.
    fn list_new_word(&mut self) -> Result<u8, LowerError> {
        let native = crate::native::lookup("list_new")
            .expect("the list surface is covered by the native table");
        self.overlay_site(&[], &[SlotType::Ref]);
        self.rc.native_open();
        let kidx = self.pool(self.funcref_const(Callee::Builtin(native)));
        self.emit(Op::Call {
            callee: kidx,
            nargs: 0,
        });
        self.rc.native_done(&mut self.code);
        let list = self.temp_word(SlotType::Ref)?;
        self.emit(Op::Mov {
            dst: Slot::body(list),
            src: Slot::overlay(0),
        });
        self.rc.produced(list);
        Ok(list)
    }

    /// One generic-application constructor call `list[t]()` [F9, B3]:
    /// the head resolved in the type namespace and the checker typed
    /// the result `list<arg>`; the runtime half is `list_new` — the
    /// element type argument never reaches the code, every list being
    /// the one Ref word.
    fn ctor_call(
        &mut self,
        recv: &Expr,
        args: &[CallArg],
        span: Span,
    ) -> Result<Place, LowerError> {
        let Some(head) = single_path_ident(recv) else {
            return Err(mismatch("a type application"));
        };
        let Some(def) = self.analyzed.package.lookup_type(&head.name) else {
            return Err(LowerError::unsupported(
                span,
                "an indexed callee",
                "the head names no type; indexing is outside the lowered surface",
            ));
        };
        let is_list = self
            .analyzed
            .decls
            .structs
            .get(&def)
            .is_some_and(|shape| shape.params.len() == 1 && shape.fields.is_empty());
        if !is_list {
            return Err(LowerError::unsupported(
                span,
                "a type-application constructor",
                "only the list type constructs by type application [B3]",
            ));
        }
        if !args.is_empty() {
            return Err(mismatch("the empty-list constructor's arguments"));
        }
        let list = self.list_new_word()?;
        Ok(Place {
            base: list,
            shape: PlaceShape::Word(SlotType::Ref),
        })
    }

    /// One map constructor call `map[k, v]()` [F9, B3]: the checker typed
    /// the result `map<key, value>`; the runtime half is `map_new` — the
    /// type arguments never reach the code, every map being the one Ref
    /// word. The empty-map word's overlay discipline is `list_new`'s: the
    /// one-word overlay area exists because the result must land
    /// somewhere.
    fn map_ctor_call(&mut self, args: &[CallArg]) -> Result<Place, LowerError> {
        if !args.is_empty() {
            return Err(mismatch("the empty-map constructor's arguments"));
        }
        let native = crate::native::lookup("map_new")
            .expect("the map surface is covered by the native table");
        self.overlay_site(&[], &[SlotType::Ref]);
        self.rc.native_open();
        let kidx = self.pool(self.funcref_const(Callee::Builtin(native)));
        self.emit(Op::Call {
            callee: kidx,
            nargs: 0,
        });
        self.rc.native_done(&mut self.code);
        let map = self.temp_word(SlotType::Ref)?;
        self.emit(Op::Mov {
            dst: Slot::body(map),
            src: Slot::overlay(0),
        });
        self.rc.produced(map);
        Ok(Place {
            base: map,
            shape: PlaceShape::Word(SlotType::Ref),
        })
    }

    fn call_value(&mut self, e: &Expr, ty: Ty) -> Result<Place, LowerError> {
        self.call_impl(e, ty, false)
    }

    /// One embedded comptime answer: the evaluator's recorded constant
    /// lowered exactly like the literal a user would have written at the
    /// call's own span (whose checked type is the call's result type, so
    /// the literal arm reads a sound fact).
    fn embedded_value(
        &mut self,
        constant: &crate::ComptimeConst,
        _ty: Ty,
        span: Span,
    ) -> Result<Place, LowerError> {
        if let crate::ComptimeConst::List(items) = constant {
            return self.embedded_list(items, span);
        }
        let synth = match constant {
            crate::ComptimeConst::Str(text) => Expr::Str {
                span,
                parts: vec![StrPart::Text {
                    span,
                    text: text.clone(),
                }],
            },
            crate::ComptimeConst::Int(value) => Expr::Literal {
                span,
                lit: Literal::Int {
                    suffix: String::new(),
                    text: value.to_string(),
                },
            },
            crate::ComptimeConst::Float(value) => Expr::Literal {
                span,
                lit: Literal::Float {
                    suffix: String::new(),
                    text: value.to_string(),
                },
            },
            crate::ComptimeConst::Bool(value) => Expr::Literal {
                span,
                lit: Literal::Bool(*value),
            },
            crate::ComptimeConst::List(_) => unreachable!("handled above"),
        };
        let typed = self.value(&synth)?;
        Ok(typed.place)
    }

    /// A list answer: the list literal a user would have written at the
    /// call's span — one `list_push` per element, each element's literal
    /// staged from the element type the call's own checked fact names
    /// (the evaluator refused every answer whose call type is not the
    /// list application before recording it).
    fn embedded_list(
        &mut self,
        items: &[crate::ComptimeConst],
        span: Span,
    ) -> Result<Place, LowerError> {
        let elem_ty = self.embedded_list_element_ty(span)?;
        let list = self.list_new_word()?;
        for item in items {
            // the push's borrow window opens before the element staging,
            // the literal construction's own discipline
            self.rc.native_open();
            self.rc.native_reads([list]);
            let item_val = self.embedded_scalar(item, elem_ty, span)?;
            self.list_push_word(list, &item_val, span)?;
        }
        Ok(Place {
            base: list,
            shape: PlaceShape::Word(SlotType::Ref),
        })
    }

    /// The element type of one embedded list answer's call site: the one
    /// application argument of the singleton apply the checker recorded
    /// at the span (the evaluator's embed gate and this read agree; a
    /// disagreement is the belt refusal, never a silent mis-lowering).
    fn embedded_list_element_ty(&self, span: Span) -> Result<Ty, LowerError> {
        let ty = self.val(span)?.ty;
        let elem = self
            .analyzed
            .decls
            .universe
            .members(ty)
            .first()
            .and_then(|&atom| {
                let decl = self.analyzed.decls.universe.atom_decl(atom).ok()?;
                match &decl.kind {
                    AtomKind::Apply(_, args) => args.first().copied(),
                    _ => None,
                }
            });
        elem.ok_or_else(|| {
            LowerError::unsupported(
                span,
                "an embedded list answer",
                "the call's checked type is not a list application; the evaluator's gate \
                 disagrees with the recorded answer",
            )
        })
    }

    /// One embedded element's literal place: the element type comes from
    /// the list's own construction because the synthesized node carries
    /// no checked fact of its own.
    fn embedded_scalar(
        &mut self,
        constant: &crate::ComptimeConst,
        ty: Ty,
        span: Span,
    ) -> Result<TypedPlace, LowerError> {
        let place = match constant {
            crate::ComptimeConst::Str(text) => self.string_expr(&[StrPart::Text {
                span,
                text: text.clone(),
            }])?,
            crate::ComptimeConst::Int(value) => self.literal(
                &Literal::Int {
                    suffix: String::new(),
                    text: value.to_string(),
                },
                span,
            )?,
            crate::ComptimeConst::Float(value) => self.literal(
                &Literal::Float {
                    suffix: String::new(),
                    text: value.to_string(),
                },
                span,
            )?,
            crate::ComptimeConst::Bool(value) => self.literal(&Literal::Bool(*value), span)?,
            crate::ComptimeConst::List(_) => {
                return Err(LowerError::unsupported(
                    span,
                    "an embedded list element",
                    "a nested list answer refuses at the evaluator",
                ));
            },
        };
        Ok(TypedPlace { place, ty })
    }

    /// Constructs one minted enum case by call form [B3]: the case's own
    /// struct span, fields filled positionally or by name, exactly like a
    /// struct literal with positional slots admitted.
    fn case_value(&mut self, args: &[CallArg], ty: Ty) -> Result<Place, LowerError> {
        let shape = self.shapes.shape(ty)?;
        let Shape::Struct(s) = &*shape else {
            return Err(mismatch("a case construction"));
        };
        let target = self.temp_span(ty)?;
        for (index, arg) in args.iter().enumerate() {
            let position = match &arg.name {
                Some(name) => s
                    .fields
                    .iter()
                    .position(|f| f.name.as_deref() == Some(name.name.as_str()))
                    .ok_or_else(|| mismatch("a case construction field"))?,
                None => index,
            };
            let Some(field) = s.fields.get(position) else {
                return Err(mismatch("a case construction slot"));
            };
            let v = self.value(&arg.value)?;
            let addrs = self
                .word_addrs(&v.place, v.ty)
                .map_err(|err| err.respanned(arg.value.span()))?;
            if addrs.len() != field.width() {
                // a member value into a union-typed payload field re-spans
                // through the field's own region, the literal store's rule
                let decl_expr = self.field_decl_expr(ty, position);
                self.store_union_member_field(target.base + field.word, decl_expr, &v, arg.span)?;
                continue;
            }
            self.store_field_span(target.base, field, &addrs);
        }
        Ok(target)
    }

    // One staging walk covers every callee class; extracting the tail or
    // native arms would re-thread the staged span and evidence state.
    #[allow(clippy::too_many_lines)]
    fn call_impl(&mut self, e: &Expr, ty: Ty, tail: bool) -> Result<Place, LowerError> {
        let Expr::Call {
            callee,
            args,
            trailing,
            span,
        } = e
        else {
            return Err(mismatch("a call"));
        };
        // the call's own span is the trait-dispatch table's key (the
        // checker records the solved instance there); the callee-shape
        // destructure below rebinds `span` to the callee path's own
        let call_span = *span;
        // A minted case constructor takes named arguments [B3]; named
        // arguments on functions stay outside the M3 contract.
        let case_ctor = match callee.as_ref() {
            Expr::Path { span, .. } => match self.res.get(*span) {
                Some(Res::Def(def)) => self
                    .analyzed
                    .decls
                    .cases
                    .get(def)
                    .filter(|c| !matches!(c.payload, EnumPayload::None))
                    .map(|_| *def),
                _ => None,
            },
            _ => None,
        };
        for arg in args {
            if arg.name.is_some() && case_ctor.is_none() {
                return Err(LowerError::unsupported(
                    arg.span,
                    "a named argument",
                    "named arguments are not in the M3 contract",
                ));
            }
        }
        // A generic-application constructor `list[t]()` [F9]: the
        // bracketed callee head resolved in the type namespace.
        if let Expr::Index { recv, .. } = &**callee {
            return self.ctor_call(recv, args, *span);
        }
        // The map constructor `map[k, v]()` [F9, B3]: the two-argument
        // application of the map struct.
        if let Expr::MapApply { .. } = &**callee {
            return self.map_ctor_call(args);
        }
        // the callee and its argument expressions in call order: a plain
        // name call feeds the written arguments; a UFCS dot-call `x.f(a)`
        // resolves to the free function and stages the receiver as its
        // first argument — `f(x, a)`, zero runtime cost [F3]. A
        // trait-method site carries no def: the solved instance names
        // the impl body (the dispatch side channel below), and `None`
        // marks the trait path.
        let (def, callee_span, mut arg_exprs, trait_dispatch): (
            Option<DefId>,
            Span,
            Vec<&Expr>,
            Option<TraitDispatch>,
        ) = if let Expr::Field { recv, name, .. } = &**callee {
            match self.res.get(name.span) {
                Some(Res::Handler) => {
                    // the `.err`/`.ok` handlers: tag dispatch plus an
                    // inlined handler body
                    let [handler] = args.as_slice() else {
                        return Err(mismatch("a handler argument"));
                    };
                    match name.name.as_str() {
                        "err" => return self.err_value(recv, &handler.value, ty),
                        "ok" => return self.ok_value(recv, &handler.value, ty),
                        _ => {
                            return Err(LowerError::unsupported(
                                name.span,
                                "a handler",
                                "only `.err` and `.ok` are lowerable this round",
                            ))
                        },
                    }
                },
                Some(Res::Dot(DotRes::Free { func, fed_param })) => {
                    if *fed_param != 0 {
                        return Err(LowerError::unsupported(
                            name.span,
                            "a dot-call past a capability parameter",
                            "only the first data parameter feeds the dot position [F3]",
                        ));
                    }
                    (
                        Some(*func),
                        name.span,
                        std::iter::once(&**recv)
                            .chain(args.iter().map(|arg| &arg.value))
                            .collect(),
                        None,
                    )
                },
                Some(Res::Dot(DotRes::Methods { candidates })) => {
                    let dispatch =
                        self.resolve_method_dispatch(call_span, name.span, Some(recv), candidates)?;
                    (
                        None,
                        name.span,
                        std::iter::once(&**recv)
                            .chain(args.iter().map(|arg| &arg.value))
                            .collect(),
                        Some(dispatch),
                    )
                },
                // The type-keyed member call: no receiver word stages, so
                // the written arguments alone feed the body; the dispatch
                // resolves through the checker's inherent record like any
                // solved inherent site (a plain local callee — no instance
                // word, no evidence words, no dispatch uop).
                Some(Res::Dot(DotRes::Static { candidates })) => {
                    let dispatch =
                        self.resolve_method_dispatch(call_span, name.span, None, candidates)?;
                    (
                        None,
                        name.span,
                        args.iter().map(|arg| &arg.value).collect(),
                        Some(dispatch),
                    )
                },
                // An implicit dispatch site (an interpolation hole's
                // `show`, F6): no surface name sits behind the dot, so
                // the resolutions carry nothing here — the checker's
                // span-keyed dispatch records ARE the site's resolution,
                // and the method-name candidates come from the registry
                // by the synthetic name.
                _ if self.analyzed.checked.trait_calls.contains_key(&call_span)
                    || self
                        .analyzed
                        .checked
                        .inherent_calls
                        .contains_key(&call_span) =>
                {
                    let candidates = self.analyzed.package.method_candidates(&name.name).to_vec();
                    let dispatch = self.resolve_method_dispatch(
                        call_span,
                        name.span,
                        Some(recv),
                        &candidates,
                    )?;
                    (
                        None,
                        name.span,
                        std::iter::once(&**recv)
                            .chain(args.iter().map(|arg| &arg.value))
                            .collect(),
                        Some(dispatch),
                    )
                },
                _ => {
                    return Err(LowerError::unsupported(
                        name.span,
                        "a method call",
                        "the dot name resolves to neither a free function nor a handler",
                    ))
                },
            }
        } else {
            let Expr::Path { path, span, .. } = &**callee else {
                return Err(LowerError::unsupported(
                    callee.span(),
                    "a computed callee",
                    "only name and dot callees are lowerable this round",
                ));
            };
            // `Trait::f(x)` qualification (F3): the checker records the
            // solved instance at the call's own span like the dot form,
            // and the receiver is the first written argument
            let qualified = match path.segments.as_slice() {
                [.., last] if matches!(self.res.get(last.span), Some(Res::TraitMethod(_))) => {
                    Some(last.span)
                },
                _ => None,
            };
            if let Some(what) = qualified {
                let recv = args.first().map(|arg| &arg.value);
                let dispatch = self.resolve_method_dispatch(call_span, what, recv, &[])?;
                (
                    None,
                    what,
                    args.iter().map(|arg| &arg.value).collect(),
                    Some(dispatch),
                )
            } else if let Some(Res::Def(def)) = self.res.get(*span) {
                (
                    Some(*def),
                    *span,
                    args.iter().map(|arg| &arg.value).collect(),
                    None,
                )
            } else if let Some(Res::Local { bind }) = self.res.get(*span) {
                // a clean check makes this a fn-typed binding: the
                // call dispatches through its Fn word [F2]
                let mut arg_exprs: Vec<&Expr> = args.iter().map(|arg| &arg.value).collect();
                if let Some(block) = trailing {
                    arg_exprs.push(&**block);
                }
                return self.calls_value(ty, *bind, *span, &arg_exprs, tail);
            } else {
                return Err(LowerError::unsupported(
                    *span,
                    "a callee name",
                    "it does not resolve to a package function",
                ));
            }
        };
        // The block-argument sugar [F7]: the checker fed the parser's
        // zero-parameter trailing block at the next open positional slot,
        // and with named arguments refused above that slot is exactly the
        // written arguments' end, so the block stages below like a
        // written trailing lambda argument — pure sugar, no calling-
        // convention surface of its own.
        if let Some(block) = trailing {
            arg_exprs.push(&**block);
        }
        if case_ctor.is_some() && case_ctor == def {
            return self.case_value(args, ty);
        }
        // A comptime call's answer embeds at the call site (G1): the
        // evaluator ran the callee on the fenced VM and recorded the
        // result on this call's span, so lowering emits it exactly like
        // a written literal and the call never reaches the runtime
        // module. A comptime callee with no recorded answer is the
        // evaluator's contract broken, not a shape the round owes a
        // lowering for — the site refuses naming the seam.
        if let ComptimeRun::Runtime(values) = &self.comptime {
            if let Some(constant) = values.get(call_span) {
                return self.embedded_value(constant, ty, call_span);
            }
            let is_comptime = def.is_some_and(|def| {
                self.analyzed.decls.fns.get(&def).is_some_and(|sig| {
                    sig.params
                        .iter()
                        .any(|p| matches!(p.mode, ds_parse::ParamMode::Comptime(_)))
                })
            });
            if is_comptime {
                return Err(LowerError::unsupported(
                    call_span,
                    "a comptime call",
                    "the evaluator recorded no answer for this site; run the comptime evaluator \
                     before lowering",
                ));
            }
        }
        // The E4 extraction family inlines at the call site over the
        // checked receiver's own union shape — no callee, no control
        // transfer, word kinds from the call site like every covered
        // generic-safe native.
        if let Some(def) = def {
            if let Some(kind) = self.extraction_def(def) {
                return self.extraction_value(kind, &arg_exprs, ty, callee_span);
            }
            if let Some(place) = self.ambient_str_place(def, &arg_exprs, callee_span)? {
                return Ok(place);
            }
            if self.ambient_cancel_def(def) {
                return self.cancel_call(&arg_exprs, callee_span);
            }
        }
        // a trait-method dispatch names its impl body directly: the call
        // stages and transfers like a local callee, with the solved
        // instance riding as the evidence word after the arguments. An
        // inherent dispatch is a plain local callee — the receiver's own
        // type picked the body, so no instance word exists; a generic
        // body's evidence words stage below like ordinary parameters
        let target = match &trait_dispatch {
            Some(TraitDispatch::Static { index, .. } | TraitDispatch::Inherent { index, .. }) => {
                Callee::Local(*index)
            },
            Some(TraitDispatch::Runtime { slot, .. }) => Callee::Evidence(*slot),
            None => {
                self.resolve_callee(def.expect("a named callee resolves to a def"), callee_span)?
            },
        };
        // a member call stages its omitted literal defaults at the call
        // site like the free-fn spelling [F7]: a trait dispatch stages
        // the trait's declared defaults (the declaration is the single
        // authority — call sites check those literals), while an
        // inherent dispatch stages the impl body's own
        let member_defaults: Vec<Expr>;
        let arg_exprs = match (def, &trait_dispatch) {
            (Some(def), _) => self.defaulted_args(def, target, arg_exprs),
            (_, Some(TraitDispatch::Static { call, .. })) => {
                member_defaults = self.trait_default_exprs(call.method, arg_exprs.len());
                let mut staged = arg_exprs;
                staged.extend(member_defaults.iter());
                staged
            },
            (_, Some(TraitDispatch::Runtime { method, .. })) => {
                member_defaults = self.trait_default_exprs(*method, arg_exprs.len());
                let mut staged = arg_exprs;
                staged.extend(member_defaults.iter());
                staged
            },
            (None, Some(TraitDispatch::Inherent { index, .. })) => {
                member_defaults =
                    self.member_default_exprs(*index, arg_exprs.len(), callee_span)?;
                let mut staged = arg_exprs;
                staged.extend(member_defaults.iter());
                staged
            },
            _ => arg_exprs,
        };
        // A direct `list_push` stages its (list, value) pair with the
        // value's whole word span — the same element guard the literal
        // path applies (a struct element stages its flattened words at
        // the stride the first push fixes). `cons` mirrors it with the
        // element's whole span in the receiver position (the stride-aware
        // prepend's head). `reduce`'s init needs no element guard: the
        // checker proves it the fold's own type, so any element that
        // could inhabit the list already passed this guard, and the
        // fold's interior span carries the init's whole width.
        let element_slot = match target {
            Callee::Builtin(native) if native.name == "list_push" => {
                Some((1usize, "a list_push element"))
            },
            Callee::Generic(generic) if generic.name == "cons" => Some((0usize, "a cons element")),
            _ => None,
        };
        // stage: evaluate every argument to a place, then move its words
        // into the overlay area (argument evaluation may itself call)
        let mut staged: Vec<SlotType> = Vec::new();
        // which staged words carry a count at runtime: Ref words, plus
        // the one Raw word of a type-variable value (the VM's RC ops
        // check the kind, so a scalar instantiation is a no-op)
        let mut counted: Vec<bool> = Vec::new();
        let mut moves: Vec<(usize, u8)> = Vec::new();
        // the written argument each staged word belongs to: a consuming
        // (`move`) parameter's words stage as a hand-off (C2)
        let mut word_arg: Vec<usize> = Vec::new();
        // staged indices whose Ref word lands in a callable-defaulted `Fn`
        // parameter (the closure-capable callable surface): the callee
        // reads the word and never owns its count, so the staging is a
        // borrow window, never a dup or a forward
        let mut defaulted: Vec<usize> = Vec::new();
        let expected_words = self.expected_param_words(target);
        // an interior-span generic stages one dynamic span between its
        // leading and trailing named words, so a staged closure word's
        // position is dynamic and the callable-defaulted slot is the
        // trailing named word alone
        let interior_span = matches!(target, Callee::Generic(g) if g.staging == Staging::Interior);
        // a local callee's per-parameter checked types, when its
        // signature is static: a union-typed parameter re-spans a bare
        // member or a strict sub-union argument into its whole region
        // here (the widening family)
        let param_types = match target {
            Callee::Local(index) => self.callee_param_types(index),
            _ => None,
        };
        // whether each written argument position feeds a consuming
        // (`move`) parameter: the staging then forwards the source's
        // count and any deinit claim — the checker's forced move makes
        // the death test true at exactly this use (C2/C1)
        let consuming = self.consuming_args(target, trait_dispatch.as_ref(), arg_exprs.len());
        // the receiver's checked type mints a generic call's evidence
        // words (the builtin instance it addresses) [H3]
        let mut receiver_ty: Option<Ty> = None;
        // the call's borrow window opens before operand evaluation: a
        // later operand's own use can fire an earlier word's last-use
        // dec there, and the window pins every word this CALL reads
        // (native borrows never dup), deferring the dec to the flush
        // after the CALL
        self.rc.native_open();
        let mut consts: Vec<(usize, ScalarConst)> = Vec::new();
        for (index, arg) in arg_exprs.iter().enumerate() {
            let param_ty = param_types
                .as_ref()
                .and_then(|types| types.get(index).copied());
            // A literal scalar argument stages its constant directly on
            // its overlay word. A tail transfer defers the staging past
            // the exit claims, so it keeps the evaluated temp.
            if !tail && let Some((slot, constant)) = self.direct_literal(arg, param_ty)? {
                if index == 0 {
                    receiver_ty = Some(self.val(arg.span())?.ty);
                }
                let at = staged.len();
                staged.push(slot);
                counted.push(false);
                word_arg.push(index);
                consts.push((at, constant));
                continue;
            }
            // A single-use computed scalar argument writes its overlay
            // word directly: the offer goes to the argument's outermost
            // data op, taken only when every later argument evaluates
            // without touching the overlay.
            let offered = if !tail
                && let Some(slot) = self.direct_compute(arg, param_ty, &arg_exprs[index + 1..])?
            {
                self.scalar_dst = Some(ScalarDst::Arg {
                    index: u8::try_from(staged.len()).expect("overlay word fits u8"),
                    slot,
                });
                true
            } else {
                false
            };
            let arg_val = self.value(arg)?;
            let direct = offered && self.scalar_dst.take().is_none();
            if index == 0 {
                receiver_ty = Some(arg_val.ty);
            }
            let deinit = self.deinit_claim(arg_val.ty, arg.span())?;
            if deinit.is_some() {
                if matches!(target, Callee::Builtin(_) | Callee::Generic(_)) {
                    return Err(LowerError::unsupported(
                        arg.span(),
                        "a deinit-carrying argument",
                        "a native call borrows staged words with no drop position of their own; \
                         the value's deinit runs only at its binding's death",
                    ));
                }
                if tail && matches!(target, Callee::Local(_)) && !consuming[index] {
                    return Err(LowerError::unsupported(
                        arg.span(),
                        "a deinit-carrying argument in tail position",
                        "the tail call reuses this frame, so the callee's borrow would outlive \
                         the frame that owns the value's deinit; keep the call out of tail \
                         position",
                    ));
                }
            }
            if let Some((slot, what)) = element_slot {
                if slot == index {
                    self.guard_list_element(&arg_val, arg.span(), what)?;
                }
            }
            // a closure argument lands only in a callable-defaulted `Fn`
            // parameter; its one Ref word marks the borrow-window index.
            // An interior-span generic's callable word rides at the end
            // of the staged span, so the static expected list cannot
            // name its position there
            if self.is_closure_word(&arg_val.place, arg_val.ty)? {
                if interior_span {
                    Self::guard_interior_closure_slot(
                        expected_words.as_deref(),
                        index + 1 == arg_exprs.len(),
                        arg.span(),
                    )?;
                } else {
                    Self::guard_callable_staged(
                        expected_words.as_deref(),
                        staged.len(),
                        arg.span(),
                    )?;
                }
                defaulted.push(staged.len());
            }
            // a union-typed parameter takes its member's whole region: a
            // bare member (a literal or an un-annotated binding) or a
            // strict sub-union argument re-spans into a staging temp of
            // the parameter's own region here, mirroring the annotated
            // binding's widening write, so the staged words match the
            // callee's parameter span
            let arg_val = match param_types.as_ref().and_then(|types| types.get(index)) {
                Some(&param_ty) if param_ty != arg_val.ty && self.is_union_ty(param_ty)? => {
                    // a union member cannot carry a deinit yet (the
                    // callee's own parameter shapes refuse first), so
                    // this guard is the staging temp's backstop for the
                    // deinit arc's landing
                    if deinit.is_some() {
                        return Err(LowerError::unsupported(
                            arg.span(),
                            "a deinit-carrying argument into a union parameter",
                            "the re-span's staging temp is anonymous, so it carries no claim \
                             contract of its own; the deinit arc owns the seam",
                        ));
                    }
                    let temp = self.temp_span(param_ty)?;
                    self.widen_into(&temp, param_ty, &arg_val, arg.span())?;
                    TypedPlace {
                        place: temp,
                        ty: param_ty,
                    }
                },
                _ => arg_val,
            };
            let var_word = self.is_var_ty(arg_val.ty);
            // a free generic fn's parameter over its own type variable
            // claims the argument as the variable's single Raw word, so
            // an argument spanning more than one word refuses here —
            // the one-word instantiation boundary, at the site where
            // the type is concrete
            let var_param = param_ty.is_some_and(|param| self.is_var_ty(param));
            if var_param {
                self.guard_one_word_instantiation(
                    self.members(arg_val.ty),
                    "a generic call's argument",
                    arg.span(),
                )?;
            }
            if direct {
                // the argument's outermost data op wrote its overlay
                // word: the staging records the word's kind and the
                // argument's ownership; no Mov and no window read ride
                // it (a scalar word carries no count)
                let PlaceShape::Word(slot) = arg_val.place.shape else {
                    return Err(mismatch("a directly staged scalar argument"));
                };
                staged.push(slot);
                counted.push(false);
                word_arg.push(index);
            } else {
                let addrs = self
                    .word_addrs(&arg_val.place, arg_val.ty)
                    .map_err(|err| err.respanned(arg.span()))?;
                for addr in addrs {
                    self.rc.native_reads([addr.slot]);
                    moves.push((staged.len(), addr.slot));
                    // the variable's single word stages as the wildcard: the
                    // overlay word records Raw (the runtime kind is the
                    // callee's fact), so the ABI slide admits the pairing
                    // whatever the source's static kind — a scalar staged at
                    // an overlay index no other type visits would otherwise
                    // fail the slide's static check against the Raw parameter.
                    // A Raw-classified staged word always records the count
                    // hand-off: the RC ops check the runtime kind, so a
                    // scalar instantiation is a no-op (a variable-keyed union
                    // member's region claim holds a counted reference under a
                    // reference substitution)
                    staged.push(if var_param { SlotType::Raw } else { addr.slot_ty });
                    counted.push(matches!(addr.slot_ty, SlotType::Ref | SlotType::Raw) || var_word);
                    word_arg.push(index);
                }
            }
        }
        // a closure word in a callable-defaulted parameter has no CALLT
        // path: the transfer leaves no post-call position for the borrow
        // flush, and the pre-transfer exit drops would free the object
        // under the callee's read. Its tail lowers in place instead (CALL
        // plus flush plus return, the R13 native-tail shape) —
        // stack-identical and count-exact; the frame-reuse guarantee
        // [H2] keeps every scalar-staging tail site
        let closure_tail = !defaulted.is_empty() && tail && matches!(target, Callee::Local(_));
        // a trait-method tail transfers through CALLTG now (the dispatch
        // tail form, H2's guarantee closure): a Static site's evidence and
        // instance words and a Runtime site's sentinel FuncRef ride the
        // same frame-reuse slide a local tail's arguments take, staged
        // after the exit claims. An inherent dispatch stays excluded — it
        // is a plain local callee taking the CALLT transfer below
        let native_tail_return =
            tail && !self.root && matches!(target, Callee::Builtin(_) | Callee::Generic(_));
        // a closure-staging local tail lowers in place for the root frame
        // too: the callee reads the word during the call, so the count
        // cannot ride the abandoned corner nor die at the pre-transfer
        // exit drops
        let in_place_tail = native_tail_return || closure_tail;
        // the frame-ending transfer: the exit drops and every claim hook
        // stage through the overlay as their own argument windows, so the
        // call's argument span stages after them, ahead of the CALLT
        let tail_transfer = tail && !in_place_tail;
        self.guard_staged_count(staged.len())?;
        // the literal arguments' constant loads land on their overlay
        // words at the staging position, in staged order
        for &(at, constant) in &consts {
            self.scalar_const_into(
                constant,
                Slot::overlay(u8::try_from(at).expect("overlay word fits u8")),
            );
        }
        for &(index, slot) in &moves {
            if !tail_transfer {
                self.emit(Op::Mov {
                    dst: Slot::overlay(u8::try_from(index).expect("overlay word fits u8")),
                    src: Slot::body(slot),
                });
            }
            // a staged word of a consuming (`move`) parameter: the callee
            // owns the value's death, so the count and any deinit claim
            // ride the staging as a hand-off — the checker's forced move
            // spent the source's death at exactly this use. A plain
            // count keeps the dup-or-forward death test outside the tail
            // transfer (a live copyable source dups), while a word a
            // claim covers always rides (a borrowed inc would leave the
            // caller's hook armed beside the callee's own)
            let consumes = word_arg
                .get(index)
                .is_some_and(|&arg| consuming.get(arg).copied().unwrap_or(false));
            // a staged Ref: a local callee's borrow parameter owns the
            // handed word (forward on the source's death, dup otherwise);
            // a native is a transient borrow window that never dups. A
            // tail-position staging abandons only when the frame's tail
            // transfer really ends the program (the root frame's named
            // corner); a non-root frame lowers its native tail call in
            // place below, so its staged borrows stay pending for the
            // post-call flush. A callable-defaulted closure word is a
            // borrow window against every callee kind: the callee reads it
            // (CALLS never consumes) and never owns its count, and a
            // closure-staging tail lowers in place below, so its flush
            // always has a post-call position
            if counted[index] {
                if defaulted.contains(&index) {
                    self.rc.staged_native(slot, false);
                } else {
                    match target {
                        Callee::Builtin(_) | Callee::Generic(_) => {
                            self.rc.staged_native(slot, tail && self.root);
                        },
                        Callee::Local(_) | Callee::Evidence(_) if tail_transfer && consumes => {
                            self.rc.staged_move(&mut self.code, slot);
                        },
                        Callee::Local(_) | Callee::Evidence(_) if tail_transfer => {
                            self.rc.staged_tail_local(&mut self.code, slot);
                        },
                        Callee::Local(_) | Callee::Evidence(_)
                            if consumes && self.rc.claim_base_of(slot).is_some() =>
                        {
                            self.rc.staged_move(&mut self.code, slot);
                        },
                        Callee::Local(_) | Callee::Evidence(_) => {
                            self.rc.staged_local(&mut self.code, slot);
                        },
                    }
                }
            } else if consumes && self.rc.claim_base_of(slot).is_some() {
                // a consuming scalar claim anchor (no Ref count of its
                // own): the hook rides the staging silently, or this
                // frame's scope end would fire it beside the callee's
                self.rc.staged_move(&mut self.code, slot);
            }
        }
        self.guard_staged_width(target, &staged, callee_span)?;
        if let Callee::Builtin(native) = target {
            if matches!(
                native.shape,
                NativeShape::Fallible { .. } | NativeShape::Checked
            ) {
                return self.native_span_result(native, ty, tail, callee_span, &staged);
            }
        }
        // a generic call site [H3]: stage the instantiated types as
        // evidence words after the real arguments and dispatch the
        // callee's ONE body through CALLG — or, from the root frame's
        // tail position, through the CALLTG transfer (a non-root frame
        // lowers the native tail in place below, the R13 shape, because
        // the body runs inside the instruction and no frame is pushed)
        let generic_target = match target {
            Callee::Generic(generic) => Some(generic),
            _ => None,
        };
        let mut generic_instance: Option<BuiltinInstance> = None;
        let generic_tail_at = staged.len();
        if let Some(generic) = generic_target {
            if tail && matches!(generic.shape, NativeShape::Checked) {
                return Err(LowerError::unsupported(
                    callee_span,
                    "a tail call to a checked-read generic",
                    "the ok/none span re-spans into the call's union at the call site, and the \
                     tail transfer leaves no position for it; keep the call out of tail position",
                ));
            }
            generic_instance = Some(self.stage_generic_evidence(
                generic,
                def.expect("a generic callee resolved to a def"),
                callee_span,
                *span,
                &mut staged,
                receiver_ty,
                tail_transfer,
            )?);
        }
        // a trait-method dispatch site [H3]: a call into a generic impl
        // body stages the runtime-evidence words its substitution
        // resolves (one evidence-chain object index per demand, in body
        // order) and then the solved declared-instance word; a call into
        // a monomorphic body stages the instance word alone. A nested
        // bound-variable site stages none of these — its slot word rides
        // below — and a nested site whose serving body is itself generic
        // stages the callee's words from this frame's own evidence area.
        // The staged types register here (the overlay span sizes from
        // them); on the tail path the stores defer past the exit claims,
        // staged after them like every tail-staged word (a claim's
        // overlay window would clobber words staged before it)
        let mut trait_evidence: Vec<EvidenceSrc> = Vec::new();
        let trait_evidence_at = staged.len();
        if let Some(TraitDispatch::Static { index, call }) = &trait_dispatch {
            let pairs = self.plan.functions()[*index as usize].evidence.clone();
            self.guard_staged_count(staged.len() + pairs.len() + 1)?;
            for pair in pairs.iter() {
                let words = if self.plan.is_default_body(*index) {
                    // the entered body is the trait's declared default:
                    // every demand keys by the trait's own variables —
                    // `Self` names the receiver (the chain starts at its
                    // member set under the site's own solved instance),
                    // every other names a trait generic the impl behind
                    // that instance substitutes through its trait
                    // reference — the declared bounds record answers
                    // neither shape
                    vec![EvidenceSrc::Const(self.default_pair_chain(
                        call.method,
                        call.instance,
                        Some(self.members(call.recv)),
                        pair,
                        "a trait-method call into a declared default body",
                        callee_span,
                    )?)]
                } else {
                    self.evidence_words(
                        pair,
                        &call.bounds,
                        Some(call.instance),
                        None,
                        Some(call.recv),
                        "a trait-method call into a generic impl body",
                        callee_span,
                    )?
                };
                trait_evidence.extend(words);
            }
            // The instance word is a per-branch id: a union-receiver
            // dispatch stages the representative branch of the covering
            // group, whose written head names the one impl item the word
            // stands for (the inline-cache story keys on the written
            // head, never on this branch id).
            trait_evidence.push(EvidenceSrc::Const(call.instance.index()));
            for _ in 0..=pairs.len() {
                staged.push(SlotType::I64);
            }
            if !tail_transfer {
                self.stage_member_evidence(&trait_evidence, trait_evidence_at);
            }
        }
        // a generic inherent member call stages the same per-pair
        // runtime-evidence words the trait path stages, minus the
        // instance word an inherent impl never has: the words trail the
        // staged arguments in the overlay, and the body claims them as
        // its trailing evidence parameters, so the ordinary CALL below —
        // and a tail site's CALLT — names the whole staged count; no
        // dispatch uop exists. The staged types register here (the
        // overlay span sizes from them), while the stores defer past the
        // exit claims on the tail path (a claim's overlay window would
        // clobber words staged before it)
        let mut member_evidence: Vec<EvidenceSrc> = Vec::new();
        let member_evidence_at = staged.len();
        if let Some(TraitDispatch::Inherent {
            index,
            bounds,
            method,
        }) = &trait_dispatch
        {
            let pairs = self.plan.functions()[*index as usize].evidence.clone();
            if !pairs.is_empty() {
                self.guard_staged_count(staged.len() + pairs.len())?;
                let inherent_site = self.analyzed.decls.inherent_impls.get(method).copied();
                for pair in pairs.iter() {
                    let words = self.evidence_words(
                        pair,
                        bounds,
                        None,
                        inherent_site,
                        receiver_ty,
                        "an inherent method call into a generic impl body",
                        callee_span,
                    )?;
                    member_evidence.extend(words);
                }
                for _ in 0..pairs.len() {
                    staged.push(SlotType::I64);
                }
                if !tail_transfer {
                    self.stage_member_evidence(&member_evidence, member_evidence_at);
                }
            }
        }
        // a free generic fn's call site [H3]: the callee's ONE body
        // claims its runtime-evidence demands as trailing parameters,
        // so the entering site stages them after the true arguments in
        // this outgoing-argument area — one evidence-chain object index
        // per demand, the checker's per-call record answering each bound
        // under this site's own substitution. The ordinary CALL below (a
        // tail site's CALLT) names the whole staged count, like the
        // inherent generic body's entering site. The staged types
        // register here (the overlay span sizes from them), while the
        // stores defer past the exit claims on the tail path (a claim's
        // overlay window would clobber words staged before it)
        let mut free_evidence: Vec<EvidenceSrc> = Vec::new();
        let free_evidence_at = staged.len();
        if let Callee::Local(index) = target {
            let pairs = self.plan.functions()[index as usize].evidence.clone();
            // a dispatch target (a trait's declared default body) stages
            // its evidence words through the dispatch branch above; this
            // free-fn staging serves named callees alone
            if trait_dispatch.is_none()
                && self.plan.functions()[index as usize].free_generic
                && !pairs.is_empty()
            {
                let Some(record) = self
                    .analyzed
                    .checked
                    .generic_call_bounds
                    .get(&call_span)
                    .cloned()
                else {
                    return Err(LowerError::unsupported(
                        callee_span,
                        "a call into a free generic fn's body",
                        "the checker recorded no bound answer at this site; report the construct",
                    ));
                };
                self.guard_staged_count(staged.len() + pairs.len())?;
                for pair in pairs.iter() {
                    let words = self.free_evidence_words(
                        pair,
                        &record,
                        "a call into a free generic fn's body",
                        callee_span,
                    )?;
                    free_evidence.extend(words);
                }
                for _ in 0..pairs.len() {
                    staged.push(SlotType::I64);
                }
                if !tail_transfer {
                    self.stage_member_evidence(&free_evidence, free_evidence_at);
                }
            }
        }
        // a nested runtime-resolved site: the target rides the frame's
        // evidence slot, whose word names an evidence-chain object. The
        // DISPATCH stages the chain's tail objects' indices as the
        // serving body's trailing evidence parameters (a static table
        // read per hop), so this site stages no words itself — it only
        // reserves the overlay: the runtime-resolved target's demand
        // count is bounded by the widest planned body's, and the
        // dispatch writes exactly what its chain object carries
        let mut runtime_riders = 0usize;
        if let Some(TraitDispatch::Runtime { .. }) = &trait_dispatch {
            runtime_riders = self.plan.max_evidence_pairs();
            self.guard_staged_count(staged.len() + runtime_riders)?;
            for _ in 0..runtime_riders {
                staged.push(SlotType::I64);
            }
        }
        // a checked generic read (`list_get`, `map_get`): the staged span
        // ends in its evidence words and the ok/none span re-spans
        // through the checked path, which emits the CALLG with the
        // real-argument count
        if let Callee::Generic(generic) = target {
            if matches!(generic.shape, NativeShape::Checked) {
                let real_args = staged.len() - usize::from(generic.evidence);
                return self.checked_native_result(target, ty, &staged, real_args);
            }
        }
        let result_words = self
            .callee_result_words(target, generic_instance, ty)
            .map_err(|err| err.respanned(callee_span))?;
        self.overlay_site(&staged, &result_words);
        let kidx = self.pool(self.funcref_const(target));
        // A native tail call transfers control only from the root frame:
        // CALLT-to-native (and CALLTG-to-generic) pops the frame inside
        // the instruction, so the staged borrows' decs have no code
        // position there — sound solely because the root frame's transfer
        // ends the program (the heap dies whole). From any other frame the
        // same tail position lowers in place: CALL + RET is
        // stack-identical (the native runs inside the CALL instruction,
        // no frame is pushed), and the post-call flush plus the return's
        // exit drops balance every count.
        // a dispatch tail (a trait site, or the root frame's generic
        // call) transfers through CALLTG: the dispatch resolution is the
        // CALLG emission's own and the transfer is the CALLT slide's, so
        // the guarantee admits no callee-class exception [H2]
        let dispatch_tail = generic_target.is_some()
            || matches!(
                trait_dispatch,
                Some(TraitDispatch::Static { .. } | TraitDispatch::Runtime { .. })
            );
        if tail_transfer {
            // control transfers to the callee; the stack never grows [H2].
            // A local callee owns its handed words (their counts ride the
            // transfer, its own exit drops them); every other owned
            // reference drops before the transfer, and a root-frame
            // native's staged borrows are the abandoned corner above. The
            // exit drops and every claim hook staged through the overlay
            // as their own argument windows, so the call's argument span
            // and the dispatch tail's evidence words stage after them —
            // the transfer hands the callee its words intact.
            self.rc.native_tail_transfer();
            self.rc.exit(&mut self.code, &[]);
            self.stage_member_evidence(&member_evidence, member_evidence_at);
            self.stage_member_evidence(&free_evidence, free_evidence_at);
            if let (Some(generic), Some(instance)) = (generic_target, generic_instance) {
                // the deferred evidence stores: one instance word per
                // constraint-list entry, staged after the exit claims
                let word = u32::try_from(instance as i64).expect("instance ids fit u32");
                let words = vec![EvidenceSrc::Const(word); usize::from(generic.evidence)];
                self.stage_member_evidence(&words, generic_tail_at);
            }
            self.stage_member_evidence(&trait_evidence, trait_evidence_at);
            self.stage_moves(&moves);
            if dispatch_tail {
                // the dispatch tail's uop names the real arguments; the
                // evidence/instance words trailing them ride the transfer
                let real_args = if let Some(generic) = generic_target {
                    staged.len() - usize::from(generic.evidence)
                } else if let Some(TraitDispatch::Static { index, .. }) = &trait_dispatch {
                    staged.len() - 1 - self.plan.functions()[*index as usize].evidence.len()
                } else {
                    staged.len() - runtime_riders
                };
                self.emit(Op::Calltg {
                    callee: kidx,
                    nargs: u8::try_from(real_args).expect("argument count fits u8"),
                });
            } else {
                self.emit(Op::Callt {
                    callee: kidx,
                    nargs: u8::try_from(staged.len()).expect("argument count fits u8"),
                });
            }
            // an unreachable marker place; no code follows a tail call
            return Ok(Place {
                base: 0,
                shape: PlaceShape::Word(SlotType::Raw),
            });
        }
        // the staged span of a generic call ends in its evidence words;
        // nargs names the real arguments, and the callee's own constraint
        // list carries the count the dispatch reads. A trait-method call
        // stages the same shape: one instance evidence word trails the
        // arguments, and the impl body the FuncRef names runs to
        // completion inside the instruction
        if generic_target.is_some() {
            let real_args = staged.len() - usize::from(generic_target.map_or(0, |g| g.evidence));
            self.emit(Op::Callg {
                callee: kidx,
                nargs: u8::try_from(real_args).expect("argument count fits u8"),
            });
        } else if let Some(TraitDispatch::Static { index, .. }) = &trait_dispatch {
            // the evidence words trail the real arguments (one per the
            // body's runtime demands, then the solved instance), so the
            // uop names the real-argument count and the callee claims
            // the rest as its trailing evidence parameters
            let real_args =
                staged.len() - 1 - self.plan.functions()[*index as usize].evidence.len();
            self.emit(Op::Callg {
                callee: kidx,
                nargs: u8::try_from(real_args).expect("argument count fits u8"),
            });
        } else if matches!(trait_dispatch, Some(TraitDispatch::Runtime { .. })) {
            // a nested runtime-resolved site: the sentinel FuncRef names
            // the frame's evidence slot and dispatches from it; the
            // chain's tail words trail the real arguments for the
            // serving body the slot may name, staged by the dispatch
            // itself from the chain object
            let real_args = staged.len() - runtime_riders;
            self.emit(Op::Callg {
                callee: kidx,
                nargs: u8::try_from(real_args).expect("argument count fits u8"),
            });
        } else {
            self.emit(Op::Call {
                callee: kidx,
                nargs: u8::try_from(staged.len()).expect("argument count fits u8"),
            });
        }
        self.rc.native_done(&mut self.code);
        let place = self.result_place(ty, &result_words, callee_span)?;
        // a known-function callee's fn-typed result is the scalar its own
        // provenance proved (a factory's result classifies Ref instead),
        // so the result word joins the provenance oracle and widens into
        // a re-storable slot like a named fn value
        if *result_words == [SlotType::Fn] && matches!(target, Callee::Local(_)) {
            self.scalar_fn_words.insert(place.base);
        }
        if in_place_tail {
            // the in-place tail call (a non-root native or generic tail,
            // or a local tail staging a closure into a callable-defaulted
            // parameter): the tail position returns the call's result,
            // the post-call flush settles the borrow windows, and the
            // return's exit drops flush every other count the frame owes
            self.emit_return(&place, ty, e.span())?;
            // an unreachable marker place; no code follows the return
            return Ok(Place {
                base: 0,
                shape: PlaceShape::Word(SlotType::Raw),
            });
        }
        Ok(place)
    }

    /// Stages one call's argument span: the recorded body words move into
    /// the overlay in argument order.
    fn stage_moves(&mut self, moves: &[(usize, u8)]) {
        for &(index, slot) in moves {
            self.emit(Op::Mov {
                dst: Slot::overlay(u8::try_from(index).expect("overlay word fits u8")),
                src: Slot::body(slot),
            });
        }
    }

    /// Copies one call's result span out of the overlay into a fresh temp:
    /// the callee transferred its result references out with the return.
    fn result_place(
        &mut self,
        ty: Ty,
        result_words: &[SlotType],
        span: Span,
    ) -> Result<Place, LowerError> {
        let base = self.alloc(result_words)?;
        for (index, slot) in result_words.iter().enumerate() {
            let word = base + u8::try_from(index).expect("result width fits u8");
            self.emit(Op::Mov {
                dst: Slot::body(word),
                src: Slot::overlay(u8::try_from(index).expect("result word fits u8")),
            });
            // a Raw result word is a type variable's runtime word: the
            // callee transferred its count, and the kind-checked drop
            // balances it when the value is a reference
            if *slot == SlotType::Ref || *slot == SlotType::Raw {
                self.rc.produced(word);
            }
        }
        // a deinit-carrying result owns its hook wherever it lands (C5):
        // the temp holds the value until a binder aliases or discards it
        if let Some(claim) = self.deinit_claim(ty, span)? {
            self.overlay_site(&claim.staged, &[SlotType::I64]);
            let words = self.ref_words(base, ty)?;
            let band = self.claim_band_word(base)?;
            self.rc
                .claimed(&mut self.code, base, claim.hook, band, &words);
        }
        let shape = match &*self.shapes.shape(ty)? {
            // the copy-out word's own kind leads: a factory's fn-typed
            // result classifies Ref (the closure its tail constructs),
            // so the place names the physical word the call slide
            // validated — a Fn classification would hide the count from
            // every use and the binder
            Shape::Word(slot) => PlaceShape::Word(result_words.first().copied().unwrap_or(*slot)),
            _ => PlaceShape::Span(ty),
        };
        Ok(Place { base, shape })
    }

    /// CALLS (the callable-word call form, F2): the callee is a local
    /// binding whose single word names the function — a scalar Fn word,
    /// or a Ref word naming a closure object (the read dispatches on the
    /// word's kind; the emitted op is the same). Arguments stage on the
    /// overlay exactly like a pool call, the callee frame overlays it,
    /// and the result copies out of the overlay. A tail position runs
    /// the maybe-Ref callee tracking [H2's closure ruling]: the callee
    /// word is maybe-Ref by classification, and the provenance tables
    /// (`scalar_fn_words` naming the function through `fn_words`) prove
    /// some words count-free scalars naming exactly one planned function
    /// — those tails transfer through CALLT with that function's own
    /// pool FuncRef, and every unprovable word (a closure's Ref, a
    /// callable-defaulted parameter, an unproven alias) keeps the
    /// in-place CALLS plus return.
    fn calls_value(
        &mut self,
        ty: Ty,
        bind: Span,
        callee_span: Span,
        arg_exprs: &[&Expr],
        tail: bool,
    ) -> Result<Place, LowerError> {
        let callee_ty = self.val(callee_span)?.ty;
        let Some((params, ret)) = fn_sig_of(&self.analyzed.decls.universe, callee_ty) else {
            return Err(LowerError::unsupported(
                callee_span,
                "a callable callee",
                "its checked type is not a function",
            ));
        };
        // the callee use is a read of the binder: a closure word's death
        // test compares against this span
        self.rc.use_of(bind, callee_span);
        let place = self
            .binders
            .get(&bind)
            .cloned()
            .ok_or_else(|| self.missing_binder(callee_span))?;
        // the callee word: a Fn-classified slot directly (a scalar word or
        // a callable-defaulted parameter), a Ref-classified value-site
        // word (CALLS reads the FuncRef behind the closure object's
        // header), or the payload word of a union span an extraction
        // handler's parameter aliases
        let word = single(&self.word_addrs(&place, callee_ty)?, callee_span)?;
        let closure = word.slot_ty == SlotType::Ref;
        // the maybe-Ref callee tracking: a proven scalar word is
        // count-free by construction and names exactly one planned
        // function, so a tail call through it can take the CALLT
        // transfer; every other word stays behind the runtime CALLS read
        let proven_fn = self
            .scalar_fn_words
            .contains(&word.slot)
            .then(|| self.fn_words.get(&word.slot).copied())
            .flatten();
        let mut expected_words: Vec<SlotType> = Vec::with_capacity(params.len());
        for ty in params {
            expected_words.extend_from_slice(&self.shapes.words(*ty)?);
        }
        let expected = expected_words.len();
        // the planned body the callee word names, when this frame
        // constructed it (a scalar Fn word or a closure object): its own
        // parameter modes stay readable, so a deinit-carrying argument
        // can be checked against its contract. A word this frame did not
        // construct (a fn-typed parameter's) erases them.
        let callee_index = self.fn_words.get(&word.slot).copied();
        let param_modes = callee_index.and_then(|index| self.planned_param_modes(index));
        // stage: evaluate every argument to a place, then move its words
        // into the overlay area (argument evaluation may itself call).
        // The borrow window opens before that evaluation and pins the
        // callee word (CALLS reads it during the call) plus every word
        // the staged arguments resolve to
        let mut staged: Vec<SlotType> = Vec::new();
        let mut moves: Vec<(usize, u8)> = Vec::new();
        // per staged word, whether it carries a count to the hand-off: a
        // Ref word always, and the frame's own type-variable word — its
        // single Raw word may hold a counted reference at runtime (the
        // evidence-carrying closure's variable parameter dispatches into
        // Ref-claiming bodies), and the kind-checked inc/dec pair makes
        // the runtime fact safe to stage blind
        let mut counted: Vec<bool> = Vec::new();
        // staged indices whose Ref word lands in a callable-defaulted `Fn`
        // parameter: the callee reads the word and never owns its count
        let mut defaulted: Vec<usize> = Vec::new();
        // a deinit-carrying argument pins its hook to this frame's death
        // sites, so a tail through the callable word lowers in place
        // (the callee's borrow would outlive a transferred frame)
        let mut deinit_arg = false;
        self.rc.native_open();
        self.rc.native_reads([word.slot]);
        let mut consts: Vec<(usize, ScalarConst)> = Vec::new();
        for (index, &arg) in arg_exprs.iter().enumerate() {
            let param_ty = params.get(index).copied();
            // A literal scalar argument stages its constant directly on
            // its overlay word. A tail transfer defers the staging past
            // the exit claims, so it keeps the evaluated temp.
            if !tail && let Some((slot, constant)) = self.direct_literal(arg, param_ty)? {
                let at = staged.len();
                staged.push(slot);
                counted.push(false);
                consts.push((at, constant));
                continue;
            }
            // A single-use computed scalar argument writes its overlay
            // word directly: the offer goes to the argument's outermost
            // data op, taken only when every later argument evaluates
            // without touching the overlay.
            let offered = if !tail
                && let Some(slot) = self.direct_compute(arg, param_ty, &arg_exprs[index + 1..])?
            {
                self.scalar_dst = Some(ScalarDst::Arg {
                    index: u8::try_from(staged.len()).expect("overlay word fits u8"),
                    slot,
                });
                true
            } else {
                false
            };
            let arg_val = self.value(arg)?;
            let direct = offered && self.scalar_dst.take().is_none();
            if self.deinit_claim(arg_val.ty, arg.span())?.is_some() {
                // a consuming parameter behind a callable word has no
                // claim contract: the affine pass borrows the argument
                // at an indirect call (the fn type erases the mode), so
                // the callee's own claim would double this frame's —
                // and an untraceable word cannot promise borrow mode
                // either. Per-span claims own the seam; named callees
                // carry the mode.
                match param_modes.as_ref().and_then(|modes| modes.get(index)) {
                    Some(ds_parse::ParamMode::Borrow | ds_parse::ParamMode::Mut(_)) => {},
                    _ => {
                        return Err(LowerError::unsupported(
                            arg.span(),
                            "a deinit-carrying argument at a callable value",
                            "the callable's fn type erases the parameter mode, so the claim \
                             contract cannot be established; pass the value to a named function",
                        ));
                    },
                }
                if tail {
                    deinit_arg = true;
                }
            }
            // a closure argument lands only in a callable-defaulted `Fn`
            // parameter, staged as a borrow window
            if self.is_closure_word(&arg_val.place, arg_val.ty)? {
                Self::guard_callable_staged(Some(&expected_words), staged.len(), arg.span())?;
                defaulted.push(staged.len());
            }
            // a union-typed parameter takes its member's whole region: a
            // bare member or a strict sub-union argument re-spans into
            // a staging temp of the parameter's region, the named-callee
            // widening family's value-site spelling
            let arg_val = match params.get(index) {
                Some(&param_ty) if param_ty != arg_val.ty && self.is_union_ty(param_ty)? => {
                    // the same staging-temp backstop as the named
                    // callee path
                    if self.deinit_claim(arg_val.ty, arg.span())?.is_some() {
                        return Err(LowerError::unsupported(
                            arg.span(),
                            "a deinit-carrying argument into a union parameter",
                            "the re-span's staging temp is anonymous, so it carries no claim \
                             contract of its own; the deinit arc owns the seam",
                        ));
                    }
                    let temp = self.temp_span(param_ty)?;
                    self.widen_into(&temp, param_ty, &arg_val, arg.span())?;
                    TypedPlace {
                        place: temp,
                        ty: param_ty,
                    }
                },
                _ => arg_val,
            };
            // the argument's own variable word stages as the wildcard: a
            // Raw word whose runtime kind the callee claims, so its
            // count rides the hand-off below like a Ref word's
            let var_word = self.is_var_ty(arg_val.ty);
            if direct {
                // the argument's outermost data op wrote its overlay
                // word: the staging records the word's kind; no Mov
                // and no window read ride it (a scalar word carries
                // no count)
                let PlaceShape::Word(slot) = arg_val.place.shape else {
                    return Err(mismatch("a directly staged scalar argument"));
                };
                staged.push(slot);
                counted.push(false);
            } else {
                for addr in self
                    .word_addrs(&arg_val.place, arg_val.ty)
                    .map_err(|err| err.respanned(arg.span()))?
                {
                    self.rc.native_reads([addr.slot]);
                    moves.push((staged.len(), addr.slot));
                    staged.push(addr.slot_ty);
                    counted.push(addr.slot_ty == SlotType::Ref || var_word);
                }
            }
        }
        self.guard_staged_count(staged.len())?;
        if staged.len() != expected {
            let reason = if staged.len() < expected {
                "a defaulted parameter on a callable value has no staging path this round; pass \
                 every argument explicitly"
            } else {
                "the staged words disagree with the callee's parameter span; a union-typed \
                 parameter takes its member's whole region"
            };
            return Err(LowerError::unsupported(
                callee_span,
                "a call argument",
                reason,
            ));
        }
        // the tail transfer: only a proven count-free scalar callee word
        // with no closure-defaulted argument and no deinit-carrying
        // argument takes it — the count-free word needs no settling and
        // the named function's own pool FuncRef drives the CALLT
        let tail_transfer = tail && proven_fn.is_some() && defaulted.is_empty() && !deinit_arg;
        // the literal arguments' constant loads land on their overlay
        // words at the staging position, in staged order
        for &(at, constant) in &consts {
            self.scalar_const_into(
                constant,
                Slot::overlay(u8::try_from(at).expect("overlay word fits u8")),
            );
        }
        for &(index, slot) in &moves {
            if !tail_transfer {
                self.emit(Op::Mov {
                    dst: Slot::overlay(u8::try_from(index).expect("overlay word fits u8")),
                    src: Slot::body(slot),
                });
            }
            // a borrow parameter owns the handed word: forward on the
            // source's death, dup otherwise (C2/C4); a callable-defaulted
            // closure word is a plain borrow — the callee reads it (CALLS
            // never consumes) and never owns its count. A transferring
            // tail's count rides the callee like every staged local word
            // (its own exit drops the handed parameter). The variable
            // word's Raw spelling counts too: the kind-checked pair makes
            // the runtime fact safe to stage blind
            if counted[index] {
                if defaulted.contains(&index) {
                    self.rc.staged_native(slot, false);
                } else if tail_transfer {
                    self.rc.staged_tail_local(&mut self.code, slot);
                } else {
                    self.rc.staged_local(&mut self.code, slot);
                }
            }
        }
        let mut result_words = self.shapes.words(ret)?;
        // a lambda-value callee is a known function: a factory's fn-typed
        // result is the closure its body constructs, so the result word
        // classifies Ref (a binding carrying an unknown fn value keeps
        // the scalar classification — a closure returned through it traps
        // at the call, the recorded return-flow seam)
        if *result_words == [SlotType::Fn] {
            if let Some(&index) = self.fn_words.get(&word.slot) {
                if self.tail_builds_a_closure(index) {
                    result_words = Rc::from([SlotType::Ref]);
                }
            }
        }
        self.overlay_site(&staged, &result_words);
        if tail_transfer {
            // the tracking proved the callee word a count-free scalar
            // naming one planned function: the tail transfers through
            // CALLT with that function's own pool FuncRef [H2], the exit
            // claims staging first and the argument moves after them
            let index = proven_fn.expect("the transfer requires a proven function");
            let kidx = self.pool(Constant::FuncRef {
                key: FuncRefKey::Dep(0), // the enclosing module
                func: index,
            });
            self.rc.native_tail_transfer();
            self.rc.exit(&mut self.code, &[]);
            self.stage_moves(&moves);
            self.emit(Op::Callt {
                callee: kidx,
                nargs: u8::try_from(staged.len()).expect("argument count fits u8"),
            });
            // an unreachable marker place; no code follows a tail call
            return Ok(Place {
                base: 0,
                shape: PlaceShape::Word(SlotType::Raw),
            });
        }
        self.emit(Op::Calls {
            callee_slot: word.slot,
            nargs: u8::try_from(staged.len()).expect("argument count fits u8"),
        });
        self.rc.native_done(&mut self.code);
        if closure {
            // the call read the closure word: at its death use the count
            // drops here, the object's death releasing the captures
            self.rc.read(&mut self.code, &[word.slot]);
        }
        let place = self.result_place(ty, &result_words, callee_span)?;
        // the same trust as the named-callee path: the factory check ran
        // over this word and left the scalar classification, so the
        // result word is a proven scalar; an unproven callee word keeps
        // no trust (its result may hold a closure's Ref at runtime)
        if *result_words == [SlotType::Fn] && self.fn_words.contains_key(&word.slot) {
            self.scalar_fn_words.insert(place.base);
        }
        if tail {
            // the in-place tail (the unproven maybe-Ref word): the CALLS
            // runs inside this frame, the post-call flush settles the
            // borrow windows, and the return's exit drops flush every
            // other count the frame owes
            self.emit_return(&place, ty, callee_span)?;
            // an unreachable marker place; no code follows the return
            return Ok(Place {
                base: 0,
                shape: PlaceShape::Word(SlotType::Raw),
            });
        }
        Ok(place)
    }

    /// A fallible or checked native's result span: neither platform span
    /// carries a source-level return type, so a tail call rejects, and a
    /// value-position call re-spans through its shape's dedicated path.
    fn native_span_result(
        &mut self,
        native: &'static Native,
        ty: Ty,
        tail: bool,
        callee_span: Span,
        staged: &[SlotType],
    ) -> Result<Place, LowerError> {
        if tail {
            let (what, reason) = if let NativeShape::Fallible { .. } = native.shape {
                (
                    "a tail call to a fallible native",
                    "CALLT-to-fallible stays unsupported: the two-word platform span re-spans \
                     into the call's union at the call site, and the tail transfer leaves no \
                     position for it; keep the call out of tail position",
                )
            } else {
                (
                    "a tail call to a checked-read native",
                    "the ok/none span has no source-level return type; keep the call out of tail \
                     position",
                )
            };
            return Err(LowerError::unsupported(callee_span, what, reason));
        }
        if let NativeShape::Fallible {
            payload,
            err_prefix,
        } = native.shape
        {
            return self.fallible_native_result(native, ty, payload, err_prefix, staged);
        }
        self.checked_native_result(Callee::Builtin(native), ty, staged, staged.len())
    }

    /// Stages one generic call's evidence words [H3]: the receiver's
    /// builtin instance — one statically-resolved instance reference,
    /// an i64 pool constant — staged on the overlay after the real
    /// arguments, one word per entry of the callee's constraint list.
    /// The checker's recorded instantiation must agree with the
    /// constraint count; a receiver no covered instance names, or one
    /// outside the callee's implemented set, is a lowering error,
    /// never a runtime misdispatch. The minted instance comes back for
    /// the result shape's instance-keyed word kind. `defer` registers
    /// the staged types without emitting the Kimm stores — the tail
    /// transfer path re-stages every word past the exit claims, and a
    /// claim's overlay window would clobber words staged before it.
    fn stage_generic_evidence(
        &mut self,
        generic: &'static GenericNative,
        def: DefId,
        span: Span,
        call: Span,
        staged: &mut Vec<SlotType>,
        receiver_ty: Option<Ty>,
        defer: bool,
    ) -> Result<BuiltinInstance, LowerError> {
        let sig = self
            .analyzed
            .decls
            .fns
            .get(&def)
            .ok_or_else(|| mismatch("a generic callee signature"))?;
        let instantiations = self
            .analyzed
            .checked
            .generic_calls
            .get(&call)
            .ok_or_else(|| {
                LowerError::unsupported(
                    span,
                    "a generic call",
                    "the checker recorded no full instantiation for it",
                )
            })?;
        if instantiations.len() != sig.generics.len()
            || instantiations.len() != usize::from(generic.evidence)
        {
            return Err(LowerError::unsupported(
                span,
                "a generic call",
                "the builtin's evidence arity disagrees with the declaration's generics",
            ));
        }
        let receiver = receiver_ty.ok_or_else(|| mismatch("a generic call's receiver"))?;
        let instance = self.shapes.builtin_instance(receiver).ok_or_else(|| {
            LowerError::unsupported(
                span,
                "a generic call",
                "no builtin instance covers the receiver's type; string, the list and map \
                 constructors, and the int and float scalars are the covered ones",
            )
        })?;
        if !generic.instances.contains(&instance) {
            return Err(LowerError::unsupported(
                span,
                "a generic call",
                "the builtin's one body implements no instance of the receiver's type",
            ));
        }
        self.guard_staged_count(staged.len() + usize::from(generic.evidence))?;
        for _ in 0..generic.evidence {
            if !defer {
                let kidx = self.pool(Constant::I64(instance as i64));
                self.emit(Op::Kimm {
                    dst: Slot::overlay(u8::try_from(staged.len()).expect("overlay word fits u8")),
                    kidx,
                });
            }
            staged.push(SlotType::I64);
        }
        Ok(instance)
    }

    /// A method-call site's solved dispatch [B6, H3]: the impl body's
    /// function-table index plus the instance id the checker recorded at
    /// the call's own span. An inherent survivor resolves through its
    /// own record below, and a site satisfied only through an in-scope
    /// assumption records no instance — the latter refuses naming the
    /// seam it waits on.
    fn resolve_method_dispatch(
        &self,
        call: Span,
        what: Span,
        recv: Option<&Expr>,
        candidates: &[MethodId],
    ) -> Result<TraitDispatch, LowerError> {
        let inherent_only = candidates.iter().all(|&method| {
            matches!(
                self.analyzed.package.method(method).owner,
                MethodOwner::Inherent(_)
            )
        }) && !candidates.is_empty();
        if let Some(&method) = self.analyzed.checked.inherent_calls.get(&call) {
            return self.resolve_inherent_method(method, call, what);
        }
        let Some(tc) = self.analyzed.checked.trait_calls.get(&call).cloned() else {
            // A bound-variable receiver inside a generic impl body: the
            // checker satisfied the call through the impl's own bound
            // (an in-scope assumption), so no instance records here —
            // dispatch resolves at runtime through the evidence slot the
            // call site entering this body staged.
            if let Some(recv) = recv
                && let Ok(val) = self.val(recv.span())
                && let [var] = self.members(val.ty)
                && self.shapes.is_var_atom(*var)
                && let Some(param) =
                    crate::shape::var_param_name(&self.analyzed.decls.universe, *var)
            {
                let matching: Vec<(MethodId, u8)> = self
                    .evidence
                    .iter()
                    .filter(|((pair_var, method), _)| {
                        pair_var == &param && candidates.contains(method)
                    })
                    .map(|((_, method), slot)| (*method, *slot))
                    .collect();
                return match matching.as_slice() {
                    [(method, slot)] => Ok(TraitDispatch::Runtime {
                        slot: *slot,
                        method: *method,
                    }),
                    [] => Err(LowerError::unsupported(
                        what,
                        "a trait-method call on a bound type variable",
                        "this body carries no runtime evidence slot for the member (the body \
                         demands no such pair, or the frame is outside the evidence seam); the \
                         seam covers generic bodies, their lambdas, and their spawned children",
                    )),
                    _ => Err(LowerError::unsupported(
                        what,
                        "a trait-method call on a bound type variable",
                        "the member name is ambiguous across the variable's bounds; qualify the \
                         trait",
                    )),
                };
            }
            return Err(LowerError::unsupported(
                what,
                if inherent_only { "an inherent method call" } else { "a trait-method call" },
                if inherent_only {
                    "the checker recorded no solved inherent dispatch for this site; report the \
                     construct"
                } else {
                    "the checker recorded no solved instance for this site (a bound-type receiver \
                     outside its generic impl body)"
                },
            ));
        };
        let Some(impl_site) = tc.impl_site else {
            return Err(LowerError::unsupported(
                what,
                "a trait-method call",
                "the solving impl item's span is missing from the declaration tables",
            ));
        };
        let member = self.analyzed.package.method(tc.method).name.clone();
        if let Some(index) = self.plan.impl_body(impl_site, &member) {
            return Ok(TraitDispatch::Static { index, call: tc });
        }
        // A defaulted member is omittable exactly because the trait's
        // declaration carries the dispatch target: an impl spelling no
        // member dispatches into the declared default body, the same
        // staged shape (evidence demands trailing the true arguments,
        // the solved instance word trailing them).
        if let MethodOwner::Trait(def) = self.analyzed.package.method(tc.method).owner
            && let Some(index) = self.plan.default_body(def, &member)
        {
            return Ok(TraitDispatch::Static { index, call: tc });
        }
        Err(LowerError::unsupported(
            what,
            "a trait-method call",
            "the solving impl's body did not plan (no body, no solved return, or a shape over its \
             own type variables); report the construct",
        ))
    }

    /// A solved inherent-method call site: the impl body the
    /// checker-recorded method id names, reached through the declaration
    /// tables' method-to-impl-item map. The receiver's own type picked
    /// the body at check time — no instance word exists for an inherent
    /// impl, so the call dispatches like a local callee; a generic body's
    /// entering site carries the checker-recorded bounds (the serving
    /// instance per declared bound under the receiver's substitution),
    /// which stage the body's evidence demands.
    fn resolve_inherent_method(
        &self,
        method: MethodId,
        call: Span,
        what: Span,
    ) -> Result<TraitDispatch, LowerError> {
        let Some(&impl_site) = self.analyzed.decls.inherent_impls.get(&method) else {
            return Err(LowerError::unsupported(
                what,
                "an inherent method call",
                "the impl item owning the method is missing from the declaration tables; report \
                 the construct",
            ));
        };
        let member = self.analyzed.package.method(method).name.clone();
        let Some(index) = self.plan.impl_body(impl_site, &member) else {
            return Err(LowerError::unsupported(
                what,
                "an inherent method call",
                "the impl's body did not plan (no body, no solved return, or a shape over its own \
                 type variables); report the construct",
            ));
        };
        let bounds = self
            .analyzed
            .checked
            .inherent_call_bounds
            .get(&call)
            .cloned()
            .unwrap_or_default();
        Ok(TraitDispatch::Inherent {
            index,
            bounds,
            method,
        })
    }

    /// The trait one evidence pair's member belongs to: the walk's key
    /// into the checker's per-site bound records.
    fn pair_trait_id(
        &self,
        pair: &RuntimePair,
        what: &'static str,
        span: Span,
    ) -> Result<TraitId, LowerError> {
        let method = self.analyzed.package.method(pair.method);
        let MethodOwner::Trait(def) = method.owner else {
            return Err(LowerError::unsupported(
                span,
                what,
                "the body's member call did not resolve to a trait member; report the construct",
            ));
        };
        self.analyzed
            .decls
            .traits
            .get(&def)
            .copied()
            .ok_or_else(|| {
                LowerError::unsupported(
                    span,
                    what,
                    "the member's trait is missing from the declaration tables",
                )
            })
    }

    /// The runtime form's evidence word for one pair: this site's
    /// substitution is its own frame's, so the receiver's single
    /// variable peels to the frame's written parameter name and the
    /// pair's slot word — the chain object's index — reads verbatim
    /// from the frame's evidence slots.
    fn slot_evidence(
        &self,
        recv: Ty,
        pair: &RuntimePair,
        what: &'static str,
        span: Span,
    ) -> Result<Vec<EvidenceSrc>, LowerError> {
        let universe = &self.analyzed.decls.universe;
        let [only] = universe.members(recv) else {
            return Err(LowerError::unsupported(
                span,
                what,
                "the receiver unions members across branches; per-branch runtime evidence has no \
                 single serving body",
            ));
        };
        let Some(param) = peeled_param(universe, *only) else {
            return Err(LowerError::unsupported(
                span,
                what,
                "the receiver's type carries no type variable of this body, so the demand keys no \
                 frame evidence slot",
            ));
        };
        let Some(&slot) = self.evidence.get(&(param, pair.method)) else {
            return Err(LowerError::unsupported(
                span,
                what,
                "this frame carries no runtime-evidence slot for the member (a demand the walk \
                 does not collect); the evidence seam covers generic bodies, their lambdas, and \
                 their spawned children",
            ));
        };
        Ok(vec![EvidenceSrc::Slot(slot)])
    }

    /// The evidence-chain object one statically-resolved demand resolves
    /// to [H3]: the serving member body's function-table index for
    /// `method` over the substitution `start` names, plus one tail chain
    /// per the serving body's own evidence demands — each demand's own
    /// member and substituted type, walked recursively. The staged word
    /// a site enters this chain through carries the object's table
    /// index; a nested runtime dispatch reads its frame's slot word,
    /// resolves the object, dispatches into `func`, and stages the tail
    /// objects' indices as the body's trailing evidence parameters — so
    /// depth is table-bounded, never frame-bounded, and a body may
    /// demand several or different members (one tail chain each).
    ///
    /// The walk memoizes its states (member, instance, substitution): a
    /// repeated state shares its entry, which is also the termination
    /// story — a cyclic demand graph builds a cyclic table the
    /// dispatches walk lazily, one hop per call.
    fn chain_for(
        &mut self,
        method: MethodId,
        instance: InstanceId,
        start: Option<&[TypeId]>,
        what: &'static str,
        span: Span,
    ) -> Result<u32, LowerError> {
        let universe = &self.analyzed.decls.universe;
        let env = &self.analyzed.decls.instances;
        // the one-word instantiation boundary: this level's serving body
        // is entered by a nested dispatch that stages the bound
        // variable's single runtime word, so a substitution spanning more
        // must refuse at this entering site, where the type is concrete
        if let Some(members) = start {
            self.guard_one_word_instantiation(members, what, span)?;
        }
        let key = (method, instance, start.map(<[TypeId]>::to_vec));
        if let Some(&index) = self.plan.chain_memo.get(&key) {
            return Ok(index);
        }
        let impl_site = self
            .analyzed
            .decls
            .instance_impls
            .get(&instance)
            .copied()
            .ok_or_else(|| {
                LowerError::unsupported(
                    span,
                    what,
                    "the serving instance's impl item is missing from the declaration tables",
                )
            })?;
        let func = self.serving_body(impl_site, method, what, span)?;
        // reserve the entry before the recursion so a cyclic demand
        // graph closes over it
        let index = u32::try_from(self.plan.chains.len()).expect("chain table fits u32");
        self.plan.chains.push(ds_mod::EvidenceChain {
            func,
            next: Vec::new(),
        });
        self.plan.chain_memo.insert(key, index);
        let pairs = self.plan.functions()[func as usize].evidence.clone();
        let mut next = Vec::with_capacity(pairs.len());
        if !pairs.is_empty() {
            if self.plan.is_default_body(func) {
                // the serving body is a trait's declared default: every
                // demand keys by the trait's own variables and resolves
                // under the substitution this level entered with — a
                // self-recursive demand closes onto this very entry
                // through the memo, so a cyclic demand graph still
                // builds a cyclic table
                for pair in pairs.iter() {
                    next.push(self.default_pair_chain(method, instance, start, pair, what, span)?);
                }
            } else {
                let Some(members) = start else {
                    return Err(LowerError::unsupported(
                        span,
                        what,
                        "the transitive serving body's own bound is not resolvable at this site; \
                         report the construct",
                    ));
                };
                for pair in pairs.iter() {
                    let pair_trait = self.pair_trait_id(pair, what, span)?;
                    // the serving body's own demand variable names the
                    // next bound: the serving impl declares it, so its
                    // conditions key by that written name
                    let Some((next_members, Some(next_instance))) =
                        ds_check::instance_solve::evidence_step(
                            universe,
                            env,
                            instance,
                            members,
                            Some(pair.var.as_str()),
                            pair_trait,
                        )
                    else {
                        return Err(LowerError::unsupported(
                            span,
                            what,
                            "the element type's transitive bound has no unique serving instance \
                             at this site; runtime evidence resolves one body per bound",
                        ));
                    };
                    // the same boundary one level down: the next link's
                    // serving body receives this substitution as its
                    // receiver
                    self.guard_one_word_instantiation(&next_members, what, span)?;
                    next.push(self.chain_for(
                        pair.method,
                        next_instance,
                        Some(&next_members),
                        what,
                        span,
                    )?);
                }
            }
        }
        self.plan.chains[index as usize].next = next;
        Ok(index)
    }

    /// One demand pair's chain inside a trait's declared default body,
    /// over the member whose default body serves this level, the
    /// instance whose impl enters it, and the receiver member set the
    /// body runs with: a `Self`-keyed pair re-serves the receiver under
    /// the same instance (the entry `chain_for` builds for the called
    /// member — the memo closes a self-recursive demand onto its own
    /// entry), while a pair over the trait's own generic variable
    /// resolves through the argument the impl's trait reference spells
    /// for it: a concrete argument names the substituted type itself,
    /// an impl-variable argument names a variable this receiver binds,
    /// the impl's own declared bound on it carrying the next level
    /// exactly as the generic-impl branch's step.
    fn default_pair_chain(
        &mut self,
        method: MethodId,
        instance: InstanceId,
        start: Option<&[TypeId]>,
        pair: &RuntimePair,
        what: &'static str,
        span: Span,
    ) -> Result<u32, LowerError> {
        // the receiver stages as the default body's `Self` variable's
        // single Raw word, so the one-word instantiation boundary guards
        // it here (the tail level re-guards what its own chain_for entry
        // already checked)
        if let Some(members) = start {
            self.guard_one_word_instantiation(members, what, span)?;
        }
        if pair.var == "Self" {
            return self.chain_for(pair.method, instance, start, what, span);
        }
        let universe = &self.analyzed.decls.universe;
        let env = &self.analyzed.decls.instances;
        let trait_id = self.pair_trait_id(pair, what, span)?;
        // the default body's trait, and the argument its impl substitutes
        // for the demanded generic (positional over the declaration's
        // own generics)
        let MethodOwner::Trait(def) = self.analyzed.package.method(method).owner else {
            return Err(LowerError::unsupported(
                span,
                what,
                "the declared default body's member did not resolve to a trait; report the \
                 construct",
            ));
        };
        let generics = self.analyzed.decls.trait_generics.get(&def);
        let impl_site = self.analyzed.decls.instance_impls.get(&instance).copied();
        let arg = generics
            .and_then(|names| names.iter().position(|name| name == &pair.var))
            .zip(impl_site.and_then(|site| self.analyzed.decls.trait_impl_args.get(&site)))
            .and_then(|(position, args)| args.get(position).copied());
        let Some(arg) = arg else {
            return Err(LowerError::unsupported(
                span,
                what,
                "the trait variable's substitution is missing from the impl's trait reference at \
                 this site; report the construct",
            ));
        };
        let members = universe.members(arg);
        // the argument is the impl's own variable: this receiver binds
        // it, and the impl's declared bound on it carries the next level
        if let [only] = members
            && let Some(var) = crate::shape::var_param_name(universe, *only)
        {
            let Some(recv) = start else {
                return Err(LowerError::unsupported(
                    span,
                    what,
                    "the trait variable maps to the impl's own variable, whose substitution this \
                     site does not carry; report the construct",
                ));
            };
            let Some((next_members, Some(next_instance))) = ds_check::instance_solve::evidence_step(
                universe,
                env,
                instance,
                recv,
                Some(var.as_str()),
                trait_id,
            ) else {
                return Err(LowerError::unsupported(
                    span,
                    what,
                    "the impl's own bound on the trait variable's substitution has no unique \
                     serving instance at this site; runtime evidence resolves one body per bound",
                ));
            };
            // the same boundary one level down: the next link's serving
            // body receives this substitution as its receiver
            self.guard_one_word_instantiation(&next_members, what, span)?;
            return self.chain_for(pair.method, next_instance, Some(&next_members), what, span);
        }
        // the argument is concrete: it names the substituted type itself
        if !members
            .iter()
            .any(|&atom| carries_a_var(universe, atom, None))
        {
            let Some(next_instance) =
                ds_check::instance_solve::resolve(universe, env, &[], trait_id, arg).solved()
            else {
                return Err(LowerError::unsupported(
                    span,
                    what,
                    "the trait variable's substituted type has no serving instance at this site; \
                     runtime evidence resolves one body per bound",
                ));
            };
            self.guard_one_word_instantiation(members, what, span)?;
            return self.chain_for(pair.method, next_instance, Some(members), what, span);
        }
        Err(LowerError::unsupported(
            span,
            what,
            "the trait variable substitutes to a compound type over the impl's variables, a shape \
             this seam resolves per variable alone; report the construct",
        ))
    }

    /// The dispatch target one solved instance names for one member: the
    /// impl's spelled body, or — when the impl spells no member — the
    /// trait's declared default body (the member is omittable exactly
    /// because the declaration carries the dispatch target).
    fn serving_body(
        &self,
        impl_site: Span,
        method: MethodId,
        what: &'static str,
        span: Span,
    ) -> Result<u32, LowerError> {
        let method = self.analyzed.package.method(method);
        if let Some(index) = self.plan.impl_body(impl_site, &method.name) {
            return Ok(index);
        }
        if let MethodOwner::Trait(def) = method.owner
            && let Some(index) = self.plan.default_body(def, &method.name)
        {
            return Ok(index);
        }
        Err(LowerError::unsupported(
            span,
            what,
            "the element type's serving impl body did not plan; report the construct",
        ))
    }

    /// Whether one decomposed branch instance's head mentions a minted
    /// variable of the written name anywhere in its shape: per-branch
    /// solving keeps a branch's condition only for the variables its
    /// own head mentions, so a demand keyed by a name the entering
    /// branch never carries is unreachable in it — the narrowing the
    /// demanding code sits under cannot hold there. A missing head
    /// consults nothing and keeps the honest refusal.
    fn branch_mentions_var(&self, instance: InstanceId, want: &str) -> bool {
        let Ok(head) = self.analyzed.decls.instances.head(instance) else {
            return true;
        };
        let universe = &self.analyzed.decls.universe;
        universe
            .members(head)
            .iter()
            .any(|&atom| carries_a_var(universe, atom, Some(want)))
    }

    /// The staged evidence words a call site entering a generic impl body
    /// stages for one of the body's demands: the checker-recorded bounds
    /// of this call site's solved dispatch answer the pair's bound — a
    /// concrete answer walks the serving chain, a `None` answer (the
    /// site sits inside a generic body whose substitution is that
    /// frame's own) reads the frame's evidence slots.
    fn evidence_words(
        &mut self,
        pair: &RuntimePair,
        bounds: &InherentCallBounds,
        entered: Option<InstanceId>,
        inherent_site: Option<Span>,
        recv: Option<Ty>,
        what: &'static str,
        span: Span,
    ) -> Result<Vec<EvidenceSrc>, LowerError> {
        let trait_id = self.pair_trait_id(pair, what, span)?;
        let universe = &self.analyzed.decls.universe;
        let env = &self.analyzed.decls.instances;
        // the recorded bounds key by the declaration's own minted variable;
        // the written parameter name bridges the two mintings
        let entry = bounds.iter().find(|(var, bound, _)| {
            crate::shape::var_param_name(universe, *var).as_deref() == Some(pair.var.as_str())
                && *bound == trait_id
        });
        let Some((_, _, served)) = entry else {
            // a demand whose variable the entering branch's head never
            // mentions: per-branch solving kept no condition for it, so
            // the demanding code sits under a narrowing that branch
            // cannot satisfy — the site stages the dead sentinel, a
            // chain word naming no object that a dispatch reads only if
            // the unreachable narrowing ever ran, trapping loudly there.
            // A union receiver (branches solving the variable
            // differently) and a head that does mention the variable
            // keep the honest refusal below.
            if let Some(instance) = entered
                && !self.branch_mentions_var(instance, &pair.var)
                && recv.is_some_and(|recv| self.members(recv).len() == 1)
            {
                return Ok(vec![EvidenceSrc::Dead]);
            }
            return Err(LowerError::unsupported(
                span,
                what,
                "the element type's bound has no unique serving instance at this site (or the \
                 receiver is a union across branches); runtime evidence resolves one body per \
                 bound",
            ));
        };
        let Some(instance) = served else {
            // the runtime form: this site's substitution is its own frame's
            let Some(recv) = recv else {
                return Err(LowerError::unsupported(
                    span,
                    what,
                    "the site's receiver type is missing; report the construct",
                ));
            };
            return self.slot_evidence(recv, pair, what, span);
        };
        let instance = *instance;
        // the chain start: the entered instance's own bound on this
        // pair's variable, substituted with the receiver this site
        // entered the body with
        let start = match (entered, inherent_site) {
            (Some(entered), _) => recv.and_then(|recv| {
                Self::chain_start(universe, env, entered, recv, &pair.var, trait_id)
            }),
            (None, Some(site)) => recv.and_then(|recv| {
                self.analyzed
                    .decls
                    .inherent_impl_bounds
                    .get(&site)
                    .and_then(|record| {
                        ds_check::instance_solve::inherent_evidence_step(
                            universe,
                            env,
                            record.head,
                            &record.conditions,
                            universe.members(recv),
                            Some(pair.var.as_str()),
                            trait_id,
                        )
                    })
                    .map(|(target, _)| target)
            }),
            (None, None) => None,
        }
        .map(Cow::Owned);
        Ok(vec![EvidenceSrc::Const(self.chain_for(
            pair.method,
            instance,
            start.as_deref(),
            what,
            span,
        )?)])
    }

    /// The staged evidence words a call into a free generic fn's body
    /// stages for one of the body's demands [H3]: the checker's
    /// per-call record answers the pair's bound under this site's own
    /// substitution — a concrete answer stages the serving chain
    /// object, and a `None` answer (the substitution is the calling
    /// frame's own variable) reads the frame's evidence area for the
    /// pair.
    fn free_evidence_words(
        &mut self,
        pair: &RuntimePair,
        record: &GenericCallBounds,
        what: &'static str,
        span: Span,
    ) -> Result<Vec<EvidenceSrc>, LowerError> {
        let trait_id = self.pair_trait_id(pair, what, span)?;
        let universe = &self.analyzed.decls.universe;
        let Some((_, _, served, solved)) = record.iter().find(|(var, bound, ..)| {
            crate::shape::var_param_name(universe, *var).as_deref() == Some(pair.var.as_str())
                && *bound == trait_id
        }) else {
            return Err(LowerError::unsupported(
                span,
                what,
                "the call site recorded no bound answer for the parameter; report the construct",
            ));
        };
        let Some(instance) = served else {
            return self.slot_evidence(*solved, pair, what, span);
        };
        let instance = *instance;
        // the chain start is the solved type itself: the demand's
        // variable takes it at this site, and the first tail chain
        // serves the body the serving instance's own head re-substitutes
        // it into
        let start = Some(Cow::Borrowed(universe.members(*solved)));
        Ok(vec![EvidenceSrc::Const(self.chain_for(
            pair.method,
            instance,
            start.as_deref(),
            what,
            span,
        )?)])
    }

    /// The one-hop chain-start substitution: the instance's declared
    /// bound on the pair's variable, solved for the receiver this site
    /// hands the body.
    fn chain_start(
        universe: &ds_check::Universe,
        env: &ds_check::instance::InstanceEnv,
        instance: InstanceId,
        recv: Ty,
        var: &str,
        trait_id: TraitId,
    ) -> Option<Vec<TypeId>> {
        ds_check::instance_solve::evidence_step(
            universe,
            env,
            instance,
            universe.members(recv),
            Some(var),
            trait_id,
        )
        .map(|(target, _)| target)
    }

    /// The one-word instantiation boundary of the runtime-evidence seam:
    /// the one generic body lowers a type variable's value to a single
    /// Raw word — the stride-1 element iteration, the binder, and every
    /// nested dispatch's staging carry that one word — while a serving
    /// body claims the substituted type's whole span as its receiver,
    /// so a multi-word substitution must refuse at the entering site
    /// (where the type is concrete), never slide at runtime. The
    /// whole-span vocabulary carries width-static spans alone (concrete
    /// call sites, signature-governed answers); the variable's width
    /// never reaches the frame, so no rider shape can carry it. Reboxing
    /// stays rejected, and per-instantiation frames are the JIT
    /// specialization tier's, never the interpreter's. A shape with no
    /// lowering answer refuses at its own staging, so an error here
    /// stays an error there.
    fn guard_one_word_instantiation(
        &mut self,
        members: &[TypeId],
        what: &'static str,
        span: Span,
    ) -> Result<(), LowerError> {
        let wide = matches!(self.shapes.member_words(members), Ok(ref words) if words.len() > 1);
        if wide {
            return Err(LowerError::unsupported(
                span,
                what,
                "the substituted type spans more than one word, while the one generic body lowers \
                 the variable's value to a single Raw word (the stride-1 element read, the \
                 binder, the nested dispatch's staging each carry that one word); reboxing stays \
                 rejected, and per-instantiation frames are the JIT specialization tier's, never \
                 the interpreter's",
            ));
        }
        Ok(())
    }

    /// Emits one dispatch site's evidence words into the overlay span at
    /// the recorded staged index: a constant word per Kimm store, a frame
    /// word per Mov — neither counted, neither a binder.
    fn stage_member_evidence(&mut self, words: &[EvidenceSrc], at: usize) {
        for (offset, word) in words.iter().enumerate() {
            let dst = Slot::overlay(u8::try_from(at + offset).expect("overlay word fits u8"));
            match word {
                EvidenceSrc::Const(index) => {
                    let kidx = self.pool(Constant::I64(i64::from(*index)));
                    self.emit(Op::Kimm { dst, kidx });
                },
                EvidenceSrc::Slot(slot) => {
                    self.emit(Op::Mov {
                        dst,
                        src: Slot::body(*slot),
                    });
                },
                EvidenceSrc::Dead => {
                    let kidx = self.pool(Constant::I64(DEAD_CHAIN));
                    self.emit(Op::Kimm { dst, kidx });
                },
            }
        }
    }

    fn resolve_callee(&self, def: DefId, span: Span) -> Result<Callee, LowerError> {
        if let Some(index) = self.plan.index_of(def) {
            return Ok(Callee::Local(index));
        }
        let name = self.analyzed.package.def(def).name.clone();
        if let Some(native) = crate::native::lookup(&name) {
            return Ok(Callee::Builtin(native));
        }
        if let Some(sig) = self.analyzed.decls.fns.get(&def) {
            if !sig.generics.is_empty() {
                if let Some(generic) = crate::native::lookup_generic(&name) {
                    return Ok(Callee::Generic(generic));
                }
                return Err(LowerError::unsupported(
                    span,
                    "a generic callee",
                    "the generic body did not plan (an extern, `var`-bound, or bodiless generic \
                     fn lowers nothing), so no dispatch reaches it",
                ));
            }
        }
        // the comptime slice carries only comptime functions: a call
        // into the package's runtime half refuses here, naming the seam
        if matches!(self.comptime, ComptimeRun::Slice) {
            return Err(LowerError::unsupported(
                span,
                "a call from a comptime function",
                "a comptime function may call the pure builtins and other comptime functions this \
                 round; a runtime helper is the slice seam",
            ));
        }
        Err(LowerError::unsupported(
            span,
            "a callee",
            "extern non-builtin and method callees are outside the contract",
        ))
    }

    /// The literal-only parameter defaults [F7] a local call stages
    /// for its omitted trailing parameters: the checker proved each
    /// default's literal against its parameter type, so the call lowers
    /// them exactly like written arguments. A non-defaulted omission is
    /// the checker's to reject; the call-site span guard names anything
    /// that still slips through.
    fn defaulted_args<'e>(
        &self,
        def: DefId,
        target: Callee,
        written: Vec<&'e Expr>,
    ) -> Vec<&'e Expr>
    where
        'a: 'e,
    {
        if !matches!(target, Callee::Local(_)) {
            return written;
        }
        let Some(sig) = self.analyzed.decls.fns.get(&def) else {
            return written;
        };
        let Some(missing) = sig.params.get(written.len()..) else {
            return written;
        };
        if !missing.iter().all(|param| param.default.is_some()) {
            return written;
        }
        let mut staged = written;
        staged.extend(
            missing
                .iter()
                .map(|param| param.default.as_ref().expect("checked directly above")),
        );
        staged
    }

    /// The literal-only parameter defaults [F7] a trait-method dispatch
    /// call stages at the call site for its omitted trailing parameters:
    /// the trait declaration is a defaulted member parameter's single
    /// authority, so the dot spelling and the qualified
    /// `Trait::method` spelling stage the trait member's registered
    /// defaults — the same literals the checker proved against the
    /// parameter types at this call site — exactly like written
    /// arguments. The staged exprs are span-preserving clones of the
    /// trait's declared literals, so the checker's span-keyed tables
    /// resolve them unchanged. A non-defaulted omission is the
    /// checker's to reject; the staged-width guard at the call names
    /// anything that slips through.
    fn trait_default_exprs(&self, method: MethodId, written: usize) -> Vec<Expr> {
        let Some(sig) = self.analyzed.decls.members.get(&method) else {
            return Vec::new();
        };
        let Some(missing) = sig.params.get(written..) else {
            return Vec::new();
        };
        if !missing.iter().all(|param| param.default.is_some()) {
            return Vec::new();
        }
        missing
            .iter()
            .map(|param| param.default.clone().expect("checked directly above"))
            .collect()
    }

    /// The literal-only parameter defaults [F7] an inherent-member call
    /// stages at the call site for its omitted trailing parameters: the
    /// checker proves each default's literal against the parameter type
    /// at the call (inherent members own their body defaults — no trait
    /// declaration exists), so the call lowers them exactly like written
    /// arguments — the same staging the free-fn spelling applies, over
    /// the impl body's own declaration (the plan entry the dispatch
    /// resolved). The staged exprs are span-preserving clones, so the
    /// checker's span-keyed tables resolve them unchanged. A default the
    /// checker never typed refuses naming the seam; a non-defaulted
    /// omission is the checker's to reject, and the staged-width guard
    /// at the call names anything that slips through.
    fn member_default_exprs(
        &self,
        index: u32,
        written: usize,
        span: Span,
    ) -> Result<Vec<Expr>, LowerError> {
        let Some(decl) = &self.plan.functions()[index as usize].decl else {
            return Ok(Vec::new());
        };
        let Some(missing) = decl.params.get(written..) else {
            return Ok(Vec::new());
        };
        if !missing.iter().all(|param| param.default.is_some()) {
            return Ok(Vec::new());
        }
        for param in missing {
            let default = param.default.as_ref().expect("checked directly above");
            if !self
                .analyzed
                .checked
                .expr_types
                .contains_key(&default.span())
            {
                return Err(LowerError::unsupported(
                    span,
                    "a defaulted trait-member parameter",
                    "the call site checks the trait's declared default while the staging lowers \
                     the impl body's own re-declared one; the two spellings disagree this round",
                ));
            }
        }
        Ok(missing
            .iter()
            .map(|param| {
                param
                    .default
                    .as_ref()
                    .expect("checked directly above")
                    .clone()
            })
            .collect())
    }

    /// A local callee's parameter word count, as its own frame layout
    /// records it (params first, one region per parameter). `None` when
    /// a parameter type has no shape; those callees keep the runtime
    /// arity check.
    /// A declared impl-member body's parameter words, from the checked
    /// binder facts keyed by each parameter's name span — the same table
    /// the body's own binding reads, so call sites and body agree.
    fn decl_param_words(&mut self, index: u32) -> Option<Vec<SlotType>> {
        let planned = self.plan.functions().get(index as usize)?;
        let decl = planned.decl.clone()?;
        let mut words = Vec::with_capacity(decl.params.len());
        for param in decl.params.iter() {
            let fact = self
                .analyzed
                .checked
                .binders
                .get(&param.name.span)
                .copied()?;
            words.extend_from_slice(&self.shapes.words(fact.ty).ok()?);
        }
        Some(words)
    }

    /// A local callee's per-parameter checked types, when its signature
    /// is static: a declared body's parameters come from the checked
    /// binder facts keyed by each parameter's name span, a def's from
    /// its recorded signature. `None` keeps the runtime arity check.
    fn callee_param_types(&mut self, index: u32) -> Option<Vec<Ty>> {
        let planned = self.plan.functions().get(index as usize)?;
        if let Some(decl) = &planned.decl {
            return decl
                .params
                .iter()
                .map(|param| {
                    self.analyzed
                        .checked
                        .binders
                        .get(&param.name.span)
                        .map(|fact| fact.ty)
                })
                .collect();
        }
        let def = planned.def?;
        let fn_ty = self.analyzed.checked.def_types.get(&def).copied()?;
        let (params, _) = fn_sig_of(&self.analyzed.decls.universe, fn_ty)?;
        Some(params.to_vec())
    }

    /// The declared parameter modes of one planned function (a declared
    /// body's or a lambda's own parameters, a def's recorded signature):
    /// a consuming (`move`) parameter's staging forwards its argument's
    /// count and any deinit claim (C2) — the checker's forced move makes
    /// the death test true at exactly that use. `None` when the body's
    /// parameters are not statically readable here.
    fn planned_param_modes(&self, index: u32) -> Option<Vec<ds_parse::ParamMode>> {
        let planned = self.plan.functions().get(index as usize)?;
        if let Some(decl) = &planned.decl {
            return Some(decl.params.iter().map(|param| param.mode).collect());
        }
        if let Some(def) = planned.def {
            let sig = self.analyzed.decls.fns.get(&def)?;
            return Some(sig.params.iter().map(|param| param.mode).collect());
        }
        if let Expr::Lambda { params, .. } = &planned.lambda {
            return Some(params.iter().map(|param| param.mode).collect());
        }
        None
    }

    /// Whether the parameter at one written-argument position consumes
    /// its argument (`move` mode, C2): the callee is a planned body (a
    /// local callee or a trait dispatch's impl body) or a runtime
    /// dispatch through the trait member's declared signature (impl
    /// bodies agree with the declaration). Natives carry no modes, and a
    /// position with no readable mode borrows.
    fn consuming_args(
        &self,
        target: Callee,
        dispatch: Option<&TraitDispatch>,
        args: usize,
    ) -> Vec<bool> {
        let modes: Option<Vec<ds_parse::ParamMode>> = match target {
            Callee::Local(index) => self.planned_param_modes(index),
            Callee::Evidence(_) => match dispatch {
                Some(TraitDispatch::Runtime { method, .. }) => self
                    .analyzed
                    .decls
                    .members
                    .get(method)
                    .map(|sig| sig.params.iter().map(|param| param.mode).collect()),
                _ => None,
            },
            Callee::Builtin(_) | Callee::Generic(_) => None,
        };
        match modes {
            Some(modes) => (0..args)
                .map(|index| matches!(modes.get(index), Some(ds_parse::ParamMode::Consume(_))))
                .collect(),
            None => vec![false; args],
        }
    }

    fn callee_param_words(&mut self, index: u32) -> Option<usize> {
        let planned = self.plan.functions().get(index as usize)?;
        if planned.decl.is_some() {
            return self.decl_param_words(index).map(|words| words.len());
        }
        let def = planned.def?;
        let fn_ty = self.analyzed.checked.def_types.get(&def).copied()?;
        let (params, _) = fn_sig_of(&self.analyzed.decls.universe, fn_ty)?;
        let mut total = 0;
        for ty in params {
            total += self.shapes.words(*ty).ok()?.len();
        }
        Some(total)
    }

    /// Whether one planned function's tail expression is a capturing
    /// lambda — a factory: its fn-typed result is the closure word the
    /// tail constructs, so a caller of it classifies the result slot Ref.
    fn tail_builds_a_closure(&self, index: u32) -> bool {
        let Expr::Lambda { body, .. } = &self.plan.functions()[index as usize].lambda else {
            return false;
        };
        let mut tail = &**body;
        loop {
            match tail {
                Expr::Paren { inner, .. } => tail = inner,
                // a declared fn's body is a block; only the lone
                // expression tail keeps the provenance pure
                Expr::Block(block) => match block.stmts.split_last() {
                    Some((
                        Stmt {
                            kind: StmtKind::Expr(e),
                            ..
                        },
                        [],
                    )) => tail = e,
                    _ => return false,
                },
                _ => break,
            }
        }
        let Expr::Lambda { .. } = tail else {
            return false;
        };
        let res = &self.analyzed.resolutions[tail.span().file.as_u32() as usize];
        !collect_captures(tail, res, &self.analyzed.checked.binders).is_empty()
    }

    /// The result word types a callee's return lands on the overlay as.
    /// A generic's instance-keyed result reads its word kind from the
    /// minted receiver instance (the gate already proved it is one the
    /// body implements), and a result-type-keyed one reads it from the
    /// call's own checked result type (`ty` — the folded element type
    /// `reduce` answers).
    fn callee_result_words(
        &mut self,
        target: Callee,
        instance: Option<BuiltinInstance>,
        ty: Ty,
    ) -> Result<SpanWords, LowerError> {
        match target {
            Callee::Builtin(native) => match native.shape {
                NativeShape::Total(slot) => Ok(Rc::from([slot])),
                // call_impl routes fallible and checked natives through
                // their own tag-test paths; this generic copy never sees
                // them
                NativeShape::Fallible { .. } => Err(mismatch("a fallible native result")),
                NativeShape::Checked => Err(mismatch("a checked-read native result")),
                NativeShape::TotalByInstance => Err(mismatch("an instance-keyed native result")),
                NativeShape::TotalByResultType => Err(mismatch("a result-keyed native result")),
                NativeShape::TaglessTotal => Err(mismatch("a tag-less-total native result")),
            },
            Callee::Generic(generic) => match generic.shape {
                NativeShape::Total(slot) => Ok(Rc::from([slot])),
                NativeShape::TotalByInstance => match instance {
                    Some(BuiltinInstance::Int) => Ok(Rc::from([SlotType::I64])),
                    Some(BuiltinInstance::Float) => Ok(Rc::from([SlotType::F64])),
                    _ => Err(mismatch("an instance-keyed generic result")),
                },
                // the tag-less total answers the call's own result
                // type's words with no discriminant (partition's
                // two-list tuple: two Ref words the destructuring binds)
                NativeShape::TotalByResultType | NativeShape::TaglessTotal => self.shapes.words(ty),
                NativeShape::Fallible { .. } | NativeShape::Checked => {
                    Err(mismatch("a generic builtin result"))
                },
            },
            Callee::Evidence(_) => self.shapes.words(ty),
            Callee::Local(index) => {
                let planned = &self.plan.functions()[index as usize];
                if planned.free_generic {
                    // the ONE body returns the type variable's single
                    // Raw word, so the site's solved answer must span
                    // one word too when the declared return is the
                    // variable — the same boundary the arguments guard
                    // (a trait's declared default body carries its solved
                    // return on the plan entry; no def signature names it)
                    let words = self.shapes.words(ty)?;
                    let declared = planned
                        .def
                        .and_then(|def| self.analyzed.checked.def_types.get(&def).copied())
                        .and_then(|fn_ty| fn_sig_of(&self.analyzed.decls.universe, fn_ty))
                        .map(|(_, ret)| ret)
                        .or(planned.ret);
                    if declared.is_some_and(|ret| self.is_var_ty(ret)) {
                        self.guard_one_word_instantiation(
                            self.members(ty),
                            "a generic call's result",
                            no_span(), // re-pointed at the call site
                        )?;
                    }
                    return Ok(words);
                }
                if let Some(ret) = planned.ret {
                    let words = self.shapes.words(ret)?;
                    if *words == [SlotType::Fn] && self.tail_builds_a_closure(index) {
                        return Ok(Rc::from([SlotType::Ref]));
                    }
                    return Ok(words);
                }
                let fn_ty = match planned.def {
                    Some(def) => self.analyzed.checked.def_types.get(&def).copied(),
                    None => self
                        .analyzed
                        .checked
                        .expr_types
                        .get(&planned.lambda.span())
                        .map(|val| val.ty),
                }
                .ok_or_else(|| mismatch("a callee signature"))?;
                let Some((_, ret)) = fn_sig_of(&self.analyzed.decls.universe, fn_ty) else {
                    return Err(mismatch("a callee signature"));
                };
                let words = self.shapes.words(ret)?;
                // A factory's fn-typed result is the closure its tail
                // lambda constructs — the one provenance this frame can
                // prove from the callee alone. Every other fn-typed
                // result keeps the scalar classification (a closure
                // returned through such a callee traps at the call — the
                // recorded return-flow seam).
                if *words == [SlotType::Fn] && self.tail_builds_a_closure(index) {
                    return Ok(Rc::from([SlotType::Ref]));
                }
                Ok(words)
            },
        }
    }

    /// One fallible native call: the two-word platform span (discriminant
    /// at overlay word 0, payload at word 1) lands like any call result.
    /// The success-typed ambient spelling types the call site as the
    /// failure union itself, so the span re-spans into that union per
    /// member ([`fn fallible_native_union`](Self::fallible_native_union));
    /// a total spelling (the ambient's `fs_list`/`write_file` rows)
    /// extracts the payload into the call's result temp, and failure
    /// prints `<prefix><message>` through println and halts on the
    /// discriminant — the M3 stand-in for the unhandled-failure handler
    /// the `error` row has no source syntax for.
    fn fallible_native_result(
        &mut self,
        native: &'static Native,
        ty: Ty,
        payload: SlotType,
        err_prefix: &'static str,
        staged: &[SlotType],
    ) -> Result<Place, LowerError> {
        if matches!(&*self.shapes.shape(ty)?, Shape::Union(_)) {
            return self.fallible_native_union(native, ty, payload, staged);
        }
        if *self.shapes.words(ty)? != [payload] {
            return Err(LowerError::unsupported(
                no_span(),
                "a fallible native call",
                "the checked result type's words and the native's payload disagree; list values \
                 have no runtime shape yet",
            ));
        }
        self.overlay_fallible(staged, payload);
        let kidx = self.pool(self.funcref_const(Callee::Builtin(native)));
        self.emit(Op::Call {
            callee: kidx,
            nargs: u8::try_from(staged.len()).expect("argument count fits u8"),
        });
        self.rc.native_done(&mut self.code);
        let ok_lbl = self.label();
        let fail_lbl = self.label();
        let cond = self.temp_word(SlotType::Bool)?;
        self.emit(Op::TagTest {
            dst: Slot::body(cond),
            src: Slot::overlay(0),
            tag: PLATFORM_IO_OK,
        });
        self.emit(Op::Jmpif {
            cond: Slot::body(cond),
            target: ok_lbl,
        });
        self.bind_label(fail_lbl);
        let discriminant = self.temp_word(SlotType::Tag)?;
        self.emit(Op::Mov {
            dst: Slot::body(discriminant),
            src: Slot::overlay(0),
        });
        let prefix = self.temp_word(SlotType::Ref)?;
        let kidx = self.pool_str(err_prefix);
        self.kstr(prefix, kidx);
        let message = self.temp_word(SlotType::Ref)?;
        self.emit(Op::Mov {
            dst: Slot::body(message),
            src: Slot::overlay(1),
        });
        self.rc.produced(message);
        let text = self.temp_word(SlotType::Ref)?;
        self.emit(Op::Data {
            op: DataOp::StrCat,
            dst: Slot::body(text),
            lhs: Slot::body(prefix),
            rhs: Slot::body(message),
        });
        self.rc.read(&mut self.code, &[prefix, message]);
        self.rc.produced_strcat(text);
        self.emit(Op::Mov {
            dst: Slot::overlay(0),
            src: Slot::body(text),
        });
        self.rc.native_open();
        self.rc.staged_native(text, false);
        let println = crate::native::lookup("println").expect("the console native is covered");
        let kidx = self.pool(self.funcref_const(Callee::Builtin(println)));
        self.emit(Op::Call {
            callee: kidx,
            nargs: 1,
        });
        self.rc.native_done(&mut self.code);
        // program end: HALT keeps its references — the heap dies whole
        self.emit(Op::Halt {
            slot: Slot::body(discriminant),
        });
        self.bind_label(ok_lbl);
        let base = self.alloc(&[payload])?;
        self.emit(Op::Mov {
            dst: Slot::body(base),
            src: Slot::overlay(1),
        });
        if payload == SlotType::Ref {
            self.rc.produced(base);
        }
        let shape = match &*self.shapes.shape(ty)? {
            Shape::Word(slot) => PlaceShape::Word(*slot),
            _ => PlaceShape::Span(ty),
        };
        Ok(Place { base, shape })
    }

    /// One fallible native call over the success-typed ambient spelling:
    /// the checked call type is the failure union itself (`string |
    /// io_err`, `int | parse_err`), so the two-word platform span
    /// re-spans into it — success constructs the payload member from
    /// overlay word 1, failure the one unit-tag error member — the same
    /// span-to-union discipline the checked reads use
    /// ([`fn checked_native_result`](Self::checked_native_result)). The
    /// union must carry exactly one member that holds the native's one
    /// payload word and exactly one payload-less error member beside it
    /// (the platform span answers one failure bit with no member
    /// mapping, so the pairing is by the checked call-site union's
    /// member shapes: the ambient's declaration owns the CLI-world
    /// spelling, and any other host's own extern row owns its members).
    /// The error member has no
    /// field for the failure message, so a `Ref` payload's native-born
    /// message count dies inside the failure arm; a scalar payload's
    /// failure word is inert (parse_int hands back the staged input's
    /// alias, a word the caller still owns and the arm never reads).
    fn fallible_native_union(
        &mut self,
        native: &'static Native,
        ty: Ty,
        payload: SlotType,
        staged: &[SlotType],
    ) -> Result<Place, LowerError> {
        let shape = self.shapes.shape(ty)?;
        let Shape::Union(u) = &*shape else {
            return Err(mismatch("a fallible native's union result"));
        };
        let success: Vec<_> = u
            .members
            .iter()
            .filter(|m| m.fields.len() == 1 && m.fields[0].slot() == payload)
            .collect();
        let failure: Vec<_> = u.members.iter().filter(|m| m.fields.is_empty()).collect();
        if success.len() + failure.len() != u.members.len() {
            return Err(LowerError::unsupported(
                no_span(),
                "a fallible native call",
                "the union's success member must be exactly the native's one payload word and \
                 every other member a payload-less error tag; wider success values and \
                 error-carried payloads have no re-span yet",
            ));
        }
        let [ok] = success.as_slice() else {
            return Err(LowerError::unsupported(
                no_span(),
                "a fallible native call",
                "the union must carry exactly one member that holds the native's payload word",
            ));
        };
        let [err] = failure.as_slice() else {
            return Err(LowerError::unsupported(
                no_span(),
                "a fallible native call",
                "the union must carry exactly one payload-less error member; the native answers \
                 one failure discriminant",
            ));
        };
        self.overlay_site(staged, &[SlotType::Tag, payload]);
        // the failure path lands a message `Ref` in word 1 whatever the
        // payload kind (io failures allocate one; parse_int hands back
        // the staged input's alias), so the word's classification mixes
        self.overlay_len = self.overlay_len.max(2);
        if self.overlay.len() < self.overlay_len {
            self.overlay.resize(self.overlay_len, HashSet::new());
        }
        self.overlay[1].insert(SlotType::Ref);
        let kidx = self.pool(self.funcref_const(Callee::Builtin(native)));
        self.emit(Op::Call {
            callee: kidx,
            nargs: u8::try_from(staged.len()).expect("argument count fits u8"),
        });
        self.rc.native_done(&mut self.code);
        let result = self.temp_span(ty)?;
        let ok_lbl = self.label();
        let join = self.label();
        let cond = self.temp_word(SlotType::Bool)?;
        self.emit(Op::TagTest {
            dst: Slot::body(cond),
            src: Slot::overlay(0),
            tag: PLATFORM_IO_OK,
        });
        self.emit(Op::Jmpif {
            cond: Slot::body(cond),
            target: ok_lbl,
        });
        // path-fork: failure and ok each write the span with their own
        // value
        let base = self.rc.save();
        let tag = self.shapes.tag_id(err.atom)?;
        self.emit(Op::TagNew {
            dst: Slot::body(result.base),
            tag,
        });
        self.zero_init_member(result.base, &u.words, &err.fields)?;
        if payload == SlotType::Ref {
            // the failure message's native-born count has no member field
            // to live in: adopt it off the overlay and drop it here
            let message = self.temp_word(SlotType::Ref)?;
            self.emit(Op::Mov {
                dst: Slot::body(message),
                src: Slot::overlay(1),
            });
            self.rc.produced(message);
            self.rc.discard(&mut self.code, &[message]);
        }
        let arm_err = self.rc.take();
        let err_jmp = self.code.len();
        self.emit(Op::Jmp { target: join });
        self.rc.restore(base);
        self.bind_label(ok_lbl);
        let tag = self.shapes.tag_id(ok.atom)?;
        self.emit(Op::TagNew {
            dst: Slot::body(result.base),
            tag,
        });
        self.zero_init_member(result.base, &u.words, &ok.fields)?;
        let [field] = ok.fields.as_slice() else {
            return Err(mismatch("a fallible native's payload member"));
        };
        self.emit(Op::Mov {
            dst: Slot::body(result.base + field.word),
            src: Slot::overlay(1),
        });
        if field.slot() == SlotType::Ref {
            self.rc.produced(result.base + field.word);
        }
        let arm_ok = self.rc.take();
        let dying = self.rc.reconcile(&[(arm_err, false), (arm_ok, false)]);
        if let [err, ok] = &dying[..] {
            let mut epilogue = Vec::new();
            Placement::emit_drops(&mut epilogue, err);
            self.splice(epilogue, err_jmp);
            self.arm_epilogue(ok);
        }
        self.bind_label(join);
        Ok(result)
    }

    /// Records the words a fallible native's result span lands on: word 0
    /// the discriminant plus the failure handler's println argument, word
    /// 1 the success payload plus the failure message.
    fn overlay_fallible(&mut self, staged: &[SlotType], payload: SlotType) {
        self.overlay_site(staged, &[SlotType::Tag, payload]);
        self.overlay_len = self.overlay_len.max(2);
        if self.overlay.len() < self.overlay_len {
            self.overlay.resize(self.overlay_len, HashSet::new());
        }
        self.overlay[0].insert(SlotType::Ref);
        self.overlay[1].insert(SlotType::Ref);
    }

    /// One checked-read native call (`list_get`, `map_get`): the ok/none
    /// span lands like a fallible native's, but neither side is a
    /// failure — it re-spans into the call's own `t | none` union: ok
    /// writes the element member with the payload's whole word span (the
    /// native channel: one overlay word per element word, a count per
    /// `Ref` word produced), none the predeclared unit member, so `?`,
    /// `.ok`, and match see an ordinary optional value. The element must
    /// be one atom; the rejected index the none side carries has no
    /// member field to surface in, so it drops. The generic spelling
    /// routes the same span through CALLG, the evidence words staged
    /// behind the real arguments.
    fn checked_native_result(
        &mut self,
        target: Callee,
        ty: Ty,
        staged: &[SlotType],
        real_args: usize,
    ) -> Result<Place, LowerError> {
        let shape = self.shapes.shape(ty)?;
        let Shape::Union(u) = &*shape else {
            return Err(LowerError::unsupported(
                no_span(),
                "a checked-read native call",
                "the call's result type must be the `t | none` union",
            ));
        };
        let none_member = u
            .member(TypeId::NONE)
            .ok_or_else(|| mismatch("a checked read's none member"))?;
        let element = set_diff(self.members(ty), &[TypeId::NONE]);
        let [elem] = element.as_slice() else {
            return Err(LowerError::unsupported(
                no_span(),
                "a checked-read native call",
                "the element type must be one atom; narrow list elements before the read",
            ));
        };
        let elem_member = u
            .member(*elem)
            .ok_or_else(|| mismatch("a checked read's element member"))?;
        // the ok payload's flat word span: the member's fields flattened
        // in declaration order — the native answers the element's whole
        // span, one overlay word per element word
        let elem_words: Vec<SlotType> = elem_member
            .fields
            .iter()
            .flat_map(|field| field.slots.iter().copied())
            .collect();
        let mut result_words = vec![SlotType::Tag];
        result_words.extend_from_slice(&elem_words);
        self.guard_staged_count(staged.len().max(result_words.len()))?;
        self.overlay_site(staged, &result_words);
        let kidx = self.pool(self.funcref_const(target));
        match target {
            // the staged span of a generic call ends in its evidence
            // words; nargs names the real arguments, the constraint list
            // carries the evidence count
            Callee::Generic(_) => {
                self.emit(Op::Callg {
                    callee: kidx,
                    nargs: u8::try_from(real_args).expect("argument count fits u8"),
                });
            },
            Callee::Builtin(_) => {
                self.emit(Op::Call {
                    callee: kidx,
                    nargs: u8::try_from(staged.len()).expect("argument count fits u8"),
                });
            },
            Callee::Local(_) | Callee::Evidence(_) => {
                return Err(mismatch("a checked-read callee"));
            },
        }
        self.rc.native_done(&mut self.code);
        let result = self.temp_span(ty)?;
        let ok_lbl = self.label();
        let join = self.label();
        let cond = self.temp_word(SlotType::Bool)?;
        self.emit(Op::TagTest {
            dst: Slot::body(cond),
            src: Slot::overlay(0),
            tag: PLATFORM_IO_OK,
        });
        self.emit(Op::Jmpif {
            cond: Slot::body(cond),
            target: ok_lbl,
        });
        // path-fork: none and ok each write the span with their own value
        let base = self.rc.save();
        let tag = self.shapes.tag_id(TypeId::NONE)?;
        self.emit(Op::TagNew {
            dst: Slot::body(result.base),
            tag,
        });
        self.zero_init_member(result.base, &u.words, &none_member.fields)?;
        let arm_none = self.rc.take();
        let none_jmp = self.code.len();
        self.emit(Op::Jmp { target: join });
        self.rc.restore(base);
        self.bind_label(ok_lbl);
        let tag = self.shapes.tag_id(*elem)?;
        self.emit(Op::TagNew {
            dst: Slot::body(result.base),
            tag,
        });
        self.zero_init_member(result.base, &u.words, &elem_member.fields)?;
        // the payload copies from the flat overlay span into the member's
        // claimed words (fields may claim scattered region offsets; the
        // flat index walks the element's declaration order)
        let mut flat = 0usize;
        for field in &elem_member.fields {
            for (offset, slot) in field.slots.iter().enumerate() {
                let word = result.base + field.word + u8::try_from(offset).expect("offset fits u8");
                self.emit(Op::Mov {
                    dst: Slot::body(word),
                    src: Slot::overlay(
                        u8::try_from(1 + flat + offset).expect("overlay word fits u8"),
                    ),
                });
                if *slot == SlotType::Ref {
                    self.rc.produced(word);
                }
            }
            flat += field.slots.len();
        }
        let arm_ok = self.rc.take();
        let dying = self.rc.reconcile(&[(arm_none, false), (arm_ok, false)]);
        if let [none, ok] = &dying[..] {
            // a dead Ref payload word keeps its zero-init claim: the miss
            // path decs its interned empty-string reference here (the
            // word is dead by construction — the live member's words are
            // the only ones ever read [BL-2])
            let mut epilogue = Vec::new();
            Placement::emit_drops(&mut epilogue, none);
            self.splice(epilogue, none_jmp);
            self.arm_epilogue(ok);
        }
        self.bind_label(join);
        Ok(result)
    }

    /// The `.err` handler: tag tests for the handled members jump to an
    /// inlined handler body; the success value widens into the result.
    fn err_value(&mut self, recv: &Expr, lambda: &Expr, ty: Ty) -> Result<Place, LowerError> {
        let src = self.value(recv)?;
        let src_ty = self.val(recv.span())?.ty;
        let Expr::Lambda { params, body, .. } = lambda else {
            return Err(LowerError::unsupported(
                lambda.span(),
                "a non-lambda handler",
                "`.err` takes a lambda that inlines at the call site",
            ));
        };
        let handled = set_diff(self.members(src_ty), self.members(ty));
        let result = self.temp_span(ty)?;
        let handler_lbl = self.label();
        let join = self.label();
        for member in &handled {
            let cond = self.temp_word(SlotType::Bool)?;
            let src_word = self.tag_addr(&src.place)?;
            let tag = self.shapes.tag_id(*member)?;
            self.emit(Op::TagTest {
                dst: Slot::body(cond),
                src: Slot::body(src_word),
                tag,
            });
            self.emit(Op::Jmpif {
                cond: Slot::body(cond),
                target: handler_lbl,
            });
        }
        // success: the kept members re-span into the result over the
        // operand's real layout — an operand's raw span never lands
        // directly (union regions are per-union-type), and the kept
        // set's region can be narrower than the operand's when a
        // handled member contributed a word shape the kept set does
        // not need
        let base = self.rc.save();
        let kept = set_diff(self.members(src_ty), &handled);
        if kept.len() == 1 && !self.is_span_ty(ty) {
            // the result is the one kept atom itself: claiming that
            // atom is honest — the word-address path extracts the
            // member's payload words out of the operand's layout
            self.widen_into(
                &result,
                ty,
                &TypedPlace {
                    place: src.place.clone(),
                    ty,
                },
                recv.span(),
            )?;
        } else {
            let labels: Vec<usize> = kept.iter().map(|_| self.label()).collect();
            for (member, label) in kept.iter().zip(&labels) {
                let cond = self.temp_word(SlotType::Bool)?;
                let src_word = self.tag_addr(&src.place)?;
                let tag = self.shapes.tag_id(*member)?;
                self.emit(Op::TagTest {
                    dst: Slot::body(cond),
                    src: Slot::body(src_word),
                    tag,
                });
                self.emit(Op::Jmpif {
                    cond: Slot::body(cond),
                    target: *label,
                });
            }
            // dead fallthrough: the handled tests above cover every
            // other member of the operand's type
            self.emit(Op::Jmp { target: join });
            for (member, label) in kept.iter().zip(labels) {
                self.bind_label(label);
                self.respan_into(&result, ty, &src.place, *member)?;
            }
        }
        let arm_ok = self.rc.take();
        let ok_div = self.rc.diverged();
        let ok_jmp = self.code.len();
        self.emit(Op::Jmp { target: join });
        self.rc.restore(base);
        self.bind_label(handler_lbl);
        {
            self.scope_push();
            let [param] = params.as_slice() else {
                return Err(mismatch("a handler parameter"));
            };
            if !matches!(param.mode, ds_parse::ParamMode::Borrow) {
                return Err(LowerError::unsupported(
                    param.span,
                    "a mode on a handler parameter",
                    "the inlined handler binds its parameter as an alias of the payload word; the \
                     extraction seam owns no claim hand-off yet",
                ));
            }
            self.binders.insert(param.name.span, Place {
                base: src.place.base,
                shape: src.place.shape.clone(),
            });
            let param_ty = self
                .analyzed
                .checked
                .binders
                .get(&param.name.span)
                .map_or(src_ty, |f| f.ty);
            self.rc_bind_borrowed(param.name.span, src.place.base, param_ty)?;
            self.scope_insert(&param.name.name, param.name.span);
            let body_tp = self.value(body)?;
            self.refuse_fn_word_answer(
                ty,
                &body_tp,
                body.span(),
                "a capturing closure in a handler's answer",
                "an unproven callable in a handler's answer",
            )?;
            self.widen_into(&result, ty, &body_tp, body.span())?;
            self.scope_pop();
        }
        let arm_err = self.rc.take();
        let err_div = self.rc.diverged();
        let dying = self.rc.reconcile(&[(arm_ok, ok_div), (arm_err, err_div)]);
        if let [ok, err] = &dying[..] {
            let mut epilogue = Vec::new();
            Placement::emit_drops(&mut epilogue, ok);
            self.splice(epilogue, ok_jmp);
            self.arm_epilogue(err);
        }
        self.bind_label(join);
        Ok(result)
    }

    /// The `.ok` handler: the mirror of `.err` — the ok-side members
    /// jump to an inlined handler body, and each remainder member
    /// re-spans into the result, which carries the difference side
    /// composed with the handler's own result.
    fn ok_value(&mut self, recv: &Expr, lambda: &Expr, ty: Ty) -> Result<Place, LowerError> {
        let src = self.value(recv)?;
        let src_ty = self.val(recv.span())?.ty;
        let Expr::Lambda { params, body, .. } = lambda else {
            return Err(LowerError::unsupported(
                lambda.span(),
                "a non-lambda handler",
                "`.ok` takes a lambda that inlines at the call site",
            ));
        };
        let [param] = params.as_slice() else {
            return Err(mismatch("a handler parameter"));
        };
        if !matches!(param.mode, ds_parse::ParamMode::Borrow) {
            return Err(LowerError::unsupported(
                param.span,
                "a mode on a handler parameter",
                "the inlined handler binds its parameter as an alias of the payload word; the \
                 extraction seam owns no claim hand-off yet",
            ));
        }
        let handled = self.ok_side(param, body, src_ty)?;
        let result = self.temp_span(ty)?;
        let handler_lbl = self.label();
        let join = self.label();
        for member in &handled {
            let cond = self.temp_word(SlotType::Bool)?;
            let src_word = self.tag_addr(&src.place)?;
            let tag = self.shapes.tag_id(*member)?;
            self.emit(Op::TagTest {
                dst: Slot::body(cond),
                src: Slot::body(src_word),
                tag,
            });
            self.emit(Op::Jmpif {
                cond: Slot::body(cond),
                target: handler_lbl,
            });
        }
        // the difference side flows into the result; the fall-through and
        // the handler arm fork the ownership map
        let base = self.rc.save();
        let remainder = set_diff(self.members(src_ty), &handled);
        let mut flow_jmps = Vec::new();
        if remainder.is_empty() {
            // every member jumps to the handler; the fallthrough is dead
            self.emit(Op::Jmp { target: join });
        } else if self.members(src_ty).len() == 1 {
            // a single-member operand: the value itself flows, no tags
            self.widen_into(
                &result,
                ty,
                &TypedPlace {
                    place: src.place.clone(),
                    ty: src_ty,
                },
                recv.span(),
            )?;
            flow_jmps.push(self.code.len());
            self.emit(Op::Jmp { target: join });
        } else {
            let labels: Vec<usize> = remainder.iter().map(|_| self.label()).collect();
            for (member, label) in remainder.iter().zip(&labels) {
                let cond = self.temp_word(SlotType::Bool)?;
                let src_word = self.tag_addr(&src.place)?;
                let tag = self.shapes.tag_id(*member)?;
                self.emit(Op::TagTest {
                    dst: Slot::body(cond),
                    src: Slot::body(src_word),
                    tag,
                });
                self.emit(Op::Jmpif {
                    cond: Slot::body(cond),
                    target: *label,
                });
            }
            // dead: the tests cover every member of the operand's type
            self.emit(Op::Jmp { target: join });
            for (member, label) in remainder.iter().zip(labels) {
                self.bind_label(label);
                self.respan_into(&result, ty, &src.place, *member)?;
                flow_jmps.push(self.code.len());
                self.emit(Op::Jmp { target: join });
            }
        }
        self.bind_label(handler_lbl);
        let arm_flow = self.rc.take();
        let flow_div = self.rc.diverged();
        self.rc.restore(base);
        {
            self.scope_push();
            self.binders.insert(param.name.span, Place {
                base: src.place.base,
                shape: src.place.shape.clone(),
            });
            let param_ty = self
                .analyzed
                .checked
                .binders
                .get(&param.name.span)
                .map_or(src_ty, |f| f.ty);
            self.rc_bind_borrowed(param.name.span, src.place.base, param_ty)?;
            self.scope_insert(&param.name.name, param.name.span);
            let body_tp = self.value(body)?;
            self.refuse_fn_word_answer(
                ty,
                &body_tp,
                body.span(),
                "a capturing closure in a handler's answer",
                "an unproven callable in a handler's answer",
            )?;
            self.widen_into(&result, ty, &body_tp, body.span())?;
            self.scope_pop();
        }
        let arm_ok = self.rc.take();
        let ok_div = self.rc.diverged();
        let dying = self.rc.reconcile(&[(arm_flow, flow_div), (arm_ok, ok_div)]);
        if let [flow, ok] = &dying[..] {
            let mut shift = 0;
            for at in &flow_jmps {
                let mut epilogue = Vec::new();
                Placement::emit_drops(&mut epilogue, flow);
                let len = epilogue.len();
                self.splice(epilogue, at + shift);
                shift += len;
            }
            self.arm_epilogue(ok);
        }
        self.bind_label(join);
        Ok(result)
    }

    /// The `.ok` handler's ok side, the checker's handled-set rule: the
    /// annotated parameter's members, or — unannotated — everything the
    /// handler's own result does not keep of the operand. Foreign
    /// annotated members never test at runtime, so they drop here.
    fn ok_side(
        &mut self,
        param: &Param,
        body: &Expr,
        src_ty: Ty,
    ) -> Result<Vec<TypeId>, LowerError> {
        let src_members = self.members(src_ty);
        if param.ty.is_some() {
            let fact = self
                .analyzed
                .checked
                .binders
                .get(&param.name.span)
                .copied()
                .ok_or_else(|| mismatch("a handler parameter"))?;
            let annotated = self.members(fact.ty);
            return Ok(src_members
                .iter()
                .copied()
                .filter(|m| annotated.contains(m))
                .collect());
        }
        let body_members = self.members(self.val(body.span())?.ty);
        Ok(src_members
            .iter()
            .copied()
            .filter(|m| !body_members.contains(m))
            .collect())
    }

    // ---- the extraction family [E4] ------------------------------------------

    /// One extraction-family call [E4]: every member inlines at the call
    /// site over the checked receiver's own union shape — a tag test
    /// picks the arm, the chosen side writes the result. No callee
    /// exists, so a tail position lowers the produced place through the
    /// value walk's ordinary return.
    fn extraction_value(
        &mut self,
        kind: Extraction,
        args: &[&Expr],
        ty: Ty,
        span: Span,
    ) -> Result<Place, LowerError> {
        let [recv, arg] = args else {
            return Err(mismatch("an extraction-family call's arguments"));
        };
        match kind {
            Extraction::UnwrapOr => self.unwrap_or_value(recv, arg, ty, span),
            Extraction::UnwrapOrElse => self.unwrap_or_else_value(recv, arg, ty, span),
            Extraction::OkOr => self.ok_or_value(recv, arg, ty, span),
            // `map_err` is the `.ok` handler over a named spelling: the
            // handled members jump to the inlined lambda, the remainder
            // re-spans, the handler's result unions in.
            Extraction::MapErr => self.ok_value(recv, arg, ty),
        }
    }

    /// The family's receiver decomposition: the checked `t | none`
    /// union's none member and its extracted t side's atom. The side's
    /// words may span any width — a struct member's fields claim their
    /// region spans — and the consumers gather them through
    /// [`fn extracted_words`](Self::extracted_words).
    fn optional_side(&mut self, recv_ty: Ty, span: Span) -> Result<TypeId, LowerError> {
        let shape = self.shapes.shape(recv_ty)?;
        let Shape::Union(u) = &*shape else {
            return Err(LowerError::unsupported(
                span,
                "an extraction-family receiver",
                "the receiver must be the `t | none` union",
            ));
        };
        if u.member(TypeId::NONE).is_none() {
            return Err(mismatch("an optional receiver's none member"));
        }
        let diff = set_diff(self.members(recv_ty), &[TypeId::NONE]);
        let [elem] = diff.as_slice() else {
            return Err(LowerError::unsupported(
                span,
                "an extraction-family receiver",
                "the extracted side must be one atom; narrow the receiver first",
            ));
        };
        Ok(*elem)
    }

    /// The extracted side's word addresses inside the receiver's union
    /// span: the element type's own field map over the span place, so a
    /// member whose fields claim scattered region offsets gathers
    /// correctly — the same discipline the checked read's ok arm copies
    /// by.
    fn extracted_words(&mut self, src: &Place, elem: Ty) -> Result<Vec<WordAddr>, LowerError> {
        self.word_addrs(src, elem)
    }

    /// `x.unwrap_or(y)` [E4]: the eager fallback — both operands evaluate,
    /// the receiver's tag picks the payload word or the fallback's.
    fn unwrap_or_value(
        &mut self,
        recv: &Expr,
        fallback: &Expr,
        ty: Ty,
        span: Span,
    ) -> Result<Place, LowerError> {
        let src = self.value(recv)?;
        let src_ty = self.val(recv.span())?.ty;
        self.optional_side(src_ty, span)?;
        let fb = self.value(fallback)?;
        let result = self.temp_span(ty)?;
        let cond = self.temp_word(SlotType::Bool)?;
        let src_word = self.tag_addr(&src.place)?;
        let none_tag = self.shapes.tag_id(TypeId::NONE)?;
        self.emit(Op::TagTest {
            dst: Slot::body(cond),
            src: Slot::body(src_word),
            tag: none_tag,
        });
        let ok_lbl = self.label();
        let join = self.label();
        self.emit(Op::JmpifNot {
            cond: Slot::body(cond),
            target: ok_lbl,
        });
        // path-fork: the fallback and the payload each own the result
        let base = self.rc.save();
        self.refuse_fn_word_answer(
            ty,
            &fb,
            fallback.span(),
            "a capturing closure in unwrap_or's fallback",
            "an unproven callable in unwrap_or's fallback",
        )?;
        self.widen_into(
            &result,
            ty,
            &TypedPlace {
                place: fb.place.clone(),
                ty: fb.ty,
            },
            span,
        )?;
        let arm_none = self.rc.take();
        let none_div = self.rc.diverged();
        let none_jmp = self.code.len();
        self.emit(Op::Jmp { target: join });
        self.rc.restore(base);
        self.bind_label(ok_lbl);
        // the payload gathers out of the receiver's union span (fields may
        // claim scattered offsets) into the result's contiguous words
        for (index, addr) in self.extracted_words(&src.place, ty)?.iter().enumerate() {
            let word = result.base + u8::try_from(index).expect("span width fits u8");
            self.emit(Op::Mov {
                dst: Slot::body(word),
                src: Slot::body(addr.slot),
            });
            if addr.slot_ty == SlotType::Ref {
                self.rc.copied(&mut self.code, word, addr.slot);
            }
        }
        let arm_ok = self.rc.take();
        let dying = self.rc.reconcile(&[(arm_none, none_div), (arm_ok, false)]);
        if let [none, ok] = &dying[..] {
            let mut epilogue = Vec::new();
            Placement::emit_drops(&mut epilogue, none);
            self.splice(epilogue, none_jmp);
            self.arm_epilogue(ok);
        }
        self.bind_label(join);
        Ok(result)
    }

    /// `x.unwrap_or_else(fn)` [E4]: the fallback lambda inlines on the
    /// none side only — lazy by construction; the ok side moves the
    /// payload.
    fn unwrap_or_else_value(
        &mut self,
        recv: &Expr,
        fallback: &Expr,
        ty: Ty,
        span: Span,
    ) -> Result<Place, LowerError> {
        let src = self.value(recv)?;
        let src_ty = self.val(recv.span())?.ty;
        self.optional_side(src_ty, span)?;
        let Expr::Lambda { params, body, .. } = fallback else {
            return Err(LowerError::unsupported(
                fallback.span(),
                "a non-lambda fallback",
                "`unwrap_or_else` takes a lambda that inlines at the call site",
            ));
        };
        if !params.is_empty() {
            return Err(mismatch("a fallback lambda's parameters"));
        }
        let result = self.temp_span(ty)?;
        let cond = self.temp_word(SlotType::Bool)?;
        let src_word = self.tag_addr(&src.place)?;
        let none_tag = self.shapes.tag_id(TypeId::NONE)?;
        self.emit(Op::TagTest {
            dst: Slot::body(cond),
            src: Slot::body(src_word),
            tag: none_tag,
        });
        let ok_lbl = self.label();
        let join = self.label();
        self.emit(Op::JmpifNot {
            cond: Slot::body(cond),
            target: ok_lbl,
        });
        // path-fork: the inlined lambda and the payload own the result
        let base = self.rc.save();
        self.scope_push();
        let body_tp = self.value(body)?;
        self.refuse_fn_word_answer(
            ty,
            &body_tp,
            body.span(),
            "a capturing closure in unwrap_or_else's fallback",
            "an unproven callable in unwrap_or_else's fallback",
        )?;
        self.widen_into(&result, ty, &body_tp, body.span())?;
        self.scope_pop();
        let arm_none = self.rc.take();
        let none_div = self.rc.diverged();
        let none_jmp = self.code.len();
        self.emit(Op::Jmp { target: join });
        self.rc.restore(base);
        self.bind_label(ok_lbl);
        // the payload gathers out of the receiver's union span like
        // `unwrap_or`'s ok arm
        for (index, addr) in self.extracted_words(&src.place, ty)?.iter().enumerate() {
            let word = result.base + u8::try_from(index).expect("span width fits u8");
            self.emit(Op::Mov {
                dst: Slot::body(word),
                src: Slot::body(addr.slot),
            });
            if addr.slot_ty == SlotType::Ref {
                self.rc.copied(&mut self.code, word, addr.slot);
            }
        }
        let arm_ok = self.rc.take();
        let dying = self.rc.reconcile(&[(arm_none, none_div), (arm_ok, false)]);
        if let [none, ok] = &dying[..] {
            let mut epilogue = Vec::new();
            Placement::emit_drops(&mut epilogue, none);
            self.splice(epilogue, none_jmp);
            self.arm_epilogue(ok);
        }
        self.bind_label(join);
        Ok(result)
    }

    /// `x.ok_or(e)` [E4]: the tag test picks the constructed member of
    /// the call's own `t | e` union — the error value on the none side,
    /// the payload on the ok side — which `?` then propagates.
    fn ok_or_value(
        &mut self,
        recv: &Expr,
        err: &Expr,
        ty: Ty,
        span: Span,
    ) -> Result<Place, LowerError> {
        // The degenerate spell: the err value's own type sits inside the
        // receiver's extracted side, so the checked `t | e` collapses to
        // the extracted type and both arms answer one value of it —
        // `unwrap_or`'s exact shape, the err value as the fallback.
        if self
            .shapes
            .shape(ty)
            .is_ok_and(|shape| !matches!(*shape, Shape::Union(_)))
        {
            return self.unwrap_or_value(recv, err, ty, span);
        }
        let src = self.value(recv)?;
        let src_ty = self.val(recv.span())?.ty;
        let elem = self.optional_side(src_ty, span)?;
        let ev = self.value(err)?;
        let err_atom = self.only_atom(ev.ty, err.span())?;
        let err_words = self.word_addrs(&ev.place, ev.ty)?;
        // the err arm stages its words directly into the result member's
        // fields, so the union payload's provenance guard runs here
        // instead of the widening write
        self.refuse_union_fn_payload(&ev, err.span())?;
        let result = self.temp_span(ty)?;
        let shape = self.shapes.shape(ty)?;
        let Shape::Union(u) = &*shape else {
            return Err(mismatch("an ok_or result union"));
        };
        // the payload's gathered addresses inside the receiver's span: the
        // elem member's fields, scattered region offsets included
        let src_shape = self.shapes.shape(src_ty)?;
        let Shape::Union(src_u) = &*src_shape else {
            return Err(mismatch("an ok_or receiver union"));
        };
        let src_elem = src_u
            .member(elem)
            .ok_or_else(|| mismatch("an ok_or receiver's extracted member"))?;
        let elem_words: Vec<WordAddr> = src_elem
            .fields
            .iter()
            .flat_map(|field| field_addrs(src.place.base, field))
            .collect();
        let none_tag = self.shapes.tag_id(TypeId::NONE)?;
        let err_tag = self.shapes.tag_id(err_atom)?;
        let elem_tag = self.shapes.tag_id(elem)?;
        let err_member = u
            .member(err_atom)
            .ok_or_else(|| mismatch("an ok_or error member"))?;
        let elem_dst = u
            .member(elem)
            .ok_or_else(|| mismatch("an ok_or result's extracted member"))?;
        let cond = self.temp_word(SlotType::Bool)?;
        let src_word = self.tag_addr(&src.place)?;
        self.emit(Op::TagTest {
            dst: Slot::body(cond),
            src: Slot::body(src_word),
            tag: none_tag,
        });
        let ok_lbl = self.label();
        let join = self.label();
        self.emit(Op::JmpifNot {
            cond: Slot::body(cond),
            target: ok_lbl,
        });
        // path-fork: the constructed error member and the payload
        let base = self.rc.save();
        self.emit(Op::TagNew {
            dst: Slot::body(result.base),
            tag: err_tag,
        });
        self.zero_init_member(result.base, &u.words, &err_member.fields)?;
        let srcs = ordered_field_srcs(&err_words, &err_member.fields)?;
        for (dst_field, srcs) in err_member.fields.iter().zip(srcs) {
            self.store_field_span(result.base, dst_field, srcs);
        }
        let arm_err = self.rc.take();
        let err_div = self.rc.diverged();
        let err_jmp = self.code.len();
        self.emit(Op::Jmp { target: join });
        self.rc.restore(base);
        self.bind_label(ok_lbl);
        self.emit(Op::TagNew {
            dst: Slot::body(result.base),
            tag: elem_tag,
        });
        self.zero_init_member(result.base, &u.words, &elem_dst.fields)?;
        // the payload stores per field like the err arm's, gathered out of
        // the receiver's span (scattered offsets name-match by field)
        let srcs = ordered_field_srcs(&elem_words, &elem_dst.fields)?;
        for (dst_field, srcs) in elem_dst.fields.iter().zip(srcs) {
            self.store_field_span(result.base, dst_field, srcs);
        }
        let arm_ok = self.rc.take();
        let dying = self.rc.reconcile(&[(arm_err, err_div), (arm_ok, false)]);
        if let [err, ok] = &dying[..] {
            let mut epilogue = Vec::new();
            Placement::emit_drops(&mut epilogue, err);
            self.splice(epilogue, err_jmp);
            self.arm_epilogue(ok);
        }
        self.bind_label(join);
        Ok(result)
    }

    /// `e?`: every propagated member re-spans into the enclosing return
    /// union via RETSPAN [E4]; the kept set extracts in place. Each exit
    /// forks the ownership map — the exit's own drops fire from its
    /// snapshot, and the continuation keeps the kept set's counts (the
    /// exits diverge, so they constrain nothing after the `?`).
    fn try_value(&mut self, e: &Expr, ty: Ty) -> Result<Place, LowerError> {
        let Expr::Try { expr, .. } = e else {
            return Err(mismatch("a `?` propagation"));
        };
        let src = self.value(expr)?;
        let src_ty = self.val(expr.span())?.ty;
        let propagated = set_diff(self.members(src_ty), self.members(ty));
        let continue_lbl = self.label();
        let mut exits = Vec::with_capacity(propagated.len());
        for member in &propagated {
            let exit = self.label();
            let cond = self.temp_word(SlotType::Bool)?;
            let src_word = self.tag_addr(&src.place)?;
            let tag = self.shapes.tag_id(*member)?;
            self.emit(Op::TagTest {
                dst: Slot::body(cond),
                src: Slot::body(src_word),
                tag,
            });
            self.emit(Op::Jmpif {
                cond: Slot::body(cond),
                target: exit,
            });
            exits.push(exit);
        }
        self.emit(Op::Jmp {
            target: continue_lbl,
        });
        let base = self.rc.save();
        for (member, exit) in propagated.iter().zip(exits) {
            self.bind_label(exit);
            self.respan_exit(&src.place, *member)?;
            // the exit dropped from its own fork of the post-call state;
            // the next exit forks from the same place
            self.rc.restore(base.clone());
        }
        self.bind_label(continue_lbl);
        // the kept set: one member extracts to its payload word
        self.extract(src.place, ty)
    }

    /// A `?` in tail position: the checker proved the operand union is
    /// contained in the return union, so every member returns through
    /// the return itself — one member is a plain return, several
    /// dispatch by tag and each re-spans into the return union, leaving
    /// no live fall-through.
    fn try_tail(&mut self, operand: &Expr) -> Result<(), LowerError> {
        let src = self.value(operand)?;
        let src_ty = self.val(operand.span())?.ty;
        let members = self.members(src_ty);
        match members {
            [] => Err(mismatch("a `?` over an empty operand union")),
            [_] => self.emit_return(&src.place, src_ty, operand.span()),
            _ => {
                let mut exits = Vec::with_capacity(members.len());
                for member in members {
                    let exit = self.label();
                    let cond = self.temp_word(SlotType::Bool)?;
                    let src_word = self.tag_addr(&src.place)?;
                    let tag = self.shapes.tag_id(*member)?;
                    self.emit(Op::TagTest {
                        dst: Slot::body(cond),
                        src: Slot::body(src_word),
                        tag,
                    });
                    self.emit(Op::Jmpif {
                        cond: Slot::body(cond),
                        target: exit,
                    });
                    exits.push(exit);
                }
                for (member, exit) in members.iter().zip(exits) {
                    self.bind_label(exit);
                    self.respan_exit(&src.place, *member)?;
                }
                Ok(())
            },
        }
    }

    /// The kept set of a `?` extraction: one member addresses its
    /// payload word; several keep the whole span.
    fn extract(&mut self, place: Place, ty: Ty) -> Result<Place, LowerError> {
        if self.members(ty).len() == 1 {
            let addrs = self.word_addrs(&place, ty)?;
            let [only] = addrs.as_slice() else {
                return Err(mismatch("an extracted member"));
            };
            Ok(Place {
                base: only.slot,
                shape: PlaceShape::Word(only.slot_ty),
            })
        } else {
            Ok(place)
        }
    }

    // ---- tail position and returns ---------------------------------------------

    /// Whether a call can transfer control in tail position [H2]: a
    /// name call, a UFCS dot-call onto a resolved free function [F3],
    /// or a member dot-call the checker solved to a monomorphic
    /// inherent body — its mangled `<target>#<member>` identity plans
    /// as a plain local callee, so the CALLT transfer applies. A
    /// trait-method site dispatches through CALLG (CALLTG stays
    /// reserved) and a runtime-evidence site resolves at run, so
    /// neither tails; their tail position lowers in place. Handlers
    /// inline, so `.err` stays a value-position call.
    fn is_tail_callee(&self, call: &Expr) -> bool {
        let Expr::Call {
            callee,
            span: call_span,
            ..
        } = call
        else {
            return false;
        };
        match &**callee {
            // A minted case constructor produces a value in place; it
            // never transfers control, so tail position lowers it as a
            // plain value [B3]. The extraction family inlines too, so a
            // tail-position family call lowers through the value walk's
            // return, not a control transfer [E4].
            Expr::Path { span, .. } => {
                // A call through a local binding reaches the maybe-Ref
                // callee tracking: the tail transfers through CALLT when
                // the callee word is provably count-free (a scalar naming
                // one function), and lowers in place (CALLS plus return)
                // where tracking cannot prove it [H2's closure ruling]
                !matches!(
                    self.res.get(*span),
                    Some(Res::Def(def))
                        if self.extraction_def(*def).is_some()
                            || self.ambient_str_def(*def)
                            || self
                                .analyzed
                                .decls
                                .cases
                                .get(def)
                                .is_some_and(|c| !matches!(c.payload, EnumPayload::None))
                )
            },
            Expr::Field { recv, name, .. } => match self.res.get(name.span) {
                Some(Res::Dot(DotRes::Free { func, .. })) => self.extraction_def(*func).is_none(),
                Some(Res::Dot(DotRes::Methods { candidates })) => matches!(
                    self.resolve_method_dispatch(*call_span, name.span, Some(recv), candidates),
                    Ok(TraitDispatch::Inherent { .. }
                        | TraitDispatch::Static { .. }
                        | TraitDispatch::Runtime { .. })
                ),
                Some(Res::Dot(DotRes::Static { candidates })) => matches!(
                    self.resolve_method_dispatch(*call_span, name.span, None, candidates),
                    Ok(TraitDispatch::Inherent { .. })
                ),
                _ => false,
            },
            _ => false,
        }
    }

    /// The extraction-family member a callee def addresses, when the
    /// ambient's own shape carries it: a non-local function of a family
    /// name, generic with two parameters — the same name-routing the
    /// native table performs, gated to match the checker's rule so the
    /// two passes never disagree [E4].
    /// Whether `def` is the ambient `str[t]` conversion extern: one
    /// generic over the converted value, one parameter. The lowering
    /// routes the call by the operand's checked type, mirroring the
    /// interpolation hole.
    fn ambient_str_def(&self, def: DefId) -> bool {
        self.plan.index_of(def).is_none()
            && self.analyzed.package.def(def).name == "str"
            && self
                .analyzed
                .decls
                .fns
                .get(&def)
                .is_some_and(|sig| sig.generics.len() == 1 && sig.params.len() == 1)
    }

    /// Whether `def` is the ambient `cancel` extern [E1, E2]: the generic
    /// one-parameter declaration the checker rows with the `cancel`
    /// effect — the internal effect the nursery boundary dispatches.
    fn ambient_cancel_def(&self, def: DefId) -> bool {
        self.plan.index_of(def).is_none()
            && self.analyzed.package.def(def).name == "cancel"
            && self
                .analyzed
                .decls
                .fns
                .get(&def)
                .is_some_and(|sig| sig.generics.len() == 1 && sig.params.len() == 1)
    }

    /// The `cancel(e)` call [E1, E2]: the operand is one bare enum member
    /// value — the failing fiber's error payload — contained in the
    /// enclosing function's return union, where the nursery boundary
    /// unions it out at the join. The seam traps the running fiber, so
    /// the call never answers; the unit place carries the value walk.
    fn cancel_call(&mut self, args: &[&Expr], span: Span) -> Result<Place, LowerError> {
        let native = crate::native::lookup("nursery_cancel")
            .ok_or_else(|| mismatch("the nursery cancel seam"))?;
        let [operand] = args else {
            return Err(LowerError::unsupported(
                span,
                "a `cancel` call",
                "stage exactly the error value; the payload is one member's word span",
            ));
        };
        let tp = self.value(operand)?;
        let payload_members = self.members(tp.ty);
        let bare = match &*self.shapes.shape(tp.ty)? {
            Shape::Union(u) => payload_members
                .iter()
                .all(|m| u.member(*m).is_some_and(|e| e.fields.is_empty())),
            Shape::Word(SlotType::Tag) => true,
            _ => false,
        };
        // the containment proof belongs to the function that owns the
        // nursery the fiber cancels into: the parent's spawn site checks
        // a spawned body's payloads against its own return union, so a
        // child frame skips the check here
        if !self.spawn_child && !set_diff(payload_members, self.members(self.ret_ty)).is_empty() {
            return Err(LowerError::unsupported(
                operand.span(),
                "a `cancel` payload",
                "the error member is not contained in the enclosing function's return union; the \
                 nursery boundary unions child errors out through the return",
            ));
        }
        if bare {
            let addr = single(&self.word_addrs(&tp.place, tp.ty)?, operand.span())?;
            if addr.slot_ty != SlotType::Tag {
                return Err(LowerError::unsupported(
                    operand.span(),
                    "a `cancel` payload",
                    "the error value must be a bare enum member this round",
                ));
            }
            self.seam_call(native, &[(addr.slot, SlotType::Tag)], &[SlotType::I64])?;
            return Ok(self.unit_value(span)?.place);
        }
        // The payloaded staging: the channel is the failing value's flat
        // word span — word 0 the member's tag, the tail the live
        // member's payload words in declaration order (the native
        // channel's staging shape), the words past the live member's
        // width plain-zeroed so a stale overlay word never rides the
        // trap. A union operand dispatches on its own tag word per
        // payloaded member (the operand's region claim layout is this
        // site's fact); any other operand mints its member's tag ahead
        // of its whole span. The staged `Ref` sources keep their counts
        // — a borrow window exactly like any native staging — and the
        // dying fiber's drain hands the delivered words' claims to the
        // joiner.
        self.rc.native_open();
        let layout = self.shapes.shape(tp.ty)?;
        if let Shape::Union(_) = &*layout {
            let tag_word = self.tag_addr(&tp.place)?;
            let mut arms: Vec<(u8, &[MemberField])> = Vec::new();
            for member in payload_members {
                let fields = layout
                    .member_fields(*member)
                    .ok_or_else(|| mismatch("a union member read"))?;
                if fields.is_empty() {
                    continue;
                }
                let tag = self.shapes.tag_id(*member)?;
                arms.push((tag, fields));
            }
            let width = arms
                .iter()
                .map(|(_, fields)| fields.iter().map(MemberField::width).sum::<usize>())
                .max()
                .unwrap_or(0);
            self.guard_staged_count(1 + width)?;
            self.emit(Op::Mov {
                dst: Slot::overlay(0),
                src: Slot::body(tag_word),
            });
            for index in 1..=width {
                let kidx = self.pool(Constant::I64(0));
                self.emit(Op::Kimm {
                    dst: Slot::overlay(u8::try_from(index).expect("overlay word fits u8")),
                    kidx,
                });
            }
            let mut labels = Vec::with_capacity(arms.len());
            for (tag, _) in &arms {
                let cond = self.temp_word(SlotType::Bool)?;
                self.emit(Op::TagTest {
                    dst: Slot::body(cond),
                    src: Slot::body(tag_word),
                    tag: *tag,
                });
                let label = self.label();
                self.emit(Op::Jmpif {
                    cond: Slot::body(cond),
                    target: label,
                });
                labels.push(label);
            }
            let call = self.label();
            self.emit(Op::Jmp { target: call });
            for ((_, fields), label) in arms.iter().zip(labels) {
                self.bind_label(label);
                let mut flat = 0usize;
                for field in *fields {
                    for offset in 0..field.width() {
                        let at =
                            field.word + u8::try_from(offset).expect("field span offset fits u8");
                        self.emit(Op::Mov {
                            dst: Slot::overlay(
                                u8::try_from(1 + flat + offset).expect("overlay word fits u8"),
                            ),
                            src: Slot::body(tag_word + at),
                        });
                        if field.slots[offset] == SlotType::Ref {
                            self.rc.staged_native(tag_word + at, false);
                        }
                    }
                    flat += field.width();
                }
                self.emit(Op::Jmp { target: call });
            }
            self.bind_label(call);
            // the staged kinds: word 0 the tag, the tail one set per index
            // across the members (mixed indices widen to Raw)
            for (_, fields) in &arms {
                let mut staged = vec![SlotType::Tag];
                staged.extend(fields.iter().flat_map(|f| f.slots.iter().copied()));
                self.overlay_site(&staged, &[]);
            }
            self.overlay_site(&[], &[SlotType::I64]);
            let kidx = self.pool(Constant::FuncRef {
                key: self.native_key(native.module),
                func: native.index,
            });
            self.emit(Op::Call {
                callee: kidx,
                nargs: u8::try_from(1 + width).expect("argument count fits u8"),
            });
        } else {
            let addrs = self.word_addrs(&tp.place, tp.ty)?;
            let kinds = self.shapes.words(tp.ty)?;
            self.guard_staged_count(1 + addrs.len())?;
            let [member] = payload_members else {
                return Err(mismatch("a payloaded cancel member"));
            };
            let tag = self.shapes.tag_id(*member)?;
            let tag_word = self.temp_word(SlotType::Tag)?;
            self.emit(Op::TagNew {
                dst: Slot::body(tag_word),
                tag,
            });
            self.emit(Op::Mov {
                dst: Slot::overlay(0),
                src: Slot::body(tag_word),
            });
            for (index, (addr, kind)) in addrs.iter().zip(kinds.iter()).enumerate() {
                self.emit(Op::Mov {
                    dst: Slot::overlay(u8::try_from(1 + index).expect("overlay word fits u8")),
                    src: Slot::body(addr.slot),
                });
                if *kind == SlotType::Ref {
                    self.rc.staged_native(addr.slot, false);
                }
            }
            let mut staged = vec![SlotType::Tag];
            staged.extend_from_slice(&kinds);
            self.overlay_site(&staged, &[SlotType::I64]);
            let kidx = self.pool(Constant::FuncRef {
                key: self.native_key(native.module),
                func: native.index,
            });
            self.emit(Op::Call {
                callee: kidx,
                nargs: u8::try_from(1 + addrs.len()).expect("argument count fits u8"),
            });
        }
        self.rc.native_done(&mut self.code);
        Ok(self.unit_value(span)?.place)
    }

    /// The ambient `str[t]` conversion as an explicit call [F6]: the
    /// operand's checked type routes the same conversion the
    /// interpolation holes use — printable primitives pass their Ref
    /// word through or convert through the str natives, anything else
    /// reports the hole's honest boundary. The one-body CALLG dispatch
    /// waits on the VM-side generic str body, so the call site routes
    /// by its solved type instead. `None` when `def` is not the
    /// conversion extern.
    fn ambient_str_place(
        &mut self,
        def: DefId,
        args: &[&Expr],
        span: Span,
    ) -> Result<Option<Place>, LowerError> {
        if !self.ambient_str_def(def) {
            return Ok(None);
        }
        let [arg] = args else {
            return Err(mismatch("a str call's argument"));
        };
        let val = self.value(arg)?;
        let word = self.hole_word(span, &val)?;
        Ok(Some(Place {
            base: word,
            shape: PlaceShape::Word(SlotType::Ref),
        }))
    }

    fn extraction_def(&self, def: DefId) -> Option<Extraction> {
        if self.plan.index_of(def).is_some() {
            return None;
        }
        let sig = self.analyzed.decls.fns.get(&def)?;
        if sig.generics.is_empty() || sig.params.len() != 2 {
            return None;
        }
        Extraction::of_name(&self.analyzed.package.def(def).name)
    }

    /// Lowers a tail-position expression: calls become CALLT, everything
    /// else returns directly [H2].
    fn tail(&mut self, e: &Expr) -> Result<(), LowerError> {
        match e {
            Expr::Paren { inner, .. } => self.tail(inner),
            Expr::Block(block) => self.block_tail(block),
            Expr::Return { .. } => self.return_value(e).map(|_| ()),
            Expr::If {
                cond, then, else_, ..
            } => {
                let cond_val = self.value(cond)?;
                let cond_word =
                    single(&self.word_addrs(&cond_val.place, cond_val.ty)?, cond.span())?;
                let else_lbl = self.label();
                self.emit(Op::JmpifNot {
                    cond: Slot::body(cond_word.slot),
                    target: else_lbl,
                });
                // path-fork: both arms exit, each dropping from its own state
                let base = self.rc.save();
                self.block_tail(then)?;
                let arm_then = self.rc.take();
                self.rc.restore(base);
                self.bind_label(else_lbl);
                let result = if let Some(else_expr) = else_ {
                    self.tail(else_expr)
                } else {
                    // a missing else falls through to the unit return [B2]
                    let unit = self.unit_value(e.span())?;
                    self.emit_return(&unit.place, unit.ty, e.span())
                };
                let arm_else = self.rc.take();
                // both arms exit the frame: each exit path dropped from
                // its own state, so nothing survives the join
                self.rc.reconcile(&[(arm_then, true), (arm_else, true)]);
                result
            },
            Expr::Match { .. } => self.match_tail(e),
            Expr::Try { expr, .. } => self.try_tail(expr),
            Expr::Call { .. } if self.is_tail_callee(e) => {
                let ty = self.val(e.span())?.ty;
                self.call_impl(e, ty, true)?;
                Ok(())
            },
            _ => {
                let tp = self.value(e)?;
                self.emit_return(&tp.place, tp.ty, e.span())
            },
        }
    }

    fn block_tail(&mut self, block: &BlockExpr) -> Result<(), LowerError> {
        self.scope_push();
        let result = self.block_tail_inner(block);
        self.scope_pop();
        result
    }

    fn block_tail_inner(&mut self, block: &BlockExpr) -> Result<(), LowerError> {
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
            }) => self.tail(expr),
            Some(Stmt {
                kind: StmtKind::Local(local),
                ..
            }) => {
                // A binding tail is a statement, not the block's value:
                // it lowers, and the frame returns unit [F2].
                self.local_stmt(local)?;
                let unit = self.unit_value(block.span)?;
                self.emit_return(&unit.place, unit.ty, block.span)
            },
            None => {
                let unit = self.unit_value(block.span)?;
                self.emit_return(&unit.place, unit.ty, block.span)
            },
        }
    }

    /// An early `return e`: the value leaves through the frame's return
    /// shape. The place handed back is dead code — the RET ends the frame,
    /// so the enclosing value slots never read it.
    fn return_value(&mut self, e: &Expr) -> Result<TypedPlace, LowerError> {
        let Expr::Return { expr, span } = e else {
            return Err(mismatch("a return"));
        };
        let tp = match expr {
            Some(expr) => self.value(expr)?,
            None => self.unit_value(*span)?,
        };
        self.emit_return(&tp.place, tp.ty, *span)?;
        self.unit_value(*span)
    }

    /// Returns one value in the enclosing function's return shape. The
    /// returned words transfer their references out with the frame; every
    /// other still-owned reference drops right before the exit op. A
    /// shape refusal over the return type re-points at `span` — the
    /// expression whose value needs the return span — instead of the
    /// type-keyed sentinel span.
    fn emit_return(&mut self, place: &Place, src_ty: Ty, span: Span) -> Result<(), LowerError> {
        self.emit_return_inner(place, src_ty)
            .map_err(|err| err.respanned(span))
    }

    fn emit_return_inner(&mut self, place: &Place, src_ty: Ty) -> Result<(), LowerError> {
        // the tail's value end is the exit boundary: the last entry of
        // the resumed-frame seam's per-statement record
        self.note_value_end();
        // A fn-typed return slot keeps the provenance rule over the
        // unproven callable words: the caller classifies the result
        // word Fn unless the tail is the factory lambda, so a
        // callable-defaulted word returned here hands the caller a Ref
        // behind the Fn trust and traps at the call. A closure word's
        // own return keeps the factory boundary (the recorded
        // return-flow seam) outside this guard, and a word addressed
        // inside a span place passed its own guard.
        if matches!(&*self.shapes.shape(self.ret_ty)?, Shape::Word(SlotType::Fn))
            && !matches!(place.shape, PlaceShape::Span(_))
            && let [addr] = self.word_addrs(place, src_ty)?.as_slice()
            && addr.slot_ty == SlotType::Fn
            && !self.scalar_fn_words.contains(&addr.slot)
        {
            return Err(LowerError::unsupported(
                no_span(),
                "an unproven callable in a fn-typed return",
                "the caller classifies the fn-typed result word Fn, so a closure riding the \
                 callable word would trap at the call; only a named fn, a non-capturing lambda, \
                 or a proven call result returns here",
            ));
        }
        // A returned value that is a part of a deinit-carrying value (a
        // field read) leaves with the dying value's own words: the span
        // claim covering the read fires here, the struct's death staged
        // while its other words are still live, the span's non-escaping
        // `Ref` words dec-ing behind the hook. The value itself — the
        // returned type carries the deinit — keeps its claim with the
        // transfer.
        if self.deinit_claim(src_ty, no_span())?.is_none() {
            let escaping = self.word_addrs(place, src_ty)?;
            let words: Vec<u8> = escaping.iter().map(|addr| addr.slot).collect();
            let mut fired = Vec::new();
            for &word in &words {
                if let Some(base) = self.rc.claim_base_of(word)
                    && !fired.contains(&base)
                {
                    fired.push(base);
                    self.rc.fire_claim(&mut self.code, base, &words);
                }
            }
        }
        if src_ty == self.ret_ty {
            let addrs = self.word_addrs(place, src_ty)?;
            if addrs.len() == 1 {
                let word = addrs[0].slot;
                self.rc.exit(&mut self.code, &[word]);
                self.emit(Op::Ret {
                    src: Slot::body(word),
                });
                return Ok(());
            }
            let contiguous = addrs.iter().enumerate().all(|(index, addr)| {
                addr.slot == place.base + u8::try_from(index).expect("span width fits u8")
            });
            if contiguous {
                let transferred: Vec<u8> = addrs.iter().map(|addr| addr.slot).collect();
                self.rc.exit(&mut self.code, &transferred);
                self.emit(Op::RetSpan {
                    src: Slot::body(place.base),
                    n: u8::try_from(addrs.len()).expect("span width fits u8"),
                });
                return Ok(());
            }
        }
        let ret_shape = self.shapes.shape(self.ret_ty)?;
        match &*ret_shape {
            Shape::Word(_) => {
                let addrs = self.word_addrs(place, src_ty)?;
                let [only] = addrs.as_slice() else {
                    return Err(mismatch("a scalar return"));
                };
                let word = only.slot;
                self.rc.exit(&mut self.code, &[word]);
                self.emit(Op::Ret {
                    src: Slot::body(word),
                });
                Ok(())
            },
            Shape::Struct(s) => {
                let target = self.temp_span(self.ret_ty)?;
                let addrs = self.word_addrs(place, src_ty)?;
                self.transfer_claim(target.base, place, src_ty)?;
                let srcs = ordered_field_srcs(&addrs, &s.fields)?;
                for (field, srcs) in s.fields.iter().zip(srcs) {
                    self.store_field_span(target.base, field, srcs);
                }
                let transferred: Vec<u8> = addrs
                    .iter()
                    .enumerate()
                    .map(|(index, _)| {
                        target.base + u8::try_from(index).expect("span width fits u8")
                    })
                    .collect();
                let width = self.shapes.words(self.ret_ty)?.len();
                self.rc.exit(&mut self.code, &transferred);
                self.emit(Op::RetSpan {
                    src: Slot::body(target.base),
                    n: u8::try_from(width).expect("span width fits u8"),
                });
                Ok(())
            },
            Shape::Tuple(s) => {
                let target = self.temp_span(self.ret_ty)?;
                let addrs = self.word_addrs(place, src_ty)?;
                self.transfer_claim(target.base, place, src_ty)?;
                if addrs.len() != s.fields.len() {
                    return Err(mismatch("a tuple return's arity"));
                }
                for (field, src_addr) in s.fields.iter().zip(addrs.iter()) {
                    self.emit(Op::Mov {
                        dst: Slot::body(target.base + field.word),
                        src: Slot::body(src_addr.slot),
                    });
                    if field.slot() == SlotType::Ref {
                        self.rc
                            .copied(&mut self.code, target.base + field.word, src_addr.slot);
                    }
                }
                let transferred: Vec<u8> = s.fields.iter().map(|f| target.base + f.word).collect();
                self.rc.exit(&mut self.code, &transferred);
                self.emit(Op::RetSpan {
                    src: Slot::body(target.base),
                    n: u8::try_from(s.fields.len()).expect("span width fits u8"),
                });
                Ok(())
            },
            Shape::Union(u) => {
                // A multi-member operand (a narrower union than the
                // declared return — the checker proved containment) is
                // tag-dispatched per member into the return union's
                // region, mirroring the tail-`?` shape: an operand's raw
                // span never returns directly, union regions are
                // per-union-type.
                let src_members = self.members(src_ty);
                if src_members.len() > 1 {
                    let src_word = self.tag_addr(place)?;
                    let mut exits = Vec::with_capacity(src_members.len());
                    for member in src_members {
                        let exit = self.label();
                        let cond = self.temp_word(SlotType::Bool)?;
                        let tag = self.shapes.tag_id(*member)?;
                        self.emit(Op::TagTest {
                            dst: Slot::body(cond),
                            src: Slot::body(src_word),
                            tag,
                        });
                        self.emit(Op::Jmpif {
                            cond: Slot::body(cond),
                            target: exit,
                        });
                        exits.push(exit);
                    }
                    for (member, exit) in src_members.iter().zip(exits) {
                        self.bind_label(exit);
                        self.respan_exit(place, *member)?;
                    }
                    return Ok(());
                }
                let atom = self.only_atom(src_ty, no_span())?;
                let member = u
                    .member(atom)
                    .ok_or_else(|| mismatch("a union member return"))?;
                // the return union's fn-typed payload slot keeps the
                // same provenance rule; the caller-side respan lifts the
                // refusal onto the returned expression's own span
                self.refuse_union_fn_payload(
                    &TypedPlace {
                        place: place.clone(),
                        ty: src_ty,
                    },
                    no_span(),
                )?;
                let target = self.temp_span(self.ret_ty)?;
                let tag = self.shapes.tag_id(atom)?;
                self.emit(Op::TagNew {
                    dst: Slot::body(target.base),
                    tag,
                });
                self.zero_init_member(target.base, &u.words, &member.fields)?;
                let addrs = self.word_addrs(place, src_ty)?;
                let srcs = ordered_field_srcs(&addrs, &member.fields)?;
                for (field, srcs) in member.fields.iter().zip(srcs) {
                    self.store_field_span(target.base, field, srcs);
                }
                let transferred: Vec<u8> = (0..u8::try_from(u.words.len())
                    .expect("span width fits u8"))
                    .map(|index| target.base + index)
                    .collect();
                self.rc.exit(&mut self.code, &transferred);
                self.emit(Op::RetSpan {
                    src: Slot::body(target.base),
                    n: u8::try_from(u.words.len()).expect("span width fits u8"),
                });
                Ok(())
            },
        }
    }

    /// One propagated member's early exit: re-span it into the enclosing
    /// return union, then return the span.
    fn respan_exit(&mut self, src_place: &Place, member: TypeId) -> Result<(), LowerError> {
        if !matches!(&*self.shapes.shape(self.ret_ty)?, Shape::Union(_)) {
            return Err(mismatch("a `?` exit in a non-union return"));
        }
        let target = self.temp_span(self.ret_ty)?;
        self.respan_into(&target, self.ret_ty, src_place, member)?;
        let n = self.shapes.words(self.ret_ty)?.len();
        // the span's references transfer out with the return
        let transferred: Vec<u8> = (0..u8::try_from(n).expect("span width fits u8"))
            .map(|index| target.base + index)
            .collect();
        self.rc.exit(&mut self.code, &transferred);
        self.emit(Op::RetSpan {
            src: Slot::body(target.base),
            n: u8::try_from(n).expect("span width fits u8"),
        });
        Ok(())
    }

    /// Writes one member of a union place into a destination union span:
    /// the tag word moves (tag ids are module-wide), the dead payload
    /// words zero-init, the member's fields follow from the source
    /// layout.
    fn respan_into(
        &mut self,
        dst: &Place,
        dst_ty: Ty,
        src: &Place,
        member: TypeId,
    ) -> Result<(), LowerError> {
        let shape = self.shapes.shape(dst_ty)?;
        let Shape::Union(u) = &*shape else {
            return Err(mismatch("a re-span destination"));
        };
        // the entry resolves through the substitution-aware lookup: a
        // strict sub-union of the site's substituted head carries the
        // substituted member atoms, which the destination's
        // variable-keyed entries match structurally
        let entry = self
            .shapes
            .member_for(u, member)
            .ok_or_else(|| mismatch("a re-span member"))?;
        let src_tag = self.tag_addr(src)?;
        if entry.atom == member {
            // tag ids are module-wide, so the source's own tag moves
            self.emit(Op::Mov {
                dst: Slot::body(dst.base),
                src: Slot::body(src_tag),
            });
        } else {
            // a substituted source atom names no entry of this head;
            // writing its tag would dispatch a member the body's tag
            // tests never test, so the tag mints the entry's own id
            self.emit(Op::TagNew {
                dst: Slot::body(dst.base),
                tag: entry.tag,
            });
        }
        self.zero_init_member(dst.base, &u.words, &entry.fields)?;
        let PlaceShape::Span(underlying) = src.shape else {
            return Err(mismatch("a re-span source"));
        };
        let src_layout = self.shapes.shape(underlying)?;
        let fields = src_layout
            .member_fields(member)
            .ok_or_else(|| mismatch("a re-span member"))?;
        for (index, field) in entry.fields.iter().enumerate() {
            // a positional field (a tuple member's) matches by position —
            // the None names all compare equal, so the name-find would
            // return the first field for every element
            let src_field = if field.name.is_some() {
                fields
                    .iter()
                    .find(|f| f.name == field.name)
                    .ok_or_else(|| mismatch("a re-span field"))?
            } else {
                fields
                    .get(index)
                    .ok_or_else(|| mismatch("a re-span field"))?
            };
            let srcs = field_addrs(src.base, src_field);
            self.store_field_span(dst.base, field, &srcs);
        }
        Ok(())
    }

    /// Writes one value into a destination span: same-type word moves, or
    /// a member construction with TAG_NEW into a union [BL-2].
    /// Moves a deinit value's claim from its source base word to a
    /// re-span destination's base (C5): the movs write a fresh copy of
    /// the words, and a pinned copy never dups, so the claim transfers
    /// silently. In-place spans and claim-less sources do nothing.
    fn transfer_claim(&mut self, dst_base: u8, src: &Place, src_ty: Ty) -> Result<(), LowerError> {
        let addrs = self.word_addrs(src, src_ty)?;
        let [first] = addrs.as_slice() else {
            return Ok(());
        };
        if first.slot == dst_base || !self.rc.has_claim(first.slot) {
            return Ok(());
        }
        self.rc.copied(&mut self.code, dst_base, first.slot);
        Ok(())
    }

    fn widen_into(
        &mut self,
        dst: &Place,
        dst_ty: Ty,
        src: &TypedPlace,
        span: Span,
    ) -> Result<(), LowerError> {
        self.transfer_claim(dst.base, &src.place, src.ty)?;
        if src.ty == dst_ty {
            let addrs = self.word_addrs(&src.place, src.ty)?;
            if addrs.is_empty() {
                return Ok(());
            }
            // Defense on the same-type word moves: the claimed type's
            // word list must fit the destination place. A caller
            // claiming a subset of the place's physical union would
            // otherwise emit full-operand-width writes (and RC claims)
            // past the destination's allocation — every honest caller
            // has the two widths equal, so this fires only on a
            // lowerer bug, as a typed refusal instead of memory
            // corruption.
            let dst_width = match dst.shape {
                PlaceShape::Word(_) => 1,
                PlaceShape::Span(underlying) => self.shapes.words(underlying)?.len(),
            };
            if addrs.len() > dst_width {
                return Err(mismatch("a widened destination span"));
            }
            if addrs.iter().enumerate().all(|(index, addr)| {
                addr.slot == dst.base + u8::try_from(index).expect("span width fits u8")
            }) {
                return Ok(()); // already in place
            }
            for (index, addr) in addrs.iter().enumerate() {
                let word = dst.base + u8::try_from(index).expect("span width fits u8");
                self.emit(Op::Mov {
                    dst: Slot::body(word),
                    src: Slot::body(addr.slot),
                });
                if matches!(addr.slot_ty, SlotType::Ref | SlotType::Raw) {
                    self.rc.copied(&mut self.code, word, addr.slot);
                }
            }
            return Ok(());
        }
        let dst_shape = self.shapes.shape(dst_ty)?;
        match &*dst_shape {
            Shape::Word(_) => {
                let addr = single(&self.word_addrs(&src.place, src.ty)?, no_span())?;
                self.emit(Op::Mov {
                    dst: Slot::body(dst.base),
                    src: Slot::body(addr.slot),
                });
                if matches!(addr.slot_ty, SlotType::Ref | SlotType::Raw) {
                    self.rc.copied(&mut self.code, dst.base, addr.slot);
                }
                Ok(())
            },
            Shape::Struct(s) => {
                let addrs = self.word_addrs(&src.place, src.ty)?;
                let srcs = ordered_field_srcs(&addrs, &s.fields)?;
                for (field, srcs) in s.fields.iter().zip(srcs) {
                    self.store_field_span(dst.base, field, srcs);
                }
                Ok(())
            },
            Shape::Tuple(s) => {
                let addrs = self.word_addrs(&src.place, src.ty)?;
                if addrs.len() != s.fields.len() {
                    return Err(mismatch("a tuple write's arity"));
                }
                for (field, addr) in s.fields.iter().zip(addrs.iter()) {
                    self.emit(Op::Mov {
                        dst: Slot::body(dst.base + field.word),
                        src: Slot::body(addr.slot),
                    });
                    if matches!(addr.slot_ty, SlotType::Ref | SlotType::Raw) {
                        self.rc
                            .copied(&mut self.code, dst.base + field.word, addr.slot);
                    }
                }
                Ok(())
            },
            Shape::Union(u) => {
                // A multi-member source (a narrower union than the
                // destination — the checker proved containment at the
                // annotated binding, the assignment, or the branch
                // join) is tag-dispatched per member into the
                // destination region, mirroring the union return path:
                // an operand's raw span never lands directly, union
                // regions are per-union-type.
                let src_members = self.members(src.ty);
                if src_members.len() > 1 {
                    let src_word = self.tag_addr(&src.place)?;
                    let continue_lbl = self.label();
                    let mut exits = Vec::with_capacity(src_members.len());
                    for member in src_members {
                        let exit = self.label();
                        let cond = self.temp_word(SlotType::Bool)?;
                        let tag = self.shapes.tag_id(*member)?;
                        self.emit(Op::TagTest {
                            dst: Slot::body(cond),
                            src: Slot::body(src_word),
                            tag,
                        });
                        self.emit(Op::Jmpif {
                            cond: Slot::body(cond),
                            target: exit,
                        });
                        exits.push(exit);
                    }
                    for (member, exit) in src_members.iter().zip(exits) {
                        self.bind_label(exit);
                        self.respan_into(dst, dst_ty, &src.place, *member)?;
                        self.emit(Op::Jmp {
                            target: continue_lbl,
                        });
                    }
                    self.bind_label(continue_lbl);
                    return Ok(());
                }
                let atom = self.only_atom(src.ty, span)?;
                // a written generic head keys its members by the
                // variable's atoms while the entering site carries the
                // substituted concrete value, so the lookup resolves
                // through the substitution's structural match — a heap
                // application by its constructor (one Ref word whatever
                // the argument), a tuple member by its elements (one
                // Raw-claimed word each, filled from the substituted
                // value's own words) — and the tag writes the member's
                // own minted id: re-minting the source's atom would tag
                // a member the body's tests never dispatch on
                let member = self.shapes.member_for(u, atom).ok_or_else(|| {
                    LowerError::unsupported(
                        span,
                        "a union member write",
                        "the destination union carries no member whose region holds the value's \
                         shape; a written generic head takes the member the site's substitution \
                         names — a heap application by its constructor, or a tuple member whose \
                         one-word-per-element claim the substituted value pairs exactly",
                    )
                })?;
                // a callable word entering the union's fn-typed
                // payload slot keeps the var slot's provenance rule:
                // the payload word is Fn-classified and every extraction
                // re-reads it, so a closure's Ref word traps there the
                // same way it traps a re-storable var slot
                self.refuse_union_fn_payload(src, span)?;
                self.emit(Op::TagNew {
                    dst: Slot::body(dst.base),
                    tag: member.tag,
                });
                self.zero_init_member(dst.base, &u.words, &member.fields)?;
                let addrs = self.word_addrs(&src.place, src.ty)?;
                let srcs = ordered_field_srcs(&addrs, &member.fields)?;
                for (field, srcs) in member.fields.iter().zip(srcs) {
                    self.store_field_span(dst.base, field, srcs);
                }
                Ok(())
            },
        }
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
            Callee::Generic(generic) => Constant::FuncRef {
                key: self.native_key(generic.module),
                func: generic.index,
            },
            Callee::Local(index) => Constant::FuncRef {
                key: FuncRefKey::Dep(0), // the enclosing module
                func: index,
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
            | BinaryOp::WrapMul,
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

fn parse_int(text: &str) -> Option<i64> {
    let (radix, digits) =
        if let Some(rest) = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
            (16, rest)
        } else if let Some(rest) = text.strip_prefix("0b").or_else(|| text.strip_prefix("0B")) {
            (2, rest)
        } else if let Some(rest) = text.strip_prefix("0o").or_else(|| text.strip_prefix("0O")) {
            (8, rest)
        } else {
            (10, text)
        };
    i64::from_str_radix(digits, radix).ok()
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
