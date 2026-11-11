//! Value shapes: the slot-level representation of solved types [BL-2].
//!
//! Every value occupies a span of frame slots — one word per scalar, a
//! bare field span per struct, and a tag word plus a payload region per
//! union. This module owns the mapping and the module-wide tag-id table.
//!
//! # Union payload strategy (the M3 choice, per the golden fixtures)
//!
//! One payload region per union TYPE: each member's fields claim the
//! lowest free region span whose word types match the field's span, with
//! contiguity mandatory within one field, and a word stays claimable by
//! every other member. Two members share a word when their word types
//! agree (the golden fixture's `bad_port.got` and `config.port` share an
//! I64 word), so the region width is the number of distinct word shapes
//! members need, not the member count. Every region word keeps its
//! `SlotType` for the union's life, and dead words are zero-initialized
//! per word. The claim algorithm is span-generic — the inline-spans
//! design, with the reboxing alternative rejected — and a struct-typed
//! field claims its whole flattened word span (a multi-word field);
//! union, tuple, and application field types still refuse, so those
//! spans wait on the span channel.
//!
//! # Primitive atoms
//!
//! The corpus declares `int`/`float`/`string`/`bool`/`char` as ambient
//! tags (conformance/README.md); the lowerer maps those names onto the
//! slot types of the same names, with `char` riding the string Ref —
//! one scalar per word, the pinned list-separator convention. `byte`
//! (the F6 ruling) rides the I64 slot as its 0-255 value: its own atom
//! in the u8 width class, outside the int conversion family — the
//! checker keeps byte and int disjoint, lowering shares the integer
//! word. `none` is the universe's predeclared unit atom and needs no
//! declaration.

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
/// `byte` shares the integer slot [F6]: the word carries the 0-255
/// value, while the checker holds it outside the int conversion family.
const PRIMITIVES: &[(&str, SlotType)] = &[
    ("int", SlotType::I64),
    ("byte", SlotType::I64),
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

/// One field of a value's span: the field's name (or `None` for a
/// primitive member's own payload word), its consecutive word types, and
/// the span offset its first word lives at. A field spans one word
/// unless its type is itself a struct, whose whole inline span the field
/// claims contiguously.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MemberField {
    /// The declared field name; `None` marks a primitive member's payload.
    pub name: Option<String>,
    /// The field's word types, in span order.
    pub slots: Vec<SlotType>,
    /// Offset of the field's first word from the span base.
    pub word: u8,
}

impl MemberField {
    /// The field's base word type.
    pub(crate) fn slot(&self) -> SlotType {
        self.slots[0]
    }

    /// How many words the field spans.
    pub(crate) fn width(&self) -> usize {
        self.slots.len()
    }
}

/// One payload field's word-type span: the field's consecutive word
/// types, the claim input for a union region and the placement input for
/// a bare struct. A struct-typed field carries its flattened multi-word
/// span; every other field type is one word or refuses.
struct PayloadField {
    /// The declared field name; `None` marks a positional payload word.
    name: Option<String>,
    /// The field's word types, in span order.
    slots: Vec<SlotType>,
}

impl PayloadField {
    /// A one-word payload field.
    fn word(name: Option<String>, slot: SlotType) -> Self {
        Self {
            name,
            slots: vec![slot],
        }
    }
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
    /// Every struct whose declaration carries a `deinit` member: the
    /// positions a deinit value cannot inhabit (a union payload)
    /// refuse against it.
    declared_deinits: std::collections::HashSet<ds_check::DefId>,
    cache: HashMap<Ty, Rc<Shape>>,
    tags: HashMap<TypeId, u8>,
    next_tag: u8,
}

