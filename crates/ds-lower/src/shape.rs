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
//! The corpus declares `int`/`float`/`string`/`bool`/`char` as ambient
//! tags (conformance/README.md); the lowerer maps those names onto the
//! slot types of the same names, with `char` riding the string Ref —
//! one scalar per word, the pinned list-separator convention. `none` is
//! the universe's predeclared unit atom and needs no declaration.

use std::{collections::HashMap, rc::Rc};

use ds_abi::{BuiltinInstance, SlotType};
use ds_check::{Analyzed, Ty, TypeExpr, TypeId};

use crate::error::LowerError;

/// Span placeholder for shape-level errors: shapes are built from types,
/// not nodes, so no source span sits behind them.
const NO_SPAN: ds_diag::Span = ds_diag::Span::new(ds_diag::FileId::new(0), 0, 0);

/// The primitive vocabulary: atom name to slot type. These atoms are
/// declared as ambient `tag` names by the corpus and check like any tag.
/// `char` shares the string Ref: one scalar value per one-char string
/// word, the runtime convention the list natives' separator pins.
const PRIMITIVES: &[(&str, SlotType)] = &[
    ("int", SlotType::I64),
    ("float", SlotType::F64),
    ("bool", SlotType::Bool),
    ("string", SlotType::Ref),
    ("char", SlotType::Ref),
];

fn primitive_slot(name: &str) -> Option<SlotType> {
    PRIMITIVES.iter().find(|(n, _)| *n == name).map(|(_, s)| *s)
}

/// The heap-reference constructors: an application of one of these is a
/// single RC'd object behind one Ref word — the list and map natives'
/// objects.
const HEAP_REFS: &[&str] = &["list", "map"];

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
    /// An anonymous tuple: one positional word per element (B3). The
    /// fields carry `None` names and their `word` is the element
    /// position; the shape never spans more words than elements.
    Tuple(StructShape),
    /// A tag+payload union span.
    Union(UnionShape),
}

