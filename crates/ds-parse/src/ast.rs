//! Spanned AST of the DreamScript decided surface.
//!
//! Every node carries a [`Span`]. Comments are attached as [`Trivia`] payload
//! (leading on the following item, trailing at container end) so the M1
//! formatter is lossless over comments; the `///` doc run a definition owns
//! is carried a second time in [`Item::doc`] — and a member's or field's in
//! its own `doc` slot — as attachment data (G3). A file
//! is a `Vec<Block>` of `##`-separated blocks (G3); a file without separators
//! is one block. Top-level definitions are order-independent data — no
//! ordering validation happens at parse time.

use ds_diag::{FileId, Span};

/// One parsed file: its blocks in source order.
#[derive(Clone, Debug)]
#[cfg_attr(feature = "ast-serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Ast {
    /// The file these blocks were parsed from.
    pub file: FileId,
    /// Span of the whole file.
    pub span: Span,
    /// `##`-separated blocks; at least one, possibly empty.
    pub blocks: Vec<Block>,
}

/// A `##`-delimited slice of a file (G3).
#[derive(Clone, Debug)]
#[cfg_attr(feature = "ast-serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Block {
    /// Span from the first item to the last (or the separator).
    pub span: Span,
    /// Items in source order.
    pub items: Vec<Item>,
    /// Comments after the last item, up to the separator or EOF.
    pub trailing_trivia: Vec<Trivia>,
}

/// A comment kept as AST payload for lossless formatting.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[cfg_attr(feature = "ast-serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Trivia {
    /// Source range of the comment.
    pub span: Span,
    /// Which comment syntax produced it.
    pub kind: TriviaKind,
}

/// Which comment syntax a [`Trivia`] came from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[cfg_attr(feature = "ast-serde", derive(serde::Serialize, serde::Deserialize))]
pub enum TriviaKind {
    /// `// ...` up to the line end.
    Line,
    /// `/* ... */`.
    Block,
}

/// A top-level definition, possibly attributed and visibility-annotated.
#[derive(Clone, Debug)]
#[cfg_attr(feature = "ast-serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Item {
    /// Span of the whole item including attributes and visibility.
    pub span: Span,
    /// `#[...]` attributes in source order.
    pub attrs: Vec<Attribute>,
    /// Visibility modifier, if any.
    pub vis: Option<Vis>,
    /// What is declared.
    pub kind: ItemKind,
    /// Comments before the item's first token.
    pub leading_trivia: Vec<Trivia>,
    /// The `///` run attached to this definition (G3): the maximal suffix
    /// of `leading_trivia` made of `///` line comments on consecutive
    /// source lines, the last immediately above the item's first token.
    /// The same trivia also stay in `leading_trivia` — the formatter's
    /// lossless channel — so this slot is pure attachment data for
    /// consumers; a run cut off by a blank line or another comment stays
    /// loose (absent here, present in `leading_trivia`).
    pub doc: Vec<Trivia>,
    /// Comments inside the item but outside any deeper attachment point.
    pub trailing_trivia: Vec<Trivia>,
}

/// A `#[name(args)]` attribute.
#[derive(Clone, Debug)]
#[cfg_attr(feature = "ast-serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Attribute {
    /// Span including `#[...]`.
    pub span: Span,
    /// Attribute name, e.g. `derive`.
    pub name: Ident,
    /// Comma-separated path arguments, e.g. `Hash, Show`.
    pub args: Vec<Path>,
}

/// Visibility of a definition (G4).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[cfg_attr(feature = "ast-serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Vis {
    /// `pub`: visible to dependents, enters the `.dsi`.
    Pub(Span),
    /// `pub(pkg)`: visible package-wide, never enters the `.dsi`.
    PubPkg(Span),
}

impl Vis {
    /// Span of the modifier.
    #[must_use]
    pub fn span(self) -> Span {
        match self {
            Self::Pub(s) | Self::PubPkg(s) => s,
        }
    }
}

/// One top-level definition.
#[derive(Clone, Debug)]
#[cfg_attr(feature = "ast-serde", derive(serde::Serialize, serde::Deserialize))]
pub enum ItemKind {
    /// `import path (sel)? as alias? if cond?` (G4).
    Import(ImportDecl),
    /// `const`/`var` binding (top level); also the converted shape of a
    /// bodied `fn name(params) -> t { ... }` declaration (F2) — one binding
    /// of one lambda, the named-function representation downstream.
    Local(LocalDecl),
    /// A bodiless `fn name(params) -> t` signature (top level); bodied
    /// declarations convert onto [`ItemKind::Local`].
    Fn(FnDecl),
    /// `extern fn name(params) -> t` (H5).
    ExternFn(FnDecl),
    /// `struct Name: supers? { fields, deinit? }` (B3).
    Struct(StructDecl),
    /// `enum Name { Case, Case(t), Case { f } }` (B3).
    Enum(EnumDecl),
    /// `tag Name` — unit tag (B1).
    Tag(TagDecl),
    /// `type Name = type` (F7).
    TypeAlias(TypeAliasDecl),
    /// `trait Name { type A; fn sig }` (B6).
    Trait(TraitDecl),
    /// `impl[g]? Trait? for? type { fn }` (B6).
    Impl(ImplDecl),
    /// `platform "std" provides <console, fsys, net, time>` (H5).
    Platform(PlatformDecl),
}

