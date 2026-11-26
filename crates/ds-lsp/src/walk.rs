//! The shared scope walk: the one traversal over the parse tree that
//! mirrors the resolver's own scope pushes — generic parameters bind
//! after their bounds, `Self` binds over trait and impl member regions,
//! match arms and `for` bodies bind their patterns, and constructor
//! brackets admit type spellings in expression position — driven by
//! one [`Visit`] implementation per walk. Every node's own surface (a
//! token class, a marked type position) answers through the hooks, so a
//! new item or expression shape threads through this module alone and
//! each walk adds only the hook it needs.

use ds_check::Package;
use ds_diag::Span;
use ds_parse::{
    Ast, BlockExpr, EnumCase, EnumDecl, EnumPayload, Expr, FieldDecl, FnDecl, Ident, ImplDecl,
    ImportDecl, Item, ItemKind, LocalDecl, Param, Pattern, PlatformDecl, Stmt, StmtKind, StrPart,
    StructDecl, StructMember, TagDecl, TraitDecl, TraitMember, Type, TypeAliasDecl, TypeParam,
};

/// The in-scope type variables and `Self` binding one node sees: the
/// resolver's type-scope state, handed to the type-position hooks.
pub(crate) struct TypeScopes<'s> {
    /// The bound type variables, outermost declaration first.
    pub(crate) vars: &'s [(String, Span)],
    /// Whether `Self` names the enclosing self type here.
    pub(crate) self_bound: bool,
}

impl TypeScopes<'_> {
    /// Whether `name` is a bound type variable, or `Self` when bound.
    pub(crate) fn binds(&self, name: &str) -> bool {
        (name == "Self" && self.self_bound) || self.vars.iter().any(|(var, _)| var == name)
    }
}

/// The per-node surface one walk paints over the shared walk. Every
/// hook defaults to nothing, so a walk implements exactly the hooks its
/// own answers read.
pub(crate) trait Visit {
    /// The package whose declared types decide a constructor
    /// application's brackets.
    fn package(&self) -> &Package;

    /// One item's own surface ahead of its shape; `false` skips the
    /// item entirely (a derive-synthesized tree never walks).
    fn enter_item(&mut self, _item: &Item) -> bool {
        true
    }
    /// One top-level binding's keyword, equals, and binders.
    fn at_local_item(&mut self, _local: &LocalDecl) {}
    /// One function head (a member names a method).
    fn at_fn(&mut self, _f: &FnDecl, _member: bool) {}
    /// One extern head.
    fn at_extern_fn(&mut self, _f: &FnDecl) {}
    /// One struct head.
    fn at_struct(&mut self, _s: &StructDecl) {}
    /// One field declaration (a struct body or an enum case payload).
    fn at_field(&mut self, _field: &FieldDecl) {}
    /// One deinit member's keyword.
    fn at_deinit(&mut self, _span: Span) {}
    /// One enum head.
    fn at_enum(&mut self, _e: &EnumDecl) {}
    /// One enum case's name.
    fn at_case(&mut self, _case: &EnumCase) {}
    /// One type alias head.
    fn at_alias(&mut self, _a: &TypeAliasDecl) {}
    /// One trait head.
    fn at_trait(&mut self, _t: &TraitDecl) {}
    /// One associated type's name.
    fn at_assoc_type(&mut self, _name: &Ident) {}
    /// One impl head (the trait reference's `for` included).
    fn at_impl(&mut self, _imp: &ImplDecl) {}
    /// One import head.
    fn at_import(&mut self, _decl: &ImportDecl) {}
    /// One tag head.
    fn at_tag(&mut self, _t: &TagDecl) {}
    /// One platform head.
    fn at_platform(&mut self, _p: &PlatformDecl) {}
    /// One generic parameter binding its name.
    fn at_type_param(&mut self, _param: &TypeParam) {}
    /// One value parameter (mode keyword, name, default equals).
    fn at_param(&mut self, _param: &Param) {}
    /// The `->` ahead of one return annotation, `from` the offset the
    /// arrow follows.
    fn at_ret_arrow(&mut self, _from: u32, _ret: &Type) {}
    /// One block's trailing trivia.
    fn at_block(&mut self, _block: &BlockExpr) {}
    /// One statement's own surface (trivia, a binding's keyword and
    /// binders).
    fn at_stmt(&mut self, _stmt: &Stmt) {}
    /// One expression node's own surface ahead of its children.
    fn at_expr(&mut self, _expr: &Expr) {}
    /// One pattern node's own surface ahead of its children.
    fn at_pattern(&mut self, _pat: &Pattern) {}
    /// One string literal, `holed` when its parts carry anything but
    /// text runs.
    fn at_str(&mut self, _span: Span, _parts: &[StrPart], _holed: bool) {}
    /// One text run of a holed string.
    fn at_str_text(&mut self, _span: Span, _holed: bool) {}
    /// One bare-identifier hole of a holed string.
    fn at_str_ident(&mut self, _name: &Ident, _holed: bool) {}
    /// One type node ahead of its nested types.
    fn at_type(&mut self, _ty: &Type, _scopes: &TypeScopes) {}
    /// One identifier use in type position (a constructor application's
    /// head, a map head, an impl's trait reference).
    fn at_type_name(&mut self, _ident: &Ident, _scopes: &TypeScopes) {}
    /// One bracketed constructor argument ahead of its shape.
    fn at_ctor_arg(&mut self, _arg: &Expr, _scopes: &TypeScopes) {}
}

