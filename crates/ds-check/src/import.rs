//! The G4 import surface: the linked half of name binding. An import's
//! selected symbols bind at use sites through a caller-supplied surface —
//! one entry per import declaration, keyed by the declaration's span —
//! while an `@std` import no graph serves keeps the merged-ambient
//! reading (its selections validate against the compile unit's merged
//! namespace, the landed visibility-round behavior) and an import no
//! linked package serves stays declarative for the link walk.

use std::sync::Arc;

use ds_diag::Span;
use rustc_hash::FxHashMap;

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
/// fields, an enum's tag rows, a non-generic alias's resolved word, and
/// a bodiless struct's opaque identity; every other type-namespace
/// shape (a tag, a trait, a generic or linear declaration) and every
/// field outside the vocabulary carries no fact and keeps its selection
/// refused.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum TypeFact {
    /// A flat, non-generic struct: named fields over the word
    /// vocabulary. A linear declaration (`~Copyable`, so a deinit
    /// carrier too) carries no fact — its lifetime discipline is not a
    /// surface fact this round.
    Struct(Vec<TypeField>),
    /// A non-generic enum: its tag rows, one per case, in declaration
    /// order. A linear declaration carries no fact (the struct clause's
    /// rule) and an empty enum carries none (uninhabited, nothing
    /// crosses); every payload must spell in the vocabulary — a unit
    /// case or a struct payload of word-shaped fields, a tuple payload
    /// carrying no member names.
    Enum(Vec<TypeCase>),
    /// One case minted by a linked enum: the case's own entry in the
    /// type namespace, naming its enum so the consuming compile's case
    /// row maps back. Registers only beside its enum's registration.
    Case(CaseFact),
    /// A non-generic alias whose body spells in the word vocabulary: the
    /// resolved word rides, and the consuming compile's mint rewrites it
    /// as the written type a local alias of the same body would carry —
    /// the name expands in that compile's own universe exactly like a
    /// local spelling. An alias naming anything the vocabulary cannot
    /// spell directly (another alias, a local type, a union) carries no
    /// fact.
    Alias(Box<TypeWord>),
    /// A bodiless, non-generic, non-linear struct declaration: the
    /// opaque handle a local `struct x` with no body reserves — identity
    /// alone, no layout. The consuming compile's mint reserves the same
    /// shape-pending atom, so the name binds in signatures while every
    /// value use keeps the local spelling's own refusals (no
    /// construction lowers, no witness exists).
    Opaque,
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
/// one Ref word), a list of one scalar and a map of two scalars (the
/// heap references' single Ref words), and an optional scalar (the
/// scalar's union with `none`: one presence tag word over the scalar's
/// own payload region, exactly `t?`'s local lowering). The names
/// resolve in the consuming compile's own universe — the same trust the
/// const fact's scalar atom enjoys.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TypeWord {
    /// A scalar atom word.
    Scalar(ScalarWord),
    /// `list[scalar]`: one Ref word over the runtime's list object.
    List(ScalarWord),
    /// `bytes`: the immutable byte sequence's one Ref word, `string`'s
    /// shape over raw bytes. No plain literal spells it, so it rides
    /// the type vocabulary alone, never the const fact's scalar words.
    Bytes,
    /// `map[scalar, scalar]`: one Ref word over the runtime's map
    /// object, the list word's own shape over two key/value members.
    Map(ScalarWord, ScalarWord),
    /// `scalar?`: the scalar's union with `none` — one presence tag
    /// word over the scalar's own payload word(s), the same span a
    /// local optional field lowers to. An optional over any wider word
    /// (a list, a map, `bytes`) carries no fact this round.
    Optional(ScalarWord),
}

/// The scalar atoms the surface trusts by name: the one word vocabulary
/// the const and type facts spell in — each scalar fact maps into it
/// (`[ConstFact::scalar_word]`, [`by_name`](Self::by_name)), and the ambient
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
}

