//! Value shapes: the slot-level representation of solved types [BL-2].
//!
//! Every value occupies a span of frame slots — one word per scalar, a
//! bare field span per struct, and a tag word plus a payload region per
//! union. This module owns the mapping and the module-wide tag-id table.
//!
//! # Union payload strategy (the M3 choice, per the golden fixtures)
//!
//! One payload region per union TYPE: each member's fields take the
//! lowest free region slot whose `SlotType` matches the field's word
//! type, and a slot stays claimable by every other member. Two members
//! share a word when their word types agree (the golden fixture's
//! `bad_port.got` and `config.port` share an I64 word), so the region
//! width is the number of distinct word shapes members need, not the
//! member count. Payloads are one word each in this round; a multi-word
//! field (a nested struct or union payload) is
//! [`LowerError::Unsupported`] — covering it needs the "minority payloads
//! reboxed to Ref" alternative, and a boxing allocator is outside the M3
//! uop set. That alternative stays open for the design record to
//! adjudicate when a payloaded-nesting corpus item lands.
//!
//! # Primitive atoms
//!
//! The corpus declares `int`/`float`/`string`/`bool` as ambient tags
//! (conformance/README.md); the lowerer maps those four names onto the
//! slot types of the same names. `none` is the universe's predeclared
//! unit atom and needs no declaration.

use std::{collections::HashMap, rc::Rc};

use ds_abi::SlotType;
use ds_check::{Analyzed, Ty, TypeExpr, TypeId};

use crate::error::LowerError;

/// Span placeholder for shape-level errors: shapes are built from types,
/// not nodes, so no source span sits behind them.
const NO_SPAN: ds_diag::Span = ds_diag::Span::new(ds_diag::FileId::new(0), 0, 0);

/// The primitive vocabulary: atom name to slot type. These atoms are
/// declared as ambient `tag` names by the corpus and check like any tag.
const PRIMITIVES: &[(&str, SlotType)] = &[
    ("int", SlotType::I64),
    ("float", SlotType::F64),
    ("bool", SlotType::Bool),
    ("string", SlotType::Ref),
];

fn primitive_slot(name: &str) -> Option<SlotType> {
    PRIMITIVES.iter().find(|(n, _)| *n == name).map(|(_, s)| *s)
}

/// One word of a value's span: the field's name (or `None` for a
/// primitive member's own payload word) plus the span offset it lives at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MemberField {
    /// The declared field name; `None` marks a primitive member's payload.
    pub name: Option<String>,
    /// The word type.
    pub slot: SlotType,
    /// Offset from the span base.
    pub word: u8,
}

/// One member of a union span [BL-2].
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MemberShape {
    /// The member's atom.
    pub atom: TypeId,
    /// The module-wide tag id.
    pub tag: u8,
    /// Payload fields, in declaration order.
    pub fields: Vec<MemberField>,
}

/// A bare struct span: fields at consecutive words, no tag.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct StructShape {
    /// Fields, in declaration order.
    pub fields: Vec<MemberField>,
}

/// A tag+payload union span [BL-2].
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct UnionShape {
    /// The span's word types, tag first.
    pub words: Vec<SlotType>,
    /// One entry per member, in member order.
    pub members: Vec<MemberShape>,
}

impl UnionShape {
    /// The member entry for one atom.
    pub(crate) fn member(&self, atom: TypeId) -> Option<&MemberShape> {
        self.members.iter().find(|m| m.atom == atom)
    }
}

/// The representation of one value type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Shape {
    /// One unboxed word (scalars, unit values, bare unit tags).
    Word(SlotType),
    /// A bare struct: fields at consecutive words.
    Struct(StructShape),
    /// A tag+payload union span.
    Union(UnionShape),
}

/// Shapes of the types of one package, plus the module-wide tag ids.
pub(crate) struct Shapes<'a> {
    analyzed: &'a Analyzed,
    cache: HashMap<Ty, Rc<Shape>>,
    tags: HashMap<TypeId, u8>,
    next_tag: u8,
}

impl<'a> Shapes<'a> {
    pub(crate) fn new(analyzed: &'a Analyzed) -> Self {
        Self {
            analyzed,
            cache: HashMap::new(),
            tags: HashMap::new(),
            next_tag: 0,
        }
    }

    /// The module-wide tag id of one atom, assigned in first-use order
    /// while functions lower in declaration order.
    pub(crate) fn tag_id(&mut self, atom: TypeId) -> u8 {
        if let Some(tag) = self.tags.get(&atom) {
            return *tag;
        }
        let tag = self.next_tag;
        self.next_tag += 1;
        self.tags.insert(atom, tag);
        tag
    }

    /// The tag table keyed by atom name, for the run entry path.
    pub(crate) fn tag_names(&self) -> HashMap<String, u8> {
        let mut out = HashMap::new();
        for (atom, tag) in &self.tags {
            if let Ok(decl) = self.analyzed.decls.universe.atom_decl(*atom) {
                out.insert(decl.name.clone(), *tag);
            }
        }
        out
    }

    /// The shape of one solved type, memoized.
    pub(crate) fn shape(&mut self, ty: Ty) -> Result<Rc<Shape>, LowerError> {
        if let Some(shape) = self.cache.get(&ty) {
            return Ok(shape.clone());
        }
        let members = self.analyzed.decls.universe.members(ty).to_vec();
        let shape = match members.as_slice() {
            [] => Shape::Word(SlotType::I64), // the empty type; never flows
            [only] => self.atom_shape(*only)?,
            _ => self.union_shape(&members)?,
        };
        let shape = Rc::new(shape);
        self.cache.insert(ty, shape.clone());
        Ok(shape)
    }