/// Whether one expression is a bare path naming a package type — the
/// head a constructor application resolves through.
pub(crate) fn is_type_head(package: &Package, recv: &Expr) -> bool {
    matches!(recv, Expr::Path { path, .. } if path.segments.len() == 1
        && package.lookup_type(&path.segments[0].name).is_some())
}

/// The shared walk itself: owns the type-variable scopes and `Self`
/// depth, drives one file's tree, and calls the visitor's hooks.
pub(crate) struct Walk<'v, V: Visit> {
    visitor: &'v mut V,
    /// In-scope type variables, outermost declaration first.
    vars: Vec<(String, Span)>,
    /// Depth of active `Self` bindings.
    self_depth: usize,
}

impl<'v, V: Visit> Walk<'v, V> {
    pub(crate) fn new(visitor: &'v mut V) -> Self {
        Self {
            visitor,
            vars: Vec::new(),
            self_depth: 0,
        }
    }

    /// Drives the walk over one file's parse tree.
    pub(crate) fn root(&mut self, ast: &Ast) {
        for block in &ast.blocks {
            for item in &block.items {
                self.item(item);
            }
        }
    }

    fn pop(&mut self, mark: usize) {
        self.vars.truncate(mark);
    }

    /// Binds one declaration's generic parameters, answering the mark
    /// to restore. The bounds walk before the names bind, exactly as
    /// the resolver walks them.
    fn generics(&mut self, params: &[TypeParam]) -> usize {
        for param in params {
            if let Some(bound) = &param.bound {
                self.ty(bound);
            }
        }
        let mark = self.vars.len();
        for param in params {
            self.visitor.at_type_param(param);
            self.vars.push((param.name.name.clone(), param.name.span));
        }
        mark
    }

    fn item(&mut self, item: &Item) {
        if !self.visitor.enter_item(item) {
            return;
        }
        match &item.kind {
            ItemKind::Import(decl) => {
                self.visitor.at_import(decl);
                if let Some(cond) = &decl.cond {
                    self.expr(cond);
                }
            },
            ItemKind::Local(local) => {
                self.visitor.at_local_item(local);
                self.expr(&local.init);
                if let Some(ty) = &local.ty {
                    self.ty(ty);
                }
            },
            ItemKind::Fn(f) => {
                self.visitor.at_fn(f, false);
                self.fn_decl(f);
            },
            ItemKind::ExternFn(f) => {
                self.visitor.at_extern_fn(f);
                self.fn_decl(f);
            },
            ItemKind::Struct(s) => {
                self.visitor.at_struct(s);
                let mark = self.generics(&s.generics);
                for ty in &s.supers {
                    self.ty(ty);
                }
                for member in &s.members {
                    match member {
                        StructMember::Field(field) => {
                            self.visitor.at_field(field);
                            self.ty(&field.ty);
                        },
                        StructMember::Deinit { span, body } => {
                            self.visitor.at_deinit(*span);
                            self.block(body);
                        },
                    }
                }
                self.pop(mark);
            },
            ItemKind::Enum(e) => {
                self.visitor.at_enum(e);
                let mark = self.generics(&e.generics);
                for ty in &e.supers {
                    self.ty(ty);
                }
                for case in &e.cases {
                    self.visitor.at_case(case);
                    self.payload(&case.payload);
                }
                self.pop(mark);
            },
            ItemKind::TypeAlias(a) => {
                self.visitor.at_alias(a);
                let mark = self.generics(&a.generics);
                self.ty(&a.ty);
                self.pop(mark);
            },
            ItemKind::Trait(t) => {
                self.visitor.at_trait(t);
                let mark = self.generics(&t.generics);
                for ty in &t.supers {
                    self.ty(ty);
                }
                // `Self` binds over the trait's members.
                self.self_depth += 1;
                for member in &t.members {
                    match member {
                        TraitMember::AssocType { name, .. } => self.visitor.at_assoc_type(name),
                        TraitMember::Fn(f) => {
                            self.visitor.at_fn(f, true);
                            self.fn_decl(f);
                        },
                    }
                }
                self.self_depth -= 1;
                self.pop(mark);
            },
            ItemKind::Impl(imp) => {
                self.visitor.at_impl(imp);
                let mark = self.generics(&imp.generics);
                if let Some(trait_ref) = &imp.trait_ref {
                    for arg in &trait_ref.args {
                        self.ty(arg);
                    }
                    let scopes = TypeScopes {
                        vars: &self.vars,
                        self_bound: self.self_depth > 0,
                    };
                    for segment in &trait_ref.path.segments {
                        self.visitor.at_type_name(segment, &scopes);
                    }
                }
                // `Self` binds over the target and the members.
                self.self_depth += 1;
                self.ty(&imp.target);
                for f in &imp.fns {
                    self.visitor.at_fn(f, true);
                    self.fn_decl(f);
                }
                self.self_depth -= 1;
                self.pop(mark);
            },
            ItemKind::Tag(t) => self.visitor.at_tag(t),
            ItemKind::Platform(p) => self.visitor.at_platform(p),
        }
    }

