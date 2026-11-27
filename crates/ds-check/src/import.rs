//! The import surface's fact vocabulary: the const, type, trait, and
//! impl link facts a linked member crosses the seam with, beside the
//! encoded-word vocabulary they spell in and the graph the link walk
//! reads.

use std::sync::Arc;

use ds_diag::Span;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::{EffectRow, Vis, decls::FnSig};

mod graph;
mod word;

pub use graph::{ForeignEntry, ForeignKind, ImportGraph, ImportSurface, MemberSurface};
use word::WordSolver;
pub use word::{
    HeapRefWord, PrimitiveWord, ScalarWord, TypeWord, heap_ref, primitive_word, render_word,
};

/// The per-target link fact a linked `const` binding carries: the
/// literal value itself, content-addressable, so a binding site embeds
/// it exactly as the literal a user would have written — no linked-module
/// evaluation and no instantiation machinery behind it. A computed
/// initializer carries no fact (its value is an evaluation of the linked
/// module) and stays refused.
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
    /// (explicit widths carry no fact).
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
    /// gates deinit-carrying declarations out of the fact.
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
    /// The declaration carries the `#[sealed]` origin attribute: the
    /// trait admits only its declaring package's own impls, so a
    /// consuming compile refuses its own impls over the trait and the
    /// install admits linked rows from the trait's own link position
    /// alone.
    pub sealed: bool,
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
        self.members == other.members && self.sealed == other.sealed
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
    /// The impl's vocabulary name (`impl show for int as fast`): a
    /// named instance sits outside the default vocabulary, reachable
    /// only through a consumer call site's selection.
    pub name: Option<String>,
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
    /// The declaring package's identity (`name@version`), stamped at
    /// the surface build: the link layering's package identity. Empty
    /// when no graph served the row.
    pub pkg: String,
    /// The declaring package's own artifact statically dispatches to
    /// this impl (a baked instance word names it), so a link cannot
    /// shadow it without the loser's body keeping its own behavior.
    pub exercised: bool,
}

