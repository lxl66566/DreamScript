//! The pooled-body static refusal: a `par_map`/`par_reduce` element body
//! runs on a worker that owns whole fibers but no siblings — the fiber
//! pool, the nurseries, and the platform tables stay host-side — so two
//! member classes diverge worker-side and refuse here, inside the callable
//! bodies the fork special-cases as combinator children:
//!
//! * the sibling-addressing fiber control natives `fiber_resume` and `nursery_join`: a worker holds
//!   no fiber slots (any resume target answers the checked-read bounce tag) and only its own
//!   nursery stub (a foreign id names it and refuses);
//! * the world surface: a worker's code snapshot strips the platform tables, so a world call traps
//!   unresolved.
//!
//! The refusal is unconditional — language legality never depends on a
//! runtime host flag (`Vm::enable_fork_pool`), so a pooled body refuses
//! under the sequential drive too: the pool is an execution detail of
//! one drive, not a second semantics. A pooled site is recognized by resolved identity, never
//! by the user-facing spelling: the @std library fns (`par_map`/
//! `par_reduce`) are bodied members whose own bodies enter the private
//! fork-join engine seams, so a call is pooled exactly when its callee
//! resolves to such a member (or to a seam itself — the private extern
//! the stdlib alone can name); a user's same-spelled declaration is
//! just a fn with no seam in reach and never pools. The hunt follows
//! the callable's reach: the callable's own
//! subtree (nested lambdas included — a hosted combinator's element
//! lambda runs inside the child's extent), named-fn and const-binding
//! bodies, fn-typed locals through their alias chains (visited-guarded,
//! so no depth truncates), value-position fn arguments (a named helper a
//! combinator receives runs inside the extent), trait and inherent
//! member bodies by member name (dispatch candidates over-approximated),
//! interpolation holes' `show` dispatches, and linked callees by their
//! carried row (their bodies live outside this package's trees). The
//! site scan covers every tree position a body can sit at, a struct's
//! `deinit` block included. A fn value this pass cannot pin — a
//! parameter or a computed callable — stays clean.

use ds_diag::{Diagnostic, ErrorCode, Span};
use ds_parse::{
    BlockExpr, Expr, Item, ItemKind, Stmt, StmtKind, StrPart, StructMember, TraitMember,
    Type as Written,
};
use rustc_hash::{FxHashMap, FxHashSet};

use crate::{
    decls::{Decls, ParamSig},
    effects::{DeclaredRows, Effect},
    import::ForeignKind,
    pattern::pattern_binders,
    resolve::{DefId, DotRes, Package, Res, Resolutions},
    type_of::{Analyzed, HoleShowDispatches},
};

/// The sibling-addressing fiber control natives a pooled drive cannot
/// serve on a worker. Both are bodiless-extern spellings — the lowerer
/// routes a callee to the native by name only when no local body plans,
/// so a bodied fn of the same name never reaches the divergence.
const CONTROL_NATIVES: [&str; 2] = ["fiber_resume", "nursery_join"];

/// The fork-join engine seams' spellings: the private bodiless generic
/// externs the @std pooled library fns route through, addressed by the
/// same by-name native routing the lowerer applies to bodiless generic
/// callees (the key the fiber control natives above hold — an engine
/// identity, never a user-facing combinator spelling).
const POOL_SEAMS: [&str; 2] = ["fork_map", "fork_reduce"];

/// One pooled call site with its callable argument's expression: a
/// call whose callee resolves to a member whose body enters a fork-join
/// seam.
struct Site<'a> {
    /// The resolved member's own name, for the seam the diagnostic names.
    combinator: String,
    /// The combinator call itself, the seam's span.
    call: &'a Expr,
    /// The fn-typed slot's argument.
    callable: &'a Expr,
}

/// One refusal found inside a pooled body: the offending member's name
/// and the call that reaches it.
enum Offense {
    /// A sibling-addressing fiber control native.
    Control {
        /// The native's declared name.
        name: String,
        /// The call that reaches it.
        span: Span,
    },
    /// A world-surface call.
    World {
        /// The callee's declared name.
        name: String,
        /// The call that reaches it.
        span: Span,
    },
}

