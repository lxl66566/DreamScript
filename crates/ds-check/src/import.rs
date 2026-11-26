//! The G4 import surface: the linked half of name binding. An import's
//! selected symbols bind at use sites through a caller-supplied surface —
//! one entry per import declaration, keyed by the declaration's span —
//! while an `@std` import no graph serves keeps the merged-ambient
//! reading (its selections validate against the compile unit's merged
//! namespace, the landed visibility-round behavior) and an import no
//! linked package serves stays declarative for the link walk.

use std::sync::Arc;

use ds_diag::Span;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::{EffectRow, Vis, decls::FnSig};

/// The per-target link fact a linked `const` binding carries: the
/// literal value itself, content-addressable, so a binding site embeds
/// it exactly as the literal a user would have written — no linked-module
/// evaluation and no instantiation machinery behind it. A computed
/// initializer carries no fact (its value is an evaluation of the linked
/// module, the artifact round's seam) and stays refused.
#[derive(Clone, PartialEq, Debug)]
pub enum ConstFact {
    /// An unsuffixed integer literal, radix prefixes included.
    Int(i64),
    /// An unsuffixed float literal.
    Float(f64),
    /// `true` / `false`.
    Bool(bool),
    /// A plain or raw string literal with no interpolation holes.
    Str(String),
    /// A char literal (rides the one-char string at runtime, as always).
    Char(char),
    /// A byte literal (stages as its 0-255 integer value).
    Byte(u8),
    /// A list literal of one scalar word's plain literals, non-empty and
    /// homogeneous: the list-typed initializer a local `const` of the
    /// same spelling carries, one Ref word the embed builds element by
    /// element over the same `list_new`/`list_push` seam a local list
    /// literal lowers through. An empty literal carries no fact (the
    /// local spelling itself has no element evidence to infer from), and
    /// a nested or mixed list carries none (each element must be a plain
    /// literal of the one word).
    List(Vec<Self>),
}

impl ConstFact {
    /// The fact one initializer expression carries, when the expression is
    /// a plain literal (parenthesization peels, a unary minus folds over a
    /// numeric literal) or a list of one scalar word's plain literals.
    /// Every other shape — a computed value, an interpolated string, a
    /// suffixed literal, an empty or nested or mixed list — answers
    /// `None` and keeps its selection refused.
    #[must_use]
    pub fn of_expr(expr: &ds_parse::Expr) -> Option<Self> {
        let mut expr = expr;
        loop {
            match expr {
                ds_parse::Expr::Paren { inner, .. } => expr = inner,
                ds_parse::Expr::List { items, .. } => {
                    // the local spelling's own rules: non-empty (an empty
                    // literal has no element evidence to infer from) and
                    // one scalar word across every element
                    let [first, rest @ ..] = &items[..] else {
                        return None;
                    };
                    let word = Self::of_expr(first)?.scalar_word()?;
                    let mut items = Vec::with_capacity(1 + rest.len());
                    for item in std::iter::once(first).chain(rest) {
                        let fact = Self::of_expr(item)?;
                        (fact.scalar_word() == Some(word)).then_some(())?;
                        items.push(fact);
                    }
                    return Some(Self::List(items));
                },
                ds_parse::Expr::Unary {
                    op, expr: operand, ..
                } => {
                    let ds_parse::Expr::Literal { lit, .. } = &**operand else {
                        return None;
                    };
                    return match (op, lit) {
                        (ds_parse::UnaryOp::Neg, ds_parse::Literal::Int { suffix, text })
                            if suffix.is_empty() =>
                        {
                            parse_int(text).and_then(i64::checked_neg).map(Self::Int)
                        },
                        (ds_parse::UnaryOp::Neg, ds_parse::Literal::Float { suffix, text })
                            if suffix.is_empty() =>
                        {
                            text.parse::<f64>().ok().map(|value| Self::Float(-value))
                        },
                        _ => None,
                    };
                },
                ds_parse::Expr::Literal { lit, .. } => return Self::of_literal(lit),
                ds_parse::Expr::Str { parts, .. } => {
                    let mut text = String::new();
                    for part in parts {
                        match part {
                            ds_parse::StrPart::Text { text: chunk, .. } => {
                                text.push_str(chunk);
                            },
                            _ => return None,
                        }
                    }
                    return Some(Self::Str(text));
                },
                ds_parse::Expr::RawStr { text, .. } => return Some(Self::Str(text.clone())),
                _ => return None,
            }
        }
    }

    /// The fact one literal carries; suffixed numerics answer `None`
    /// (explicit widths are not in the contract).
    fn of_literal(lit: &ds_parse::Literal) -> Option<Self> {
        match lit {
            ds_parse::Literal::Int { suffix, text } if suffix.is_empty() => {
                parse_int(text).map(Self::Int)
            },
            ds_parse::Literal::Float { suffix, text } if suffix.is_empty() => {
                text.parse::<f64>().ok().map(Self::Float)
            },
            ds_parse::Literal::Bool(value) => Some(Self::Bool(*value)),
            ds_parse::Literal::Char(c) => Some(Self::Char(*c)),
            ds_parse::Literal::Byte(b) => Some(Self::Byte(*b)),
            _ => None,
        }
    }

    /// The scalar word a scalar fact's value types as; `None` for the
    /// list fact (its vocabulary is the element word inside).
    const fn scalar_word(&self) -> Option<ScalarWord> {
        match self {
            Self::Int(_) => Some(ScalarWord::Int),
            Self::Float(_) => Some(ScalarWord::Float),
            Self::Bool(_) => Some(ScalarWord::Bool),
            Self::Str(_) => Some(ScalarWord::Str),
            Self::Char(_) => Some(ScalarWord::Char),
            Self::Byte(_) => Some(ScalarWord::Byte),
            Self::List(_) => None,
        }
    }

    /// The type the fact's value carries in the consuming compile, minted
    /// in that compile's own universe: a scalar fact's ambient atom, a
    /// list fact the applied `list` constructor over its element word's
    /// atom — the same mints a local initializer's own solving produces.
    /// `None` when a name resolves to no atom there (the uses then fail
    /// at their own sites rather than mis-shaping).
    pub(crate) fn ty(&self, universe: &mut crate::Universe) -> Option<crate::Ty> {
        match self {
            Self::List(items) => {
                let word = items.first()?.scalar_word()?;
                let element = scalar_ty(universe, word)?;
                let ctor = universe.atom_by_name("list")?;
                let apply = universe.apply_atom(ctor, &[element]).ok()?;
                universe.atom_ty(apply).ok()
            },
            fact => scalar_ty(universe, fact.scalar_word()?),
        }
    }
}

/// One scalar word's ambient atom type in the consuming universe.
fn scalar_ty(universe: &mut crate::Universe, word: ScalarWord) -> Option<crate::Ty> {
    let atom = universe.atom_by_name(word.atom_name())?;
    universe.atom_ty(atom).ok()
}

/// One integer literal's digit text (radix prefix kept, separators already
/// stripped by the parser) to its word.
fn parse_int(text: &str) -> Option<i64> {
    let (radix, digits) = match text.as_bytes() {
        [b'0', b'x', ..] => (16, &text[2..]),
        [b'0', b'b', ..] => (2, &text[2..]),
        [b'0', b'o', ..] => (8, &text[2..]),
        _ => (10, text),
    };
    i64::from_str_radix(digits, radix).ok()
}

/// The per-target link fact a linked data type carries: its structural
/// word layout over the surface's trusted vocabulary, so the consuming
/// compile mints the same declaration products a local declaration
/// would — one atom, one field signature, one AST-shaped row per
/// declaration product. The vocabulary carries a flat struct's named
/// fields (a linear declaration's move-discipline bit riding beside
/// them), an enum's tag rows (the same bit), a lone tag's unit atom, a
/// non-generic alias's resolved word, a bodiless struct's opaque
/// identity, and a plain trait's member rows; every other
/// type-namespace shape (a generic declaration) and every field outside
/// the vocabulary carries no fact and keeps its selection refused.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum TypeFact {
    /// A flat, non-generic struct: named fields over the word
    /// vocabulary, with the declaration's linearity (`~Copyable`)
    /// riding beside them — the consuming compile's mint carries the
    /// same move discipline a local declaration would. The producer
    /// gates deinit-carrying declarations out of the fact: a linked
    /// deinit body is the artifact round's member machinery.
    Struct {
        /// Named fields over the word vocabulary.
        fields: Vec<TypeField>,
        /// Whether the declaration subtracts copyability.
        linear: bool,
    },
    /// A non-generic enum: its tag rows, one per case, in declaration
    /// order — an empty enum rides its empty rows, so the name binds
    /// uninhabited exactly like the local spelling — with the
    /// declaration's linearity riding beside them. Every payload must
    /// spell in the vocabulary: a unit case, a struct payload of
    /// word-shaped fields, or a tuple payload of word-shaped positional
    /// members.
    Enum {
        /// Tag rows, one per case.
        cases: Vec<TypeCase>,
        /// Whether the declaration subtracts copyability.
        linear: bool,
    },
    /// A lone tag: the unit atom a local `tag` declaration mints, the
    /// name and identity alone — no layout, no case rows. Its values
    /// cross through the shared tag mapping exactly like a unit case's:
    /// the producer's tag-table row seeds the consuming compile's table
    /// at lowering.
    Tag,
    /// One case minted by a linked enum: the case's own entry in the
    /// type namespace, naming its enum so the consuming compile's case
    /// row maps back. Registers only beside its enum's registration.
    Case(CaseFact),
    /// A non-generic alias whose body spells in the member's own
    /// linkable vocabulary — the same words a struct field spells, an
    /// alias chain expanding transparently, a union body carrying one
    /// word per member: the resolved word rides, and the consuming
    /// compile's mint rewrites it as the written type a local alias of
    /// the same body would carry — the name expands in that compile's
    /// own universe exactly like a local spelling. A generic alias and
    /// a body the vocabulary cannot spell (a tuple, an applied alias)
    /// carry no fact.
    Alias(Box<TypeWord>),
    /// A bodiless, non-generic, non-linear struct declaration: the
    /// opaque handle a local `struct x` with no body reserves — identity
    /// alone, no layout. The consuming compile's mint reserves the same
    /// shape-pending atom, so the name binds in signatures while every
    /// value use keeps the local spelling's own refusals (no
    /// construction lowers, no witness exists).
    Opaque,
    /// A plain trait: its member rows, one per declared `fn`, each
    /// carrying the written signature a call checks against — the rows
    /// the consuming compile's mint registers exactly like a local
    /// declaration's, so the trait's members join the method registry
    /// and a linked impl's dispatch resolves through them. A member's
    /// declared default body crosses as its own link row when it plans
    /// and stages no runtime evidence; a generic trait, a supertrait,
    /// and an associated type keep their selection refusals.
    Trait(TraitFact),
}

/// One linked trait's declaration fact: its member rows in declaration
/// order, each carrying the written signature. The signatures ride to
/// the decls table and are never compared — one source text determines
/// the fact, exactly like a linked fn's own signature.
#[derive(Clone, Debug)]
pub struct TraitFact {
    /// The trait's member rows, in declaration order.
    pub members: Vec<TraitMemberFact>,
}

/// One member row of a linked trait.
#[derive(Clone, Debug)]
pub struct TraitMemberFact {
    /// The declared member name.
    pub name: String,
    /// The member's name occurrence, rebased into the consuming
    /// compile's file space so method diagnostics render.
    pub span: Span,
    /// The written signature, as the declaring trait spells it.
    pub sig: Arc<FnSig>,
    /// The member's declared default body, when it crossed: the body's
    /// index in the linked module's table — the dispatch target a
    /// consumer reaches when its solving impl spells no member, the
    /// linked mirror of the plan's own default-body table. `None` for
    /// a bodiless member or a body the producer's walk refused.
    pub default_func: Option<u32>,
}

impl PartialEq for TraitFact {
    fn eq(&self, other: &Self) -> bool {
        self.members == other.members
    }
}

impl Eq for TraitFact {}

