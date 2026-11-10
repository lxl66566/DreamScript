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
    collections::{HashMap, HashSet},
    rc::Rc,
};

use ds_abi::{BuiltinInstance, ConstIdx, RcOp, SlotType, Uop};
use ds_check::{
    Analyzed, BinderFact, DefId, DotRes, Extraction, Res, Resolutions, Ty, TypeId, Val,
};
use ds_diag::Span;
use ds_mod::Constant;
use ds_parse::{
    BinaryOp, BlockExpr, CallArg, EnumPayload, Expr, Ident, Literal, LocalDecl, MatchArm, Param,
    Path, Pattern, Stmt, StmtKind, StrPart, Type, UnaryOp,
};

use crate::{
    PlacedOp,
    error::LowerError,
    native::{GenericNative, Native, NativeShape, PLATFORM_IO_OK},
    place::{Owned, Placement},
    shape::{MemberField, Shape, Shapes},
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
        }
    }

    /// The result word type of this op.
    fn result_slot(self, lhs: SlotType) -> SlotType {
        match self {
            Self::AddI64
            | Self::SubI64
            | Self::MulI64
            | Self::DivI64
            | Self::ModI64
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

/// A walked expression's result: its place plus the use's static type.
struct TypedPlace {
    place: Place,
    ty: Ty,
}

/// One word of a value inside a place.
#[derive(Clone, Debug)]
struct WordAddr {
    /// The field name where one exists; `None` for scalars.
    name: Option<String>,
    /// The absolute body slot.
    slot: u8,
    /// The word type.
    slot_ty: SlotType,
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
    /// resolves to it by name — its word calls).
    pub(crate) def: Option<DefId>,
    /// The enclosing bindings a synthesized lambda captures, in
    /// first-use order: binder span plus declared type (F2). Always empty
    /// for named functions — a named body resolves its own names.
    pub(crate) captures: Rc<Vec<(Span, Ty)>>,
}

/// The module plan: lowered functions in function-table order.
pub(crate) struct Plan {
    functions: Vec<PlannedFn>,
    index: HashMap<DefId, u32>,
}

impl Plan {
    pub(crate) fn new() -> Self {
        Self {
            functions: Vec::new(),
            index: HashMap::new(),
        }
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
            captures: Rc::new(Vec::new()),
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
    ) -> (u32, Rc<Vec<(Span, Ty)>>) {
        let index = u32::try_from(self.functions.len()).expect("function index fits u32");
        let captures = Rc::new(collect_captures(lambda, res, binders));
        self.functions.push(PlannedFn {
            name: format!("{enclosing}${index}"),
            lambda: lambda.clone(),
            def: None,
            captures: captures.clone(),
        });
        (index, captures)
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
        Expr::Literal { .. } | Expr::RawStr { .. } | Expr::Break(_) | Expr::Continue(_) => {},
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

/// A call target: a module-local function or a contract builtin.
#[derive(Clone, Copy)]
enum Callee {
    /// A builtin native in one of the two reserved native modules (the
    /// `builtin` core or the `std` platform module — the native's own
    /// routing says which).
    Builtin(&'static Native),
    /// A generic builtin's ONE body: the call routes through CALLG with
    /// the instantiated-type evidence words staged after the real
    /// arguments [H3].
    Generic(&'static GenericNative),
    /// This module's function-table index.
    Local(u32),
}

/// One function's lowering state.
pub(crate) struct FnLower<'a, 'p, 's> {
    analyzed: &'a Analyzed,
    res: &'a Resolutions,
    plan: &'p mut Plan,
    shapes: &'s mut Shapes<'a>,
    name: String,
    /// Whether this function is a synthesized lambda value's body: a use
    /// of a binder the walk never bound is a capture there.
    synth: bool,
    /// Whether this frame is the program's root: the run entry (`main`).
    /// Only a root frame may tail-transfer to a native with its staged
    /// borrows abandoned — the program ends with that call, so the heap
    /// dies whole; every other frame returns into a live program and
    /// flushes the counts it owes before the exit.
    root: bool,
    ret_ty: Ty,
    params_len: usize,
    slots: Vec<SlotType>,
    code: Vec<Op>,
    labels: Vec<usize>,
    pool: Vec<Constant>,
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
    /// The Perceus dup/drop placement over this function's walk (C4).
    rc: Placement,
}

impl<'a, 'p, 's> FnLower<'a, 'p, 's> {
    /// Lowers one planned function to a validated ds-mod function record
    /// plus its placed (RC-bearing) op sequence.
    pub(crate) fn lower(
        analyzed: &'a Analyzed,
        res: &'a Resolutions,
        plan: &'p mut Plan,
        shapes: &'s mut Shapes<'a>,
        lin: &ds_check::Linearity,
        planned: &PlannedFn,
    ) -> Result<(ds_mod::Function, Vec<PlacedOp>), LowerError> {
        let Expr::Lambda { params, body, .. } = &planned.lambda else {
            return Err(LowerError::unsupported(
                planned.lambda.span(),
                "a non-lambda function body",
                "planned functions are const-arrow lambdas",
            ));
        };
        // A named function reads its type from the package table; a
        // synthesized lambda value carries its checked type on its own
        // expression span.
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
        let mut this = Self {
            analyzed,
            res,
            plan,
            shapes,
            name: planned.name.clone(),
            synth: planned.def.is_none(),
            root: planned.name == "main",
            ret_ty: ret,
            params_len: 0,
            slots: Vec::new(),
            code: Vec::new(),
            labels: Vec::new(),
            pool: Vec::new(),
            binders: HashMap::new(),
            fn_words: HashMap::new(),
            scalar_fn_words: HashSet::new(),
            scopes: vec![HashMap::new()],
            overlay: Vec::new(),
            overlay_len: 0,
            rc: Placement::from_linearity(lin),
        };
        this.bind_params(params)?;
        this.bind_captures(&planned.captures)?;
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

    fn members(&self, ty: Ty) -> Vec<TypeId> {
        self.analyzed.decls.universe.members(ty).to_vec()
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

    /// Adds one pool constant (deduplicated), returning its index.
    fn pool(&mut self, constant: Constant) -> u16 {
        if let Some(index) = self.pool.iter().position(|c| *c == constant) {
            return u16::try_from(index).expect("pool index fits u16");
        }
        let index = u16::try_from(self.pool.len()).expect("pool index fits u16");
        self.pool.push(constant);
        index
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
            _ => self.ref_words(place.base, ty)?,
        })
    }

    /// Registers a binder with the RC pass: its Ref words and the deinit
    /// hook its concrete type carries (none in the lowerable subset — a
    /// deinit-carrying type has no constructible surface yet, so the hook
    /// target lands with the `~Copyable` round).
    fn rc_bind(&mut self, binder: Span, base: u8, ty: Ty) -> Result<(), LowerError> {
        let words = self.ref_words(base, ty)?;
        self.rc.bound(binder, &words, None);
        Ok(())
    }

    // ---- params and scopes ---------------------------------------------------

    fn bind_params(&mut self, params: &[Param]) -> Result<(), LowerError> {
        for param in params {
            if !matches!(param.mode, ds_parse::ParamMode::Borrow) {
                return Err(LowerError::unsupported(
                    param.span,
                    "a non-borrow parameter mode",
                    "mut/move/comptime parameters arrive with the M4 linearity lowering",
                ));
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
            self.binders.insert(param.name.span, Place { base, shape });
            // a borrow parameter owns its incoming reference: the caller
            // handed a count (dup or forward at its staging) and this
            // frame's drops balance it (C2/C4)
            for word in self.ref_words(base, fact.ty)? {
                self.rc.produced(word, None);
            }
            self.rc_bind(param.name.span, base, fact.ty)?;
            self.scopes
                .last_mut()
                .expect("the root scope exists")
                .insert(param.name.name.clone(), param.name.span);
            self.params_len += words.len();
        }
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
            for word in self.ref_words(base, ty)? {
                self.rc.produced(word, None);
            }
            self.rc_bind(bind, base, ty)?;
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
        match self.members(ty).as_slice() {
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
                if self.members(*underlying).as_slice() == [atom] {
                    return Ok(vec![WordAddr {
                        name: None,
                        slot: place.base,
                        slot_ty: *slot,
                    }]);
                }
                let fields = self.member_fields(*underlying, atom)?;
                match fields.as_slice() {
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
                    // the same bare struct: words in declaration order
                    return Ok(s
                        .fields
                        .iter()
                        .map(|f| WordAddr {
                            name: f.name.clone(),
                            slot: place.base + f.word,
                            slot_ty: f.slot,
                        })
                        .collect());
                }
                let atom = self.only_atom(ty, no_span())?;
                let fields = self.member_fields(*underlying, atom)?;
                s.fields
                    .iter()
                    .map(|field| {
                        let member = fields
                            .iter()
                            .find(|f| f.name == field.name)
                            .ok_or_else(|| mismatch("a union member field"))?;
                        Ok(WordAddr {
                            name: field.name.clone(),
                            slot: place.base + member.word,
                            slot_ty: field.slot,
                        })
                    })
                    .collect()
            },
            (Shape::Tuple(s), PlaceShape::Span(underlying)) => {
                let place_shape = self.shapes.shape(*underlying)?;
                if !matches!(&*place_shape, Shape::Tuple(_)) {
                    return Err(mismatch("a tuple use against a non-tuple span"));
                }
                Ok(s.fields
                    .iter()
                    .map(|f| WordAddr {
                        name: None,
                        slot: place.base + f.word,
                        slot_ty: f.slot,
                    })
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

    /// The fields of one member atom inside a union type's layout.
    fn member_fields(
        &mut self,
        union_ty: Ty,
        atom: TypeId,
    ) -> Result<Vec<MemberField>, LowerError> {
        let shape = self.shapes.shape(union_ty)?;
        let Shape::Union(u) = &*shape else {
            return Err(mismatch("a union member read"));
        };
        let member = u
            .member(atom)
            .ok_or_else(|| mismatch("a union member read"))?;
        Ok(member.fields.clone())
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

    /// The unit value: one I64 word, matching the native unit convention.
    fn unit_value(&mut self, span: Span) -> Result<TypedPlace, LowerError> {
        let ty = self.val(span).map_or(self.ret_ty, |v| v.ty);
        let base = self.temp_word(SlotType::I64)?;
        let kidx = self.pool(Constant::I64(0));
        self.emit(Op::Kimm {
            dst: Slot::body(base),
            kidx,
        });
        Ok(TypedPlace {
            place: Place {
                base,
                shape: PlaceShape::Word(SlotType::I64),
            },
            ty,
        })
    }

    fn stmt(&mut self, stmt: &Stmt) -> Result<(), LowerError> {
        match &stmt.kind {
            StmtKind::Local(local) => self.local_stmt(local).map(|_| ()),
            StmtKind::Expr(expr) => {
                let tp = self.value(expr)?;
                // a statement-position value is discarded: its references
                // die at this use when nothing else keeps them
                let words = self.tracked_words(&tp.place, tp.ty)?;
                self.rc.discard(&mut self.code, &words);
                Ok(())
            },
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
    fn local_stmt(&mut self, local: &LocalDecl) -> Result<TypedPlace, LowerError> {
        let init = self.value(&local.init)?;
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
            let target = self.temp_span(declared)?;
            self.refuse_closure_widen(&init, declared, local.span)?;
            self.widen_into(&target, declared, &init)?;
            self.place_of_ty(target.base, declared)?
        } else {
            init.place.clone()
        };
        match pat {
            Pattern::Binding { span, name } => {
                self.binders.insert(*span, place.clone());
                // the place's own word classification leads: a const
                // aliasing a closure word binds the Ref word the declared
                // fn type does not name, and the binder's death drops it
                let words = self.tracked_words(&place, declared)?;
                self.rc.bound(*span, &words, None);
                self.scope_insert(&name.name, *span);
            },
            Pattern::Tuple { pats, .. } => {
                if local.mutable {
                    return Err(LowerError::unsupported(
                        local.span,
                        "a destructuring var binding",
                        "a var owns one re-storable span; destructured elements need their own \
                         var slots",
                    ));
                }
                self.bind_tuple_pattern(pats.as_slice(), &TypedPlace {
                    place: place.clone(),
                    ty: declared,
                })?;
            },
            Pattern::Wildcard(_) => {
                // `const _ = e` discards: no binder names the value, so
                // its references die here when nothing else keeps them
                let words = self.tracked_words(&place, declared)?;
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
            ds_parse::AssignOp::Plain => self.value(right)?,
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
                let rhs = self.value(right)?;
                let result = self.arith(int_op, float_op, &current, &rhs, left.span())?;
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
        // (compound stores re-store arithmetic results only, never Ref
        // words — the string compound ops have no uop)
        for word in self.ref_words(place.base, declared)? {
            self.rc.store_over(&mut self.code, &[word]);
        }
        self.refuse_closure_widen(&assigned, declared, e.span())?;
        self.widen_into(&place, declared, &assigned)?;
        Ok(self.unit_value(e.span())?.place)
    }

    /// One `while cond { body }`: a backward jump over the re-tested
    /// condition [F1]. The body lowers as statements (its value is unit);
    /// every body slot is frame-static, so an iteration re-writes the same
    /// words. Refinements harvested inside an iteration never escape it
    /// (the checker restores its snapshot at loop exit; the lowerer reads
    /// only per-expression checked types, which already respect that).
    fn while_value(&mut self, e: &Expr) -> Result<Place, LowerError> {
        let Expr::While { cond, body, span } = e else {
            return Err(mismatch("a while loop"));
        };
        let head = self.label();
        let exit = self.label();
        self.bind_label(head);
        let cond_val = self.value(cond)?;
        let cond_word = single(&self.word_addrs(&cond_val.place, cond_val.ty)?, cond.span())?;
        self.emit(Op::JmpifNot {
            cond: Slot::body(cond_word.slot),
            target: exit,
        });
        // loop-fork the ownership map: words owned on entry AND at the
        // back edge survive the loop; body-only words died inside it, and
        // uses one depth deeper never kill outer productions
        let entry = self.rc.save();
        self.rc.loop_enter();
        self.scope_push();
        for stmt in &body.stmts {
            self.stmt(stmt)?;
        }
        self.scope_pop();
        self.rc.loop_exit();
        let body_end = self.rc.take();
        self.rc.join(&[entry, body_end]);
        self.emit(Op::Jmp { target: head });
        self.bind_label(exit);
        Ok(self.unit_value(*span)?.place)
    }

    /// A `for x in a..b` over an int range: a counted loop with one
    /// induction slot. The endpoints evaluate once; the step test
    /// re-runs per iteration against the frozen end. The pattern binder
    /// names the induction word, so every body read sees the current
    /// step [F7].
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
        let Expr::Range {
            inclusive,
            start,
            end,
            ..
        } = peel_parens(iter)
        else {
            return Err(LowerError::unsupported(
                e.span(),
                "a for over a collection",
                "iteration over stdlib collections arrives with their own surface",
            ));
        };
        let start_tp = self.value(start)?;
        let start_w = single(
            &self.word_addrs(&start_tp.place, start_tp.ty)?,
            start.span(),
        )?;
        let end_tp = self.value(end)?;
        let end_w = single(&self.word_addrs(&end_tp.place, end_tp.ty)?, end.span())?;
        if start_w.slot_ty != SlotType::I64 || end_w.slot_ty != SlotType::I64 {
            return Err(LowerError::unsupported(
                e.span(),
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
        self.bind_label(head);
        let bound_op = if *inclusive { DataOp::LeI64 } else { DataOp::LtI64 };
        let cond = self.emit_data(bound_op, &ind_tp, &end_tp, e.span())?;
        let cond_w = single(&self.word_addrs(&cond, start_tp.ty)?, e.span())?;
        self.emit(Op::JmpifNot {
            cond: Slot::body(cond_w.slot),
            target: exit,
        });
        // loop-fork the ownership map exactly like the while shape
        let entry = self.rc.save();
        self.rc.loop_enter();
        self.scope_push();
        match pat {
            Pattern::Binding {
                span: bind_span,
                name,
            } => {
                self.binders.insert(*bind_span, ind_place.clone());
                self.rc_bind(*bind_span, ind, start_tp.ty)?;
                self.scope_insert(&name.name, *bind_span);
            },
            Pattern::Wildcard(_) => {},
            _ => {
                return Err(LowerError::unsupported(
                    e.span(),
                    "a destructuring for pattern",
                    "only plain name and wildcard patterns lower this round",
                ))
            },
        }
        match &**body {
            Expr::Block(block) => {
                for stmt in &block.stmts {
                    self.stmt(stmt)?;
                }
            },
            other => {
                let tp = self.value(other)?;
                let words = self.ref_words(tp.place.base, tp.ty)?;
                self.rc.discard(&mut self.code, &words);
            },
        }
        self.scope_pop();
        self.rc.loop_exit();
        let body_end = self.rc.take();
        self.rc.join(&[entry, body_end]);
        self.emit(Op::Data {
            op: DataOp::AddI64,
            dst: Slot::body(ind),
            lhs: Slot::body(ind),
            rhs: Slot::body(step),
        });
        self.emit(Op::Jmp { target: head });
        self.bind_label(exit);
        Ok(self.unit_value(*span)?.place)
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
                let kidx = self.pool(Constant::Str(text.clone()));
                self.emit(Op::Kstr {
                    dst: Slot::body(dst),
                    kidx,
                });
                self.rc.produced(dst, None);
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
            Expr::Break(_)
            | Expr::Continue(_)
            | Expr::Nursery { .. }
            | Expr::Spawn { .. }
            | Expr::Defer { .. } => {
                return Err(LowerError::unsupported(
                    e.span(),
                    "a control statement",
                    "loop-exit/nursery statements have no lowering; write tail expressions and \
                     while loops",
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
                let kidx = self.pool(Constant::Str(c.to_string()));
                self.emit(Op::Kstr {
                    dst: Slot::body(dst),
                    kidx,
                });
                self.rc.produced(dst, None);
                Ok(Place {
                    base: dst,
                    shape: PlaceShape::Word(SlotType::Ref),
                })
            },
            Literal::Byte(_) => Err(LowerError::unsupported(
                span,
                "a byte literal",
                "no byte slot is in the M3 uop contract",
            )),
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
        let Expr::Lambda { generics, span, .. } = e else {
            return Err(mismatch("a lambda value"));
        };
        if !generics.is_empty() {
            return Err(LowerError::unsupported(
                *span,
                "a generic lambda value",
                "generic bodies are the M6 CALL_GENERIC seam",
            ));
        }
        let (index, captures) =
            self.plan
                .push_lambda(&self.name, e, self.res, &self.analyzed.checked.binders);
        let kidx = self.pool(Constant::FuncRef {
            module: String::new(), // the enclosing module
            func: index,
        });
        let func_word = self.temp_word(SlotType::Fn)?;
        self.emit(Op::Kconst {
            dst: Slot::body(func_word),
            kidx,
        });
        if captures.is_empty() {
            self.fn_words.insert(func_word, index);
            self.scalar_fn_words.insert(func_word);
            return Ok(Place {
                base: func_word,
                shape: PlaceShape::Word(SlotType::Fn),
            });
        }
        // stage the capture block on the overlay, in the order the
        // synthesized body binds it; a capture whose place is a closure
        // word refuses (the callable-classification seam: the body's
        // capture slot would classify Fn and the Ref word cannot sit there)
        let mut staged: Vec<SlotType> = Vec::new();
        let mut moves: Vec<u8> = Vec::new();
        for &(bind, ty) in &*captures {
            let place = self
                .binders
                .get(&bind)
                .cloned()
                .ok_or_else(|| self.missing_binder(*span))?;
            if self.is_closure_word(&place, ty)? {
                return Err(LowerError::unsupported(
                    *span,
                    "a lambda capturing a closure",
                    "the capture block would carry a Ref word behind a Fn-classified capture \
                     slot; pass the callable as a parameter instead",
                ));
            }
            // a fn-typed capture must prove scalar provenance: a
            // callable-defaulted parameter (or an alias of one) may hold a
            // closure's Ref word at runtime, and the copy-in would write it
            // into the Fn-classified capture slot
            if matches!(&*self.shapes.shape(ty)?, Shape::Word(SlotType::Fn)) {
                let addrs = self
                    .word_addrs(&place, ty)
                    .map_err(|err| err.respanned(*span))?;
                let proven = addrs.len() == 1 && self.scalar_fn_words.contains(&addrs[0].slot);
                if !proven {
                    return Err(LowerError::unsupported(
                        *span,
                        "a lambda capturing a callable binding",
                        "the capture cannot prove a non-capturing scalar; a callable-defaulted \
                         parameter may hold a closure at runtime — pass it as a parameter instead",
                    ));
                }
            }
            for addr in self
                .word_addrs(&place, ty)
                .map_err(|err| err.respanned(*span))?
            {
                moves.push(addr.slot);
                staged.push(addr.slot_ty);
            }
        }
        self.guard_staged_count(staged.len())?;
        for (index, slot) in moves.iter().enumerate() {
            self.emit(Op::Mov {
                dst: Slot::overlay(u8::try_from(index).expect("overlay word fits u8")),
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
        self.rc.produced(dst, None);
        self.fn_words.insert(dst, index);
        Ok(Place {
            base: dst,
            shape: PlaceShape::Word(SlotType::Ref),
        })
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
            Callee::Builtin(_) => None,
            Callee::Generic(generic) => Some(generic.params.to_vec()),
            Callee::Local(index) => {
                let planned = self.plan.functions().get(index as usize)?;
                planned
                    .def
                    .and_then(|def| self.analyzed.checked.def_types.get(&def).copied())
                    .and_then(|fn_ty| fn_sig_of(&self.analyzed.decls.universe, fn_ty))
                    .and_then(|(params, _)| {
                        let mut words = Vec::new();
                        for ty in params {
                            words.extend(self.shapes.words(*ty).ok()?);
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

    /// A closure value widened into a var span refuses here: the span
    /// classifies its fn-typed word Fn, so the re-storable slot would
    /// hold a Ref word behind the type's back.
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
    /// natives, a string or char passes its Ref word through.
    fn string_expr(&mut self, parts: &[StrPart]) -> Result<Place, LowerError> {
        let mut acc: Option<u8> = None;
        for part in parts {
            let word = match part {
                StrPart::Text { text, .. } => {
                    let dst = self.temp_word(SlotType::Ref)?;
                    let kidx = self.pool(Constant::Str(text.clone()));
                    self.emit(Op::Kstr {
                        dst: Slot::body(dst),
                        kidx,
                    });
                    self.rc.produced(dst, None);
                    dst
                },
                StrPart::Expr { expr, .. } => {
                    let val = self.value(expr)?;
                    self.hole_word(expr.span(), &val)?
                },
                StrPart::Ident { name, .. } => {
                    let ty = self.val(name.span)?.ty;
                    let path = Path {
                        span: name.span,
                        segments: vec![name.clone()],
                    };
                    let place = self.path_value(&path, name.span, ty)?;
                    self.hole_word(name.span, &TypedPlace { place, ty })?
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
                    self.rc.produced(dst, None);
                    dst
                },
            });
        }
        let base = if let Some(base) = acc {
            base
        } else {
            // the empty string literal: a plain constant, not a chain
            let dst = self.temp_word(SlotType::Ref)?;
            let kidx = self.pool(Constant::Str(String::new()));
            self.emit(Op::Kstr {
                dst: Slot::body(dst),
                kidx,
            });
            self.rc.produced(dst, None);
            dst
        };
        Ok(Place {
            base,
            shape: PlaceShape::Word(SlotType::Ref),
        })
    }

    /// `str(e)` of one hole [F6]: the operand's checked type routes the
    /// conversion. Anything the checker did not prove printable lands as
    /// a lowering error (the `show` derive is the M6 CALL_GENERIC seam).
    fn hole_word(&mut self, span: Span, val: &TypedPlace) -> Result<u8, LowerError> {
        let members = self.members(val.ty);
        let [atom] = members.as_slice() else {
            return Err(LowerError::unsupported(
                span,
                "an interpolation hole",
                "the operand's type is not a printable primitive; the `show` derive is the M6 seam",
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
            "int" => self.str_native_word("str_int", SlotType::I64, val, span),
            "float" => self.str_native_word("str_float", SlotType::F64, val, span),
            "bool" => self.str_native_word("str_bool", SlotType::Bool, val, span),
            _ => Err(LowerError::unsupported(
                span,
                "an interpolation hole",
                "the operand's type is not a printable primitive; the `show` derive is the M6 seam",
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
        self.emit(Op::Mov {
            dst: Slot::overlay(0),
            src: Slot::body(only.slot),
        });
        let kidx = self.pool(funcref_const(Callee::Builtin(native)));
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
        self.rc.produced(dst, None);
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
                            let kidx = self.pool(Constant::FuncRef {
                                module: String::new(), // the enclosing module
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
                        } else if kind == ds_check::DefKind::Fn {
                            Err(LowerError::unsupported(
                                span,
                                "a function value",
                                "only planned module functions mint callable words; native and \
                                 generic callees have no word this round",
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

    /// `x is T`: a TAG_TEST against the tested atom, or a constant when
    /// the checked types already decide it (legal specialization).
    fn is_value(&mut self, expr: &Expr, ty: &Type, _span: Span) -> Result<Place, LowerError> {
        let operand = self.value(expr)?;
        let operand_ty = self.val(expr.span())?.ty;
        let tested = self.type_atom(ty)?;
        let members = self.members(operand_ty);
        let dst = self.temp_word(SlotType::Bool)?;
        if members.as_slice() == [tested] {
            let kidx = self.pool(Constant::Bool(true));
            self.emit(Op::Kconst {
                dst: Slot::body(dst),
                kidx,
            });
        } else if !members.contains(&tested) {
            let kidx = self.pool(Constant::Bool(false));
            self.emit(Op::Kconst {
                dst: Slot::body(dst),
                kidx,
            });
        } else {
            let src = self.tag_addr(&operand.place)?;
            let tag = self.shapes.tag_id(tested)?;
            self.emit(Op::TagTest {
                dst: Slot::body(dst),
                src: Slot::body(src),
                tag,
            });
        }
        Ok(Place {
            base: dst,
            shape: PlaceShape::Word(SlotType::Bool),
        })
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
        let field = match &*place_shape {
            Shape::Union(_) => {
                let atom = self.only_atom(recv_ty, e.span())?;
                let fields = self.member_fields(underlying, atom)?;
                fields
                    .into_iter()
                    .find(|f| f.name.as_deref() == Some(name.name.as_str()))
                    .ok_or_else(|| mismatch("a union member field"))?
            },
            Shape::Struct(s) => s
                .fields
                .iter()
                .find(|f| f.name.as_deref() == Some(name.name.as_str()))
                .cloned()
                .ok_or_else(|| mismatch("a struct field"))?,
            Shape::Word(_) | Shape::Tuple(_) => return Err(mismatch("a field access")),
        };
        Ok(Place {
            base: recv_val.place.base + field.word,
            shape: PlaceShape::Word(field.slot),
        })
    }

    /// A struct literal: a fresh span, fields written by name [B3].
    fn struct_literal(&mut self, e: &Expr, static_ty: Ty) -> Result<Place, LowerError> {
        let Expr::Struct { fields, .. } = e else {
            return Err(mismatch("a struct literal"));
        };
        let shape = self.shapes.shape(static_ty)?;
        let Shape::Struct(s) = &*shape else {
            return Err(mismatch("a struct literal"));
        };
        let target = self.temp_span(static_ty)?;
        for (name, value) in fields {
            let field = s
                .fields
                .iter()
                .find(|f| f.name.as_deref() == Some(name.name.as_str()))
                .ok_or_else(|| mismatch("a struct literal field"))?;
            let addr = if let Some(expr) = value {
                let v = self.value(expr)?;
                single(&self.word_addrs(&v.place, v.ty)?, expr.span())?
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
                single(&self.word_addrs(&place, ty)?, name.span)?
            };
            // a fn-typed field must prove scalar provenance: a closure word
            // (or a callable-defaulted binding that may hold one at
            // runtime) would write a Ref word into the Fn-classified field
            // slot and trap at the store
            if field.slot == SlotType::Fn && !self.scalar_fn_words.contains(&addr.slot) {
                return Err(LowerError::unsupported(
                    name.span,
                    "a fn-typed struct field",
                    "the value cannot prove a non-capturing scalar word; a closure or an unproven \
                     callable binding cannot inhabit a struct field",
                ));
            }
            self.emit(Op::Mov {
                dst: Slot::body(target.base + field.word),
                src: Slot::body(addr.slot),
            });
            if addr.slot_ty == SlotType::Ref {
                self.rc
                    .copied(&mut self.code, target.base + field.word, addr.slot);
            }
        }
        Ok(target)
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
            self.emit(Op::Mov {
                dst: Slot::body(target.base + field.word),
                src: Slot::body(addr.slot),
            });
            if field.slot == SlotType::Ref {
                self.rc
                    .copied(&mut self.code, target.base + field.word, addr.slot);
            }
        }
        Ok(target)
    }

    /// Binary operators: typed uops for scalars, short-circuit chains for
    /// `&&`/`||`, memberwise chains for structural `==` [B9].
    fn binary_value(&mut self, e: &Expr, _static_ty: Ty) -> Result<Place, LowerError> {
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
        let lhs = self.value(left)?;
        let rhs = self.value(right)?;
        let place = match op {
            BinaryOp::Eq | BinaryOp::Ne => self.eq_chain(&lhs, &rhs, e.span())?,
            BinaryOp::Add => self.arith(DataOp::AddI64, DataOp::AddF64, &lhs, &rhs, *span)?,
            BinaryOp::Sub => self.arith(DataOp::SubI64, DataOp::SubF64, &lhs, &rhs, *span)?,
            BinaryOp::Mul => self.arith(DataOp::MulI64, DataOp::MulF64, &lhs, &rhs, *span)?,
            BinaryOp::Div => self.arith(DataOp::DivI64, DataOp::DivF64, &lhs, &rhs, *span)?,
            BinaryOp::Rem => {
                self.reject_float_rem(&lhs, *span)?;
                self.arith(DataOp::ModI64, DataOp::ModI64, &lhs, &rhs, *span)?
            },
            BinaryOp::Lt => self.compare(DataOp::LtI64, DataOp::LtF64, &lhs, &rhs, *span)?,
            BinaryOp::Le => self.compare(DataOp::LeI64, DataOp::LeF64, &lhs, &rhs, *span)?,
            BinaryOp::Gt => self.compare(DataOp::GtI64, DataOp::GtF64, &lhs, &rhs, *span)?,
            BinaryOp::Ge => self.compare(DataOp::GeI64, DataOp::GeF64, &lhs, &rhs, *span)?,
            BinaryOp::WrapAdd
            | BinaryOp::WrapSub
            | BinaryOp::WrapMul
            | BinaryOp::Shl
            | BinaryOp::Shr
            | BinaryOp::BitAnd
            | BinaryOp::BitOr
            | BinaryOp::BitXor => {
                return Err(LowerError::unsupported(
                    *span,
                    "a bitwise, shift, or wrapping operator",
                    "outside the M3 uop contract",
                ))
            },
            BinaryOp::And | BinaryOp::Or | BinaryOp::In => unreachable!("handled above"),
        };
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

    /// Float remainder is a boundary, not a trap: the numeric table admits
    /// float rem [B7], but the locked uop set carries ModI64 only — both
    /// Rem sites (the binary operator and the compound assignment) check
    /// the left operand's word and reject F64 at compile time.
    fn reject_float_rem(&mut self, lhs: &TypedPlace, span: Span) -> Result<(), LowerError> {
        if let [only] = self.word_addrs(&lhs.place, lhs.ty)?.as_slice()
            && only.slot_ty == SlotType::F64
        {
            return Err(LowerError::unsupported(
                span,
                "a float remainder",
                "no ModF64 uop exists in the locked set; the numeric table admits float rem — \
                 library rem arrives with @std",
            ));
        }
        Ok(())
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
        let r = single(&self.word_addrs(&rhs.place, rhs.ty)?, span)?;
        let dst = self.temp_word(op.result_slot(l.slot_ty))?;
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

    /// Structural `==`: one uop per scalar, a fail-fast chain per struct,
    /// a tag dispatch plus payload compare per union [B9].
    fn eq_chain(
        &mut self,
        lhs: &TypedPlace,
        rhs: &TypedPlace,
        span: Span,
    ) -> Result<Place, LowerError> {
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
            Shape::Struct(s) => {
                let fail = self.label();
                let join = self.label();
                let l = self.word_addrs(&lhs.place, lhs.ty)?;
                let r = self.word_addrs(&rhs.place, rhs.ty)?;
                for (field, rf) in s.fields.iter().zip(r.iter()) {
                    let lf = l
                        .iter()
                        .find(|a| a.name == field.name)
                        .ok_or_else(|| mismatch("a struct field equality"))?;
                    let cond = self.temp_word(SlotType::Bool)?;
                    let op = eq_op(field.slot)?;
                    self.emit(Op::Data {
                        op,
                        dst: Slot::body(cond),
                        lhs: Slot::body(lf.slot),
                        rhs: Slot::body(rf.slot),
                    });
                    if op == DataOp::EqStr {
                        self.rc.read(&mut self.code, &[lf.slot, rf.slot]);
                    }
                    self.emit(Op::JmpifNot {
                        cond: Slot::body(cond),
                        target: fail,
                    });
                }
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
                for (field, (lf, rf)) in s.fields.iter().zip(l.iter().zip(r.iter())) {
                    let cond = self.temp_word(SlotType::Bool)?;
                    let op = eq_op(field.slot)?;
                    self.emit(Op::Data {
                        op,
                        dst: Slot::body(cond),
                        lhs: Slot::body(lf.slot),
                        rhs: Slot::body(rf.slot),
                    });
                    if op == DataOp::EqStr {
                        self.rc.read(&mut self.code, &[lf.slot, rf.slot]);
                    }
                    self.emit(Op::JmpifNot {
                        cond: Slot::body(cond),
                        target: fail,
                    });
                }
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

    /// Union equality: per-member tag tests, then the member's payload
    /// compare; distinct members never compare equal [B9].
    fn union_eq(&mut self, lhs: &TypedPlace, rhs: &TypedPlace, ty: Ty) -> Result<u8, LowerError> {
        let shape = self.shapes.shape(ty)?;
        let Shape::Union(u) = &*shape else {
            return Err(mismatch("a union equality"));
        };
        let members = u.members.clone();
        let ltag = self.tag_addr(&lhs.place)?;
        let rtag = self.tag_addr(&rhs.place)?;
        let false_lbl = self.label();
        let done = self.label();
        let dst = self.temp_word(SlotType::Bool)?;
        for member in &members {
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
                let field = member.fields[0].clone();
                let op = eq_op(field.slot)?;
                self.emit(Op::Data {
                    op,
                    dst: Slot::body(dst),
                    lhs: Slot::body(lhs.place.base + field.word),
                    rhs: Slot::body(rhs.place.base + field.word),
                });
                if op == DataOp::EqStr {
                    let words = [lhs.place.base + field.word, rhs.place.base + field.word];
                    self.rc.read(&mut self.code, &words);
                }
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
            UnaryOp::WrapNeg => {
                return Err(LowerError::unsupported(
                    span,
                    "a wrapping negation",
                    "no wrapping uop is in the M3 contract",
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
        let result = self.temp_span(ty)?;
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
        self.widen_into(&result, ty, &then_tp)?;
        let arm_then = self.rc.take();
        let then_div = self.rc.diverged();
        let then_jmp = self.code.len();
        self.emit(Op::Jmp { target: join });
        self.rc.restore(base);
        self.bind_label(else_lbl);
        if let Some(else_expr) = else_ {
            let else_tp = self.value(else_expr)?;
            self.widen_into(&result, ty, &else_tp)?;
        } else {
            // a missing else is the unit else [B2]: the checker already
            // types the if as the then-type united with unit
            let unit = self.unit_value(e.span())?;
            self.widen_into(&result, ty, &unit)?;
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
        let result = self.temp_span(ty)?;
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
    /// has no live fall-through.
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
        let mut catchall = None;
        for (index, arm) in arms.iter().enumerate() {
            let atoms = self.pattern_atoms(&arm.pat)?;
            if atoms.is_empty() {
                catchall = Some(index);
                continue;
            }
            for atom in atoms {
                let cond = self.temp_word(SlotType::Bool)?;
                let src = self.tag_addr(&scrut.place)?;
                let tag = self.shapes.tag_id(atom)?;
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
            if arm.guard.is_some() || arm.ann.is_some() {
                return Err(LowerError::unsupported(
                    arm.span,
                    "a guarded or annotated match arm",
                    "guards arrive with the record's guard lowering; payload patterns cover this \
                     round",
                ));
            }
            self.rc.restore(base.clone());
            self.bind_arm_pattern(&arm.pat, scrut)?;
            // a value arm decs at its own jump what only it kept; a tail
            // arm exits the frame, its exit path dropping from its own
            // state
            if let Some((result, ty, join)) = value {
                let body_tp = self.value(&arm.body)?;
                self.widen_into(result, ty, &body_tp)?;
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

    /// A match over a single-atom scrutinee (a tuple or a scalar): one
    /// member has no tags to dispatch on, so exactly one catchall-shaped
    /// arm lowers — the destructuring match `match t { (a, b) => ... }`.
    /// Disjoint tests need a union scrutinee; literal sub-patterns are
    /// value-level tests with no seam yet.
    fn match_single(
        &mut self,
        scrut: &TypedPlace,
        arms: &[MatchArm],
        value: Option<(&Place, Ty, usize)>,
    ) -> Result<(), LowerError> {
        let [arm] = arms else {
            let span = arms.first().map_or_else(no_span, |arm| arm.span);
            return Err(LowerError::unsupported(
                span,
                "a multi-arm match on a non-union value",
                "one destructuring arm covers a single-member scrutinee; disjoint tests need a \
                 union",
            ));
        };
        if arm.guard.is_some() || arm.ann.is_some() {
            return Err(LowerError::unsupported(
                arm.span,
                "a guarded or annotated match arm",
                "guards arrive with the record's guard lowering; this round's arms destructure",
            ));
        }
        match &arm.pat {
            Pattern::Tuple { .. } | Pattern::Binding { .. } | Pattern::Wildcard(_) => {},
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
                self.widen_into(result, ty, &body)?;
            },
            None => self.tail(&arm.body)?,
        }
        Ok(())
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
                "a tuple member of a union is a multi-word payload, outside this frame contract; \
                 destructure after a member test",
            )),
            Pattern::Literal { .. }
            | Pattern::Str { .. }
            | Pattern::RawStr { .. }
            | Pattern::Call { .. } => Err(LowerError::unsupported(
                pattern_span(pat),
                "a literal or constructor pattern",
                "member, or-, and catchall patterns cover the M3 surface",
            )),
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
    fn bind_arm_pattern(&mut self, pat: &Pattern, scrut: &TypedPlace) -> Result<(), LowerError> {
        match pat {
            Pattern::Binding { span, name } => {
                if self.binding_atom(&name.name).is_some() {
                    // a bare tag arm tests membership [F9]; nothing binds
                    return Ok(());
                }
                self.binders.insert(*span, scrut.place.clone());
                self.rc_bind(*span, scrut.place.base, scrut.ty)?;
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
                let member_fields = self.member_fields(underlying, atom)?;
                for (name, sub) in fields {
                    let field = member_fields
                        .iter()
                        .find(|f| f.name.as_deref() == Some(name.name.as_str()))
                        .ok_or_else(|| mismatch("a pattern field"))?;
                    let bind = match sub {
                        None => (name.span, name.name.clone()),
                        Some(Pattern::Binding { span, name }) => (*span, name.name.clone()),
                        _ => {
                            return Err(LowerError::unsupported(
                                name.span,
                                "a nested pattern field",
                                "only binding sub-patterns lower this round",
                            ))
                        },
                    };
                    self.binders.insert(bind.0, Place {
                        base: scrut.place.base + field.word,
                        shape: PlaceShape::Word(field.slot),
                    });
                    if let Some(fact) = self.analyzed.checked.binders.get(&bind.0).copied() {
                        self.rc_bind(bind.0, scrut.place.base + field.word, fact.ty)?;
                    }
                    self.scope_insert(&bind.1, bind.0);
                }
                Ok(())
            },
            Pattern::Tuple { pats, .. } => {
                self.bind_tuple_pattern(pats, scrut)?;
                Ok(())
            },
            _ => Ok(()),
        }
    }

    /// Binds one tuple pattern's elements to the tuple span's words:
    /// one word per element, bound in place (aliases, the const
    /// convention). Sub-patterns are plain names and `_` this round —
    /// a nested pattern would need a multi-word element.
    fn bind_tuple_pattern(
        &mut self,
        pats: &[Pattern],
        scrut: &TypedPlace,
    ) -> Result<(), LowerError> {
        let shape = self.shapes.shape(scrut.ty)?;
        let Shape::Tuple(s) = &*shape else {
            return Err(mismatch("a tuple destructuring"));
        };
        for (index, sub) in pats.iter().enumerate() {
            let Some(field) = s.fields.get(index) else {
                return Err(mismatch("a tuple pattern's arity"));
            };
            match sub {
                Pattern::Wildcard(_) => {},
                Pattern::Binding { span, name } => {
                    let base = scrut.place.base + field.word;
                    self.binders.insert(*span, Place {
                        base,
                        shape: PlaceShape::Word(field.slot),
                    });
                    if let Some(fact) = self.analyzed.checked.binders.get(span).copied() {
                        self.rc_bind(*span, base, fact.ty)?;
                    }
                    self.scope_insert(&name.name, *span);
                },
                _ => {
                    return Err(LowerError::unsupported(
                        pattern_span(sub),
                        "a nested tuple element pattern",
                        "tuple elements destructure by plain names and `_` this round; nested \
                         patterns need multi-word elements",
                    ))
                },
            }
        }
        Ok(())
    }

    // ---- calls ------------------------------------------------------------------

    /// The one-word-element guard both list construction paths share: a
    /// pushed element must be one scalar word — int, float, bool, tag, a
    /// heap Ref, or a callable's scalar FuncRef (a named fn or
    /// non-capturing lambda value). A bare struct or tuple element stages
    /// its raw field words, so a one-field struct would store its field's
    /// word as the element and reading it back hands that word to a place
    /// typed as the struct; wider or struct elements arrive with reboxing.
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
        let one_word = matches!(self.shapes.words(val.ty)?.as_slice(), [SlotType::I64
            | SlotType::F64
            | SlotType::Bool
            | SlotType::Tag
            | SlotType::Ref
            | SlotType::Fn]);
        let bare_fields = matches!(
            &*self.shapes.shape(val.ty)?,
            Shape::Struct(_) | Shape::Tuple(_)
        );
        if !one_word || bare_fields {
            return Err(LowerError::unsupported(
                span,
                what,
                "list elements lower as one scalar word; struct and tuple elements would store \
                 their raw field words as the element; reboxing covers wider elements",
            ));
        }
        if matches!(&*self.shapes.shape(val.ty)?, Shape::Word(SlotType::Fn)) {
            let addrs = self.word_addrs(&val.place, val.ty)?;
            let proven = addrs.len() == 1 && self.scalar_fn_words.contains(&addrs[0].slot);
            if !proven {
                return Err(LowerError::unsupported(
                    span,
                    what,
                    "a callable element must prove a non-capturing scalar word; a closure or an \
                     unproven callable parameter cannot inhabit a collection element",
                ));
            }
        }
        Ok(())
    }

    /// A reentrant combinator's lambda argument must answer one word:
    /// the VM's hosted-call seam returns a single value, and a
    /// multi-word (struct or tuple) return would silently keep only its
    /// first word. A non-fn-typed argument is not this guard's business
    /// — the call site's own checks report it.
    fn guard_hosted_lambda(&mut self, val: &TypedPlace, span: Span) -> Result<(), LowerError> {
        let Some((_, ret)) = fn_sig_of(&self.analyzed.decls.universe, val.ty) else {
            return Ok(());
        };
        if self.shapes.words(ret)?.len() != 1 {
            return Err(LowerError::unsupported(
                span,
                "a container-combinator lambda",
                "the hosted per-element call answers one word; a struct or tuple return needs \
                 reboxing",
            ));
        }
        Ok(())
    }

    /// The staged-span shape guards a call refuses before dispatch.
    /// `map_set`'s key and value lower as one machine word each: a wider
    /// argument (a union span) would shift the staged words under the
    /// native's positional contract. A local callee's slide contract is
    /// its parameter span: an argument narrower than the parameter
    /// region (an inline unit tag or payload case meeting a multi-word
    /// union parameter) would trap in the VM's slide validation, so the
    /// call refuses here with the seam it needs.
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
                    "map keys and values lower as one machine word; wider values arrive with \
                     reboxing",
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
                         union-typed parameter takes its member's whole region — pass a \
                         same-typed binding until argument widening lands",
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

    /// One list literal `[a, b, c]`: the empty `list_new` call, then one
    /// `list_push` per element — the list word staged first, the
    /// element's words after it (an element expression may itself
    /// call). The list is a freshly created Ref its frame uniquely
    /// owns, so the in-place appends carry no window discipline [B3].
    /// The staged element borrows or transfers by its own death: `list_push`
    /// acquires its own count when storing, so a dying element's post-call
    /// drop hands the count to the list (`native_pending`); a live element
    /// keeps its frame count and the list holds the acquired one.
    fn list_literal(&mut self, e: &Expr) -> Result<Place, LowerError> {
        let Expr::List { items, .. } = e else {
            return Err(mismatch("a list literal"));
        };
        let list = self.list_new_word()?;
        for item in items {
            let item_val = self.value(item)?;
            self.guard_list_element(&item_val, item.span(), "a list literal element")?;
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
            let kidx = self.pool(funcref_const(Callee::Builtin(push)));
            self.emit(Op::Call {
                callee: kidx,
                nargs: u8::try_from(staged.len()).expect("argument count fits u8"),
            });
            self.rc.native_done(&mut self.code);
        }
        Ok(Place {
            base: list,
            shape: PlaceShape::Word(SlotType::Ref),
        })
    }

    /// The empty-list word: the zero-argument `list_new` call whose one
    /// Ref result lands on the overlay (the empty `list_new` call still
    /// needs its one-word overlay area — the result must land
    /// somewhere), copied into a fresh body word the frame owns.
    fn list_new_word(&mut self) -> Result<u8, LowerError> {
        let native = crate::native::lookup("list_new")
            .expect("the list surface is covered by the native table");
        self.overlay_site(&[], &[SlotType::Ref]);
        let kidx = self.pool(funcref_const(Callee::Builtin(native)));
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
        self.rc.produced(list, None);
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
        let kidx = self.pool(funcref_const(Callee::Builtin(native)));
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
        self.rc.produced(map, None);
        Ok(Place {
            base: map,
            shape: PlaceShape::Word(SlotType::Ref),
        })
    }

    fn call_value(&mut self, e: &Expr, ty: Ty) -> Result<Place, LowerError> {
        self.call_impl(e, ty, false)
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
            let field = match &arg.name {
                Some(name) => s
                    .fields
                    .iter()
                    .find(|f| f.name.as_deref() == Some(name.name.as_str()))
                    .ok_or_else(|| mismatch("a case construction field"))?,
                None => s
                    .fields
                    .get(index)
                    .ok_or_else(|| mismatch("a case construction slot"))?,
            };
            let v = self.value(&arg.value)?;
            let addr = single(&self.word_addrs(&v.place, v.ty)?, arg.value.span())?;
            self.emit(Op::Mov {
                dst: Slot::body(target.base + field.word),
                src: Slot::body(addr.slot),
            });
            if addr.slot_ty == SlotType::Ref {
                self.rc
                    .copied(&mut self.code, target.base + field.word, addr.slot);
            }
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
        if let Some(block) = trailing {
            return Err(LowerError::unsupported(
                block.span(),
                "a trailing block argument",
                "block-argument sugar lowers in its own round; pass the lambda inline in \
                 parentheses",
            ));
        }
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
        // first argument — `f(x, a)`, zero runtime cost [F3]
        let (def, callee_span, arg_exprs): (DefId, Span, Vec<&Expr>) =
            if let Expr::Field { recv, name, .. } = &**callee {
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
                            *func,
                            name.span,
                            std::iter::once(&**recv)
                                .chain(args.iter().map(|arg| &arg.value))
                                .collect(),
                        )
                    },
                    Some(Res::Dot(DotRes::Methods { .. })) => {
                        return Err(LowerError::unsupported(
                            name.span,
                            "a trait or inherent method call",
                            "method dispatch needs instance evidence (the M6 dispatch seam); call \
                             the free function instead [F3]",
                        ))
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
                let Some(Res::Def(def)) = self.res.get(*span) else {
                    if let [.., last] = path.segments.as_slice() {
                        if matches!(self.res.get(last.span), Some(Res::TraitMethod(_))) {
                            return Err(LowerError::unsupported(
                                last.span,
                                "a trait-method call",
                                "Trait::f qualification dispatches through instances (the M6 \
                                 seam); a free function or UFCS call lowers instead [F3]",
                            ));
                        }
                    }
                    if let Some(Res::Local { bind }) = self.res.get(*span) {
                        // a clean check makes this a fn-typed binding: the
                        // call dispatches through its Fn word [F2]
                        return self.calls_value(ty, *bind, *span, args);
                    }
                    return Err(LowerError::unsupported(
                        *span,
                        "a callee name",
                        "it does not resolve to a package function",
                    ));
                };
                (*def, *span, args.iter().map(|arg| &arg.value).collect())
            };
        if case_ctor == Some(def) {
            return self.case_value(args, ty);
        }
        // The E4 extraction family inlines at the call site over the
        // checked receiver's own union shape — no callee, no control
        // transfer, word kinds from the call site like every covered
        // generic-safe native.
        if let Some(kind) = self.extraction_def(def) {
            return self.extraction_value(kind, &arg_exprs, ty, callee_span);
        }
        if let Some(place) = self.ambient_str_place(def, &arg_exprs, callee_span)? {
            return Ok(place);
        }
        let target = self.resolve_callee(def, callee_span)?;
        let arg_exprs = self.defaulted_args(def, target, arg_exprs);
        // A direct `list_push` stages its (list, value) pair with the
        // value as one word — the same element guard the literal path
        // applies, extended to this path so a struct argument rejects
        // here instead of trapping (or silently mis-staging one field
        // wide) in the VM. `cons` mirrors it with the element in the
        // receiver position: the one body prepends exactly one word.
        // `reduce`'s init seeds the folded accumulator with exactly one
        // word, so the same element guard covers it.
        let element_slot = match target {
            Callee::Builtin(native) if native.name == "list_push" => {
                Some((1usize, "a list_push element"))
            },
            Callee::Generic(generic) if generic.name == "cons" => Some((0usize, "a cons element")),
            Callee::Generic(generic) if generic.name == "reduce" => Some((1usize, "a reduce init")),
            _ => None,
        };
        // the reentrant combinators reenter the dispatcher per element,
        // and the hosted call answers one value word
        let hosted_lambda = matches!(
            target,
            Callee::Generic(generic) if matches!(generic.name, "map" | "filter" | "reduce")
        );
        // stage: evaluate every argument to a place, then move its words
        // into the overlay area (argument evaluation may itself call)
        let mut staged: Vec<SlotType> = Vec::new();
        let mut moves: Vec<(usize, u8)> = Vec::new();
        // staged indices whose Ref word lands in a callable-defaulted `Fn`
        // parameter (the closure-capable callable surface): the callee
        // reads the word and never owns its count, so the staging is a
        // borrow window, never a dup or a forward
        let mut defaulted: Vec<usize> = Vec::new();
        let expected_words = self.expected_param_words(target);
        // the receiver's checked type mints a generic call's evidence
        // words (the builtin instance it addresses) [H3]
        let mut receiver_ty: Option<Ty> = None;
        for (index, arg) in arg_exprs.iter().enumerate() {
            let arg_val = self.value(arg)?;
            if index == 0 {
                receiver_ty = Some(arg_val.ty);
            }
            if let Some((slot, what)) = element_slot {
                if slot == index {
                    self.guard_list_element(&arg_val, arg.span(), what)?;
                }
            }
            if hosted_lambda && index + 1 == arg_exprs.len() {
                self.guard_hosted_lambda(&arg_val, arg.span())?;
            }
            // a closure argument lands only in a callable-defaulted `Fn`
            // parameter; its one Ref word marks the borrow-window index
            if self.is_closure_word(&arg_val.place, arg_val.ty)? {
                Self::guard_callable_staged(expected_words.as_deref(), staged.len(), arg.span())?;
                defaulted.push(staged.len());
            }
            for addr in self
                .word_addrs(&arg_val.place, arg_val.ty)
                .map_err(|err| err.respanned(arg.span()))?
            {
                moves.push((staged.len(), addr.slot));
                staged.push(addr.slot_ty);
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
        self.guard_staged_count(staged.len())?;
        for (index, slot) in moves {
            self.emit(Op::Mov {
                dst: Slot::overlay(u8::try_from(index).expect("overlay word fits u8")),
                src: Slot::body(slot),
            });
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
            if staged[index] == SlotType::Ref {
                if defaulted.contains(&index) {
                    self.rc.staged_native(slot, false);
                } else {
                    match target {
                        Callee::Builtin(_) | Callee::Generic(_) => {
                            self.rc.staged_native(slot, tail && self.root);
                        },
                        Callee::Local(_) => self.rc.staged_local(&mut self.code, slot),
                    }
                }
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
        // callee's ONE body through CALLG
        let generic_target = match target {
            Callee::Generic(generic) => Some(generic),
            _ => None,
        };
        let mut generic_instance: Option<BuiltinInstance> = None;
        if let Some(generic) = generic_target {
            if tail {
                return Err(LowerError::unsupported(
                    callee_span,
                    "a tail call to a generic builtin",
                    "CALLTG, the generic tail variant, stays reserved; keep the call out of tail \
                     position",
                ));
            }
            generic_instance = Some(self.stage_generic_evidence(
                generic,
                def,
                callee_span,
                *span,
                &mut staged,
                receiver_ty,
            )?);
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
        let kidx = self.pool(funcref_const(target));
        // A native tail call transfers control only from the root frame:
        // CALLT-to-native pops the frame inside the instruction, so the
        // staged borrows' decs have no code position there — sound solely
        // because the root frame's transfer ends the program (the heap
        // dies whole). From any other frame the same tail position lowers
        // in place: CALL + RET is stack-identical (the native runs inside
        // the CALL instruction, no frame is pushed), and the post-call
        // flush plus the return's exit drops balance every count.
        let native_tail_return = tail && !self.root && matches!(target, Callee::Builtin(_));
        // a closure-staging local tail lowers in place for the root frame
        // too: the callee reads the word during the call, so the count
        // cannot ride the abandoned corner nor die at the pre-transfer
        // exit drops
        let in_place_tail = native_tail_return || closure_tail;
        if tail && !in_place_tail {
            // control transfers to the callee; the stack never grows [H2].
            // A local callee owns its handed words (its own exit drops
            // them); every still-owned reference drops before the
            // transfer, and a root-frame native's staged borrows are the
            // abandoned corner above.
            self.rc.native_done(&mut self.code);
            self.rc.exit(&mut self.code, &[]);
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
        // the staged span of a generic call ends in its evidence words;
        // nargs names the real arguments, and the callee's own constraint
        // list carries the count the dispatch reads
        if generic_target.is_some() {
            let real_args = staged.len() - usize::from(generic_target.map_or(0, |g| g.evidence));
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
        let place = self.result_place(ty, &result_words)?;
        if in_place_tail {
            // the in-place tail call (a non-root native tail, or a local
            // tail staging a closure into a callable-defaulted parameter):
            // the tail position returns the call's result, the post-call
            // flush settles the borrow windows, and the return's exit
            // drops flush every other count the frame owes
            self.emit_return(&place, ty)?;
            // an unreachable marker place; no code follows the return
            return Ok(Place {
                base: 0,
                shape: PlaceShape::Word(SlotType::Raw),
            });
        }
        Ok(place)
    }

    /// Copies one call's result span out of the overlay into a fresh temp:
    /// the callee transferred its result references out with the return.
    fn result_place(&mut self, ty: Ty, result_words: &[SlotType]) -> Result<Place, LowerError> {
        let base = self.alloc(result_words)?;
        for (index, slot) in result_words.iter().enumerate() {
            let word = base + u8::try_from(index).expect("result width fits u8");
            self.emit(Op::Mov {
                dst: Slot::body(word),
                src: Slot::overlay(u8::try_from(index).expect("result word fits u8")),
            });
            if *slot == SlotType::Ref {
                self.rc.produced(word, None);
            }
        }
        let shape = match &*self.shapes.shape(ty)? {
            Shape::Word(slot) => PlaceShape::Word(*slot),
            _ => PlaceShape::Span(ty),
        };
        Ok(Place { base, shape })
    }

    /// CALLS (the callable-word call form, F2): the callee is a local
    /// binding whose single word names the function — a scalar Fn word,
    /// or a Ref word naming a closure object (the read dispatches on the
    /// word's kind; the emitted op is the same). Arguments stage on the
    /// overlay exactly like a pool call, the callee frame overlays it,
    /// and the result copies out of the overlay. There is no tail
    /// variant, so a tail-position call lowers through the value walk
    /// and the enclosing return RETs its result.
    fn calls_value(
        &mut self,
        ty: Ty,
        bind: Span,
        callee_span: Span,
        args: &[CallArg],
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
        let expected_words: Vec<SlotType> = params
            .iter()
            .map(|ty| self.shapes.words(*ty))
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .flatten()
            .collect();
        let expected = expected_words.len();
        // stage: evaluate every argument to a place, then move its words
        // into the overlay area (argument evaluation may itself call)
        let mut staged: Vec<SlotType> = Vec::new();
        let mut moves: Vec<(usize, u8)> = Vec::new();
        // staged indices whose Ref word lands in a callable-defaulted `Fn`
        // parameter: the callee reads the word and never owns its count
        let mut defaulted: Vec<usize> = Vec::new();
        for arg in args {
            let arg_val = self.value(&arg.value)?;
            // a closure argument lands only in a callable-defaulted `Fn`
            // parameter, staged as a borrow window
            if self.is_closure_word(&arg_val.place, arg_val.ty)? {
                Self::guard_callable_staged(Some(&expected_words), staged.len(), arg.value.span())?;
                defaulted.push(staged.len());
            }
            for addr in self
                .word_addrs(&arg_val.place, arg_val.ty)
                .map_err(|err| err.respanned(arg.value.span()))?
            {
                moves.push((staged.len(), addr.slot));
                staged.push(addr.slot_ty);
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
        for (index, slot) in moves {
            self.emit(Op::Mov {
                dst: Slot::overlay(u8::try_from(index).expect("overlay word fits u8")),
                src: Slot::body(slot),
            });
            // a borrow parameter owns the handed word: forward on the
            // source's death, dup otherwise (C2/C4); a callable-defaulted
            // closure word is a plain borrow — the callee reads it (CALLS
            // never consumes) and never owns its count
            if staged[index] == SlotType::Ref {
                if defaulted.contains(&index) {
                    self.rc.staged_native(slot, false);
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
        if result_words.as_slice() == [SlotType::Fn] {
            if let Some(&index) = self.fn_words.get(&word.slot) {
                if self.tail_builds_a_closure(index) {
                    result_words = vec![SlotType::Ref];
                }
            }
        }
        self.overlay_site(&staged, &result_words);
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
        self.result_place(ty, &result_words)
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
                    "the platform failure span has no source-level return type; keep the call out \
                     of tail position",
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
    /// the result shape's instance-keyed word kind.
    fn stage_generic_evidence(
        &mut self,
        generic: &'static GenericNative,
        def: DefId,
        span: Span,
        call: Span,
        staged: &mut Vec<SlotType>,
        receiver_ty: Option<Ty>,
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
            let kidx = self.pool(Constant::I64(instance as i64));
            self.emit(Op::Kimm {
                dst: Slot::overlay(u8::try_from(staged.len()).expect("overlay word fits u8")),
                kidx,
            });
            staged.push(SlotType::I64);
        }
        Ok(instance)
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
                    "a generic body's dispatch is the M6 CALL_GENERIC seam",
                ));
            }
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

    /// A local callee's parameter word count, as its own frame layout
    /// records it (params first, one region per parameter). `None` when
    /// a parameter type has no shape; those callees keep the runtime
    /// arity check.
    fn callee_param_words(&mut self, index: u32) -> Option<usize> {
        let planned = self.plan.functions().get(index as usize)?;
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
    ) -> Result<Vec<SlotType>, LowerError> {
        match target {
            Callee::Builtin(native) => match native.shape {
                NativeShape::Total(slot) => Ok(vec![slot]),
                // call_impl routes fallible and checked natives through
                // their own tag-test paths; this generic copy never sees
                // them
                NativeShape::Fallible { .. } => Err(mismatch("a fallible native result")),
                NativeShape::Checked => Err(mismatch("a checked-read native result")),
                NativeShape::TotalByInstance => Err(mismatch("an instance-keyed native result")),
                NativeShape::TotalByResultType => Err(mismatch("a result-keyed native result")),
            },
            Callee::Generic(generic) => match generic.shape {
                NativeShape::Total(slot) => Ok(vec![slot]),
                NativeShape::TotalByInstance => match instance {
                    Some(BuiltinInstance::Int) => Ok(vec![SlotType::I64]),
                    Some(BuiltinInstance::Float) => Ok(vec![SlotType::F64]),
                    _ => Err(mismatch("an instance-keyed generic result")),
                },
                NativeShape::TotalByResultType => self.shapes.words(ty),
                NativeShape::Fallible { .. } | NativeShape::Checked => {
                    Err(mismatch("a generic builtin result"))
                },
            },
            Callee::Local(index) => {
                let planned = &self.plan.functions()[index as usize];
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
                if words.as_slice() == [SlotType::Fn] && self.tail_builds_a_closure(index) {
                    return Ok(vec![SlotType::Ref]);
                }
                Ok(words)
            },
        }
    }

    /// One fallible native call: the two-word platform span (discriminant
    /// at overlay word 0, payload at word 1) lands like any call result;
    /// success extracts the payload into the call's result temp, failure
    /// prints `<prefix><message>` through println and halts on the
    /// discriminant. The print-and-halt is the M3 stand-in for the
    /// unhandled-failure handler the `error` row has no source syntax for.
    fn fallible_native_result(
        &mut self,
        native: &'static Native,
        ty: Ty,
        payload: SlotType,
        err_prefix: &'static str,
        staged: &[SlotType],
    ) -> Result<Place, LowerError> {
        if self.shapes.words(ty)? != [payload] {
            return Err(LowerError::unsupported(
                no_span(),
                "a fallible native call",
                "the checked result type's words and the native's payload disagree; list values \
                 have no runtime shape yet",
            ));
        }
        self.overlay_fallible(staged, payload);
        let kidx = self.pool(funcref_const(Callee::Builtin(native)));
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
        let kidx = self.pool(Constant::Str(err_prefix.to_owned()));
        self.emit(Op::Kstr {
            dst: Slot::body(prefix),
            kidx,
        });
        self.rc.produced(prefix, None);
        let message = self.temp_word(SlotType::Ref)?;
        self.emit(Op::Mov {
            dst: Slot::body(message),
            src: Slot::overlay(1),
        });
        self.rc.produced(message, None);
        let text = self.temp_word(SlotType::Ref)?;
        self.emit(Op::Data {
            op: DataOp::StrCat,
            dst: Slot::body(text),
            lhs: Slot::body(prefix),
            rhs: Slot::body(message),
        });
        self.rc.read(&mut self.code, &[prefix, message]);
        self.rc.produced(text, None);
        self.emit(Op::Mov {
            dst: Slot::overlay(0),
            src: Slot::body(text),
        });
        self.rc.staged_native(text, false);
        let println = crate::native::lookup("println").expect("the console native is covered");
        let kidx = self.pool(funcref_const(Callee::Builtin(println)));
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
            self.rc.produced(base, None);
        }
        let shape = match &*self.shapes.shape(ty)? {
            Shape::Word(slot) => PlaceShape::Word(*slot),
            _ => PlaceShape::Span(ty),
        };
        Ok(Place { base, shape })
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

    /// One checked-read native call (`list_get`, `map_get`): the
    /// two-word ok/none span lands like a fallible native's, but neither
    /// side is a failure — it re-spans into the call's own `t | none`
    /// union: ok writes the element member with the payload word, none
    /// the predeclared unit member, so `?`, `.ok`, and match see an
    /// ordinary optional value. The element must be one single-word
    /// atom; the rejected index the none side carries has no member
    /// field to surface in, so it drops. The generic spelling routes the
    /// same span through CALLG, the evidence words staged behind the
    /// real arguments.
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
            .ok_or_else(|| mismatch("a checked read's none member"))?
            .clone();
        let element = set_diff(&self.members(ty), &[TypeId::NONE]);
        let [elem] = element.as_slice() else {
            return Err(LowerError::unsupported(
                no_span(),
                "a checked-read native call",
                "the element type must be one atom; narrow list elements before the read",
            ));
        };
        let elem_member = u
            .member(*elem)
            .ok_or_else(|| mismatch("a checked read's element member"))?
            .clone();
        let [field] = elem_member.fields.as_slice() else {
            return Err(LowerError::unsupported(
                no_span(),
                "a checked-read native call",
                "the element type must be one word; rebox wider elements first",
            ));
        };
        self.overlay_site(staged, &[SlotType::Tag, field.slot]);
        let kidx = self.pool(funcref_const(target));
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
            Callee::Local(_) => return Err(mismatch("a checked-read callee")),
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
        self.emit(Op::Mov {
            dst: Slot::body(result.base + field.word),
            src: Slot::overlay(1),
        });
        if field.slot == SlotType::Ref {
            self.rc.produced(result.base + field.word, None);
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
        let handled = set_diff(&self.members(src_ty), &self.members(ty));
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
        let kept = set_diff(&self.members(src_ty), &handled);
        if kept.len() == 1 && !self.is_span_ty(ty) {
            // the result is the one kept atom itself: claiming that
            // atom is honest — the word-address path extracts the
            // member's payload words out of the operand's layout
            self.widen_into(&result, ty, &TypedPlace {
                place: src.place.clone(),
                ty,
            })?;
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
            self.rc_bind(param.name.span, src.place.base, param_ty)?;
            self.scope_insert(&param.name.name, param.name.span);
            let body_tp = self.value(body)?;
            self.widen_into(&result, ty, &body_tp)?;
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
        let remainder = set_diff(&self.members(src_ty), &handled);
        let mut flow_jmps = Vec::new();
        if remainder.is_empty() {
            // every member jumps to the handler; the fallthrough is dead
            self.emit(Op::Jmp { target: join });
        } else if self.members(src_ty).len() == 1 {
            // a single-member operand: the value itself flows, no tags
            self.widen_into(&result, ty, &TypedPlace {
                place: src.place.clone(),
                ty: src_ty,
            })?;
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
            self.rc_bind(param.name.span, src.place.base, param_ty)?;
            self.scope_insert(&param.name.name, param.name.span);
            let body_tp = self.value(body)?;
            self.widen_into(&result, ty, &body_tp)?;
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
    /// union's none member and its single one-word t side — the same
    /// restriction the checked reads carry; wider sides wait on reboxing.
    fn optional_side(
        &mut self,
        recv_ty: Ty,
        span: Span,
    ) -> Result<(TypeId, MemberField), LowerError> {
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
        let diff = set_diff(&self.members(recv_ty), &[TypeId::NONE]);
        let [elem] = diff.as_slice() else {
            return Err(LowerError::unsupported(
                span,
                "an extraction-family receiver",
                "the extracted side must be one atom; narrow the receiver first",
            ));
        };
        let elem_member = u
            .member(*elem)
            .ok_or_else(|| mismatch("an optional receiver's extracted member"))?
            .clone();
        let [field] = elem_member.fields.as_slice() else {
            return Err(LowerError::unsupported(
                span,
                "an extraction-family receiver",
                "the extracted side must be one word; rebox wider members first",
            ));
        };
        Ok((*elem, field.clone()))
    }

    /// The receiver's one-word t side read out of its union span; the
    /// call's own checked type is the extracted type, so it shapes the
    /// payload place.
    fn extracted_place(
        &mut self,
        base: u8,
        field: &MemberField,
        ty: Ty,
    ) -> Result<Place, LowerError> {
        self.place_of_ty(base + field.word, ty)
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
        let (_, field) = self.optional_side(src_ty, span)?;
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
        self.widen_into(&result, ty, &TypedPlace {
            place: fb.place.clone(),
            ty: fb.ty,
        })?;
        let arm_none = self.rc.take();
        let none_div = self.rc.diverged();
        let none_jmp = self.code.len();
        self.emit(Op::Jmp { target: join });
        self.rc.restore(base);
        self.bind_label(ok_lbl);
        let payload = self.extracted_place(src.place.base, &field, ty)?;
        self.widen_into(&result, ty, &TypedPlace { place: payload, ty })?;
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
        let (_, field) = self.optional_side(src_ty, span)?;
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
        self.widen_into(&result, ty, &body_tp)?;
        self.scope_pop();
        let arm_none = self.rc.take();
        let none_div = self.rc.diverged();
        let none_jmp = self.code.len();
        self.emit(Op::Jmp { target: join });
        self.rc.restore(base);
        self.bind_label(ok_lbl);
        let payload = self.extracted_place(src.place.base, &field, ty)?;
        self.widen_into(&result, ty, &TypedPlace { place: payload, ty })?;
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
        let src = self.value(recv)?;
        let src_ty = self.val(recv.span())?.ty;
        let (elem, field) = self.optional_side(src_ty, span)?;
        let ev = self.value(err)?;
        let err_atom = self.only_atom(ev.ty, err.span())?;
        let err_words = self.word_addrs(&ev.place, ev.ty)?;
        let result = self.temp_span(ty)?;
        let shape = self.shapes.shape(ty)?;
        let Shape::Union(u) = &*shape else {
            return Err(mismatch("an ok_or result union"));
        };
        let none_tag = self.shapes.tag_id(TypeId::NONE)?;
        let err_tag = self.shapes.tag_id(err_atom)?;
        let elem_tag = self.shapes.tag_id(elem)?;
        let err_member = u
            .member(err_atom)
            .ok_or_else(|| mismatch("an ok_or error member"))?
            .clone();
        let elem_dst = u
            .member(elem)
            .ok_or_else(|| mismatch("an ok_or result's extracted member"))?
            .clone();
        let [dst_field] = elem_dst.fields.as_slice() else {
            return Err(LowerError::unsupported(
                span,
                "an ok_or result",
                "the extracted side must be one word; rebox wider members first",
            ));
        };
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
        for dst_field in &err_member.fields {
            let addr = src_word_for(&err_words, dst_field.name.as_deref())?;
            self.emit(Op::Mov {
                dst: Slot::body(result.base + dst_field.word),
                src: Slot::body(addr.slot),
            });
            if dst_field.slot == SlotType::Ref {
                self.rc
                    .copied(&mut self.code, result.base + dst_field.word, addr.slot);
            }
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
        self.emit(Op::Mov {
            dst: Slot::body(result.base + dst_field.word),
            src: Slot::body(src.place.base + field.word),
        });
        if dst_field.slot == SlotType::Ref {
            self.rc.copied(
                &mut self.code,
                result.base + dst_field.word,
                src.place.base + field.word,
            );
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
    /// union via RETSPAN [E4]; the kept set extracts in place.
    fn try_value(&mut self, e: &Expr, ty: Ty) -> Result<Place, LowerError> {
        let Expr::Try { expr, .. } = e else {
            return Err(mismatch("a `?` propagation"));
        };
        let src = self.value(expr)?;
        let src_ty = self.val(expr.span())?.ty;
        let propagated = set_diff(&self.members(src_ty), &self.members(ty));
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
        for (member, exit) in propagated.iter().zip(exits) {
            self.bind_label(exit);
            self.respan_exit(&src.place, *member)?;
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
        match members.as_slice() {
            [] => Err(mismatch("a `?` over an empty operand union")),
            [_] => self.emit_return(&src.place, src_ty),
            _ => {
                let mut exits = Vec::with_capacity(members.len());
                for member in &members {
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

    /// Whether a call's callee can transfer control in tail position: a
    /// name call, or a UFCS dot-call onto a resolved free function [F3].
    /// Handlers inline, so `.err` stays a value-position call.
    fn is_tail_callee(&self, callee: &Expr) -> bool {
        match callee {
            // A minted case constructor produces a value in place; it
            // never transfers control, so tail position lowers it as a
            // plain value [B3]. The extraction family inlines too, so a
            // tail-position family call lowers through the value walk's
            // return, not a control transfer [E4].
            Expr::Path { span, .. } => {
                // CALLS has no tail variant: a call through a local
                // binding lowers as a value and the enclosing return RETs
                // its result.
                if matches!(self.res.get(*span), Some(Res::Local { .. })) {
                    return false;
                }
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
            Expr::Field { name, .. } => matches!(
                self.res.get(name.span),
                Some(Res::Dot(DotRes::Free { func, .. })) if self.extraction_def(*func).is_none()
            ),
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
                    self.emit_return(&unit.place, unit.ty)
                };
                let arm_else = self.rc.take();
                // both arms exit the frame: each exit path dropped from
                // its own state, so nothing survives the join
                self.rc.reconcile(&[(arm_then, true), (arm_else, true)]);
                result
            },
            Expr::Match { .. } => self.match_tail(e),
            Expr::Try { expr, .. } => self.try_tail(expr),
            Expr::Call { callee, .. } if self.is_tail_callee(callee) => {
                let ty = self.val(e.span())?.ty;
                self.call_impl(e, ty, true)?;
                Ok(())
            },
            _ => {
                let tp = self.value(e)?;
                self.emit_return(&tp.place, tp.ty)
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
                self.emit_return(&unit.place, unit.ty)
            },
            None => {
                let unit = self.unit_value(block.span)?;
                self.emit_return(&unit.place, unit.ty)
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
        self.emit_return(&tp.place, tp.ty)?;
        self.unit_value(*span)
    }

    /// Returns one value in the enclosing function's return shape. The
    /// returned words transfer their references out with the frame; every
    /// other still-owned reference drops right before the exit op.
    fn emit_return(&mut self, place: &Place, src_ty: Ty) -> Result<(), LowerError> {
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
                for (field, src_addr) in s.fields.iter().zip(addrs.iter()) {
                    self.emit(Op::Mov {
                        dst: Slot::body(target.base + field.word),
                        src: Slot::body(src_addr.slot),
                    });
                    if field.slot == SlotType::Ref {
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
            Shape::Tuple(s) => {
                let target = self.temp_span(self.ret_ty)?;
                let addrs = self.word_addrs(place, src_ty)?;
                if addrs.len() != s.fields.len() {
                    return Err(mismatch("a tuple return's arity"));
                }
                for (field, src_addr) in s.fields.iter().zip(addrs.iter()) {
                    self.emit(Op::Mov {
                        dst: Slot::body(target.base + field.word),
                        src: Slot::body(src_addr.slot),
                    });
                    if field.slot == SlotType::Ref {
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
                    for member in &src_members {
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
                    .ok_or_else(|| mismatch("a union member return"))?
                    .clone();
                let target = self.temp_span(self.ret_ty)?;
                let tag = self.shapes.tag_id(atom)?;
                self.emit(Op::TagNew {
                    dst: Slot::body(target.base),
                    tag,
                });
                self.zero_init_member(target.base, &u.words, &member.fields)?;
                let addrs = self.word_addrs(place, src_ty)?;
                for field in &member.fields {
                    let src_addr = src_word_for(&addrs, field.name.as_deref())?;
                    self.emit(Op::Mov {
                        dst: Slot::body(target.base + field.word),
                        src: Slot::body(src_addr.slot),
                    });
                    if field.slot == SlotType::Ref {
                        self.rc
                            .copied(&mut self.code, target.base + field.word, src_addr.slot);
                    }
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
        let entry = u
            .member(member)
            .ok_or_else(|| mismatch("a re-span member"))?
            .clone();
        let src_tag = self.tag_addr(src)?;
        self.emit(Op::Mov {
            dst: Slot::body(dst.base),
            src: Slot::body(src_tag),
        });
        self.zero_init_member(dst.base, &u.words, &entry.fields)?;
        let PlaceShape::Span(underlying) = src.shape else {
            return Err(mismatch("a re-span source"));
        };
        let fields = self.member_fields(underlying, member)?;
        for field in &entry.fields {
            let src_field = fields
                .iter()
                .find(|f| f.name == field.name)
                .ok_or_else(|| mismatch("a re-span field"))?;
            self.emit(Op::Mov {
                dst: Slot::body(dst.base + field.word),
                src: Slot::body(src.base + src_field.word),
            });
            if field.slot == SlotType::Ref {
                self.rc.copied(
                    &mut self.code,
                    dst.base + field.word,
                    src.base + src_field.word,
                );
            }
        }
        Ok(())
    }

    /// Writes one value into a destination span: same-type word moves, or
    /// a member construction with TAG_NEW into a union [BL-2].
    fn widen_into(&mut self, dst: &Place, dst_ty: Ty, src: &TypedPlace) -> Result<(), LowerError> {
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
                if addr.slot_ty == SlotType::Ref {
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
                if addr.slot_ty == SlotType::Ref {
                    self.rc.copied(&mut self.code, dst.base, addr.slot);
                }
                Ok(())
            },
            Shape::Struct(s) => {
                let addrs = self.word_addrs(&src.place, src.ty)?;
                for field in &s.fields {
                    let addr = src_word_for(&addrs, field.name.as_deref())?;
                    self.emit(Op::Mov {
                        dst: Slot::body(dst.base + field.word),
                        src: Slot::body(addr.slot),
                    });
                    if addr.slot_ty == SlotType::Ref {
                        self.rc
                            .copied(&mut self.code, dst.base + field.word, addr.slot);
                    }
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
                    if addr.slot_ty == SlotType::Ref {
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
                    for member in &src_members {
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
                let atom = self.only_atom(src.ty, no_span())?;
                let member = u
                    .member(atom)
                    .ok_or_else(|| mismatch("a union member write"))?
                    .clone();
                let tag = self.shapes.tag_id(atom)?;
                self.emit(Op::TagNew {
                    dst: Slot::body(dst.base),
                    tag,
                });
                self.zero_init_member(dst.base, &u.words, &member.fields)?;
                let addrs = self.word_addrs(&src.place, src.ty)?;
                for field in &member.fields {
                    let addr = src_word_for(&addrs, field.name.as_deref())?;
                    self.emit(Op::Mov {
                        dst: Slot::body(dst.base + field.word),
                        src: Slot::body(addr.slot),
                    });
                    if addr.slot_ty == SlotType::Ref {
                        self.rc
                            .copied(&mut self.code, dst.base + field.word, addr.slot);
                    }
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
    fn finalize(self) -> Result<(ds_mod::Function, Vec<PlacedOp>), LowerError> {
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
        Ok((function, placed))
    }
}

// zero loads for the dead payload words of a constructed member: a
// union span's every word carries a value of its declared kind, so
// whole-span moves and typed callee parameters never see a foreign kind
// (the live member's words are the only ones ever read [BL-2])
impl FnLower<'_, '_, '_> {
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
            SlotType::Ref => {
                let k = self.pool(Constant::Str(String::new()));
                self.emit(Op::Kstr {
                    dst: Slot::body(dst),
                    kidx: k,
                });
                // the dead payload word owns its interned empty-string
                // reference like any production
                self.rc.produced(dst, None);
                return;
            },
            // Fn words are scalar packed FuncRefs: no heap resource to zero
            SlotType::Tag | SlotType::I32 | SlotType::Raw | SlotType::Fn => return,
        };
        self.emit(Op::Kconst {
            dst: Slot::body(dst),
            kidx,
        });
    }

    /// Writes kind-correct zeros into every payload word the constructed
    /// member does not fill.
    fn zero_init_member(
        &mut self,
        base: u8,
        words: &[SlotType],
        member_fields: &[MemberField],
    ) -> Result<(), LowerError> {
        let live: HashSet<u8> = member_fields.iter().map(|f| f.word).collect();
        for (index, slot) in words.iter().enumerate().skip(1) {
            let word = u8::try_from(index).map_err(|_| mismatch("a union span"))?;
            if !live.contains(&word) {
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

fn funcref_const(target: Callee) -> Constant {
    match target {
        Callee::Builtin(native) => Constant::FuncRef {
            module: native.module.name().to_owned(),
            func: native.index,
        },
        Callee::Generic(generic) => Constant::FuncRef {
            module: generic.module.name().to_owned(),
            func: generic.index,
        },
        Callee::Local(index) => Constant::FuncRef {
            module: String::new(), // the enclosing module
            func: index,
        },
    }
}

/// The single identifier of a one-segment path expression, if it is one.
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

fn src_word_for(addrs: &[WordAddr], name: Option<&str>) -> Result<WordAddr, LowerError> {
    addrs
        .iter()
        .find(|a| match name {
            Some(n) => a.name.as_deref() == Some(n),
            None => a.name.is_none(),
        })
        .cloned()
        .ok_or_else(|| mismatch("a value field"))
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
    Span::new(ds_diag::FileId::new(0), 0, 0)
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