impl Offense {
    /// The refusal naming the member and the pooled-body seam.
    fn diagnostic(&self, combinator: &str, pooled: Span) -> Diagnostic {
        match self {
            Self::Control { name, span } => Diagnostic::error(
                ErrorCode::PooledControlNative,
                format!(
                    "`{name}` addresses a fiber or nursery the fork-join drive holds host-side; a \
                     pooled `{combinator}` child owns no sibling targets [E3]"
                ),
            )
            .with_primary(*span, format!("`{name}` runs inside the pooled body"))
            .with_secondary(pooled, "the pooled body enters here"),
            Self::World { name, span } => Diagnostic::error(
                ErrorCode::PooledWorldCall,
                format!(
                    "`{name}` touches the world surface; a pooled `{combinator}` child runs \
                     without it [E3]"
                ),
            )
            .with_primary(*span, format!("`{name}` runs inside the pooled body"))
            .with_secondary(pooled, "the pooled body enters here"),
        }
    }
}

/// Refuses the worker-diverging members in every pooled body of one
/// analyzed package: each `par_map`/`par_reduce` call's callable
/// argument is hunted through its reachable bodies, and the first
/// offense per site reports. Clean callables (the corpus and every
/// clean program) answer nothing.
///
/// `declared` carries the ambient effect rows, the same table the
/// effects pass reads: a callee whose declared row carries `world`
/// refuses, applied by name exactly as the row pass applies it.
#[must_use]
pub fn check_pooled_bodies(analyzed: &Analyzed, declared: &DeclaredRows) -> Vec<Diagnostic> {
    let index = Index::new(analyzed);
    let sites = index.sites();
    let mut diags = Vec::new();
    for site in sites {
        if let Some(offense) = index.hunt(site.callable, declared) {
            diags.push(offense.diagnostic(&site.combinator, site.call.span()));
        }
    }
    diags
}

/// The pass's read-only substrate over one analyzed package: the
/// per-file resolution tables, the bodies keyed by definition and
/// member name, the fn-typed locals' initializers, the call candidates,
/// and the seam-entering definitions the site gate reads.
struct Index<'a> {
    package: &'a Package,
    decls: &'a Decls,
    resolutions: &'a [Resolutions],
    holes: &'a HoleShowDispatches,
    /// Tree position by file id, so any span resolves under its own
    /// file's table.
    files: FxHashMap<u32, usize>,
    /// Every definition's body: `None` for a bodiless extern or
    /// signature, `Some` for a binding's initializer (a named fn is the
    /// binding of its lambda).
    by_def: FxHashMap<DefId, Option<Body<'a>>>,
    /// Member bodies by member name: trait defaults and impl fns, the
    /// dispatch candidates' over-approximation.
    methods: FxHashMap<String, Vec<Body<'a>>>,
    /// A local binding's initializer, keyed by the binder span a use
    /// resolves to.
    locals: FxHashMap<Span, Body<'a>>,
    /// Call expressions, pending the resolution-backed gate.
    candidates: Vec<&'a Expr>,
    /// Definitions whose own bodies enter a pooled seam — the @std
    /// library fns' identity, discovered structurally at index time.
    pooled_defs: FxHashSet<DefId>,
}

/// One walkable body: an expression or a block.
#[derive(Clone, Copy)]
enum Body<'a> {
    /// An expression subtree.
    Expr(&'a Expr),
    /// A block's statements.
    Block(&'a BlockExpr),
}

impl Body<'_> {
    /// The body's own span, the hunt's visited key.
    fn span(&self) -> Span {
        match self {
            Self::Expr(e) => e.span(),
            Self::Block(b) => b.span,
        }
    }
}

/// One reachable callee a walked body names.
enum Hop {
    /// A package definition, with the use that named it.
    Def(DefId, Span),
    /// A local binding.
    Local(Span),
    /// A member name (dispatch candidates over-approximate by name).
    Member(String),
}

