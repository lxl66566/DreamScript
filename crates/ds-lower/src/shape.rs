//! Value shapes: the slot-level representation of solved types.
//!
//! Every value occupies a span of frame slots — one word per scalar, a
//! bare field span per struct, and a tag word plus a payload region per
//! union. This module owns the mapping and the module-wide tag-id table.
//!
//! # The unit word
//!
//! A part-less struct spans one reserved word in every value position —
//! an unnamed I64 word whose content is never written or read (the
//! frame's inert zero holds it), so no value spans zero words and the
//! ABI's RETSPAN minimum holds without exception.
//!
//! # Union payload strategy
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
//! field claims its whole flattened word span (a multi-word field); a
//! union-typed field claims the union's own tag-plus-region span, a
//! tuple field its elements' whole spans, and a heap-application
//! field (`list[t]`, `map[k, v]`) its one Ref word. Bare tuples place each
//! element's whole span at consecutive offsets. A member value stored into a union-typed field
//! re-spans through the same tag/zero/write construction the
//! annotated-binding widen uses.
//!
//! # Primitive atoms
//!
//! The ambient declares `int`/`float`/`string`/`bool`/`char` as tags;
//! the lowerer maps them onto the slot types of the same names, with
//! `char` riding the I64 slot as its code point — one unboxed scalar per
//! word, the text spelling (`show`, interpolation) encoding UTF-8 at the
//! conversion seam. `byte` rides the I64 slot as its 0-255
//! value: its own atom in the u8 width class, outside the int conversion
//! family — the checker keeps byte and int disjoint, lowering shares the
//! integer word. `none` is the universe's predeclared unit atom and needs
//! no declaration.

use std::{collections::HashMap, fmt::Write as _, rc::Rc};

use ds_abi::{BuiltinInstance, SlotType};
use ds_check::{Analyzed, Ty, TypeExpr, TypeId, Universe};

use crate::{error::LowerError, types::Subst};

/// Span placeholder for shape-level errors: shapes are built from types,
/// not nodes, so no source span sits behind them.
const NO_SPAN: ds_diag::Span = ds_diag::Span::new(ds_diag::FileId::new(0), 0, 0);

/// The primitive vocabulary's shape-layer half: each scalar word's
/// frame slot, keyed by the word itself so the shared word table (the
/// checker's linked-surface module owns the names) stays the only name
/// table — a widened primitive lands in both crates in one edit, this
/// match failing to compile until the new word's slot is mapped.
const fn scalar_slot(word: ds_check::import::ScalarWord) -> SlotType {
    use ds_check::import::ScalarWord;
    match word {
        // `byte`, `u32`, and `char` ride the integer slot: the word
        // carries the 0-255 or 32-bit value or the code point, the
        // checker holding each width outside the int conversions
        ScalarWord::Int | ScalarWord::Byte | ScalarWord::U32 | ScalarWord::Char => SlotType::I64,
        ScalarWord::Float => SlotType::F64,
        ScalarWord::Bool => SlotType::Bool,
        ScalarWord::Str => SlotType::Ref,
    }
}

/// The frame slot one primitive atom name maps onto: the shared
/// primitive word table's row the name spells, each row's slot carried
/// by its word (`bytes` the string-shaped Ref, the comptime `type`
/// atom the `Tag` word).
fn primitive_slot(name: &str) -> Option<SlotType> {
    match ds_check::import::primitive_word(name)? {
        ds_check::import::PrimitiveWord::Scalar(word) => Some(scalar_slot(word)),
        ds_check::import::PrimitiveWord::Bytes => Some(SlotType::Ref),
        ds_check::import::PrimitiveWord::ComptimeType => Some(SlotType::Tag),
    }
}

/// One field of a value's span: the field's name (or `None` for a
/// primitive member's own payload word), its consecutive word types, and
/// the span offset its first word lives at. A field claims its whole
/// inline span contiguously.
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
/// span; a tuple field's element carries its own whole span.
struct PayloadField {
    /// The declared field name; `None` marks a positional payload word.
    name: Option<String>,
    /// The field's word types, in span order.
    slots: Vec<SlotType>,
}

/// The part-less struct's reserved word: one unnamed I64 word whose
/// content is never written or read — the frame's inert zero holds it —
/// so a part-less struct spans one word in every value position (the
/// bare value, a struct-typed field, a union payload) and the one-word
/// minimum holds without exception.
fn reserved_word() -> PayloadField {
    PayloadField::word(None, SlotType::I64)
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

/// One member of a union span.
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

/// A tag+payload union span.
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

/// The word types of one value's span, shared from the type cache: a
/// read-only list, so a call site clones this handle and never builds a
/// fresh list per read.
pub(crate) type SpanWords = Rc<[SlotType]>;

/// The representation of one value type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Shape {
    /// One unboxed word (scalars, unit values, bare unit tags).
    Word(SlotType),
    /// A bare struct: fields at consecutive words.
    Struct(StructShape),
    /// An anonymous tuple: one positional span per element. The fields
    /// carry `None` names and their `word` is the element's span offset.
    Tuple(StructShape),
    /// A tag+payload union span.
    Union(UnionShape),
}

impl Shape {
    /// One union member's fields: the slice handle a union read borrows
    /// instead of copying the field list.
    pub(crate) fn member_fields(&self, atom: TypeId) -> Option<&[MemberField]> {
        match self {
            Shape::Union(u) => u.member(atom).map(|member| member.fields.as_slice()),
            _ => None,
        }
    }
}

/// The key one word of a value span takes in an instance key: a
/// scalar word keys by its slot, a heap-reference word by the builtin
/// instance behind it (`==`, `len`, and the other collection natives
/// dispatch on the instance, so `string` and `bytes` never share), and
/// a Tag word — or a Ref word no instance covers, such as `char` — by
/// its atom, since tag dispatch and the uncovered words key on the
/// member's identity.
#[derive(Clone, PartialEq, Eq, Hash)]
pub(crate) enum WordKey {
    /// A scalar or callable word: the slot alone (`int` and `byte` share
    /// one integer word; two fn types share one Fn word).
    Slot(SlotType),
    /// A heap-reference word: the builtin instance behind it.
    Heap(BuiltinInstance),
    /// A word keyed by its atom: a union member's Tag word and any word
    /// no builtin instance covers.
    Atom(TypeId),
}

/// The key one recorded type takes under one instantiation: the shape
/// with the field names dropped, so two argument types that lower the
/// same code share one body. A bare struct keys by its fields' word
/// spans; a struct declaring a `deinit` keys by its own atom and shares
/// with nothing (its binding sites claim its own hook); a tuple keys by
/// its elements; a union by its members, each member by atom with its
/// payload fields' (offset, span) pairs — tag dispatch and payload
/// placement key on those. A generic enum's application is one union
/// over its own case atoms: a payloaded case's atom is the application's
/// own mint and a bare case's the unit atom every application shares, so
/// two applications share a body exactly while their case atoms agree.
/// A type still naming a type variable refuses: the substitution
/// resolved every recorded type of the body.
/// One union member's key entry: the member's atom with its payload
/// fields' (offset, span) pairs, in claim order.
pub(crate) type UnionMemberKey = (TypeId, Vec<(u8, Vec<SlotType>)>);

#[derive(Clone, PartialEq, Eq, Hash)]
pub(crate) enum ShapeKey {
    /// One word.
    Word(WordKey),
    /// A bare struct: one entry per field, the field's word span.
    Struct(Vec<Vec<SlotType>>),
    /// A struct declaring a `deinit`: its own atom, unshared.
    DeinitStruct(TypeId),
    /// A tuple: one recursively keyed entry per element.
    Tuple(Vec<ShapeKey>),
    /// A union: one entry per member — the member's atom and its payload
    /// fields' (word offset, span) pairs, in claim order.
    Union(Vec<UnionMemberKey>),
}

/// The key one union takes: each member's atom with its payload fields'
/// (word offset, span) pairs, in claim order. A union-typed atom (a
/// generic enum's application) and a union of several atoms key alike.
fn union_key(union: &UnionShape) -> ShapeKey {
    ShapeKey::Union(
        union
            .members
            .iter()
            .map(|member| (member.atom, member_key(member)))
            .collect(),
    )
}

/// One union member's payload fields as key entries.
fn member_key(member: &MemberShape) -> Vec<(u8, Vec<SlotType>)> {
    member
        .fields
        .iter()
        .map(|field| (field.word, field.slots.clone()))
        .collect()
}

/// The claim-band tags of the teardown establishment marker (see the
/// placement pass): a deinit claim's dedicated band word holds SET while
/// the claim lives and CLEAR once any death path or consuming move spent
/// it. They sit inside the reserved range (below every module mint and
/// above the net failure classes) so a stale user word can
/// never spoof a set band in a recycled frame region; mirrored in the
/// VM's claim directory (crates/ds-vm/src/claims.rs), which decodes them
/// from the drop shape's own clear op.
pub(crate) const CLAIM_BAND_SET: u8 = u8::MAX - 8;
pub(crate) const CLAIM_BAND_CLEAR: u8 = u8::MAX - 9;