impl PartialEq for TraitMemberFact {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
            && self.span == other.span
            && self.default_func == other.default_func
    }
}

impl Eq for TraitMemberFact {}

/// One linked trait impl's member link fact: the impl that owns the
/// members, the one word its head implements them over, and each bodied
/// member's index in the linked module's table — the CALLG target a
/// consuming dispatch site addresses. A generic impl, a conditional
/// impl, and an impl whose members did not all plan keep their
/// refusals.
#[derive(Clone, Debug)]
pub struct ImplFact {
    /// The implemented trait's declared name.
    pub trait_name: String,
    /// The implemented head, one word of the vocabulary.
    pub head: TypeWord,
    /// The impl's bodied members, one row per member fn.
    pub members: Vec<ImplMemberFact>,
    /// The impl item's span, rebased into the consuming compile's file
    /// space so coherence diagnostics render.
    pub span: Span,
    /// The linked dep entry's position in the consuming module's
    /// recorded list, assigned at the surface build: the module whose
    /// table holds the member bodies.
    pub link: u32,
}

// PartialEq ignores the span: equality is the linked impl's identity
// (the trait, the head, the member rows, the owning module's link
// position), which one producer source determines; the span rides to
// the coherence diagnostic alone, and two surface builds of one package
// rebase it differently — the link pass's dedup keys on this equality,
// never the span.
impl PartialEq for ImplFact {
    fn eq(&self, other: &Self) -> bool {
        self.trait_name == other.trait_name
            && self.head == other.head
            && self.members == other.members
            && self.link == other.link
    }
}

impl Eq for ImplFact {}

/// One member row of a linked trait impl.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ImplMemberFact {
    /// The member's name, matching the trait's own row.
    pub name: String,
    /// The member body's index in the linked module's table.
    pub func: u32,
}

/// One case of a linked enum: its name and payload shape.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct TypeCase {
    /// The declared case name.
    pub name: String,
    /// The case's payload shape.
    pub payload: CasePayload,
}

/// A linked enum case's payload over the word vocabulary.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum CasePayload {
    /// A bare case: the tag alone.
    Unit,
    /// A tuple payload: positional members over the word vocabulary,
    /// constructed by call form exactly like the local spelling.
    Tuple(Vec<TypeWord>),
    /// A struct payload: named fields over the word vocabulary,
    /// positionally constructed.
    Struct(Vec<TypeField>),
}

/// The fact one linked enum case carries: its enum's name (the case's
/// row maps back to the enum's def) and the payload shape.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct CaseFact {
    /// The owning enum's declared name.
    pub enum_name: String,
    /// The case's payload shape.
    pub payload: CasePayload,
}

/// One field of a linked struct: its name and word kind.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct TypeField {
    /// The declared field name.
    pub name: String,
    /// The field's word kind.
    pub word: TypeWord,
}

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

impl TypeFact {
    /// The link facts for every type declaration one package compile
    /// exports, as one co-derivation fixpoint: a field may name another
    /// declared type of the same member, and that reference's
    /// eligibility is the referenced declaration's own fact, so the
    /// per-def facts derive together over an eligible set that starts
    /// at every candidate and shrinks until stable — a pair of structs
    /// naming each other through container words co-derives in one
    /// round, while an inline cycle never stabilizes and keeps its
    /// refusal. A Named reference may also name a type a `pub import`
    /// row re-exports, whose fact the origin package already derived —
    /// no co-derivation, only the row's same-member gate. The
    /// candidates are the compile's own type defs (the
    /// `ambient_files` leading trees are the ambient members, whose
    /// definitions never export; an import-bound def is another
    /// package's identity and rides its own re-export row), and the
    /// deinit-carrying structs stay out (C5: a linked deinit body is the
    /// artifact round's member machinery). The map's keys are exactly
    /// the candidates whose fact survived.
    #[must_use]
    pub fn member_facts(
        analyzed: &crate::type_of::Analyzed,
        ambient_files: usize,
        carried: &std::collections::HashMap<String, u32, std::hash::RandomState>,
    ) -> FxHashMap<crate::DefId, Arc<Self>> {
        let carried: FxHashMap<String, u32> =
            carried.iter().map(|(k, v)| (k.clone(), *v)).collect();
        let carried = &carried;
        let package = &analyzed.package;
        let decls = &analyzed.decls;
        let mut deinit = std::collections::HashSet::new();
        for ast in analyzed.asts.iter().skip(ambient_files) {
            for block in &ast.blocks {
                for item in &block.items {
                    let ds_parse::ItemKind::Struct(decl) = &item.kind else {
                        continue;
                    };
                    if decl
                        .members
                        .iter()
                        .any(|member| matches!(member, ds_parse::StructMember::Deinit { .. }))
                        && let Some(def) = package.lookup_type(&decl.name.name)
                    {
                        deinit.insert(def.index());
                    }
                }
            }
        }
        // The trait items keyed by their def: a trait's fact reads the
        // declaration itself (its written member rows), which no decls
        // table carries whole.
        let mut traits: FxHashMap<crate::DefId, &ds_parse::TraitDecl> = FxHashMap::default();
        for ast in analyzed.asts.iter().skip(ambient_files) {
            for block in &ast.blocks {
                for item in &block.items {
                    let ds_parse::ItemKind::Trait(t) = &item.kind else {
                        continue;
                    };
                    if let Some(def) = package.lookup_type(&t.name.name)
                        && package.def(def).span == t.name.span
                    {
                        traits.insert(def, t);
                    }
                }
            }
        }
        let mut candidates = Vec::new();
        for (index, def) in package.defs().enumerate() {
            if (def.span.file.as_u32() as usize) < ambient_files {
                continue; // the ambient's own definitions never export
            }
            if !matches!(
                def.kind,
                crate::DefKind::Struct
                    | crate::DefKind::Enum
                    | crate::DefKind::Case
                    | crate::DefKind::TypeAlias
                    | crate::DefKind::Tag
                    | crate::DefKind::Trait
            ) {
                continue;
            }
            let id = crate::DefId::from_raw(u32::try_from(index).expect("def count fits u32"));
            if package.foreign().get(&id).is_some() {
                continue; // an import-bound def rides its own re-export row
            }
            candidates.push(id);
        }
        let mut eligible: FxHashSet<crate::DefId> = candidates.iter().copied().collect();
        loop {
            let mut facts = FxHashMap::default();
            let mut next = FxHashSet::default();
            for &def in &candidates {
                if let Some(fact) = Self::of_def(
                    package,
                    decls,
                    &deinit,
                    &eligible,
                    traits.get(&def).copied(),
                    carried,
                    def,
                ) {
                    next.insert(def);
                    facts.insert(def, Arc::new(fact));
                }
            }
            if next == eligible {
                return facts;
            }
            eligible = next;
        }
    }

    /// One declaration's fact over the eligible set: the kind dispatch
    /// the export walk owns, the struct deinit gate included (a linked
    /// deinit body is the artifact round's member machinery).
    fn of_def(
        package: &crate::resolve::Package,
        decls: &crate::Decls,
        deinit: &std::collections::HashSet<u32>,
        eligible: &FxHashSet<crate::DefId>,
        trait_decl: Option<&ds_parse::TraitDecl>,
        carried: &FxHashMap<String, u32>,
        def: crate::DefId,
    ) -> Option<Self> {
        match package.def(def).kind {
            crate::DefKind::Struct if !deinit.contains(&def.index()) => {
                Self::struct_fact(package, decls, eligible, def)
            },
            crate::DefKind::Enum => Self::enum_fact(package, decls, eligible, def),
            crate::DefKind::Case => {
                let enum_name = package.def(decls.cases.get(&def)?.enum_def).name.clone();
                let payload = WordSolver {
                    package,
                    decls,
                    eligible,
                    file: package.def(def).span.file,
                }
                .case_payload(def)?;
                Some(Self::Case(CaseFact { enum_name, payload }))
            },
            crate::DefKind::TypeAlias => {
                Self::alias_fact(package, decls, eligible, package.def(def).span.file, def)
            },
            crate::DefKind::Tag => Self::tag_fact(decls, def),
            crate::DefKind::Trait => Self::trait_fact(trait_decl?, carried),
            _ => None,
        }
    }

    /// The fact one declared trait carries, when it stays plain: no
    /// generic parameters, no supertraits, and no associated types.
    /// Every row carries the written signature the member's registered
    /// method id already holds; a member declaring a default body
    /// carries the body's table index when the body crossed — the
    /// carried table names exactly the bodies that plan and stage no
    /// runtime evidence, so a body the walk refused keeps its member's
    /// row bodiless and the consumer's impls must spell it.
    fn trait_fact(t: &ds_parse::TraitDecl, carried: &FxHashMap<String, u32>) -> Option<Self> {
        if !t.generics.is_empty() || !t.supers.is_empty() {
            return None;
        }
        let mut members = Vec::with_capacity(t.members.len());
        for member in &t.members {
            let ds_parse::TraitMember::Fn(f) = member else {
                return None; // an associated type is member machinery of its own
            };
            let default_func = f
                .body
                .is_some()
                .then(|| {
                    carried
                        .get(&format!("{}#{}", t.name.name, f.name.name))
                        .copied()
                })
                .flatten();
            members.push(TraitMemberFact {
                name: f.name.name.clone(),
                span: f.name.span,
                sig: Arc::new(FnSig::of(f)),
                default_func,
            });
        }
        Some(Self::Trait(TraitFact { members }))
    }

    /// The fact one declared struct carries, when its whole shape
    /// spells in the vocabulary: non-generic, every field a word the
    /// vocabulary carries, the declaration's linearity riding beside
    /// them. A bodiless declaration (its atom still shape-pending)
    /// carries the opaque identity alone. Everything else — a generic
    /// parameterization, a field the vocabulary cannot spell — answers
    /// `None` and keeps its selection refused.
    fn struct_fact(
        package: &crate::resolve::Package,
        decls: &crate::Decls,
        eligible: &FxHashSet<crate::DefId>,
        def: crate::DefId,
    ) -> Option<Self> {
        let shape = decls.structs.get(&def)?;
        if !shape.params.is_empty() {
            return None;
        }
        let atom = decls.atoms.get(&def)?;
        let fields = match decls.universe.atom_decl(*atom).ok()?.kind {
            crate::AtomKind::Struct(ref fields) => fields.clone(),
            // the bodiless declaration: identity reserved, no layout —
            // the atom stays shape-pending exactly like a local spelling
            crate::AtomKind::Reserved => return Some(Self::Opaque),
            _ => return None,
        };
        let solver = WordSolver {
            package,
            decls,
            eligible,
            file: package.def(def).span.file,
        };
        let mut out = Vec::with_capacity(fields.len());
        for field in fields {
            out.push(TypeField {
                name: field.name.clone()?,
                word: solver.word_of_expr(&field.ty)?,
            });
        }
        Some(Self::Struct {
            fields: out,
            linear: shape.linear,
        })
    }

    /// The fact one declared enum carries, when every case spells in
    /// the vocabulary: non-generic, every payload a unit, a struct
    /// payload of word-shaped fields, or a tuple payload of
    /// word-shaped positional members; an empty enum rides its empty
    /// rows. Every other shape answers `None` and keeps its selection
    /// refused.
    fn enum_fact(
        package: &crate::resolve::Package,
        decls: &crate::Decls,
        eligible: &FxHashSet<crate::DefId>,
        def: crate::DefId,
    ) -> Option<Self> {
        let shape = decls.enums.get(&def)?;
        if !shape.params.is_empty() {
            return None;
        }
        let solver = WordSolver {
            package,
            decls,
            eligible,
            file: package.def(def).span.file,
        };
        let mut out = Vec::with_capacity(shape.cases.len());
        for (name, ..) in &shape.cases {
            let case_def = package.lookup_type(name)?;
            out.push(TypeCase {
                name: name.clone(),
                payload: solver.case_payload(case_def)?,
            });
        }
        Some(Self::Enum {
            cases: out,
            linear: shape.linear,
        })
    }