/// An import declaration (G4).
#[derive(Clone, Debug)]
#[cfg_attr(feature = "ast-serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ImportDecl {
    /// Span of the whole declaration.
    pub span: Span,
    /// Module path, e.g. `@std/csv` or `acme/pipeline/schema`.
    pub path: ImportPath,
    /// Selective symbols `(Row, parse_row)`, with per-symbol aliasing.
    pub select: Vec<ImportSel>,
    /// Module alias `as pio`.
    pub alias: Option<Ident>,
    /// Conditional import `if os == "windows"`.
    pub cond: Option<Expr>,
}

/// The `@std/csv`-style path of an import.
#[derive(Clone, Debug)]
#[cfg_attr(feature = "ast-serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ImportPath {
    /// Span of the whole path.
    pub span: Span,
    /// Whether the path starts with `@`.
    pub at_sign: bool,
    /// Slash-separated segments.
    pub segments: Vec<Ident>,
}

/// One entry of an import's selective list.
#[derive(Clone, Debug)]
#[cfg_attr(feature = "ast-serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ImportSel {
    /// Span of the entry.
    pub span: Span,
    /// Imported name.
    pub name: Ident,
    /// `as alias`, if any.
    pub alias: Option<Ident>,
}

/// A `const`/`var` binding statement or item.
#[derive(Clone, Debug)]
#[cfg_attr(feature = "ast-serde", derive(serde::Serialize, serde::Deserialize))]
pub struct LocalDecl {
    /// Span of the whole declaration.
    pub span: Span,
    /// `const` or `var`.
    pub mutable: bool,
    /// Binding pattern; usually a plain name, tuples destructure.
    pub pat: Pattern,
    /// Optional type annotation.
    pub ty: Option<Type>,
    /// Initializer expression.
    pub init: Expr,
}

/// A named function declaration: bodiless top-level signatures plus every
/// trait/impl member (bodied `fn` declarations at top level convert onto
/// [`ItemKind::Local`]).
#[derive(Clone, Debug)]
#[cfg_attr(feature = "ast-serde", derive(serde::Serialize, serde::Deserialize))]
pub struct FnDecl {
    /// Span of the whole declaration.
    pub span: Span,
    /// Function name.
    pub name: Ident,
    /// Declared generic parameters `[T: bound]`.
    pub generics: Vec<TypeParam>,
    /// Parameters.
    pub params: Vec<Param>,
    /// Return type after `->`.
    pub ret: Option<Type>,
    /// Body block; absent for signatures (traits, externs).
    pub body: Option<BlockExpr>,
    /// The `///` run attached to this member declaration (G3): filled for
    /// trait/impl member `fn`s, where the run immediately precedes the `fn`.
    /// Top-level declarations carry their doc in [`Item::doc`], so this slot
    /// stays empty there. The same trivia also stay in the enclosing item's
    /// deep trivia — pure attachment data, not a move.
    pub doc: Vec<Trivia>,
}

/// One declared generic parameter.
#[derive(Clone, Debug)]
#[cfg_attr(feature = "ast-serde", derive(serde::Serialize, serde::Deserialize))]
pub struct TypeParam {
    /// Span of the parameter.
    pub span: Span,
    /// Parameter name.
    pub name: Ident,
    /// Optional bound (`T: Ord`).
    pub bound: Option<Type>,
}

/// One parameter of a function or function type.
#[derive(Clone, Debug)]
#[cfg_attr(feature = "ast-serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Param {
    /// Span of the parameter.
    pub span: Span,
    /// Passing mode (C2): borrow (default), `mut`, consume (`move`),
    /// or `comptime`.
    pub mode: ParamMode,
    /// Parameter name.
    pub name: Ident,
    /// Optional type annotation.
    pub ty: Option<Type>,
    /// Optional literal-only default `x: t = lit` (F7); conversion rejects
    /// non-literal defaults, so a `Some` value is always a literal shape.
    pub default: Option<Expr>,
}

