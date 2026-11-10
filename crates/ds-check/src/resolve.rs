//! Package-level name resolution over the parsed surface [G3, F3, F9].
//!
//! A [`Package`] collects the declarations of its files into TWO
//! namespaces [G3, F9]: a type namespace (structs, enums, enum cases,
//! tags, aliases, traits) and a value namespace (functions, const/var
//! bindings). Files in
//! one package share both, there are zero intra-package imports, and
//! top-level definitions are order-independent — collection inserts every
//! declaration before any use is resolved, so intra-file and cross-file
//! definition cycles are legal by construction (resolution is name lookup,
//! not dependency ordering; the "whole-package fixpoint" of the record is
//! exactly this two-pass shape).
//!
//! # Namespace split [F9]
//!
//! Constructor positions resolve against the TYPE namespace only: brace
//! construction (`user { ... }`), bare tag references, pattern paths, and
//! generic-application constructors (`list[page]()`, whose head and type
//! arguments are type uses). A call-form name resolves against the VALUE
//! namespace (a function call), else the TYPE namespace (a constructor
//! call — tag or payload construction); present in BOTH it is an
//! ambiguity error (DS2014), never a priority search. A bare (non-call)
//! use resolves VALUE-first, then TYPE — a bare tag is a
//! value-producing constructor, while a struct/enum/trait name reached at
//! a bare use resolves so the checker can word its "type used as a value"
//! error. `struct map` and `fn map` therefore coexist, each resolving in
//! its own positions.
//!
//! # Method tier [F3]
//!
//! UFCS resolution is fixed: a dot-call `x.f(args)` feeds the receiver to
//! the first parameter of a FREE function named `f` if one exists (the
//! dot position is a function position — the VALUE namespace only), else
//! the registered method candidates (trait members, plus inherent impl
//! fns); `Trait::f(x)` qualifies directly. Trait methods are REGISTERED
//! here only — selection among candidates is the checker's type-directed
//! work, and F3's true-ambiguity error therefore lives there too, never as
//! a priority search here. The fed parameter index is recorded (always 0
//! today — capability parameters have no syntax yet, so the first
//! parameter IS the first data parameter; the checker enforces the
//! exclusion once they do).
//!
//! # Local scoping [F2]
//!
//! Params, lets, lambda parameters, match-arm binders, and for-patterns
//! bind into lexical scopes. Same-scope redefinition is an error (both
//! spans named); nested scopes may shadow outer bindings and package
//! names. An initializer is resolved BEFORE its own binders bind
//! (`const x = x` uses the outer `x`); the defined name of a const-arrow
//! function is in scope inside its own body via the package table
//! (recursion). Generic parameters bind in a separate type-variable scope
//! stack for type positions; a trait member's signatures additionally bind
//! `Self` as the self type [F2, record 6.1]. Or-pattern alternatives rebind
//! identical names into one arm scope without error; whether alternatives
//! must bind the SAME set is the checker's [F4].
//!
//! # Positions that do not resolve here
//!
//! Bare field accesses (`x.f` without a call) are struct-owned and
//! type-directed — the checker resolves them. `.err(h)`/`.ok(h)` are
//! decided surface handler forms [section 10 item 2], not names, and are
//! recorded as [`Res::Handler`]. Imports [G4] are M7: their aliases do not
//! enter the namespace yet. Multi-segment paths beyond `Trait::f` are
//! module paths, also M7. Enum case tails (`net_err::timeout`) resolve
//! their head; case-name existence is the checker's.
//!
//! # Example
//!
//! ```
//! use ds_check::{Package, Res, Resolutions};
//! use ds_diag::Files;
//! use ds_parse::parse;
//!
//! # fn main() -> Result<(), Vec<ds_diag::Diagnostic>> {
//! let mut files = Files::new();
//! let file = files.add_source(
//!     "m.ds",
//!     "tag timeout\nconst wait = (t: timeout) -> timeout { t }\n",
//! );
//! let ast = parse(&files, file)?;
//!
//! let mut package = Package::new();
//! assert!(package.add_file(&ast).is_empty());
//! let (_, diags) = package.resolve(&ast);
//! assert!(diags.is_empty());
//! # Ok(())
//! # }
//! ```

use std::collections::HashMap;

use ds_diag::{Diagnostic, ErrorCode, Span, Suggestion};
use ds_parse::{
    Ast, Expr, FnDecl, Ident, Item, ItemKind, Param, Path, Pattern, Stmt, StmtKind, StructMember,
    TraitMember, Type, TypeParam,
};

/// Identity of one package-level definition inside its [`Package`].
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct DefId(u32);

impl DefId {
    /// Builds an id from a raw table index (serialization, tests).
    #[must_use]
    pub const fn from_raw(index: u32) -> Self {
        Self(index)
    }

    /// Raw index inside its package; also its declaration order.
    #[must_use]
    pub const fn index(self) -> u32 {
        self.0
    }
}

/// What a top-level declaration defines.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DefKind {
    /// A named function or extern function.
    Fn,
    /// A `const` binding (const-arrow functions included — they are
    /// bindings whose value is a function [F2]).
    Const,
    /// A `var` binding.
    Var,
    /// A struct.
    Struct,
    /// An enum.
    Enum,
    /// A unit tag [B1].
    Tag,
    /// A case minted by an enum block [B3]: a member of the tag
    /// namespace like a tag, constructed per its payload.
    Case,
    /// A trait [B6].
    Trait,
    /// A type alias [F7].
    TypeAlias,
}

impl DefKind {
    /// Whether the kind declares into the type namespace [F9]; the rest
    /// declare into the value namespace.
    #[must_use]
    pub const fn in_type_namespace(self) -> bool {
        matches!(
            self,
            Self::Struct | Self::Enum | Self::Tag | Self::Case | Self::Trait | Self::TypeAlias
        )
    }

    /// Whether the kind mints an atom a bare pattern can test: traits and
    /// aliases are sets, not constructors.
    #[must_use]
    pub const fn is_constructor(self) -> bool {
        matches!(self, Self::Struct | Self::Enum | Self::Tag | Self::Case)
    }

    /// The diagnostic word for the kind ("struct", "function", ...).
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Fn => "function",
            Self::Const | Self::Var => "binding",
            Self::Struct => "struct",
            Self::Enum => "enum",
            Self::Tag => "tag",
            Self::Case => "enum case",
            Self::Trait => "trait",
            Self::TypeAlias => "type alias",
        }
    }
}