impl<'a> Shapes<'a> {
    pub(crate) fn new(
        analyzed: &'a Analyzed,
        declared_deinits: std::collections::HashSet<ds_check::DefId>,
    ) -> Self {
        Self {
            analyzed,
            declared_deinits,
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

    /// A union member that declares a deinit refuses (C5): the union's
    /// death decs the live member's payload words through the placement
    /// and frees nested objects through the VM cascade, but neither
    /// carries the member value's own hook — storing a deinit value in
    /// a union payload would silently skip its deinit.
    fn guard_deinit_member(&self, atom: TypeId) -> Result<(), LowerError> {
        let Some(def) = self.analyzed.decls.defs_of_atoms.get(&atom) else {
            return Ok(());
        };
        if self.declared_deinits.contains(def) {
            return Err(unsupported(
                "a deinit-carrying union member",
                "the union's death decs the member's payload words and frees nested objects, but \
                 dispatches no user deinit; a deinit value's death is its own binding's",
            ));
        }
        Ok(())
    }

    /// Whether an atom is one of the checker's minted type variables: a
    /// generic parameter of an impl, function, or alias, named `$...`
    /// (`$` sits outside the identifier charset). Its values ride one
    /// Raw word — the runtime kind is the caller's fact, checked by the
    /// VM at every hand-off, never a static one.
    pub(crate) fn is_var_atom(&self, atom: TypeId) -> bool {
        self.analyzed
            .decls
            .universe
            .atom_decl(atom)
            .is_ok_and(|decl| decl.name.starts_with('$'))
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
            Shape::Struct(s) | Shape::Tuple(s) => s
                .fields
                .iter()
                .flat_map(|f| f.slots.iter().copied())
                .collect(),
            Shape::Union(u) => u.words.clone(),
        })
    }

