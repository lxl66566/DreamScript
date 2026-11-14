//! Completion's position slices beyond flat scope names: the member
//! surface after a dot, type-variable scopes in type positions, and
//! prefix ranking over every answer.
//!
//! The member slice rides the checked expression-type table the hover
//! answers read: the receiver is the tightest expression whose span ends
//! exactly at the dot, and its members are the declaration tables' own
//! data (struct fields, enum cases, inherent impl members keyed by the
//! owner name, trait members through solved instances, UFCS free fns
//! whose first parameter names the receiver's type). Where no checked
//! type answers — no unpoisoned entry, or the receiver narrowed to the
//! empty type — the ordinary scope completion serves the position and
//! nothing member-specific is guessed.
//!
//! Type positions are a walk over the file's parse tree mirroring the
//! resolver's type-scope pushes: every [`ds_parse::Type`] node is a type
//! position, the generic-argument brackets of a constructor application
//! in expression position (`list[t]`) included, and the answer lists the
//! enclosing declarations' type variables (innermost first, `Self` inside
//! impl and trait member regions) ahead of the type-namespace names the
//! scope-index reports visible.

use ds_check::{Analyzed, DefKind, MethodOwner, Package, ScopeEntry, ScopeKind, Val};
use ds_diag::{FileId, Span};
use ds_parse::{
    Ast, BlockExpr, EnumPayload, Expr, FnDecl, Item, ItemKind, Stmt, StmtKind, StructMember,
    TraitMember, Type, TypeParam,
};
use rustc_hash::FxHashSet;
use tower_lsp::lsp_types as lsp;

use crate::convert;

/// Whether one byte is an identifier character of the scan-back (ASCII
/// letters, digits, underscore, and every non-ASCII byte, so a multibyte
/// identifier never splits a prefix).
fn ident_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_' || byte >= 0x80
}

/// The byte offset where the identifier being typed at `at` starts: the
/// scan stops at the first non-identifier byte behind the position.
fn ident_start(source: &str, at: u32) -> u32 {
    let bytes = source.as_bytes();
    let mut start = (at as usize).min(bytes.len());
    while start > 0 && ident_byte(bytes[start - 1]) {
        start -= 1;
    }
    u32::try_from(start).expect("a source offset fits u32")
}

/// The typed prefix at one offset: the identifier characters immediately
/// before it, empty when the position sits behind no identifier.
pub(crate) fn typed_prefix(source: &str, at: u32) -> String {
    source[ident_start(source, at) as usize..at as usize].to_owned()
}

/// The dot's offset when the position is a member position — right after
/// the dot or inside the member name being typed after it — else `None`.
pub(crate) fn member_dot(source: &str, at: u32) -> Option<u32> {
    let bytes = source.as_bytes();
    let dot = ident_start(source, at).checked_sub(1)?;
    (bytes[dot as usize] == b'.').then_some(dot)
}

/// The receiver's checked value at one dot: the tightest unpoisoned
/// expression-type entry whose span ends exactly at the dot. `None` when
/// no entry answers or the solved type is empty (the checker narrowed it
/// away, or the state is an error) — the honest fallback's trigger.
pub(crate) fn receiver_val(analyzed: &Analyzed, file: FileId, dot: u32) -> Option<Val> {
    let mut best: Option<(u32, Val)> = None;
    for (span, val) in &analyzed.checked.expr_types {
        if span.file != file || span.end != dot || val.poisoned {
            continue;
        }
        if best.is_none_or(|(start, _)| span.start > start) {
            best = Some((span.start, *val));
        }
    }
    let (_, val) = best?;
    let empty = analyzed.decls.universe.members(val.ty).is_empty();
    (!empty).then_some(val)
}