/// The first tag id the platform reserves at the top of the u8 space,
/// counting down from the fallible-native success discriminant through
/// the cancellation tag, the claim-band pair, the net failure classes,
/// and the utf8 decode failure to this floor: module lowering mints
/// ids upward from 0 and a mint that reached the reserved range would
/// alias a user member with a platform span discriminant (TAG_TEST
/// dispatch is byte equality). Pinned against the VM's deepest
/// reservation by the native-table drift test.
pub(crate) const FIRST_RESERVED_TAG: u8 = crate::native::UTF8_ERR;

/// Shapes of the types of one package, plus the module-wide tag ids.
///
/// The types are read through the lens: the registry is the one the
/// plan's discovery left (its own copy once a substitution minted a type),
/// and while a body lowers under an instantiation every type it reads is
/// substituted first. A variable atom reaching the shape layer under an
/// active instantiation is therefore a type that skipped the lens, and it
/// refuses — the one Raw word is the generic body's own shape, never an
/// instance's.
pub(crate) struct Shapes<'a> {
    analyzed: &'a Analyzed,
    /// The registry as this lowering reads it.
    universe: &'a Universe,
    /// The substituted types the discovery minted, keyed by (substitution,
    /// source type).
    minted: &'a HashMap<(u32, Ty), Ty>,
    /// The solved field signatures the plan's substitutions derived, the
    /// frozen read's first table over the analyzed package's own.
    apply_lens: &'a HashMap<TypeId, Rc<[ds_check::decls::SolvedField]>>,
    /// The per-case facts the plan's substitutions derived, the frozen
    /// read's first table over the analyzed package's own.
    enum_lens: &'a HashMap<TypeId, Rc<[ds_check::decls::AppliedCase]>>,
    /// The case atoms the substitutions minted, keyed by
    /// (substitution, the source record's own case atom).
    case_mints: &'a HashMap<(u32, TypeId), TypeId>,
    /// The instantiation the body being lowered was planned under.
    active: Option<Rc<Subst>>,
    /// Every struct whose declaration carries a `deinit` member: the
    /// positions a deinit value cannot inhabit (a union payload)
    /// refuse against it.
    declared_deinits: std::collections::HashSet<ds_check::DefId>,
    cache: HashMap<Ty, Rc<Shape>>,
    word_cache: HashMap<Ty, SpanWords>,
    /// Shapes of single atoms, cached for this `Shapes` lifetime: `atom_shape`
    /// reads only `analyzed` and `declared_deinits`, both fixed at
    /// construction, and mints no tag id, so an entry never goes stale
    /// (unlike `Universe::atom_ty`, which a mint clears).
    atom_cache: HashMap<TypeId, Rc<Shape>>,
    /// Word spans of the member lists the runtime-evidence chain
    /// substitutes per link: those lists key no `Ty`, so the cache is
    /// keyed by the list itself.
    member_cache: HashMap<Box<[TypeId]>, SpanWords>,
    tags: HashMap<TypeId, u8>,
    /// The served splice's rows whose labels resolve to no atom of this
    /// package: the ambient's own bodies mint those rows in a fresh walk
    /// (structural atoms a body check registers), while a compile whose
    /// analysis never touches them holds no atom to key. They ride the
    /// module's table verbatim and consume their ids, so the bodies
    /// lowered here continue minting after them.
    unkeyed_rows: Vec<ds_mod::MemberTag>,
    next_tag: u8,
}

impl<'a> Shapes<'a> {
    pub(crate) fn new(
        analyzed: &'a Analyzed,
        types: &'a crate::types::Types<'_>,
        declared_deinits: std::collections::HashSet<ds_check::DefId>,
    ) -> Self {
        Self {
            analyzed,
            universe: types.universe(),
            minted: types.minted(),
            apply_lens: types.apply_map(),
            enum_lens: types.apply_enum_map(),
            case_mints: types.case_mint_map(),
            active: None,
            declared_deinits,
            cache: HashMap::new(),
            word_cache: HashMap::new(),
            atom_cache: HashMap::new(),
            member_cache: HashMap::new(),
            tags: HashMap::new(),
            unkeyed_rows: Vec::new(),
            next_tag: 0,
        }
    }

    /// The solved field signature one application atom carries: the
    /// plan's derived entries first, the analyzed package's recorded
    /// ones under them.
    pub(crate) fn apply_fields_of(
        &self,
        atom: TypeId,
    ) -> Option<Rc<[ds_check::decls::SolvedField]>> {
        self.apply_lens
            .get(&atom)
            .cloned()
            .or_else(|| self.analyzed.decls.apply_structs.get(&atom).cloned())
    }

    /// The per-case facts one enum application atom carries: the
    /// plan's derived entries first, the analyzed package's recorded
    /// ones under them.
    pub(crate) fn apply_cases_of(
        &self,
        atom: TypeId,
    ) -> Option<Rc<[ds_check::decls::AppliedCase]>> {
        self.enum_lens
            .get(&atom)
            .cloned()
            .or_else(|| self.analyzed.decls.apply_enums.get(&atom).cloned())
    }

    /// Whether one atom is an application of a user-declared generic
    /// enum head: the value shape the application's own case atoms lay
    /// out, the payloaded ones minted per application.
    pub(crate) fn is_enum_application(&self, atom: TypeId) -> bool {
        match self.universe.atom_decl(atom) {
            Ok(decl) => match &decl.kind {
                ds_check::AtomKind::Apply(ctor, _) => self
                    .analyzed
                    .decls
                    .defs_of_atoms
                    .get(ctor)
                    .is_some_and(|def| self.analyzed.decls.enums.contains_key(def)),
                _ => false,
            },
            Err(_) => false,
        }
    }

    /// The word span of a solved field type: the same vocabulary the
    /// written field types take, read over the type's own members. A
    /// struct-typed field carries its whole flattened span, a union its
    /// own tag-plus-region span, and a tuple its elements' whole
    /// spans.
    fn ty_field_span(&mut self, ty: Ty, depth: usize) -> Result<Option<Vec<SlotType>>, LowerError> {
        const SPAN_DEPTH_LIMIT: usize = 32;
        if depth >= SPAN_DEPTH_LIMIT {
            return Err(unsupported(
                "a struct payload field",
                "the field's inline span nests past the depth budget; a cyclic field type has no \
                 finite span",
            ));
        }
        let members = self.universe.members(ty).to_vec();
        match members.as_slice() {
            [] => Ok(Some(vec![SlotType::I64])),
            [only] => self.atom_field_span(*only, depth),
            _ => {
                let shape = self.members_shape(&members, depth + 1)?;
                Ok(Some(shape_words(&shape)))
            },
        }
    }

    /// The fields of one solved application signature as member fields
    /// at consecutive words, the walk a nominal struct's declared
    /// fields takes. A part-less application keeps the reserved word
    /// the part-less struct carries.
    fn solved_struct_fields(
        &mut self,
        fields: &[ds_check::decls::SolvedField],
    ) -> Result<Vec<MemberField>, LowerError> {
        if fields.is_empty() {
            return Ok(vec![MemberField {
                name: None,
                slots: vec![SlotType::I64],
                word: 0,
            }]);
        }
        let mut laid = Vec::with_capacity(fields.len());
        let mut cursor = 0usize;
        for field in fields {
            let Some(slots) = self.ty_field_span(field.ty, 0)? else {
                return Err(unsupported(
                    "an application value's field",
                    "the field's shape stays outside the inline-span vocabulary",
                ));
            };
            let word = word_of(cursor)?;
            cursor += slots.len();
            laid.push(MemberField {
                name: Some(field.name.clone()),
                slots,
                word,
            });
        }
        Ok(laid)
    }

    /// The case atom the active instantiation minted for the checked
    /// record's own: a generic body's arm and construction records name
    /// the once-check's case atom, the instance's union shape its own
    /// mint. `None` keeps the record's atom (no active substitution, or
    /// a bare case's shared unit atom).
    pub(crate) fn substituted_case_atom(&self, atom: TypeId) -> Option<TypeId> {
        let active = self.active.as_ref()?;
        self.case_mints.get(&(active.id, atom)).copied()
    }

    /// The case atoms of one enum application, in the enum's declaration
    /// order — the bare cases' shared unit atoms and the per-application
    /// payloaded case atoms the checker (or the lens) minted. No record
    /// answers the application whose derivation refused: a payload
    /// that never solved, or a body naming a variable where the
    /// checker recorded none.
    fn enum_case_atoms(&self, atom: TypeId) -> Result<Vec<TypeId>, LowerError> {
        let Some(cases) = self.apply_cases_of(atom) else {
            return Err(unsupported(
                "an enum application value",
                "its case atoms never minted: a payload that never solved under the application's \
                 arguments carries no value shape",
            ));
        };
        Ok(cases.iter().map(|case| case.atom).collect())
    }