/// A parameter's passing mode (C2).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[cfg_attr(feature = "ast-serde", derive(serde::Serialize, serde::Deserialize))]
pub enum ParamMode {
    /// Default immutable borrow.
    Borrow,
    /// `mut`: exclusive mutable window.
    Mut(Span),
    /// `move`: consume (C2); exact spelling is a parse-time liberal choice.
    Consume(Span),
    /// `comptime`: compile-time parameter (G1).
    Comptime(Span),
}

/// A struct declaration.
#[derive(Clone, Debug)]
#[cfg_attr(feature = "ast-serde", derive(serde::Serialize, serde::Deserialize))]
pub struct StructDecl {
    /// Span of the whole declaration.
    pub span: Span,
    /// Struct name.
    pub name: Ident,
    /// Declared generic parameters.
    pub generics: Vec<TypeParam>,
    /// Supertypes after `:` (traits, `~Copyable`).
    pub supers: Vec<Type>,
    /// Fields and optional `deinit` member, in order.
    pub members: Vec<StructMember>,
    /// Whether a `{ ... }` body is present (`struct dog: animal` has none).
    pub has_body: bool,
}

/// One member of a struct body.
#[derive(Clone, Debug)]
#[cfg_attr(feature = "ast-serde", derive(serde::Serialize, serde::Deserialize))]
pub enum StructMember {
    /// `name: type` field.
    Field(FieldDecl),
    /// `deinit { ... }` (C5).
    Deinit {
        /// Span of `deinit { ... }`.
        span: Span,
        /// The teardown block.
        body: BlockExpr,
    },
}

/// A struct field or struct-pattern field.
#[derive(Clone, Debug)]
#[cfg_attr(feature = "ast-serde", derive(serde::Serialize, serde::Deserialize))]
pub struct FieldDecl {
    /// Span of `name: type`.
    pub span: Span,
    /// Field name.
    pub name: Ident,
    /// Field type.
    pub ty: Type,
    /// The `///` run attached to this field (G3): filled for struct-body
    /// fields and enum-case payload fields, where the run immediately
    /// precedes the field. The same trivia also stay in the enclosing
    /// item's deep trivia — pure attachment data, not a move.
    pub doc: Vec<Trivia>,
}

/// An enum declaration.
#[derive(Clone, Debug)]
#[cfg_attr(feature = "ast-serde", derive(serde::Serialize, serde::Deserialize))]
pub struct EnumDecl {
    /// Span of the whole declaration.
    pub span: Span,
    /// Enum name.
    pub name: Ident,
    /// Declared generic parameters.
    pub generics: Vec<TypeParam>,
    /// Supertypes after `:`.
    pub supers: Vec<Type>,
    /// Cases in source order.
    pub cases: Vec<EnumCase>,
}

/// One case of an enum.
#[derive(Clone, Debug)]
#[cfg_attr(feature = "ast-serde", derive(serde::Serialize, serde::Deserialize))]
pub struct EnumCase {
    /// Span of the case.
    pub span: Span,
    /// Case name.
    pub name: Ident,
    /// Tuple payload types, struct payload fields, or none.
    pub payload: EnumPayload,
}

/// The payload of an enum case.
#[derive(Clone, Debug)]
#[cfg_attr(feature = "ast-serde", derive(serde::Serialize, serde::Deserialize))]
pub enum EnumPayload {
    /// No payload.
    None,
    /// `Case(t1, t2)` tuple payload.
    Tuple(Vec<Type>),
    /// `Case { f: t }` struct payload.
    Struct(Vec<FieldDecl>),
}

/// A platform declaration (H5): the platform a package is checked
/// against. The checker verifies the package's effect usage is a subset
/// of the platform's declared row — the provides list names the world
/// capabilities the platform serves, an absent list providing nothing
/// (the embed target).
#[derive(Clone, Debug)]
#[cfg_attr(feature = "ast-serde", derive(serde::Serialize, serde::Deserialize))]
pub struct PlatformDecl {
    /// Span of the whole declaration.
    pub span: Span,
    /// The platform's name, from its plain string literal (e.g. `std`).
    pub name: PlatformName,
    /// The provided world capabilities, in declaration order.
    pub provides: Vec<Ident>,
}

/// The platform's name: one plain string literal's text.
#[derive(Clone, Debug)]
#[cfg_attr(feature = "ast-serde", derive(serde::Serialize, serde::Deserialize))]
pub struct PlatformName {
    /// Span of the string literal.
    pub span: Span,
    /// The name's text.
    pub name: String,
}

/// A unit tag declaration (B1).
#[derive(Clone, Debug)]
#[cfg_attr(feature = "ast-serde", derive(serde::Serialize, serde::Deserialize))]
pub struct TagDecl {
    /// Span of the declaration.
    pub span: Span,
    /// Tag name.
    pub name: Ident,
}