/// The first tag id the platform reserves at the top of the u8 space,
/// counting down from the fallible-native success discriminant through
/// the cancellation tag: module lowering mints ids upward from 0 and a
/// mint that reached the reserved range would alias a user member with
/// a platform span discriminant (TAG_TEST dispatch is byte equality).
/// Pinned against the VM's deepest reservation by the native-table
/// drift test.
pub(crate) const FIRST_RESERVED_TAG: u8 = u8::MAX - 5;

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
    /// while functions lower in declaration order. The counter is
    /// checked against the platform's reserved top: a module with more
    /// distinct tag atoms than the space below it leaves refuses with a
    /// typed error instead of wrapping into the reserved range.
    pub(crate) fn tag_id(&mut self, atom: TypeId) -> Result<u8, LowerError> {
        if let Some(tag) = self.tags.get(&atom) {
            return Ok(*tag);
        }
        if self.next_tag >= FIRST_RESERVED_TAG {
            return Err(unsupported(
                "a union member",
                "the module's distinct tag atoms exceed the tag-id space below the platform's \
                 reserved top",
            ));
        }
        let tag = self.next_tag;
        self.next_tag += 1;
        self.tags.insert(atom, tag);
        Ok(tag)
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
            Shape::Struct(s) | Shape::Tuple(s) => s.fields.iter().map(|f| f.slot).collect(),
            Shape::Union(u) => u.words.clone(),
        })
    }

    /// A union span: the tag word plus the shared payload region, member
    /// fields assigned per the module-doc strategy.
    fn union_shape(&mut self, members: &[TypeId]) -> Result<Shape, LowerError> {
        let mut region: Vec<SlotType> = Vec::new();
        let mut shapes = Vec::with_capacity(members.len());
        for atom in members {
            let tag = self.tag_id(*atom)?;
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
            ds_check::AtomKind::Tuple(elements) => {
                let mut fields = Vec::with_capacity(elements.len());
                for (index, element) in elements.iter().enumerate() {
                    // One plain frame word per element: struct, union, and
                    // nested-tuple elements carry their own spans and need
                    // reboxing (the open alternative in this module's doc).
                    let shape = self.atom_shape(*element)?;
                    let Shape::Word(slot) = shape else {
                        return Err(unsupported(
                            "a multi-word tuple element",
                            "tuple elements lower as one plain frame word this round; struct, \
                             union, and nested-tuple elements need reboxing",
                        ));
                    };
                    fields.push(MemberField {
                        name: None,
                        slot,
                        word: word_of(index)?,
                    });
                }
                Ok(Shape::Tuple(StructShape { fields }))
            },
            // a heap-reference application: one Ref word, the object
            // behind it owned by the runtime (the list object)
            ds_check::AtomKind::Apply(ctor, _) if self.heap_ref(*ctor) => {
                Ok(Shape::Word(SlotType::Ref))
            },
            // A fn value is one Fn word (the callable-word ruling, F2):
            // a non-capturing lambda mints its copyable scalar FuncRef
            // into the slot; a capturing lambda's VALUE is a Ref word
            // naming the closure object, flowing through value-site
            // places and callable-defaulted fn-typed parameter slots.
            ds_check::AtomKind::FnSig(..) => Ok(Shape::Word(SlotType::Fn)),
            _ => Err(unsupported(
                "an enum or application value",
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
            // Positional slots (tuple-payload enum cases [B3]) keep
            // their `None` name; the span places them by word offset.
            ds_check::AtomKind::Struct(fields) => fields
                .iter()
                .map(|f| match self.field_slot(&f.ty) {
                    Some(slot) => Ok((f.name.clone(), slot)),
                    None => Err(unsupported(
                        "a struct payload field",
                        "only single-word primitive and unit field types are lowerable this round",
                    )),
                })
                .collect(),
            ds_check::AtomKind::Tuple(_) => Err(unsupported(
                "a tuple member of a union",
                "a tuple payload spans several words and the union region gives members one word \
                 each this round; reboxing is the open alternative",
            )),
            // A callable member's payload is the one Fn word (F2): the
            // checked read hands the element word out under the element's
            // own classification — a closure element refuses at the
            // construction guard, so the word reaching a payload slot is
            // the scalar FuncRef.
            ds_check::AtomKind::FnSig(..) => Ok(vec![(None, SlotType::Fn)]),
            _ => Err(unsupported(
                "an enum, application, or function member",
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

    /// The builtin instance of a generic call's receiver type [H3]: the
    /// statically-resolved instance reference a CALLG site stages as
    /// its evidence words. The covered vocabulary is the builtin
    /// instance set — `string`, the `list`/`map` container constructors,
    /// and the `int`/`float` scalars — a single-atom type check, so
    /// unions and unresolved variables answer None and the call site
    /// reports the uncovered receiver.
    pub(crate) fn builtin_instance(&self, ty: Ty) -> Option<BuiltinInstance> {
        let universe = &self.analyzed.decls.universe;
        let [only] = universe.members(ty) else {
            return None;
        };
        let decl = universe.atom_decl(*only).ok()?;
        match &decl.kind {
            ds_check::AtomKind::Apply(ctor, _) if self.heap_ref(*ctor) => {
                let ctor_name = universe.atom_decl(*ctor).ok()?.name.clone();
                match ctor_name.as_str() {
                    "list" => Some(BuiltinInstance::List),
                    "map" => Some(BuiltinInstance::Map),
                    _ => None,
                }
            },
            _ => match decl.name.as_str() {
                "string" => Some(BuiltinInstance::Str),
                "int" => Some(BuiltinInstance::Int),
                "float" => Some(BuiltinInstance::Float),
                _ => None,
            },
        }
    }

    /// Whether an atom is a heap-reference constructor.
    fn heap_ref(&self, ctor: TypeId) -> bool {
        self.analyzed
            .decls
            .universe
            .atom_decl(ctor)
            .is_ok_and(|decl| HEAP_REFS.contains(&decl.name.as_str()))
    }
}

fn word_of(index: usize) -> Result<u8, LowerError> {
    u8::try_from(index).map_err(|_| unsupported("a value span", "more than 255 payload words"))
}

fn unsupported(what: &'static str, reason: &'static str) -> LowerError {
    LowerError::unsupported(NO_SPAN, what, reason)
}