/// The completion items of one checked receiver type: struct fields and
/// enum cases of every member atom (applications through their
/// constructor's declaration), the inherent impl members owned by those
/// atoms' type names, the members of every trait the type provably
/// implements through solved instances, and the UFCS free functions whose
/// first parameter's written type names the receiver — the dot spelling
/// of a free function's name.
pub(crate) fn members(analyzed: &Analyzed, recv: Val) -> Vec<lsp::CompletionItem> {
    let decls = &analyzed.decls;
    let universe = &decls.universe;
    let package = &analyzed.package;
    let mut out: Vec<lsp::CompletionItem> = Vec::new();
    // Dedup on (label, kind) by scan: one receiver's member list is small
    // and `CompletionItemKind` derives no `Hash`.
    let mut push = |label: &str, kind: lsp::CompletionItemKind| {
        if !out
            .iter()
            .any(|item: &lsp::CompletionItem| item.label == label && item.kind == Some(kind))
        {
            out.push(lsp::CompletionItem {
                label: label.to_owned(),
                kind: Some(kind),
                ..lsp::CompletionItem::default()
            });
        }
    };
    // The declaring def of every member atom: nominal atoms map directly,
    // applications through their constructor. The def names key the
    // inherent and UFCS tiers; the shapes key the fields and cases.
    let mut owner_names: Vec<&str> = Vec::new();
    for &atom in universe.members(recv.ty) {
        let Ok(decl) = universe.atom_decl(atom) else {
            continue;
        };
        let def = match &decl.kind {
            ds_check::AtomKind::Apply(ctor, _) => decls.defs_of_atoms.get(ctor).copied(),
            _ => decls.defs_of_atoms.get(&atom).copied(),
        };
        let Some(def) = def else {
            continue;
        };
        let owner = &package.def(def);
        match owner.kind {
            DefKind::Struct => {
                if let Some(shape) = decls.structs.get(&def) {
                    for field in &shape.fields {
                        push(&field.name.name, lsp::CompletionItemKind::FIELD);
                    }
                }
            },
            DefKind::Enum => {
                if let Some(shape) = decls.enums.get(&def) {
                    for (name, ..) in &shape.cases {
                        push(name, lsp::CompletionItemKind::ENUM_MEMBER);
                    }
                }
            },
            // Tags carry no shape; traits and aliases are not receivers.
            _ => {},
        }
        owner_names.push(owner.name.as_str());
    }
    // Inherent impl members: owned by the target type's name (F3).
    for method in package.methods() {
        if let MethodOwner::Inherent(owner) = &method.owner
            && owner_names.iter().any(|name| name == owner)
        {
            push(&method.name, lsp::CompletionItemKind::METHOD);
        }
    }
    // Trait members through solved instances: every trait whose instance
    // set contains the receiver's type (B6).
    for (raw, def) in package.defs().iter().enumerate() {
        if def.kind != DefKind::Trait {
            continue;
        }
        let Ok(id) = u32::try_from(raw) else {
            break;
        };
        let id = ds_check::DefId::from_raw(id);
        let Some(trait_id) = decls.traits.get(&id) else {
            continue;
        };
        if decls
            .instances
            .resolve(universe, *trait_id, recv.ty)
            .is_empty()
        {
            continue;
        }
        if let Some(names) = decls.trait_members.get(&id) {
            for name in names {
                push(name, lsp::CompletionItemKind::METHOD);
            }
        }
    }
    // UFCS candidates: free fns whose first parameter's written type is a
    // path naming the receiver's type — the head alone, no argument
    // unification (a generic `list[t]` parameter matches a `list[int]`
    // receiver through its constructor's name).
    for (id, sig) in &decls.fns {
        let Some(first) = sig.params.first() else {
            continue;
        };
        let Some(Type::Path { path, .. }) = &first.ty else {
            continue;
        };
        let [only] = path.segments.as_slice() else {
            continue;
        };
        if owner_names.iter().any(|name| *name == only.name) {
            push(&package.def(*id).name, lsp::CompletionItemKind::FUNCTION);
        }
    }
    out
}

/// The type-variable answer at one offset: `Some` when the position sits
/// inside a type node (or a constructor application's argument brackets
/// in expression position), carrying the in-scope type variables — every
/// enclosing declaration's own, innermost first — and whether `Self` is
/// bound (impl and trait member regions).
pub(crate) struct TypeVars {
    /// In-scope type variables with their defining spans, innermost
    /// declaration first.
    pub names: Vec<(String, Span)>,
    /// Whether `Self` names the enclosing self type here.
    pub self_in_scope: bool,
}

