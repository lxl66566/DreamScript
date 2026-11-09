//! Package-level name resolution over the parsed surface [G3, F3].
//!
//! A [`Package`] collects the declarations of its files into ONE namespace
//! [G3]: files in one package share it, there are zero intra-package
//! imports, and top-level definitions are order-independent — collection
//! inserts every declaration before any use is resolved, so intra-file and
//! cross-file definition cycles are legal by construction (resolution is
//! name lookup, not dependency ordering; the "whole-package fixpoint" of
//! the record is exactly this two-pass shape).
//!
//! # Method tier [F3]
//!
//! UFCS resolution is fixed: a dot-call `x.f(args)` feeds the receiver to
//! the first parameter of a FREE function named `f` if one exists, else the
//! registered method candidates (trait members, plus inherent impl fns);
//! `Trait::f(x)` qualifies directly. Trait methods are REGISTERED here
//! only — selection among candidates is the checker's type-directed work,
//! and F3's true-ambiguity error therefore lives there too, never as a
//! priority search here. There is no name-level ambiguity in this pass: the
//! package namespace is unique and locals shadow. A dot-call resolving
//! nowhere reports both tiers in F3's order. The fed parameter index is
//! recorded (always 0 today — capability parameters have no syntax yet, so
//! the first parameter IS the first data parameter; the checker enforces
//! the exclusion once they do).
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
//! `self` as the self type [section 6.1]. Or-pattern alternatives rebind
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
    /// A trait [B6].
    Trait,
    /// A type alias [F7].
    TypeAlias,
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

/// A package: the declarations of its files in one namespace [G3], plus
/// the method-tier registries [F3]. Collection (`add_file`) is
/// order-independent; use resolution (`resolve`) runs against the complete
/// tables, which is what makes intra-package definition cycles legal.
#[derive(Clone, PartialEq, Eq, Default, Debug)]
pub struct Package {
    files: Vec<ds_diag::FileId>,
    defs: Vec<Def>,
    names: HashMap<String, DefId>,
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