    /// The fact one declared alias carries, when its body spells in
    /// the member's own linkable vocabulary: non-generic (an alias with
    /// parameters resolves through use-site args and is no single
    /// word), its written body resolving through the same word solver
    /// a struct field's type does — the ambient spellings, an alias
    /// chain (transparently expanded, visit-stack-guarded), a declared
    /// type of the same member as the Named word, and a union body over
    /// words. A tuple body and a generic alias keep their refusals.
    fn alias_fact(
        package: &crate::resolve::Package,
        decls: &crate::Decls,
        eligible: &FxHashSet<crate::DefId>,
        file: ds_diag::FileId,
        def: crate::DefId,
    ) -> Option<Self> {
        let solver = WordSolver {
            package,
            decls,
            eligible,
            file,
        };
        let (params, body) = decls.aliases.get(&def)?;
        if !params.is_empty() {
            return None;
        }
        Some(Self::Alias(Box::new(solver.written_word(
            body,
            false,
            &mut Vec::new(),
        )?)))
    }

    /// The fact one declared tag carries: the unit atom's identity
    /// alone. A tag restating the predeclared `none` (the universe's
    /// distinguished unit atom) carries no fact — the consuming
    /// compile's own `none` is that same atom, so the selection keeps
    /// the reserved-name refusal the walk already spells.
    fn tag_fact(decls: &crate::Decls, def: crate::DefId) -> Option<Self> {
        let atom = decls.atoms.get(&def).copied()?;
        match decls.universe.atom_decl(atom).ok()?.kind {
            crate::AtomKind::Unit if atom != crate::TypeId::NONE => Some(Self::Tag),
            _ => None,
        }
    }
}

impl ImplFact {
    /// The member link facts for every plain trait impl one package
    /// compile declares: one row per impl item, the head one word of
    /// the vocabulary and each bodied member's index in the linked
    /// module's own table. A generic impl, a conditional impl, an impl
    /// of a trait this member did not itself declare (whose fact then
    /// refused), and an impl whose members did not all plan (a member
    /// body over the impl's own variables) carry no row and keep the
    /// consumer's dispatch refused.
    #[must_use]
    pub fn member_facts(
        analyzed: &crate::type_of::Analyzed,
        ambient_files: usize,
        functions: &[String],
        eligible: &std::collections::HashSet<crate::DefId>,
        traits: &FxHashMap<crate::DefId, Arc<TypeFact>>,
    ) -> Vec<Self> {
        let package = &analyzed.package;
        let decls = &analyzed.decls;
        let eligible: &FxHashSet<crate::DefId> = &eligible.iter().copied().collect();
        let mut out = Vec::new();
        for ast in analyzed.asts.iter().skip(ambient_files) {
            for block in &ast.blocks {
                for item in &block.items {
                    let ds_parse::ItemKind::Impl(imp) = &item.kind else {
                        continue;
                    };
                    let Some(trait_ref) = &imp.trait_ref else {
                        continue; // an inherent impl is the type-keyed seam's own round
                    };
                    let [only] = trait_ref.path.segments.as_slice() else {
                        continue;
                    };
                    if !imp.generics.is_empty() {
                        continue;
                    }
                    let Some(def) = package
                        .lookup_type(&only.name)
                        .filter(|&def| {
                            package.foreign().get(&def).is_none()
                                && (package.def(def).span.file.as_u32() as usize) >= ambient_files
                        })
                        .filter(|&def| traits.contains_key(&def))
                    else {
                        continue;
                    };
                    // the implemented trait must have crossed itself:
                    // its member rows are the rows a consumer checks
                    // against, so an impl of a refused trait rides no
                    // fact
                    if !matches!(&*traits[&def], TypeFact::Trait(_)) {
                        continue;
                    }
                    // one instance per impl item, its written head one
                    // word over the eligible vocabulary — a conditional
                    // impl dispatches through condition evidence the
                    // consumer cannot stage
                    let mut heads = Vec::new();
                    for (instance, site) in &decls.instance_impls {
                        if *site != item.span {
                            continue;
                        }
                        if decls
                            .instances
                            .conditions(*instance)
                            .is_ok_and(|conditions| !conditions.is_empty())
                        {
                            heads.clear();
                            break;
                        }
                        let Ok(written) = decls.instances.written(*instance) else {
                            continue;
                        };
                        let Some(word) = WordSolver {
                            package,
                            decls,
                            eligible,
                            file: item.span.file,
                        }
                        .word_of_ty(written) else {
                            continue;
                        };
                        heads.push(word);
                    }
                    if heads.is_empty() {
                        continue;
                    }
                    let mut members = Vec::with_capacity(imp.fns.len());
                    let mut planned = true;
                    for f in &imp.fns {
                        let name = format!(
                            "{}#{}#{}",
                            only.name,
                            crate::decls::render_type(&imp.target),
                            f.name.name
                        );
                        let Some(func) = functions.iter().position(|row| *row == name) else {
                            planned = false;
                            break;
                        };
                        members.push(ImplMemberFact {
                            name: f.name.name.clone(),
                            func: u32::try_from(func).expect("function index fits u32"),
                        });
                    }
                    if !planned {
                        continue;
                    }
                    for head in heads {
                        out.push(Self {
                            trait_name: only.name.clone(),
                            head,
                            members: members.clone(),
                            span: item.span,
                            link: 0,
                        });
                    }
                }
            }
        }
        out
    }
}

/// One exported trait member whose declared default body the
/// producer's walk refuses to carry: the member's own span and the
/// seam that named it.
#[derive(Clone, Debug)]
pub struct RefusedDefault {
    /// The member's name occurrence, the primary site the producer's
    /// own diagnostic renders.
    pub span: Span,
    /// The declaring trait's name.
    pub trait_name: String,
    /// The member's name.
    pub member: String,
    /// The seam the body reached: why the crossing refused.
    pub seam: &'static str,
}

/// Every `pub` trait member of the member files whose declared default
/// body cannot cross the link seam: a body the module never planned,
/// a body staging runtime evidence (its nested dispatches ride the
/// evidence-chain machinery a consumer cannot build over the
/// producer's bodies), or a body carrying an effect row the
/// consumer's dispatch would never union. The producer's own compile
/// refuses on these — a shape the surface cannot honestly serve fails
/// before any consumer sees it, never as a trap at a dispatch site.
#[must_use]
pub fn refused_default_bodies(
    analyzed: &crate::type_of::Analyzed,
    ambient_files: usize,
    effects: &crate::effects::Effects,
    functions: &[String],
    carried: &std::collections::HashMap<String, u32, std::hash::RandomState>,
) -> Vec<RefusedDefault> {
    let package = &analyzed.package;
    let mut out = Vec::new();
    for ast in analyzed.asts.iter().skip(ambient_files) {
        for block in &ast.blocks {
            for item in &block.items {
                let ds_parse::ItemKind::Trait(t) = &item.kind else {
                    continue;
                };
                let Some(def) = package
                    .lookup_type(&t.name.name)
                    .filter(|&def| package.def(def).span == t.name.span)
                else {
                    continue;
                };
                if package.def(def).vis != Vis::Pub {
                    continue; // a private trait never serves a consumer
                }
                for member in &t.members {
                    let ds_parse::TraitMember::Fn(f) = member else {
                        continue;
                    };
                    let Some(body) = &f.body else {
                        continue;
                    };
                    let mangle = format!("{}#{}", t.name.name, f.name.name);
                    let seam = if !carried.contains_key(&mangle) {
                        if functions.contains(&mangle) {
                            "the body stages runtime evidence a consumer cannot build over the \
                             producer's bodies"
                        } else {
                            "the body did not plan in this module"
                        }
                    } else if effects.rows_under(body.span).any(|row| !row.is_pure()) {
                        "the body carries an effect row the consumer's dispatch would never union"
                    } else {
                        continue;
                    };
                    out.push(RefusedDefault {
                        span: f.name.span,
                        trait_name: t.name.name.clone(),
                        member: f.name.name.clone(),
                        seam,
                    });
                }
            }
        }
    }
    out
}

/// One fact build's word resolver: the ambient vocabulary plus the
/// in-word references to the other linked declared types of the same
/// member, gated by the co-derivation fixpoint's eligible set.
struct WordSolver<'a> {
    /// The producer compile's package: the def kinds and spans the
    /// gates read, and the import-bound rows and re-export records a
    /// Named reference resolves through.
    package: &'a crate::resolve::Package,
    /// The producer compile's solved declarations.
    decls: &'a crate::Decls,
    /// The fixpoint's current eligible set: the declarations a Named
    /// reference may name.
    eligible: &'a FxHashSet<crate::DefId>,
    /// The file of the declaration whose fact is being built: the
    /// same-member gate every Named reference passes (the surface
    /// serves one member's own definitions and its own `pub import`
    /// rows, so a cross-member reference would name no row).
    file: ds_diag::FileId,
}

impl WordSolver<'_> {
    /// One solved field type's word kind, when it spells in the
    /// vocabulary: a bare atom, a union — `t | none` either order is
    /// exactly `t?` (B4), and any other union is one enum's complete
    /// case set or a union of words — or a tuple, one word per
    /// positional member.
    fn word_of_expr(&self, expr: &crate::TypeExpr) -> Option<TypeWord> {
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
    fn word_of_ty(&self, ty: crate::Ty) -> Option<TypeWord> {
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
    /// `t?` (B4) — the optional over the other side's whole word — any
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
    /// spelling names the enum, whose solving rebuilds this union [B3].
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
    fn case_payload(&self, def: crate::DefId) -> Option<CasePayload> {
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
    /// optional rule (B4). Every other written shape — a tuple, a fn
    /// type, a multi-segment path, an applied alias — answers `None`.
    fn written_word(
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
    /// the optional (B4), and every other member carries one word — a
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

/// One import declaration's linked surface, keyed by the declaration's
/// span in [`ImportGraph`].
#[derive(Clone, Debug)]
pub enum ImportSurface {
    /// An `@std` path: the compile unit's merged ambient namespace
    /// serves (selections validate against it, names already visible),
    /// and a module alias resolves through `member`'s declarations.
    Merged {
        /// The merged member's file id inside the consuming compile.
        member: ds_diag::FileId,
    },
    /// A linked package member's exported surface: every top-level
    /// definition the named member declares, with the link facts a
    /// binding call site lowers through.
    Linked(MemberSurface),
    /// The path names no member of the linked package: every selection
    /// refuses naming the member segment.
    NoSuchMember {
        /// The offending member segment, for the diagnostic's wording.
        member: String,
    },
}

/// One linked member's exported surface.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct MemberSurface {
    /// The member's top-level definitions, declaration order.
    pub entries: Vec<ForeignEntry>,
    /// The member's declared trait impls, one row per impl item: the
    /// owner-keyed member link facts a consuming trait call site
    /// dispatches through, each member body's index addressing this
    /// member's own module table at the surface's link position.
    pub impls: Vec<ImplFact>,
    /// The linked module's tag-table rows, the declaring declaration's
    /// name beside the label and the id: the shared tag mapping's
    /// identity. A union-typed word crossing the module boundary
    /// carries the producing module's tag ids as immediates,
    /// so the consuming compile's own table must reserve each row's id
    /// for the atom the linked facts mint under the same name — the
    /// seed runs at lowering, before any body mints. Rows the consuming
    /// compile mints no atom for (the producer's private atoms) ride
    /// verbatim, consuming their ids.
    pub tags: Vec<ds_mod::MemberTag>,
}

/// One linked member's top-level definitions, the import-side shape: the
/// declared tier, the written signature a call checks against, the
/// solved effect row the caller unions, and the link facts the call
/// lowering addresses.
#[derive(Clone, Debug)]
pub struct ForeignEntry {
    // PartialEq ignores the signature: equality is the linked fact set
    // (name, tier, shape marks, span, row, link facts), which one source
    // text determines; the signature rides the surface to the decls
    // table and is never compared.
    /// The declared name.
    pub name: String,
    /// The declared visibility tier.
    pub vis: Vis,
    /// What the declaration defines.
    pub kind: ForeignKind,
    /// A generic declaration: its one body is a per-instantiation fact
    /// the linked module serves, so it refuses selection.
    pub generic: bool,
    /// A defaulted parameter: its literal stages at the linked module's
    /// own call sites, so it refuses selection.
    pub defaulted: bool,
    /// The defining name occurrence, rebased into the consuming compile's
    /// file space so two-span diagnostics render.
    pub decl_span: Span,
    /// The written signature, fn declarations only.
    pub sig: Option<Arc<FnSig>>,
    /// The definition's solved effect row.
    pub row: EffectRow,
    /// The definition's world-capability bits.
    pub caps: u8,
    /// The literal fact a `const` binding carries: the value itself, so
    /// selection sites embed it with no linked-module machinery. `None`
    /// for every other kind and for a computed initializer.
    pub value: Option<ConstFact>,
    /// The structural fact a type declaration carries: the word layout
    /// the consuming compile mints its own def from. `None` for every
    /// other kind and for every shape the vocabulary cannot spell.
    pub ty: Option<Arc<TypeFact>>,
    /// The linked dep entry's position in the consuming module's
    /// recorded list. For a re-exported entry (`pub import`) this is the
    /// ORIGIN package's position — the body lives in the origin's
    /// module, so the call's FuncRef addresses it there — while a direct
    /// member's position names the linked package itself.
    pub link: u32,
    /// The origin package's link name when this entry re-exports
    /// transitively (a `pub import` row of a package that itself
    /// imported the member): the name the next consumer's dep-list walk
    /// records for the entry's `link` position, so the closure composes
    /// over package chains. `None` for a direct member of the linked
    /// package.
    pub origin: Option<String>,
    /// The function's index inside the linked module's table; `None`
    /// for a bodiless declaration (an extern), whose callee is the
    /// native the consumer's own lowering answers by name — the
    /// compiler-wired criterion — never a linked body.
    pub func: Option<u32>,
    /// The entry links the reserved `@std` package: the compiler-wired
    /// identity the @std spellings and recognitions key on — this round
    /// the package's first member is the ambient core itself (the
    /// migrated members riding beside it), and a declaring compile
    /// registers that same core as its ambient, so a word-shaped atom
    /// head the consuming ambient also declares is the same declaration,
    /// and the lowerer's compiler-wired recognition keeps serving its
    /// @std-linked defs. Every other linked package's entries carry
    /// `false` and their own identities alone.
    pub std: bool,
}

/// What one linked entry declares.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ForeignKind {
    /// A function.
    Fn,
    /// A const or var binding.
    Binding,
    /// A type-namespace declaration.
    Type,
}

impl ForeignKind {
    /// The diagnostic word for the kind.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Fn => "function",
            Self::Binding => "binding",
            Self::Type => "type",
        }
    }
}