impl<'a> Index<'a> {
    /// Scans the package's trees once: definition bodies, member
    /// bodies, local initializers, and the combinator call candidates.
    fn new(analyzed: &'a Analyzed) -> Self {
        let mut this = Self {
            package: &analyzed.package,
            decls: &analyzed.decls,
            resolutions: &analyzed.resolutions,
            holes: &analyzed.checked.hole_show_dispatches,
            files: FxHashMap::default(),
            by_def: FxHashMap::default(),
            methods: FxHashMap::default(),
            locals: FxHashMap::default(),
            candidates: Vec::new(),
            pooled_defs: FxHashSet::default(),
        };
        for (position, ast) in analyzed.asts.iter().enumerate() {
            this.files.insert(ast.file.as_u32(), position);
        }
        for item in analyzed
            .asts
            .iter()
            .flat_map(|ast| ast.blocks.iter())
            .flat_map(|block| block.items.iter())
        {
            this.register_item(item);
            walk_item(item, &mut |visit| match visit {
                Visit::Expr(expr) => this.scan_expr(expr),
                Visit::Stmt(stmt) => this.scan_stmt(stmt),
            });
        }
        this.pooled_defs = this
            .by_def
            .iter()
            .filter_map(|(def, body)| body.map(|body| (*def, body)))
            .filter(|(_, body)| this.body_enters_seam(*body))
            .map(|(def, _)| def)
            .collect();
        this
    }

