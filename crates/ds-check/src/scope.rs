//! Scope iteration for tooling: the declaration scopes visible at one
//! source position.
//!
//! The resolver walks files with transient scope stacks and records only
//! name *uses*; nothing in its tables answers "which names could I use
//! here". [`scope_frames`] walks the same tree shape and keeps the scope
//! structure: one [`ScopeFrame`] per value scope the resolver pushes
//! (a function or lambda's parameter scope, a block's statement scope, a
//! match arm's binder scope, a for-pattern's binder scope, a struct
//! deinit's `self` scope), each binding carrying the byte offset from
//! which it turns visible — an initializer resolves before its binders
//! bind (`const x = x` uses the outer `x`), and a binding earlier in a
//! block is invisible at a later binding's initializer. The walk reads
//! the package's declared types to bind arm patterns exactly as the
//! resolver binds them: a bare arm identifier naming a constructor type
//! or the predeclared `none` is a member test that binds nothing.
//! Derive-synthesized items mint their spans inside the attributed
//! declaration, so the walk skips them: a cursor there names the
//! declaration, never the minted body.
//!
//! [`visible_at`] assembles the answer for one offset: the ambient's
//! package-level declarations first, the package's other declarations
//! next, then the enclosing local scopes innermost-first. Shadowing
//! mirrors resolution: an inner local hides an outer local and any
//! package value declaration of the same name, while a type-namespace
//! declaration of the name stays visible — constructor positions still
//! resolve it.
//!
//! Type-variable scopes (generic parameters, `Self`) bind in the
//! resolver's type stacks and stay outside this slice; member completion
//! and imports are later work.

use ds_diag::{FileId, Span};
use ds_parse::{
    Ast, BlockExpr, Expr, FnDecl, Item, ItemKind, Pattern, Stmt, StmtKind, StructMember,
    TraitMember,
};
use rustc_hash::FxHashSet;

use crate::{
    pattern::{arm_pattern_binders, pattern_binders},
    resolve::{DefId, DefKind, Package},
};

/// What one visible name declares or binds.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum ScopeKind {
    /// A function or extern function definition.
    Fn,
    /// A `const` binding.
    Const,
    /// A `var` binding.
    Var,
    /// A struct.
    Struct,
    /// An enum.
    Enum,
    /// A unit tag.
    Tag,
    /// An enum case minted by an enum block.
    Case,
    /// A trait.
    Trait,
    /// A type alias.
    TypeAlias,
    /// A local binding: a block `const`/`var`, a match-arm binder, or a
    /// for-pattern binder.
    Let,
    /// A parameter of a function or lambda.
    Param,
}

impl From<DefKind> for ScopeKind {
    fn from(kind: DefKind) -> Self {
        match kind {
            DefKind::Fn => Self::Fn,
            DefKind::Const => Self::Const,
            DefKind::Var => Self::Var,
            DefKind::Struct => Self::Struct,
            DefKind::Enum => Self::Enum,
            DefKind::Tag => Self::Tag,
            DefKind::Case => Self::Case,
            DefKind::Trait => Self::Trait,
            DefKind::TypeAlias => Self::TypeAlias,
        }
    }
}

/// One binding a local scope holds.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ScopeBinding {
    /// The bound name.
    pub name: String,
    /// What binds it.
    pub kind: ScopeKind,
    /// Span of the defining name occurrence.
    pub span: Span,
    /// The byte offset from which the binding turns visible.
    pub visible_from: u32,
}

/// One value scope of a file: the region where it is active and the
/// bindings it holds. Frames nest by span, so the frames containing one
/// offset order innermost-first by width.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ScopeFrame {
    /// The region where the scope is active.
    pub span: Span,
    /// The bindings the scope holds, in declaration order.
    pub bindings: Vec<ScopeBinding>,
}

/// One name visible at a position: its spelling, kind, and defining span.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ScopeEntry {
    /// The visible name.
    pub name: String,
    /// What the name declares or binds.
    pub kind: ScopeKind,
    /// Span of the defining name occurrence; carries the file.
    pub span: Span,
}