impl MemberSurface {
    /// The entry one selection or qualified member names, if any.
    #[must_use]
    pub fn entry(&self, name: &str) -> Option<&ForeignEntry> {
        self.entries.iter().find(|entry| entry.name == name)
    }
}

/// The import surfaces one compile binds through: one entry per import
/// declaration the caller linked, keyed by the declaration's span, plus
/// the link-name walk the surfaces' positions and the module's dep list
/// both derive from — the source's own import names in first-occurrence
/// order, then each surface's re-export origins as the walk met them
/// (`pub import`), so a transitive entry's FuncRef addresses the origin
/// package the same dep list names. The graph also carries the run's
/// bound artifacts (H5): each `[artifacts]` name the manifest graph
/// resolved mapped to its installed library's exported C symbols in
/// enumeration order — the fold a bound extern's declaration resolves
/// through, exactly the table the run installs as the artifact's native
/// module.
#[derive(Clone, Debug, Default)]
pub struct ImportGraph {
    surfaces: FxHashMap<Span, ImportSurface>,
    link_order: Vec<String>,
    artifacts: std::collections::BTreeMap<String, Arc<Vec<String>>>,
}

impl ImportGraph {
    /// No surfaces: every import stays declarative.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds one import declaration's surface.
    pub fn add(&mut self, span: Span, surface: ImportSurface) {
        self.surfaces.insert(span, surface);
    }

    /// Adds one resolved artifact's exported-symbol table, in
    /// enumeration order (the same sorted report `enumerate` answers).
    pub fn add_artifact(&mut self, name: &str, symbols: Vec<String>) {
        self.artifacts.insert(name.to_owned(), Arc::new(symbols));
    }

    /// One import declaration's surface.
    #[must_use]
    pub fn get(&self, span: Span) -> Option<&ImportSurface> {
        self.surfaces.get(&span)
    }

    /// The resolved artifacts' exported-symbol tables.
    #[must_use]
    pub fn artifacts(&self) -> &std::collections::BTreeMap<String, Arc<Vec<String>>> {
        &self.artifacts
    }

    /// Whether nothing linked.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.surfaces.is_empty()
    }

    /// Records the link-name walk the surfaces' positions assigned: the
    /// ordered names the dep-list writer reuses, so the two never drift.
    pub fn set_link_order(&mut self, order: Vec<String>) {
        self.link_order = order;
    }

    /// The ordered link names the surfaces' positions assigned, the
    /// dep-list writer's walk: empty until the builder that assigned the
    /// positions records it.
    #[must_use]
    pub fn link_order(&self) -> &[String] {
        &self.link_order
    }
}

impl PartialEq for ForeignEntry {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
            && self.vis == other.vis
            && self.kind == other.kind
            && self.generic == other.generic
            && self.defaulted == other.defaulted
            && self.decl_span == other.decl_span
            && self.row == other.row
            && self.caps == other.caps
            && self.value == other.value
            && self.ty == other.ty
            && self.link == other.link
            && self.origin == other.origin
            && self.func == other.func
            && self.std == other.std
    }
}

impl Eq for ForeignEntry {}

#[cfg(test)]
mod tests {
    use super::ConstFact;

    fn literal(source: &str) -> Option<ConstFact> {
        let mut files = ds_diag::Files::new();
        let file = files.add_source("const_fact.ds", source);
        let ast = ds_parse::parse(&files, file).expect("the source parses");
        let Some(ds_parse::ItemKind::Local(local)) = ast
            .blocks
            .iter()
            .flat_map(|block| block.items.iter())
            .next()
            .map(|item| &item.kind)
        else {
            panic!("the fixture declares a binding");
        };
        ConstFact::of_expr(&local.init)
    }

    /// The literal vocabulary carries: every scalar atom's plain literal,
    /// parenthesized or unary-negated numerics, radix-prefixed ints, and
    /// the two string spellings; everything computed, interpolated,
    /// suffixed, or list-shaped carries nothing — and the int-min literal
    /// (whose operand alone overflows the word) is not spellable, matching
    /// the owning module's own literal lowering.
    #[test]
    fn the_fact_is_the_plain_literal() {
        assert_eq!(literal("const a = 100"), Some(ConstFact::Int(100)));
        assert_eq!(literal("const a = 0xff"), Some(ConstFact::Int(255)));
        assert_eq!(literal("const a = (7)"), Some(ConstFact::Int(7)));
        assert_eq!(literal("const a = -5"), Some(ConstFact::Int(-5)));
        assert_eq!(literal("const a = 1.5"), Some(ConstFact::Float(1.5)));
        assert_eq!(literal("const a = -0.25"), Some(ConstFact::Float(-0.25)));
        assert_eq!(literal("const a = true"), Some(ConstFact::Bool(true)));
        assert_eq!(literal("const a = 'x'"), Some(ConstFact::Char('x')));
        assert_eq!(literal("const a = b'x'"), Some(ConstFact::Byte(b'x')));
        assert_eq!(
            literal("const a = \"hi\""),
            Some(ConstFact::Str("hi".to_owned()))
        );
        assert_eq!(
            literal("const a = r\"raw\""),
            Some(ConstFact::Str("raw".to_owned()))
        );
        // a list of one scalar word's plain literals carries, elements
        // verbatim in order
        assert_eq!(
            literal("const a = [1, 2]"),
            Some(ConstFact::List(vec![ConstFact::Int(1), ConstFact::Int(2)]))
        );
        assert_eq!(
            literal("const a = [\"x\", \"y\"]"),
            Some(ConstFact::List(vec![
                ConstFact::Str("x".to_owned()),
                ConstFact::Str("y".to_owned())
            ]))
        );
        // the refusals: computed, interpolated, suffixed, an empty or
        // nested or mixed list, the wrapping negation (a different
        // operation than the literal's own), and the unspellable int-min
        // operand
        assert_eq!(literal("const a = 2 * 3"), None);
        assert_eq!(literal("const a = \"n = ${n}\""), None);
        assert_eq!(literal("const a = 1i64"), None);
        assert_eq!(literal("const a = []"), None);
        assert_eq!(literal("const a = [1, \"a\"]"), None);
        assert_eq!(literal("const a = [[1]]"), None);
        assert_eq!(literal("const a = -%5"), None);
        assert_eq!(literal("const a = -other"), None);
        assert_eq!(literal("const a = -9223372036854775808"), None);
    }

