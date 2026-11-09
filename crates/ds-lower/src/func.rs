//! Per-function lowering: slots, the symbolic uop buffer, and the
//! expression walk.
//!
//! The walk consumes the checked tables: every expression's solved type
//! (flow-refined inside branches [B2]) comes from `expr_types`, every
//! binder's declared type from `binders`, and every name use from the
//! file's resolutions. Tail positions lower to RET/RETSPAN/CALLT
//! directly [H2]; every other position lowers to a place — a span of body
//! slots — that callers read or widen into their own spans.

use std::collections::{HashMap, HashSet};

use ds_abi::{ConstIdx, SlotType, Uop};
use ds_check::{Analyzed, DefId, Res, Resolutions, Ty, TypeId, Val};
use ds_diag::Span;
use ds_mod::Constant;
use ds_parse::{
    BinaryOp, BlockExpr, Expr, Literal, LocalDecl, MatchArm, OrTail, Param, Path, Pattern, Stmt,
    StmtKind, StrPart, Type, UnaryOp,
};

use crate::{
    error::LowerError,
    native::{BUILTIN_MODULE, Native, NativeShape, PLATFORM_IO_OK},
    shape::{MemberField, Shape, Shapes},
};

/// One symbolic frame slot: a body word, or an outgoing-overlay word that
/// resolves against the frame tail at finalization.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Slot {
    index: u8,
    overlay: bool,
}

impl Slot {
    const fn body(index: u8) -> Self {
        Self {
            index,
            overlay: false,
        }
    }

    const fn overlay(index: u8) -> Self {
        Self {
            index,
            overlay: true,
        }
    }
}

/// One-word data operations the buffer carries symbolically.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum DataOp {
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
#[derive(Clone, Copy, Debug)]
enum Op {
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

/// One planned function: name plus defining binding.
pub(crate) struct PlannedFn<'a> {
    pub(crate) name: String,
    pub(crate) def: DefId,
    pub(crate) local: &'a LocalDecl,
}

/// The module plan: lowered functions in function-table order.
pub(crate) struct Plan<'a> {
    functions: Vec<PlannedFn<'a>>,
    index: HashMap<DefId, u32>,
}

impl<'a> Plan<'a> {
    pub(crate) fn new() -> Self {
        Self {
            functions: Vec::new(),
            index: HashMap::new(),
        }
    }

    pub(crate) fn push(&mut self, name: String, def: DefId, local: &'a LocalDecl) {
        self.index.insert(
            def,
            u32::try_from(self.functions.len()).expect("function index fits u32"),
        );
        self.functions.push(PlannedFn { name, def, local });
    }

    /// The function-table index of a planned DefId.
    pub(crate) fn index_of(&self, def: DefId) -> Option<u32> {
        self.index.get(&def).copied()
    }

    pub(crate) fn functions(&self) -> &[PlannedFn<'a>] {
        &self.functions
    }

    pub(crate) fn names(&self) -> Vec<String> {
        self.functions.iter().map(|f| f.name.clone()).collect()
    }

    pub(crate) fn has_main(&self) -> bool {
        self.functions.iter().any(|f| f.name == "main")
    }
}

