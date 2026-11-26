//! The encoded-word vocabulary the linked facts spell types in: the
//! scalar, primitive, and heap-reference tables the producer's ambient
//! names resolve by, the word spelling those compose into, and the
//! solver that reads a producer's solved types back as words.

use super::*;

/// The word vocabulary a linked struct's fields spell in: the scalar
/// atoms by their ambient names, `bytes` (the immutable byte sequence's
/// one Ref word), the three container constructors closed over the
/// vocabulary itself — a list of any word, a map of any two words, and
/// an optional over any word (the word's union with `none`: one
/// presence tag word over the word's own payload region, exactly `t?`'s
/// local lowering) — and the Named word, an in-word reference to
/// another declared type the same producer member serves — one it
/// declares or one its own `pub import` row re-exports (a nested struct
/// field, a container over a declared member, an enum-typed field), so
/// every spelling the containers can build from the ambient atoms, the
/// member's own linkable declarations, and its re-export rows carries.
/// The consuming compile resolves a Named word through the linked
/// surface onto its own minted entity for that same declaration —
/// identity, not name lookup, never its ambient or local names. A tuple
/// crosses as one word per positional member — no ambient constructor
/// atom rides it, the mint re-spelling the written product the consuming
/// universe solves — while the empty tuple `()` and a type the member
/// cannot itself link stay outside.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum TypeWord {
    /// A scalar atom word.
    Scalar(ScalarWord),
    /// `list[word]`: one Ref word over the runtime's list object.
    List(Box<Self>),
    /// `bytes`: the immutable byte sequence's one Ref word, `string`'s
    /// shape over raw bytes. No plain literal spells it, so it rides
    /// the type vocabulary alone, never the const fact's scalar words.
    Bytes,
    /// `map[word, word]`: one Ref word over the runtime's map object,
    /// the list word's own shape over two key/value members.
    Map(Box<Self>, Box<Self>),
    /// `word?`: the word's union with `none` — one presence tag word
    /// over the word's own payload word(s), the same span a local
    /// optional field lowers to.
    Optional(Box<Self>),
    /// Another declared type of the same producer member: the name its
    /// own declaration owns, which the consuming compile resolves
    /// through the link table onto the def its registration minted — a
    /// flat struct (or the shape-pending opaque), an enum (the union of
    /// its minted case atoms), a minted case, or a tag. The containers
    /// wrap it like any word.
    Named(String),
    /// A union over other words: the member set the optional and the
    /// one-enum complete-case spellings do not cover — a partial
    /// case-subset union (`red | green`) or a mixed scalar-and-case set.
    /// Each member spells a word of its own; `t | none` rides the
    /// optional word, a lone unit-typed member stays outside (a field
    /// naming one keeps its refusal — the linked vocabulary's own line,
    /// narrower than the local spelling the Tag word serves).
    Union(Vec<Self>),
    /// A tuple `(word, word, ...)`: positional members over the
    /// vocabulary, one word each — the same product a local tuple
    /// field's conversion builds, so the consuming compile's mint
    /// re-spells the written tuple whose solving hash-conses the
    /// structural atom a local spelling would. The empty tuple `()`
    /// stays outside (the lone unit-typed word's own seam), as does a
    /// bare unit-typed member (the atom word's own gate).
    Tuple(Vec<Self>),
}

/// The scalar atoms the surface trusts by name: the one word vocabulary
/// the const and type facts spell in — each scalar fact maps into it
/// (`[ConstFact::scalar_word]`, the primitive word table), and the ambient
/// atom names resolve in the consuming compile's own universe, the same
/// trust the const fact's scalar atom enjoys.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ScalarWord {
    /// `int`.
    Int,
    /// `float`.
    Float,
    /// `bool`.
    Bool,
    /// `string`.
    Str,
    /// `char`.
    Char,
    /// `byte`.
    Byte,
    /// `u32`.
    U32,
}

impl ScalarWord {
    /// The ambient atom name the word resolves by in the consuming
    /// compile.
    #[must_use]
    pub const fn atom_name(self) -> &'static str {
        match self {
            Self::Int => "int",
            Self::Float => "float",
            Self::Bool => "bool",
            Self::Str => "string",
            Self::Char => "char",
            Self::Byte => "byte",
            Self::U32 => "u32",
        }
    }
}