    /// Whether one walked body calls a pooled seam directly: any call
    /// position in the subtree (nested lambdas included) whose callee
    /// resolves to a seam extern.
    fn body_enters_seam(&self, body: Body<'a>) -> bool {
        let mut found = false;
        let scan = |expr: &'a Expr, found: &mut bool| {
            if let Expr::Call { callee, .. } = expr
                && let Some(def) = self.seam_callee(callee)
                && self.is_seam(def)
            {
                *found = true;
            }
        };
        match body {
            Body::Expr(expr) => walk_expr(expr, &mut |visit| {
                if let Visit::Expr(expr) = visit {
                    scan(expr, &mut found);
                }
            }),
            Body::Block(block) => walk_block(block, &mut |visit| {
                if let Visit::Expr(expr) = visit {
                    scan(expr, &mut found);
                }
            }),
        }
        found
    }

    /// The definition one call callee resolves to when it names a
    /// package definition: a single-name path, or the free fn a dot's
    /// receiver feeds.
    fn seam_callee(&self, callee: &Expr) -> Option<DefId> {
        match peel(callee) {
            Expr::Path { path, .. } if path.segments.len() == 1 => {
                match self.res_of(path.segments[0].span) {
                    Some(Res::Def(def)) => Some(*def),
                    _ => None,
                }
            },
            Expr::Field { name, .. } => match self.res_of(name.span) {
                Some(Res::Dot(DotRes::Free { func, .. })) => Some(*func),
                _ => None,
            },
            _ => None,
        }
    }

    /// Whether a definition is a pooled seam itself: the bodiless
    /// generic extern spelled for one of the engine's fork-join
    /// natives.
    fn is_seam(&self, def: DefId) -> bool {
        matches!(self.by_def.get(&def), Some(None))
            && self
                .decls
                .fns
                .get(&def)
                .is_some_and(|sig| !sig.generics.is_empty())
            && POOL_SEAMS.contains(&self.package.def(def).name.as_str())
    }

    /// One item's declaration-level records: definition bodies and
    /// member bodies.
    fn register_item(&mut self, item: &'a Item) {
        match &item.kind {
            ItemKind::Local(local) => {
                let body = Body::Expr(&local.init);
                for (span, name) in pattern_binders(&local.pat) {
                    if let Some(def) = self.package.lookup_value(name) {
                        self.by_def.insert(def, Some(body));
                    }
                    self.locals.insert(span, body);
                }
            },
            ItemKind::Fn(f) | ItemKind::ExternFn(f) => {
                if let Some(def) = self.package.lookup_value(&f.name.name) {
                    self.by_def.insert(def, None);
                }
            },
            ItemKind::Trait(t) => {
                for member in &t.members {
                    let TraitMember::Fn(f) = member else {
                        continue;
                    };
                    if let Some(body) = &f.body {
                        self.methods
                            .entry(f.name.name.clone())
                            .or_default()
                            .push(Body::Block(body));
                    }
                }
            },
            ItemKind::Impl(imp) => {
                for f in &imp.fns {
                    if let Some(body) = &f.body {
                        self.methods
                            .entry(f.name.name.clone())
                            .or_default()
                            .push(Body::Block(body));
                    }
                }
            },
            ItemKind::Import(_)
            | ItemKind::Struct(_)
            | ItemKind::Enum(_)
            | ItemKind::Tag(_)
            | ItemKind::TypeAlias(_)
            | ItemKind::Platform(_) => {},
        }
    }

    /// The expression scan: every call expression is a candidate, the
    /// resolution-backed gate below deciding its identity.
    fn scan_expr(&mut self, expr: &'a Expr) {
        if let Expr::Call { .. } = expr {
            self.candidates.push(expr);
        }
    }

    /// The statement scan: local initializers, keyed by binder span.
    fn scan_stmt(&mut self, stmt: &'a Stmt) {
        let StmtKind::Local(local) = &stmt.kind else {
            return;
        };
        let body = Body::Expr(&local.init);
        for (span, _) in pattern_binders(&local.pat) {
            self.locals.insert(span, body);
        }
    }

    /// The resolution recorded at a use span, under its own file's
    /// table.
    fn res_of(&self, span: Span) -> Option<&'a Res> {
        let position = self.files.get(&span.file.as_u32()).copied()?;
        self.resolutions.get(position)?.get(span)
    }

    /// The pooled call sites: candidates gated by resolved identity —
    /// the callee is a pooled seam itself (the private extern only the
    /// stdlib's own file names) or a bodied member whose body enters
    /// one, the @std library fns' shape; every other callee (a user's
    /// same-spelled fn among them) is just a call. The fn-typed slot's
    /// argument is the callable.
    fn sites(&self) -> Vec<Site<'a>> {
        let mut sites = Vec::new();
        for &call in &self.candidates {
            let Expr::Call {
                callee,
                args,
                trailing,
                ..
            } = call
            else {
                continue;
            };
            let (def, fed) = match callee.as_ref() {
                Expr::Path { path, .. } if path.segments.len() == 1 => {
                    let Some(Res::Def(def)) = self.res_of(path.segments[0].span) else {
                        continue;
                    };
                    (*def, None)
                },
                Expr::Field { name, .. } => {
                    let Some(Res::Dot(DotRes::Free { func, fed_param })) = self.res_of(name.span)
                    else {
                        continue;
                    };
                    // The dot's receiver occupies its fed slot.
                    (*func, Some(*fed_param))
                },
                _ => continue,
            };
            let Some(sig) = self.decls.fns.get(&def) else {
                continue;
            };
            let combinator = self.package.def(def).name.clone();
            if !self.is_seam(def) && !self.pooled_defs.contains(&def) {
                continue;
            }
            let Some(callable) = callable_arg(&sig.params, fed, args, trailing.as_deref()) else {
                continue;
            };
            sites.push(Site {
                combinator,
                call,
                callable,
            });
        }
        sites
    }

    /// Hunts one pooled callable's reach for the first offense: bodies
    /// queue through a visited set (cycles answer clean), and each
    /// walked body's callees resolve to definitions, locals, or member
    /// names.
    fn hunt(&self, callable: &'a Expr, declared: &DeclaredRows) -> Option<Offense> {
        let mut visited: FxHashSet<Span> = FxHashSet::default();
        let mut queue: Vec<Body<'a>> = Vec::new();
        if let Some(offense) =
            self.chase_value(callable, declared, &mut queue, &mut FxHashSet::default())
        {
            return Some(offense);
        }
        while let Some(body) = queue.pop() {
            if !visited.insert(body.span()) {
                continue;
            }
            let mut hops: Vec<Hop> = Vec::new();
            self.collect(body, &mut hops);
            for hop in hops {
                let offense = match hop {
                    Hop::Def(def, at) => self.def_hop(def, at, declared, &mut queue),
                    Hop::Local(bind) => {
                        let Some(Body::Expr(init)) = self.locals.get(&bind) else {
                            continue;
                        };
                        self.chase_value(init, declared, &mut queue, &mut FxHashSet::default())
                    },
                    Hop::Member(name) => {
                        if let Some(bodies) = self.methods.get(&name) {
                            queue.extend(bodies.iter().copied());
                        }
                        continue;
                    },
                };
                if offense.is_some() {
                    return offense;
                }
            }
        }
        None
    }

    /// Follows one fn-typed value to the body it may run: a lambda
    /// queues its own subtree, a named definition takes the def hop, a
    /// local chases its initializer binding step after binding step —
    /// the chase's visited set guards the local chains (a cycle answers
    /// clean), so no alias depth truncates the reach. Definitions are
    /// visited-guarded at the queue, so only locals need the set.
    fn chase_value(
        &self,
        expr: &'a Expr,
        declared: &DeclaredRows,
        queue: &mut Vec<Body<'a>>,
        seen: &mut FxHashSet<Span>,
    ) -> Option<Offense> {
        let mut value = expr;
        loop {
            match peel(value) {
                Expr::Lambda { .. } => {
                    queue.push(Body::Expr(peel(value)));
                    return None;
                },
                Expr::Path { path, .. } if path.segments.len() == 1 => {
                    let only = &path.segments[0];
                    match self.res_of(only.span) {
                        Some(Res::Def(def)) => {
                            return self.def_hop(*def, only.span, declared, queue);
                        },
                        Some(Res::Local { bind }) => {
                            if !seen.insert(*bind) {
                                return None;
                            }
                            let Some(Body::Expr(init)) = self.locals.get(bind) else {
                                return None;
                            };
                            value = init;
                        },
                        _ => return None,
                    }
                },
                _ => return None,
            }
        }
    }

    /// One definition hop: a bodiless control native or a world-row
    /// callee offends at the use that named it; a bodied definition
    /// queues its body. A linked callee's body lives in the linked
    /// module, out of this hunt's reach, so its carried link facts judge
    /// it alone: the entry's solved row refuses a world callee (the same
    /// fact the comptime fence reads), and a fn entry named for a
    /// control native refuses by entry name — the lowerer routes a
    /// bodiless linked callee to the native exactly as it routes a local
    /// extern spelling.
    fn def_hop(
        &self,
        def: DefId,
        at: Span,
        declared: &DeclaredRows,
        queue: &mut Vec<Body<'a>>,
    ) -> Option<Offense> {
        let name = self.package.def(def).name.clone();
        if let Some(entry) = self.package.foreign().get(&def) {
            if entry.kind == ForeignKind::Fn && CONTROL_NATIVES.contains(&name.as_str()) {
                return Some(Offense::Control { name, span: at });
            }
            if entry.row.contains(Effect::World) {
                return Some(Offense::World { name, span: at });
            }
            return None;
        }
        if matches!(self.by_def.get(&def), Some(None)) && CONTROL_NATIVES.contains(&name.as_str()) {
            return Some(Offense::Control { name, span: at });
        }
        if declared.row(&name).contains(Effect::World) {
            return Some(Offense::World { name, span: at });
        }
        if let Some(Some(body)) = self.by_def.get(&def) {
            queue.push(*body);
        }
        None
    }

    /// Collects one body's reachable callees and show-hole dispatches:
    /// every node of the subtree, nested lambda bodies included (they
    /// run inside the child's extent).
    fn collect(&self, body: Body<'a>, hops: &mut Vec<Hop>) {
        match body {
            Body::Expr(expr) => walk_expr(expr, &mut |visit| match visit {
                Visit::Expr(expr) => self.collect_expr(expr, hops),
                Visit::Stmt(_) => {},
            }),
            Body::Block(block) => walk_block(block, &mut |visit| match visit {
                Visit::Expr(expr) => self.collect_expr(expr, hops),
                Visit::Stmt(_) => {},
            }),
        }
    }

    /// One walked node: a call resolves its callee, a string resolves
    /// its holes' show dispatches, and a value-position name a fn value
    /// may carry (a combinator argument like `filter(helper)` runs the
    /// named body inside this extent) — resolving it over-approximates
    /// toward the safe direction.
    fn collect_expr(&self, expr: &'a Expr, hops: &mut Vec<Hop>) {
        match expr {
            Expr::Call { callee, .. } => self.call_hops(callee, hops),
            Expr::Str { parts, .. } => {
                for part in parts {
                    let span = match part {
                        StrPart::Expr { span, .. } | StrPart::Ident { span, .. } => *span,
                        StrPart::Text { .. } => continue,
                    };
                    if let Some(&method) = self.holes.get(&span) {
                        hops.push(Hop::Member(self.package.method(method).name.clone()));
                    }
                }
            },
            Expr::Path { path, .. } if path.segments.len() == 1 => {
                let only = &path.segments[0];
                match self.res_of(only.span) {
                    Some(Res::Def(def)) => hops.push(Hop::Def(*def, only.span)),
                    Some(Res::Local { bind }) => hops.push(Hop::Local(*bind)),
                    _ => {},
                }
            },
            _ => {},
        }
    }

    /// One call's callee hops: a single-name path resolves its
    /// definition, local, or trait member; a dot resolves the free
    /// function or every registered method candidate.
    fn call_hops(&self, callee: &'a Expr, hops: &mut Vec<Hop>) {
        match peel(callee) {
            Expr::Path { path, .. } => match path.segments.as_slice() {
                [only] => match self.res_of(only.span) {
                    Some(Res::Def(def)) => hops.push(Hop::Def(*def, only.span)),
                    Some(Res::Local { bind }) => hops.push(Hop::Local(*bind)),
                    Some(Res::TraitMethod(method)) => {
                        hops.push(Hop::Member(self.package.method(*method).name.clone()));
                    },
                    _ => {},
                },
                [_head, member] => {
                    if let Some(Res::TraitMethod(method)) = self.res_of(member.span) {
                        hops.push(Hop::Member(self.package.method(*method).name.clone()));
                    }
                },
                _ => {},
            },
            Expr::Field { name, .. } => match self.res_of(name.span) {
                Some(Res::Dot(DotRes::Free { func, .. })) => hops.push(Hop::Def(*func, name.span)),
                Some(Res::Dot(DotRes::Methods { candidates } | DotRes::Static { candidates })) => {
                    for &method in candidates {
                        hops.push(Hop::Member(self.package.method(method).name.clone()));
                    }
                },
                _ => {},
            },
            _ => {},
        }
    }
}