/// Walks the file's parse tree mirroring the resolver's type-scope pushes
/// and answers the type variables in scope at one offset, `None` when the
/// offset sits in no type position.
pub(crate) fn type_vars(ast: &Ast, package: &Package, file: FileId, at: u32) -> Option<TypeVars> {
    let mut walker = Walker {
        package,
        file,
        at,
        found: None,
        vars: Vec::new(),
        self_depth: 0,
    };
    for block in &ast.blocks {
        for item in &block.items {
            walker.item(item);
        }
    }
    walker.found
}

/// The type-position walk. Scopes push and pop exactly where the
/// resolver's do, so the variables in scope at a covered type node are
/// the ones its resolution would see.
struct Walker<'a> {
    package: &'a Package,
    file: FileId,
    at: u32,
    found: Option<TypeVars>,
    /// In-scope type variables, outermost declaration first (the snapshot
    /// reverses into innermost-first).
    vars: Vec<(String, Span)>,
    /// Depth of active `Self` bindings.
    self_depth: usize,
}

impl Walker<'_> {
    /// Whether the walked position sits inside `span` (inclusive at both
    /// ends: a cursor right after a typed prefix still names it).
    fn covers(&self, span: Span) -> bool {
        span.file == self.file && span.start <= self.at && self.at <= span.end
    }

    /// Marks one span as a type position when it covers the offset.
    fn mark(&mut self, span: Span) {
        if self.found.is_none() && self.covers(span) {
            let names = self.vars.iter().rev().cloned().collect();
            self.found = Some(TypeVars {
                names,
                self_in_scope: self.self_depth > 0,
            });
        }
    }

    /// Visits one type node: a covered span marks, then the node's nested
    /// types walk (their scope stack is the same, so marking at the
    /// outermost covering node is exact).
    fn ty(&mut self, ty: &Type) {
        self.mark(ty.span());
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

    /// Binds one declaration's generic parameters, answering how many
    /// entries to pop. The bounds resolve before the names bind, exactly
    /// as the resolver walks them.
    fn generics(&mut self, params: &[TypeParam]) -> usize {
        for param in params {
            if let Some(bound) = &param.bound {
                self.ty(bound);
            }
        }
        for param in params {
            self.vars.push((param.name.name.clone(), param.name.span));
        }
        params.len()
    }

    fn pop(&mut self, count: usize) {
        self.vars.truncate(self.vars.len() - count);
    }

    fn item(&mut self, item: &Item) {
        match &item.kind {
            ItemKind::Local(local) => {
                self.expr(&local.init);
                if let Some(ty) = &local.ty {
                    self.ty(ty);
                }
            },
            ItemKind::Fn(f) | ItemKind::ExternFn(f) => self.fn_decl(f),
            ItemKind::Struct(s) => {
                let n = self.generics(&s.generics);
                for ty in &s.supers {
                    self.ty(ty);
                }
                for member in &s.members {
                    match member {
                        StructMember::Field(field) => self.ty(&field.ty),
                        StructMember::Deinit { body, .. } => self.block(body),
                    }
                }
                self.pop(n);
            },
            ItemKind::Enum(e) => {
                let n = self.generics(&e.generics);
                for ty in &e.supers {
                    self.ty(ty);
                }
                for case in &e.cases {
                    match &case.payload {
                        EnumPayload::None => {},
                        EnumPayload::Tuple(types) => {
                            for ty in types {
                                self.ty(ty);
                            }
                        },
                        EnumPayload::Struct(fields) => {
                            for field in fields {
                                self.ty(&field.ty);
                            }
                        },
                    }
                }
                self.pop(n);
            },
            ItemKind::TypeAlias(a) => {
                let n = self.generics(&a.generics);
                self.ty(&a.ty);
                self.pop(n);
            },
            ItemKind::Trait(t) => {
                let n = self.generics(&t.generics);
                for ty in &t.supers {
                    self.ty(ty);
                }
                // `Self` binds over the trait's members (F2).
                self.self_depth += 1;
                for member in &t.members {
                    if let TraitMember::Fn(f) = member {
                        self.fn_decl(f);
                    }
                }
                self.self_depth -= 1;
                self.pop(n);
            },
            ItemKind::Impl(imp) => {
                let n = self.generics(&imp.generics);
                if let Some(trait_ref) = &imp.trait_ref {
                    for arg in &trait_ref.args {
                        self.ty(arg);
                    }
                }
                // `Self` binds over the target and the members (F2).
                self.self_depth += 1;
                self.ty(&imp.target);
                for f in &imp.fns {
                    self.fn_decl(f);
                }
                self.self_depth -= 1;
                self.pop(n);
            },
            ItemKind::Import(_) | ItemKind::Tag(_) | ItemKind::Platform(_) => {},
        }
    }

    fn fn_decl(&mut self, f: &FnDecl) {
        let n = self.generics(&f.generics);
        for param in &f.params {
            if let Some(ty) = &param.ty {
                self.ty(ty);
            }
            if let Some(default) = &param.default {
                self.expr(default);
            }
        }
        if let Some(ret) = &f.ret {
            self.ty(ret);
        }
        if let Some(body) = &f.body {
            self.block(body);
        }
        self.pop(n);
    }

    fn block(&mut self, block: &BlockExpr) {
        for stmt in &block.stmts {
            self.stmt(stmt);
        }
    }

    fn stmt(&mut self, stmt: &Stmt) {
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

    fn expr(&mut self, expr: &Expr) {
        match expr {
            Expr::Literal { .. }
            | Expr::RawStr { .. }
            | Expr::Path { .. }
            | Expr::Break(_)
            | Expr::Continue(_) => {},
            Expr::Str { parts, .. } => {
                for part in parts {
                    if let ds_parse::StrPart::Expr { expr, .. } = part {
                        self.expr(expr);
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
                let n = self.generics(generics);
                for param in params {
                    if let Some(ty) = &param.ty {
                        self.ty(ty);
                    }
                    if let Some(default) = &param.default {
                        self.expr(default);
                    }
                }
                if let Some(ret) = ret {
                    self.ty(ret);
                }
                self.expr(body);
                self.pop(n);
            },
            // A fn type spelled in expression position is a type node.
            Expr::FnType { ty, .. } => self.ty(ty),
            Expr::Is { expr, ty, .. } => {
                self.expr(expr);
                self.ty(ty);
            },
            // A constructor application's argument brackets are a type
            // position when the head names a package type (the resolver's
            // ctor-apply rule, F9); the map constructor is pinned to the
            // `map` head by the grammar.
            Expr::Index { recv, index, .. } => {
                self.expr(recv);
                if self.is_type_head(recv) {
                    self.mark(index.span());
                }
            },
            Expr::MapApply { key, value, .. } => {
                self.mark(key.span());
                self.mark(value.span());
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
            Expr::For { iter, body, .. } => {
                self.expr(iter);
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

    /// Whether one expression is a bare path naming a package type — the
    /// head a constructor application resolves through (F9).
    fn is_type_head(&self, recv: &Expr) -> bool {
        matches!(recv, Expr::Path { path, .. } if path.segments.len() == 1
            && self.package.lookup_type(&path.segments[0].name).is_some())
    }
}

/// Whether one scope kind declares into the type namespace (F9): the
/// type-position filter over the scope-index's answer.
fn type_namespace(kind: ScopeKind) -> bool {
    matches!(
        kind,
        ScopeKind::Struct
            | ScopeKind::Enum
            | ScopeKind::Tag
            | ScopeKind::Case
            | ScopeKind::Trait
            | ScopeKind::TypeAlias
    )
}

/// The items of one type position: the in-scope type variables (`Self`
/// included when bound) ahead of the type-namespace names visible per
/// the scope-index's rules, a variable shadowing the package names of
/// its spelling.
pub(crate) fn type_items(vars: &TypeVars, entries: &[ScopeEntry]) -> Vec<lsp::CompletionItem> {
    let mut out = Vec::new();
    let mut shadowed: FxHashSet<&str> = vars.names.iter().map(|(name, _)| name.as_str()).collect();
    for (name, _) in &vars.names {
        out.push(lsp::CompletionItem {
            label: name.clone(),
            kind: Some(lsp::CompletionItemKind::TYPE_PARAMETER),
            ..lsp::CompletionItem::default()
        });
    }
    if vars.self_in_scope {
        shadowed.insert("Self");
        out.push(lsp::CompletionItem {
            label: "Self".to_owned(),
            kind: Some(lsp::CompletionItemKind::TYPE_PARAMETER),
            ..lsp::CompletionItem::default()
        });
    }
    for entry in entries {
        if !type_namespace(entry.kind) || shadowed.contains(entry.name.as_str()) {
            continue;
        }
        out.push(lsp::CompletionItem {
            label: entry.name.clone(),
            kind: Some(convert::completion_kind(entry.kind)),
            ..lsp::CompletionItem::default()
        });
    }
    out
}

/// The rank of one label against the typed prefix: an exact prefix
/// first, a word-boundary match next, a plain substring after, and the
/// rest last. Case-sensitive — identifiers are.
fn match_rank(label: &str, prefix: &str) -> u8 {
    if label.starts_with(prefix) {
        return 0;
    }
    let label_bytes = label.as_bytes();
    let mut at = 0;
    while let Some(found) = label[at..].find(prefix) {
        let boundary = at + found;
        let word_boundary = boundary > 0
            && (label_bytes[boundary - 1] == b'_'
                || (label_bytes[boundary - 1].is_ascii_lowercase()
                    && label_bytes[boundary].is_ascii_uppercase()));
        if word_boundary {
            return 1;
        }
        at = boundary + 1;
    }
    if label.contains(prefix) { 2 } else { 3 }
}

/// Ranks one completion answer by prefix match, stable within each group
/// by the existing order; an empty prefix keeps the answer unchanged.
pub(crate) fn rank(items: Vec<lsp::CompletionItem>, prefix: &str) -> Vec<lsp::CompletionItem> {
    if prefix.is_empty() {
        return items;
    }
    let mut ranked: Vec<(u8, lsp::CompletionItem)> = items
        .into_iter()
        .map(|item| (match_rank(&item.label, prefix), item))
        .collect();
    ranked.sort_by_key(|(rank, _)| *rank);
    ranked.into_iter().map(|(_, item)| item).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn member_positions_and_prefixes_scan_back() {
        let source = "box.field_name";
        // Right after the dot: an empty prefix and the dot behind it.
        assert_eq!(typed_prefix(source, 4), "");
        assert_eq!(member_dot(source, 4), Some(3));
        // Inside the member name: the typed prefix and the same dot.
        assert_eq!(typed_prefix(source, 7), "fie");
        assert_eq!(member_dot(source, 7), Some(3));
        // A position behind no dot names no member position.
        assert_eq!(member_dot(source, 2), None);
        assert_eq!(member_dot("a + b", 5), None);
    }

    #[test]
    fn ranking_groups_prefix_boundary_substring_rest() {
        let items = |labels: &[&str]| {
            labels
                .iter()
                .map(|label| lsp::CompletionItem {
                    label: (*label).to_owned(),
                    ..lsp::CompletionItem::default()
                })
                .collect::<Vec<_>>()
        };
        // Prefix first, then the word-boundary match, then the substring,
        // then the rest; each group keeps the incoming order.
        let ranked = rank(items(&["other", "extra", "my_trim", "trim"]), "tr");
        let labels: Vec<&str> = ranked.iter().map(|item| item.label.as_str()).collect();
        assert_eq!(labels, ["trim", "my_trim", "extra", "other"]);
        // A camel-case boundary is a word boundary, and matching stays
        // case-sensitive: `Tr` finds `myTrim` at its boundary while the
        // lowercase `trim` lands in the rest.
        let ranked = rank(items(&["other", "myTrim", "trim"]), "Tr");
        let labels: Vec<&str> = ranked.iter().map(|item| item.label.as_str()).collect();
        assert_eq!(labels, ["myTrim", "other", "trim"]);
        // An empty prefix keeps the answer's own order.
        let ranked = rank(items(&["b", "a"]), "");
        let labels: Vec<&str> = ranked.iter().map(|item| item.label.as_str()).collect();
        assert_eq!(labels, ["b", "a"]);
    }
}