/// A type alias (F7).
#[derive(Clone, Debug)]
#[cfg_attr(feature = "ast-serde", derive(serde::Serialize, serde::Deserialize))]
pub struct TypeAliasDecl {
    /// Span of the whole declaration.
    pub span: Span,
    /// Alias name.
    pub name: Ident,
    /// Declared generic parameters.
    pub generics: Vec<TypeParam>,
    /// Aliased type expression.
    pub ty: Type,
}

/// A trait declaration (B6).
#[derive(Clone, Debug)]
#[cfg_attr(feature = "ast-serde", derive(serde::Serialize, serde::Deserialize))]
pub struct TraitDecl {
    /// Span of the whole declaration.
    pub span: Span,
    /// Trait name.
    pub name: Ident,
    /// Declared generic parameters.
    pub generics: Vec<TypeParam>,
    /// Supertraits after `:`.
    pub supers: Vec<Type>,
    /// Members in source order.
    pub members: Vec<TraitMember>,
}

/// One trait member.
#[derive(Clone, Debug)]
#[cfg_attr(feature = "ast-serde", derive(serde::Serialize, serde::Deserialize))]
pub enum TraitMember {
    /// Associated type `type A` (B6).
    AssocType {
        /// Span of the member.
        span: Span,
        /// Associated type name.
        name: Ident,
    },
    /// Function signature, with optional default body. Boxed: [`FnDecl`] is
    /// far past the member enum's other variant, and every consumer binds
    /// it by reference (deref coercion), so the indirection costs one
    /// pointer per member slot.
    Fn(Box<FnDecl>),
}

/// An impl block (B6): inherent (`impl list<t> { ... }`) or
/// trait (`impl[T: ord] ord for list<t> { ... }`).
#[derive(Clone, Debug)]
#[cfg_attr(feature = "ast-serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ImplDecl {
    /// Span of the whole declaration.
    pub span: Span,
    /// Declared generic parameters.
    pub generics: Vec<TypeParam>,
    /// The trait being implemented, if this is a trait impl.
    pub trait_ref: Option<TypeRef>,
    /// The implementing type.
    pub target: Type,
    /// Implemented functions.
    pub fns: Vec<FnDecl>,
}

/// A type expression (B1, B4).
#[derive(Clone, Debug)]
#[cfg_attr(feature = "ast-serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Type {
    /// Nominal path, optionally generic: `list<int>`.
    Path {
        /// Span of the whole type.
        span: Span,
        /// The path.
        path: Path,
        /// Generic arguments.
        args: Vec<Type>,
    },
    /// `t?` — exactly `t | none` (B4).
    Optional {
        /// Span of `t?`.
        span: Span,
        /// The inner type.
        inner: Box<Type>,
    },
    /// `~t` negated constraint (C1).
    Negation {
        /// Span of `~t`.
        span: Span,
        /// The negated type.
        inner: Box<Type>,
    },
    /// `a | b` union.
    Union {
        /// Span of `a | b`.
        span: Span,
        /// Left side.
        left: Box<Type>,
        /// Right side.
        right: Box<Type>,
    },
    /// `a & b` intersection.
    Intersect {
        /// Span of `a & b`.
        span: Span,
        /// Left side.
        left: Box<Type>,
        /// Right side.
        right: Box<Type>,
    },
    /// `a - b` difference (compile-time only) (B1).
    Difference {
        /// Span of `a - b`.
        span: Span,
        /// Left side.
        left: Box<Type>,
        /// Right side.
        right: Box<Type>,
    },
    /// Parenthesized type.
    Paren {
        /// Span including parens.
        span: Span,
        /// Inner type.
        inner: Box<Type>,
    },
    /// Anonymous tuple type `(a, b)` (B3).
    Tuple {
        /// Span including parens.
        span: Span,
        /// Element types.
        items: Vec<Type>,
    },
    /// `()`, the unit type.
    Unit(Span),
    /// Function type `fn(mut file) -> t`.
    Fn {
        /// Span of the whole type.
        span: Span,
        /// Parameters.
        params: Vec<Param>,
        /// Return type.
        ret: Option<Box<Type>>,
    },
    /// Raw pointer `*const t` / `*mut t` (extern surface) (H5).
    Ptr {
        /// Span of the whole type.
        span: Span,
        /// Whether the pointee is mutable.
        mutable: bool,
        /// Pointee type.
        inner: Box<Type>,
    },
    /// The `type` keyword as a type atom (`comptime T: type`) (G2).
    TypeKw(Span),
    /// Opaque foreign type named by a string argument, `c["sqlite3"]` (H5).
    Foreign {
        /// Span of the whole reference.
        span: Span,
        /// The cooked name.
        name: String,
    },
}