    /// The module-wide tag id of one atom, assigned in first-use order
    /// while functions lower in declaration order — except the
    /// predeclared `none`, whose discriminant is the platform's own
    /// (never a module-local id, never a table row): an optional word
    /// crossing a module boundary carries the same immediate on both
    /// sides with nothing to seed. The counter is
    /// checked against the platform's reserved top: a module with more
    /// distinct tag atoms than the space below it leaves refuses with a
    /// typed error instead of wrapping into the reserved range.
    pub(crate) fn tag_id(&mut self, atom: TypeId) -> Result<u8, LowerError> {
        if let Some(tag) = self.tags.get(&atom) {
            return Ok(*tag);
        }
        if atom == TypeId::NONE {
            return Ok(crate::native::NONE_TAG);
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

    /// Seeds the tag table from the served splice's rows: the spliced
    /// bodies carry the ambient's minted ids as code immediates, so the
    /// merged module must own those ids before any body lowers here,
    /// and the ids must be the ones the fresh lowering's ambient-first
    /// walk assigns. The rows must be dense from zero, below the
    /// reserved range, and name each atom at most once. A row's label
    /// need not resolve to an atom of this package — the ambient's own
    /// structural atoms mint only in the body walk the splice skips —
    /// so an unresolved label rides the table verbatim instead of
    /// refusing; the served bytes' identity pin and the equivalence
    /// gates hold the row set against the fresh walk.
    pub(crate) fn seed_served(&mut self, rows: &[ds_mod::MemberTag]) -> Result<(), LowerError> {
        for (at, row) in rows.iter().enumerate() {
            let drift = |what: &str| LowerError::ServedDrift {
                detail: format!(
                    "the served tag row {at} is `{}` (id {}): {what}",
                    row.name, row.id
                ),
            };
            if usize::from(row.id) != at {
                return Err(drift("the ids are not dense from zero"));
            }
            if row.id >= FIRST_RESERVED_TAG {
                return Err(drift("the id reaches the platform's reserved range"));
            }
            if let Some(atom) = self.universe.atom_by_name(&row.name) {
                if self.tags.contains_key(&atom) {
                    return Err(drift("the row names an atom twice"));
                }
                self.tags.insert(atom, row.id);
            } else {
                if self.unkeyed_rows.iter().any(|row2| row2.name == row.name) {
                    return Err(drift("the row names an atom twice"));
                }
                self.unkeyed_rows.push(row.clone());
            }
            self.next_tag = row.id + 1;
        }
        Ok(())
    }

    /// The tag table's rows, one per minted atom in id order (ids are
    /// dense from zero in first-use order, so sorting by id reconstructs
    /// the assignment order). Each row carries the declaring
    /// declaration's name beside the label: two files may declare one
    /// label (distinct atoms, distinct ids), so the rows key by atom —
    /// a name-keyed fold would drop one row at random — and a
    /// defless structural atom (a body check's minted application)
    /// carries an empty declaring name.
    pub(crate) fn tag_rows(&self) -> Vec<ds_mod::MemberTag> {
        let mut rows: Vec<ds_mod::MemberTag> = self
            .tags
            .iter()
            .map(|(atom, id)| ds_mod::MemberTag {
                decl: self
                    .analyzed
                    .decls
                    .defs_of_atoms
                    .get(atom)
                    .map(|def| self.analyzed.package.def(*def).name.clone())
                    .unwrap_or_default(),
                name: self
                    .analyzed
                    .decls
                    .universe
                    .atom_decl(*atom)
                    .map_or_else(|_| String::new(), |decl| decl.name.clone()),
                id: *id,
            })
            .collect();
        rows.extend(self.unkeyed_rows.iter().cloned());
        rows.sort_unstable_by_key(|row| row.id);
        rows
    }

    /// Seeds the tag table from the linked modules' rows (the shared tag
    /// mapping): a union-typed word crossing a module boundary carries
    /// the producing module's tag ids as immediates, so this compile's
    /// own table must reserve each row's id for the atom the linked
    /// facts mint under the same name before any body lowers here. A
    /// row keys an atom only when that atom is this compile's own
    /// linked-fact mint (the name index names the def the install
    /// minted) — every other row drops: a name this compile resolves to
    /// its own local atom cannot carry a crossing value (the linked
    /// signature naming it refused at the walk), and a name resolving
    /// to nothing here (the producer's private atoms) crosses the same
    /// way never. Rows may repeat (two imports of one package); an
    /// exact repeat is a no-op. A row whose id another name already
    /// owns refuses: two linked packages disagreeing on one id is a
    /// cross-wire no remap can fix (their code immediates are frozen in
    /// their artifacts), the shared mapping round's own refusal.
    pub(crate) fn seed_linked(&mut self, rows: &[ds_mod::MemberTag]) -> Result<(), LowerError> {
        for row in rows {
            let name = &row.name;
            let id = row.id;
            let refuse = |reason: String| LowerError::LinkedTagRow {
                name: name.clone(),
                id,
                reason,
            };
            if id >= FIRST_RESERVED_TAG {
                return Err(refuse(
                    "the id reaches the platform's reserved range".to_owned(),
                ));
            }
            // the linked-fact mint this name answers, when it is one
            let Some(atom) = self
                .analyzed
                .decls
                .foreign_type(name)
                .and_then(|def| self.analyzed.decls.atoms.get(&def).copied())
                .filter(|&atom| self.universe.atom_by_name(name) == Some(atom))
            else {
                continue;
            };
            match self.tags.get(&atom) {
                Some(&held) if held == id => {},
                Some(&held) => {
                    return Err(refuse(format!(
                        "the linked mint already holds id {held} — a linked module re-keyed its \
                         own row"
                    )));
                },
                None => {
                    if let Some(held) = self.id_owner(id) {
                        return Err(refuse(format!(
                            "`{held}` already owns the id — two linked modules disagree on the id \
                             space, and their code immediates are frozen in their artifacts"
                        )));
                    }
                    self.tags.insert(atom, id);
                    self.next_tag = self.next_tag.max(id + 1);
                },
            }
        }
        Ok(())
    }

    /// The label owning one tag id already: a keyed atom's declaration
    /// label or an unkeyed row's label.
    fn id_owner(&self, id: u8) -> Option<String> {
        for (&atom, &held) in &self.tags {
            if held == id
                && let Ok(decl) = self.universe.atom_decl(atom)
            {
                return Some(decl.name.clone());
            }
        }
        for row in &self.unkeyed_rows {
            if row.id == id {
                return Some(row.name.clone());
            }
        }
        None
    }

    /// A union member that declares a deinit refuses: the union's
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
    /// (`$` sits outside the identifier charset). Outside an
    /// instantiation its values ride one Raw word — the runtime kind is
    /// the caller's fact, checked by the VM at every hand-off, never a
    /// static one; under one, the lens substituted it away.
    pub(crate) fn is_var_atom(&self, atom: TypeId) -> bool {
        is_var_atom(self.universe, atom)
    }

    /// The registry as this lowering reads it.
    pub(crate) fn universe(&self) -> &'a Universe {
        self.universe
    }

    /// Lowers the bodies to come under one instantiation's substitution
    /// (`None` returns to the generic view).
    pub(crate) fn enter(&mut self, subst: Option<Rc<Subst>>) {
        self.active = subst;
    }

    /// The type one recorded type reads as under the active instantiation:
    /// itself outside one; under one, the type the discovery substituted
    /// it to. A type naming a substituted variable that the discovery
    /// never resolved refuses here — the walk lowers no variable's word
    /// for an instance.
    pub(crate) fn subst(&self, ty: Ty) -> Result<Ty, LowerError> {
        let Some(active) = &self.active else {
            return Ok(ty);
        };
        if let Some(substituted) = self.minted.get(&(active.id, ty)) {
            return Ok(*substituted);
        }
        if active.mentions(self.universe, ty) {
            return Err(unsupported(
                "an instantiated body's type",
                "it still names a type variable the plan's discovery never substituted; report \
                 the site",
            ));
        }
        Ok(ty)
    }

    /// The key one recorded type takes under the active instantiation —
    /// the instance table's sharing key, computed on a throwaway probe so
    /// the union members mint no tag ids into any live lowering. A type
    /// still naming a type variable refuses: the discovery substituted
    /// every recorded type of the body, so one reaching here skipped the
    /// substitution.
    pub(crate) fn type_key(&mut self, ty: Ty) -> Result<ShapeKey, LowerError> {
        if crate::types::has_variables(self.universe, ty) {
            return Err(unsupported(
                "an instance key",
                "the type still names a type variable the instantiation does not substitute; \
                 report the construct",
            ));
        }
        match self.universe.members(ty) {
            // the empty type never flows; it keys as its one inert word
            [] => return Ok(ShapeKey::Word(WordKey::Slot(SlotType::I64))),
            [only] => return self.atom_key(*only),
            _ => {},
        }
        let Shape::Union(union) = &*self.shape(ty)? else {
            return Err(unsupported(
                "an instance key",
                "a multi-member type the shape layer did not lay out as a union",
            ));
        };
        Ok(union_key(union))
    }

    /// Whether one nominal atom or application's constructor declares a
    /// deinit.
    fn atom_declares_deinit(&self, atom: TypeId) -> bool {
        let def = self
            .analyzed
            .decls
            .defs_of_atoms
            .get(&atom)
            .copied()
            .or_else(
                || match self.universe.atom_decl(atom).ok().map(|decl| &decl.kind) {
                    Some(ds_check::AtomKind::Apply(ctor, _)) => {
                        self.analyzed.decls.defs_of_atoms.get(ctor).copied()
                    },
                    _ => None,
                },
            );
        def.is_some_and(|def| self.declared_deinits.contains(&def))
    }

    /// The key one atom takes, recursively including every tuple element
    /// whose span or nested tags affect the instantiated body.
    fn atom_key(&mut self, atom: TypeId) -> Result<ShapeKey, LowerError> {
        // A deinit-carrying atom keys by its own identity: the hook each
        // body claims is the struct's own, including through an application.
        if self.atom_declares_deinit(atom) {
            return Ok(ShapeKey::DeinitStruct(atom));
        }
        let shape = self.atom_shape(atom)?;
        match &*shape {
            Shape::Word(slot) => self.word_key(atom, *slot).map(ShapeKey::Word),
            Shape::Struct(s) => Ok(ShapeKey::Struct(
                s.fields.iter().map(|field| field.slots.clone()).collect(),
            )),
            Shape::Tuple(s) => {
                let Ok(ds_check::AtomKind::Tuple(elements)) =
                    self.universe.atom_decl(atom).map(|decl| decl.kind.clone())
                else {
                    return Err(unsupported("an instance key", "a tuple type's atom"));
                };
                elements
                    .iter()
                    .zip(s.fields.iter())
                    .map(|(&element, field)| {
                        let element_shape = self.atom_shape(element)?;
                        if matches!(&*element_shape, Shape::Word(_)) {
                            // Keep the established one-word erasures,
                            // including a fn element's heap Ref slot.
                            self.word_key(element, field.slot()).map(ShapeKey::Word)
                        } else {
                            self.atom_key(element)
                        }
                    })
                    .collect::<Result<Vec<_>, _>>()
                    .map(ShapeKey::Tuple)
            },
            Shape::Union(u) => Ok(union_key(u)),
        }
    }

    /// The key one word takes: the erasures the instance table keys by.
    fn word_key(&self, atom: TypeId, slot: SlotType) -> Result<WordKey, LowerError> {
        if self.is_var_atom(atom) {
            return Err(unsupported(
                "an instance key",
                "the word's type still names a type variable the instantiation does not \
                 substitute; report the construct",
            ));
        }
        if slot == SlotType::Tag {
            return Ok(WordKey::Atom(atom));
        }
        if slot == SlotType::Ref {
            return Ok(match self.atom_builtin_instance(atom) {
                Some(instance) => WordKey::Heap(instance),
                None => WordKey::Atom(atom),
            });
        }
        Ok(WordKey::Slot(slot))
    }

    /// The shape every type of one generic body lowers under — the
    /// sequence of [`ShapeKey`] entries in the body's fixed type order,
    /// rendered as one deterministic string so an instance's table name
    /// (its digest) holds across compiles while the shape holds. Atoms
    /// render by declared name: two atoms sharing a name share a digest,
    /// and the collision takes an ordinal.
    pub(crate) fn render_key(&self, key: &[ShapeKey]) -> String {
        let mut out = String::from("shape-key:");
        for ty in key {
            out.push('|');
            self.render_ty(ty, &mut out);
        }
        out
    }

    fn render_ty(&self, key: &ShapeKey, out: &mut String) {
        match key {
            ShapeKey::Word(word) => match word {
                WordKey::Slot(slot) => write!(out, "w:{slot:?}").expect("a String write"),
                WordKey::Heap(instance) => write!(out, "h:{instance:?}").expect("a String write"),
                WordKey::Atom(atom) => self.render_atom(*atom, out),
            },
            ShapeKey::Struct(fields) => {
                out.push_str("struct(");
                for (at, field) in fields.iter().enumerate() {
                    if at > 0 {
                        out.push(',');
                    }
                    out.push('[');
                    for slot in field {
                        write!(out, "{slot:?};").expect("a String write");
                    }
                    out.push(']');
                }
                out.push(')');
            },
            ShapeKey::DeinitStruct(atom) => {
                out.push_str("deinit(");
                self.render_atom(*atom, out);
                out.push(')');
            },
            ShapeKey::Tuple(elements) => {
                out.push_str("tuple(");
                for (at, element) in elements.iter().enumerate() {
                    if at > 0 {
                        out.push(',');
                    }
                    self.render_ty(element, out);
                }
                out.push(')');
            },
            ShapeKey::Union(members) => {
                out.push_str("union(");
                for (at, (atom, fields)) in members.iter().enumerate() {
                    if at > 0 {
                        out.push(',');
                    }
                    self.render_atom(*atom, out);
                    out.push('{');
                    for (word, span) in fields {
                        write!(out, "{word}:").expect("a String write");
                        for slot in span {
                            write!(out, "{slot:?};").expect("a String write");
                        }
                    }
                    out.push('}');
                }
                out.push(')');
            },
        }
    }

    fn render_atom(&self, atom: TypeId, out: &mut String) {
        let name = self
            .universe
            .atom_decl(atom)
            .map_or_else(|_| "?".to_owned(), |decl| decl.name.clone());
        write!(out, "a:{name}").expect("a String write");
    }

    /// The shape of one solved type, memoized.
    pub(crate) fn shape(&mut self, ty: Ty) -> Result<Rc<Shape>, LowerError> {
        if let Some(shape) = self.cache.get(&ty) {
            return Ok(shape.clone());
        }
        // The member set is the analysis's own: reading it through the
        // analyzed reference keeps it borrowed across the shape walk below.
        let members = self.universe.members(ty);
        let shape = self.members_shape(members, 0)?;
        let shape = Rc::new(shape);
        self.cache.insert(ty, shape.clone());
        Ok(shape)
    }

    /// The word types of one solved type's span, cached per type.
    pub(crate) fn words(&mut self, ty: Ty) -> Result<SpanWords, LowerError> {
        if let Some(words) = self.word_cache.get(&ty) {
            return Ok(words.clone());
        }
        let words: SpanWords = shape_words(&*self.shape(ty)?).into();
        self.word_cache.insert(ty, words.clone());
        Ok(words)
    }

    /// The word types of one solved member list's span — the member-slice
    /// form of [`words`](Self::words), for substituted types the checker
    /// solved without minting a `Ty` the lowering side can name (the
    /// runtime-evidence chain's per-link substitutions). Cached by the
    /// list: a chain re-substitutes the same lists at every link, and the
    /// shape walk allocates per member.
    pub(crate) fn member_words(&mut self, members: &[TypeId]) -> Result<SpanWords, LowerError> {
        if let Some(words) = self.member_cache.get(members) {
            return Ok(words.clone());
        }
        let shape = self.members_shape(members, 0)?;
        let words: SpanWords = shape_words(&shape).into();
        self.member_cache.insert(members.into(), words.clone());
        Ok(words)
    }

    /// The shape of one member list: the shared body of
    /// [`shape`](Self::shape) and [`member_words`](Self::member_words),
    /// and the union field's span walk's own entry. `depth` threads the
    /// field-span budget through a union-typed field's members, so a
    /// struct nesting through a union back into itself refuses typed at
    /// the budget instead of recursing.
    pub(crate) fn members_shape(
        &mut self,
        members: &[TypeId],
        depth: usize,
    ) -> Result<Shape, LowerError> {
        Ok(match members {
            [] => Shape::Word(SlotType::I64), // the empty type; never flows
            [only] => (*self.atom_shape(*only)?).clone(),
            _ => self.union_shape(members, depth)?,
        })
    }

    /// A union span: the tag word plus the shared payload region, member
    /// fields claiming the lowest free region span per the module-doc
    /// strategy. A member's `claimed` set scopes to that member alone:
    /// members share region words by kind, one member's fields never
    /// share a word among themselves.
    fn union_shape(&mut self, members: &[TypeId], depth: usize) -> Result<Shape, LowerError> {
        let mut region: Vec<SlotType> = Vec::new();
        let mut shapes = Vec::with_capacity(members.len());
        for atom in members {
            let tag = self.tag_id(*atom)?;
            self.guard_deinit_member(*atom)?;
            let payload = self.payload_fields(*atom, depth)?;
            let mut claimed = vec![false; region.len()];
            let mut fields = Vec::with_capacity(payload.len());
            for field in payload {
                let word = claim_span(&mut region, &mut claimed, &field.slots)?;
                fields.push(MemberField {
                    name: field.name,
                    slots: field.slots,
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
    /// span, or the arity-0 tuple's unit word — the cached handle, the
    /// way `shape` and `words` share theirs, so a walk reading one atom
    /// per element never copies the shape.
    pub(crate) fn atom_shape(&mut self, atom: TypeId) -> Result<Rc<Shape>, LowerError> {
        if let Some(shape) = self.atom_cache.get(&atom) {
            return Ok(shape.clone());
        }
        let shape = Rc::new(self.atom_shape_uncached(atom)?);
        self.atom_cache.insert(atom, shape.clone());
        Ok(shape)
    }

    fn atom_shape_uncached(&mut self, atom: TypeId) -> Result<Shape, LowerError> {
        let universe = &self.universe;
        let Ok(decl) = universe.atom_decl(atom) else {
            return Err(unsupported("a value", "its atom has no declaration"));
        };
        match &decl.kind {
            // a type variable's value is one Raw word: the runtime kind
            // is the caller's fact (the VM checks it at every hand-off).
            // Under an instantiation the lens substituted every variable
            // away, so one reaching here skipped it.
            ds_check::AtomKind::Unit if self.is_var_atom(atom) => {
                if self.active.is_some() {
                    return Err(unsupported(
                        "an instantiated body's value",
                        "its type still names a type variable the instantiation does not \
                         substitute",
                    ));
                }
                Ok(Shape::Word(SlotType::Raw))
            },
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
                let mut cursor = 0usize;
                for element in elements {
                    if self.atom_declares_deinit(*element) {
                        return Err(unsupported(
                            "a deinit-carrying tuple element",
                            "the tuple has no per-element deinit claim; per-span deinit claims \
                             own the seam",
                        ));
                    }
                    let callable = matches!(
                        self.universe.atom_decl(*element).map(|decl| &decl.kind),
                        Ok(ds_check::AtomKind::FnSig(..))
                    );
                    let shape = self.atom_shape(*element)?;
                    let mut slots = shape_words(&shape);
                    // A fn-typed element holds the closure Ref word: the
                    // heap position the span's count traffic covers.
                    if callable {
                        slots[0] = SlotType::Ref;
                    }
                    let word = word_of(cursor)?;
                    cursor += slots.len();
                    fields.push(MemberField {
                        name: None,
                        slots,
                        word,
                    });
                }
                Ok(Shape::Tuple(StructShape { fields }))
            },
            // a heap-reference application: one Ref word, the object
            // behind it owned by the runtime (the list object)
            ds_check::AtomKind::Apply(ctor, _) if self.heap_ref(*ctor) => {
                Ok(Shape::Word(SlotType::Ref))
            },
            // A user-declared generic struct's application: the solved
            // field signature over the application's arguments, laid out
            // as a bare struct — the value shape the sites construct,
            // read, and stage. A field nesting through the application
            // itself has no finite span; the depth budget refuses it
            // typed.
            ds_check::AtomKind::Apply(ctor, _) if !self.is_enum_application(atom) => {
                let Some(fields) = self.apply_fields_of(atom) else {
                    return Err(unsupported(
                        "an application value",
                        "its constructor declares no fields this lowering solved; a generic \
                         body's own variable or an unsolved field type carries no value shape here",
                    ));
                };
                let fields = self.solved_struct_fields(&fields)?;
                Ok(Shape::Struct(StructShape { fields }))
            },
            // A user-declared generic enum's application: a tagged union
            // over its own case atoms — the bare cases' shared unit
            // atoms and the per-application payloaded case atoms, each
            // claiming the shared payload region. A single-case
            // application lays out as its case's own shape, exactly the
            // singleton union the non-generic spelling names.
            ds_check::AtomKind::Apply(..) => {
                let members = self.enum_case_atoms(atom)?;
                let shape = self.members_shape(&members, 0)?;
                Ok(shape)
            },
            // A fn value is one Fn word:
            // a non-capturing lambda mints its copyable scalar FuncRef
            // into the slot; a capturing lambda's VALUE is a Ref word
            // naming the closure object, flowing through value-site
            // places and callable-defaulted fn-typed parameter slots.
            ds_check::AtomKind::FnSig(..) => Ok(Shape::Word(SlotType::Fn)),
            _ => Err(unsupported(
                "an atom with no resolved shape",
                "the runtime contract lowers unit, primitive, struct, tuple, and application \
                 atoms; this atom's own shape never resolved (a generic enum's opaque head, a \
                 case payload whose fields never solved), and its values wait on their own seams",
            )),
        }
    }

    /// One atom's payload: the member's fields as word-type spans. Unit
    /// tags carry no payload; primitives carry their own word; struct
    /// members carry their declared fields, a struct-typed field carrying
    /// its whole flattened span; a tuple member carries one positional
    /// span per element. A field type the inline-span vocabulary cannot
    /// spell refuses.
    fn payload_fields(
        &mut self,
        atom: TypeId,
        depth: usize,
    ) -> Result<Vec<PayloadField>, LowerError> {
        let universe = &self.universe;
        let Ok(decl) = universe.atom_decl(atom) else {
            return Err(unsupported("a value", "its atom has no declaration"));
        };
        match &decl.kind {
            // a union member over a type variable has no static payload
            // shape: the member's runtime word kind is the caller's fact,
            // and a checked read would need runtime reflection to re-span
            // (under an instantiation the lens replaced the member, so
            // this is a generic body's own union)
            ds_check::AtomKind::Unit if self.is_var_atom(atom) => Err(unsupported(
                "a union member over a type variable",
                "the variable's runtime word kind is a runtime fact; a runtime reflection table \
                 is the seam",
            )),
            ds_check::AtomKind::Unit => match primitive_slot(&decl.name) {
                Some(slot) => Ok(vec![PayloadField::word(None, slot)]),
                None => Ok(Vec::new()),
            },
            // A part-less struct's payload is its one reserved word:
            // an unnamed I64 word holding the
            // frame's inert zero, never written and never read, so the
            // shape's every value position spans at least one word.
            ds_check::AtomKind::Struct(fields) if fields.is_empty() => Ok(vec![reserved_word()]),
            // Positional slots (tuple-payload enum cases) keep
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
                        "struct fields lower as inline word spans of primitive, unit, struct, \
                         tuple, union, or heap-application type; this field's shape stays outside \
                         the inline-span vocabulary",
                    )),
                })
                .collect(),
            // A tuple member's positional slots claim the region like a
            // struct member's fields — one span per element, no word
            // shared among them; the gather walks them positionally. A
            // struct, nested tuple, or generic-application element claims
            // its whole span as one positional claim.
            ds_check::AtomKind::Tuple(elements) => {
                let mut out = Vec::with_capacity(elements.len());
                for element in elements {
                    if self.atom_declares_deinit(*element) {
                        return Err(unsupported(
                            "a deinit-carrying tuple element",
                            "the tuple has no per-element deinit claim; per-span deinit claims \
                             own the seam",
                        ));
                    }
                    let shape = self.atom_shape(*element)?;
                    out.push(PayloadField {
                        name: None,
                        slots: shape_words(&shape),
                    });
                }
                Ok(out)
            },
            // A callable member's payload is the one Fn word: the
            // checked read hands the element word out under the element's
            // own classification — a closure element refuses at the
            // construction guard, so the word reaching a payload slot is
            // the scalar FuncRef.
            ds_check::AtomKind::FnSig(..) => Ok(vec![PayloadField::word(None, SlotType::Fn)]),
            // A heap-reference application member (a list, a map): the
            // same one Ref word `atom_shape` rules the bare value — the
            // object behind it owned by the runtime — claimed
            // positionally exactly like a tuple member's element word
            ds_check::AtomKind::Apply(ctor, _) if self.heap_ref(*ctor) => {
                Ok(vec![PayloadField::word(None, SlotType::Ref)])
            },
            // A user-declared generic struct's application as a union
            // member: its solved fields claim the region exactly like a
            // struct member's (the deinit guard `union_shape` runs
            // already resolved the constructor's own hook)
            ds_check::AtomKind::Apply(..) if !self.is_enum_application(atom) => {
                let Some(fields) = self.apply_fields_of(atom) else {
                    return Err(unsupported(
                        "an application member",
                        "its constructor declares no fields this lowering solved; a generic \
                         body's own variable or an unsolved field type carries no value shape here",
                    ));
                };
                let mut out = Vec::with_capacity(fields.len());
                for field in fields.iter() {
                    match self.ty_field_span(field.ty, depth)? {
                        Some(slots) if !slots.is_empty() => out.push(PayloadField {
                            name: Some(field.name.clone()),
                            slots,
                        }),
                        _ => {
                            return Err(unsupported(
                                "an application member's field",
                                "the field's shape stays outside the inline-span vocabulary",
                            ))
                        },
                    }
                }
                if out.is_empty() {
                    out.push(reserved_word());
                }
                Ok(out)
            },
            // A user-declared generic enum's application as a union
            // member: the application's own whole tag-plus-region span
            // claims the region as one positional claim, the same
            // whole-span rule a union-typed field carries
            ds_check::AtomKind::Apply(..) => {
                let members = self.enum_case_atoms(atom)?;
                let shape = self.union_shape(&members, depth + 1)?;
                Ok(vec![PayloadField {
                    name: None,
                    slots: shape_words(&shape),
                }])
            },
            _ => Err(unsupported(
                "an enum member",
                "a union member lowers as unit, primitive, struct, tuple, callable, or \
                 application words; anything else waits on the seam its own value shape names",
            )),
        }
    }

    /// The word span of a written field type: a primitive or unit one
    /// word, a type variable's Raw word, a struct's whole flattened
    /// inline span (its fields' spans concatenated), a tuple's elements'
    /// whole spans at consecutive words, a union's own tag-plus-region
    /// span, and a heap application's one Ref word. `None` is the shared refusal: any
    /// other shape stays outside the inline-span vocabulary. `depth`
    /// bounds the recursion — a cyclic field type has no finite span,
    /// and the budget refuses it typed instead of recursing.
    fn field_span(
        &mut self,
        expr: &TypeExpr,
        depth: usize,
    ) -> Result<Option<Vec<SlotType>>, LowerError> {
        const SPAN_DEPTH_LIMIT: usize = 32;
        if depth >= SPAN_DEPTH_LIMIT {
            return Err(unsupported(
                "a struct payload field",
                "the field's inline span nests past the depth budget; a cyclic field type has no \
                 finite span",
            ));
        }
        match expr {
            // A tuple field claims its elements' whole spans at
            // consecutive words, the same layout the bare tuple value
            // carries. A union element keeps its refusal: the product
            // distributes over the union, so the solved field type is a
            // union of tuple atoms and no inline tuple layout exists. The
            // empty tuple stays outside: a `()`-typed field would span
            // zero words.
            TypeExpr::Tuple(elements) => {
                let [first, rest @ ..] = &elements[..] else {
                    return Ok(None);
                };
                let mut slots = Vec::new();
                for element in std::iter::once(first).chain(rest) {
                    if matches!(element, TypeExpr::Union(..)) {
                        return Ok(None);
                    }
                    match self.field_span(element, depth + 1)? {
                        Some(element_slots) => slots.extend(element_slots),
                        None => return Ok(None),
                    }
                }
                Ok(Some(slots))
            },
            // the union's own span: tag word plus payload region, the
            // same layout the checked reads shape
            TypeExpr::Union(..) => {
                let Some(members) = expr_members(expr) else {
                    return Ok(None);
                };
                let shape = self.members_shape(&members, depth + 1)?;
                Ok(Some(shape_words(&shape)))
            },
            TypeExpr::Atom(id) => self.atom_field_span(*id, depth),
            _ => Ok(None),
        }
    }

    /// The word span of a nominal field type, the atom half of
    /// [`field_span`](Self::field_span).
    fn atom_field_span(
        &mut self,
        id: TypeId,
        depth: usize,
    ) -> Result<Option<Vec<SlotType>>, LowerError> {
        let universe = &self.universe;
        let Ok(decl) = universe.atom_decl(id) else {
            return Ok(None);
        };
        if self.is_var_atom(id) {
            return Ok(Some(vec![SlotType::Raw]));
        }
        if let Some(slot) = primitive_slot(&decl.name) {
            return Ok(Some(vec![slot]));
        }
        match &decl.kind {
            // a lone unit tag or case as a field type carries the
            // case's own Tag word, the same one `atom_shape` rules the
            // bare value, so the embedded word and the field's span
            // agree at the construction and every read
            ds_check::AtomKind::Unit => Ok(Some(vec![SlotType::Tag])),
            ds_check::AtomKind::Struct(_) => {
                if let Some(def) = self.analyzed.decls.defs_of_atoms.get(&id)
                    && self.declared_deinits.contains(def)
                {
                    return Err(unsupported(
                        "a struct payload field",
                        "a deinit-carrying field type has no claim of its own inside the inline \
                         span, so its hook would run at the wrong death; per-span deinit claims \
                         own the seam",
                    ));
                }
                let payload = self.payload_fields(id, depth + 1)?;
                Ok(Some(
                    payload.into_iter().flat_map(|field| field.slots).collect(),
                ))
            },
            // a heap-reference application (a list, a map): the one Ref
            // word `atom_shape` rules the bare value
            ds_check::AtomKind::Apply(ctor, _) if self.heap_ref(*ctor) => {
                Ok(Some(vec![SlotType::Ref]))
            },
            // a fn-typed field holds the closure Ref word: the heap
            // position's word, whose count the span's traffic covers — a
            // non-capturing fn entering wraps into its static closure
            // object at the construction
            ds_check::AtomKind::FnSig(..) => Ok(Some(vec![SlotType::Ref])),
            // a user-declared generic struct's application: its solved
            // fields' whole flattened span claims the field, the same
            // walk a nominal struct's field takes; a deinit-carrying
            // constructor keeps the refusal its own binding takes
            ds_check::AtomKind::Apply(ctor, _) if !self.is_enum_application(id) => {
                if let Some(def) = self.analyzed.decls.defs_of_atoms.get(ctor)
                    && self.declared_deinits.contains(def)
                {
                    return Err(unsupported(
                        "a struct payload field",
                        "a deinit-carrying field type has no claim of its own inside the inline \
                         span, so its hook would run at the wrong death; per-span deinit claims \
                         own the seam",
                    ));
                }
                let Some(fields) = self.apply_fields_of(id) else {
                    return Ok(None);
                };
                let mut slots = Vec::new();
                for field in fields.iter() {
                    match self.ty_field_span(field.ty, depth + 1)? {
                        Some(span) => slots.extend(span),
                        None => return Ok(None),
                    }
                }
                // a part-less application keeps the reserved word its
                // value shape carries, so the field's span and the
                // constructed value agree at the literal's store
                if slots.is_empty() {
                    slots.push(SlotType::I64);
                }
                Ok(Some(slots))
            },
            // a user-declared generic enum's application: its own whole
            // tag-plus-region span, the same union layout the value
            // shape lays out — the depth budget riding the span walk,
            // so a payload nesting through the application itself
            // refuses typed instead of recursing
            ds_check::AtomKind::Apply(..) => {
                let members = self.enum_case_atoms(id)?;
                let shape = self.union_shape(&members, depth + 1)?;
                Ok(Some(shape_words(&shape)))
            },
            // An aliased tuple field claims its elements' whole spans, the
            // same walk the written spelling takes — a struct element's
            // deinit refusal and the fn element's closure Ref word ride
            // their own arms; the empty tuple refuses on the same line.
            ds_check::AtomKind::Tuple(elements) => {
                let [first, rest @ ..] = &elements[..] else {
                    return Ok(None);
                };
                let mut slots = Vec::new();
                for element in std::iter::once(first).chain(rest) {
                    match self.atom_field_span(*element, depth + 1)? {
                        Some(element_slots) => slots.extend(element_slots),
                        None => return Ok(None),
                    }
                }
                Ok(Some(slots))
            },
            _ => Ok(None),
        }
    }

    /// The element type of one whole-list application: iteration
    /// and membership read it. `None` for any other type — a union of
    /// lists, a non-list application, a non-application.
    pub(crate) fn list_element(&self, ty: Ty) -> Option<Ty> {
        self.heap_arg(ty, "list", 0)
    }

    /// The payload type of one whole-channel application: the send's
    /// staging reads it (a bare member value re-spans into the payload
    /// type's whole span before the hand-off). `None` for any other
    /// type — a union of channels, a non-chan application, a
    /// non-application.
    pub(crate) fn chan_payload(&self, ty: Ty) -> Option<Ty> {
        self.heap_arg(ty, "chan", 0)
    }

    /// The element type of one whole-arena application: the insert's
    /// staging reads it (a bare member value re-spanning into the
    /// element type before the store, and the one-word policy checking
    /// the staged width). `None` for any other type — a union of
    /// arenas, a non-arena application, a non-application.
    pub(crate) fn arena_element(&self, ty: Ty) -> Option<Ty> {
        self.heap_arg(ty, "arena", 0)
    }

    /// The element type of one whole-box application: the construction
    /// and the store's staging read it (the held value's whole word
    /// span). `None` for any other type — a union of boxes, a non-box
    /// application, a non-application.
    pub(crate) fn box_element(&self, ty: Ty) -> Option<Ty> {
        self.heap_arg(ty, "box", 0)
    }

    /// Whether one declared atom is a whole-box application: the
    /// equality plan's refusal reads it (the container's word has no
    /// content compare of its own).
    pub(crate) fn atom_is_box(&self, atom: TypeId) -> bool {
        let universe = &self.universe;
        let Ok(decl) = universe.atom_decl(atom) else {
            return false;
        };
        matches!(
            &decl.kind,
            ds_check::AtomKind::Apply(ctor, _)
                if universe.atom_decl(*ctor).is_ok_and(|ctor| {
                    ds_check::import::heap_ref(&ctor.name).is_some_and(|row| row.name == "box")
                })
        )
    }

    /// The value type of one whole-map application: the map value
    /// channel's staging reads it (a bare member value re-spans into the
    /// value type's whole span before the store). `None` for any other
    /// type — a union of maps, a non-map application, a non-application.
    pub(crate) fn map_value(&self, ty: Ty) -> Option<Ty> {
        self.heap_arg(ty, "map", 1)
    }

    /// One whole-container application's argument type at `position` —
    /// the one walk the list element, the map value, and the channel
    /// payload reads share, so the container names and arities live in
    /// the shared heap-reference word table alone. `None` for any other
    /// type — a union of containers, a container of the wrong name, a
    /// non-container application, a non-application.
    fn heap_arg(&self, ty: Ty, name: &str, position: usize) -> Option<Ty> {
        let universe = &self.universe;
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
        let row = ds_check::import::heap_ref(&universe.atom_decl(*ctor).ok()?.name)?;
        if row.name != name || args.len() != row.args {
            return None;
        }
        args.get(position).copied()
    }

    /// The builtin instance of a generic call's receiver type: the
    /// statically-resolved instance reference a CALLG site stages as
    /// its evidence words. The covered vocabulary is the builtin
    /// instance set — `string`, `bytes`, the `list`/`map` container
    /// constructors, the `int`/`float` scalars, and the record instance
    /// a struct value names (the element-taking closes' wider head) — a
    /// single-atom type check, so unions and unresolved variables
    /// answer None and the call site reports the uncovered receiver.
    pub(crate) fn builtin_instance(&self, ty: Ty) -> Option<BuiltinInstance> {
        let [only] = self.universe.members(ty) else {
            return None
        };
        self.atom_builtin_instance(*only)
    }

    /// Whether an atom is a heap-reference constructor: a row of the
    /// shared heap-reference word table.
    fn heap_ref(&self, ctor: TypeId) -> bool {
        self.analyzed
            .decls
            .universe
            .atom_decl(ctor)
            .is_ok_and(|decl| ds_check::import::heap_ref(&decl.name).is_some())
    }

    /// The builtin instance one declared atom names — the one vocabulary
    /// the by-type walk ([`Shapes::builtin_instance`], which keys on
    /// solved types) and the atom-keyed plan walk share, so a new
    /// builtin instance lands in both keyings in one edit: the
    /// heap-reference containers, the record instance a struct value
    /// names, and the builtin-instance scalars the shared primitive
    /// word table's rows spell (`string`/`bytes`/`int`/`float` — `char`
    /// and the widths ride plain scalar words, no collection content
    /// compare behind them).
    pub(crate) fn atom_builtin_instance(&self, atom: TypeId) -> Option<BuiltinInstance> {
        let universe = &self.universe;
        let decl = universe.atom_decl(atom).ok()?;
        match &decl.kind {
            ds_check::AtomKind::Apply(ctor, _) if self.heap_ref(*ctor) => {
                match universe.atom_decl(*ctor).ok()?.name.as_str() {
                    "list" => Some(BuiltinInstance::List),
                    "map" => Some(BuiltinInstance::Map),
                    "arena" => Some(BuiltinInstance::Arena),
                    _ => None,
                }
            },
            ds_check::AtomKind::Struct(_) => Some(BuiltinInstance::Record),
            _ => match ds_check::import::primitive_word(&decl.name)? {
                ds_check::import::PrimitiveWord::Scalar(ds_check::import::ScalarWord::Str) => {
                    Some(BuiltinInstance::Str)
                },
                ds_check::import::PrimitiveWord::Bytes => Some(BuiltinInstance::Bytes),
                ds_check::import::PrimitiveWord::Scalar(ds_check::import::ScalarWord::Int) => {
                    Some(BuiltinInstance::Int)
                },
                ds_check::import::PrimitiveWord::Scalar(ds_check::import::ScalarWord::Float) => {
                    Some(BuiltinInstance::Float)
                },
                _ => None,
            },
        }
    }

    /// The argument types of one atom's generic application, if it is
    /// one: the list's element type, the map's key and value types.
    pub(crate) fn apply_args(&self, atom: TypeId) -> Option<&[Ty]> {
        match &self.universe.atom_decl(atom).ok()?.kind {
            ds_check::AtomKind::Apply(_, args) => Some(args),
            _ => None,
        }
    }

    /// The member entry a single-atom source writes into a union: the
    /// direct entry when the union carries the atom itself, else the
    /// one member the source matches under a consistent variable
    /// binding. A written generic head keys its members by the
    /// variable's atoms (`int | list[$t]`, `int | ($t, $t)`) while the
    /// entering site carries the substituted concrete value, so the
    /// lookup matches structurally: a variable binds (consistently —
    /// one variable, one binding), a heap application matches by
    /// constructor alone (its one Ref word is argument-independent),
    /// any other application by constructor with recursively matched
    /// arguments, a tuple by arity with recursively matched elements,
    /// and every other kind by identity. The member's region claims a
    /// Raw word wherever a variable sat, and Raw accepts any runtime
    /// kind, so the member's tag names the substituted value
    /// truthfully whatever the site's substitution; the claimed word
    /// COUNT is structural (one word per tuple element, one Ref word
    /// per heap application), so the pairing against the source's own
    /// words stays exact — never a runtime width guess. No matching
    /// member, or two matching members (`(t, t) | (t, int)` at an
    /// `(int, int)` site), answers None — the honest refusal.
    pub(crate) fn member_for<'b>(
        &self,
        u: &'b UnionShape,
        atom: TypeId,
    ) -> Option<&'b MemberShape> {
        if let Some(member) = u.member(atom) {
            return Some(member);
        }
        let mut matched = u.members.iter().filter(|member| {
            let mut bindings = Vec::new();
            self.atom_under(member.atom, atom, &mut bindings)
                || self.applied_member_of(member.atom, atom)
        });
        let only = matched.next()?;
        matched.next().is_none().then_some(only)
    }

    /// Whether a member's atom is an application of `head`'s own atom
    /// while the head names a user-declared generic struct: the
    /// ctor-head arm of the structural matcher. Two applications of the
    /// one constructor both answer true, so the caller's single-match
    /// rule refuses the ambiguous arm.
    fn applied_member_of(&self, member: TypeId, head: TypeId) -> bool {
        let Some(&def) = self.analyzed.decls.defs_of_atoms.get(&head) else {
            return false;
        };
        let Some(shape) = self.analyzed.decls.structs.get(&def) else {
            return false;
        };
        if shape.params.is_empty() {
            return false;
        }
        matches!(
            self.universe.atom_decl(member).ok().map(|d| &d.kind),
            Some(ds_check::AtomKind::Apply(ctor, _)) if *ctor == head
        )
    }

    /// Whether `pattern` (a member's atom, its variables as binders)
    /// matches `value` (the entering site's atom) under one consistent
    /// binding: a variable atom binds anything, a heap-reference
    /// application matches by constructor alone, any other application
    /// by constructor with pairwise-matched arguments, a tuple by
    /// arity with pairwise-matched elements, and every other kind by
    /// identity. The binding lives for one candidate match alone —
    /// consistency is a property of that single match.
    fn atom_under(
        &self,
        pattern: TypeId,
        value: TypeId,
        bindings: &mut Vec<(TypeId, Vec<TypeId>)>,
    ) -> bool {
        if pattern == value {
            return true;
        }
        let universe = &self.universe;
        let (Ok(pdecl), Ok(vdecl)) = (universe.atom_decl(pattern), universe.atom_decl(value))
        else {
            return false;
        };
        if self.is_var_atom(pattern) {
            return bind(pattern, vec![value], bindings);
        }
        match (&pdecl.kind, &vdecl.kind) {
            // one Ref word whatever the argument, so the constructor
            // alone identifies the match
            (ds_check::AtomKind::Apply(pctor, _), ds_check::AtomKind::Apply(vctor, _))
                if pctor == vctor && self.heap_ref(*pctor) =>
            {
                true
            },
            (ds_check::AtomKind::Apply(pctor, pargs), ds_check::AtomKind::Apply(vctor, vargs))
                if pctor == vctor && pargs.len() == vargs.len() =>
            {
                pargs
                    .iter()
                    .zip(vargs.iter())
                    .all(|(p, v)| self.ty_under(*p, *v, bindings))
            },
            (ds_check::AtomKind::Tuple(pelems), ds_check::AtomKind::Tuple(velems))
                if pelems.len() == velems.len() =>
            {
                pelems
                    .iter()
                    .zip(velems.iter())
                    .all(|(p, v)| self.atom_under(*p, *v, bindings))
            },
            _ => false,
        }
    }

    /// One application argument's solved member set under the matcher:
    /// a single variable binds the whole set (its own word count is the
    /// runtime fact the Raw claim carries), a pair of single atoms
    /// delegates to [`atom_under`](Self::atom_under), and anything else
    /// matches by member-list identity — a compound pattern over a
    /// union argument never guesses a decomposition.
    fn ty_under(&self, pattern: Ty, value: Ty, bindings: &mut Vec<(TypeId, Vec<TypeId>)>) -> bool {
        let universe = &self.universe;
        let pmembers = universe.members(pattern);
        let vmembers = universe.members(value);
        match (pmembers, vmembers) {
            ([p], _) if self.is_var_atom(*p) => bind(*p, vmembers.to_vec(), bindings),
            ([p], [v]) => self.atom_under(*p, *v, bindings),
            _ => pmembers == vmembers,
        }
    }
}