/// One package-level definition: name, kind, and defining name span.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Def {
    /// The declared name.
    pub name: String,
    /// What the declaration defines.
    pub kind: DefKind,
    /// Span of the defining name occurrence; carries the file.
    pub span: Span,
}

/// Identity of one registered method inside its [`Package`].
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct MethodId(u32);

impl MethodId {
    /// Builds an id from a raw table index (serialization, tests).
    #[must_use]
    pub const fn from_raw(index: u32) -> Self {
        Self(index)
    }

    /// Raw index inside its package; also its declaration order.
    #[must_use]
    pub const fn index(self) -> u32 {
        self.0
    }
}

/// Which declaration a registered method belongs to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MethodOwner {
    /// A trait member function; trait impls do not re-register — the
    /// trait's own signature already provides the candidate name.
    Trait(DefId),
    /// A function of an inherent impl (`impl t { fn ... }`).
    Inherent,
}

/// One registered method: a trait member or an inherent impl function.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct MethodDecl {
    /// The method name as called after the dot.
    pub name: String,
    /// The owning trait or inherent impl.
    pub owner: MethodOwner,
    /// Span of the method's name; carries the file.
    pub span: Span,
}

/// What one name use resolved to.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Res {
    /// A package-level definition.
    Def(DefId),
    /// A local binder (param, let, lambda, match arm, for pattern, or type
    /// variable); `bind` is the binder's span.
    Local {
        /// Span of the introducing binder.
        bind: Span,
    },
    /// The `Trait::f` escape hatch [F3]: the named trait's member.
    TraitMethod(MethodId),
    /// The callee name of a UFCS dot-call [F3].
    Dot(DotRes),
    /// The `.err(h)`/`.ok(h)` handler surface form [section 10 item 2];
    /// no name sits behind it.
    Handler,
}

/// The resolution of a dot-call's method position [F3].
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum DotRes {
    /// The free-function tier won: the receiver feeds parameter
    /// `fed_param` (0-based). Always 0 today: capability parameters have
    /// no surface yet, so the first parameter is the first data parameter;
    /// the checker enforces the dot-position exclusion once they do.
    Free {
        /// The winning free function (or const binding of one).
        func: DefId,
        /// Index of the parameter the receiver feeds.
        fed_param: usize,
    },
    /// No free function; the registered method candidates, in declaration
    /// order. Selection is the checker's type-directed work.
    Methods {
        /// Candidate method ids: trait members and inherent impl fns.
        candidates: Vec<MethodId>,
    },
}

/// The side table a resolution pass produces: use span to resolution.
/// Checker passes consume it; nothing in it mutates the package.
#[derive(Clone, PartialEq, Eq, Default, Debug)]
pub struct Resolutions {
    uses: HashMap<Span, Res>,
}

impl Resolutions {
    /// The resolution recorded at a use span, if any.
    #[must_use]
    pub fn get(&self, span: Span) -> Option<&Res> {
        self.uses.get(&span)
    }

    /// Number of recorded uses.
    #[must_use]
    pub fn len(&self) -> usize {
        self.uses.len()
    }

    /// Whether no use was recorded.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.uses.is_empty()
    }
}

/// A package: the declarations of its files in the type and value
/// namespaces [G3, F9], plus the method-tier registries [F3]. Collection
/// (`add_file`) is order-independent; use resolution (`resolve`) runs
/// against the complete tables, which is what makes intra-package
/// definition cycles legal.
#[derive(Clone, PartialEq, Eq, Default, Debug)]
pub struct Package {
    files: Vec<ds_diag::FileId>,
    defs: Vec<Def>,
    type_names: HashMap<String, DefId>,
    value_names: HashMap<String, DefId>,
    methods: Vec<MethodDecl>,
    method_index: HashMap<String, Vec<MethodId>>,
}

/// Resolves one package [G3]: collects every file's declarations into one
/// namespace, then resolves every file's name uses against the complete
/// tables — collection is order-independent and resolution runs last,
/// which is what makes intra-package definition cycles legal. The
/// resolutions align with `asts` positionally. The diagnostics are the
/// collection class (duplicate definitions) and the resolution class; the
/// typed half ([`crate::type_of::check_resolved`]) adds the declaration
/// and body classes.
#[must_use]
pub fn resolve_package<T>(asts: &[T]) -> (Package, Vec<Resolutions>, Vec<Diagnostic>)
where
    T: std::borrow::Borrow<Ast>,
{
    let mut package = Package::new();
    let mut diags = Vec::new();
    for ast in asts {
        diags.extend(package.add_file(ast.borrow()));
    }
    let mut resolutions = Vec::with_capacity(asts.len());
    for ast in asts {
        let (uses, mut file_diags) = package.resolve(ast.borrow());
        resolutions.push(uses);
        diags.append(&mut file_diags);
    }
    (package, resolutions, diags)
}

impl Package {
    /// An empty package.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Collects one parsed file's declarations into the namespace,
    /// returning duplicate-definition diagnostics as they are met (both
    /// spans named; the first definition is the secondary label). Imports
    /// contribute nothing yet [G4, M7].
    pub fn add_file(&mut self, ast: &Ast) -> Vec<Diagnostic> {
        self.files.push(ast.file);
        let mut diags = Vec::new();
        for block in &ast.blocks {
            for item in &block.items {
                self.collect_item(item, &mut diags);
            }
        }
        diags
    }

    /// Resolves one parsed file's name uses against the complete package
    /// tables, returning the side table and the diagnostics in walk order.
    #[must_use]
    pub fn resolve(&self, ast: &Ast) -> (Resolutions, Vec<Diagnostic>) {
        let mut resolver = FileResolver {
            package: self,
            uses: HashMap::new(),
            diags: Vec::new(),
            scopes: vec![HashMap::new()],
            type_scopes: vec![HashMap::new()],
        };
        for block in &ast.blocks {
            for item in &block.items {
                resolver.item(item);
            }
        }
        (
            Resolutions {
                uses: resolver.uses,
            },
            resolver.diags,
        )
    }

    /// The declaration of a definition.
    #[must_use]
    pub fn def(&self, id: DefId) -> &Def {
        &self.defs[id.index() as usize]
    }