impl Type {
    /// Span of the type expression.
    #[must_use]
    pub fn span(&self) -> Span {
        match self {
            Self::Path { span, .. }
            | Self::Optional { span, .. }
            | Self::Negation { span, .. }
            | Self::Union { span, .. }
            | Self::Intersect { span, .. }
            | Self::Difference { span, .. }
            | Self::Paren { span, .. }
            | Self::Tuple { span, .. }
            | Self::Fn { span, .. }
            | Self::Ptr { span, .. }
            | Self::Unit(span)
            | Self::Foreign { span, .. }
            | Self::TypeKw(span) => *span,
        }
    }
}

/// A path of `::`-separated identifiers.
#[derive(Clone, PartialEq, Eq, Debug)]
#[cfg_attr(feature = "ast-serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Path {
    /// Span of the whole path.
    pub span: Span,
    /// Segments.
    pub segments: Vec<Ident>,
}

impl Path {
    /// Renders the path back to source text (`a::b`).
    #[must_use]
    pub fn text(&self) -> String {
        self.segments
            .iter()
            .map(|s| s.name.as_str())
            .collect::<Vec<_>>()
            .join("::")
    }
}

/// A trait reference in an impl head: path plus optional generic args.
#[derive(Clone, Debug)]
#[cfg_attr(feature = "ast-serde", derive(serde::Serialize, serde::Deserialize))]
pub struct TypeRef {
    /// Span of the reference.
    pub span: Span,
    /// Trait path.
    pub path: Path,
    /// Generic arguments.
    pub args: Vec<Type>,
}

/// An identifier with its span.
#[derive(Clone, PartialEq, Eq, Debug)]
#[cfg_attr(feature = "ast-serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Ident {
    /// Span of the name.
    pub span: Span,
    /// The name text.
    pub name: String,
}

/// A pattern (F4).
#[derive(Clone, Debug)]
#[cfg_attr(feature = "ast-serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Pattern {
    /// `_`.
    Wildcard(Span),
    /// A binding name.
    Binding {
        /// Span of the name.
        span: Span,
        /// Bound name.
        name: Ident,
    },
    /// A literal (numbers, strings, chars, `true`, `false`).
    Literal {
        /// Span of the literal.
        span: Span,
        /// The literal.
        lit: Literal,
    },
    /// A string literal pattern; holes are parsed but rejected by checking.
    Str {
        /// Span including quotes.
        span: Span,
        /// Parts in source order.
        parts: Vec<StrPart>,
    },
    /// A raw string pattern.
    RawStr {
        /// Span including delimiters.
        span: Span,
        /// Hash count of the delimiters.
        hashes: u32,
        /// Raw content.
        text: String,
    },
    /// A unit case or enum-case reference by path.
    Path {
        /// Span of the path.
        span: Span,
        /// The path.
        path: Path,
    },
    /// A tuple-case pattern `BadNumber(s)`.
    Call {
        /// Span of the whole pattern.
        span: Span,
        /// Case path.
        path: Path,
        /// Payload sub-patterns.
        args: Vec<Pattern>,
    },
    /// A struct pattern `BadPort { got: p }`, with punning.
    Struct {
        /// Span of the whole pattern.
        span: Span,
        /// Struct path.
        path: Path,
        /// Fields; `None` pattern means punning (`name` binds `name`).
        fields: Vec<(Ident, Option<Pattern>)>,
    },
    /// A tuple or parenthesized pattern.
    Tuple {
        /// Span including parens.
        span: Span,
        /// Sub-patterns; one element without a trailing comma is a paren.
        pats: Vec<Pattern>,
        /// Whether a trailing comma forced tuple-ness.
        trailing_comma: bool,
    },
    /// An or-pattern `a | b`.
    Or {
        /// Span of the whole pattern.
        span: Span,
        /// Alternatives.
        pats: Vec<Pattern>,
    },
}

/// One match arm (F4).
#[derive(Clone, Debug)]
#[cfg_attr(feature = "ast-serde", derive(serde::Serialize, serde::Deserialize))]
pub struct MatchArm {
    /// Span of the whole arm.
    pub span: Span,
    /// The pattern.
    pub pat: Pattern,
    /// Optional type annotation on the binding (`b: BadPort => ...`).
    pub ann: Option<Type>,
    /// Optional `if cond` guard.
    pub guard: Option<Expr>,
    /// Arm body expression (a block or any expression).
    pub body: Expr,
}