/// Span of one pattern: every variant carries its own extent.
fn pattern_span(pat: &Pattern) -> Span {
    match pat {
        Pattern::Wildcard(span)
        | Pattern::Literal { span, .. }
        | Pattern::Str { span, .. }
        | Pattern::RawStr { span, .. }
        | Pattern::Path { span, .. }
        | Pattern::Call { span, .. }
        | Pattern::Struct { span, .. }
        | Pattern::Tuple { span, .. }
        | Pattern::Or { span, .. }
        | Pattern::Binding { span, .. } => *span,
    }
}

/// The binder entries of one pattern, all turning visible at
/// `visible_from`.
fn pattern_bindings(pat: &Pattern, visible_from: u32) -> Vec<ScopeBinding> {
    pattern_binders(pat)
        .into_iter()
        .map(|(span, name)| ScopeBinding {
            name: name.to_owned(),
            kind: ScopeKind::Let,
            span,
            visible_from,
        })
        .collect()
}

/// The binder entries of one match-arm pattern, all turning visible at
/// `visible_from`: a bare identifier naming a member test (`arm_tests`)
/// binds nothing; every other arm shape binds like a declaration
/// pattern.
fn arm_bindings(
    pat: &Pattern,
    arm_tests: &FxHashSet<&str>,
    visible_from: u32,
) -> Vec<ScopeBinding> {
    arm_pattern_binders(pat, arm_tests)
        .into_iter()
        .map(|(span, name)| ScopeBinding {
            name: name.to_owned(),
            kind: ScopeKind::Let,
            span,
            visible_from,
        })
        .collect()
}

/// The names a bare match-arm identifier tests instead of binding:
/// the package's constructor types, plus the predeclared `none` while no
/// package declaration claims it — the resolver's arm-tag rule, so the
/// walk binds what resolution binds.
fn arm_member_tests(package: &Package) -> FxHashSet<&str> {
    let mut tests: FxHashSet<&str> = package
        .defs()
        .filter(|def| def.kind.is_constructor())
        .map(|def| def.name.as_str())
        .collect();
    if !package.declares("none") {
        tests.insert("none");
    }
    tests
}

/// The parameter entries of one parameter list, each visible from the end
/// of its own parameter (an annotation resolves before the next parameter
/// binds, matching the resolver's sequential binding).
fn param_bindings(params: &[ds_parse::Param]) -> Vec<ScopeBinding> {
    params
        .iter()
        .map(|param| ScopeBinding {
            name: param.name.name.clone(),
            kind: ScopeKind::Param,
            span: param.name.span,
            visible_from: param.span.end,
        })
        .collect()
}

/// Collects a file's scope frames by walking its tree, mirroring the
/// resolver's scoping decisions scope for scope. `package` supplies the
/// declared types behind the arm member-test rule.
#[must_use]
pub fn scope_frames(ast: &Ast, package: &Package) -> Vec<ScopeFrame> {
    let mut walker = Walker {
        frames: Vec::new(),
        stack: Vec::new(),
        arm_tests: arm_member_tests(package),
    };
    for block in &ast.blocks {
        for item in &block.items {
            walker.item(item);
        }
    }
    walker.frames
}

/// The scope walk: frames accumulate as scopes are pushed and popped;
/// the stack holds the indices of the frames currently open, so a binding
/// lands in the frame the resolver would bind it into.
struct Walker<'p> {
    frames: Vec<ScopeFrame>,
    stack: Vec<usize>,
    arm_tests: FxHashSet<&'p str>,
}