    /// The declaration of a registered method.
    #[must_use]
    pub fn method(&self, id: MethodId) -> &MethodDecl {
        &self.methods[id.index() as usize]
    }

    /// The definition of a type-namespace name, if any [F9].
    #[must_use]
    pub fn lookup_type(&self, name: &str) -> Option<DefId> {
        self.type_names.get(name).copied()
    }

    /// The definition of a value-namespace name, if any [F9].
    #[must_use]
    pub fn lookup_value(&self, name: &str) -> Option<DefId> {
        self.value_names.get(name).copied()
    }

    /// Whether either namespace declares the name — the sentinel checks
    /// (`none`, `self`) that must not shadow universe spellings.
    #[must_use]
    pub fn declares(&self, name: &str) -> bool {
        self.type_names.contains_key(name) || self.value_names.contains_key(name)
    }

    /// The namespace-agnostic lookup: the type declaration when the name
    /// has one, else the value declaration. For consumers that cannot say
    /// which namespace they mean (a name's atom, a legacy plan); call
    /// sites that know their position use [`Self::lookup_type`] or
    /// [`Self::lookup_value`].
    #[must_use]
    pub fn lookup(&self, name: &str) -> Option<DefId> {
        self.lookup_type(name).or_else(|| self.lookup_value(name))
    }

    /// The ids of the files collected so far, in addition order.
    #[must_use]
    pub fn files(&self) -> &[ds_diag::FileId] {
        &self.files
    }

    /// The registered candidates for a method name, in declaration order.
    #[must_use]
    pub fn method_candidates(&self, name: &str) -> &[MethodId] {
        self.method_index
            .get(name)
            .map_or(&[] as &[MethodId], Vec::as_slice)
    }

    /// Number of package-level definitions.
    #[must_use]
    pub fn len(&self) -> usize {
        self.defs.len()
    }

    /// Whether the package has no definitions.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.defs.is_empty()
    }

    fn collect_item(&mut self, item: &Item, diags: &mut Vec<Diagnostic>) {
        match &item.kind {
            ItemKind::Import(_) => {},
            ItemKind::Local(local) => {
                let kind = if local.mutable { DefKind::Var } else { DefKind::Const };
                self.collect_pattern_defs(&local.pat, kind, diags);
            },
            ItemKind::Fn(f) | ItemKind::ExternFn(f) => {
                self.declare(&f.name, DefKind::Fn, diags);
            },
            ItemKind::Struct(s) => {
                self.declare(&s.name, DefKind::Struct, diags);
            },
            ItemKind::Enum(e) => {
                self.declare(&e.name, DefKind::Enum, diags);
                // Generic enums keep the opaque atom: their case names do
                // not mint yet (substitution over minted case shapes is
                // future work), so they collect no defs.
                if e.generics.is_empty() {
                    for case in &e.cases {
                        self.declare(&case.name, DefKind::Case, diags);
                    }
                }
            },
            ItemKind::Tag(t) => {
                self.declare(&t.name, DefKind::Tag, diags);
            },
            ItemKind::TypeAlias(a) => {
                self.declare(&a.name, DefKind::TypeAlias, diags);
            },
            ItemKind::Trait(t) => {
                let id = self.declare(&t.name, DefKind::Trait, diags);
                for member in &t.members {
                    if let TraitMember::Fn(f) = member {
                        self.register_method(&f.name, MethodOwner::Trait(id), diags);
                    }
                }
            },
            ItemKind::Impl(imp) => {
                // Trait impls do not re-register: the trait's signatures
                // already provide the candidate names.
                if imp.trait_ref.is_none() {
                    for f in &imp.fns {
                        self.register_method(&f.name, MethodOwner::Inherent, diags);
                    }
                }
            },
        }
    }

    /// Declares one name into its kind's namespace [F9]; a duplicate
    /// WITHIN that namespace reports both spans and keeps the first
    /// definition — the same name in the other namespace coexists.
    fn declare(&mut self, name: &Ident, kind: DefKind, diags: &mut Vec<Diagnostic>) -> DefId {
        self.declare_name(&name.name, name.span, kind, diags)
    }

    fn declare_name(
        &mut self,
        name: &str,
        span: Span,
        kind: DefKind,
        diags: &mut Vec<Diagnostic>,
    ) -> DefId {
        let names =
            if kind.in_type_namespace() { &mut self.type_names } else { &mut self.value_names };
        if let Some(&first) = names.get(name) {
            let first_span = self.defs[first.index() as usize].span;
            diags.push(
                Diagnostic::error(
                    ErrorCode::DuplicateDefinition,
                    format!("`{name}` is already defined"),
                )
                .with_secondary(first_span, "first defined here")
                .with_primary(span, "redefined here"),
            );
            return first;
        }
        let id = DefId::from_raw(u32::try_from(self.defs.len()).expect("table index fits u32"));
        self.defs.push(Def {
            name: name.to_owned(),
            kind,
            span,
        });
        names.insert(name.to_owned(), id);
        id
    }

    /// Registers one method name; a same-name same-owner registration is a
    /// duplicate (two inherent impls, or one trait declaring a member
    /// twice) that reports both spans and keeps the first — the discipline
    /// of [`Self::declare`]. Two traits may share a method name; call
    /// sites disambiguate by receiver [F3].
    fn register_method(&mut self, name: &Ident, owner: MethodOwner, diags: &mut Vec<Diagnostic>) {
        if let Some(&id) = self.method_index.get(&name.name).and_then(|ids| {
            ids.iter()
                .find(|&&id| self.methods[id.index() as usize].owner == owner)
        }) {
            let first_span = self.methods[id.index() as usize].span;
            diags.push(
                Diagnostic::error(
                    ErrorCode::DuplicateDefinition,
                    format!("method `{}` is already defined", name.name),
                )
                .with_secondary(first_span, "first defined here")
                .with_primary(name.span, "redefined here"),
            );
            return;
        }
        let id =
            MethodId::from_raw(u32::try_from(self.methods.len()).expect("table index fits u32"));
        self.method_index
            .entry(name.name.clone())
            .or_default()
            .push(id);
        self.methods.push(MethodDecl {
            name: name.name.clone(),
            owner,
            span: name.span,
        });
    }

    /// Top-level binding patterns declare one def per bound name, through
    /// the shared binder walker so pun fields and case sub-patterns
    /// declare like any other binder shape.
    fn collect_pattern_defs(&mut self, pat: &Pattern, kind: DefKind, diags: &mut Vec<Diagnostic>) {
        for (span, name) in crate::pattern::pattern_binders(pat) {
            self.declare_name(name, span, kind, diags);
        }
    }
}