/// A `{ ... }` block; its value is the final expression (F2).
#[derive(Clone, Debug)]
#[cfg_attr(feature = "ast-serde", derive(serde::Serialize, serde::Deserialize))]
pub struct BlockExpr {
    /// Span including braces.
    pub span: Span,
    /// Statements in source order.
    pub stmts: Vec<Stmt>,
    /// Comments after the last statement, before the closing brace.
    pub trailing_trivia: Vec<Trivia>,
}

/// One statement inside a block.
#[derive(Clone, Debug)]
#[cfg_attr(feature = "ast-serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Stmt {
    /// Span of the statement.
    pub span: Span,
    /// What the statement is.
    pub kind: StmtKind,
    /// Comments before the statement's first token.
    pub leading_trivia: Vec<Trivia>,
    /// Comments inside the statement but outside any nested block.
    pub trailing_trivia: Vec<Trivia>,
}

/// A statement is a binding or an expression (F1).
#[derive(Clone, Debug)]
#[cfg_attr(feature = "ast-serde", derive(serde::Serialize, serde::Deserialize))]
pub enum StmtKind {
    /// `const`/`var` binding.
    Local(LocalDecl),
    /// Any expression, including assignments and control forms.
    Expr(Expr),
}

/// An expression.
#[derive(Clone, Debug)]
#[cfg_attr(feature = "ast-serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Expr {
    /// Integer literal (B7); text is unparsed digits plus suffix.
    Literal {
        /// Span of the literal.
        span: Span,
        /// The literal.
        lit: Literal,
    },
    /// Interpolating or plain string; holes are structured (F6).
    Str {
        /// Span including quotes.
        span: Span,
        /// Parts in source order.
        parts: Vec<StrPart>,
    },
    /// A raw string; no escapes, no interpolation (F6).
    RawStr {
        /// Span including `r`, hashes, and quotes.
        span: Span,
        /// Hash count of the delimiters.
        hashes: u32,
        /// The raw content between the delimiters.
        text: String,
    },
    /// A lambda: `(x, y: t) -> t { ... }` or `x => e` (F2).
    Lambda {
        /// Span of the whole lambda.
        span: Span,
        /// Declared generic parameters, for generic const-arrow functions.
        generics: Vec<TypeParam>,
        /// Parameters.
        params: Vec<Param>,
        /// Declared return type (implies a block body).
        ret: Option<Type>,
        /// Whether `=>` separated the body.
        arrow: bool,
        /// Body expression.
        body: Box<Expr>,
    },
    /// A path expression (`x`, `Trait::f`) (F3).
    Path {
        /// Span of the path.
        span: Span,
        /// The path.
        path: Path,
    },
    /// A struct literal `User { name, age: 1 }` with punning.
    Struct {
        /// Span of the whole literal.
        span: Span,
        /// Struct path.
        path: Path,
        /// Fields; `None` expr means punning.
        fields: Vec<(Ident, Option<Expr>)>,
    },
    /// A tuple or parenthesized expression.
    Tuple {
        /// Span including parens.
        span: Span,
        /// Elements; empty is the unit value.
        items: Vec<Expr>,
        /// Whether a trailing comma forced tuple-ness.
        trailing_comma: bool,
    },
    /// A parenthesized expression, kept distinct for lossless formatting.
    Paren {
        /// Span including parens.
        span: Span,
        /// Inner expression.
        inner: Box<Expr>,
    },
    /// A list literal `[a, b, c]` (F7, B3); the empty form types only
    /// against an expected `list[t]`.
    List {
        /// Span including brackets.
        span: Span,
        /// Elements in source order.
        items: Vec<Expr>,
    },
    /// A call `f(args)`; `trailing` is the block-argument lambda (F7).
    Call {
        /// Span of the whole call.
        span: Span,
        /// Called expression.
        callee: Box<Expr>,
        /// Arguments; named arguments follow positional ones (F7).
        args: Vec<CallArg>,
        /// Trailing block argument, if present.
        trailing: Option<Box<Expr>>,
    },
    /// Field access `x.f` — for calls, UFCS applies (F3).
    Field {
        /// Span of `x.f`.
        span: Span,
        /// Receiver.
        recv: Box<Expr>,
        /// Field name.
        name: Ident,
    },
    /// Index `xs[i]` (F5).
    Index {
        /// Span of `xs[i]`.
        span: Span,
        /// Indexed expression.
        recv: Box<Expr>,
        /// Index expression.
        index: Box<Expr>,
    },
    /// The map constructor's type application `map[k, v]` (F9, B3): the
    /// one expression-position application with two type arguments, pinned
    /// to the head spelled `map` by the grammar — no other head takes a
    /// two-element bracket. Call position constructs the empty map
    /// (`map[k, v]()`) exactly like `list[t]()`; the bracketed arguments
    /// are type spellings in expression position, the list application's
    /// own convention.
    MapApply {
        /// Span of `map[k, v]`.
        span: Span,
        /// The applied head; always the spelled `map`.
        head: Ident,
        /// The key type argument.
        key: Box<Expr>,
        /// The value type argument.
        value: Box<Expr>,
    },
    /// A fn type spelled in expression position: the constructor
    /// brackets admit it (`list[fn(int) -> int]()`,
    /// `map[string, fn(int) -> int]`) because `fn` heads no value
    /// expression, so the node nests only inside those brackets. Value
    /// positions reject it.
    FnType {
        /// Span of the fn type.
        span: Span,
        /// The fn type itself (always `Type::Fn`).
        ty: Box<Type>,
    },
    /// Postfix `?` propagation (E4).
    Try {
        /// Span of `e?`.
        span: Span,
        /// Operand.
        expr: Box<Expr>,
    },
    /// Unary `-`, `-%`, `!` (F5, B7).
    Unary {
        /// Span including the operator.
        span: Span,
        /// Operator.
        op: UnaryOp,
        /// Operand.
        expr: Box<Expr>,
    },
    /// `expr is type` narrowing test (F7); the right side is a type.
    Is {
        /// Span of the whole test.
        span: Span,
        /// Tested expression.
        expr: Box<Expr>,
        /// Tested-against type.
        ty: Box<Type>,
    },
    /// A binary operation (arithmetic, shifts, comparison, `in`,
    /// logical, bitwise) (F5).
    Binary {
        /// Span of the whole expression.
        span: Span,
        /// Operator.
        op: BinaryOp,
        /// Left side.
        left: Box<Expr>,
        /// Right side.
        right: Box<Expr>,
    },
    /// A range `a..b` or `a..=b` (F7).
    Range {
        /// Span of the whole range.
        span: Span,
        /// Whether the end is inclusive.
        inclusive: bool,
        /// Start.
        start: Box<Expr>,
        /// End.
        end: Box<Expr>,
    },
    /// An assignment `x = e` or compound `x += e`.
    Assign {
        /// Span of the whole assignment.
        span: Span,
        /// Operator.
        op: AssignOp,
        /// Place expression.
        left: Box<Expr>,
        /// Value.
        right: Box<Expr>,
    },
    /// `if cond { a } else { b }` — an expression (F1).
    If {
        /// Span of the whole expression.
        span: Span,
        /// Condition.
        cond: Box<Expr>,
        /// Then block.
        then: BlockExpr,
        /// `else` block or `else if` chain.
        else_: Option<Box<Expr>>,
    },
    /// `match scrutinee { arms }` — an expression (F4).
    Match {
        /// Span of the whole expression.
        span: Span,
        /// Scrutinee.
        scrutinee: Box<Expr>,
        /// Arms.
        arms: Vec<MatchArm>,
    },
    /// `while cond { body }`.
    While {
        /// Span of the whole expression.
        span: Span,
        /// Condition.
        cond: Box<Expr>,
        /// Body.
        body: BlockExpr,
    },
    /// `for pat in iter { body }` or `for (pat in iter) body` (F7).
    For {
        /// Span of the whole expression.
        span: Span,
        /// Binding pattern.
        pat: Pattern,
        /// Iterable.
        iter: Box<Expr>,
        /// Body.
        body: Box<Expr>,
    },
    /// `return e?` — also the `or return e` tail (F2, E4).
    Return {
        /// Span of the whole return.
        span: Span,
        /// Returned value, if any.
        expr: Option<Box<Expr>>,
    },
    /// `break`.
    Break(Span),
    /// `continue`.
    Continue(Span),
    /// `nursery { ... }` (E2).
    Nursery {
        /// Span of the whole expression.
        span: Span,
        /// Body.
        body: BlockExpr,
    },
    /// `spawn { ... }` (E2).
    Spawn {
        /// Span of the whole expression.
        span: Span,
        /// Body.
        body: BlockExpr,
    },
    /// `defer { ... }` scope-exit sugar (E5).
    Defer {
        /// Span of the whole expression.
        span: Span,
        /// Body.
        body: BlockExpr,
    },
    /// A block used as an expression.
    Block(BlockExpr),
}