/// A call target: a module-local function or a contract builtin.
#[derive(Clone, Copy)]
enum Callee {
    /// A builtin native in the reserved `builtin` module.
    Builtin(&'static Native),
    /// This module's function-table index.
    Local(u32),
}

/// One function's lowering state.
pub(crate) struct FnLower<'a, 'p, 'q, 's> {
    analyzed: &'a Analyzed,
    res: &'a Resolutions,
    plan: &'p Plan<'q>,
    shapes: &'s mut Shapes<'a>,
    name: String,
    ret_ty: Ty,
    params_len: usize,
    slots: Vec<SlotType>,
    code: Vec<Op>,
    labels: Vec<usize>,
    pool: Vec<Constant>,
    /// Binder span -> place.
    binders: HashMap<Span, Place>,
    /// Name scopes, innermost last: name -> binder span.
    scopes: Vec<HashMap<String, Span>>,
    /// Overlay word usage: index -> word types seen there.
    overlay: Vec<HashSet<SlotType>>,
    overlay_len: usize,
}

impl<'a, 'p, 'q, 's> FnLower<'a, 'p, 'q, 's> {
    /// Lowers one planned function to a validated ds-mod function record.
    pub(crate) fn lower(
        analyzed: &'a Analyzed,
        res: &'a Resolutions,
        plan: &'p Plan<'q>,
        shapes: &'s mut Shapes<'a>,
        planned: &PlannedFn<'q>,
    ) -> Result<ds_mod::Function, LowerError> {
        let Expr::Lambda { params, body, .. } = &planned.local.init else {
            return Err(LowerError::unsupported(
                planned.local.span,
                "a non-lambda top-level initializer",
                "only const-arrow lambdas lower this round",
            ));
        };
        let Some(fn_ty) = analyzed.checked.def_types.get(&planned.def).copied() else {
            return Err(LowerError::unsupported(
                planned.local.span,
                "a generic or unchecked function",
                "only monomorphic checked functions lower this round",
            ));
        };
        let Some((_, ret)) = fn_sig_of(&analyzed.decls.universe, fn_ty) else {
            return Err(LowerError::unsupported(
                planned.local.span,
                "a non-function binding",
                "top-level functions must be const-arrow lambdas this round",
            ));
        };
        let mut this = Self {
            analyzed,
            res,
            plan,
            shapes,
            name: planned.name.clone(),
            ret_ty: ret,
            params_len: 0,
            slots: Vec::new(),
            code: Vec::new(),
            labels: Vec::new(),
            pool: Vec::new(),
            binders: HashMap::new(),
            scopes: vec![HashMap::new()],
            overlay: Vec::new(),
            overlay_len: 0,
        };
        this.bind_params(params)?;
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
            let words = self.shapes.words(fact.ty)?;
            let base = self.alloc(&words)?;
            let shape = match &*self.shapes.shape(fact.ty)? {
                Shape::Word(slot) => PlaceShape::Word(*slot),
                _ => PlaceShape::Span(fact.ty),
            };
            self.binders.insert(param.name.span, Place { base, shape });
            self.scopes
                .last_mut()
                .expect("the root scope exists")
                .insert(param.name.name.clone(), param.name.span);
            self.params_len += words.len();
        }
        Ok(())
    }

    fn scope_push(&mut self) {
        self.scopes.push(HashMap::new());
    }