    /// A union span: the tag word plus the shared payload region, member
    /// fields claiming the lowest free region span per the module-doc
    /// strategy. A member's `claimed` set scopes to that member alone:
    /// members share region words by kind, one member's fields never
    /// share a word among themselves.
    fn union_shape(&mut self, members: &[TypeId]) -> Result<Shape, LowerError> {
        let mut region: Vec<SlotType> = Vec::new();
        let mut shapes = Vec::with_capacity(members.len());
        for atom in members {
            let tag = self.tag_id(*atom)?;
            self.guard_deinit_member(*atom)?;
            let payload = self.payload_fields(*atom, 0)?;
            let mut claimed = vec![false; region.len()];
            let mut fields = Vec::with_capacity(payload.len());
            for field in &payload {
                let word = claim_span(&mut region, &mut claimed, &field.slots)?;
                fields.push(MemberField {
                    name: field.name.clone(),
                    slots: field.slots.clone(),
                    word,
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
            // a type variable's value is one Raw word: the runtime kind
            // is the caller's fact (the VM checks it at every hand-off)
            ds_check::AtomKind::Unit if self.is_var_atom(atom) => Ok(Shape::Word(SlotType::Raw)),
            ds_check::AtomKind::Unit => Ok(Shape::Word(
                primitive_slot(&decl.name).unwrap_or(SlotType::Tag),
            )),
            ds_check::AtomKind::Struct(_) => {
                let payload = self.payload_fields(atom, 0)?;
                let mut fields = Vec::with_capacity(payload.len());
                // a bare struct places its field spans at consecutive
                // words; no sharing, so a cursor suffices
                let mut cursor = 0usize;
                for field in payload {
                    let word = word_of(cursor)?;
                    cursor += field.slots.len();
                    fields.push(MemberField {
                        name: field.name,
                        slots: field.slots,
                        word,
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
                    // nested-tuple elements carry their own spans, and the
                    // tuple vocabulary keeps one word per element until
                    // those spans stage per element.
                    let shape = self.atom_shape(*element)?;
                    let Shape::Word(slot) = shape else {
                        return Err(unsupported(
                            "a multi-word tuple element",
                            "tuple elements lower as one plain frame word; a wider element needs \
                             its whole span staged per element, which tuple spans do not carry yet",
                        ));
                    };
                    fields.push(MemberField {
                        name: None,
                        slots: vec![slot],
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

    /// One atom's payload: the member's fields as word-type spans. Unit
    /// tags carry no payload; primitives carry their own word; struct
    /// members carry their declared fields, a struct-typed field carrying
    /// its whole flattened span. Union, tuple, and application field
    /// types refuse.
    fn payload_fields(&self, atom: TypeId, depth: usize) -> Result<Vec<PayloadField>, LowerError> {
        let universe = &self.analyzed.decls.universe;
        let Ok(decl) = universe.atom_decl(atom) else {
            return Err(unsupported("a value", "its atom has no declaration"));
        };
        match &decl.kind {
            // a union member over a type variable has no static payload
            // shape: the member's runtime word kind is the caller's fact,
            // and a checked read would need runtime reflection to re-span
            ds_check::AtomKind::Unit if self.is_var_atom(atom) => Err(unsupported(
                "a union member over a type variable",
                "the variable's runtime word kind is a runtime fact; a runtime reflection table \
                 is the seam",
            )),
            ds_check::AtomKind::Unit => match primitive_slot(&decl.name) {
                Some(slot) => Ok(vec![PayloadField::word(None, slot)]),
                None => Ok(Vec::new()),
            },
            // Positional slots (tuple-payload enum cases [B3]) keep
            // their `None` name; the span places them by word offset.
            ds_check::AtomKind::Struct(fields) => fields
                .iter()
                .map(|f| match self.field_span(&f.ty, depth)? {
                    Some(slots) => Ok(PayloadField {
                        name: f.name.clone(),
                        slots,
                    }),
                    None => Err(unsupported(
                        "a struct payload field",
                        "struct fields lower as inline word spans of primitive, unit, or struct \
                         type; union, tuple, and application field types stay outside the \
                         inline-span vocabulary",
                    )),
                })
                .collect(),
            ds_check::AtomKind::Tuple(_) => Err(unsupported(
                "a tuple member of a union",
                "a tuple payload spans several words the region would have to carry as one \
                 contiguous claim; the region takes struct member fields only",
            )),
            // A callable member's payload is the one Fn word (F2): the
            // checked read hands the element word out under the element's
            // own classification — a closure element refuses at the
            // construction guard, so the word reaching a payload slot is
            // the scalar FuncRef.
            ds_check::AtomKind::FnSig(..) => Ok(vec![PayloadField::word(None, SlotType::Fn)]),
            _ => Err(unsupported(
                "an enum, application, or function member",
                "the M3 runtime contract covers unit, primitive, and struct atoms only",
            )),
        }
    }

    /// The word span of a written field type: a primitive or unit one
    /// word, a type variable's Raw word, and a struct's whole flattened
    /// inline span (its fields' spans concatenated). `None` is the shared
    /// refusal: union, tuple, and application types stay outside the
    /// inline-span vocabulary. `depth` bounds the recursion — a cyclic
    /// field type has no finite span, and the budget refuses it typed
    /// instead of recursing.
    fn field_span(
        &self,
        expr: &TypeExpr,
        depth: usize,
    ) -> Result<Option<Vec<SlotType>>, LowerError> {
        const SPAN_DEPTH_LIMIT: usize = 32;
        let TypeExpr::Atom(id) = expr else {
            return Ok(None);
        };
        let universe = &self.analyzed.decls.universe;
        let Ok(decl) = universe.atom_decl(*id) else {
            return Ok(None);
        };
        if self.is_var_atom(*id) {
            return Ok(Some(vec![SlotType::Raw]));
        }
        if let Some(slot) = primitive_slot(&decl.name) {
            return Ok(Some(vec![slot]));
        }
        match decl.kind {
            ds_check::AtomKind::Unit => Ok(Some(vec![SlotType::I64])), // a unit word
            ds_check::AtomKind::Struct(_) => {
                if let Some(def) = self.analyzed.decls.defs_of_atoms.get(id)
                    && self.declared_deinits.contains(def)
                {
                    return Err(unsupported(
                        "a struct payload field",
                        "a deinit-carrying field type has no claim of its own inside the inline \
                         span, so its hook would run at the wrong death; per-span deinit claims \
                         own the seam",
                    ));
                }
                if depth >= SPAN_DEPTH_LIMIT {
                    return Err(unsupported(
                        "a struct payload field",
                        "the field's inline span nests past the depth budget; a cyclic field type \
                         has no finite span",
                    ));
                }
                let payload = self.payload_fields(*id, depth + 1)?;
                Ok(Some(
                    payload.into_iter().flat_map(|field| field.slots).collect(),
                ))
            },
            _ => Ok(None),
        }
    }

    /// The element type of one whole-list application [F7]: iteration
    /// and membership read it. `None` for any other type — a union of
    /// lists, a non-list application, a non-application.
    pub(crate) fn list_element(&self, ty: Ty) -> Option<Ty> {
        let universe = &self.analyzed.decls.universe;
        let [only] = universe.members(ty) else {
            return None
        };
        let decl = universe.atom_decl(*only).ok()?;
        let ds_check::AtomKind::Apply(ctor, args) = &decl.kind else {
            return None
        };
        if !self.heap_ref(*ctor) {
            return None;
        }
        let [element] = &**args else { return None };
        (universe.atom_decl(*ctor).ok()?.name == "list").then_some(*element)
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

/// The generic parameter name behind one minted type variable: the
/// shared counter mints `${name}#N` (both `#` and `$` sit outside the
/// identifier charset, so the split is unambiguous). Two minting sites
/// exist — instance declaration and body checking — and their atoms
/// differ, so consumers bridging the two (the runtime-evidence seam) key
/// by this name.
pub(crate) fn var_param_name(universe: &ds_check::Universe, atom: TypeId) -> Option<String> {
    let decl = universe
        .atom_decl(atom)
        .ok()
        .filter(|decl| decl.name.starts_with('$'))?;
    let base = decl.name.strip_prefix('$')?;
    base.rsplit_once('#').map(|(name, _)| name.to_owned())
}

/// Claims the lowest free contiguous region span matching `span`'s word
/// types, growing the region past its end when no gap fits: a field's
/// words sit at consecutive region words (contiguity is mandatory), a
/// claimed word is invisible to the member's later fields, and every
/// region word keeps its `SlotType` for the union's life — a word only
/// ever hosts values of its own kind. Returns the field's base offset
/// into the union span (the tag word at 0 precedes the region).
fn claim_span(
    region: &mut Vec<SlotType>,
    claimed: &mut Vec<bool>,
    span: &[SlotType],
) -> Result<u8, LowerError> {
    debug_assert!(!span.is_empty(), "a payload field spans at least one word");
    let start = (0..=region.len())
        .find(|&start| span_fits(region, claimed, span, start))
        .expect("a span fits by growing past the region end");
    while region.len() < start + span.len() {
        region.push(span[region.len() - start]);
        claimed.push(false);
    }
    claimed[start..start + span.len()].fill(true);
    word_of(start + 1)
}

/// Whether `span` fits contiguously at `start`: each of its words either
/// matches a free region word of its own type or sits past the region
/// end, where the claim grows the region.
fn span_fits(region: &[SlotType], claimed: &[bool], span: &[SlotType], start: usize) -> bool {
    (0..span.len()).all(|k| {
        let pos = start + k;
        pos >= region.len() || (region[pos] == span[k] && !claimed[pos])
    })
}

fn word_of(index: usize) -> Result<u8, LowerError> {
    u8::try_from(index).map_err(|_| unsupported("a value span", "more than 255 payload words"))
}

fn unsupported(what: &'static str, reason: &'static str) -> LowerError {
    LowerError::unsupported(NO_SPAN, what, reason)
}

#[cfg(test)]
mod tests {
    use SlotType::{Bool, F64, I64, Ref};
    use ds_diag::{Files, human};
    use ds_parse::parse;

    use super::*;

    /// Claims one member's field spans against the shared region, in
    /// order: the per-member `claimed` starts clear, exactly as
    /// `union_shape` processes one member.
    fn claim_member(region: &mut Vec<SlotType>, spans: &[&[SlotType]]) -> Vec<u8> {
        let mut claimed = vec![false; region.len()];
        spans
            .iter()
            .map(|span| claim_span(region, &mut claimed, span).expect("the span claims"))
            .collect()
    }

    #[test]
    fn one_word_claims_stay_lowest_free_word() {
        // The width-1 rule is the span rule's degenerate case: config's
        // host/port claim the first two region words, bad_port's two int
        // words reuse the port word and grow one — the golden fixture's
        // word sharing, byte-identical through the span rework.
        let mut region = Vec::new();
        assert_eq!(claim_member(&mut region, &[&[Ref], &[I64]]), [1, 2]);
        assert_eq!(claim_member(&mut region, &[&[I64], &[I64]]), [2, 3]);
        assert_eq!(region, [Ref, I64, I64]);
    }

    #[test]
    fn a_multi_word_span_needs_one_contiguous_gap() {
        // Contiguity is mandatory: no two adjacent free I64 words exist
        // before the region's trailing pair, so the [I64, I64] span
        // starts after the Bool word instead of at the lowest free I64
        // word.
        let mut region = Vec::new();
        claim_member(&mut region, &[&[I64], &[Bool], &[I64], &[I64]]);
        assert_eq!(region, [I64, Bool, I64, I64]);
        assert_eq!(claim_member(&mut region, &[&[I64, I64]]), [3]);
        assert_eq!(region, [I64, Bool, I64, I64]);
    }

    #[test]
    fn a_span_grows_the_region_only_for_its_tail() {
        // The lowest fitting span reuses free region words first and
        // grows the region only past its end, keeping every grown word's
        // kind stable.
        let mut region = vec![I64];
        assert_eq!(claim_member(&mut region, &[&[I64, I64, I64]]), [1]);
        assert_eq!(region, [I64, I64, I64]);
    }

    #[test]
    fn one_members_spans_share_no_word() {
        // Member-local disjointness: the member's second field skips
        // the words its first span claimed (cross-member word reuse
        // stays the design's sharing, pinned above).
        let mut region = Vec::new();
        assert_eq!(claim_member(&mut region, &[&[I64, I64], &[I64]]), [1, 3]);
        assert_eq!(region, [I64, I64, I64]);
    }

    #[test]
    fn every_region_word_keeps_its_kind() {
        // A word claimed by one kind never serves another: the F64
        // field grows the region instead of reusing Bool or I64 words,
        // and the later [I64, I64] span skips the F64 word mid-gap.
        let mut region = Vec::new();
        claim_member(&mut region, &[&[Bool], &[I64]]);
        assert_eq!(claim_member(&mut region, &[&[F64], &[I64, I64]]), [3, 4]);
        assert_eq!(region, [Bool, I64, F64, I64, I64]);
    }

    /// A corpus union: two payloaded struct members sharing the port
    /// word plus a bare tag, a narrower two-member union, struct
    /// literals, var widening, and both match shapes.
    const UNION_SRC: &str = concat!(
        "tag int\n",
        "tag string\n",
        "tag io_error\n",
        "struct config { host: string, port: int }\n",
        "struct bad_port { expected: int, got: int }\n",
        "extern fn println (msg: string) -> ()\n",
        "fn port_of(r: config | io_error) -> int {\n",
        "  match r {\n",
        "    io_error => 0,\n",
        "    c => c.port,\n",
        "  }\n",
        "}\n",
        "fn show(r: config | bad_port | io_error) -> () {\n",
        "  match r {\n",
        "    io_error => println(\"io\"),\n",
        "    c => println(\"payload\"),\n",
        "  }\n",
        "}\n",
        "fn main() -> () {\n",
        "  var store: config | bad_port | io_error = config { host: \"h\", port: 1 }\n",
        "  store = bad_port { expected: 2, got: 3 }\n",
        "  show(store)\n",
        "  show(io_error)\n",
        "  var p = port_of(io_error)\n",
        "  store = config { host: \"x\", port: p }\n",
        "  show(store)\n",
        "}\n",
    );

    /// The shape of one source-rendered type, claimed through the same
    /// `Shapes` the lowerer drives; no lowering runs here.
    fn shape_of(source: &str, rendered: &str) -> Rc<Shape> {
        let mut files = Files::new();
        let id = files.add_source("probe.ds", source);
        let asts = [parse(&files, id).expect("the probe parses")];
        let analyzed = ds_check::analyze(&files, &asts);
        assert!(
            analyzed.checked.diags.is_empty(),
            "{}",
            analyzed
                .checked
                .diags
                .iter()
                .map(|diag| human(diag, &files))
                .collect::<String>()
        );
        let ty = analyzed
            .checked
            .expr_types
            .values()
            .map(|val| val.ty)
            .find(|ty| analyzed.decls.universe.render(*ty) == rendered)
            .unwrap_or_else(|| panic!("no expression of {rendered}"));
        let mut shapes = Shapes::new(&analyzed, std::collections::HashSet::new());
        shapes.shape(ty).expect("the shape lowers")
    }

    #[test]
    fn the_corpus_union_keeps_its_pinned_region() {
        // Recorded from the pre-rework allocator, byte-identical after
        // it: io_error carries no payload, config claims words 1-2,
        // bad_port reuses the port word and grows one.
        let shape = shape_of(UNION_SRC, "io_error | config | bad_port");
        let Shape::Union(u) = &*shape else {
            panic!("the corpus type is a union");
        };
        assert_eq!(u.words, [SlotType::Tag, Ref, I64, I64]);
        let [bare, config, bad] = u.members.as_slice() else {
            panic!("the corpus union has three members");
        };
        assert_eq!(bare.fields, []);
        assert_eq!(config.fields, [
            MemberField {
                name: Some("host".into()),
                slots: vec![Ref],
                word: 1,
            },
            MemberField {
                name: Some("port".into()),
                slots: vec![I64],
                word: 2,
            },
        ]);
        assert_eq!(bad.fields, [
            MemberField {
                name: Some("expected".into()),
                slots: vec![I64],
                word: 2,
            },
            MemberField {
                name: Some("got".into()),
                slots: vec![I64],
                word: 3,
            },
        ]);
        // the narrower union claims the narrower region
        let narrow = shape_of(UNION_SRC, "io_error | config");
        let Shape::Union(u) = &*narrow else {
            panic!("the narrower corpus type is a union");
        };
        assert_eq!(u.words, [SlotType::Tag, Ref, I64]);
    }

    /// A struct-typed field claims its real span width: the width flip
    /// the span rework lands. `boxed.at` (a two-word struct) claims two
    /// contiguous region words, the following scalar grows past the span,
    /// and the two-Ref member's fields claim their own grown words.
    #[test]
    fn a_struct_typed_field_claims_its_whole_span_width() {
        let src = concat!(
            "tag int\n",
            "tag string\n",
            "struct point { x: int, y: int }\n",
            "struct boxed { at: point, n: int }\n",
            "struct wide { a: string, b: string }\n",
            "tag miss\n",
            "extern fn println (msg: string) -> ()\n",
            "fn area(b: boxed | miss) -> int {\n",
            "  match b {\n",
            "    miss => 0,\n",
            "    b => b.at.x + b.at.y + b.n,\n",
            "  }\n",
            "}\n",
            "fn show(w: wide | miss) -> () {\n",
            "  match w {\n",
            "    miss => println(\"miss\"),\n",
            "    w => println(w.a),\n",
            "  }\n",
            "}\n",
            "fn whole(t: boxed | wide | miss) -> int {\n",
            "  match t {\n",
            "    miss => 0,\n",
            "    other => 1,\n",
            "  }\n",
            "}\n",
        );
        let shape = shape_of(src, "boxed | wide | miss");
        let Shape::Union(u) = &*shape else {
            panic!("the probe type is a union");
        };
        // boxed claims [I64, I64] at words 1-2 then n at word 3; wide
        // grows the region with its two Ref words
        assert_eq!(u.words, [SlotType::Tag, I64, I64, I64, Ref, Ref]);
        let [boxed, wide, miss] = u.members.as_slice() else {
            panic!("the probe union has three members");
        };
        assert_eq!(miss.fields, []);
        assert_eq!(boxed.fields, [
            MemberField {
                name: Some("at".into()),
                slots: vec![I64, I64],
                word: 1,
            },
            MemberField {
                name: Some("n".into()),
                slots: vec![I64],
                word: 3,
            },
        ]);
        assert_eq!(wide.fields, [
            MemberField {
                name: Some("a".into()),
                slots: vec![Ref],
                word: 4,
            },
            MemberField {
                name: Some("b".into()),
                slots: vec![Ref],
                word: 5,
            },
        ]);
        // a bare struct's own layout flattens the same field span, its
        // words starting at 0 (no tag word before them)
        let bare = shape_of(src, "boxed");
        let Shape::Struct(s) = &*bare else {
            panic!("boxed is a bare struct");
        };
        assert_eq!(
            s.fields
                .iter()
                .map(|f| (f.name.clone(), f.slots.clone(), f.word))
                .collect::<Vec<_>>(),
            vec![
                (Some("at".into()), vec![I64, I64], 0),
                (Some("n".into()), vec![I64], 2),
            ]
        );
    }

    #[test]
    fn member_spans_stay_disjoint_and_zero_init_covers_every_dead_word() {
        // The silent-breakage tripwires of the span rework: within one
        // member no two fields share a word, every field word matches
        // the region word's kind, and the live words leave every other
        // word dead — the words zero-init writes once each, with each
        // word's own kind.
        let shape = shape_of(UNION_SRC, "io_error | config | bad_port");
        let Shape::Union(u) = &*shape else {
            panic!("the corpus type is a union");
        };
        for member in &u.members {
            let mut live = std::collections::HashSet::new();
            for field in &member.fields {
                let word = usize::from(field.word);
                assert!(
                    (1..u.words.len()).contains(&word),
                    "the field stays inside the payload region"
                );
                for (offset, slot) in field.slots.iter().enumerate() {
                    assert_eq!(
                        u.words[word + offset],
                        *slot,
                        "every word of the field's span keeps its claimed kind"
                    );
                }
                for offset in 0..field.width() {
                    assert!(
                        live.insert(field.word + u8::try_from(offset).expect("offset fits u8")),
                        "one member never shares a word"
                    );
                }
            }
            let dead: std::collections::HashSet<u8> = (1..u.words.len())
                .filter_map(|index| u8::try_from(index).ok())
                .filter(|word| !live.contains(word))
                .collect();
            assert_eq!(
                live.len() + dead.len(),
                u.words.len() - 1,
                "live and dead words partition the region exactly"
            );
        }
    }
}