impl Expr {
    /// Span of the expression.
    #[must_use]
    pub fn span(&self) -> Span {
        match self {
            Self::Literal { span, .. }
            | Self::Str { span, .. }
            | Self::RawStr { span, .. }
            | Self::Lambda { span, .. }
            | Self::Path { span, .. }
            | Self::Struct { span, .. }
            | Self::Tuple { span, .. }
            | Self::List { span, .. }
            | Self::Paren { span, .. }
            | Self::Call { span, .. }
            | Self::Field { span, .. }
            | Self::Index { span, .. }
            | Self::MapApply { span, .. }
            | Self::FnType { span, .. }
            | Self::Try { span, .. }
            | Self::Unary { span, .. }
            | Self::Binary { span, .. }
            | Self::Is { span, .. }
            | Self::Range { span, .. }
            | Self::Assign { span, .. }
            | Self::If { span, .. }
            | Self::Match { span, .. }
            | Self::While { span, .. }
            | Self::For { span, .. }
            | Self::Return { span, .. }
            | Self::Nursery { span, .. }
            | Self::Spawn { span, .. }
            | Self::Defer { span, .. }
            | Self::Break(span)
            | Self::Continue(span) => *span,
            Self::Block(b) => b.span,
        }
    }
}

/// One argument of a call (F7): positional, or named (`f(x: 1)`).
#[derive(Clone, Debug)]
#[cfg_attr(feature = "ast-serde", derive(serde::Serialize, serde::Deserialize))]
pub struct CallArg {
    /// Span of the whole argument.
    pub span: Span,
    /// The name of a named argument; `None` for positional.
    pub name: Option<Ident>,
    /// The argument value.
    pub value: Expr,
}