    /// The word types of one solved type's span.
    pub(crate) fn words(&mut self, ty: Ty) -> Result<Vec<SlotType>, LowerError> {
        let shape = self.shape(ty)?;
        Ok(match &*shape {
            Shape::Word(slot) => vec![*slot],
            Shape::Struct(s) => s.fields.iter().map(|f| f.slot).collect(),
            Shape::Union(u) => u.words.clone(),
        })
    }

    /// A union span: the tag word plus the shared payload region, member
    /// fields assigned per the module-doc strategy.
    fn union_shape(&mut self, members: &[TypeId]) -> Result<Shape, LowerError> {
        let mut region: Vec<SlotType> = Vec::new();
        let mut shapes = Vec::with_capacity(members.len());
        for atom in members {
            let tag = self.tag_id(*atom);
            let payload = self.payload_fields(*atom)?;
            let mut claimed = vec![false; region.len()];
            let mut fields = Vec::with_capacity(payload.len());
            for (name, slot) in payload {
                let index = if let Some(i) =
                    (0..region.len()).find(|i| region[*i] == slot && !claimed[*i])
                {
                    i
                } else {
                    region.push(slot);
                    claimed.push(false);
                    region.len() - 1
                };
                claimed[index] = true;
                fields.push(MemberField {
                    name,
                    slot,
                    word: word_of(index + 1)?,
                });
            }
            shapes.push(MemberShape {
                atom: *atom,
                tag,
                fields,
            });
        }
        let mut words = vec![SlotType::Tag];
        words.append(&mut region);
        Ok(Shape::Union(UnionShape {
            words,
            members: shapes,
        }))
    }

    /// A single-atom type: a primitive or unit-tag word, a bare struct
    /// span, or the arity-0 tuple's unit word.
    fn atom_shape(&mut self, atom: TypeId) -> Result<Shape, LowerError> {
        let universe = &self.analyzed.decls.universe;
        let Ok(decl) = universe.atom_decl(atom) else {
            return Err(unsupported("a value", "its atom has no declaration"));
        };
        match &decl.kind {
            ds_check::AtomKind::Unit => Ok(Shape::Word(
                primitive_slot(&decl.name).unwrap_or(SlotType::Tag),
            )),
            ds_check::AtomKind::Struct(_) => {
                let payload = self.payload_fields(atom)?;
                let mut fields = Vec::with_capacity(payload.len());
                for (index, (name, slot)) in payload.into_iter().enumerate() {
                    fields.push(MemberField {
                        name,
                        slot,
                        word: word_of(index)?,
                    });
                }
                Ok(Shape::Struct(StructShape { fields }))
            },
            ds_check::AtomKind::Tuple(elements) if elements.is_empty() => {
                Ok(Shape::Word(SlotType::I64)) // the `()` unit value
            },
            _ => Err(unsupported(
                "an enum, tuple, application, or function value",
                "the M3 runtime contract covers unit, primitive, and struct atoms only",
            )),
        }
    }

    /// One atom's payload: `(name, word type)` pairs. Unit tags carry no
    /// payload; primitives carry their own word; struct members carry
    /// their declared fields. A multi-word field type is unsupported.
    fn payload_fields(&self, atom: TypeId) -> Result<Vec<(Option<String>, SlotType)>, LowerError> {
        let universe = &self.analyzed.decls.universe;
        let Ok(decl) = universe.atom_decl(atom) else {
            return Err(unsupported("a value", "its atom has no declaration"));
        };
        match &decl.kind {
            ds_check::AtomKind::Unit => match primitive_slot(&decl.name) {
                Some(slot) => Ok(vec![(None, slot)]),
                None => Ok(Vec::new()),
            },
            ds_check::AtomKind::Struct(fields) => fields
                .iter()
                .filter(|f| f.name.is_some())
                .map(|f| match self.field_slot(&f.ty) {
                    Some(slot) => Ok((f.name.clone(), slot)),
                    None => Err(unsupported(
                        "a struct payload field",
                        "only single-word primitive and unit field types are lowerable this round",
                    )),
                })
                .collect(),
            _ => Err(unsupported(
                "an enum, tuple, application, or function member",
                "the M3 runtime contract covers unit, primitive, and struct atoms only",
            )),
        }
    }

    /// The word type of a written field type: primitives and unit only.
    fn field_slot(&self, expr: &TypeExpr) -> Option<SlotType> {
        let TypeExpr::Atom(id) = expr else {
            return None;
        };
        let universe = &self.analyzed.decls.universe;
        let decl = universe.atom_decl(*id).ok()?;
        if let Some(slot) = primitive_slot(&decl.name) {
            return Some(slot);
        }
        match decl.kind {
            ds_check::AtomKind::Unit => Some(SlotType::I64), // a unit word
            _ => None,
        }
    }
}

fn word_of(index: usize) -> Result<u8, LowerError> {
    u8::try_from(index).map_err(|_| unsupported("a value span", "more than 255 payload words"))
}

fn unsupported(what: &'static str, reason: &'static str) -> LowerError {
    LowerError::unsupported(NO_SPAN, what, reason)
}