/// Binds one variable to a member list under the matcher's consistency
/// rule: a repeated variable agrees with its earlier binding or the
/// candidate match fails.
fn bind(var: TypeId, bound: Vec<TypeId>, bindings: &mut Vec<(TypeId, Vec<TypeId>)>) -> bool {
    if let Some((_, existing)) = bindings.iter_mut().find(|(v, _)| v == &var) {
        *existing == bound
    } else {
        bindings.push((var, bound));
        true
    }
}

/// Whether an atom is one of the checker's minted type variables: a
/// generic parameter of an impl, function, or alias, named `$...` (`$`
/// sits outside the identifier charset).
pub(crate) fn is_var_atom(universe: &Universe, atom: TypeId) -> bool {
    universe
        .atom_decl(atom)
        .is_ok_and(|decl| decl.name.starts_with('$'))
}

/// The generic parameter name behind one minted type variable: the
/// shared counter mints `${name}#N` (both `#` and `$` sit outside the
/// identifier charset, so the split is unambiguous). Two minting sites
/// exist — instance declaration and body checking — and their atoms
/// differ, so consumers bridging the two (the runtime-evidence seam) key
/// by this name.
pub(crate) fn var_param_name(universe: &Universe, atom: TypeId) -> Option<String> {
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

/// The member atoms a written union of atoms names, read-only: an atom
/// itself, or a union of such sides, sorted and deduplicated — the
/// interned solved set's own order, so a field's layout is the one its
/// checked reads shape. `None` marks a side the walk cannot name
/// without solving (a tuple product, a trait constraint); those field
/// types keep their refusal.
pub(crate) fn expr_members(expr: &TypeExpr) -> Option<Vec<TypeId>> {
    match expr {
        TypeExpr::Atom(id) => Some(vec![*id]),
        TypeExpr::Union(a, b) => {
            let mut members = expr_members(a)?;
            members.extend(expr_members(b)?);
            members.sort();
            members.dedup();
            Some(members)
        },
        _ => None,
    }
}

/// The word types of one shape's span.
pub(crate) fn shape_words(shape: &Shape) -> Vec<SlotType> {
    match shape {
        Shape::Word(slot) => vec![*slot],
        Shape::Struct(s) | Shape::Tuple(s) => s
            .fields
            .iter()
            .flat_map(|f| f.slots.iter().copied())
            .collect(),
        Shape::Union(u) => u.words.clone(),
    }
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
        let empty_apply = HashMap::new();
        let empty_enum_apply = HashMap::new();
        let types =
            crate::types::Types::new(&analyzed.decls.universe, &empty_apply, &empty_enum_apply);
        let mut shapes = Shapes::new(&analyzed, &types, std::collections::HashSet::new());
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

    /// A part-less struct spans its one reserved word in every value
    /// position: the bare value's shape carries the unnamed I64 field
    /// at word 0, a struct-typed field of the shape claims one word of
    /// the enclosing inline span, and a union member of the shape
    /// claims one region word — so no value position spans zero words
    /// and a fn returning the shape never lowers an empty RETSPAN.
    #[test]
    fn a_partless_struct_spans_its_reserved_word_everywhere() {
        let src = concat!(
            "tag int\n",
            "tag string\n",
            "struct marker {}\n",
            "struct holder { m: marker, n: int }\n",
            "tag miss\n",
            "extern fn println (msg: string) -> ()\n",
            "fn make() -> marker { marker {} }\n",
            "fn make_h() -> holder { holder { m: marker {}, n: 1 } }\n",
            "fn tag_of(u: marker | miss) -> () {\n",
            "  match u {\n",
            "    marker {} => println(\"m\"),\n",
            "    miss => println(\"x\"),\n",
            "  }\n",
            "}\n",
        );
        let bare = shape_of(src, "marker");
        let Shape::Struct(s) = &*bare else {
            panic!("the part-less struct is a bare struct shape");
        };
        assert_eq!(s.fields, [MemberField {
            name: None,
            slots: vec![I64],
            word: 0,
        }]);
        let held = shape_of(src, "holder");
        let Shape::Struct(h) = &*held else {
            panic!("holder is a bare struct shape");
        };
        assert_eq!(h.fields, [
            MemberField {
                name: Some("m".into()),
                slots: vec![I64],
                word: 0,
            },
            MemberField {
                name: Some("n".into()),
                slots: vec![I64],
                word: 1,
            },
        ]);
        let union = shape_of(src, "marker | miss");
        let Shape::Union(u) = &*union else {
            panic!("the probe union is a union shape");
        };
        let marker = u
            .members
            .iter()
            .find(|m| m.fields.len() == 1)
            .expect("the part-less member claims one region word");
        assert_eq!(marker.fields, [MemberField {
            name: None,
            slots: vec![I64],
            word: 1,
        }]);
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

    /// The linked tag rows seed this compile's table (the shared tag
    /// mapping): a row naming a linked-fact mint keys the atom at the
    /// row's id, an exact repeat is a no-op, fresh mints continue past
    /// the keyed ids, every other row drops (a local atom's name never
    /// crosses — the linked signature naming it refused), and two
    /// linked names claiming one id refuse with the row named.
    #[test]
    fn linked_tag_rows_seed_key_mints_and_refuse_crossed_ids() {
        /// One linked row: the label under the producer's own declaring
        /// declaration name (the test's spellings share both).
        fn row(label: &str, id: u8) -> ds_mod::MemberTag {
            ds_mod::MemberTag {
                decl: label.to_owned(),
                name: label.to_owned(),
                id,
            }
        }
        let mut files = Files::new();
        let source = concat!(
            "pub tag int
",
            "tag local
",
            "import @lib (tone, quiet, loud)
",
            "fn probe(t: tone) -> int {
",
            "  match t {
",
            "    quiet => 1,
",
            "    loud => 2,
",
            "  }
",
            "}
",
            "fn main() -> int {
",
            "7
",
            "}
",
        );
        let id = files.add_source("seed.ds", source);
        let ast = parse(&files, id).expect("the probe parses");
        let at = ds_diag::Span::new(id, 1, 1);
        let entry = |name: &str, fact: Option<ds_check::TypeFact>| ds_check::ForeignEntry {
            name: name.to_owned(),
            vis: ds_check::Vis::Pub,
            kind: ds_check::ForeignKind::Type,
            generic: false,
            defaulted: false,
            decl_span: at,
            sig: None,
            row: ds_check::EffectRow::default(),
            caps: 0,
            value: None,
            ty: fact.map(std::sync::Arc::new),
            link: 0,
            origin: None,
            func: None,
            std: false,
        };
        let unit = || ds_check::import::CasePayload::Unit;
        let surface = ds_check::MemberSurface {
            tags: Vec::new(),
            impls: Vec::new(),
            inherent: Vec::new(),
            entries: vec![
                entry(
                    "tone",
                    Some(ds_check::TypeFact::Enum {
                        cases: vec![
                            ds_check::import::TypeCase {
                                name: "quiet".to_owned(),
                                payload: unit(),
                            },
                            ds_check::import::TypeCase {
                                name: "loud".to_owned(),
                                payload: unit(),
                            },
                        ],
                        linear: false,
                    }),
                ),
                entry(
                    "quiet",
                    Some(ds_check::TypeFact::Case(ds_check::import::CaseFact {
                        enum_name: "tone".to_owned(),
                        payload: unit(),
                    })),
                ),
                entry(
                    "loud",
                    Some(ds_check::TypeFact::Case(ds_check::import::CaseFact {
                        enum_name: "tone".to_owned(),
                        payload: unit(),
                    })),
                ),
            ],
        };
        let mut imports = ds_check::ImportGraph::new();
        for item in ast.blocks.iter().flat_map(|block| block.items.iter()) {
            if let ds_parse::ItemKind::Import(import) = &item.kind {
                imports.add(
                    import.span,
                    ds_check::ImportSurface::Linked(surface.clone()),
                );
            }
        }
        let (package, resolutions, _) = ds_check::resolve_package_linked(&[&ast], &imports);
        let analyzed = ds_check::type_of::analyze_resolved(
            &files,
            vec![std::sync::Arc::new(ast)].into(),
            package.into(),
            resolutions.into(),
            Vec::new(),
            std::sync::Arc::default(),
        );
        assert!(
            analyzed.checked.diags.is_empty(),
            "the probe checks clean: {:?}",
            analyzed.checked.diags
        );
        let atom_of = |name: &str| {
            analyzed
                .decls
                .foreign_type(name)
                .and_then(|def| analyzed.decls.atoms.get(&def).copied())
                .unwrap_or_else(|| panic!("{name} minted"))
        };
        let local = analyzed
            .decls
            .universe
            .atom_by_name("local")
            .expect("the local tag declares");

        // the keyed seed: quiet at 5, loud at 6, an exact repeat skipped,
        // the local atom's row dropped, a name resolving to nothing dropped
        let empty_apply = HashMap::new();
        let empty_enum_apply = HashMap::new();
        let types =
            crate::types::Types::new(&analyzed.decls.universe, &empty_apply, &empty_enum_apply);
        let mut shapes = Shapes::new(&analyzed, &types, std::collections::HashSet::new());
        shapes
            .seed_linked(&[
                row("quiet", 5),
                row("loud", 6),
                row("quiet", 5),
                row("local", 3),
                row("private", 4),
            ])
            .expect("the rows seed");
        assert_eq!(shapes.tag_id(atom_of("quiet")), Ok(5));
        assert_eq!(shapes.tag_id(atom_of("loud")), Ok(6));
        // the dropped rows' ids stay free and fresh mints continue past
        // the keyed ids
        assert_eq!(shapes.tag_id(local), Ok(7));

        // a second linked name claiming a keyed id refuses, naming the
        // row and the holder
        let empty_apply = HashMap::new();
        let empty_enum_apply = HashMap::new();
        let types =
            crate::types::Types::new(&analyzed.decls.universe, &empty_apply, &empty_enum_apply);
        let mut crossed = Shapes::new(&analyzed, &types, std::collections::HashSet::new());
        crossed
            .seed_linked(&[row("quiet", 5)])
            .expect("the first row seeds");
        let err = crossed
            .seed_linked(&[row("loud", 5)])
            .expect_err("two linked mints cannot share one id");
        assert!(
            err.to_string().contains("loud") && err.to_string().contains("quiet"),
            "the refusal names the row and the holder: {err}"
        );

        // a row reaching the reserved range refuses
        let empty_apply = HashMap::new();
        let empty_enum_apply = HashMap::new();
        let types =
            crate::types::Types::new(&analyzed.decls.universe, &empty_apply, &empty_enum_apply);
        let mut reserved = Shapes::new(&analyzed, &types, std::collections::HashSet::new());
        let err = reserved
            .seed_linked(&[row("quiet", FIRST_RESERVED_TAG)])
            .expect_err("the reserved range is closed");
        assert!(
            err.to_string().contains("reserved"),
            "the refusal names the range: {err}"
        );
    }
}