    /// The definition of a package-level name, if any.
    #[must_use]
    pub fn lookup(&self, name: &str) -> Option<DefId> {
        self.names.get(name).copied()
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
                        self.register_method(&f.name, MethodOwner::Trait(id));
                    }
                }
            },
            ItemKind::Impl(imp) => {
                // Trait impls do not re-register: the trait's signatures
                // already provide the candidate names.
                if imp.trait_ref.is_none() {
                    for f in &imp.fns {
                        self.register_method(&f.name, MethodOwner::Inherent);
                    }
                }
            },
        }
    }

    /// Declares one name; a duplicate reports both spans and keeps the
    /// first definition.
    fn declare(&mut self, name: &Ident, kind: DefKind, diags: &mut Vec<Diagnostic>) -> DefId {
        if let Some(&first) = self.names.get(&name.name) {
            let first_span = self.defs[first.index() as usize].span;
            diags.push(
                Diagnostic::error(
                    ErrorCode::DuplicateDefinition,
                    format!("`{}` is already defined", name.name),
                )
                .with_secondary(first_span, "first defined here")
                .with_primary(name.span, "redefined here"),
            );
            return first;
        }
        let id = DefId::from_raw(u32::try_from(self.defs.len()).expect("table index fits u32"));
        self.defs.push(Def {
            name: name.name.clone(),
            kind,
            span: name.span,
        });
        self.names.insert(name.name.clone(), id);
        id
    }

    fn register_method(&mut self, name: &Ident, owner: MethodOwner) {
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

    /// Top-level binding patterns declare one def per bound name.
    fn collect_pattern_defs(&mut self, pat: &Pattern, kind: DefKind, diags: &mut Vec<Diagnostic>) {
        match pat {
            Pattern::Binding { name, .. } => {
                self.declare(name, kind, diags);
            },
            Pattern::Tuple { pats, .. } | Pattern::Or { pats, .. } => {
                for sub in pats {
                    self.collect_pattern_defs(sub, kind, diags);
                }
            },
            Pattern::Struct { fields, .. } => {
                for (_, field) in fields {
                    if let Some(sub) = field {
                        self.collect_pattern_defs(sub, kind, diags);
                    }
                }
            },
            Pattern::Wildcard(_)
            | Pattern::Literal { .. }
            | Pattern::Str { .. }
            | Pattern::RawStr { .. }
            | Pattern::Path { .. }
            | Pattern::Call { .. } => {},
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
            ItemKind::Fn(f) | ItemKind::ExternFn(f) => self.fn_decl(f),
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
                // A trait member's signatures spell the self type `self`
                // [section 6.1].
                self.type_scopes
                    .push(HashMap::from([("self".to_owned(), t.name.span)]));
                for member in &t.members {
                    match member {
                        TraitMember::AssocType { .. } => {},
                        TraitMember::Fn(f) => self.fn_decl(f),
                    }
                }
                self.type_scopes.pop();
                self.type_scopes.pop();
            },
            ItemKind::Impl(imp) => {
                self.type_params(&imp.generics);
                let trait_impl = imp.trait_ref.is_some();
                if let Some(trait_ref) = &imp.trait_ref {
                    for arg in &trait_ref.args {
                        self.type_(arg);
                    }
                    self.path_use(&trait_ref.path);
                }
                // A trait impl's signatures spell the self type `self` too
                // [section 6.1], mirroring the trait's own members.
                if trait_impl {
                    self.type_scopes
                        .push(HashMap::from([("self".to_owned(), imp.span)]));
                }
                self.type_(&imp.target);
                for f in &imp.fns {
                    self.fn_decl(f);
                }
                if trait_impl {
                    self.type_scopes.pop();
                }
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

    fn fn_decl(&mut self, f: &FnDecl) {
        self.type_params(&f.generics);
        self.scopes.push(HashMap::new());
        for param in &f.params {
            self.param(param);
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

    fn param(&mut self, param: &Param) {
        if let Some(ty) = &param.ty {
            self.type_(ty);
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
                    self.param(param);
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
                self.path_use(path);
                for (name, value) in fields {
                    match value {
                        // Punning: the field name is also a value use.
                        None => self.name_use(name),
                        Some(expr) => self.expr(expr),
                    }
                }
            },
            Expr::Tuple { items, .. } => {
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
                // UFCS: a dot-callee gets tiered resolution [F3].
                if let Expr::Field { recv, name, .. } = callee.as_ref() {
                    self.expr(recv);
                    self.dot_use(name);
                } else {
                    self.expr(callee);
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
            Expr::Or { left, right, .. } => {
                self.expr(left);
                self.or_tail(right);
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
                    self.bind_pattern(&arm.pat, false);
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

    fn or_tail(&mut self, tail: &ds_parse::OrTail) {
        match tail {
            ds_parse::OrTail::Expr(expr) => self.expr(expr),
            ds_parse::OrTail::Return { expr, .. } => {
                if let Some(expr) = expr {
                    self.expr(expr);
                }
            },
            ds_parse::OrTail::Break(_) | ds_parse::OrTail::Continue(_) => {},
        }
    }

    /// A plain identifier use: locals shadow package names. Bare `none`
    /// is the universe's distinguished unit atom [B4], not a package name;
    /// the checker types it directly, so it records nothing here.
    fn name_use(&mut self, name: &Ident) {
        let res = if let Some(bind) = self.lookup_local(&name.name) {
            Res::Local { bind }
        } else if let Some(def) = self.package.lookup(&name.name) {
            Res::Def(def)
        } else if name.name == "none" {
            return;
        } else {
            self.unknown_name(name, self.shadowing_candidates());
            return;
        };
        self.uses.insert(name.span, res);
    }

    /// A path expression: one segment is a name use; `Trait::f` qualifies
    /// directly [F3]; longer paths are module paths [G4, M7].
    fn path_use(&mut self, path: &Path) {
        match path.segments.as_slice() {
            [only] => self.name_use(only),
            [head, method] => {
                let Some(def) = self.package.lookup(&head.name) else {
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

    /// A UFCS dot-call's method position [F3]: free functions first, then
    /// registered methods; the `.err`/`.ok` handler forms are surface
    /// syntax, not names.
    fn dot_use(&mut self, name: &Ident) {
        if name.name == "err" || name.name == "ok" {
            self.uses.insert(name.span, Res::Handler);
            return;
        }
        if let Some(func) = self.package.lookup(&name.name) {
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
        } else if let Some(def) = self.package.lookup(&name.name) {
            Res::Def(def)
        } else if name.name == "none" {
            // `none` is the universe's distinguished unit atom [B4], not a
            // package declaration; the checker solves it directly.
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
        match pat {
            Pattern::Wildcard(_)
            | Pattern::Literal { .. }
            | Pattern::Str { .. }
            | Pattern::RawStr { .. } => {},
            Pattern::Binding { name, .. } => self.bind(name, or_rebind),
            // Pattern paths are item references, not locals.
            Pattern::Path { path, .. } => self.pattern_path(path),
            Pattern::Call { path, args, .. } => {
                self.pattern_path(path);
                for arg in args {
                    self.bind_pattern(arg, or_rebind);
                }
            },
            Pattern::Struct { path, fields, .. } => {
                self.pattern_path(path);
                for (name, field) in fields {
                    match field {
                        // Punning binds the field name.
                        None => self.bind(name, or_rebind),
                        Some(sub) => self.bind_pattern(sub, or_rebind),
                    }
                }
            },
            Pattern::Tuple { pats, .. } => {
                for sub in pats {
                    self.bind_pattern(sub, or_rebind);
                }
            },
            Pattern::Or { pats, .. } => {
                for sub in pats {
                    self.bind_pattern(sub, true);
                }
            },
        }
    }

    fn pattern_path(&mut self, path: &Path) {
        match path.segments.as_slice() {
            [only] => {
                if let Some(def) = self.package.lookup(&only.name) {
                    self.uses.insert(only.span, Res::Def(def));
                } else {
                    let candidates = self
                        .package
                        .defs
                        .iter()
                        .map(|def| def.name.clone())
                        .collect::<Vec<_>>();
                    self.unknown_name_at(only, candidates, "pattern");
                }
            },
            // The tail is a case name; case tables are the checker's.
            [head, _case] => {
                if let Some(def) = self.package.lookup(&head.name) {
                    self.uses.insert(head.span, Res::Def(def));
                } else {
                    let candidates = self
                        .package
                        .defs
                        .iter()
                        .map(|def| def.name.clone())
                        .collect::<Vec<_>>();
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

    /// Callable-ish names for dot-call suggestions: package names plus
    /// registered method names.
    fn callable_candidates(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .package
            .defs
            .iter()
            .map(|def| def.name.clone())
            .collect();
        names.extend(self.package.method_index.keys().cloned());
        names
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