    /// One enum case's payload.
    fn payload(&mut self, payload: &EnumPayload) {
        match payload {
            EnumPayload::None => {},
            EnumPayload::Tuple(types) => {
                for ty in types {
                    self.ty(ty);
                }
            },
            EnumPayload::Struct(fields) => {
                for field in fields {
                    self.visitor.at_field(field);
                    self.ty(&field.ty);
                }
            },
        }
    }

    /// One declared function's shape — generic parameters, parameters,
    /// return annotation, body — shared by top-level signatures, externs,
    /// and trait and impl members.
    fn fn_decl(&mut self, f: &FnDecl) {
        let mark = self.generics(&f.generics);
        for param in &f.params {
            self.param(param);
        }
        if let Some(ret) = &f.ret {
            let after = f.params.last().map_or(f.name.span.end, |p| p.span.end);
            self.visitor.at_ret_arrow(after, ret);
            self.ty(ret);
        }
        if let Some(body) = &f.body {
            self.block(body);
        }
        self.pop(mark);
    }

    fn param(&mut self, param: &Param) {
        self.visitor.at_param(param);
        if let Some(ty) = &param.ty {
            self.ty(ty);
        }
        if let Some(default) = &param.default {
            self.expr(default);
        }
    }

    fn block(&mut self, block: &BlockExpr) {
        self.visitor.at_block(block);
        for stmt in &block.stmts {
            self.stmt(stmt);
        }
    }

    fn stmt(&mut self, stmt: &Stmt) {
        self.visitor.at_stmt(stmt);
        match &stmt.kind {
            StmtKind::Local(local) => {
                self.expr(&local.init);
                if let Some(ty) = &local.ty {
                    self.ty(ty);
                }
            },
            StmtKind::Expr(expr) => self.expr(expr),
        }
    }

    /// One type expression: every node is a type position ahead of its
    /// nested types.
    fn ty(&mut self, ty: &Type) {
        let scopes = TypeScopes {
            vars: &self.vars,
            self_bound: self.self_depth > 0,
        };
        self.visitor.at_type(ty, &scopes);
        match ty {
            Type::Path { args, .. } => {
                for arg in args {
                    self.ty(arg);
                }
            },
            Type::Optional { inner, .. }
            | Type::Negation { inner, .. }
            | Type::Paren { inner, .. }
            | Type::Ptr { inner, .. } => self.ty(inner),
            Type::Union { left, right, .. }
            | Type::Intersect { left, right, .. }
            | Type::Difference { left, right, .. } => {
                self.ty(left);
                self.ty(right);
            },
            Type::Tuple { items, .. } => {
                for item in items {
                    self.ty(item);
                }
            },
            Type::Fn { params, ret, .. } => {
                for param in params {
                    if let Some(ty) = &param.ty {
                        self.ty(ty);
                    }
                }
                if let Some(ret) = ret {
                    self.ty(ret);
                }
            },
            Type::Unit(_) | Type::TypeKw(_) | Type::Foreign { .. } => {},
        }
    }

    /// One string literal's parts, the holes' expressions walked.
    fn str_parts(&mut self, span: Span, parts: &[StrPart]) {
        let holed = parts
            .iter()
            .any(|part| !matches!(part, StrPart::Text { .. }));
        self.visitor.at_str(span, parts, holed);
        for part in parts {
            match part {
                StrPart::Text { span, .. } => self.visitor.at_str_text(*span, holed),
                StrPart::Expr { expr, .. } => self.expr(expr),
                StrPart::Ident { name, .. } => self.visitor.at_str_ident(name, holed),
            }
        }
    }