impl Walker<'_> {
    fn push(&mut self, span: Span, bindings: Vec<ScopeBinding>) {
        let index = self.frames.len();
        self.frames.push(ScopeFrame { span, bindings });
        self.stack.push(index);
    }

    fn pop(&mut self) {
        self.stack.pop();
    }

    fn bind(&mut self, binding: ScopeBinding) {
        let index = *self.stack.last().expect("the stack holds an open frame");
        self.frames[index].bindings.push(binding);
    }

    fn item(&mut self, item: &Item) {
        // A derive-synthesized impl mints its span inside the attributed
        // declaration: every position inside it names the user's source,
        // so its minted binders never answer.
        if item.synthesized {
            return;
        }
        match &item.kind {
            // A top-level binding declares a package def: its name
            // reaches uses through the package table, so only the
            // initializer's own scopes walk here.
            ItemKind::Local(local) => self.expr(&local.init),
            ItemKind::Fn(f) | ItemKind::ExternFn(f) => self.fn_decl(f),
            ItemKind::Struct(s) => {
                for member in &s.members {
                    if let StructMember::Deinit { span, body } = member {
                        self.push(body.span, vec![ScopeBinding {
                            name: "self".to_owned(),
                            kind: ScopeKind::Param,
                            span: *span,
                            visible_from: span.start,
                        }]);
                        self.block(body);
                        self.pop();
                    }
                }
            },
            ItemKind::Trait(t) => {
                for member in &t.members {
                    if let TraitMember::Fn(f) = member {
                        self.fn_decl(f);
                    }
                }
            },
            ItemKind::Impl(imp) => {
                for f in &imp.fns {
                    self.fn_decl(f);
                }
            },
            ItemKind::Import(_)
            | ItemKind::Tag(_)
            | ItemKind::Enum(_)
            | ItemKind::TypeAlias(_)
            | ItemKind::Platform(_) => {},
        }
    }

    fn fn_decl(&mut self, f: &FnDecl) {
        self.push(f.span, param_bindings(&f.params));
        if let Some(body) = &f.body {
            self.block(body);
        }
        self.pop();
    }

    fn block(&mut self, block: &BlockExpr) {
        self.push(block.span, Vec::new());
        for stmt in &block.stmts {
            self.stmt(stmt);
        }
        self.pop();
    }

    fn stmt(&mut self, stmt: &Stmt) {
        match &stmt.kind {
            // The initializer resolves before the binders bind, and the
            // binding turns visible only after the whole declaration.
            StmtKind::Local(local) => {
                self.expr(&local.init);
                for (span, name) in pattern_binders(&local.pat) {
                    self.bind(ScopeBinding {
                        name: name.to_owned(),
                        kind: ScopeKind::Let,
                        span,
                        visible_from: local.span.end,
                    });
                }
            },
            // A block-local `fn`: the body's frames hold the
            // declaration's own name under the parameters, and the name
            // then binds in the enclosing block, visible from the
            // declaration's end like any binding.
            StmtKind::Fn(f) => {
                self.push(f.span, vec![ScopeBinding {
                    name: f.name.name.clone(),
                    kind: ScopeKind::Let,
                    span: f.name.span,
                    visible_from: f.name.span.end,
                }]);
                self.push(f.span, param_bindings(&f.params));
                if let Some(body) = &f.body {
                    self.block(body);
                }
                self.pop();
                self.pop();
                self.bind(ScopeBinding {
                    name: f.name.name.clone(),
                    kind: ScopeKind::Let,
                    span: f.name.span,
                    visible_from: f.span.end,
                });
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
            | Expr::Continue(_)
            // A conversion placeholder names nothing.
            | Expr::Invalid(_)
            // Types bind no value names.
            | Expr::FnType { .. } => {},
            Expr::Str { parts, .. } => {
                for part in parts {
                    if let ds_parse::StrPart::Expr { expr, .. } = part {
                        self.expr(expr);
                    }
                }
            },
            Expr::Lambda {
                span, params, body, ..
            } => {
                self.push(*span, param_bindings(params));
                self.expr(body);
                self.pop();
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
            Expr::FieldKey { recv, name, .. } => {
                self.expr(recv);
                self.expr(name);
            },
            Expr::Index { recv, index, .. } => {
                self.expr(recv);
                self.expr(index);
            },
            Expr::MapApply { key, value, .. } => {
                self.expr(key);
                self.expr(value);
            },
            Expr::Is { expr, .. } => self.expr(expr),
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
                cond,
                then,
                else_,
                ..
            } => {
                self.expr(cond);
                self.block(then);
                if let Some(else_expr) = else_ {
                    self.expr(else_expr);
                }
            },
            Expr::Match { scrutinee, arms, .. } => {
                self.expr(scrutinee);
                for arm in arms {
                    // The binders turn visible at the end of the arm's
                    // pattern, covering the guard and the body.
                    self.push(
                        arm.span,
                        arm_bindings(&arm.pat, &self.arm_tests, pattern_span(&arm.pat).end),
                    );
                    if let Some(guard) = &arm.guard {
                        self.expr(guard);
                    }
                    self.expr(&arm.body);
                    self.pop();
                }
            },
            Expr::While { cond, body, .. } => {
                self.expr(cond);
                self.block(body);
            },
            Expr::For {
                span,
                pat,
                iter,
                body,
                ..
            } => {
                // The iterable resolves before the pattern binds.
                self.expr(iter);
                self.push(*span, pattern_bindings(pat, iter.span().end));
                self.expr(body);
                self.pop();
            },
            Expr::Return { expr, .. } => {
                if let Some(expr) = expr {
                    self.expr(expr);
                }
            },
            Expr::Nursery { body, .. }
            | Expr::Spawn { body, .. }
            | Expr::Defer { body, .. } => self.block(body),
            Expr::Block(block) => self.block(block),
        }
    }
}