/// One part of an interpolated string (F6).
#[derive(Clone, Debug)]
#[cfg_attr(feature = "ast-serde", derive(serde::Serialize, serde::Deserialize))]
pub enum StrPart {
    /// Literal text with escapes already cooked.
    Text {
        /// Span of the text run.
        span: Span,
        /// Cooked text.
        text: String,
    },
    /// `${expr}` hole.
    Expr {
        /// Span of the hole.
        span: Span,
        /// The hole's expression.
        expr: Box<Expr>,
    },
    /// `$x` bare-identifier hole.
    Ident {
        /// Span of `$name`.
        span: Span,
        /// The identifier (without `$`).
        name: Ident,
    },
}

/// A non-string literal.
#[derive(Clone, Debug)]
#[cfg_attr(feature = "ast-serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Literal {
    /// Integer (B7): raw digits text (radix prefix kept) and suffix.
    Int {
        /// Suffix text, e.g. `i64`; empty when unsuffixed.
        suffix: String,
        /// Raw digit text without separators, e.g. `0xff`.
        text: String,
    },
    /// Float (B7): raw mantissa/exponent text and suffix.
    Float {
        /// Suffix text, e.g. `f32`; empty when unsuffixed.
        suffix: String,
        /// Raw digit text without separators.
        text: String,
    },
    /// `'x'` Unicode scalar with escapes cooked.
    Char(char),
    /// `b'x'` byte with escapes cooked.
    Byte(u8),
    /// `true` / `false`.
    Bool(bool),
}

/// Unary operator (F5, B7).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[cfg_attr(feature = "ast-serde", derive(serde::Serialize, serde::Deserialize))]
pub enum UnaryOp {
    /// Negate (traps on int-min).
    Neg,
    /// Negate wrapping (B7).
    WrapNeg,
    /// Boolean not.
    Not,
}

/// Binary operator — the frozen set (F5).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[cfg_attr(feature = "ast-serde", derive(serde::Serialize, serde::Deserialize))]
pub enum BinaryOp {
    /// `+` (traps).
    Add,
    /// `+%` wrapping (B7).
    WrapAdd,
    /// `-`.
    Sub,
    /// `-%` wrapping.
    WrapSub,
    /// `*`.
    Mul,
    /// `*%` wrapping.
    WrapMul,
    /// `/` (traps on zero).
    Div,
    /// `%` (traps on zero).
    Rem,
    /// `<<` (traps past width).
    Shl,
    /// `>>`.
    Shr,
    /// `==` structural (B9).
    Eq,
    /// `!=`.
    Ne,
    /// `<`.
    Lt,
    /// `<=`.
    Le,
    /// `>`.
    Gt,
    /// `>=`.
    Ge,
    /// `x in y` containment (F7).
    In,
    /// `&&`.
    And,
    /// `||`.
    Or,
    /// `&` bitwise.
    BitAnd,
    /// `|` bitwise.
    BitOr,
    /// `^` bitwise.
    BitXor,
}

/// Assignment operator.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[cfg_attr(feature = "ast-serde", derive(serde::Serialize, serde::Deserialize))]
pub enum AssignOp {
    /// `=`.
    Plain,
    /// `+=`.
    Add,
    /// `-=`.
    Sub,
    /// `*=`.
    Mul,
    /// `/=`.
    Div,
    /// `%=`.
    Rem,
    /// `<<=`.
    Shl,
    /// `>>=`.
    Shr,
}