// PartialEq ignores the span: equality is the linked impl's identity
// (the trait, the head, the member rows, the owning module's link
// position, and the stamped package identity and exercise fact — all
// functions of one producer source), which one producer source
// determines; the span rides to the coherence diagnostic alone, and two
// surface builds of one package rebase it differently — the link pass's
// dedup keys on this equality, never the span.
impl PartialEq for ImplFact {
    fn eq(&self, other: &Self) -> bool {
        self.trait_name == other.trait_name
            && self.name == other.name
            && self.head == other.head
            && self.members == other.members
            && self.link == other.link
            && self.pkg == other.pkg
            && self.exercised == other.exercised
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
    /// deinit-carrying structs stay out. The map's keys are exactly the
    /// candidates whose fact survived.
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
        let mut traits: FxHashMap<crate::DefId, (&ds_parse::TraitDecl, bool)> =
            FxHashMap::default();
        for ast in analyzed.asts.iter().skip(ambient_files) {
            for block in &ast.blocks {
                for item in &block.items {
                    let ds_parse::ItemKind::Trait(t) = &item.kind else {
                        continue;
                    };
                    if let Some(def) = package.lookup_type(&t.name.name)
                        && package.def(def).span == t.name.span
                    {
                        let sealed = item.attrs.iter().any(|a| a.name.name == "sealed");
                        traits.insert(def, (t, sealed));
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
    /// the export walk owns, the struct deinit gate included.
    fn of_def(
        package: &crate::resolve::Package,
        decls: &crate::Decls,
        deinit: &std::collections::HashSet<u32>,
        eligible: &FxHashSet<crate::DefId>,
        trait_decl: Option<(&ds_parse::TraitDecl, bool)>,
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
            crate::DefKind::Trait => {
                let (decl, sealed) = trait_decl?;
                Self::trait_fact(decl, carried, sealed)
            },
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
    fn trait_fact(
        t: &ds_parse::TraitDecl,
        carried: &FxHashMap<String, u32>,
        sealed: bool,
    ) -> Option<Self> {
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
        Some(Self::Trait(TraitFact { members, sealed }))
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
            // the case resolves scoped to this enum, not by global name:
            // two enums may share a case name, and either one's def
            // would answer the bare lookup
            let case_def = decls
                .cases
                .iter()
                .find_map(|(case, info)| {
                    (info.enum_def == def && package.def(*case).name == *name).then_some(*case)
                })
                .or_else(|| package.lookup_type(name))?;
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
    /// whose members did not all plan (a member body over the impl's
    /// own variables) carry no row and keep the consumer's dispatch
    /// refused. The implemented trait may be this member's own
    /// declaration (its fact rode beside the row, the member rows a
    /// consumer checks against) or a linked one this member itself
    /// imports — a third party's impl of another package's trait over
    /// another package's type crosses the same row, the link layering
    /// resolving it against the origin's own.
    #[must_use]
    pub fn member_facts(
        analyzed: &crate::type_of::Analyzed,
        ambient_files: usize,
        functions: &[String],
        eligible: &std::collections::HashSet<crate::DefId>,
        traits: &FxHashMap<crate::DefId, Arc<TypeFact>>,
        exercised: &std::collections::HashSet<u32>,
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
                        continue; // an inherent impl belongs to the type-keyed seam
                    };
                    let [only] = trait_ref.path.segments.as_slice() else {
                        continue;
                    };
                    if !imp.generics.is_empty() {
                        continue;
                    }
                    // the trait reference resolves through the local
                    // namespace first, then the link table — the same
                    // pull the impl item's own declaration took (the
                    // link pass's registrations ride no type_names row)
                    let Some(def) = package
                        .lookup_type(&only.name)
                        .or_else(|| package.foreign_type_def(&only.name))
                    else {
                        continue;
                    };
                    let own_trait = package.foreign().get(&def).is_none();
                    if own_trait {
                        // the producer's own trait must have crossed
                        // itself: its member rows are the rows a consumer
                        // checks against, so an impl of a refused trait
                        // rides no fact
                        if (package.def(def).span.file.as_u32() as usize) < ambient_files
                            || !traits.contains_key(&def)
                            || !matches!(&*traits[&def], TypeFact::Trait(_))
                        {
                            continue;
                        }
                    } else if !matches!(
                        package
                            .foreign()
                            .get(&def)
                            .and_then(|entry| entry.ty.as_deref()),
                        Some(TypeFact::Trait(_))
                    ) {
                        // a linked trait the surface never carried
                        // member rows for: the impl checked against no
                        // registration, so no row rides
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
                        let mut name = format!(
                            "{}#{}#{}",
                            only.name,
                            crate::decls::render_type(&imp.target),
                            f.name.name
                        );
                        // A named impl's mangled body name carries the
                        // vocabulary, mirroring the plan's own naming.
                        if let Some(impl_name) = &imp.name {
                            name.push('#');
                            name.push_str(&impl_name.name);
                        }
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
                            name: imp.name.as_ref().map(|n| n.name.clone()),
                            head,
                            members: members.clone(),
                            span: item.span,
                            link: 0,
                            pkg: String::new(),
                            exercised: impl_exercised(decls, item.span, exercised),
                        });
                    }
                }
            }
        }
        out
    }
}

/// Whether the package's own lowering statically resolved an instance
/// of the impl item: any of the item's instances among the statically
/// exercised set marks the row.
fn impl_exercised(
    decls: &crate::Decls,
    item_span: Span,
    exercised: &std::collections::HashSet<u32>,
) -> bool {
    exercised.iter().any(|index| {
        decls
            .instance_impls
            .get(&crate::InstanceId::from_raw(*index))
            .is_some_and(|site| *site == item_span)
    })
}

/// One linked inherent impl's member link fact (the type-keyed receiver
/// seam): the impl's owner type name, each served member body's index in
/// the linked module's table, and the written signature a consumer's
/// call checks against — the inherent mirror of [`ImplFact`], keyed by
/// the owner the consumer's method registry registers instead of an
/// instance. A generic impl, a generic or defaulted member, and an
/// unplanned body keep their refusals: the row simply does not cross,
/// and the consumer's call site refuses at resolution. An inherent row
/// carries no exercise fact: an inherent call site addresses the body
/// directly (no instance word exists to bake), so a losing row's own
/// package keeps its behavior whatever the consumer shadows.
#[derive(Clone, Debug)]
pub struct InherentFact {
    /// The impl's target type's declared name: the owner identity the
    /// consuming compile's registry keys (`MethodOwner::Inherent`), the
    /// same head path name the local registration spells.
    pub target: String,
    /// The impl's served member rows, one per crossing member fn.
    pub members: Vec<InherentMemberFact>,
    /// The impl item's span, rebased into the consuming compile's file
    /// space so shadowing diagnostics render.
    pub span: Span,
    /// The linked dep entry's position in the consuming module's
    /// recorded list, assigned at the surface build: the module whose
    /// table holds the member bodies.
    pub link: u32,
    /// The declaring package's identity (`name@version`), stamped at
    /// the surface build: the link layering's package identity, the
    /// loser's name a root shadowing warning spells. Empty when no
    /// graph served the row.
    pub pkg: String,
}

/// One member row of a linked inherent impl.
#[derive(Clone, Debug)]
pub struct InherentMemberFact {
    /// The member's name as called after the dot or behind the type
    /// name.
    pub name: String,
    /// The member's name occurrence, rebased into the consuming
    /// compile's file space so method diagnostics render.
    pub span: Span,
    /// The written signature, as the declaring impl spells it.
    pub sig: Arc<FnSig>,
    /// The member body's index in the linked module's table.
    pub func: u32,
}

// PartialEq ignores the span and the signature, exactly like
// [`ImplFact`]'s own: equality is the linked impl's identity (the
// owner, the member names, the body indexes, the owning module's link
// position, and the stamped package identity), which one producer
// source determines; two surface builds of one package rebase the
// spans differently, so the link pass's dedup keys on this equality,
// never the span. The signature rides to the consumer's call check
// and is never compared.
impl PartialEq for InherentMemberFact {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name && self.func == other.func
    }
}