/// One file's resolution pass: walks items with lexical scopes and records
/// every name use.
struct FileResolver<'a> {
    package: &'a Package,
    uses: HashMap<Span, Res>,
    diags: Vec<Diagnostic>,
    /// Value scopes, innermost last: binder name to binder span.
    scopes: Vec<HashMap<String, Span>>,
    /// Type-variable scopes, innermost last.
    type_scopes: Vec<HashMap<String, Span>>,
}

/// What a bare identifier in a pattern names.
enum ArmTag {
    /// No constructor entry: the identifier binds.
    Binding,
    /// The universe's predeclared `none` atom, which has no def.
    BuiltinNone,
    /// A constructor type's definition.
    Def(DefId),
}

impl FileResolver<'_> {
    fn item(&mut self, item: &Item) {
        match &item.kind {
            ItemKind::Import(_) | ItemKind::Tag(_) => {},
            ItemKind::Local(local) => {
                // A top-level binding declares a PACKAGE def [G3], not a
                // local: the initializer and annotation resolve here, the
                // binders bind nowhere, and uses reach the name through
                // the package table.
                self.expr(&local.init);
                if let Some(ty) = &local.ty {
                    self.type_(ty);
                }
            },
            ItemKind::Fn(f) | ItemKind::ExternFn(f) => self.fn_decl(f, false),
            ItemKind::Struct(s) => {
                self.type_params(&s.generics);
                for ty in &s.supers {
                    self.type_(ty);
                }
                for member in &s.members {
                    match member {
                        StructMember::Field(field) => self.type_(&field.ty),
                        // `self` inside a deinit body is the dying
                        // instance [C5], borrowed; the walk never types it,
                        // so binding the name here is all resolution owes.
                        StructMember::Deinit { span, body } => {
                            self.scopes.push(HashMap::new());
                            self.bind(
                                &Ident {
                                    span: *span,
                                    name: "self".to_owned(),
                                },
                                false,
                            );
                            self.block(body);
                            self.scopes.pop();
                        },
                    }
                }
                self.type_scopes.pop();
            },
            ItemKind::Enum(e) => {
                self.type_params(&e.generics);
                for ty in &e.supers {
                    self.type_(ty);
                }
                for case in &e.cases {
                    match &case.payload {
                        ds_parse::EnumPayload::None => {},
                        ds_parse::EnumPayload::Tuple(types) => {
                            for ty in types {
                                self.type_(ty);
                            }
                        },
                        ds_parse::EnumPayload::Struct(fields) => {
                            for field in fields {
                                self.type_(&field.ty);
                            }
                        },
                    }
                }
                self.type_scopes.pop();
            },
            ItemKind::TypeAlias(a) => {
                self.type_params(&a.generics);
                self.type_(&a.ty);
                self.type_scopes.pop();
            },
            ItemKind::Trait(t) => {
                self.type_params(&t.generics);
                for ty in &t.supers {
                    self.type_(ty);
                }
                // A trait member's signatures spell the self type `Self`
                // [F2, record 6.1].
                self.type_scopes
                    .push(HashMap::from([("Self".to_owned(), t.name.span)]));
                for member in &t.members {
                    match member {
                        TraitMember::AssocType { .. } => {},
                        TraitMember::Fn(f) => self.fn_decl(f, true),
                    }
                }
                self.type_scopes.pop();
                self.type_scopes.pop();
            },
            ItemKind::Impl(imp) => {
                self.type_params(&imp.generics);
                if let Some(trait_ref) = &imp.trait_ref {
                    for arg in &trait_ref.args {
                        self.type_(arg);
                    }
                    self.path_use(&trait_ref.path);
                }
                // An impl's members spell the self type `Self` [F2, record
                // 6.1], trait and inherent alike.
                self.type_scopes
                    .push(HashMap::from([("Self".to_owned(), imp.span)]));
                self.type_(&imp.target);
                for f in &imp.fns {
                    self.fn_decl(f, true);
                }
                self.type_scopes.pop();
                self.type_scopes.pop();
            },
        }
    }

    /// Binds generic type parameters in a fresh type scope.
    fn type_params(&mut self, params: &[TypeParam]) {
        let mut scope = HashMap::new();
        for param in params {
            self.record_dup(&mut scope, &param.name);
            if let Some(bound) = &param.bound {
                self.type_(bound);
            }
        }
        self.type_scopes.push(scope);
    }

    fn fn_decl(&mut self, f: &FnDecl, member: bool) {
        self.type_params(&f.generics);
        self.scopes.push(HashMap::new());
        for (index, param) in f.params.iter().enumerate() {
            self.param(param, member && index == 0);
        }
        if let Some(ret) = &f.ret {
            self.type_(ret);
        }
        if let Some(body) = &f.body {
            self.block(body);
        }
        self.scopes.pop();
        self.type_scopes.pop();
    }

    fn param(&mut self, param: &Param, receiver: bool) {
        if let Some(ty) = &param.ty {
            // The annotated receiver's `self` annotation is the checker's
            // to report (the receiver ruling names the new spelling); a
            // plain `self` path here would double-report.
            let bare_self = matches!(
                ty,
                Type::Path { path, args, .. }
                    if args.is_empty()
                        && path.segments.len() == 1
                        && path.segments[0].name == "self"
            );
            if !(receiver && bare_self) {
                self.type_(ty);
            }
        }
        self.bind(&param.name, false);
    }

    fn local(&mut self, local: &ds_parse::LocalDecl) {
        // The initializer resolves before the binders bind.
        self.expr(&local.init);
        if let Some(ty) = &local.ty {
            self.type_(ty);
        }
        self.bind_pattern(&local.pat, false);
    }

    fn block(&mut self, block: &ds_parse::BlockExpr) {
        self.scopes.push(HashMap::new());
        for stmt in &block.stmts {
            self.stmt(stmt);
        }
        self.scopes.pop();
    }

    fn stmt(&mut self, stmt: &Stmt) {
        match &stmt.kind {
            StmtKind::Local(local) => self.local(local),
            StmtKind::Expr(expr) => self.expr(expr),
        }
    }

    // ---- value positions ---------------------------------------------------

    fn expr(&mut self, expr: &Expr) {
        match expr {
            Expr::Literal { .. } | Expr::RawStr { .. } | Expr::Break(_) | Expr::Continue(_) => {},
            Expr::Str { parts, .. } => {
                for part in parts {
                    match part {
                        ds_parse::StrPart::Text { .. } => {},
                        ds_parse::StrPart::Expr { expr, .. } => self.expr(expr),
                        // `$x` bare holes are a value use of the name.
                        ds_parse::StrPart::Ident { name, .. } => self.name_use(name),
                    }
                }
            },
            Expr::Lambda {
                generics,
                params,
                ret,
                body,
                ..
            } => {
                self.type_params(generics);
                self.scopes.push(HashMap::new());
                for param in params {
                    self.param(param, false);
                }
                if let Some(ret) = ret {
                    self.type_(ret);
                }
                self.expr(body);
                self.scopes.pop();
                self.type_scopes.pop();
            },
            Expr::Path { path, .. } => self.path_use(path),
            Expr::Struct { path, fields, .. } => {
                self.ctor_path_use(path);
                for (name, value) in fields {
                    match value {
                        // Punning: the field name is also a value use.
                        None => self.name_use(name),
                        Some(expr) => self.expr(expr),
                    }
                }
            },
            Expr::List { items, .. } | Expr::Tuple { items, .. } => {
                for item in items {
                    self.expr(item);
                }
            },
            Expr::Paren { inner, .. } => self.expr(inner),
            Expr::Call {
                callee,
                args,
                trailing,
                ..
            } => {
                match callee.as_ref() {
                    // UFCS: a dot-callee gets tiered resolution [F3].
                    Expr::Field { recv, name, .. } => {
                        self.expr(recv);
                        self.dot_use(name);
                    },
                    // A call-form name resolves per the namespace split
                    // [F9]; a generic-application constructor head is a
                    // type use [F9].
                    Expr::Path { path, .. } if path.segments.len() == 1 => {
                        self.call_name_use(&path.segments[0]);
                    },
                    Expr::Index { recv, index, .. } => self.ctor_apply_use(recv, index),
                    Expr::MapApply {
                        head, key, value, ..
                    } => self.map_apply_use(head, key, value),
                    _ => self.expr(callee),
                }
                for arg in args {
                    self.expr(&arg.value);
                }
                if let Some(tail) = trailing {
                    self.expr(tail);
                }
            },
            Expr::Field { recv, .. } => {
                // Bare field access: struct-owned, type-directed later.
                self.expr(recv);
            },
            Expr::Index { recv, index, .. } => {
                self.expr(recv);
                self.expr(index);
            },
            Expr::MapApply {
                head, key, value, ..
            } => self.map_apply_use(head, key, value),
            Expr::Try { expr, .. } | Expr::Unary { expr, .. } => self.expr(expr),
            Expr::Is { expr, ty, .. } => {
                self.expr(expr);
                self.type_(ty);
            },
            Expr::Binary { left, right, .. }
            | Expr::Range {
                start: left,
                end: right,
                ..
            }
            | Expr::Assign { left, right, .. } => {
                self.expr(left);
                self.expr(right);
            },
            Expr::If {
                cond, then, else_, ..
            } => {
                self.expr(cond);
                self.block(then);
                if let Some(else_expr) = else_ {
                    self.expr(else_expr);
                }
            },
            Expr::Match {
                scrutinee, arms, ..
            } => {
                self.expr(scrutinee);
                for arm in arms {
                    // Each arm is its own scope nested in the enclosing one.
                    self.scopes.push(HashMap::new());
                    self.arm_pattern(&arm.pat);
                    if let Some(ann) = &arm.ann {
                        self.type_(ann);
                    }
                    if let Some(guard) = &arm.guard {
                        self.expr(guard);
                    }
                    self.expr(&arm.body);
                    self.scopes.pop();
                }
            },
            Expr::While { cond, body, .. } => {
                self.expr(cond);
                self.block(body);
            },
            Expr::For {
                pat, iter, body, ..
            } => {
                self.expr(iter);
                self.scopes.push(HashMap::new());
                self.bind_pattern(pat, false);
                self.expr(body);
                self.scopes.pop();
            },
            Expr::Return { expr, .. } => {
                if let Some(expr) = expr {
                    self.expr(expr);
                }
            },
            Expr::Nursery { body, .. } | Expr::Spawn { body, .. } | Expr::Defer { body, .. } => {
                self.block(body);
            },
            Expr::Block(block) => self.block(block),
        }
    }

    /// A plain identifier use: locals shadow package names, the value
    /// namespace comes next, and the type namespace last — a bare tag is a
    /// value-producing constructor, and a struct/enum/trait name reached
    /// here still resolves so the checker words its "type used as a value"
    /// error [F9]. Bare `none` is the universe's distinguished unit atom
    /// [B4], not a package name; the checker types it directly, so it
    /// records nothing here.
    fn name_use(&mut self, name: &Ident) {
        let res = if let Some(bind) = self.lookup_local(&name.name) {
            Res::Local { bind }
        } else if let Some(def) = self.package.lookup_value(&name.name) {
            Res::Def(def)
        } else if let Some(def) = self.package.lookup_type(&name.name) {
            Res::Def(def)
        } else if name.name == "none" {
            return;
        } else {
            self.unknown_name(name, self.shadowing_candidates());
            return;
        };
        self.uses.insert(name.span, res);
    }

    /// The callee of a call form [F9]: locals first (an indirect call of a
    /// bound function value), then the value namespace, then the type
    /// namespace (a constructor call — tag or payload construction).
    /// Present in BOTH namespaces the call is ambiguous — an error naming
    /// both declarations, never a priority search.
    fn call_name_use(&mut self, name: &Ident) {
        if let Some(bind) = self.lookup_local(&name.name) {
            self.uses.insert(name.span, Res::Local { bind });
            return;
        }
        let res = match (
            self.package.lookup_value(&name.name),
            self.package.lookup_type(&name.name),
        ) {
            (Some(value), Some(ty)) => {
                self.namespace_collision(name, value, ty);
                // Still record the value declaration so the checker types
                // the call through and the ambiguity stays the single
                // diagnostic; the error is fatal.
                Res::Def(value)
            },
            (Some(value), None) => Res::Def(value),
            (None, Some(ty)) => Res::Def(ty),
            (None, None) => {
                if name.name == "none" {
                    return;
                }
                self.unknown_name(name, self.callable_candidates());
                return;
            },
        };
        self.uses.insert(name.span, res);
    }

    /// A call-form name present in both namespaces: DS2014, naming both
    /// declarations [F9].
    fn namespace_collision(&mut self, name: &Ident, value: DefId, ty: DefId) {
        let value_def = self.package.def(value);
        let ty_def = self.package.def(ty);
        self.diags.push(
            Diagnostic::error(
                ErrorCode::AmbiguousCall,
                format!(
                    "call-form `{}` is ambiguous between the value `{}` and the {} `{}` [F9]",
                    name.name,
                    value_def.name,
                    ty_def.kind.word(),
                    ty_def.name
                ),
            )
            .with_primary(name.span, "ambiguous call form")
            .with_secondary(value_def.span, "the value declaration")
            .with_secondary(ty_def.span, "the type declaration"),
        );
    }

    /// A generic-application constructor `list[page]()` [F9]: the head
    /// resolves in the type namespace and the bracketed arguments are type
    /// uses. A head that names no type is an ordinary index expression
    /// (`xs[i]`), whose typing is the checker's.
    fn ctor_apply_use(&mut self, recv: &Expr, index: &Expr) {
        let head = single_path_ident(recv);
        if let Some(head) = head.filter(|id| self.package.lookup_type(&id.name).is_some()) {
            self.type_name_use(head);
            self.ctor_type_arg(index);
        } else {
            self.expr(recv);
            self.expr(index);
        }
    }

    /// The map constructor's type application `map[k, v]` [F9, B3]: the
    /// head is a type use and the two bracketed arguments are type uses,
    /// the list application's own convention.
    fn map_apply_use(&mut self, head: &Ident, key: &Expr, value: &Expr) {
        self.type_name_use(head);
        self.ctor_type_arg(key);
        self.ctor_type_arg(value);
    }

    /// One bracketed constructor argument: a type spelled in expression
    /// position — a path is a type use, a tuple walks its items, anything
    /// else falls back to the expression walk.
    fn ctor_type_arg(&mut self, arg: &Expr) {
        match arg {
            Expr::Path { path, .. } if path.segments.len() == 1 => {
                self.type_name_use(&path.segments[0]);
            },
            Expr::Paren { inner, .. } => self.ctor_type_arg(inner),
            Expr::Tuple { items, .. } => {
                for item in items {
                    self.ctor_type_arg(item);
                }
            },
            _ => self.expr(arg),
        }
    }

    /// A path expression: one segment is a name use; `Trait::f` qualifies
    /// directly [F3]; longer paths are module paths [G4, M7].
    fn path_use(&mut self, path: &Path) {
        match path.segments.as_slice() {
            [only] => self.name_use(only),
            [head, method] => {
                let Some(def) = self.package.lookup_type(&head.name) else {
                    // A value name at the qualifier position still gets
                    // the sharper "not a trait" word.
                    if self.package.lookup_value(&head.name).is_some() {
                        self.diags.push(
                            Diagnostic::error(
                                ErrorCode::UnresolvedName,
                                format!(
                                    "`{}` is not a trait; `A::f` is the trait-method escape hatch \
                                     [F3]",
                                    head.name
                                ),
                            )
                            .with_primary(head.span, "not a trait"),
                        );
                        return;
                    }
                    self.unknown_name(head, self.shadowing_candidates());
                    return;
                };
                if self.package.def(def).kind != DefKind::Trait {
                    self.diags.push(
                        Diagnostic::error(
                            ErrorCode::UnresolvedName,
                            format!(
                                "`{}` is not a trait; `A::f` is the trait-method escape hatch [F3]",
                                head.name
                            ),
                        )
                        .with_primary(head.span, "not a trait"),
                    );
                    return;
                }
                let candidate = self
                    .package
                    .method_candidates(&method.name)
                    .iter()
                    .copied()
                    .find(|&id| self.package.method(id).owner == MethodOwner::Trait(def));
                let Some(id) = candidate else {
                    let others = self
                        .package
                        .method_candidates(&method.name)
                        .iter()
                        .copied()
                        .filter(|&id| self.package.method(id).owner != MethodOwner::Trait(def))
                        .map(|id| self.package.method(id).owner_name(self.package))
                        .collect::<Vec<_>>();
                    let mut diag = Diagnostic::error(
                        ErrorCode::UnresolvedName,
                        format!("trait `{}` has no method `{}`", head.name, method.name),
                    )
                    .with_primary(method.span, "no such member");
                    if !others.is_empty() {
                        diag = diag.with_note(format!(
                            "method `{}` exists on: {}",
                            method.name,
                            others.join(", ")
                        ));
                    }
                    self.diags.push(diag);
                    return;
                };
                self.uses.insert(head.span, Res::Def(def));
                self.uses.insert(method.span, Res::TraitMethod(id));
            },
            _ => {
                self.diags.push(
                    Diagnostic::error(
                        ErrorCode::UnresolvedName,
                        "multi-segment paths resolve through imports, which land with modules [G4]",
                    )
                    .with_primary(path.span, "module path"),
                );
            },
        }
    }

    /// A brace-construction head `user { ... }` [F9]: the type namespace
    /// only; longer paths are module paths [G4, M7].
    fn ctor_path_use(&mut self, path: &Path) {
        match path.segments.as_slice() {
            [only] => {
                if let Some(def) = self.package.lookup_type(&only.name) {
                    self.uses.insert(only.span, Res::Def(def));
                } else if only.name != "none" {
                    let candidates = self.type_candidates();
                    self.unknown_name_at(only, candidates, "struct");
                }
            },
            _ => {
                self.diags.push(
                    Diagnostic::error(
                        ErrorCode::UnresolvedName,
                        "qualified struct literals resolve through imports, which land with \
                         modules [G4]",
                    )
                    .with_primary(path.span, "qualified constructor path"),
                );
            },
        }
    }

    /// A UFCS dot-call's method position [F3]: free functions first, then
    /// registered methods; the `.err`/`.ok` handler forms are surface
    /// syntax, not names. The dot is a function position, so the free
    /// tier searches the VALUE namespace only [F9].
    fn dot_use(&mut self, name: &Ident) {
        if name.name == "err" || name.name == "ok" {
            self.uses.insert(name.span, Res::Handler);
            return;
        }
        if let Some(func) = self.package.lookup_value(&name.name) {
            // Capability parameters have no syntax yet, so the receiver
            // feeds parameter 0 — the first data parameter [F3].
            self.uses
                .insert(name.span, Res::Dot(DotRes::Free { func, fed_param: 0 }));
            return;
        }
        let candidates: Vec<MethodId> = self.package.method_candidates(&name.name).to_vec();
        if candidates.is_empty() {
            let mut diag = Diagnostic::error(
                ErrorCode::UnresolvedName,
                format!("no name `{}` behind the dot", name.name),
            )
            .with_primary(name.span, "not a free function, not a method")
            .with_note("tried free functions first, then trait and inherent methods [F3]");
            if let Some(near) = nearest(&name.name, self.callable_candidates()) {
                diag = diag.with_suggestion(Suggestion::Replace {
                    span: name.span,
                    with: near,
                });
            }
            self.diags.push(diag);
            return;
        }
        self.uses
            .insert(name.span, Res::Dot(DotRes::Methods { candidates }));
    }

    // ---- type positions ----------------------------------------------------

    fn type_(&mut self, ty: &Type) {
        match ty {
            Type::Path { path, args, .. } => {
                match path.segments.as_slice() {
                    [only] => self.type_name_use(only),
                    _ => {
                        self.diags.push(
                            Diagnostic::error(
                                ErrorCode::UnresolvedName,
                                "qualified type paths resolve through imports, which land with \
                                 modules [G4]",
                            )
                            .with_primary(path.span, "qualified type path"),
                        );
                    },
                }
                for arg in args {
                    self.type_(arg);
                }
            },
            Type::Optional { inner, .. }
            | Type::Negation { inner, .. }
            | Type::Paren { inner, .. }
            | Type::Ptr { inner, .. } => self.type_(inner),
            Type::Union { left, right, .. }
            | Type::Intersect { left, right, .. }
            | Type::Difference { left, right, .. } => {
                self.type_(left);
                self.type_(right);
            },
            Type::Tuple { items, .. } => {
                for item in items {
                    self.type_(item);
                }
            },
            Type::Fn { params, ret, .. } => {
                for param in params {
                    if let Some(ty) = &param.ty {
                        self.type_(ty);
                    }
                }
                if let Some(ret) = ret {
                    self.type_(ret);
                }
            },
            Type::Unit(_) | Type::TypeKw(_) | Type::Foreign { .. } => {},
        }
    }

    fn type_name_use(&mut self, name: &Ident) {
        let res = if let Some(bind) = self.lookup_type_var(&name.name) {
            Res::Local { bind }
        } else if let Some(def) = self.package.lookup_type(&name.name) {
            Res::Def(def)
        } else if name.name == "none" {
            // `none` is the universe's distinguished unit atom [B4], not a
            // package declaration; the checker solves it directly.
            return;
        } else if name.name == "self" {
            // `self` as a type name is retired; the receiver type is
            // spelled `Self` [F2, record 6.1].
            self.diags.push(
                Diagnostic::error(
                    ErrorCode::LowercaseSelfType,
                    "`self` is retired as a type name; the receiver type is spelled `Self`",
                )
                .with_primary(name.span, "type name `self`")
                .with_suggestion(Suggestion::Replace {
                    span: name.span,
                    with: "Self".to_owned(),
                }),
            );
            return;
        } else {
            let candidates = self.type_candidates();
            self.unknown_name_at(name, candidates, "type");
            return;
        };
        self.uses.insert(name.span, res);
    }

    // ---- patterns ----------------------------------------------------------

    /// Binds a pattern's binders into the current scope and resolves its
    /// case paths. `or_rebind` allows identical rebinding across
    /// or-pattern alternatives [F4].
    fn bind_pattern(&mut self, pat: &Pattern, or_rebind: bool) {
        self.pattern_at(pat, or_rebind, false);
    }

    /// A match-arm pattern: sub-patterns are member tests first, bindings
    /// second [F9].
    fn arm_pattern(&mut self, pat: &Pattern) {
        self.pattern_at(pat, false, true);
    }

    /// One pattern walk shared by binding declarations (`arm == false`)
    /// and match-arm tests (`arm == true`): in a test position a single
    /// identifier that names a constructor type (or the predeclared
    /// `none`) is a member test that binds nothing — bare tags are
    /// constructor values, so the type namespace wins over any
    /// same-named local; identifiers with no constructor entry bind.
    fn pattern_at(&mut self, pat: &Pattern, or_rebind: bool, arm: bool) {
        match pat {
            Pattern::Wildcard(_)
            | Pattern::Literal { .. }
            | Pattern::Str { .. }
            | Pattern::RawStr { .. } => {},
            Pattern::Binding { name, .. } => match self.arm_tag(name, arm) {
                ArmTag::Binding => self.bind(name, or_rebind),
                ArmTag::BuiltinNone => {},
                ArmTag::Def(def) => {
                    self.uses.insert(name.span, Res::Def(def));
                },
            },
            // Pattern paths are item references, not locals.
            Pattern::Path { path, .. } => self.pattern_path(path),
            Pattern::Call { path, args, .. } => {
                self.pattern_path(path);
                for arg in args {
                    self.pattern_at(arg, or_rebind, arm);
                }
            },
            Pattern::Struct { path, fields, .. } => {
                self.pattern_path(path);
                for (name, field) in fields {
                    match field {
                        // Punning binds the field name.
                        None => self.bind(name, or_rebind),
                        Some(sub) => self.pattern_at(sub, or_rebind, arm),
                    }
                }
            },
            Pattern::Tuple { pats, .. } => {
                for sub in pats {
                    self.pattern_at(sub, or_rebind, arm);
                }
            },
            Pattern::Or { pats, .. } => {
                for sub in pats {
                    self.pattern_at(sub, true, arm);
                }
            },
        }
    }

    /// The type-namespace entry a bare arm identifier names [F9]: the
    /// predeclared `none`, a constructor type's def, or nothing (a
    /// binding). Traits and aliases are sets, not constructors.
    fn arm_tag(&self, name: &Ident, arm: bool) -> ArmTag {
        if arm && name.name == "none" && !self.package.declares("none") {
            return ArmTag::BuiltinNone;
        }
        if !arm {
            return ArmTag::Binding;
        }
        self.package
            .lookup_type(&name.name)
            .filter(|&def| self.package.def(def).kind.is_constructor())
            .map_or(ArmTag::Binding, ArmTag::Def)
    }

    /// Pattern paths are constructor references [F9]: the type namespace
    /// only.
    fn pattern_path(&mut self, path: &Path) {
        let candidates = || {
            self.package
                .defs
                .iter()
                .filter(|def| def.kind.in_type_namespace())
                .map(|def| def.name.clone())
                .collect::<Vec<_>>()
        };
        match path.segments.as_slice() {
            [only] => {
                if let Some(def) = self.package.lookup_type(&only.name) {
                    self.uses.insert(only.span, Res::Def(def));
                } else {
                    let candidates = candidates();
                    self.unknown_name_at(only, candidates, "pattern");
                }
            },
            // The tail is a case name; case tables are the checker's.
            [head, _case] => {
                if let Some(def) = self.package.lookup_type(&head.name) {
                    self.uses.insert(head.span, Res::Def(def));
                } else {
                    let candidates = candidates();
                    self.unknown_name_at(head, candidates, "pattern");
                }
            },
            _ => {
                self.diags.push(
                    Diagnostic::error(
                        ErrorCode::UnresolvedName,
                        "multi-segment pattern paths are not part of the surface [F4]",
                    )
                    .with_primary(path.span, "too many segments"),
                );
            },
        }
    }

    // ---- scopes ----------------------------------------------------------

    fn bind(&mut self, name: &Ident, rebind_ok: bool) {
        let scope = self.scopes.last_mut().expect("a scope is always open");
        match scope.get(&name.name) {
            Some(&first) if !rebind_ok => {
                self.diags.push(
                    Diagnostic::error(
                        ErrorCode::DuplicateDefinition,
                        format!("`{}` is already bound in this scope", name.name),
                    )
                    .with_secondary(first, "first bound here")
                    .with_primary(name.span, "bound again here"),
                );
            },
            _ => {
                scope.insert(name.name.clone(), name.span);
            },
        }
    }

    /// Records a binder in `scope`, reporting same-scope duplicates.
    fn record_dup(&mut self, scope: &mut HashMap<String, Span>, name: &Ident) {
        if let Some(&first) = scope.get(&name.name) {
            self.diags.push(
                Diagnostic::error(
                    ErrorCode::DuplicateDefinition,
                    format!("`{}` is already bound in this scope", name.name),
                )
                .with_secondary(first, "first bound here")
                .with_primary(name.span, "bound again here"),
            );
            return;
        }
        scope.insert(name.name.clone(), name.span);
    }

    fn lookup_local(&self, name: &str) -> Option<Span> {
        self.scopes.iter().rev().find_map(|s| s.get(name).copied())
    }

    fn lookup_type_var(&self, name: &str) -> Option<Span> {
        self.type_scopes
            .iter()
            .rev()
            .find_map(|s| s.get(name).copied())
    }

    // ---- diagnostics -------------------------------------------------------

    fn unknown_name(&mut self, name: &Ident, candidates: Vec<String>) {
        self.unknown_name_at(name, candidates, "name");
    }

    fn unknown_name_at(&mut self, name: &Ident, candidates: Vec<String>, what: &str) {
        let mut diag = Diagnostic::error(
            ErrorCode::UnresolvedName,
            format!("unknown {} `{}`", what, name.name),
        )
        .with_primary(name.span, "not found");
        if let Some(near) = nearest(&name.name, candidates) {
            diag = diag
                .with_note(format!("did you mean `{near}`?"))
                .with_suggestion(Suggestion::Replace {
                    span: name.span,
                    with: near,
                });
        }
        self.diags.push(diag);
    }

    /// Names visible for suggestions at a value use: package names plus
    /// every local in scope.
    fn shadowing_candidates(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .package
            .defs
            .iter()
            .map(|def| def.name.clone())
            .collect();
        for scope in &self.scopes {
            names.extend(scope.keys().cloned());
        }
        names
    }

    /// Names visible at a type use: package names plus type variables.
    fn type_candidates(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .package
            .defs
            .iter()
            .map(|def| def.name.clone())
            .collect();
        for scope in &self.type_scopes {
            names.extend(scope.keys().cloned());
        }
        names
    }

    /// Callable-ish names for dot-call suggestions: value-namespace
    /// declarations plus registered method names.
    fn callable_candidates(&self) -> Vec<String> {
        let mut names: Vec<String> = self.package.value_names.keys().cloned().collect();
        names.extend(self.package.method_index.keys().cloned());
        names
    }
}