/// The word one primitive atom's row of the shared table spells: a
/// scalar word the linked surface carries, the byte-sequence word it
/// carries as its own [`TypeWord::Bytes`] variant, or the builtin
/// comptime type-value atom, which never rides a field fact (its `Tag`
/// word lives only inside comptime evaluation) while the lowerer's
/// shape layer still maps its name.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PrimitiveWord {
    /// A scalar word of the linked surface's vocabulary.
    Scalar(ScalarWord),
    /// The byte-sequence word: one Ref word locally, its own
    /// linked-surface variant.
    Bytes,
    /// The builtin comptime type-value atom.
    ComptimeType,
}

/// The primitive word vocabulary: the one table the checker's linked
/// surface and the lowerer's shape layer both read, so widening a
/// primitive (a width riding an existing frame slot, as `u32` rides
/// the integer word) updates the linked surface and the shape mapping
/// in the same edit. The shape layer derives each row's frame slot
/// from its word, never from a second name table: a new scalar word
/// fails the lowerer's slot match until mapped, the same
/// compile-time discipline the table itself keeps.
const PRIMITIVE_WORDS: &[(&str, PrimitiveWord)] = &[
    ("int", PrimitiveWord::Scalar(ScalarWord::Int)),
    ("byte", PrimitiveWord::Scalar(ScalarWord::Byte)),
    ("u32", PrimitiveWord::Scalar(ScalarWord::U32)),
    ("float", PrimitiveWord::Scalar(ScalarWord::Float)),
    ("bool", PrimitiveWord::Scalar(ScalarWord::Bool)),
    ("string", PrimitiveWord::Scalar(ScalarWord::Str)),
    ("char", PrimitiveWord::Scalar(ScalarWord::Char)),
    ("bytes", PrimitiveWord::Bytes),
    ("type", PrimitiveWord::ComptimeType),
];

/// The word one ambient atom name spells, when the primitive
/// vocabulary trusts it; every other name answers `None`.
#[must_use]
pub fn primitive_word(name: &str) -> Option<PrimitiveWord> {
    PRIMITIVE_WORDS
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, word)| *word)
}

/// One heap-reference container's row of the shared table: the
/// constructor's ambient atom name, its application's argument count,
/// and whether the container's word crosses the linked surface — the
/// one place the linked walk's container names and the lowerer's heap
/// constructors agree, so a new container updates both sides in the
/// same edit. `chan` stays local-only: the value crosses through the
/// platform's natives while the link fact vocabulary spells no word
/// for its cross-boundary surface yet.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct HeapRefWord {
    /// The constructor's ambient atom name.
    pub name: &'static str,
    /// The application's argument count.
    pub args: usize,
    /// Whether the container's word crosses the linked surface.
    pub linked: bool,
}

/// The heap-reference container vocabulary, one row per constructor.
const HEAP_REFS: &[HeapRefWord] = &[
    HeapRefWord {
        name: "list",
        args: 1,
        linked: true,
    },
    HeapRefWord {
        name: "map",
        args: 2,
        linked: true,
    },
    HeapRefWord {
        name: "chan",
        args: 1,
        linked: false,
    },
    HeapRefWord {
        name: "arena",
        args: 1,
        linked: false,
    },
    HeapRefWord {
        name: "box",
        args: 1,
        linked: false,
    },
];

/// The heap-reference container one ambient atom name constructs,
/// when the vocabulary names it; every other name answers `None`.
#[must_use]
pub fn heap_ref(name: &str) -> Option<HeapRefWord> {
    HEAP_REFS.iter().find(|row| row.name == name).copied()
}

/// One word's diagnostic spelling, the producer's own vocabulary
/// rendered back for a consuming compile's coherence refusals.
#[must_use]
pub fn render_word(word: &TypeWord) -> String {
    match word {
        TypeWord::Scalar(word) => word.atom_name().to_owned(),
        TypeWord::Bytes => "bytes".to_owned(),
        TypeWord::List(inner) => format!("list[{}]", render_word(inner)),
        TypeWord::Map(key, value) => {
            format!("map[{}, {}]", render_word(key), render_word(value))
        },
        TypeWord::Optional(inner) => format!("{}?", render_word(inner)),
        TypeWord::Named(name) => name.clone(),
        TypeWord::Union(words) => words
            .iter()
            .map(render_word)
            .collect::<Vec<_>>()
            .join(" | "),
        TypeWord::Tuple(words) => format!(
            "({})",
            words.iter().map(render_word).collect::<Vec<_>>().join(", ")
        ),
    }
}