/// Peels parenthesization.
fn peel(expr: &Expr) -> &Expr {
    match expr {
        Expr::Paren { inner, .. } => peel(inner),
        _ => expr,
    }
}

/// The fn-typed slot's argument: positional arguments in order, named
/// arguments by parameter name, the trailing lambda at the next open
/// slot, the fed receiver occupying its own — the checker's own slot
/// mapping, read-only.
fn callable_arg<'e>(
    params: &[ParamSig],
    fed: Option<usize>,
    args: &'e [ds_parse::CallArg],
    trailing: Option<&'e Expr>,
) -> Option<&'e Expr> {
    let mut slots: Vec<Option<&'e Expr>> = vec![None; params.len()];
    let mut next = 0;
    if let Some(index) = fed {
        if index >= slots.len() {
            return None;
        }
        next = index + 1;
    }
    for arg in args {
        match &arg.name {
            Some(name) => {
                let index = params.iter().position(|p| p.name == name.name)?;
                slots[index] = Some(&arg.value);
            },
            None => {
                if next < slots.len() {
                    slots[next] = Some(&arg.value);
                    next += 1;
                }
            },
        }
    }
    if let Some(tail) = trailing
        && next < slots.len()
    {
        slots[next] = Some(tail);
    }
    let callable = params
        .iter()
        .position(|p| matches!(p.ty, Some(Written::Fn { .. })))?;
    slots.into_iter().nth(callable).flatten()
}