    fn scope_pop(&mut self) {
        self.scopes.pop();
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
            (Shape::Word(slot), PlaceShape::Word(_)) => Ok(vec![WordAddr {
                name: None,
                slot: place.base,
                slot_ty: *slot,
            }]),
            (Shape::Word(slot), PlaceShape::Span(underlying)) => {
                let atom = self.only_atom(ty, no_span())?;
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
                let _ = self.value(expr)?;
                Ok(())
            },
        }
    }

    /// One `const`/`var` binding statement.
    fn local_stmt(&mut self, local: &LocalDecl) -> Result<TypedPlace, LowerError> {
        if local.mutable {
            return Err(LowerError::unsupported(
                local.span,
                "a `var` binding",
                "mutation arrives with the M4 linearity lowering",
            ));
        }
        let init = self.value(&local.init)?;
        let Pattern::Binding { span, name } = &local.pat else {
            return Err(LowerError::unsupported(
                local.span,
                "a destructuring binding pattern",
                "only plain name bindings lower this round",
            ));
        };
        // The binder aliases the initializer's place: bindings are
        // immutable, so every use is a read of live slots.
        self.binders.insert(*span, init.place.clone());
        self.scope_insert(&name.name, *span);
        Ok(TypedPlace {
            place: init.place,
            ty: self
                .analyzed
                .checked
                .binders
                .get(span)
                .map_or(init.ty, |f| f.ty),
        })
    }

    // ---- the expression walk ---------------------------------------------------

    fn value(&mut self, e: &Expr) -> Result<TypedPlace, LowerError> {
        let val = self.val(e.span())?;
        let place = match e {
            Expr::Literal { lit, .. } => self.literal(lit, e.span())?,
            Expr::Str { parts, .. } => self.string_expr(parts, e.span())?,
            Expr::RawStr { text, .. } => {
                let dst = self.temp_word(SlotType::Ref)?;
                let kidx = self.pool(Constant::Str(text.clone()));
                self.emit(Op::Kstr {
                    dst: Slot::body(dst),
                    kidx,
                });
                Place {
                    base: dst,
                    shape: PlaceShape::Word(SlotType::Ref),
                }
            },
            Expr::Path { path, .. } => self.path_value(path, e.span(), val.ty)?,
            Expr::Paren { inner, .. } => return self.value(inner),
            Expr::Block(block) => return self.block_value(block),
            Expr::If { .. } => self.if_value(e, val.ty)?,
            Expr::Match { .. } => self.match_value(e, val.ty)?,
            Expr::Struct { .. } => self.struct_literal(e, val.ty)?,
            Expr::Call { .. } => self.call_value(e, val.ty)?,
            Expr::Field { .. } => self.field_access(e, val.ty)?,
            Expr::Try { .. } => self.try_value(e, val.ty)?,
            Expr::Or { .. } => self.or_value(e, val.ty)?,
            Expr::Unary { op, expr, span } => self.unary(*op, expr, *span, val.ty)?,
            Expr::Binary { .. } => self.binary_value(e, val.ty)?,
            Expr::Is { expr, ty, span } => self.is_value(expr, ty, *span)?,
            Expr::Return { .. }
            | Expr::Assign { .. }
            | Expr::While { .. }
            | Expr::For { .. }
            | Expr::Break(_)
            | Expr::Continue(_)
            | Expr::Nursery { .. }
            | Expr::Spawn { .. }
            | Expr::Defer { .. } => {
                return Err(LowerError::unsupported(
                    e.span(),
                    "a control or assignment statement",
                    "bare return/assign/loop statements have no lowering; write tail expressions",
                ))
            },
            Expr::Lambda { .. } => {
                return Err(LowerError::unsupported(
                    e.span(),
                    "a lambda value",
                    "closures are not in the M3 contract; handlers inline at their call site",
                ))
            },
            Expr::Tuple { .. } | Expr::Index { .. } | Expr::Range { .. } => {
                return Err(LowerError::unsupported(
                    e.span(),
                    "a tuple, index, or range expression",
                    "tuples and collections arrive with the M3 stdlib surface",
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
            Literal::Char(_) | Literal::Byte(_) => Err(LowerError::unsupported(
                span,
                "a char or byte literal",
                "no char slot is in the M3 uop contract",
            )),
        }
    }

    /// One interpolating string: a STRCAT chain over its parts [F6].
    fn string_expr(&mut self, parts: &[StrPart], span: Span) -> Result<Place, LowerError> {
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
                    dst
                },
                StrPart::Expr { expr, .. } => {
                    let part = self.value(expr)?;
                    let addrs = self.word_addrs(&part.place, part.ty)?;
                    match addrs.as_slice() {
                        [only] if only.slot_ty == SlotType::Ref => only.slot,
                        _ => {
                            return Err(LowerError::unsupported(
                                expr.span(),
                                "an interpolation hole",
                                "holes must be string-typed; wrap non-strings in \
                                 str_int/str_float/str_bool",
                            ))
                        },
                    }
                },
                StrPart::Ident { span, .. } => {
                    return Err(LowerError::unsupported(
                        *span,
                        "a bare `$name` interpolation hole",
                        "the bare `$name` spelling is not in the M3 contract",
                    ))
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
                    dst
                },
            });
        }
        let base = acc.ok_or_else(|| {
            LowerError::unsupported(
                span,
                "an empty interpolation",
                "write an empty string literal instead",
            )
        })?;
        Ok(Place {
            base,
            shape: PlaceShape::Word(SlotType::Ref),
        })
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
                self.binders.get(bind).cloned().ok_or_else(|| {
                    LowerError::unsupported(span, "a local", "it has no lowered place")
                })
            },
            Some(Res::Def(def)) => {
                let kind = self.analyzed.package.def(*def).kind;
                match kind {
                    ds_check::DefKind::Tag => {
                        let atom = self
                            .analyzed
                            .decls
                            .atoms
                            .get(def)
                            .copied()
                            .ok_or_else(|| mismatch("a tag construction"))?;
                        let dst = self.temp_word(SlotType::Tag)?;
                        let tag = self.shapes.tag_id(atom);
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
                        "a top-level value",
                        "only functions and unit tags are lowerable at top level",
                    )),
                }
            },
            None if only.name == "none" => {
                // the predeclared unit atom: no package DefId exists
                let dst = self.temp_word(SlotType::Tag)?;
                let tag = self.shapes.tag_id(TypeId::NONE);
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
            let tag = self.shapes.tag_id(tested);
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
            Shape::Word(_) => return Err(mismatch("a field access")),
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
            self.emit(Op::Mov {
                dst: Slot::body(target.base + field.word),
                src: Slot::body(addr.slot),
            });
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
        let lhs = self.value(left)?;
        let rhs = self.value(right)?;
        let place = match op {
            BinaryOp::Eq | BinaryOp::Ne => self.eq_chain(&lhs, &rhs, e.span())?,
            BinaryOp::Add => self.arith(DataOp::AddI64, DataOp::AddF64, &lhs, &rhs, *span)?,
            BinaryOp::Sub => self.arith(DataOp::SubI64, DataOp::SubF64, &lhs, &rhs, *span)?,
            BinaryOp::Mul => self.arith(DataOp::MulI64, DataOp::MulF64, &lhs, &rhs, *span)?,
            BinaryOp::Div => self.arith(DataOp::DivI64, DataOp::DivF64, &lhs, &rhs, *span)?,
            BinaryOp::Rem => self.arith(DataOp::ModI64, DataOp::ModI64, &lhs, &rhs, *span)?,
            BinaryOp::Lt => self.compare(DataOp::LtI64, DataOp::LtF64, &lhs, &rhs, *span)?,
            BinaryOp::Le => self.compare(DataOp::LeI64, DataOp::LeF64, &lhs, &rhs, *span)?,
            BinaryOp::Gt => self.compare(DataOp::GtI64, DataOp::GtF64, &lhs, &rhs, *span)?,
            BinaryOp::Ge => self.compare(DataOp::GeI64, DataOp::GeF64, &lhs, &rhs, *span)?,
            BinaryOp::In
            | BinaryOp::WrapAdd
            | BinaryOp::WrapSub
            | BinaryOp::WrapMul
            | BinaryOp::Shl
            | BinaryOp::Shr
            | BinaryOp::BitAnd
            | BinaryOp::BitOr
            | BinaryOp::BitXor => {
                return Err(LowerError::unsupported(
                    *span,
                    "a bitwise, shift, wrapping, or containment operator",
                    "outside the M3 uop contract",
                ))
            },
            BinaryOp::And | BinaryOp::Or => unreachable!("handled above"),
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
        self.emit(Op::Jmp { target: join });
        self.bind_label(short);
        let lie = self.pool(Constant::Bool(!is_and));
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
                    self.emit(Op::Data {
                        op: eq_op(field.slot)?,
                        dst: Slot::body(cond),
                        lhs: Slot::body(lf.slot),
                        rhs: Slot::body(rf.slot),
                    });
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
                self.emit(Op::Data {
                    op: eq_op(field.slot)?,
                    dst: Slot::body(dst),
                    lhs: Slot::body(lhs.place.base + field.word),
                    rhs: Slot::body(rhs.place.base + field.word),
                });
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
        let then_tp = self.block_value(then)?;
        self.widen_into(&result, ty, &then_tp)?;
        self.emit(Op::Jmp { target: join });
        self.bind_label(else_lbl);
        let Some(else_expr) = else_ else {
            return Err(LowerError::unsupported(
                e.span(),
                "an `if` without `else`",
                "value-position ifs need both arms",
            ));
        };
        let else_tp = self.value(else_expr)?;
        self.widen_into(&result, ty, &else_tp)?;
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
    /// arms TAG_TEST and jump in; the catchall receives the fall-through.
    fn match_arms(
        &mut self,
        scrut: &TypedPlace,
        arms: &[MatchArm],
        value: Option<(&Place, Ty, usize)>,
    ) -> Result<(), LowerError> {
        if !self.is_span_ty(scrut.ty) {
            return Err(LowerError::unsupported(
                no_span(),
                "a match on a non-union value",
                "the M3 dispatch is tag-based; scalar matches need their own seam",
            ));
        }
        let labels: Vec<usize> = arms.iter().map(|_| self.label()).collect();
        let mut catchall = None;
        for (index, arm) in arms.iter().enumerate() {
            let Some(atom) = self.pattern_atom(&arm.pat)? else {
                catchall = Some(index);
                continue;
            };
            let cond = self.temp_word(SlotType::Bool)?;
            let src = self.tag_addr(&scrut.place)?;
            let tag = self.shapes.tag_id(atom);
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
        let Some(catchall) = catchall else {
            return Err(LowerError::unsupported(
                no_span(),
                "a match without a catchall arm",
                "the M3 dispatch needs a final `_` or binding arm",
            ));
        };
        self.emit(Op::Jmp {
            target: labels[catchall],
        });
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
            self.bind_arm_pattern(&arm.pat, scrut)?;
            match value {
                Some((result, ty, join)) => {
                    let body_tp = self.value(&arm.body)?;
                    self.widen_into(result, ty, &body_tp)?;
                    self.emit(Op::Jmp { target: join });
                },
                None => self.tail(&arm.body)?,
            }
        }
        Ok(())
    }

    /// The member atom a pattern names; `None` for a catchall arm.
    fn pattern_atom(&mut self, pat: &Pattern) -> Result<Option<TypeId>, LowerError> {
        match pat {
            Pattern::Wildcard(_) | Pattern::Binding { .. } => Ok(None),
            Pattern::Path { path, .. } => {
                let [only] = path.segments.as_slice() else {
                    return Err(LowerError::unsupported(
                        pattern_span(pat),
                        "a multi-segment pattern path",
                        "qualified patterns are not in the M3 contract",
                    ));
                };
                self.path_atom(&only.name)
                    .map(Some)
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
                    .map(Some)
                    .ok_or_else(|| mismatch("a pattern's member atom"))
            },
            Pattern::Literal { .. }
            | Pattern::Str { .. }
            | Pattern::RawStr { .. }
            | Pattern::Call { .. }
            | Pattern::Tuple { .. }
            | Pattern::Or { .. } => Err(LowerError::unsupported(
                pattern_span(pat),
                "a literal, tuple, constructor, or or-pattern",
                "member and catchall patterns cover the M3 surface",
            )),
        }
    }

    /// Binds one arm's pattern binders to scrutinee words.
    fn bind_arm_pattern(&mut self, pat: &Pattern, scrut: &TypedPlace) -> Result<(), LowerError> {
        match pat {
            Pattern::Binding { span, name } => {
                self.binders.insert(*span, scrut.place.clone());
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
                    self.scope_insert(&bind.1, bind.0);
                }
                Ok(())
            },
            _ => Ok(()),
        }
    }

    // ---- calls ------------------------------------------------------------------

    fn call_value(&mut self, e: &Expr, ty: Ty) -> Result<Place, LowerError> {
        self.call_impl(e, ty, false)
    }

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
        if trailing.is_some() {
            return Err(LowerError::unsupported(
                *span,
                "a trailing block argument",
                "block-argument sugar is not in the M3 contract",
            ));
        }
        for arg in args {
            if arg.name.is_some() {
                return Err(LowerError::unsupported(
                    arg.span,
                    "a named argument",
                    "named arguments are not in the M3 contract",
                ));
            }
        }
        // the `.err` handler: dispatch plus an inlined handler body
        if let Expr::Field { recv, name, .. } = &**callee {
            if matches!(self.res.get(name.span), Some(Res::Handler)) {
                if name.name != "err" {
                    return Err(LowerError::unsupported(
                        name.span,
                        "an `.ok` handler",
                        "only `.err` is lowerable this round",
                    ));
                }
                let [handler] = args.as_slice() else {
                    return Err(mismatch("a handler argument"));
                };
                return self.err_value(recv, &handler.value, ty);
            }
            return Err(LowerError::unsupported(
                name.span,
                "a method call",
                "UFCS dot-calls arrive with the M3 stdlib surface",
            ));
        }
        let Expr::Path {
            span: callee_span, ..
        } = &**callee
        else {
            return Err(LowerError::unsupported(
                callee.span(),
                "a computed callee",
                "only name callees are lowerable this round",
            ));
        };
        let Some(Res::Def(def)) = self.res.get(*callee_span) else {
            return Err(LowerError::unsupported(
                *callee_span,
                "a callee name",
                "it does not resolve to a package function",
            ));
        };
        let target = self.resolve_callee(*def, *callee_span)?;
        // stage: evaluate every argument to a place, then move its words
        // into the overlay area (argument evaluation may itself call)
        let mut staged: Vec<SlotType> = Vec::new();
        let mut moves: Vec<(usize, u8)> = Vec::new();
        for arg in args {
            let arg_val = self.value(&arg.value)?;
            for addr in self.word_addrs(&arg_val.place, arg_val.ty)? {
                moves.push((staged.len(), addr.slot));
                staged.push(addr.slot_ty);
            }
        }
        for (index, slot) in moves {
            self.emit(Op::Mov {
                dst: Slot::overlay(u8::try_from(index).expect("overlay word fits u8")),
                src: Slot::body(slot),
            });
        }
        if let Callee::Builtin(native) = target {
            if let NativeShape::Fallible {
                payload,
                err_prefix,
            } = native.shape
            {
                if tail {
                    return Err(LowerError::unsupported(
                        *callee_span,
                        "a tail call to a fallible native",
                        "the platform failure span has no source-level return type; keep the call \
                         out of tail position",
                    ));
                }
                return self.fallible_native_result(native, ty, payload, err_prefix, &staged);
            }
        }
        let result_words = self.callee_result_words(target)?;
        self.overlay_site(&staged, &result_words);
        let kidx = self.pool(funcref_const(target));
        if tail {
            // control transfers to the callee; the stack never grows [H2]
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
        self.emit(Op::Call {
            callee: kidx,
            nargs: u8::try_from(staged.len()).expect("argument count fits u8"),
        });
        // copy the result span out of the overlay into a temp
        let base = self.alloc(&result_words)?;
        for index in 0..result_words.len() {
            self.emit(Op::Mov {
                dst: Slot::body(base + u8::try_from(index).expect("result width fits u8")),
                src: Slot::overlay(u8::try_from(index).expect("result width fits u8")),
            });
        }
        let shape = match &*self.shapes.shape(ty)? {
            Shape::Word(slot) => PlaceShape::Word(*slot),
            _ => PlaceShape::Span(ty),
        };
        Ok(Place { base, shape })
    }

    fn resolve_callee(&self, def: DefId, span: Span) -> Result<Callee, LowerError> {
        if let Some(index) = self.plan.index_of(def) {
            return Ok(Callee::Local(index));
        }
        let name = self.analyzed.package.def(def).name.clone();
        if let Some(native) = crate::native::lookup(&name) {
            return Ok(Callee::Builtin(native));
        }
        if name == "fs_list" {
            return Err(LowerError::unsupported(
                span,
                "a call to `fs_list`",
                "list values have no runtime shape yet; the list contract owns this",
            ));
        }
        if self
            .analyzed
            .decls
            .fns
            .get(&def)
            .is_some_and(|sig| !sig.generics.is_empty())
        {
            return Err(LowerError::unsupported(
                span,
                "a generic callee",
                "generic dispatch is the M6 CALL_GENERIC seam",
            ));
        }
        Err(LowerError::unsupported(
            span,
            "a callee",
            "extern non-builtin and method callees are outside the contract",
        ))
    }

    /// The result word types a callee's return lands on the overlay as.
    fn callee_result_words(&mut self, target: Callee) -> Result<Vec<SlotType>, LowerError> {
        match target {
            Callee::Builtin(native) => match native.shape {
                NativeShape::Total(slot) => Ok(vec![slot]),
                // call_impl routes fallible natives through their own
                // tag-test path; this generic copy never sees them
                NativeShape::Fallible { .. } => Err(mismatch("a fallible native result")),
            },
            Callee::Local(index) => {
                let planned = &self.plan.functions()[index as usize];
                let fn_ty = self
                    .analyzed
                    .checked
                    .def_types
                    .get(&planned.def)
                    .copied()
                    .ok_or_else(|| mismatch("a callee signature"))?;
                let Some((_, ret)) = fn_sig_of(&self.analyzed.decls.universe, fn_ty) else {
                    return Err(mismatch("a callee signature"));
                };
                self.shapes.words(ret)
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
        let message = self.temp_word(SlotType::Ref)?;
        self.emit(Op::Mov {
            dst: Slot::body(message),
            src: Slot::overlay(1),
        });
        let text = self.temp_word(SlotType::Ref)?;
        self.emit(Op::Data {
            op: DataOp::StrCat,
            dst: Slot::body(text),
            lhs: Slot::body(prefix),
            rhs: Slot::body(message),
        });
        self.emit(Op::Mov {
            dst: Slot::overlay(0),
            src: Slot::body(text),
        });
        let println = crate::native::lookup("println").expect("the console native is covered");
        let kidx = self.pool(funcref_const(Callee::Builtin(println)));
        self.emit(Op::Call {
            callee: kidx,
            nargs: 1,
        });
        self.emit(Op::Halt {
            slot: Slot::body(discriminant),
        });
        self.bind_label(ok_lbl);
        let base = self.alloc(&[payload])?;
        self.emit(Op::Mov {
            dst: Slot::body(base),
            src: Slot::overlay(1),
        });
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
            let tag = self.shapes.tag_id(*member);
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
        // success: the kept set flows straight into the result span
        self.widen_into(&result, ty, &TypedPlace {
            place: src.place.clone(),
            ty,
        })?;
        self.emit(Op::Jmp { target: join });
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
            self.scope_insert(&param.name.name, param.name.span);
            let body_tp = self.value(body)?;
            self.widen_into(&result, ty, &body_tp)?;
            self.scope_pop();
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
            let tag = self.shapes.tag_id(*member);
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

    /// `a or return b`: the unwanted members exit through the function
    /// return; the wanted set extracts in place [E4].
    fn or_value(&mut self, e: &Expr, ty: Ty) -> Result<Place, LowerError> {
        let Expr::Or { left, right, span } = e else {
            return Err(mismatch("an `or` extraction"));
        };
        let OrTail::Return { expr: ret_expr, .. } = right.as_ref() else {
            return Err(LowerError::unsupported(
                *span,
                "an `or` with a plain fallback or loop exit",
                "only `or return` is lowerable this round",
            ));
        };
        let src = self.value(left)?;
        let src_ty = self.val(left.span())?.ty;
        let unwanted = set_diff(&self.members(src_ty), &self.members(ty));
        let exit_lbl = self.label();
        let continue_lbl = self.label();
        for member in &unwanted {
            let cond = self.temp_word(SlotType::Bool)?;
            let src_word = self.tag_addr(&src.place)?;
            let tag = self.shapes.tag_id(*member);
            self.emit(Op::TagTest {
                dst: Slot::body(cond),
                src: Slot::body(src_word),
                tag,
            });
            self.emit(Op::Jmpif {
                cond: Slot::body(cond),
                target: exit_lbl,
            });
        }
        self.emit(Op::Jmp {
            target: continue_lbl,
        });
        self.bind_label(exit_lbl);
        match ret_expr {
            Some(expr) => {
                let value = self.value(expr)?;
                self.emit_return(&value.place, value.ty)?;
            },
            None => {
                return Err(LowerError::unsupported(
                    *span,
                    "a bare `or return`",
                    "the M3 surface always names the returned value",
                ))
            },
        }
        self.bind_label(continue_lbl);
        self.extract(src.place, ty)
    }

    /// The kept set of a `?`/`or` extraction: one member addresses its
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

    /// Lowers a tail-position expression: calls become CALLT, everything
    /// else returns directly [H2].
    fn tail(&mut self, e: &Expr) -> Result<(), LowerError> {
        match e {
            Expr::Paren { inner, .. } => self.tail(inner),
            Expr::Block(block) => self.block_tail(block),
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
                self.block_tail(then)?;
                self.bind_label(else_lbl);
                match else_ {
                    Some(else_expr) => self.tail(else_expr),
                    None => Err(LowerError::unsupported(
                        e.span(),
                        "an `if` without `else`",
                        "tail-position ifs need both arms",
                    )),
                }
            },
            Expr::Match { .. } => self.match_tail(e),
            Expr::Call { callee, .. } if matches!(&**callee, Expr::Path { .. }) => {
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
                let tp = self.local_stmt(local)?;
                self.emit_return(&tp.place, tp.ty)
            },
            None => {
                let unit = self.unit_value(block.span)?;
                self.emit_return(&unit.place, unit.ty)
            },
        }
    }

    /// Returns one value in the enclosing function's return shape.
    fn emit_return(&mut self, place: &Place, src_ty: Ty) -> Result<(), LowerError> {
        if src_ty == self.ret_ty {
            let addrs = self.word_addrs(place, src_ty)?;
            if addrs.len() == 1 {
                self.emit(Op::Ret {
                    src: Slot::body(addrs[0].slot),
                });
                return Ok(());
            }
            let contiguous = addrs.iter().enumerate().all(|(index, addr)| {
                addr.slot == place.base + u8::try_from(index).expect("span width fits u8")
            });
            if contiguous {
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
                self.emit(Op::Ret {
                    src: Slot::body(only.slot),
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
                }
                self.emit(Op::RetSpan {
                    src: Slot::body(target.base),
                    n: u8::try_from(s.fields.len()).expect("span width fits u8"),
                });
                Ok(())
            },
            Shape::Union(u) => {
                let atom = self.only_atom(src_ty, no_span())?;
                let member = u
                    .member(atom)
                    .ok_or_else(|| mismatch("a union member return"))?
                    .clone();
                let target = self.temp_span(self.ret_ty)?;
                let tag = self.shapes.tag_id(atom);
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
                }
                self.emit(Op::RetSpan {
                    src: Slot::body(target.base),
                    n: u8::try_from(u.words.len()).expect("span width fits u8"),
                });
                Ok(())
            },
        }
    }

    /// One propagated member's early exit: re-span it into the enclosing
    /// return union — the tag word moves, the payload words follow.
    fn respan_exit(&mut self, src_place: &Place, member: TypeId) -> Result<(), LowerError> {
        let ret_shape = self.shapes.shape(self.ret_ty)?;
        let Shape::Union(u) = &*ret_shape else {
            return Err(mismatch("a `?` exit in a non-union return"));
        };
        let target = self.temp_span(self.ret_ty)?;
        let src_tag = self.tag_addr(src_place)?;
        self.emit(Op::Mov {
            dst: Slot::body(target.base),
            src: Slot::body(src_tag),
        });
        let member = u
            .member(member)
            .ok_or_else(|| mismatch("a propagated member"))?
            .clone();
        self.zero_init_member(target.base, &u.words, &member.fields)?;
        let PlaceShape::Span(underlying) = src_place.shape else {
            return Err(mismatch("a propagated member's source"));
        };
        let fields = self.member_fields(underlying, member.atom)?;
        for field in &member.fields {
            let src_field = fields
                .iter()
                .find(|f| f.name == field.name)
                .ok_or_else(|| mismatch("a propagated member field"))?;
            self.emit(Op::Mov {
                dst: Slot::body(target.base + field.word),
                src: Slot::body(src_place.base + src_field.word),
            });
        }
        self.emit(Op::RetSpan {
            src: Slot::body(target.base),
            n: u8::try_from(u.words.len()).expect("span width fits u8"),
        });
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
            if addrs.iter().enumerate().all(|(index, addr)| {
                addr.slot == dst.base + u8::try_from(index).expect("span width fits u8")
            }) {
                return Ok(()); // already in place
            }
            for (index, addr) in addrs.iter().enumerate() {
                self.emit(Op::Mov {
                    dst: Slot::body(dst.base + u8::try_from(index).expect("span width fits u8")),
                    src: Slot::body(addr.slot),
                });
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
                }
                Ok(())
            },
            Shape::Union(u) => {
                let atom = self.only_atom(src.ty, no_span())?;
                let member = u
                    .member(atom)
                    .ok_or_else(|| mismatch("a union member write"))?
                    .clone();
                let tag = self.shapes.tag_id(atom);
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
                }
                Ok(())
            },
        }
    }

    // ---- finalization ---------------------------------------------------------

    fn finalize(self) -> Result<ds_mod::Function, LowerError> {
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
        // resolve ops to uops: label positions are op indices, shifted one
        // past the FUNCF entry word
        let mut code = Vec::with_capacity(self.code.len() + 1);
        code.push(Uop::Funcf {
            frame_slots: u16::try_from(total).expect("frame slots fit u16"),
        });
        for op in &self.code {
            let body = |slot: Slot| resolve(slot, body_len);
            code.push(match *op {
                Op::Kimm { dst, kidx } => Uop::Kimm {
                    dst: body(dst),
                    kidx: ConstIdx(kidx),
                },
                Op::Kconst { dst, kidx } => Uop::Kconst {
                    dst: body(dst),
                    kidx: ConstIdx(kidx),
                },
                Op::Kstr { dst, kidx } => Uop::Kstr {
                    dst: body(dst),
                    kidx: ConstIdx(kidx),
                },
                Op::Data { op, dst, lhs, rhs } => op.uop(body(dst), body(lhs), body(rhs)),
                Op::TagTest { dst, src, tag } => Uop::TagTest {
                    dst: body(dst),
                    src: body(src),
                    tag,
                },
                Op::TagNew { dst, tag } => Uop::TagNew {
                    dst: body(dst),
                    tag,
                },
                Op::Mov { dst, src } => Uop::Mov {
                    dst: body(dst),
                    src: body(src),
                },
                Op::Jmp { target } => Uop::Jmp {
                    target: self.target(target),
                },
                Op::Jmpif { cond, target } => Uop::Jmpif {
                    cond: body(cond),
                    target: self.target(target),
                },
                Op::JmpifNot { cond, target } => Uop::JmpifNot {
                    cond: body(cond),
                    target: self.target(target),
                },
                Op::Call { callee, nargs } => Uop::Call {
                    callee: ConstIdx(callee),
                    nargs,
                },
                Op::Callt { callee, nargs } => Uop::Callt {
                    callee: ConstIdx(callee),
                    nargs,
                },
                Op::Ret { src } => Uop::Ret { src: body(src) },
                Op::RetSpan { src, n } => Uop::RetSpan { src: body(src), n },
                Op::Halt { slot } => Uop::Halt { slot: body(slot) },
            });
        }
        ds_mod::Function::new(self.name.clone(), layout, self.pool, code)
            .map_err(|error| LowerError::Build(error.to_string()))
    }

    fn target(&self, label: usize) -> u32 {
        let position = self.labels[label];
        debug_assert_ne!(position, usize::MAX, "a jump references an unbound label");
        u32::try_from(position + 1).expect("label count fits u32")
    }
}

// zero loads for the dead payload words of a constructed member: a
// union span's every word carries a value of its declared kind, so
// whole-span moves and typed callee parameters never see a foreign kind
// (the live member's words are the only ones ever read [BL-2])
impl FnLower<'_, '_, '_, '_> {
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
                return;
            },
            SlotType::Tag | SlotType::I32 | SlotType::Raw => return,
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
            module: BUILTIN_MODULE.to_owned(),
            func: native.index,
        },
        Callee::Local(index) => Constant::FuncRef {
            module: String::new(), // the enclosing module
            func: index,
        },
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