    /// The structural vocabulary carries: a flat struct's named fields
    /// over every scalar word, `bytes`, and the container constructors
    /// closed over the vocabulary itself — a list of any word (a nested
    /// list included), a map of any two words, an optional over any word
    /// (an optional list included) — and the part-less struct, a linear
    /// declaration carrying its move-discipline bit beside the fields,
    /// a tag's unit identity, the Named word's identity reference, and
    /// a tuple field's one word per positional member; a generic struct
    /// carries nothing — it keeps its selection refusal.
    #[test]
    fn the_fact_is_the_flat_struct_s_word_layout() {
        let source = concat!(
            "pub trait Copyable {\n",
            "}\n",
            "pub tag int\n",
            "pub tag string\n",
            "pub tag byte\n",
            "pub tag u32\n",
            "pub tag bytes\n",
            "pub struct list[t] {\n",
            "}\n",
            "pub struct map[k, v] {\n",
            "}\n",
            "pub struct cell {\n",
            "  n: int,\n",
            "  label: string,\n",
            "  tags: list[string],\n",
            "  code: byte,\n",
            "  w: u32,\n",
            "  data: bytes,\n",
            "}\n",
            "pub struct keyed {\n",
            "  index: map[string, int],\n",
            "  seen: map[byte, byte],\n",
            "}\n",
            "pub struct opt {\n",
            "  n: int?,\n",
            "}\n",
            "pub struct bare {\n",
            "}\n",
            "pub struct boxed[t] {\n",
            "  v: t,\n",
            "}\n",
            "pub enum shade {\n",
            "  red,\n",
            "}\n",
            "pub struct holder {\n",
            "  c: cell,\n",
            "  maybe: cell?,\n",
            "}\n",
            "pub struct opt_list {\n",
            "  xs: list[int]?,\n",
            "}\n",
            "pub struct grid {\n",
            "  rows: list[list[byte]],\n",
            "}\n",
            "pub struct twin {\n",
            "  p: (int, int),\n",
            "}\n",
            "pub struct guard: ~Copyable {\n",
            "  n: int,\n",
            "}\n",
            "pub struct keyed_wide {\n",
            "  index: map[string, cell],\n",
            "}\n",
            "pub struct over_pair {\n",
            "  v: (int, twin),\n",
            "}\n",
            "pub struct unit_field {\n",
            "  u: (),\n",
            "}\n",
            "pub tag mark\n",
            "pub struct marked_pair {\n",
            "  m: (int, mark),\n",
            "}\n",
            "pub tag point\n",
            "fn main() -> int {\n",
            "7\n",
            "}\n",
        );
        let mut files = ds_diag::Files::new();
        let file = files.add_source("type_fact.ds", source);
        let ast = ds_parse::parse(&files, file).expect("the source parses");
        let analyzed = crate::type_of::analyze(&files, &[ast]);
        let facts =
            super::TypeFact::member_facts(&analyzed, 0, &std::collections::HashMap::default());
        let fact = |name: &str| {
            let def = analyzed
                .package
                .lookup_type(name)
                .unwrap_or_else(|| panic!("{name} declares"));
            facts.get(&def).map(|fact| (**fact).clone())
        };
        let fields = |fields| {
            Some(super::TypeFact::Struct {
                fields,
                linear: false,
            })
        };
        assert_eq!(
            fact("cell"),
            fields(vec![
                super::TypeField {
                    name: "n".to_owned(),
                    word: super::TypeWord::Scalar(super::ScalarWord::Int),
                },
                super::TypeField {
                    name: "label".to_owned(),
                    word: super::TypeWord::Scalar(super::ScalarWord::Str),
                },
                super::TypeField {
                    name: "tags".to_owned(),
                    word: super::TypeWord::List(Box::new(super::TypeWord::Scalar(
                        super::ScalarWord::Str,
                    ))),
                },
                super::TypeField {
                    name: "code".to_owned(),
                    word: super::TypeWord::Scalar(super::ScalarWord::Byte),
                },
                // the widened width rides the shared word table's row,
                // crossing the walk the same edit the shape layer maps
                // its slot in
                super::TypeField {
                    name: "w".to_owned(),
                    word: super::TypeWord::Scalar(super::ScalarWord::U32),
                },
                super::TypeField {
                    name: "data".to_owned(),
                    word: super::TypeWord::Bytes,
                },
            ])
        );
        assert_eq!(fact("bare"), fields(Vec::new()));
        assert_eq!(
            fact("keyed"),
            fields(vec![
                super::TypeField {
                    name: "index".to_owned(),
                    word: super::TypeWord::Map(
                        Box::new(super::TypeWord::Scalar(super::ScalarWord::Str)),
                        Box::new(super::TypeWord::Scalar(super::ScalarWord::Int)),
                    ),
                },
                super::TypeField {
                    name: "seen".to_owned(),
                    word: super::TypeWord::Map(
                        Box::new(super::TypeWord::Scalar(super::ScalarWord::Byte)),
                        Box::new(super::TypeWord::Scalar(super::ScalarWord::Byte)),
                    ),
                },
            ])
        );
        assert_eq!(
            fact("opt"),
            fields(vec![super::TypeField {
                name: "n".to_owned(),
                word: super::TypeWord::Optional(Box::new(super::TypeWord::Scalar(
                    super::ScalarWord::Int,
                ))),
            }])
        );
        // the container constructors close over the vocabulary itself: an
        // optional over a list, a list over a list
        assert_eq!(
            fact("opt_list"),
            fields(vec![super::TypeField {
                name: "xs".to_owned(),
                word: super::TypeWord::Optional(Box::new(super::TypeWord::List(Box::new(
                    super::TypeWord::Scalar(super::ScalarWord::Int),
                )))),
            }])
        );
        assert_eq!(
            fact("grid"),
            fields(vec![super::TypeField {
                name: "rows".to_owned(),
                word: super::TypeWord::List(Box::new(super::TypeWord::List(Box::new(
                    super::TypeWord::Scalar(super::ScalarWord::Byte),
                )))),
            }])
        );
        assert_eq!(
            fact("guard"),
            Some(super::TypeFact::Struct {
                fields: vec![super::TypeField {
                    name: "n".to_owned(),
                    word: super::TypeWord::Scalar(super::ScalarWord::Int),
                }],
                linear: true,
            }),
            "a linear declaration carries its move-discipline bit"
        );
        // the Named word: a field naming the member's own declared
        // types — a nested struct, an optional over it, a container
        // over it — carries as the identity reference
        assert_eq!(
            fact("holder"),
            fields(vec![
                super::TypeField {
                    name: "c".to_owned(),
                    word: super::TypeWord::Named("cell".to_owned()),
                },
                super::TypeField {
                    name: "maybe".to_owned(),
                    word: super::TypeWord::Optional(Box::new(super::TypeWord::Named(
                        "cell".to_owned(),
                    ))),
                },
            ])
        );
        assert_eq!(
            fact("keyed_wide"),
            fields(vec![super::TypeField {
                name: "index".to_owned(),
                word: super::TypeWord::Map(
                    Box::new(super::TypeWord::Scalar(super::ScalarWord::Str)),
                    Box::new(super::TypeWord::Named("cell".to_owned())),
                ),
            }])
        );
        assert_eq!(fact("point"), Some(super::TypeFact::Tag));
        // a tuple field carries one word per positional member
        assert_eq!(
            fact("twin"),
            fields(vec![super::TypeField {
                name: "p".to_owned(),
                word: super::TypeWord::Tuple(vec![
                    super::TypeWord::Scalar(super::ScalarWord::Int),
                    super::TypeWord::Scalar(super::ScalarWord::Int),
                ]),
            }])
        );
        for refused in ["boxed", "list"] {
            assert_eq!(fact(refused), None, "{refused} carries no fact");
        }
        // a Named tuple member rides like any word — the consumer's own
        // lowering owns the multi-word element refusal, exactly like the
        // local spelling's
        assert_eq!(
            fact("over_pair"),
            fields(vec![super::TypeField {
                name: "v".to_owned(),
                word: super::TypeWord::Tuple(vec![
                    super::TypeWord::Scalar(super::ScalarWord::Int),
                    super::TypeWord::Named("twin".to_owned()),
                ]),
            }])
        );
        // the empty tuple (the arity-0 atom, the lone unit-typed word's
        // own seam) and a unit-typed tuple member keep their refusals
        for refused in ["unit_field", "marked_pair"] {
            assert_eq!(fact(refused), None, "{refused} carries no fact");
        }
    }

    /// The Named word's own gates: a reference naming a def outside the
    /// member (here every def is one member, so the gate rides on the
    /// eligible set instead) and a type whose own fact refuses (a
    /// generic struct, a deinit carrier) spell no word — the
    /// referencing fact refuses with them, while a reference to a
    /// tuple-carrying struct rides like any Named word. A lone case
    /// field keeps its refusal (a unit word spells only inside a
    /// union), while a multi-atom case subset carries the union word.
    #[test]
    fn the_named_word_gates_on_the_referenced_fact() {
        let source = concat!(
            "pub trait Copyable {\n",
            "}\n",
            "pub tag int\n",
            "pub tag string\n",
            "pub struct list[t] {\n",
            "}\n",
            "pub struct cell {\n",
            "  n: int,\n",
            "}\n",
            "pub struct boxed[t] {\n",
            "  v: t,\n",
            "}\n",
            "pub struct owned: ~Copyable {\n",
            "  n: int,\n",
            "  deinit { }\n",
            "}\n",
            "pub struct twin {\n",
            "  p: (int, int),\n",
            "}\n",
            "pub enum shade {\n",
            "  red,\n",
            "  green,\n",
            "  blue,\n",
            "}\n",
            "pub enum pick[t] {\n",
            "  one { v: t },\n",
            "}\n",
            "pub struct over_boxed {\n",
            "  v: boxed[int],\n",
            "}\n",
            "pub struct over_owned {\n",
            "  v: owned,\n",
            "}\n",
            "pub struct over_twin {\n",
            "  v: twin,\n",
            "}\n",
            "pub struct over_pick {\n",
            "  v: pick[int],\n",
            "}\n",
            "pub struct subset {\n",
            "  v: red,\n",
            "}\n",
            "pub struct partial {\n",
            "  pick: red | green,\n",
            "  mixed: red | int,\n",
            "}\n",
            "pub struct tinted {\n",
            "  shade: shade,\n",
            "  all: list[shade],\n",
            "  maybe: shade?,\n",
            "}\n",
            "pub struct wrap {\n",
            "  inner: cell,\n",
            "}\n",
            "pub struct nest {\n",
            "  w: wrap,\n",
            "}\n",
            "pub struct pair {\n",
            "  a: nest,\n",
            "  b: cell,\n",
            "}\n",
            "fn main() -> int {\n",
            "7\n",
            "}\n",
        );
        let mut files = ds_diag::Files::new();
        let file = files.add_source("named_gate.ds", source);
        let ast = ds_parse::parse(&files, file).expect("the source parses");
        let analyzed = crate::type_of::analyze(&files, &[ast]);
        let facts =
            super::TypeFact::member_facts(&analyzed, 0, &std::collections::HashMap::default());
        let fact = |name: &str| {
            let def = analyzed
                .package
                .lookup_type(name)
                .unwrap_or_else(|| panic!("{name} declares"));
            facts.get(&def).map(|fact| (**fact).clone())
        };
        let named = |name: &str| super::TypeWord::Named(name.to_owned());
        let fields = |fields| {
            Some(super::TypeFact::Struct {
                fields,
                linear: false,
            })
        };
        // the referenced facts that refuse take the referencing fact
        // with them; a lone case field keeps its refusal
        for refused in ["over_boxed", "over_owned", "over_pick", "subset"] {
            assert_eq!(fact(refused), None, "{refused} carries no fact");
        }
        // a tuple field carries its positional words, so a reference to
        // the tuple-carrying struct rides like any Named word
        assert_eq!(
            fact("twin"),
            fields(vec![super::TypeField {
                name: "p".to_owned(),
                word: super::TypeWord::Tuple(vec![
                    super::TypeWord::Scalar(super::ScalarWord::Int),
                    super::TypeWord::Scalar(super::ScalarWord::Int),
                ]),
            }])
        );
        assert_eq!(
            fact("over_twin"),
            fields(vec![super::TypeField {
                name: "v".to_owned(),
                word: named("twin"),
            }])
        );
        // a partial case-subset union and a mixed case-scalar set carry
        // the union word, one word per member
        assert_eq!(
            fact("partial"),
            fields(vec![
                super::TypeField {
                    name: "pick".to_owned(),
                    word: super::TypeWord::Union(vec![named("red"), named("green"),]),
                },
                super::TypeField {
                    name: "mixed".to_owned(),
                    word: super::TypeWord::Union(vec![
                        named("red"),
                        super::TypeWord::Scalar(super::ScalarWord::Int),
                    ]),
                },
            ])
        );
        // a chain of Named references co-derives: nest names wrap names
        // cell, pair names both
        assert_eq!(
            fact("wrap"),
            fields(vec![super::TypeField {
                name: "inner".to_owned(),
                word: named("cell"),
            }])
        );
        assert_eq!(
            fact("nest"),
            fields(vec![super::TypeField {
                name: "w".to_owned(),
                word: named("wrap"),
            }])
        );
        assert_eq!(
            fact("pair"),
            fields(vec![
                super::TypeField {
                    name: "a".to_owned(),
                    word: named("nest"),
                },
                super::TypeField {
                    name: "b".to_owned(),
                    word: named("cell"),
                },
            ])
        );
        // an enum-typed field and its containers name the enum
        assert_eq!(
            fact("tinted"),
            fields(vec![
                super::TypeField {
                    name: "shade".to_owned(),
                    word: named("shade"),
                },
                super::TypeField {
                    name: "all".to_owned(),
                    word: super::TypeWord::List(Box::new(named("shade"))),
                },
                super::TypeField {
                    name: "maybe".to_owned(),
                    word: super::TypeWord::Optional(Box::new(named("shade"))),
                },
            ])
        );
    }