    /// One pattern: a binder-free walk over its sub-patterns (the
    /// resolver binds the pattern's own names).
    fn pattern(&mut self, pat: &Pattern) {
        self.visitor.at_pattern(pat);
        match pat {
            Pattern::Str { span, parts } => self.str_parts(*span, parts),
            Pattern::Call { args, .. } => {
                for arg in args {
                    self.pattern(arg);
                }
            },
            Pattern::Struct { fields, .. } => {
                for (_, pat) in fields {
                    if let Some(pat) = pat {
                        self.pattern(pat);
                    }
                }
            },
            Pattern::Tuple { pats, .. } | Pattern::Or { pats, .. } => {
                for pat in pats {
                    self.pattern(pat);
                }
            },
            _ => {},
        }
    }

    /// One bracketed constructor argument: a type spelled in expression
    /// position when its shape names one, an ordinary expression
    /// otherwise.
    fn ctor_arg(&mut self, arg: &Expr) {
        let scopes = TypeScopes {
            vars: &self.vars,
            self_bound: self.self_depth > 0,
        };
        self.visitor.at_ctor_arg(arg, &scopes);
        if let Expr::Path { path, .. } = arg
            && path.segments.len() == 1
        {
            self.visitor.at_type_name(&path.segments[0], &scopes);
            return;
        }
        match arg {
            Expr::Paren { inner, .. } => self.ctor_arg(inner),
            Expr::Tuple { items, .. } => {
                for item in items {
                    self.ctor_arg(item);
                }
            },
            Expr::FnType { ty, .. } => self.ty(ty),
            _ => self.expr(arg),
        }
    }

    fn expr(&mut self, expr: &Expr) {
        self.visitor.at_expr(expr);
        match expr {
            Expr::Literal { .. }
            | Expr::RawStr { .. }
            | Expr::Path { .. }
            | Expr::Break(_)
            | Expr::Continue(_)
            // a conversion placeholder holds nothing to visit
            | Expr::Invalid(_) => {},
            Expr::Str { span, parts, .. } => self.str_parts(*span, parts),
            Expr::Lambda {
                span,
                generics,
                params,
                ret,
                body,
                ..
            } => {
                let mark = self.generics(generics);
                for param in params {
                    self.param(param);
                }
                if let Some(ret) = ret {
                    let after = params
                        .last()
                        .map_or(generics.last().map_or(span.start, |g| g.span.end), |p| {
                            p.span.end
                        });
                    self.visitor.at_ret_arrow(after, ret);
                    self.ty(ret);
                }
                self.expr(body);
                self.pop(mark);
            },
            Expr::FnType { ty, .. } => self.ty(ty),
            Expr::Is { expr, ty, .. } => {
                self.expr(expr);
                self.ty(ty);
            },
            // A constructor application's argument brackets are a type
            // position when the head names a package type; the map
            // constructor's brackets always are.
            Expr::Index { recv, index, .. } => {
                if is_type_head(self.visitor.package(), recv) {
                    if let Expr::Path { path, .. } = recv.as_ref() {
                        let scopes = TypeScopes {
                            vars: &self.vars,
                            self_bound: self.self_depth > 0,
                        };
                        self.visitor.at_type_name(&path.segments[0], &scopes);
                    }
                    self.ctor_arg(index);
                } else {
                    self.expr(recv);
                    self.expr(index);
                }
            },
            Expr::MapApply {
                head, key, value, ..
            } => {
                let scopes = TypeScopes {
                    vars: &self.vars,
                    self_bound: self.self_depth > 0,
                };
                self.visitor.at_type_name(head, &scopes);
                self.ctor_arg(key);
                self.ctor_arg(value);
            },
            Expr::Struct { fields, .. } => {
                for (_, value) in fields {
                    if let Some(expr) = value {
                        self.expr(expr);
                    }
                }
            },
            Expr::List { items, .. } | Expr::Tuple { items, .. } => {
                for item in items {
                    self.expr(item);
                }
            },
            Expr::Paren { inner, .. }
            | Expr::Try { expr: inner, .. }
            | Expr::Unary { expr: inner, .. } => self.expr(inner),
            Expr::Call {
                callee,
                args,
                trailing,
                ..
            } => {
                self.expr(callee);
                for arg in args {
                    self.expr(&arg.value);
                }
                if let Some(tail) = trailing {
                    self.expr(tail);
                }
            },
            Expr::Field { recv, .. } => self.expr(recv),
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
                    self.pattern(&arm.pat);
                    if let Some(ann) = &arm.ann {
                        self.ty(ann);
                    }
                    if let Some(guard) = &arm.guard {
                        self.expr(guard);
                    }
                    self.expr(&arm.body);
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
                self.pattern(pat);
                self.expr(body);
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
}