/// The identifier of a single-segment path expression, if it is one.
fn single_path_ident(expr: &Expr) -> Option<&Ident> {
    let Expr::Path { path, .. } = expr else {
        return None;
    };
    match path.segments.as_slice() {
        [only] => Some(only),
        _ => None,
    }
}

impl MethodDecl {
    /// The human-readable owner name for diagnostics: the trait's name or
    /// `inherent`.
    fn owner_name(&self, package: &Package) -> String {
        match self.owner {
            MethodOwner::Trait(id) => package.def(id).name.clone(),
            MethodOwner::Inherent => "an inherent impl".to_owned(),
        }
    }
}

/// The nearest candidate by edit distance; ties break to the
/// lexicographically smallest name so the suggestion is deterministic.
fn nearest(name: &str, mut candidates: Vec<String>) -> Option<String> {
    candidates.sort();
    let threshold = (name.len() / 3).max(1);
    candidates
        .into_iter()
        .map(|candidate| (edit_distance(name, &candidate), candidate))
        .min_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)))
        .and_then(|(distance, candidate)| (distance <= threshold).then_some(candidate))
}

/// Levenshtein edit distance over bytes; small strings only (identifiers).
fn edit_distance(a: &str, b: &str) -> usize {
    let b_bytes = b.as_bytes();
    let mut previous: Vec<usize> = (0..=b_bytes.len()).collect();
    let mut current = vec![0; b_bytes.len() + 1];
    for (i, a_byte) in a.as_bytes().iter().enumerate() {
        current[0] = i + 1;
        for (j, b_byte) in b_bytes.iter().enumerate() {
            let cost = usize::from(a_byte != b_byte);
            current[j + 1] = (previous[j + 1] + 1)
                .min(current[j] + 1)
                .min(previous[j] + cost);
        }
        core::mem::swap(&mut previous, &mut current);
    }
    previous[b_bytes.len()]
}