    /// The Named word's re-export gate: a field naming an import-bound
    /// type carries the word only when a `pub import` row of the same
    /// member re-exports the name — the surface row the consuming
    /// compile resolves the word through, the entry's own fact the
    /// origin package already derived — while a privately imported
    /// type names no row and keeps its refusal. The shape pass forces
    /// the referenced installs forward either way, so the referencing
    /// struct's universe fields carry the name instead of dropping it
    /// silently.
    #[test]
    fn the_named_word_gates_on_the_re_export_row() {
        let source = concat!(
            "pub tag int\n",
            "\n",
            "pub import @lib (cell)\n",
            "import @lib (seed)\n",
            "\n",
            "pub struct fleet {\n",
            "  head: cell,\n",
            "}\n",
            "\n",
            "pub struct sealed {\n",
            "  s: seed,\n",
            "}\n",
            "\n",
            "fn main() -> int {\n",
            "7\n",
            "}\n",
        );
        let mut files = ds_diag::Files::new();
        let file = files.add_source("reexport_gate.ds", source);
        let ast = ds_parse::parse(&files, file).expect("the source parses");
        let at = ds_diag::Span::new(file, 1, 1);
        let entry = |name: &str| crate::ForeignEntry {
            name: name.to_owned(),
            vis: crate::Vis::Pub,
            kind: crate::ForeignKind::Type,
            generic: false,
            defaulted: false,
            decl_span: at,
            sig: None,
            row: crate::EffectRow::default(),
            caps: 0,
            value: None,
            ty: Some(std::sync::Arc::new(super::TypeFact::Struct {
                fields: vec![super::TypeField {
                    name: "n".to_owned(),
                    word: super::TypeWord::Scalar(super::ScalarWord::Int),
                }],
                linear: false,
            })),
            link: 0,
            origin: None,
            func: None,
            std: false,
        };
        let surface = crate::MemberSurface {
            tags: Vec::new(),
            impls: Vec::new(),
            entries: vec![entry("cell"), entry("seed")],
        };
        let mut imports = crate::ImportGraph::new();
        for item in ast.blocks.iter().flat_map(|block| block.items.iter()) {
            if let ds_parse::ItemKind::Import(import) = &item.kind {
                imports.add(import.span, crate::ImportSurface::Linked(surface.clone()));
            }
        }
        let (package, resolutions, diags) = crate::resolve_package_linked(&[&ast], &imports);
        assert!(diags.is_empty(), "the rows bind clean: {diags:?}");
        let analyzed = crate::type_of::analyze_resolved(
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
        let facts =
            super::TypeFact::member_facts(&analyzed, 0, &std::collections::HashMap::default());
        let fact = |name: &str| {
            let def = analyzed
                .package
                .lookup_type(name)
                .unwrap_or_else(|| panic!("{name} declares"));
            facts.get(&def).map(|fact| (**fact).clone())
        };
        // the re-exported entry's name carries; the privately imported
        // one keeps the refusal
        assert_eq!(
            fact("fleet"),
            Some(super::TypeFact::Struct {
                fields: vec![super::TypeField {
                    name: "head".to_owned(),
                    word: super::TypeWord::Named("cell".to_owned()),
                }],
                linear: false,
            })
        );
        assert_eq!(fact("sealed"), None, "a private import names no row");
        // the forced installs shaped both structs' universe fields, so
        // the re-exported type's atom answers under its entry name
        let cell = analyzed
            .package
            .foreign()
            .iter()
            .find(|(_, entry)| entry.name == "cell")
            .map(|(def, _)| *def)
            .expect("cell registers");
        assert!(
            analyzed.decls.atoms.contains_key(&cell),
            "the re-exported type's install forced forward"
        );
        let fleet = analyzed
            .package
            .lookup_type("fleet")
            .expect("fleet declares");
        let fleet_atom = analyzed
            .decls
            .atoms
            .get(&fleet)
            .copied()
            .expect("fleet's atom declares");
        let crate::AtomKind::Struct(ref fields) = analyzed
            .decls
            .universe
            .atom_decl(fleet_atom)
            .expect("shapes")
            .kind
        else {
            panic!("fleet's atom carries its fields");
        };
        assert_eq!(fields.len(), 1, "the linked-typed field never drops");
    }

    /// The enum vocabulary carries: a non-generic enum's tag rows — unit
    /// cases, struct payloads of word-shaped fields (`bytes` included,
    /// two payloaded cases too since each row names its case, a nested
    /// container word included), and tuple payloads of word-shaped
    /// positional members — and each minted case its own fact naming its
    /// enum; a linear enum carries its move-discipline bit; a generic
    /// enum and an empty enum's refusal set carry nothing — those keep
    /// their selection refusals.
    #[test]
    fn the_fact_is_the_enum_s_tag_rows() {
        let source = concat!(
            "pub trait Copyable {\n",
            "}\n",
            "pub tag int\n",
            "pub tag string\n",
            "pub tag byte\n",
            "pub tag bytes\n",
            "pub struct list[t] {\n",
            "}\n",
            "pub enum shade {\n",
            "  red,\n",
            "  green { n: int, label: string },\n",
            "  blue { tags: list[byte], data: bytes },\n",
            "}\n",
            "pub enum pair[t] {\n",
            "  one { v: t },\n",
            "}\n",
            "pub enum bare {\n",
            "}\n",
            "pub enum guard: ~Copyable {\n",
            "  red,\n",
            "}\n",
            "pub enum trip {\n",
            "  one(int, int),\n",
            "  two(string, byte),\n",
            "}\n",
            "pub enum wide {\n",
            "  lone { v: list[list[byte]] },\n",
            "}\n",
            "fn main() -> int {\n",
            "7\n",
            "}\n",
        );
        let mut files = ds_diag::Files::new();
        let file = files.add_source("enum_fact.ds", source);
        let ast = ds_parse::parse(&files, file).expect("the source parses");
        let analyzed = crate::type_of::analyze(&files, &[ast]);
        let word = |name: &str, word: super::ScalarWord| super::TypeField {
            name: name.to_owned(),
            word: super::TypeWord::Scalar(word),
        };
        let green = vec![
            word("n", super::ScalarWord::Int),
            word("label", super::ScalarWord::Str),
        ];
        let blue = vec![
            super::TypeField {
                name: "tags".to_owned(),
                word: super::TypeWord::List(Box::new(super::TypeWord::Scalar(
                    super::ScalarWord::Byte,
                ))),
            },
            super::TypeField {
                name: "data".to_owned(),
                word: super::TypeWord::Bytes,
            },
        ];
        let facts =
            super::TypeFact::member_facts(&analyzed, 0, &std::collections::HashMap::default());
        let enum_fact = |name: &str| {
            let def = analyzed
                .package
                .lookup_type(name)
                .unwrap_or_else(|| panic!("{name} declares"));
            facts.get(&def).map(|fact| (**fact).clone())
        };
        let case_fact = |name: &str, _enum_name: &str| {
            let def = analyzed
                .package
                .lookup_type(name)
                .unwrap_or_else(|| panic!("{name} declares"));
            facts.get(&def).map(|fact| (**fact).clone())
        };
        let rows = |cases, linear| Some(super::TypeFact::Enum { cases, linear });
        assert_eq!(
            enum_fact("shade"),
            rows(
                vec![
                    super::TypeCase {
                        name: "red".to_owned(),
                        payload: super::CasePayload::Unit,
                    },
                    super::TypeCase {
                        name: "green".to_owned(),
                        payload: super::CasePayload::Struct(green.clone()),
                    },
                    super::TypeCase {
                        name: "blue".to_owned(),
                        payload: super::CasePayload::Struct(blue.clone()),
                    },
                ],
                false
            )
        );
        assert_eq!(
            case_fact("red", "shade"),
            Some(super::TypeFact::Case(super::CaseFact {
                enum_name: "shade".to_owned(),
                payload: super::CasePayload::Unit,
            }))
        );
        assert_eq!(
            case_fact("green", "shade"),
            Some(super::TypeFact::Case(super::CaseFact {
                enum_name: "shade".to_owned(),
                payload: super::CasePayload::Struct(green),
            }))
        );
        assert_eq!(
            case_fact("blue", "shade"),
            Some(super::TypeFact::Case(super::CaseFact {
                enum_name: "shade".to_owned(),
                payload: super::CasePayload::Struct(blue),
            }))
        );
        // an empty enum rides its empty rows; a tuple payload carries
        // its positional words; a linear enum carries its bit
        assert_eq!(enum_fact("bare"), rows(Vec::new(), false));
        assert_eq!(
            enum_fact("trip"),
            rows(
                vec![
                    super::TypeCase {
                        name: "one".to_owned(),
                        payload: super::CasePayload::Tuple(vec![
                            super::TypeWord::Scalar(super::ScalarWord::Int),
                            super::TypeWord::Scalar(super::ScalarWord::Int),
                        ]),
                    },
                    super::TypeCase {
                        name: "two".to_owned(),
                        payload: super::CasePayload::Tuple(vec![
                            super::TypeWord::Scalar(super::ScalarWord::Str),
                            super::TypeWord::Scalar(super::ScalarWord::Byte),
                        ]),
                    },
                ],
                false,
            )
        );
        assert_eq!(
            case_fact("one", "trip"),
            Some(super::TypeFact::Case(super::CaseFact {
                enum_name: "trip".to_owned(),
                payload: super::CasePayload::Tuple(vec![
                    super::TypeWord::Scalar(super::ScalarWord::Int),
                    super::TypeWord::Scalar(super::ScalarWord::Int),
                ]),
            })),
            "a tuple payload carries its positional words"
        );
        assert_eq!(
            enum_fact("guard"),
            rows(
                vec![super::TypeCase {
                    name: "red".to_owned(),
                    payload: super::CasePayload::Unit,
                }],
                true,
            ),
            "a linear enum carries its move-discipline bit"
        );
        assert_eq!(enum_fact("pair"), None, "pair carries no fact");
        // a payload field over the containers' own closure carries: a
        // nested list word rides like any other
        assert_eq!(
            case_fact("lone", "wide"),
            Some(super::TypeFact::Case(super::CaseFact {
                enum_name: "wide".to_owned(),
                payload: super::CasePayload::Struct(vec![super::TypeField {
                    name: "v".to_owned(),
                    word: super::TypeWord::List(Box::new(super::TypeWord::List(Box::new(
                        super::TypeWord::Scalar(super::ScalarWord::Byte),
                    )))),
                }]),
            }))
        );
    }

    /// A lone tag's fact is its unit atom's identity; a tag restating
    /// the predeclared `none` carries none.
    #[test]
    fn the_fact_is_the_tag_s_unit_atom() {
        let source = concat!(
            "pub tag mark\n",
            "pub tag none\n",
            "fn main() -> int {\n",
            "7\n",
            "}\n",
        );
        let mut files = ds_diag::Files::new();
        let file = files.add_source("tag_fact.ds", source);
        let ast = ds_parse::parse(&files, file).expect("the source parses");
        let analyzed = crate::type_of::analyze(&files, &[ast]);
        let facts =
            super::TypeFact::member_facts(&analyzed, 0, &std::collections::HashMap::default());
        let tag_fact = |name: &str| {
            let def = analyzed
                .package
                .lookup_type(name)
                .unwrap_or_else(|| panic!("{name} declares"));
            facts.get(&def).map(|fact| (**fact).clone())
        };
        assert_eq!(tag_fact("mark"), Some(super::TypeFact::Tag));
        assert_eq!(
            tag_fact("none"),
            None,
            "a tag restating the predeclared none carries no fact"
        );
    }

    /// The alias vocabulary carries: a non-generic alias whose body
    /// spells in the member's own linkable vocabulary rides as its
    /// resolved word — the ambient spellings, the container spellings
    /// over any words (parentheses peeling), an alias chain expanding
    /// transparently, a declared type of the same member as the Named
    /// word, and a union body carrying one word per member — the same
    /// closure the structural fact spells; a generic alias and a body
    /// naming anything else (a tuple, an applied alias) carry nothing.
    /// A bodiless struct carries the opaque identity (its atom stays
    /// shape-pending), distinct from the part-less struct's empty
    /// field list.
    #[test]
    fn the_fact_is_the_alias_s_word_and_the_bodiless_struct_s_identity() {
        let source = concat!(
            "pub tag int\n",
            "pub tag string\n",
            "pub tag byte\n",
            "pub tag bytes\n",
            "pub struct list[t] {\n",
            "}\n",
            "pub struct map[k, v] {\n",
            "}\n",
            "pub struct cell {\n",
            "  n: int,\n",
            "}\n",
            "pub type row = map[string, string]\n",
            "pub type words = list[string]\n",
            "pub type port = int?\n",
            "pub type grid = list[list[byte]]\n",
            "pub type sparse = list[int]?\n",
            "pub type name = string\n",
            "pub type raw = bytes\n",
            "pub type paren = (int)\n",
            "pub type pair[k, v] = map[k, v]\n",
            "pub enum shade {\n",
            "  red,\n",
            "  green,\n",
            "  blue,\n",
            "}\n",
            "pub type chain = row\n",
            "pub type local = cell\n",
            "pub type held = list[cell]\n",
            "pub type either = int | string\n",
            "pub type tint = red | green\n",
            "pub type maybeset = red | green | none\n",
            "pub type applied = pair[string, string]\n",
            "pub type lone = red\n",
            "pub type loop = cycle\n",
            "pub type cycle = loop\n",
            "pub type two = (int, int)\n",
            "pub struct opaque\n",
            "pub struct empty {\n",
            "}\n",
            "fn main() -> int {\n",
            "7\n",
            "}\n",
        );
        let mut files = ds_diag::Files::new();
        let file = files.add_source("alias_fact.ds", source);
        let ast = ds_parse::parse(&files, file).expect("the source parses");
        let analyzed = crate::type_of::analyze(&files, &[ast]);
        let facts =
            super::TypeFact::member_facts(&analyzed, 0, &std::collections::HashMap::default());
        let alias = |name: &str| {
            let def = analyzed
                .package
                .lookup_type(name)
                .unwrap_or_else(|| panic!("{name} declares"));
            facts.get(&def).map(|fact| (**fact).clone())
        };
        assert_eq!(
            alias("row"),
            Some(super::TypeFact::Alias(Box::new(super::TypeWord::Map(
                Box::new(super::TypeWord::Scalar(super::ScalarWord::Str)),
                Box::new(super::TypeWord::Scalar(super::ScalarWord::Str))
            ))))
        );
        assert_eq!(
            alias("words"),
            Some(super::TypeFact::Alias(Box::new(super::TypeWord::List(
                Box::new(super::TypeWord::Scalar(super::ScalarWord::Str))
            ))))
        );
        assert_eq!(
            alias("port"),
            Some(super::TypeFact::Alias(Box::new(super::TypeWord::Optional(
                Box::new(super::TypeWord::Scalar(super::ScalarWord::Int))
            ))))
        );
        // the written side recurses through the containers too: a nested
        // list body, an optional list body
        assert_eq!(
            alias("grid"),
            Some(super::TypeFact::Alias(Box::new(super::TypeWord::List(
                Box::new(super::TypeWord::List(Box::new(super::TypeWord::Scalar(
                    super::ScalarWord::Byte
                ))))
            ))))
        );
        assert_eq!(
            alias("sparse"),
            Some(super::TypeFact::Alias(Box::new(super::TypeWord::Optional(
                Box::new(super::TypeWord::List(Box::new(super::TypeWord::Scalar(
                    super::ScalarWord::Int
                ))))
            ))))
        );
        assert_eq!(
            alias("name"),
            Some(super::TypeFact::Alias(Box::new(super::TypeWord::Scalar(
                super::ScalarWord::Str
            ))))
        );
        assert_eq!(
            alias("raw"),
            Some(super::TypeFact::Alias(Box::new(super::TypeWord::Bytes)))
        );
        assert_eq!(
            alias("paren"),
            Some(super::TypeFact::Alias(Box::new(super::TypeWord::Scalar(
                super::ScalarWord::Int
            ))))
        );
        // an alias chain expands through the referenced alias's own
        // body, and a body naming a declared type of the same member
        // carries the Named word
        assert_eq!(
            alias("chain"),
            Some(super::TypeFact::Alias(Box::new(super::TypeWord::Map(
                Box::new(super::TypeWord::Scalar(super::ScalarWord::Str)),
                Box::new(super::TypeWord::Scalar(super::ScalarWord::Str))
            ))))
        );
        assert_eq!(
            alias("local"),
            Some(super::TypeFact::Alias(Box::new(super::TypeWord::Named(
                "cell".to_owned()
            ))))
        );
        assert_eq!(
            alias("held"),
            Some(super::TypeFact::Alias(Box::new(super::TypeWord::List(
                Box::new(super::TypeWord::Named("cell".to_owned()))
            ))))
        );
        // a union body carries one word per member — a scalar pair, a
        // partial case-subset union, and the `t | none` optional rule
        // wrapping the subset
        let subset = |words| {
            Some(super::TypeFact::Alias(Box::new(super::TypeWord::Union(
                words,
            ))))
        };
        assert_eq!(
            alias("either"),
            subset(vec![
                super::TypeWord::Scalar(super::ScalarWord::Int),
                super::TypeWord::Scalar(super::ScalarWord::Str),
            ])
        );
        assert_eq!(
            alias("tint"),
            subset(vec![
                super::TypeWord::Named("red".to_owned()),
                super::TypeWord::Named("green".to_owned()),
            ])
        );
        assert_eq!(
            alias("maybeset"),
            Some(super::TypeFact::Alias(Box::new(super::TypeWord::Optional(
                Box::new(super::TypeWord::Union(vec![
                    super::TypeWord::Named("red".to_owned()),
                    super::TypeWord::Named("green".to_owned()),
                ]))
            ))))
        );
        // the refusals: generic parameters, an applied alias, a lone
        // case (a unit word spells only inside a union), an inline
        // alias cycle, and a tuple body
        for refused in ["pair", "applied", "lone", "loop", "cycle", "two"] {
            assert_eq!(alias(refused), None, "{refused} carries no fact");
        }
        let struct_fact = alias;
        assert_eq!(struct_fact("opaque"), Some(super::TypeFact::Opaque));
        assert_eq!(
            struct_fact("empty"),
            Some(super::TypeFact::Struct {
                fields: Vec::new(),
                linear: false,
            })
        );
    }

    /// The consumer-side mint of the artifact round's richer alias and
    /// union words: a linked struct field over a partial case-subset
    /// union and linked aliases whose bodies name a linked type and a
    /// union mint exactly the products a local declaration would — the
    /// field's solved signature the union of the minted case atoms, the
    /// aliases' written bodies expanding in this compile's own universe
    /// through the link table — so the consumer constructs over the
    /// subset, matches its members, and annotates through the aliases
    /// checking clean.
    #[test]
    fn the_union_word_and_the_alias_body_mint_on_the_consumer() {
        let source = concat!(
            "pub tag int\n",
            "pub tag string\n",
            "pub struct list[t] {\n",
            "}\n",
            "import @lib (cell, shade, red, green, blue, partisan, cells, either)\n",
            "\n",
            "fn probe(p: partisan) -> int {\n",
            "  match p.pick {\n",
            "    red => 1,\n",
            "    green => 2,\n",
            "  }\n",
            "}\n",
            "\n",
            "fn main() -> int {\n",
            "  const held = partisan { pick: green }\n",
            "  const xs: cells = [cell { n: 1 }]\n",
            "  const e: either = \"hi\"\n",
            "  var total = probe(held)\n",
            "  match e {\n",
            "    other => total = total + 1,\n",
            "  }\n",
            "  match held.pick {\n",
            "    red => total = total + 10,\n",
            "    green => total = total + 100,\n",
            "  }\n",
            "  total\n",
            "}\n",
        );
        let mut files = ds_diag::Files::new();
        let file = files.add_source("union_mint.ds", source);
        let ast = ds_parse::parse(&files, file).expect("the source parses");
        let at = ds_diag::Span::new(file, 1, 1);
        let ty_entry = |name: &str, ty: super::TypeFact| crate::ForeignEntry {
            name: name.to_owned(),
            vis: crate::Vis::Pub,
            kind: crate::ForeignKind::Type,
            generic: false,
            defaulted: false,
            decl_span: at,
            sig: None,
            row: crate::EffectRow::default(),
            caps: 0,
            value: None,
            ty: Some(std::sync::Arc::new(ty)),
            link: 0,
            origin: None,
            func: None,
            std: false,
        };
        let named = |name: &str| super::TypeWord::Named(name.to_owned());
        let unit = |name: &str| super::TypeCase {
            name: name.to_owned(),
            payload: super::CasePayload::Unit,
        };
        let surface = crate::MemberSurface {
            tags: Vec::new(),
            impls: Vec::new(),
            entries: vec![
                ty_entry("cell", super::TypeFact::Struct {
                    fields: vec![super::TypeField {
                        name: "n".to_owned(),
                        word: super::TypeWord::Scalar(super::ScalarWord::Int),
                    }],
                    linear: false,
                }),
                ty_entry("shade", super::TypeFact::Enum {
                    cases: vec![unit("red"), unit("green"), unit("blue")],
                    linear: false,
                }),
                ty_entry(
                    "red",
                    super::TypeFact::Case(super::CaseFact {
                        enum_name: "shade".to_owned(),
                        payload: super::CasePayload::Unit,
                    }),
                ),
                ty_entry(
                    "green",
                    super::TypeFact::Case(super::CaseFact {
                        enum_name: "shade".to_owned(),
                        payload: super::CasePayload::Unit,
                    }),
                ),
                ty_entry(
                    "blue",
                    super::TypeFact::Case(super::CaseFact {
                        enum_name: "shade".to_owned(),
                        payload: super::CasePayload::Unit,
                    }),
                ),
                ty_entry("partisan", super::TypeFact::Struct {
                    fields: vec![super::TypeField {
                        name: "pick".to_owned(),
                        word: super::TypeWord::Union(vec![named("red"), named("green")]),
                    }],
                    linear: false,
                }),
                ty_entry(
                    "cells",
                    super::TypeFact::Alias(Box::new(super::TypeWord::List(Box::new(named(
                        "cell",
                    ))))),
                ),
                ty_entry(
                    "either",
                    super::TypeFact::Alias(Box::new(super::TypeWord::Union(vec![
                        super::TypeWord::Scalar(super::ScalarWord::Int),
                        super::TypeWord::Scalar(super::ScalarWord::Str),
                    ]))),
                ),
            ],
        };
        let mut imports = crate::ImportGraph::new();
        for item in ast.blocks.iter().flat_map(|block| block.items.iter()) {
            if let ds_parse::ItemKind::Import(import) = &item.kind {
                imports.add(import.span, crate::ImportSurface::Linked(surface.clone()));
            }
        }
        let (package, resolutions, diags) = crate::resolve_package_linked(&[&ast], &imports);
        assert!(diags.is_empty(), "the rows bind clean: {diags:?}");
        let analyzed = crate::type_of::analyze_resolved(
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
        // the subset field's minted signature solves to the union of
        // the minted case atoms — exactly the local spelling's set
        let partisan = analyzed
            .decls
            .foreign_type("partisan")
            .expect("partisan registers");
        let atom = analyzed
            .decls
            .atoms
            .get(&partisan)
            .copied()
            .expect("partisan's atom mints");
        let crate::AtomKind::Struct(ref fields) = analyzed
            .decls
            .universe
            .atom_decl(atom)
            .expect("shapes")
            .kind
        else {
            panic!("partisan's atom carries its fields");
        };
        // the subset field's minted signature is the union of the
        // minted case atoms — exactly the local spelling's tree
        let member_names = |expr: &crate::TypeExpr| match expr {
            crate::TypeExpr::Atom(id) => Some(
                analyzed
                    .decls
                    .universe
                    .atom_decl(*id)
                    .expect("a declared atom")
                    .name
                    .clone(),
            ),
            _ => None,
        };
        let crate::TypeExpr::Union(left, right) = &fields[0].ty else {
            panic!("the subset field's signature spells the union");
        };
        for member in [left, right] {
            let name = member_names(member).expect("a bare atom per member");
            assert!(
                name == "red" || name == "green",
                "the subset holds the minted cases alone: {name}"
            );
        }
        // the aliases' minted bodies carry the written spellings a local
        // alias would: a container over the linked name, a union tree
        // over the scalars
        let cells = analyzed
            .decls
            .foreign_type("cells")
            .expect("the alias registers");
        let (params, body) = analyzed
            .decls
            .aliases
            .get(&cells)
            .expect("the alias's written body mints");
        assert!(params.is_empty(), "the linked alias is non-generic");
        let ds_parse::Type::Path { path, args, .. } = body else {
            panic!("the alias body spells the container");
        };
        assert_eq!(path.segments.len(), 1, "a single-segment path");
        assert_eq!(path.segments[0].name, "list", "the list constructor");
        let [ds_parse::Type::Path { path: arg, .. }] = &args[..] else {
            panic!("one constructor argument");
        };
        assert_eq!(arg.segments[0].name, "cell", "the linked cell name");
        let either = analyzed
            .decls
            .foreign_type("either")
            .expect("the union alias registers");
        let (_, body) = analyzed
            .decls
            .aliases
            .get(&either)
            .expect("the union alias's written body mints");
        assert!(
            matches!(body, ds_parse::Type::Union { .. }),
            "the union alias's body spells the union tree"
        );
    }

    /// The consumer-side mint of the tuple word: a linked struct whose
    /// field holds a tuple of scalar members, and one whose list
    /// element is a tuple, mint exactly the products a local
    /// declaration would — the field's solved signature the product
    /// expr the universe solves into the structural atom, the written
    /// body the tuple spelling — so the consumer constructs,
    /// destructures, and lists over them checking clean.
    #[test]
    fn the_tuple_word_mints_on_the_consumer() {
        let source = concat!(
            "pub tag int\n",
            "pub tag string\n",
            "pub struct list[t] {\n",
            "}\n",
            "import @lib (twin, ledger)\n",
            "\n",
            "fn main() -> int {\n",
            "  const own = twin { p: (3, 4) }\n",
            "  const (a, b) = own.p\n",
            "  const book = ledger { rows: [(1, 2)] }\n",
            "  a + b\n",
            "}\n",
        );
        let mut files = ds_diag::Files::new();
        let file = files.add_source("tuple_mint.ds", source);
        let ast = ds_parse::parse(&files, file).expect("the source parses");
        let at = ds_diag::Span::new(file, 1, 1);
        let ty_entry = |name: &str, ty: super::TypeFact| crate::ForeignEntry {
            name: name.to_owned(),
            vis: crate::Vis::Pub,
            kind: crate::ForeignKind::Type,
            generic: false,
            defaulted: false,
            decl_span: at,
            sig: None,
            row: crate::EffectRow::default(),
            caps: 0,
            value: None,
            ty: Some(std::sync::Arc::new(ty)),
            link: 0,
            origin: None,
            func: None,
            std: false,
        };
        let pair = || {
            super::TypeWord::Tuple(vec![
                super::TypeWord::Scalar(super::ScalarWord::Int),
                super::TypeWord::Scalar(super::ScalarWord::Int),
            ])
        };
        let surface = crate::MemberSurface {
            tags: Vec::new(),
            impls: Vec::new(),
            entries: vec![
                ty_entry("twin", super::TypeFact::Struct {
                    fields: vec![super::TypeField {
                        name: "p".to_owned(),
                        word: pair(),
                    }],
                    linear: false,
                }),
                ty_entry("ledger", super::TypeFact::Struct {
                    fields: vec![super::TypeField {
                        name: "rows".to_owned(),
                        word: super::TypeWord::List(Box::new(pair())),
                    }],
                    linear: false,
                }),
            ],
        };
        let mut imports = crate::ImportGraph::new();
        for item in ast.blocks.iter().flat_map(|block| block.items.iter()) {
            if let ds_parse::ItemKind::Import(import) = &item.kind {
                imports.add(import.span, crate::ImportSurface::Linked(surface.clone()));
            }
        }
        let (package, resolutions, diags) = crate::resolve_package_linked(&[&ast], &imports);
        assert!(diags.is_empty(), "the rows bind clean: {diags:?}");
        let analyzed = crate::type_of::analyze_resolved(
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
        let atom_name = |expr: &crate::TypeExpr| match expr {
            crate::TypeExpr::Atom(id) => Some(
                analyzed
                    .decls
                    .universe
                    .atom_decl(*id)
                    .expect("a declared atom")
                    .name
                    .clone(),
            ),
            _ => None,
        };
        let struct_fields = |name: &str| {
            let def = analyzed
                .decls
                .foreign_type(name)
                .unwrap_or_else(|| panic!("{name} registers"));
            let atom = analyzed
                .decls
                .atoms
                .get(&def)
                .copied()
                .unwrap_or_else(|| panic!("{name}'s atom mints"));
            let crate::AtomKind::Struct(ref fields) = analyzed
                .decls
                .universe
                .atom_decl(atom)
                .expect("shapes")
                .kind
            else {
                panic!("{name}'s atom carries its fields");
            };
            fields.clone()
        };
        // the tuple field's minted signature is the product expr over
        // the scalar atoms — exactly the local spelling's tree
        let twin_fields = struct_fields("twin");
        let crate::TypeExpr::Tuple(ref members) = twin_fields[0].ty else {
            panic!("the tuple field's signature spells the product");
        };
        assert_eq!(
            members.iter().map(atom_name).collect::<Vec<_>>(),
            vec![Some("int".to_owned()), Some("int".to_owned())],
            "the product holds the scalar atoms in order"
        );
        // the written body spells the tuple a local field would
        let twin = analyzed.decls.foreign_type("twin").expect("twin registers");
        let shape = analyzed
            .decls
            .structs
            .get(&twin)
            .expect("the struct row mints");
        let ds_parse::Type::Tuple { items, .. } = &shape.fields[0].ty else {
            panic!("the written body spells the tuple");
        };
        assert_eq!(items.len(), 2, "one written member per word");
        // the list-over-tuple field's element set holds the one
        // structural tuple atom, its elements the scalar atoms
        let ledger_fields = struct_fields("ledger");
        let crate::TypeExpr::Atom(list_atom) = ledger_fields[0].ty else {
            panic!("the field's signature names the list");
        };
        let crate::AtomKind::Apply(_, args) = &analyzed
            .decls
            .universe
            .atom_decl(list_atom)
            .expect("shapes")
            .kind
        else {
            panic!("the list application mints");
        };
        let [arg, ..] = args.as_ref() else {
            panic!("one constructor argument");
        };
        let elements: Vec<String> = analyzed
            .decls
            .universe
            .members(*arg)
            .iter()
            .filter_map(
                |&id| match &analyzed.decls.universe.atom_decl(id).ok()?.kind {
                    crate::AtomKind::Tuple(words) => Some(words.clone()),
                    _ => None,
                },
            )
            .flatten()
            .filter_map(|id| {
                analyzed
                    .decls
                    .universe
                    .atom_decl(id)
                    .ok()
                    .map(|decl| decl.name.clone())
            })
            .collect();
        assert_eq!(
            elements,
            vec!["int".to_owned(), "int".to_owned()],
            "the element set holds the one structural tuple atom"
        );
    }

    /// The trait vocabulary carries: a plain trait's member rows in
    /// declaration order, each with its written signature; a member
    /// carrying a default body, a generic trait, and a supertrait
    /// refuse the crossing whole. The impl walk keys each plain impl's
    /// member rows by the mangled table names the plan owns — the head
    /// one word of the same vocabulary a struct field spells — refusing
    /// an impl of a trait that refused and an impl whose members did
    /// not all plan.
    #[test]
    fn the_fact_is_the_trait_s_member_rows() {
        let source = concat!(
            "pub tag int\n",
            "pub struct pair {\n",
            "  a: int,\n",
            "  b: int,\n",
            "}\n",
            "pub struct hidden {\n",
            "  v: int,\n",
            "}\n",
            "pub trait sizing {\n",
            "  fn size(self) -> int\n",
            "  fn label(self) -> int\n",
            "}\n",
            "pub trait defaulted {\n",
            "  fn answer(self) -> int {\n",
            "    7\n",
            "  }\n",
            "}\n",
            "pub trait wide[t] {\n",
            "  fn pick(self) -> t\n",
            "}\n",
            "pub trait sub: sizing {\n",
            "}\n",
            "impl sizing for pair {\n",
            "  fn size(self) -> int {\n",
            "    self.a + self.b\n",
            "  }\n",
            "  fn label(self) -> int {\n",
            "    1\n",
            "  }\n",
            "}\n",
            "impl sizing for int {\n",
            "  fn size(self) -> int {\n",
            "    2\n",
            "  }\n",
            "  fn label(self) -> int {\n",
            "    3\n",
            "  }\n",
            "}\n",
            "impl defaulted for pair {\n",
            "  fn answer(self) -> int {\n",
            "    9\n",
            "  }\n",
            "}\n",
            "fn main() -> int {\n",
            "7\n",
            "}\n",
        );
        let mut files = ds_diag::Files::new();
        let file = files.add_source("trait_fact.ds", source);
        let ast = ds_parse::parse(&files, file).expect("the source parses");
        let analyzed = crate::type_of::analyze(&files, &[ast]);
        let facts =
            super::TypeFact::member_facts(&analyzed, 0, &std::collections::HashMap::default());
        let fact = |name: &str| {
            analyzed
                .package
                .lookup_type(name)
                .and_then(|def| facts.get(&def).map(|fact| (**fact).clone()))
        };
        // a plain trait's rows carry, names in declaration order
        let Some(super::TypeFact::Trait(trait_fact)) = fact("sizing") else {
            panic!("sizing carries its rows");
        };
        assert_eq!(
            trait_fact
                .members
                .iter()
                .map(|member| member.name.as_str())
                .collect::<Vec<_>>(),
            vec!["size", "label"]
        );
        assert_eq!(
            trait_fact.members[0].sig.params.len(),
            1,
            "the written signature rides the row"
        );
        for refused in ["wide", "sub"] {
            assert_eq!(fact(refused), None, "{refused} carries no fact");
        }
        // a defaulted member's row crosses bodiless over an empty
        // carried table, and its body's own link rides when the table
        // names the mangled row the plan holds
        let Some(super::TypeFact::Trait(defaulted)) = fact("defaulted") else {
            panic!("defaulted carries its rows");
        };
        assert_eq!(defaulted.members[0].default_func, None);
        let carried = [("defaulted#answer".to_owned(), 4u32)]
            .into_iter()
            .collect::<std::collections::HashMap<_, _>>();
        let with_defaults = super::TypeFact::member_facts(&analyzed, 0, &carried);
        let Some(super::TypeFact::Trait(linked)) = analyzed
            .package
            .lookup_type("defaulted")
            .and_then(|def| with_defaults.get(&def).map(|fact| (**fact).clone()))
        else {
            panic!("defaulted carries its rows over the carried table");
        };
        assert_eq!(linked.members[0].default_func, Some(4));
        // the impl rows key over the plan's mangled table names: both
        // heads cross (a Named word, a scalar word), members in order
        let eligible: std::collections::HashSet<crate::DefId> = facts.keys().copied().collect();
        let functions = vec![
            "sizing#pair#size".to_owned(),
            "sizing#pair#label".to_owned(),
            "sizing#int#size".to_owned(),
            "sizing#int#label".to_owned(),
            "defaulted#pair#answer".to_owned(),
        ];
        let rows = super::ImplFact::member_facts(&analyzed, 0, &functions, &eligible, &facts);
        let row = |trait_name: &str, rendered: &str| {
            rows.iter()
                .find(|row| {
                    row.trait_name == trait_name && super::render_word(&row.head) == rendered
                })
                .cloned()
        };
        let pair_row = row("sizing", "pair").expect("the pair impl carries its row");
        assert_eq!(
            pair_row
                .members
                .iter()
                .map(|member| (member.name.as_str(), member.func))
                .collect::<Vec<_>>(),
            vec![("size", 0), ("label", 1)]
        );
        let int_row = row("sizing", "int").expect("the scalar-headed impl carries its row");
        assert_eq!(int_row.members[0].func, 2);
        // the defaulted trait crosses now, so its spelled impl rides
        // its row too — the omission door lives with the consumer's
        // mint, not the walk
        let defaulted_row =
            row("defaulted", "pair").expect("the defaulted trait's impl carries its row");
        assert_eq!(defaulted_row.members[0].func, 4);
        // a member the plan never tabled (the mangle misses) refuses
        // the whole impl
        let short = ["sizing#pair#size".to_owned()];
        let none = super::ImplFact::member_facts(&analyzed, 0, &short, &eligible, &facts);
        assert!(
            none.iter()
                .all(|row| super::render_word(&row.head) != "pair"),
            "an unplanned member refuses the impl"
        );
    }
}