/// One tree-visit event: an expression or a statement.
enum Visit<'t> {
    /// An expression node.
    Expr(&'t Expr),
    /// A statement node.
    Stmt(&'t Stmt),
}

/// Walks one item's expressions and statements.
fn walk_item<'t>(item: &'t Item, on: &mut impl FnMut(Visit<'t>)) {
    match &item.kind {
        ItemKind::Local(local) => walk_expr(&local.init, on),
        ItemKind::Impl(imp) => {
            for f in &imp.fns {
                if let Some(body) = &f.body {
                    walk_block(body, on);
                }
            }
        },
        ItemKind::Trait(t) => {
            for member in &t.members {
                if let TraitMember::Fn(f) = member
                    && let Some(body) = &f.body
                {
                    walk_block(body, on);
                }
            }
        },
        ItemKind::Struct(s) => {
            // A deinit block runs at the value's death site, so its own
            // pooled call sites hunt like any body's.
            for member in &s.members {
                if let StructMember::Deinit { body, .. } = member {
                    walk_block(body, on);
                }
            }
        },
        ItemKind::Fn(_)
        | ItemKind::ExternFn(_)
        | ItemKind::Import(_)
        | ItemKind::Enum(_)
        | ItemKind::Tag(_)
        | ItemKind::TypeAlias(_)
        | ItemKind::Platform(_) => {},
    }
}

/// Walks a block's statements.
fn walk_block<'t>(block: &'t BlockExpr, on: &mut impl FnMut(Visit<'t>)) {
    for stmt in &block.stmts {
        on(Visit::Stmt(stmt));
        match &stmt.kind {
            StmtKind::Local(local) => walk_expr(&local.init, on),
            // A block-local `fn` body counts in the segment it sits in.
            StmtKind::Fn(f) => {
                if let Some(body) = &f.body {
                    walk_block(body, on);
                }
            },
            StmtKind::Expr(expr) => walk_expr(expr, on),
        }
    }
}

/// Walks one expression's subtree, visiting every node before its
/// children.
fn walk_expr<'t>(expr: &'t Expr, on: &mut impl FnMut(Visit<'t>)) {
    on(Visit::Expr(expr));
    match expr {
        Expr::Literal { .. }
        | Expr::RawStr { .. }
        | Expr::Path { .. }
        | Expr::FnType { .. }
        | Expr::Break(_)
        | Expr::Continue(_)
        // A conversion placeholder holds no nested values.
        | Expr::Invalid(_) => {},
        Expr::Str { parts, .. } => {
            for part in parts {
                if let StrPart::Expr { expr, .. } = part {
                    walk_expr(expr, on);
                }
            }
        },
        Expr::Lambda { body, .. } => walk_expr(body, on),
        Expr::Struct { fields, .. } => {
            for (_, value) in fields {
                if let Some(value) = value {
                    walk_expr(value, on);
                }
            }
        },
        Expr::Tuple { items, .. } | Expr::List { items, .. } => {
            for item in items {
                walk_expr(item, on);
            }
        },
        Expr::Paren { inner, .. }
        | Expr::Try { expr: inner, .. }
        | Expr::Unary { expr: inner, .. }
        | Expr::Is { expr: inner, .. } => walk_expr(inner, on),
        Expr::Call {
            callee,
            args,
            trailing,
            ..
        } => {
            walk_expr(callee, on);
            for arg in args {
                walk_expr(&arg.value, on);
            }
            if let Some(tail) = trailing {
                walk_expr(tail, on);
            }
        },
        Expr::Field { recv, .. } => walk_expr(recv, on),
        Expr::FieldKey { recv, name, .. } => {
            walk_expr(recv, on);
            walk_expr(name, on);
        },
        Expr::Index { recv, index, .. } => {
            walk_expr(recv, on);
            walk_expr(index, on);
        },
        Expr::MapApply { key, value, .. } => {
            walk_expr(key, on);
            walk_expr(value, on);
        },
        Expr::Binary { left, right, .. } | Expr::Assign { left, right, .. } => {
            walk_expr(left, on);
            walk_expr(right, on);
        },
        Expr::Range { start, end, .. } => {
            walk_expr(start, on);
            walk_expr(end, on);
        },
        Expr::If {
            cond, then, else_, ..
        } => {
            walk_expr(cond, on);
            walk_block(then, on);
            if let Some(else_) = else_ {
                walk_expr(else_, on);
            }
        },
        Expr::Match {
            scrutinee, arms, ..
        } => {
            walk_expr(scrutinee, on);
            for arm in arms {
                if let Some(guard) = &arm.guard {
                    walk_expr(guard, on);
                }
                walk_expr(&arm.body, on);
            }
        },
        Expr::While { cond, body, .. } => {
            walk_expr(cond, on);
            walk_block(body, on);
        },
        Expr::For { iter, body, .. } => {
            walk_expr(iter, on);
            walk_expr(body, on);
        },
        Expr::Return { expr, .. } => {
            if let Some(expr) = expr {
                walk_expr(expr, on);
            }
        },
        Expr::Nursery { body, .. } | Expr::Spawn { body, .. } | Expr::Defer { body, .. } => {
            walk_block(body, on);
        },
        Expr::Block(block) => walk_block(block, on),
    }
}