/// One fact build's word resolver: the ambient vocabulary plus the
/// in-word references to the other linked declared types of the same
/// member, gated by the co-derivation fixpoint's eligible set.
pub(super) struct WordSolver<'a> {
    /// The producer compile's package: the def kinds and spans the
    /// gates read, and the import-bound rows and re-export records a
    /// Named reference resolves through.
    pub(super) package: &'a crate::resolve::Package,
    /// The producer compile's solved declarations.
    pub(super) decls: &'a crate::Decls,
    /// The fixpoint's current eligible set: the declarations a Named
    /// reference may name.
    pub(super) eligible: &'a FxHashSet<crate::DefId>,
    /// The file of the declaration whose fact is being built: the
    /// same-member gate every Named reference passes (the surface
    /// serves one member's own definitions and its own `pub import`
    /// rows, so a cross-member reference would name no row).
    pub(super) file: ds_diag::FileId,
}

impl WordSolver<'_> {
    /// One solved field type's word kind, when it spells in the
    /// vocabulary: a bare atom, a union — `t | none` either order is
    /// exactly `t?`, and any other union is one enum's complete
    /// case set or a union of words — or a tuple, one word per
    /// positional member.
    pub(super) fn word_of_expr(&self, expr: &crate::TypeExpr) -> Option<TypeWord> {
        match expr {
            crate::TypeExpr::Atom(id) => self.atom_word(*id),
            crate::TypeExpr::Union(left, right) => self.union_word(left, right),
            crate::TypeExpr::Tuple(members) => self.tuple_word(members),
            _ => None,
        }
    }

    /// One solved tuple field's word kind: one word per positional
    /// member, each spelling in the vocabulary. The empty tuple (the
    /// arity-0 atom) stays outside — the lone unit-typed word's own
    /// seam — and a bare unit-typed member refuses through the atom
    /// word's own gate.
    fn tuple_word(&self, members: &[crate::TypeExpr]) -> Option<TypeWord> {
        let [first, rest @ ..] = members else {
            return None;
        };
        let mut out = Vec::with_capacity(1 + rest.len());
        for member in std::iter::once(first).chain(rest) {
            out.push(self.word_of_expr(member)?);
        }
        Some(TypeWord::Tuple(out))
    }

    /// One solved argument type's word kind — a container member's
    /// `Ty`, a solved set of atoms: a single-atom set is the atom's own
    /// word, a set naming `none` beside one word is exactly that
    /// word's optional, and any other multi-member set is one enum's
    /// complete case set (the enum's Named word) or a union of words
    /// (the optional wrapping either).
    pub(super) fn word_of_ty(&self, ty: crate::Ty) -> Option<TypeWord> {
        let mut atoms = Vec::new();
        let mut optional = false;
        for &atom in self.decls.universe.members(ty) {
            if atom == crate::TypeId::NONE {
                optional = true;
            } else {
                atoms.push(atom);
            }
        }
        let word = match atoms.as_slice() {
            [only] => self.atom_word(*only),
            [] => return None,
            many => self.enum_word(many).or_else(|| self.union_of_atoms(many)),
        }?;
        if optional { Some(TypeWord::Optional(Box::new(word))) } else { Some(word) }
    }

    /// One atom's word kind: the ambient scalars and `bytes` by name,
    /// the `list`/`map` constructors recursively over their members, a
    /// solved tuple atom as one word per element, and a declared
    /// type's own atom as the Named word.
    fn atom_word(&self, id: crate::atom::TypeId) -> Option<TypeWord> {
        let decl = self.decls.universe.atom_decl(id).ok()?;
        match &decl.kind {
            // a primitive word: the shared table's row the name spells
            crate::AtomKind::Unit => match primitive_word(&decl.name) {
                Some(PrimitiveWord::Scalar(word)) => Some(TypeWord::Scalar(word)),
                Some(PrimitiveWord::Bytes) => Some(TypeWord::Bytes),
                Some(PrimitiveWord::ComptimeType) | None => None,
            },
            // a heap-reference constructor applied to word members
            crate::AtomKind::Apply(ctor, args) => {
                let ctor = self.decls.universe.atom_decl(*ctor).ok()?;
                let row = heap_ref(&ctor.name).filter(|row| row.linked)?;
                if args.len() != row.args {
                    return None;
                }
                match row.name {
                    "list" => self
                        .word_of_ty(args[0])
                        .map(|word| TypeWord::List(Box::new(word))),
                    "map" => Some(TypeWord::Map(
                        Box::new(self.word_of_ty(args[0])?),
                        Box::new(self.word_of_ty(args[1])?),
                    )),
                    _ => None,
                }
            },
            // a solved tuple atom — a container member over a tuple
            // (a field's own spelling held the product expr): one word
            // per element, the empty tuple refusing like the lone
            // unit-typed word
            crate::AtomKind::Tuple(elements) => {
                let [first, rest @ ..] = &elements[..] else {
                    return None;
                };
                let mut out = Vec::with_capacity(1 + rest.len());
                for &element in std::iter::once(first).chain(rest) {
                    out.push(self.atom_word(element)?);
                }
                Some(TypeWord::Tuple(out))
            },
            // a declared type's own atom: a struct, a minted case, a
            // payloaded tag — or the shape-pending opaque reservation
            crate::AtomKind::Struct(_) | crate::AtomKind::Reserved => self.named_word(id),
            _ => None,
        }
    }

    /// One union's word kind: `t | none` in either order is exactly
    /// `t?` — the optional over the other side's whole word — any
    /// other union is one enum's complete case set or a union of words.
    fn union_word(&self, left: &crate::TypeExpr, right: &crate::TypeExpr) -> Option<TypeWord> {
        let atom = |expr: &crate::TypeExpr| match expr {
            crate::TypeExpr::Atom(id) => Some(*id),
            _ => None,
        };
        let inner = if atom(right) == Some(crate::TypeId::NONE) {
            left
        } else if atom(left) == Some(crate::TypeId::NONE) {
            right
        } else {
            let mut atoms = Vec::new();
            Self::flatten_union(left, &mut atoms)?;
            Self::flatten_union(right, &mut atoms)?;
            return self
                .enum_word(&atoms)
                .or_else(|| self.union_of_atoms(&atoms));
        };
        if atom(inner) == Some(crate::TypeId::NONE) {
            return None; // `none` alone spells no payload word
        }
        self.word_of_expr(inner)
            .map(|word| TypeWord::Optional(Box::new(word)))
    }

    /// A multi-atom set's union word: one word per atom, each spelling
    /// in the vocabulary (a scalar, `bytes`, a container, a declared
    /// type's Named word). A lone unit-typed atom spells only inside a
    /// union — a field naming one keeps its refusal — so the unit kind
    /// falls to the Named word here alone.
    fn union_of_atoms(&self, atoms: &[crate::atom::TypeId]) -> Option<TypeWord> {
        let mut words = Vec::with_capacity(atoms.len());
        for &atom in atoms {
            words.push(self.atom_word(atom).or_else(|| self.named_word(atom))?);
        }
        Some(TypeWord::Union(words))
    }

    /// Collects one union tree's atoms; a non-atom node spells no Named
    /// word.
    fn flatten_union(expr: &crate::TypeExpr, atoms: &mut Vec<crate::atom::TypeId>) -> Option<()> {
        match expr {
            crate::TypeExpr::Atom(id) => atoms.push(*id),
            crate::TypeExpr::Union(left, right) => {
                Self::flatten_union(left, atoms)?;
                Self::flatten_union(right, atoms)?;
            },
            _ => return None,
        }
        Some(())
    }

    /// One atom set's word kind when the set is exactly one non-generic
    /// enum's case atoms — that enum's own Named word: the written
    /// spelling names the enum, whose solving rebuilds this union.
    /// A partial case set (a true subset union) answers `None` here and
    /// the caller's union fallback spells it word per member.
    fn enum_word(&self, atoms: &[crate::atom::TypeId]) -> Option<TypeWord> {
        let mut enum_def = None;
        for &atom in atoms {
            let def = self.decls.defs_of_atoms.get(&atom).copied()?;
            let case = self.decls.cases.get(&def)?;
            match enum_def {
                None => enum_def = Some(case.enum_def),
                Some(seen) if seen == case.enum_def => {},
                Some(_) => return None,
            }
        }
        let enum_def = enum_def?;
        let shape = self.decls.enums.get(&enum_def)?;
        if !shape.params.is_empty() {
            return None;
        }
        // the set must cover every case: the enum's whole value set
        let mut covered = Vec::with_capacity(shape.cases.len());
        for (name, ..) in &shape.cases {
            // the cases of a re-exported enum are import-bound defs
            // the name index the linked install filled answers
            let case_def = self
                .package
                .lookup_type(name)
                .or_else(|| self.decls.foreign_type(name))?;
            covered.push(self.decls.atoms.get(&case_def).copied()?);
        }
        covered.sort_unstable();
        let mut named = atoms.to_vec();
        named.sort_unstable();
        if covered != named {
            return None;
        }
        self.named_name(enum_def).map(TypeWord::Named)
    }

    /// One declared-type atom's word: the Named word, an in-word
    /// reference the consuming compile resolves through the linked
    /// surface onto its own minted entity for that same declaration —
    /// never its ambient or local names.
    fn named_word(&self, atom: crate::atom::TypeId) -> Option<TypeWord> {
        let def = self.decls.defs_of_atoms.get(&atom).copied()?;
        self.named_name(def).map(TypeWord::Named)
    }

    /// The name a Named reference to one def carries, when the
    /// consuming surface serves that name: a same-member eligible
    /// declaration under its own declared name, and an import-bound
    /// def under the name a `pub import` row of this member re-exports
    /// it by (the re-exported fact rode the origin package's own
    /// derivation, so the reference needs no co-derivation here). A
    /// privately imported def names no surface row of any dependent,
    /// so it carries no word and keeps its refusal; the same-member
    /// gate keeps a cross-member declaration out the same way.
    fn named_name(&self, def: crate::DefId) -> Option<String> {
        if let Some(entry) = self.package.foreign().get(&def) {
            entry.ty.as_ref()?;
            return self
                .package
                .reexports()
                .iter()
                .find(|&&(row_def, _, span)| row_def == def && span.file == self.file)
                .map(|(_, name, _)| name.clone());
        }
        (self.package.def(def).span.file == self.file && self.eligible.contains(&def))
            .then(|| self.package.def(def).name.clone())
    }

    /// One case's payload shape over the vocabulary, from its minted
    /// declaration products.
    pub(super) fn case_payload(&self, def: crate::DefId) -> Option<CasePayload> {
        match &self.decls.cases.get(&def)?.payload {
            ds_parse::EnumPayload::None => Some(CasePayload::Unit),
            // the tuple payload's members are the case atom's positional
            // fields: each solves to one word of the vocabulary
            ds_parse::EnumPayload::Tuple(_) => {
                let fields = self.atom_fields(def)?;
                let mut out = Vec::with_capacity(fields.len());
                for field in fields {
                    out.push(self.word_of_expr(&field.ty)?);
                }
                Some(CasePayload::Tuple(out))
            },
            ds_parse::EnumPayload::Struct(_) => {
                let fields = self.atom_fields(def)?;
                let mut out = Vec::with_capacity(fields.len());
                for field in fields {
                    out.push(TypeField {
                        name: field.name.clone()?,
                        word: self.word_of_expr(&field.ty)?,
                    });
                }
                Some(CasePayload::Struct(out))
            },
        }
    }

    /// The solved field signature one payloaded case's atom carries.
    fn atom_fields(&self, def: crate::DefId) -> Option<Vec<crate::Field>> {
        let atom = self.decls.atoms.get(&def)?;
        match self.decls.universe.atom_decl(*atom).ok()?.kind {
            crate::AtomKind::Struct(ref fields) => Some(fields.clone()),
            _ => None,
        }
    }

    /// One written type's word kind — the alias body's resolver: the
    /// ambient spellings resolve by name (spelling-resolution equals
    /// universe-resolution only because ambient names are unshadowable,
    /// `Universe::declare_atom` refusing `DuplicateName`), a path
    /// naming the producer's own declared types carries the Named word
    /// (a struct or an enum at the top level, a minted case or a tag
    /// only inside a union — the field vocabulary's own line), an alias
    /// chain expands transparently through the referenced alias's own
    /// body (visit-stack-guarded, so an inline cycle refuses), an
    /// optional marker wraps one word, parenthesization peels, and a
    /// union body builds one word per member with `t | none` the
    /// optional rule. Every other written shape — a tuple, a fn
    /// type, a multi-segment path, an applied alias — answers `None`.
    pub(super) fn written_word(
        &self,
        ty: &ds_parse::Type,
        in_union: bool,
        stack: &mut Vec<crate::DefId>,
    ) -> Option<TypeWord> {
        match ty {
            ds_parse::Type::Path { path, args, .. } => {
                let [only] = path.segments.as_slice() else {
                    return None;
                };
                if args.is_empty() {
                    // the primitive words the shared table spells; a
                    // name it does not trust falls to the producer's
                    // own declarations
                    return match primitive_word(&only.name) {
                        Some(PrimitiveWord::Scalar(word)) => Some(TypeWord::Scalar(word)),
                        Some(PrimitiveWord::Bytes) => Some(TypeWord::Bytes),
                        Some(PrimitiveWord::ComptimeType) | None => {
                            self.written_named(&only.name, in_union, stack)
                        },
                    };
                }
                let row = heap_ref(&only.name).filter(|row| row.linked)?;
                if row.args != args.len() {
                    return None;
                }
                match row.name {
                    "list" => self
                        .written_word(&args[0], false, stack)
                        .map(|word| TypeWord::List(Box::new(word))),
                    "map" => Some(TypeWord::Map(
                        Box::new(self.written_word(&args[0], false, stack)?),
                        Box::new(self.written_word(&args[1], false, stack)?),
                    )),
                    _ => None,
                }
            },
            ds_parse::Type::Optional { inner, .. } => self
                .written_word(inner, false, stack)
                .map(|word| TypeWord::Optional(Box::new(word))),
            ds_parse::Type::Paren { inner, .. } => self.written_word(inner, in_union, stack),
            ds_parse::Type::Union { left, right, .. } => self.written_union(left, right, stack),
            _ => None,
        }
    }

    /// One written path's word when the name resolves onto the
    /// producer's own declared types: another alias expands through its
    /// own body (the visit stack breaking an inline cycle), and a
    /// struct or an enum carries the Named word — a minted case or a
    /// tag only inside a union, where the union's tag word carries
    /// their values. The [`Self::named_name`] gates (the same-member
    /// rule, the re-export row, the fixpoint's eligibility) own every
    /// Named reference here.
    fn written_named(
        &self,
        name: &str,
        in_union: bool,
        stack: &mut Vec<crate::DefId>,
    ) -> Option<TypeWord> {
        let def = self.package.lookup_type(name)?;
        match self.package.def(def).kind {
            crate::DefKind::TypeAlias => {
                if stack.contains(&def) {
                    return None;
                }
                let (params, body) = self.decls.aliases.get(&def)?;
                if !params.is_empty() {
                    return None;
                }
                stack.push(def);
                let word = self.written_word(body, false, stack);
                stack.pop();
                word
            },
            crate::DefKind::Struct | crate::DefKind::Enum => {
                self.named_name(def).map(TypeWord::Named)
            },
            crate::DefKind::Case | crate::DefKind::Tag if in_union => {
                self.named_name(def).map(TypeWord::Named)
            },
            _ => None,
        }
    }

    /// One written union body's word: the members flatten (a
    /// parenthesized subtree recursing), the predeclared `none` marks
    /// the optional, and every other member carries one word — a
    /// case or a tag spelling here as the Named word the union's tag
    /// word carries. A `none`-only body answers `None`.
    fn written_union(
        &self,
        left: &ds_parse::Type,
        right: &ds_parse::Type,
        stack: &mut Vec<crate::DefId>,
    ) -> Option<TypeWord> {
        let mut members: Vec<Option<TypeWord>> = Vec::new();
        self.flatten_written_union(left, stack, &mut members)?;
        self.flatten_written_union(right, stack, &mut members)?;
        let mut optional = false;
        let mut words = Vec::new();
        for member in members {
            match member {
                None => optional = true,
                Some(word) => words.push(word),
            }
        }
        let inner = match words.len() {
            0 => return None,
            1 => words.pop().expect("the slice held one word"),
            _ => TypeWord::Union(words),
        };
        if optional { Some(TypeWord::Optional(Box::new(inner))) } else { Some(inner) }
    }

    /// Flattens one written union subtree into per-member words, a
    /// `None` entry marking the predeclared `none` sentinel.
    fn flatten_written_union(
        &self,
        ty: &ds_parse::Type,
        stack: &mut Vec<crate::DefId>,
        members: &mut Vec<Option<TypeWord>>,
    ) -> Option<()> {
        match ty {
            ds_parse::Type::Paren { inner, .. } => {
                self.flatten_written_union(inner, stack, members)
            },
            ds_parse::Type::Union { left, right, .. } => {
                self.flatten_written_union(left, stack, members)?;
                self.flatten_written_union(right, stack, members)
            },
            ds_parse::Type::Path { path, args, .. }
                if args.is_empty()
                    && let [only] = path.segments.as_slice()
                    && only.name == "none"
                    && !self.package.declares("none") =>
            {
                members.push(None);
                Some(())
            },
            other => {
                members.push(Some(self.written_word(other, true, stack)?));
                Some(())
            },
        }
    }
}