impl Eq for InherentMemberFact {}

impl PartialEq for InherentFact {
    fn eq(&self, other: &Self) -> bool {
        self.target == other.target
            && self.members == other.members
            && self.link == other.link
            && self.pkg == other.pkg
    }
}

impl Eq for InherentFact {}

impl InherentFact {
    /// The member link facts for every plain inherent impl one package
    /// compile declares over an exported type: one row per impl item,
    /// the owner the type's declared name, each bodied member's index
    /// in the linked module's own table. The owner type's own `pub`
    /// tier is the members' export gate (a member carries no vis of
    /// its own), and the row crosses only when the type's structural
    /// fact crossed — a consumer that cannot mint the owner cannot
    /// spell its members. A generic impl, an applied or qualified
    /// target, a generic or defaulted member, and a body the module
    /// never planned keep their refusals.
    #[must_use]
    pub fn member_facts(
        analyzed: &crate::type_of::Analyzed,
        ambient_files: usize,
        functions: &[String],
        types: &FxHashMap<crate::DefId, Arc<TypeFact>>,
    ) -> Vec<Self> {
        let package = &analyzed.package;
        let mut out = Vec::new();
        for ast in analyzed.asts.iter().skip(ambient_files) {
            for block in &ast.blocks {
                for item in &block.items {
                    let ds_parse::ItemKind::Impl(imp) = &item.kind else {
                        continue;
                    };
                    if imp.trait_ref.is_some() || !imp.generics.is_empty() {
                        continue;
                    }
                    let ds_parse::Type::Path { path, args, .. } = &imp.target else {
                        continue;
                    };
                    let [only] = path.segments.as_slice() else {
                        continue;
                    };
                    if !args.is_empty() {
                        continue;
                    }
                    // the owner resolves through the package namespace,
                    // which never answers a linked name: the row crosses
                    // only over the declaring package's own type, so
                    // the origin package alone ever serves a type's
                    // members — no third-party row exists to layer
                    let Some(def) = package.lookup_type(&only.name) else {
                        continue;
                    };
                    if package.def(def).vis != Vis::Pub || !types.contains_key(&def) {
                        continue;
                    }
                    let mut members = Vec::new();
                    for f in &imp.fns {
                        if !f.generics.is_empty()
                            || f.params.iter().any(|param| param.default.is_some())
                        {
                            continue;
                        }
                        let name = format!("{}#{}", only.name, f.name.name);
                        let Some(func) = functions.iter().position(|row| *row == name) else {
                            continue;
                        };
                        members.push(InherentMemberFact {
                            name: f.name.name.clone(),
                            span: f.name.span,
                            sig: Arc::new(FnSig::of(f)),
                            func: u32::try_from(func).expect("function index fits u32"),
                        });
                    }
                    if members.is_empty() {
                        continue;
                    }
                    out.push(Self {
                        target: only.name.clone(),
                        members,
                        span: item.span,
                        link: 0,
                        pkg: String::new(),
                    });
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
            inherent: Vec::new(),
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
            "  first(int, int),\n",
            "  second(string, byte),\n",
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
        let case_fact = |name: &str, enum_name: &str| {
            let enum_def = analyzed
                .package
                .lookup_type(enum_name)
                .unwrap_or_else(|| panic!("{enum_name} declares"));
            let def = analyzed
                .decls
                .cases
                .iter()
                .find_map(|(case, info)| {
                    (info.enum_def == enum_def && analyzed.package.def(*case).name == name)
                        .then_some(*case)
                })
                .unwrap_or_else(|| panic!("{enum_name}::{name} declares"));
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
                        name: "first".to_owned(),
                        payload: super::CasePayload::Tuple(vec![
                            super::TypeWord::Scalar(super::ScalarWord::Int),
                            super::TypeWord::Scalar(super::ScalarWord::Int),
                        ]),
                    },
                    super::TypeCase {
                        name: "second".to_owned(),
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
            case_fact("first", "trip"),
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

    /// The consumer-side mint of the richer alias and union words: a
    /// linked struct field over a partial case-subset
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
            inherent: Vec::new(),
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
            inherent: Vec::new(),
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
        let rows = super::ImplFact::member_facts(
            &analyzed,
            0,
            &functions,
            &eligible,
            &facts,
            &std::collections::HashSet::new(),
        );
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
        let none = super::ImplFact::member_facts(
            &analyzed,
            0,
            &short,
            &eligible,
            &facts,
            &std::collections::HashSet::new(),
        );
        assert!(
            none.iter()
                .all(|row| super::render_word(&row.head) != "pair"),
            "an unplanned member refuses the impl"
        );
    }

    /// The inherent walk keys each plain impl's member rows by the
    /// mangled table names the plan owns, the owner the type's own
    /// declaration: a `pub` type's impl carries its planned members in
    /// order, while a generic member, a defaulted member, a member the
    /// plan never tabled, a private owner, and an impl over an applied
    /// target keep their refusals — the row does not cross and the
    /// consumer's call site refuses at resolution.
    #[test]
    fn the_inherent_rows_key_over_the_owner_and_the_plan() {
        let source = concat!(
            "pub struct pair {\n",
            "  a: int,\n",
            "  b: int,\n",
            "}\n",
            "struct hidden {\n",
            "  v: int,\n",
            "}\n",
            "pub struct box[t] {\n",
            "  v: t,\n",
            "}\n",
            "impl pair {\n",
            "  fn size(self) -> int {\n",
            "    self.a + self.b\n",
            "  }\n",
            "  fn make(a: int, b: int) -> pair {\n",
            "    pair { a: a, b: b }\n",
            "  }\n",
            "  fn defaulted(self, scale: int = 2) -> int {\n",
            "    self.a * scale\n",
            "  }\n",
            "  fn generic[t](self) -> int {\n",
            "    self.a\n",
            "  }\n",
            "}\n",
            "impl hidden {\n",
            "  fn size(self) -> int {\n",
            "    self.v\n",
            "  }\n",
            "}\n",
            "impl box[int] {\n",
            "  fn get(self) -> int {\n",
            "    self.v\n",
            "  }\n",
            "}\n",
            "fn main() -> int {\n",
            "7\n",
            "}\n",
        );
        let mut files = ds_diag::Files::new();
        let file = files.add_source("inherent_fact.ds", source);
        let ast = ds_parse::parse(&files, file).expect("the source parses");
        let analyzed = crate::type_of::analyze(&files, &[ast]);
        let facts =
            super::TypeFact::member_facts(&analyzed, 0, &std::collections::HashMap::default());
        let functions = vec!["pair#size".to_owned(), "pair#make".to_owned()];
        let rows = super::InherentFact::member_facts(&analyzed, 0, &functions, &facts);
        assert_eq!(rows.len(), 1, "one row crosses: the pub pair impl alone");
        let row = &rows[0];
        assert_eq!(row.target, "pair");
        assert_eq!(
            row.members
                .iter()
                .map(|member| (member.name.as_str(), member.func))
                .collect::<Vec<_>>(),
            vec![("size", 0), ("make", 1)],
            "the planned members cross in order; the defaulted and generic members keep their \
             refusals"
        );
        assert_eq!(
            row.members[0].sig.params.len(),
            1,
            "the written signature rides the row"
        );
        // a member the plan never tabled keeps its own refusal: the
        // member's row stays home while its sibling crosses alone
        let short = ["pair#size".to_owned()];
        let unplanned = super::InherentFact::member_facts(&analyzed, 0, &short, &facts);
        let [only] = &unplanned[..] else {
            panic!("the pair row crosses over its planned member alone");
        };
        assert_eq!(
            only.members
                .iter()
                .map(|member| member.name.as_str())
                .collect::<Vec<_>>(),
            vec!["size"],
            "an unplanned member keeps its refusal without refusing the impl"
        );
    }
}
