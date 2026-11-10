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

use ds_abi::{ConstIdx, RcOp, SlotType, Uop};
use ds_check::{Analyzed, DefId, DotRes, Res, Resolutions, Ty, TypeId, Val};
use ds_diag::Span;
use ds_mod::Constant;
use ds_parse::{
    BinaryOp, BlockExpr, CallArg, EnumPayload, Expr, Ident, Literal, LocalDecl, MatchArm, Param,
    Path, Pattern, Stmt, StmtKind, StrPart, Type, UnaryOp,
};

use crate::{
    PlacedOp,
    error::LowerError,
    native::{Native, NativeShape, PLATFORM_IO_OK},
    place::Placement,
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
    /// (C5) and pools the block for reuse inside `ds_rt::Heap`.
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
    /// A builtin native in one of the two reserved native modules (the
    /// `builtin` core or the `std` platform module — the native's own
    /// routing says which).
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
    /// The Perceus dup/drop placement over this function's walk (C4).
    rc: Placement,
}

impl<'a, 'p, 'q, 's> FnLower<'a, 'p, 'q, 's> {
    /// Lowers one planned function to a validated ds-mod function record
    /// plus its placed (RC-bearing) op sequence.
    pub(crate) fn lower(
        analyzed: &'a Analyzed,
        res: &'a Resolutions,
        plan: &'p Plan<'q>,
        shapes: &'s mut Shapes<'a>,
        lin: &ds_check::Linearity,
        planned: &PlannedFn<'q>,
    ) -> Result<(ds_mod::Function, Vec<PlacedOp>), LowerError> {
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
            rc: Placement::from_linearity(lin),
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
            let words = self.shapes.words(fact.ty)?;
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
            (Shape::Word(slot), PlaceShape::Word(_)) => Ok(vec![WordAddr {
                name: None,
                slot: place.base,
                slot_ty: *slot,
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
                let words = self.ref_words(tp.place.base, tp.ty)?;
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
            self.widen_into(&target, declared, &init)?;
            self.place_of_ty(target.base, declared)?
        } else {
            init.place.clone()
        };
        match pat {
            Pattern::Binding { span, name } => {
                self.binders.insert(*span, place.clone());
                self.rc_bind(*span, place.base, declared)?;
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
            _ => {
                return Err(LowerError::unsupported(
                    local.span,
                    "a destructuring binding pattern",
                    "plain names and tuple patterns lower this round",
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
            .ok_or_else(|| mismatch("an assignment place"))?;
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
            Expr::Lambda { .. } => {
                return Err(LowerError::unsupported(
                    e.span(),
                    "a lambda value",
                    "closures are not in the M3 contract; handlers inline at their call site",
                ))
            },
            Expr::Tuple { items, span, .. } => {
                if items.is_empty() {
                    self.unit_value(*span)?.place
                } else {
                    self.tuple_literal(items, val.ty, *span)?
                }
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
                self.binders.get(bind).cloned().ok_or_else(|| {
                    LowerError::unsupported(span, "a local", "it has no lowered place")
                })
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
        self.emit(Op::Jmp { target: join });
        let arm_full = self.rc.take();
        self.rc.restore(base);
        self.bind_label(short);
        let lie = self.pool(Constant::Bool(!is_and));
        self.emit(Op::Kconst {
            dst: Slot::body(dst),
            kidx: lie,
        });
        let arm_short = self.rc.take();
        self.rc.join(&[arm_full, arm_short]);
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
        self.emit(Op::Jmp { target: join });
        let arm_then = self.rc.take();
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
        self.rc.join(&[arm_then, arm_else]);
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
            match value {
                Some((result, ty, join)) => {
                    let body_tp = self.value(&arm.body)?;
                    self.widen_into(result, ty, &body_tp)?;
                    self.emit(Op::Jmp { target: join });
                },
                None => self.tail(&arm.body)?,
            }
            states.push(self.rc.take());
        }
        self.rc.join(&states);
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
            if !matches!(self.shapes.words(item_val.ty)?.as_slice(), [SlotType::I64
                | SlotType::F64
                | SlotType::Bool
                | SlotType::Tag
                | SlotType::Ref])
            {
                return Err(LowerError::unsupported(
                    item.span(),
                    "a list literal element",
                    "list elements lower as one machine word; wider elements arrive with reboxing",
                ));
            }
            let push = crate::native::lookup("list_push")
                .expect("the list surface is covered by the native table");
            let mut staged = vec![SlotType::Ref];
            let mut moves = vec![(0usize, list)];
            for addr in self.word_addrs(&item_val.place, item_val.ty)? {
                moves.push((staged.len(), addr.slot));
                staged.push(addr.slot_ty);
            }
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
        let target = self.resolve_callee(def, callee_span)?;
        // stage: evaluate every argument to a place, then move its words
        // into the overlay area (argument evaluation may itself call)
        let mut staged: Vec<SlotType> = Vec::new();
        let mut moves: Vec<(usize, u8)> = Vec::new();
        for arg in &arg_exprs {
            let arg_val = self.value(arg)?;
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
            // a staged Ref: a local callee's borrow parameter owns the
            // handed word (forward on the source's death, dup otherwise);
            // a native is a transient borrow window that never dups
            if staged[index] == SlotType::Ref {
                match target {
                    Callee::Builtin(_) => self.rc.staged_native(slot, tail),
                    Callee::Local(_) => self.rc.staged_local(&mut self.code, slot),
                }
            }
        }
        if let Callee::Builtin(native) = target {
            if let NativeShape::Fallible {
                payload,
                err_prefix,
            } = native.shape
            {
                if tail {
                    return Err(LowerError::unsupported(
                        callee_span,
                        "a tail call to a fallible native",
                        "the platform failure span has no source-level return type; keep the call \
                         out of tail position",
                    ));
                }
                return self.fallible_native_result(native, ty, payload, err_prefix, &staged);
            }
            if let NativeShape::Checked = native.shape {
                if tail {
                    return Err(LowerError::unsupported(
                        callee_span,
                        "a tail call to a checked-read native",
                        "the ok/none span has no source-level return type; keep the call out of \
                         tail position",
                    ));
                }
                return self.checked_native_result(native, ty, &staged);
            }
        }
        let result_words = self.callee_result_words(target)?;
        self.overlay_site(&staged, &result_words);
        let kidx = self.pool(funcref_const(target));
        if tail {
            // control transfers to the callee; the stack never grows [H2].
            // The frame ends here: native borrow drops flush and every
            // still-owned reference drops before the transfer.
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
        self.emit(Op::Call {
            callee: kidx,
            nargs: u8::try_from(staged.len()).expect("argument count fits u8"),
        });
        self.rc.native_done(&mut self.code);
        // copy the result span out of the overlay into a temp: the callee
        // transferred its result references out with the return
        let base = self.alloc(&result_words)?;
        for (index, slot) in result_words.iter().enumerate() {
            let word = base + u8::try_from(index).expect("result width fits u8");
            self.emit(Op::Mov {
                dst: Slot::body(word),
                src: Slot::overlay(u8::try_from(index).expect("result width fits u8")),
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

    fn resolve_callee(&self, def: DefId, span: Span) -> Result<Callee, LowerError> {
        if let Some(index) = self.plan.index_of(def) {
            return Ok(Callee::Local(index));
        }
        let name = self.analyzed.package.def(def).name.clone();
        if let Some(native) = crate::native::lookup(&name) {
            return Ok(Callee::Builtin(native));
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
                // call_impl routes fallible and checked natives through
                // their own tag-test paths; this generic copy never sees
                // them
                NativeShape::Fallible { .. } => Err(mismatch("a fallible native result")),
                NativeShape::Checked => Err(mismatch("a checked-read native result")),
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

    /// One checked-read native call (`list_get`): the two-word ok/none
    /// span lands like a fallible native's, but neither side is a
    /// failure — it re-spans into the call's own `t | none` union: ok
    /// writes the element member with the payload word, none the
    /// predeclared unit member, so `?`, `.ok`, and match see an ordinary
    /// optional value. The element must be one single-word atom; the
    /// rejected index the none side carries has no member field to
    /// surface in, so it drops.
    fn checked_native_result(
        &mut self,
        native: &'static Native,
        ty: Ty,
        staged: &[SlotType],
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
        let kidx = self.pool(funcref_const(Callee::Builtin(native)));
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
        // path-fork: none and ok each write the span with their own value
        let base = self.rc.save();
        let tag = self.shapes.tag_id(TypeId::NONE);
        self.emit(Op::TagNew {
            dst: Slot::body(result.base),
            tag,
        });
        self.zero_init_member(result.base, &u.words, &none_member.fields)?;
        self.emit(Op::Jmp { target: join });
        let arm_none = self.rc.take();
        self.rc.restore(base);
        self.bind_label(ok_lbl);
        let tag = self.shapes.tag_id(*elem);
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
        self.rc.join(&[arm_none, arm_ok]);
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
        let base = self.rc.save();
        self.widen_into(&result, ty, &TypedPlace {
            place: src.place.clone(),
            ty,
        })?;
        self.emit(Op::Jmp { target: join });
        let arm_ok = self.rc.take();
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
        self.rc.join(&[arm_ok, arm_err]);
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
        // the difference side flows into the result; the fall-through and
        // the handler arm fork the ownership map
        let base = self.rc.save();
        let remainder = set_diff(&self.members(src_ty), &handled);
        if remainder.is_empty() {
            // every member jumps to the handler; the fallthrough is dead
            self.emit(Op::Jmp { target: join });
        } else if self.members(src_ty).len() == 1 {
            // a single-member operand: the value itself flows, no tags
            self.widen_into(&result, ty, &TypedPlace {
                place: src.place.clone(),
                ty: src_ty,
            })?;
            self.emit(Op::Jmp { target: join });
        } else {
            let labels: Vec<usize> = remainder.iter().map(|_| self.label()).collect();
            for (member, label) in remainder.iter().zip(&labels) {
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
                    target: *label,
                });
            }
            // dead: the tests cover every member of the operand's type
            self.emit(Op::Jmp { target: join });
            for (member, label) in remainder.iter().zip(labels) {
                self.bind_label(label);
                self.respan_into(&result, ty, &src.place, *member)?;
                self.emit(Op::Jmp { target: join });
            }
        }
        self.bind_label(handler_lbl);
        let arm_flow = self.rc.take();
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
        self.rc.join(&[arm_flow, arm_ok]);
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
            // plain value [B3].
            Expr::Path { span, .. } => !matches!(
                self.res.get(*span),
                Some(Res::Def(def))
                    if self
                        .analyzed
                        .decls
                        .cases
                        .get(def)
                        .is_some_and(|c| !matches!(c.payload, EnumPayload::None))
            ),
            Expr::Field { name, .. } => {
                matches!(self.res.get(name.span), Some(Res::Dot(DotRes::Free { .. })))
            },
            _ => false,
        }
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
                self.rc.join(&[arm_then, arm_else]);
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
                // the dead payload word owns its interned empty-string
                // reference like any production
                self.rc.produced(dst, None);
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
            module: native.module.name().to_owned(),
            func: native.index,
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