/// The names visible at one byte offset of one file: the ambient's
/// declarations first, the package's other declarations next, then the
/// enclosing local scopes innermost-first. `frames` are the file's
/// [`scope_frames`] value and `package` the resolver's collected tables,
/// so the answer and name resolution read the same namespace (a
/// local shadows a package value declaration of its name, while a
/// type-namespace declaration of the name stays visible). Collection
/// keeps one def per name and namespace, so the package entries need no
/// dedup pass.
#[must_use]
pub fn visible_at(
    frames: &[ScopeFrame],
    package: &Package,
    ambient: FileId,
    at: u32,
) -> Vec<ScopeEntry> {
    // The frames containing the offset, innermost first: frames nest by
    // span, so the tightest containing frame is the narrowest. The
    // containment is half-open — an identifier starting at the frame's
    // end byte lies outside it, matching the resolver's scoping, where a
    // use resolves under a frame only as a node inside it.
    let mut containing: Vec<&ScopeFrame> = frames
        .iter()
        .filter(|frame| frame.span.start <= at && at < frame.span.end)
        .collect();
    containing.sort_by_key(|frame| frame.span.end - frame.span.start);
    let mut locals: Vec<ScopeEntry> = Vec::new();
    let mut local_names: FxHashSet<String> = FxHashSet::default();
    for frame in &containing {
        for binding in &frame.bindings {
            if binding.visible_from <= at && local_names.insert(binding.name.clone()) {
                locals.push(ScopeEntry {
                    name: binding.name.clone(),
                    kind: binding.kind,
                    span: binding.span,
                });
            }
        }
    }
    let mut out: Vec<ScopeEntry> = Vec::new();
    for is_ambient in [true, false] {
        for raw in 0..package.len() {
            let raw = u32::try_from(raw).expect("def count fits u32");
            let def = package.def(DefId::from_raw(raw));
            if (def.span.file == ambient) != is_ambient {
                continue;
            }
            // A local hides a package value declaration of the same name;
            // a type-namespace or case declaration stays visible.
            if !def.kind.in_type_namespace()
                && !matches!(def.kind, DefKind::Case)
                && local_names.contains(&def.name)
            {
                continue;
            }
            out.push(ScopeEntry {
                name: def.name.clone(),
                kind: ScopeKind::from(def.kind),
                span: def.span,
            });
        }
    }
    out.extend(locals);
    out
}