impl ScalarWord {
    /// The word one ambient atom name spells, when the surface trusts
    /// it; every other name answers `None` and keeps its selection
    /// refused.
    #[must_use]
    pub fn by_name(name: &str) -> Option<Self> {
        match name {
            "int" => Some(Self::Int),
            "float" => Some(Self::Float),
            "bool" => Some(Self::Bool),
            "string" => Some(Self::Str),
            "char" => Some(Self::Char),
            "byte" => Some(Self::Byte),
            _ => None,
        }
    }

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
        }
    }
}

impl TypeFact {
    /// The fact one declared struct carries, when its whole shape spells
    /// in the vocabulary: non-generic, not linear, every field a word the
    /// vocabulary carries. A bodiless declaration (its atom still
    /// shape-pending) carries the opaque identity alone. The caller gates
    /// the def's kind — a struct-payload enum case mints a struct atom
    /// but is not a type-namespace struct export. Everything else
    /// answers `None` and keeps its selection refused.
    #[must_use]
    pub fn of_struct(decls: &crate::Decls, def: crate::DefId) -> Option<Self> {
        let shape = decls.structs.get(&def)?;
        if !shape.params.is_empty() || shape.linear {
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
        let mut out = Vec::with_capacity(fields.len());
        for field in fields {
            out.push(TypeField {
                name: field.name.clone()?,
                word: word_of_expr(decls, &field.ty)?,
            });
        }
        Some(Self::Struct(out))
    }

    /// The fact one declared alias carries, when its body spells directly
    /// in the word vocabulary: non-generic (an alias with parameters
    /// resolves through use-site args and is no single word), the body a
    /// word the vocabulary names. Everything else — a body naming
    /// another alias, a local type, a union — answers `None` and keeps
    /// its selection refused.
    #[must_use]
    pub fn of_alias(decls: &crate::Decls, def: crate::DefId) -> Option<Self> {
        let (params, body) = decls.aliases.get(&def)?;
        if !params.is_empty() {
            return None;
        }
        Some(Self::Alias(Box::new(word_of_written(body)?)))
    }

    /// The fact one declared enum carries, when every case spells in the
    /// vocabulary: non-generic, not linear, at least one case, every
    /// payload a unit or a struct payload of word-shaped fields (the
    /// tuple payload carries no member names). Everything else answers
    /// `None` and keeps its selection refused.
    #[must_use]
    pub fn of_enum(
        package: &crate::resolve::Package,
        decls: &crate::Decls,
        def: crate::DefId,
    ) -> Option<Self> {
        let shape = decls.enums.get(&def)?;
        if !shape.params.is_empty() || shape.linear || shape.cases.is_empty() {
            return None;
        }
        let mut out = Vec::with_capacity(shape.cases.len());
        for (name, ..) in &shape.cases {
            let case_def = package.lookup_type(name)?;
            out.push(TypeCase {
                name: name.clone(),
                payload: case_payload(decls, case_def)?,
            });
        }
        Some(Self::Enum(out))
    }

    /// The fact one minted enum case carries: its enum's name and its
    /// payload, when the payload spells in the vocabulary. The enum's
    /// own gates are [`Self::of_enum`]'s; a case fact of an enum that
    /// carries none still answers here, but the link walk registers a
    /// case only beside its registered enum.
    #[must_use]
    pub fn of_case(decls: &crate::Decls, def: crate::DefId, enum_name: &str) -> Option<Self> {
        decls.cases.get(&def)?;
        Some(Self::Case(CaseFact {
            enum_name: enum_name.to_owned(),
            payload: case_payload(decls, def)?,
        }))
    }
}

/// One case's payload shape over the vocabulary, from its minted
/// declaration products.
fn case_payload(decls: &crate::Decls, def: crate::DefId) -> Option<CasePayload> {
    match &decls.cases.get(&def)?.payload {
        ds_parse::EnumPayload::None => Some(CasePayload::Unit),
        ds_parse::EnumPayload::Tuple(_) => None,
        ds_parse::EnumPayload::Struct(_) => {
            let atom = decls.atoms.get(&def)?;
            let fields = match decls.universe.atom_decl(*atom).ok()?.kind {
                crate::AtomKind::Struct(ref fields) => fields.clone(),
                _ => return None,
            };
            let mut out = Vec::with_capacity(fields.len());
            for field in fields {
                out.push(TypeField {
                    name: field.name.clone()?,
                    word: word_of_expr(decls, &field.ty)?,
                });
            }
            Some(CasePayload::Struct(out))
        },
    }
}

/// One solved field type's word kind, when it spells in the vocabulary.
fn word_of_expr(decls: &crate::Decls, expr: &crate::TypeExpr) -> Option<TypeWord> {
    match expr {
        crate::TypeExpr::Atom(id) => {
            let decl = decls.universe.atom_decl(*id).ok()?;
            match &decl.kind {
                // a bare scalar atom, or the bytes word
                crate::AtomKind::Unit => {
                    if decl.name == "bytes" {
                        Some(TypeWord::Bytes)
                    } else {
                        ScalarWord::by_name(&decl.name).map(TypeWord::Scalar)
                    }
                },
                // a heap-reference constructor applied to scalar members
                crate::AtomKind::Apply(ctor, args) => {
                    let ctor = decls.universe.atom_decl(*ctor).ok()?;
                    let scalar = |ty| {
                        let [member] = decls.universe.members(ty) else {
                            return None;
                        };
                        let name = decls.universe.atom_decl(*member).ok()?.name.clone();
                        ScalarWord::by_name(&name)
                    };
                    if ctor.name == "list" {
                        let [arg] = &**args else {
                            return None;
                        };
                        scalar(*arg).map(TypeWord::List)
                    } else if ctor.name == "map" {
                        let [key, value] = &**args else {
                            return None;
                        };
                        Some(TypeWord::Map(scalar(*key)?, scalar(*value)?))
                    } else {
                        None
                    }
                },
                _ => None,
            }
        },
        // `t | none` in either order is exactly `t?` (B4): the optional
        // scalar's presence-tag word
        crate::TypeExpr::Union(left, right) => {
            let atom = |expr: &crate::TypeExpr| match expr {
                crate::TypeExpr::Atom(id) => Some(*id),
                _ => None,
            };
            let (left, right) = (atom(left)?, atom(right)?);
            let scalar = if right == crate::TypeId::NONE {
                left
            } else if left == crate::TypeId::NONE {
                right
            } else {
                return None;
            };
            let name = decls.universe.atom_decl(scalar).ok()?.name.clone();
            ScalarWord::by_name(&name).map(TypeWord::Optional)
        },
        _ => None,
    }
}

/// One written type's word kind, when it spells directly in the
/// vocabulary by name — the alias body's resolver. A path segments
/// through exactly one ambient spelling (a scalar, `bytes`, or the `list`
/// /`map` constructors over scalar members), an optional marker wraps
/// one scalar, and parenthesization peels; every other written shape (a
/// union, a tuple, a fn type, a multi-segment path) answers `None`.
/// Spelling-resolution equals universe-resolution only because ambient
/// names are unshadowable (`Universe::declare_atom` refuses
/// `DuplicateName`): resolving the body in the consuming universe
/// would admit alias-of-alias chains this refusal excludes.
fn word_of_written(ty: &ds_parse::Type) -> Option<TypeWord> {
    let scalar_path = |ty: &ds_parse::Type| -> Option<ScalarWord> {
        let ds_parse::Type::Path { path, args, .. } = ty else {
            return None;
        };
        let [only] = path.segments.as_slice() else {
            return None;
        };
        if !args.is_empty() {
            return None;
        }
        ScalarWord::by_name(&only.name)
    };
    match ty {
        ds_parse::Type::Path { path, args, .. } => {
            let [only] = path.segments.as_slice() else {
                return None;
            };
            match only.name.as_str() {
                "bytes" if args.is_empty() => Some(TypeWord::Bytes),
                name if args.is_empty() => ScalarWord::by_name(name).map(TypeWord::Scalar),
                "list" => {
                    let [arg] = &**args else {
                        return None;
                    };
                    scalar_path(arg).map(TypeWord::List)
                },
                "map" => {
                    let [key, value] = &**args else {
                        return None;
                    };
                    Some(TypeWord::Map(scalar_path(key)?, scalar_path(value)?))
                },
                _ => None,
            }
        },
        ds_parse::Type::Optional { inner, .. } => scalar_path(inner).map(TypeWord::Optional),
        ds_parse::Type::Paren { inner, .. } => word_of_written(inner),
        _ => None,
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
    /// The linked module's tag-table rows, name and id: the shared tag
    /// mapping's identity. A union-typed word crossing the module
    /// boundary carries the producing module's tag ids as immediates,
    /// so the consuming compile's own table must reserve each row's id
    /// for the atom the linked facts mint under the same name — the
    /// seed runs at lowering, before any body mints. Rows the consuming
    /// compile mints no atom for (the producer's private atoms) ride
    /// verbatim, consuming their ids.
    pub tags: Vec<(String, u8)>,
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
    /// over every scalar word, `bytes`, a list of one scalar, a map of
    /// two scalars, an optional scalar, and the part-less struct; a
    /// generic struct, a linear declaration, an enum, a tag, and a field
    /// outside the vocabulary (a nested struct, a tuple, an optional over
    /// a wider word) carry nothing — those keep their selection
    /// refusals.
    #[test]
    fn the_fact_is_the_flat_struct_s_word_layout() {
        let source = concat!(
            "pub trait Copyable {\n",
            "}\n",
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
            "  label: string,\n",
            "  tags: list[string],\n",
            "  code: byte,\n",
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
            "}\n",
            "pub struct opt_list {\n",
            "  xs: list[int]?,\n",
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
            "pub tag point\n",
            "fn main() -> int {\n",
            "7\n",
            "}\n",
        );
        let mut files = ds_diag::Files::new();
        let file = files.add_source("type_fact.ds", source);
        let ast = ds_parse::parse(&files, file).expect("the source parses");
        let analyzed = crate::type_of::analyze(&files, &[ast]);
        let fact = |name: &str| {
            let def = analyzed
                .package
                .lookup_type(name)
                .unwrap_or_else(|| panic!("{name} declares"));
            super::TypeFact::of_struct(&analyzed.decls, def)
        };
        assert_eq!(
            fact("cell"),
            Some(super::TypeFact::Struct(vec![
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
                    word: super::TypeWord::List(super::ScalarWord::Str),
                },
                super::TypeField {
                    name: "code".to_owned(),
                    word: super::TypeWord::Scalar(super::ScalarWord::Byte),
                },
                super::TypeField {
                    name: "data".to_owned(),
                    word: super::TypeWord::Bytes,
                },
            ]))
        );
        assert_eq!(fact("bare"), Some(super::TypeFact::Struct(Vec::new())));
        assert_eq!(
            fact("keyed"),
            Some(super::TypeFact::Struct(vec![
                super::TypeField {
                    name: "index".to_owned(),
                    word: super::TypeWord::Map(super::ScalarWord::Str, super::ScalarWord::Int),
                },
                super::TypeField {
                    name: "seen".to_owned(),
                    word: super::TypeWord::Map(super::ScalarWord::Byte, super::ScalarWord::Byte),
                },
            ]))
        );
        assert_eq!(
            fact("opt"),
            Some(super::TypeFact::Struct(vec![super::TypeField {
                name: "n".to_owned(),
                word: super::TypeWord::Optional(super::ScalarWord::Int),
            }]))
        );
        for refused in [
            "boxed",
            "shade",
            "holder",
            "opt_list",
            "twin",
            "guard",
            "keyed_wide",
            "point",
            "list",
        ] {
            assert_eq!(fact(refused), None, "{refused} carries no fact");
        }
    }

    /// The enum vocabulary carries: a non-generic enum's tag rows — unit
    /// cases and struct payloads of word-shaped fields (`bytes` included,
    /// two payloaded cases too since each row names its case) — and each
    /// minted case its own fact naming its enum; a generic enum, a linear
    /// enum, an empty enum, a tuple payload, and a payload field outside
    /// the word vocabulary carry nothing — those keep their selection
    /// refusals.
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
            "}\n",
            "pub enum wide {\n",
            "  one { v: list[list[byte]] },\n",
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
                word: super::TypeWord::List(super::ScalarWord::Byte),
            },
            super::TypeField {
                name: "data".to_owned(),
                word: super::TypeWord::Bytes,
            },
        ];
        let enum_fact = |name: &str| {
            let def = analyzed
                .package
                .lookup_type(name)
                .unwrap_or_else(|| panic!("{name} declares"));
            super::TypeFact::of_enum(&analyzed.package, &analyzed.decls, def)
        };
        let case_fact = |name: &str, enum_name: &str| {
            let def = analyzed
                .package
                .lookup_type(name)
                .unwrap_or_else(|| panic!("{name} declares"));
            super::TypeFact::of_case(&analyzed.decls, def, enum_name)
        };
        assert_eq!(
            enum_fact("shade"),
            Some(super::TypeFact::Enum(vec![
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
            ]))
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
        // the refusals: generic, empty, linear, a tuple payload, and a
        // payload field outside the vocabulary
        for refused in ["pair", "bare", "guard"] {
            assert_eq!(enum_fact(refused), None, "{refused} carries no fact");
        }
        assert_eq!(
            case_fact("one", "trip"),
            None,
            "a tuple payload carries no fact"
        );
        assert_eq!(
            case_fact("one", "wide"),
            None,
            "a payload field outside the vocabulary carries no fact"
        );
    }

    /// The alias vocabulary carries: a non-generic alias whose body spells
    /// directly in the word vocabulary rides as its resolved word — the
    /// scalar, `bytes`, list, map, and optional spellings, parentheses
    /// peeling; a generic alias and a body naming anything else (another
    /// alias, a local struct, a union, a tuple) carry nothing. A bodiless
    /// struct carries the opaque identity (its atom stays shape-pending),
    /// distinct from the part-less struct's empty field list.
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
            "pub type name = string\n",
            "pub type raw = bytes\n",
            "pub type paren = (int)\n",
            "pub type pair[k, v] = map[k, v]\n",
            "pub type chain = row\n",
            "pub type local = cell\n",
            "pub type either = int | string\n",
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
        let alias = |name: &str| {
            let def = analyzed
                .package
                .lookup_type(name)
                .unwrap_or_else(|| panic!("{name} declares"));
            super::TypeFact::of_alias(&analyzed.decls, def)
        };
        assert_eq!(
            alias("row"),
            Some(super::TypeFact::Alias(Box::new(super::TypeWord::Map(
                super::ScalarWord::Str,
                super::ScalarWord::Str
            ))))
        );
        assert_eq!(
            alias("words"),
            Some(super::TypeFact::Alias(Box::new(super::TypeWord::List(
                super::ScalarWord::Str
            ))))
        );
        assert_eq!(
            alias("port"),
            Some(super::TypeFact::Alias(Box::new(super::TypeWord::Optional(
                super::ScalarWord::Int
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
        // the refusals: generic parameters, and bodies the vocabulary
        // cannot spell directly — another alias, a local struct, a
        // union, a tuple
        for refused in ["pair", "chain", "local", "either", "two"] {
            assert_eq!(alias(refused), None, "{refused} carries no fact");
        }
        let struct_fact = |name: &str| {
            let def = analyzed
                .package
                .lookup_type(name)
                .unwrap_or_else(|| panic!("{name} declares"));
            super::TypeFact::of_struct(&analyzed.decls, def)
        };
        assert_eq!(struct_fact("opaque"), Some(super::TypeFact::Opaque));
        assert_eq!(
            struct_fact("empty"),
            Some(super::TypeFact::Struct(Vec::new()))
        );
    }
}
