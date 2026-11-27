//! # Modes
//!
//! Borrow and `mut` parameters are windows the caller retains: borrowing
//! uses read freely, and a consuming use whose value stays in scope (a
//! local alias) keeps the borrow-copy discipline — only a use whose value
//! leaves the frame (a return, a `move` argument, a tail answer) is an
//! escape ([`ErrorCode::BorrowEscapes`]), ownership the window does not
//! hold transferring out, a double deinit at runtime.
//! Only `move` parameters consume their argument. Unresolvable callees
//! (an indirect call through a fn-typed value) default to borrowing; the
//! runtime `rc==1` uniqueness checks close that gap.
//!
//! # Walk order
//!
//! Top-level bindings are evaluated on demand and order-independent;
//! their initializers walk in a first pass over each file's items so a
//! linear definition consumed in one item and used in another is a move
//! wherever the items sit. Function and impl bodies walk after them.
//!
//! # Known limits
//!
//! * Per-binder copyability comes from the declared type; refinement inside an `is` branch of a
//!   mixed binding (`file | int` narrowed to `int`) still moves. Per-point precision is
//!   borrow-inference work.
//! * A field projection in value position moves the whole receiver binding (partial-move tracking
//!   is the Perceus placement pass).
//! * Field projection out of a borrow/mut window never moves.
//! * `defer` bodies walk at their source position.
//! * The loop audit checks one re-execution round (the condition and body of `while`, the body of
//!   `for`) against the merged state; a use the forward walk already reported is never reported
//!   twice. A loop nested inside an outer audit round skips its own round — the audit walk mutates
//!   nothing, so the round is a duplicate re-check that compounded per nesting level.
//! * The uninitialized state is infrastructure: every binding form carries an initializer and the
//!   resolver binds in flow order, so [`ErrorCode::UninitializedUse`] fires only once a surface
//!   form can defer initialization.
//! * Candidate method calls (`Res::Dot(DotRes::Methods)`) consume their receiver only when every
//!   candidate's first parameter is `move`; their non-receiver arguments walk as borrows until
//!   candidate selection is exposed to this pass.
//! * A lambda body walks once at its source position, so a call re-executing a body whose capture
//!   an earlier call already consumed goes unaudited. Closure call cardinality is a later surface
//!   decision (an Fn-vs-once contract); the runtime rc check owns that seam.

use super::*;

// ---- the walk -----------------------------------------------------------------

/// The forward walker. One instance covers one package; top-level
/// bindings' states persist across items so a value consumed in one item
/// and used in another is still a move.

pub(super) struct Lin<'a> {
    pub(super) package: &'a Package,
    pub(super) decls: &'a Decls,
    pub(super) facts: &'a HashMap<Span, BinderFact>,
    pub(super) def_types: &'a FxHashMap<DefId, Ty>,
    /// The comptime copies each loop span audits, keyed by the loop's
    /// own span: a copy walks once per field, straight-line.
    pub(super) copy_counts: HashMap<Span, usize>,
    pub(super) res: Option<&'a Resolutions>,
    /// The checked `Val` of every expression, keyed by its span: the
    /// early-exit flag read here marks a call to a `never` fn diverging.
    pub(super) expr_types: &'a FxHashMap<Span, Val>,
    /// The interpolation holes whose `show` the checker solved at their
    /// own span: the solved dispatch, read for the receiver's
    /// passing mode — the implicit dispatch borrows exactly where the
    /// written `.show()` would.
    pub(super) hole_dispatches: &'a FxHashMap<Span, TraitCall>,
    /// The same table for solved inherent dispatches.
    pub(super) hole_inherents: &'a FxHashMap<Span, MethodId>,
    /// Every dispatched hole's member: the runtime-evidence form
    /// records only here, so the receiver-mode read falls through to it.
    pub(super) hole_runtime: &'a FxHashMap<Span, MethodId>,
    pub(super) out: Linearity,
    pub(super) state: States,
    /// State writes made while walking the open branch, arm, or loop body,
    /// newest last; a rewind restores that region's entry state.
    pub(super) undo: Vec<StateUndo>,
    pub(super) info: FxHashMap<Place, Info<'a>>,
    /// Binders introduced per scope, in bind order (deinit order at exit).
    pub(super) scopes: Vec<Vec<Place>>,
    /// Audit mode: reads are checked against the current state but nothing
    /// mutates — the loop back-edge re-execution check.
    pub(super) audit: bool,
    /// Use sites already reported, so the loop audit never reports twice.
    pub(super) reported: HashSet<(Place, Span)>,
    /// The last use per binding: (use span, borrowed).
    pub(super) last_use: FxHashMap<Place, (Span, bool)>,
    /// The lambda walk's collected mutable captures: a `var` binding the
    /// body captures moves at the construction, so the post-walk records
    /// its death in the enclosing scope. `None` outside a lambda body
    /// (a spawned child's captures keep the spawn seam's acquire
    /// discipline), and the innermost lambda's list replaces the outer's
    /// for the walk's duration.
    pub(super) capture_sink: Option<Vec<Place>>,
    /// Capture depth: inside a lambda or spawn body, every use of a
    /// binding from an outer epoch is a capture and moves it.
    pub(super) epoch: usize,
    /// Whether the subtree's value leaves its borrowing frame: a return's
    /// expression, a `move` argument's staging, a function or lambda
    /// body's tail. A `~Copyable` window read in such a position escapes
    /// ([`ErrorCode::BorrowEscapes`]); every other consuming read (a local
    /// alias, a display hole) keeps the borrow-copy discipline.
    pub(super) escaping: bool,
}

impl<'a> Lin<'a> {
    /// First pass: one file's top-level binder patterns come live.
    /// Every file registers before any walk pass.
    pub(super) fn register_top_bindings(&mut self, ast: &'a Ast) {
        for block in &ast.blocks {
            for item in &block.items {
                if let ItemKind::Local(local) = &item.kind {
                    self.bind_pattern_defs(&local.pat);
                }
            }
        }
    }

    /// Second pass: one file's top-level initializers (a bodied `fn` is
    /// one, through its lambda), bodies, and declaration shapes.
    pub(super) fn top_level_and_bodies(&mut self, ast: &'a Ast) {
        for block in &ast.blocks {
            for item in &block.items {
                match &item.kind {
                    // The initializer walks alone: the pattern's
                    // definitions are live since the first pass, and
                    // re-binding would mask a move the initializer made.
                    ItemKind::Local(local) => {
                        self.expr(&local.init, true);
                    },
                    _ => self.item(item),
                }
                // One top-level walk sits outside every snapshot region, so
                // the writes it leaves logged are never rewound.
                self.undo.clear();
            }
        }
    }

    fn item(&mut self, item: &'a Item) {
        // A top-level item walk starts outside every snapshot region, and the
        // local-table clear is a scope-wide reset no rewind may undo.
        debug_assert!(self.undo.is_empty());
        self.state
            .retain(|place, _| !matches!(place, Place::Local(_)));
        match &item.kind {
            ItemKind::Fn(f) => {
                if f.body.is_some() {
                    self.fn_decl(f);
                }
            },
            ItemKind::Trait(t) => {
                for member in &t.members {
                    if let TraitMember::Fn(f) = member
                        && f.body.is_some()
                    {
                        self.fn_decl(f);
                    }
                }
            },
            ItemKind::Impl(imp) => {
                for f in &imp.fns {
                    if f.body.is_some() {
                        self.fn_decl(f);
                    }
                }
            },
            ItemKind::Struct(s) => {
                self.deinit_shape(s);
                self.deinit_bodies(s);
            },
            ItemKind::Local(_)
            | ItemKind::ExternFn(_)
            | ItemKind::Enum(_)
            | ItemKind::Tag(_)
            | ItemKind::TypeAlias(_)
            | ItemKind::Import(_)
            | ItemKind::Platform(_) => {},
        }
    }

    /// One function body: parameters bind (owned only in `move` mode),
    /// the body walks, and live owned parameters drop at the end.
    fn fn_decl(&mut self, f: &'a FnDecl) {
        self.scopes.push(Vec::new());
        for param in &f.params {
            self.bind_param(param);
        }
        if let Some(body) = &f.body {
            // The body's tail expression is the function's answer, so its
            // value leaves the frame (an explicit `return` marks its own
            // subtree).
            let outer = self.escaping;
            self.escaping = true;
            self.block(body);
            self.escaping = outer;
        }
        self.drop_scope(f.body.as_ref().map_or(f.name.span, |b| b.span));
    }

    /// Walks every declared `deinit` body like a borrowed-receiver
    /// function body: `self` is the dying instance, never owned by
    /// this frame, so its uses borrow and its fields' last-use facts —
    /// what the deinit function's own lowering consumes — record here.
    fn deinit_bodies(&mut self, s: &'a StructDecl) {
        for member in &s.members {
            let StructMember::Deinit { span, body } = member else {
                continue;
            };
            self.scopes.push(Vec::new());
            let copyable = self.facts.get(span).is_none_or(|fact| fact.copyable);
            self.bind_binder(*span, "self", copyable, false);
            self.block(body);
            self.drop_scope(*span);
        }
    }

    /// The declarative deinit checks: `deinit` lives on `~Copyable`
    /// declarations only, and at most once per struct.
    fn deinit_shape(&mut self, s: &'a StructDecl) {
        let linear = s.supers.iter().any(linear_marker);
        let mut first: Option<Span> = None;
        for member in &s.members {
            if let StructMember::Deinit { span, .. } = member {
                if let Some(first_span) = first {
                    self.out.diags.push(
                        Diagnostic::error(
                            ErrorCode::DeinitIllFormed,
                            "a struct declares at most one `deinit` [C5]".to_owned(),
                        )
                        .with_primary(*span, "second `deinit`")
                        .with_secondary(first_span, "first `deinit`"),
                    );
                } else {
                    first = Some(*span);
                }
                if !linear {
                    self.out.diags.push(
                        Diagnostic::error(
                            ErrorCode::DeinitIllFormed,
                            "deinit is restricted to `~Copyable` types [C5]".to_owned(),
                        )
                        .with_primary(*span, "deinit outside a `~Copyable` declaration"),
                    );
                }
            }
        }
    }

    // ---- scopes and binders ---------------------------------------------------

    fn bind_param(&mut self, param: &'a Param) {
        let copyable = self
            .facts
            .get(&param.name.span)
            .is_none_or(|fact| fact.copyable);
        self.bind_binder(
            param.name.span,
            &param.name.name,
            copyable,
            matches!(param.mode, ParamMode::Consume(_)),
        );
    }

    fn bind_binder(&mut self, span: Span, name: &'a str, copyable: bool, owned: bool) {
        if self.audit {
            return;
        }
        let place = Place::Local(span);
        self.info.insert(place, Info {
            name,
            copyable,
            owned,
            epoch: self.epoch,
        });
        self.set_state(place, State::Live);
        if let Some(scope) = self.scopes.last_mut() {
            scope.push(place);
        }
    }

    /// Pattern binders of a local or arm: owned, facts from the walk.
    fn bind_pattern(&mut self, pat: &'a Pattern) {
        for (span, name) in Self::pattern_binders(pat) {
            let copyable = self.facts.get(&span).is_none_or(|fact| fact.copyable);
            self.bind_binder(span, name, copyable, true);
        }
    }

    /// Pattern binders as top-level definitions: owned, copyable from
    /// the memoized top-level types, no scope-exit death.
    fn bind_pattern_defs(&mut self, pat: &'a Pattern) {
        for (_, name) in Self::pattern_binders(pat) {
            let Some(def) = self.package.lookup_value(name) else {
                continue;
            };
            let place = Place::Def(def);
            let copyable = self
                .def_types
                .get(&def)
                .is_some_and(|ty| ty_copyable(self.decls, *ty, &FxHashMap::default()));
            self.info.insert(place, Info {
                name,
                copyable,
                owned: true,
                epoch: 0,
            });
            // Package-wide registration precedes every snapshot region, so
            // this write is not retroactively undoable.
            self.state.insert(place, State::Live);
        }
    }

    /// One pattern's binder (name span, name) pairs, in walk order — the
    /// shared binder walker.
    fn pattern_binders(pat: &'a Pattern) -> Vec<(Span, &'a str)> {
        crate::pattern::pattern_binders(pat)
    }

    /// Installs a parked prefix: the served definition places' static
    /// facts (names re-borrowed from this compile's package, whose
    /// namespace the serve rebuilt identically), their boundary states,
    /// and their last-use entries — a compiled file's later use of a
    /// served binding overwrites the entry exactly as the one-walk order
    /// does.
    pub(super) fn install_served(&mut self, served: &ServedLinearity, package: &'a Package) {
        for (def, fact) in &served.defs {
            self.info.insert(Place::Def(*def), Info {
                name: package.def(*def).name.as_str(),
                copyable: fact.copyable,
                owned: fact.owned,
                epoch: 0,
            });
        }
        for (def, state) in &served.states {
            self.state.insert(Place::Def(*def), *state);
        }
        self.last_use.extend(
            served
                .last_use
                .iter()
                .map(|(&place, &entry)| (place, entry)),
        );
    }

    /// The walk so far as a parked prefix, taken at the served boundary —
    /// after the served files' walks, before any compiled file's. Local
    /// places never ride: their spans are unreachable from another file,
    /// and the next item's local-table clear drops them in the one walk
    /// anyway. Last uses park per place (an installing walk resolves them
    /// with its own package), so the rows themselves never serialize.
    pub(super) fn snapshot_prefix(&self) -> ServedLinearity {
        let defs = self
            .info
            .iter()
            .filter_map(|(place, info)| match place {
                Place::Def(def) => Some((*def, DefFact {
                    copyable: info.copyable,
                    owned: info.owned,
                })),
                Place::Local(_) => None,
            })
            .collect();
        let states = self
            .state
            .iter()
            .filter_map(|(place, state)| match place {
                Place::Def(def) => Some((*def, *state)),
                Place::Local(_) => None,
            })
            .collect();
        ServedLinearity {
            out: Linearity {
                diags: self.out.diags.clone(),
                moves: self.out.moves.clone(),
                drops: self.out.drops.clone(),
                last_uses: Vec::new(),
            },
            defs,
            states,
            last_use: self.last_use.clone(),
        }
    }

    /// Records the scope-exit deaths of one scope's live owned
    /// non-copyable binders, in bind order.
    fn drop_scope(&mut self, at: Span) {
        let places = self.scopes.pop().unwrap_or_default();
        if self.audit {
            return;
        }
        for place in places {
            if self.owns_live_noncopyable(place) {
                self.out.drops.push(DropSite {
                    binder: place.binder_span(self.package),
                    at,
                });
            }
        }
    }

    fn owns_live_noncopyable(&self, place: Place) -> bool {
        self.info.get(&place).copied().is_some_and(|info| {
            info.owned && !info.copyable && matches!(self.state.get(&place), Some(State::Live))
        })
    }

    // ---- the read/move core -----------------------------------------------------

    /// One use of a binding: checks initialization and liveness, records
    /// the last use, and moves when the use is consuming.
    pub(super) fn read(&mut self, place: Place, at: Span, consuming: bool) {
        let Some(info) = self.info.get(&place).copied() else {
            return;
        };
        match self.state.get(&place).copied() {
            Some(State::Live) => {
                // Inside a lambda or spawn body, using an outer binding is
                // a capture: the closure owns the value from here on.
                let captured = self.epoch > info.epoch;
                // A lambda's capture of a mutable owned binding moves it
                // into the capture block: the
                // sink records the place so the lambda's post-walk kills
                // it in the enclosing scope. A spawned body keeps the
                // spawn seam's acquire discipline — the spawn arm suspends
                // the sink for its walk, so its captures never land here.
                if self.is_captured_var(place)
                    && let Some(sink) = &mut self.capture_sink
                    && !sink.contains(&place)
                {
                    sink.push(place);
                }
                let moving = info.owned && !info.copyable && (consuming || captured);
                // A consuming use of a borrowed `~Copyable` value (a
                // borrow/`mut` parameter's window) at a position whose
                // value leaves the frame hands out an ownership the window
                // does not hold — a double deinit at runtime — so the
                // escape rejects here. Local aliases and display
                // reads keep the borrow-copy discipline (the value dies
                // inside the window's own scope, the hook staying with the
                // owner), and a capture's own channel is the lowerer's
                // deinit-carrying-capture refusal, not this one.
                if !info.owned && !info.copyable && consuming && self.escaping && !captured {
                    let diag = Diagnostic::error(
                        ErrorCode::BorrowEscapes,
                        format!(
                            "value `{}` is borrowed for this call; a `~Copyable` value cannot \
                             escape its window [C2]",
                            info.name
                        ),
                    )
                    .with_primary(at, "escape of a borrowed value")
                    .with_secondary(place.binder_span(self.package), "borrowed here");
                    self.report_once(place, at, diag);
                }
                self.last_use.insert(place, (at, !moving));
                if moving && !self.audit {
                    self.out.moves.push(Move {
                        binder: place.binder_span(self.package),
                        at,
                    });
                    self.set_state(place, State::Dead { at });
                }
            },
            Some(State::Dead { at: moved_at }) => {
                self.report_once(place, at, moved_diag(info.name, at, moved_at));
            },
            Some(State::Uninit) => {
                let diag = Self::uninit_diag(info.name, place, at, self.package);
                self.report_once(place, at, diag);
            },
            None => {},
        }
    }

    /// Whether `place` is a mutable owned local captured by a body the
    /// current lambda walk is inside (the kill sink's admission
    /// predicate): the binding moves into the capture block at the
    /// construction, so the enclosing scope's later uses are use-after-move.
    /// Borrows and `mut` windows never qualify (ownership the
    /// frame does not hold), nor do spawned captures (the sink is
    /// suspended there).
    fn is_captured_var(&self, place: Place) -> bool {
        let Place::Local(bind) = place else {
            return false;
        };
        let Some(info) = self.info.get(&place).copied() else {
            return false;
        };
        info.owned
            && self.epoch > info.epoch
            && self.facts.get(&bind).is_some_and(|fact| fact.mutable)
    }

    /// Pushes `diag` for this use site unless the audit round already
    /// reported it.
    fn report_once(&mut self, place: Place, at: Span, diag: Diagnostic) {
        if self.reported.insert((place, at)) {
            self.out.diags.push(diag);
        }
    }

    // ---- state writes and branch snapshots ----------------------------------

    /// The single funnel for state writes during a body walk: logs the
    /// previous state for a rewind, and writes nothing in audit mode.
    fn set_state(&mut self, place: Place, state: State) {
        if self.audit {
            return;
        }
        let prev = self.state.insert(place, state);
        self.undo.push(StateUndo { place, prev });
    }

    /// Undoes every state write logged since `mark`, newest first: the walk
    /// is back at that region's entry state.
    fn rewind(&mut self, mark: usize) {
        for undo in self.undo.drain(mark..).rev() {
            match undo.prev {
                Some(prev) => self.state.insert(undo.place, prev),
                None => self.state.remove(&undo.place),
            };
        }
    }

    /// A region's writes as a delta over its entry state. Each written
    /// place's final state is the live table's, so this runs before the
    /// region's rewind.
    fn delta_since(&self, mark: usize) -> Delta {
        let mut delta = Delta::default();
        for undo in &self.undo[mark..] {
            if let Some(state) = self.state.get(&undo.place) {
                delta.insert(undo.place, *state);
            }
        }
        delta
    }

    /// Writes a delta's entries, so an enclosing region's rewind undoes
    /// them too.
    fn apply_delta(&mut self, delta: &Delta) {
        for (place, state) in delta {
            self.set_state(*place, *state);
        }
    }

    /// Merges a loop body's delta into the entry state in place and rewinds:
    /// a place the body moved stays moved after the loop, since the back edge
    /// re-executes it.
    fn merge_loop(&mut self, mark: usize) {
        let mut body = self.delta_since(mark);
        self.rewind(mark);
        body.retain(|place, state| match self.state.get(place) {
            // The body's move stands over the entry state's live claim; every
            // other place back at its entry state drops out of the delta.
            Some(entry_state) => {
                matches!(entry_state, State::Live) && !matches!(state, State::Live)
            },
            None => false,
        });
        self.apply_delta(&body);
    }

    // ---- statements ------------------------------------------------------------

    fn block(&mut self, block: &'a BlockExpr) -> bool {
        self.scopes.push(Vec::new());
        let mut diverges = false;
        for (index, stmt) in block.stmts.iter().enumerate() {
            // The last expression statement is the block's value: it
            // inherits the enclosing escape context (a returned if-arm's
            // tail escapes with the return). Every other statement's value
            // is discarded in scope, so its reads never escape.
            let tail_value =
                index + 1 == block.stmts.len() && matches!(stmt.kind, ds_parse::StmtKind::Expr(_));
            let outer = self.escaping;
            if !tail_value {
                self.escaping = false;
            }
            if self.stmt(stmt) {
                diverges = true;
            }
            self.escaping = outer;
        }
        self.drop_scope(block.span);
        diverges
    }

    fn stmt(&mut self, stmt: &'a Stmt) -> bool {
        match &stmt.kind {
            ds_parse::StmtKind::Local(local) => {
                self.local(local);
                false
            },
            ds_parse::StmtKind::Fn(f) => {
                self.block_fn(f, stmt.span);
                false
            },
            ds_parse::StmtKind::Expr(expr) => self.expr(expr, true),
        }
    }

    /// One block-local `fn` declaration: the body walks as its own frame
    /// and its captures kill at the declaration's span, the lambda walk's
    /// own discipline. The declaration's own name binds inside the pushed
    /// scope for the body's reads, then again in the enclosing frame
    /// after the walk — a block-scoped binding the sibling statements
    /// borrow, never one a capture moves (the binder is immutable).
    fn block_fn(&mut self, f: &'a FnDecl, at: Span) {
        let Some(body) = &f.body else {
            return;
        };
        self.scopes.push(Vec::new());
        self.epoch += 1;
        for param in &f.params {
            self.bind_param(param);
        }
        self.bind_binder(f.name.span, &f.name.name, true, false);
        // The capture kill runs after the body walk: the sink collects
        // the mutable bindings the body captured, the construction's
        // kill the enclosing scope reads.
        let outer_sink = self.capture_sink.replace(Vec::new());
        let outer = self.escaping;
        self.escaping = true;
        self.block(body);
        self.escaping = outer;
        let captured_vars = self
            .capture_sink
            .take()
            .expect("the fn walk installs its sink");
        self.capture_sink = outer_sink;
        self.epoch -= 1;
        if !self.audit {
            for place in captured_vars {
                if matches!(self.state.get(&place), Some(State::Live)) {
                    self.out.moves.push(Move {
                        binder: place.binder_span(self.package),
                        at,
                    });
                    self.set_state(place, State::Dead { at });
                }
            }
        }
        self.drop_scope(at);
        let copyable = self
            .facts
            .get(&f.name.span)
            .is_none_or(|fact| fact.copyable);
        self.bind_binder(f.name.span, &f.name.name, copyable, true);
    }

    fn local(&mut self, local: &'a LocalDecl) {
        self.expr(&local.init, true);
        self.bind_pattern(&local.pat);
    }

    // ---- expressions -------------------------------------------------------------

    /// Walks one expression. `consuming` says whether a binding used at
    /// this position moves (value position) or borrows (window, test,
    /// structural comparison). Returns whether every path exits early.
    fn expr(&mut self, e: &'a Expr, consuming: bool) -> bool {
        match e {
            // A fn type in the constructor brackets is a pure type
            // spelling; no binding use. A conversion placeholder uses no
            // binding and exits no path.
            Expr::Literal { .. } | Expr::RawStr { .. } | Expr::FnType { .. } | Expr::Invalid(_) => {
                false
            },
            Expr::Str { parts, .. } => {
                for part in parts {
                    match part {
                        ds_parse::StrPart::Expr { span, expr } => {
                            let consuming = self.hole_dispatch_consuming(*span).unwrap_or(true);
                            self.expr(expr, consuming);
                        },
                        // `$x` is a consuming value use of the name, same
                        // as a `${x}` hole — unless the checker solved a
                        // `show` dispatch behind the hole, whose receiver
                        // passes per the solved member's mode
                        ds_parse::StrPart::Ident { span, name, .. } => {
                            let consuming = self.hole_dispatch_consuming(*span).unwrap_or(true);
                            self.name_use(name, consuming);
                        },
                        ds_parse::StrPart::Text { .. } => {},
                    }
                }
                false
            },
            Expr::Path { path, .. } => {
                self.path_use(path, consuming);
                false
            },
            Expr::Paren { inner, .. } => self.expr(inner, consuming),
            // A list literal's items move into the constructed list, like
            // a tuple's.
            Expr::List { items, .. } | Expr::Tuple { items, .. } => {
                for item in items {
                    self.expr(item, true);
                }
                false
            },
            Expr::Struct { fields, .. } => {
                for (name, value) in fields {
                    match value {
                        Some(expr) => {
                            self.expr(expr, true);
                        },
                        // Punning is a value use of the name.
                        None => self.name_use(name, true),
                    }
                }
                false
            },
            Expr::Lambda { .. } => self.lambda(e),
            Expr::Call { .. } => self.call(e),
            // A field projection: in a borrowed position it reads the
            // receiver; in a consuming position a copyable field reads
            // while a linear field transfers; inside a capture context it
            // always transfers (the closure must own the receiver).
            Expr::Field { recv, name, .. } => {
                let transfers = self.projection_transfers(recv, name, consuming);
                self.expr(recv, transfers)
            },
            // The keyed access reads the receiver by the same rule; the
            // bracketed constant projects the comptime record, whose
            // member is a string constant (no binding use). The field is
            // a per-copy fact, so a consuming read conservatively
            // transfers when any field of the receiver's shape could.
            Expr::FieldKey { recv, name, .. } => {
                let transfers = consuming && self.keyed_transfers(recv);
                self.expr(recv, transfers);
                self.expr(name, false);
                false
            },
            Expr::Index { recv, index, .. } => {
                self.expr(recv, consuming);
                self.expr(index, true);
                false
            },
            // The map application's bracketed arguments are type spellings;
            // a type-name path is no binding use, so the walk only
            // keeps the arm total.
            Expr::MapApply { key, value, .. } => {
                self.expr(key, consuming);
                self.expr(value, consuming);
                false
            },
            // The operand's value flows into the split; the failing path
            // exits the function, so no join is needed.
            Expr::Try { expr, .. } | Expr::Unary { expr, .. } => self.expr(expr, true),
            // `is` narrows by borrowing.
            Expr::Is { expr, .. } => self.expr(expr, false),
            Expr::Binary {
                op, left, right, ..
            } => {
                // Structural comparison borrows its operands.
                let borrowed = matches!(op, BinaryOp::Eq | BinaryOp::Ne);
                self.expr(left, !borrowed);
                self.expr(right, !borrowed);
                false
            },
            Expr::Range { start, end, .. } => {
                self.expr(start, true);
                self.expr(end, true);
                false
            },
            Expr::Assign { .. } => self.assign(e),
            Expr::If {
                cond, then, else_, ..
            } => self.if_expr(cond, then, else_.as_deref()),
            Expr::Match {
                scrutinee, arms, ..
            } => self.match_expr(scrutinee, arms),
            Expr::While { cond, body, .. } => self.while_expr(cond, body),
            Expr::For {
                pat,
                iter,
                body,
                span,
                ..
            } => self.for_expr(pat, iter, body, *span),
            Expr::Return { expr, .. } => {
                if let Some(expr) = expr {
                    let outer = self.escaping;
                    self.escaping = true;
                    self.expr(expr, true);
                    self.escaping = outer;
                }
                true
            },
            Expr::Break(_) | Expr::Continue(_) => true,
            // The nursery body runs now; `defer` blocks are walked at
            // their source position.
            Expr::Nursery { body, .. } | Expr::Defer { body, .. } => self.block(body),
            // The child body's uses are captures: ownership moves at the
            // spawn expression, on the current state. The capture
            // sink suspends for the walk — a spawned child's captures
            // keep the spawn seam's acquire discipline, so no kill fires
            // for them at any enclosing construction.
            Expr::Spawn { body, .. } => {
                self.epoch += 1;
                let sink = self.capture_sink.take();
                self.block(body);
                self.capture_sink = sink;
                self.epoch -= 1;
                false
            },
            Expr::Block(block) => self.block(block),
        }
    }

    fn path_use(&mut self, path: &Path, consuming: bool) {
        let [only] = path.segments.as_slice() else {
            return;
        };
        match self.res.and_then(|res| res.get(only.span)) {
            Some(Res::Local { bind }) => self.read(Place::Local(*bind), only.span, consuming),
            Some(Res::Def(def)) => {
                // Function, type, and tag names used as values are
                // copyable references; only const/var bindings carry
                // linear values.
                if self.is_data_def(*def) {
                    self.read(Place::Def(*def), only.span, consuming);
                }
            },
            None | Some(Res::TraitMethod(_) | Res::Dot(_) | Res::Handler | Res::Module(_)) => {},
        }
    }

    fn name_use(&mut self, name: &'a Ident, consuming: bool) {
        match self.res.and_then(|res| res.get(name.span)) {
            Some(Res::Local { bind }) => self.read(Place::Local(*bind), name.span, consuming),
            Some(Res::Def(def)) => {
                if self.is_data_def(*def) {
                    self.read(Place::Def(*def), name.span, consuming);
                }
            },
            None | Some(Res::TraitMethod(_) | Res::Dot(_) | Res::Handler | Res::Module(_)) => {},
        }
    }

    /// The receiver mode one interpolation hole's solved `show` dispatch
    /// passes: consuming exactly when the solved member's receiver
    /// parameter consumes, mirroring the written dot-call's rule; `None`
    /// when no dispatch record sits at the span, leaving the hole's own
    /// value-use mode.
    fn hole_dispatch_consuming(&self, span: Span) -> Option<bool> {
        let method = self
            .hole_dispatches
            .get(&span)
            .map(|call| call.method)
            .or_else(|| self.hole_inherents.get(&span).copied())
            .or_else(|| self.hole_runtime.get(&span).copied())?;
        let sig = self.member_sig(method)?;
        Some(
            sig.params
                .first()
                .is_some_and(|p| matches!(p.mode, ParamMode::Consume(_))),
        )
    }

    fn is_data_def(&self, def: DefId) -> bool {
        matches!(self.package.def(def).kind, DefKind::Const | DefKind::Var)
    }

    fn lambda(&mut self, e: &'a Expr) -> bool {
        let Expr::Lambda {
            params, body, span, ..
        } = e
        else {
            return false;
        };
        self.scopes.push(Vec::new());
        // Parameters bind inside the capture boundary: they are locals of
        // this frame, so their uses in the body are not captures;
        // a nested lambda entered later still captures them.
        self.epoch += 1;
        for param in params {
            self.bind_param(param);
        }
        // The lambda's capture kill runs at the construction, after the
        // body walk: the sink collects the mutable bindings the body
        // captured (a nested lambda swaps in its own list, so the inner
        // construction's kill is the inner walk's).
        let outer_sink = self.capture_sink.replace(Vec::new());
        // The lambda's answer leaves it at its call, so the body
        // expression is an escape context like a function body's tail.
        let outer = self.escaping;
        self.escaping = true;
        let diverges = self.expr(body, true);
        self.escaping = outer;
        let captured_vars = self
            .capture_sink
            .take()
            .expect("the lambda walk installs its sink");
        self.capture_sink = outer_sink;
        self.epoch -= 1;
        // A captured `var` binding moved into the capture block at this
        // construction: every use in the enclosing scope after it reads a
        // dead binding. Only a binding still live here moves — an
        // affine capture already died at its own consuming use inside the
        // body, and a nested lambda's kill may have beaten this walk here.
        if !self.audit {
            for place in captured_vars {
                if matches!(self.state.get(&place), Some(State::Live)) {
                    self.out.moves.push(Move {
                        binder: place.binder_span(self.package),
                        at: *span,
                    });
                    self.set_state(place, State::Dead { at: *span });
                }
            }
        }
        self.drop_scope(*span);
        diverges
    }

    fn assign(&mut self, e: &'a Expr) -> bool {
        let Expr::Assign {
            op,
            left,
            right,
            span,
        } = e
        else {
            return false;
        };
        let Some(place) = self.place_of(left) else {
            // A field/subscript place chain (the assignment sugar): the
            // root binding reads borrowed (a copyable root — the type
            // walk refuses the linear ones — so no move, no drop), each
            // index probe moves into the synthesized `set` call, and the
            // value moves into the store.
            if let Some(root) = place_root(left) {
                let place = self
                    .res
                    .and_then(|res| res.get(root.span))
                    .and_then(|res| match res {
                        Res::Local { bind } => Some(Place::Local(*bind)),
                        Res::Def(def) if self.is_data_def(*def) => Some(Place::Def(*def)),
                        _ => None,
                    });
                if let Some(place) = place {
                    self.read(place, root.span, false);
                } else {
                    self.expr(left, false);
                }
                for step in place_steps(left) {
                    if let PlaceStep::Index { index } = step {
                        self.expr(index, true);
                    }
                }
            } else {
                self.expr(left, false);
            }
            self.expr(right, true);
            return false;
        };
        // A plain assignment to a captured `var` binding inside a lambda
        // body is the capture's own use: the sink records the place and
        // the last-use row names the assignment, so the lowerer's
        // construction sees the binding's final use inside the lambda and
        // moves it (the compound arm's read() records both).
        if *op == ds_parse::AssignOp::Plain
            && self.is_captured_var(place)
            && let Some(sink) = &mut self.capture_sink
            && !sink.contains(&place)
        {
            sink.push(place);
            self.last_use.insert(place, (left.span(), true));
        }
        if *op == ds_parse::AssignOp::Plain {
            self.expr(right, true);
            if !self.audit && self.owns_live_noncopyable(place) {
                // The replaced value dies at the assignment.
                self.out.drops.push(DropSite {
                    binder: place.binder_span(self.package),
                    at: *span,
                });
            }
            self.set_state(place, State::Live);
        } else {
            // Compound assignment reads the current value, then mutates it.
            self.read(place, left.span(), false);
            self.expr(right, true);
        }
        false
    }

    /// The uninit-use diagnostic: the use span primary, the binder secondary.
    fn uninit_diag(name: &str, place: Place, at: Span, package: &Package) -> Diagnostic {
        Diagnostic::error(
            ErrorCode::UninitializedUse,
            format!("binding `{name}` may not be initialized on this path"),
        )
        .with_primary(at, "may be uninitialized here")
        .with_secondary(place.binder_span(package), "bound here")
    }

    /// Whether a field projection in this position moves its receiver: it
    /// does inside capture contexts (the receiver must be owned there), in
    /// borrowed positions it never does, and elsewhere only when the
    /// projected field itself is non-copyable. Unknown shapes (unions,
    /// generic constructors) transfer conservatively.
    fn projection_transfers(&self, recv: &'a Expr, name: &Ident, consuming: bool) -> bool {
        if self.epoch
            > self
                .place_of(recv)
                .and_then(|place| self.info.get(&place).map(|info| info.epoch))
                .unwrap_or(0)
        {
            return true;
        }
        if !consuming {
            return false;
        }
        let Some(ty) = self.place_of(recv).and_then(|place| self.place_ty(place)) else {
            return true;
        };
        let mut all_copy = true;
        for atom in self.decls.universe.members(ty) {
            let Ok(decl) = self.decls.universe.atom_decl(*atom) else {
                return true;
            };
            if decl.name.starts_with('$') {
                return true;
            }
            match &decl.kind {
                AtomKind::Struct(fields) => {
                    let Some(field) = fields
                        .iter()
                        .find(|f| f.name.as_deref() == Some(name.name.as_str()))
                    else {
                        // A member without the field makes the projection
                        // ill-typed; let the type walk report it.
                        continue;
                    };
                    all_copy &=
                        expr_copyable(self.decls, &field.ty, no_linear_vars(), &mut Vec::new());
                },
                // Unions of shapes, generic applications, enums: the
                // projected type is not decidable here.
                _ => return true,
            }
        }
        !all_copy
    }

    /// The keyed access's consuming-transfer rule: true when any field
    /// of the receiver's shape is linear (the read may transfer one),
    /// false only when every field copies, undecidable shapes true.
    fn keyed_transfers(&self, recv: &Expr) -> bool {
        if self.epoch
            > self
                .place_of(recv)
                .and_then(|place| self.info.get(&place).map(|info| info.epoch))
                .unwrap_or(0)
        {
            return true;
        }
        let Some(ty) = self.place_of(recv).and_then(|place| self.place_ty(place)) else {
            return true;
        };
        let mut any_linear = false;
        for atom in self.decls.universe.members(ty) {
            let Ok(decl) = self.decls.universe.atom_decl(*atom) else {
                return true;
            };
            if decl.name.starts_with('$') {
                return true;
            }
            match &decl.kind {
                AtomKind::Struct(fields) => {
                    for field in fields {
                        any_linear |= !expr_copyable(
                            self.decls,
                            &field.ty,
                            no_linear_vars(),
                            &mut Vec::new(),
                        );
                    }
                },
                _ => return true,
            }
        }
        any_linear
    }

    /// Whether the checked walk marked this expression's answer
    /// diverging: a call to a `never` fn exits at the call, so the state
    /// after it is unreachable and the walk reports it as an early exit.
    fn expr_diverges(&self, span: Span) -> bool {
        self.expr_types.get(&span).is_some_and(|val| val.diverges)
    }

    fn place_ty(&self, place: Place) -> Option<Ty> {
        match place {
            Place::Local(span) => self.facts.get(&span).map(|fact| fact.ty),
            Place::Def(def) => self.def_types.get(&def).copied(),
        }
    }

    fn place_of(&self, left: &'a Expr) -> Option<Place> {
        let Expr::Path { path, .. } = peel(left) else {
            return None;
        };
        let [only] = path.segments.as_slice() else {
            return None;
        };
        match self.res.and_then(|res| res.get(only.span)) {
            Some(Res::Local { bind }) => Some(Place::Local(*bind)),
            Some(Res::Def(def)) if self.is_data_def(*def) => Some(Place::Def(*def)),
            _ => None,
        }
    }

    fn if_expr(&mut self, cond: &'a Expr, then: &'a BlockExpr, else_: Option<&'a Expr>) -> bool {
        self.expr(cond, true);
        // Both sides walk from the condition's state; each side's writes are
        // logged and rewound, so the join reads one delta per side instead of
        // one state table per side.
        let mark = self.undo.len();
        let then_diverges = self.block(then);
        let then_delta = self.delta_since(mark);
        self.rewind(mark);
        let else_diverges = match else_ {
            Some(else_expr) => self.expr(else_expr, true),
            None => false,
        };
        // A diverging side leaves the state of the side the walk reports.
        // The then side's writes are the join's accumulator.
        match (then_diverges, else_diverges) {
            // Only the else side produces; its writes still stand.
            (true, false) => {},
            // The else side diverges, so the then side's writes stand.
            (_, true) => {
                self.rewind(mark);
                self.apply_delta(&then_delta);
            },
            (false, false) => {
                let else_delta = self.delta_since(mark);
                self.rewind(mark);
                let mut merged = then_delta;
                merge_branch(&mut merged, &self.state, &else_delta);
                self.apply_delta(&merged);
            },
        }
        then_diverges && else_diverges
    }

    fn match_expr(&mut self, scrutinee: &'a Expr, arms: &'a [MatchArm]) -> bool {
        // The scrutinee feeds its pattern's binders, which are local
        // registrations (a pattern field never anchors a claim), and a
        // guard is a bool test: neither value leaves the frame, so the
        // match's escape context applies to the arm bodies alone.
        let outer = self.escaping;
        self.escaping = false;
        self.expr(scrutinee, true);
        self.escaping = outer;
        // Every arm starts from the scrutinee's state, and each arm's writes
        // are logged and rewound at its end.
        let mark = self.undo.len();
        // The first producing arm's table becomes the join's accumulator and
        // every later one merges into it, so one arm's table is live at a
        // time. The last arm's table stands when no arm produces, which is
        // the entry state when the match has no arms.
        let mut accumulator: Option<Delta> = None;
        let mut last: Option<Delta> = None;
        let mut all_diverge = true;
        for arm in arms {
            self.scopes.push(Vec::new());
            self.bind_pattern(&arm.pat);
            let mut diverges = arm
                .guard
                .as_ref()
                .is_some_and(|guard| self.expr(guard, true));
            if !diverges {
                diverges = self.expr(&arm.body, true);
            }
            self.drop_scope(arm.span);
            let delta = self.delta_since(mark);
            self.rewind(mark);
            all_diverge &= diverges;
            if diverges {
                last = Some(delta);
                continue;
            }
            match &mut accumulator {
                Some(accumulator) => merge_branch(accumulator, &self.state, &delta),
                None => accumulator = Some(delta),
            }
        }
        if let Some(join) = accumulator.or(last) {
            self.apply_delta(&join);
        }
        all_diverge
    }

    fn while_expr(&mut self, cond: &'a Expr, body: &'a BlockExpr) -> bool {
        self.expr(cond, true);
        let mark = self.undo.len();
        self.block(body);
        self.merge_loop(mark);
        // The loop re-executes the condition and body on the back edge:
        // one audit round against the merged state. A loop nested
        // inside an outer audit round skips its own: the audit walk
        // mutates nothing, so the round would re-check the same reads
        // against the same state, and the re-walk compounded per nesting
        // level (exponential in loop depth).
        if !self.audit {
            self.audit = true;
            self.expr(cond, true);
            self.block(body);
            self.audit = false;
        }
        false
    }

    fn for_expr(&mut self, pat: &'a Pattern, iter: &'a Expr, body: &'a Expr, span: Span) -> bool {
        // The comptime expansion loop runs its copies in sequence: each
        // copy is one straight-line execution, the linear state threading
        // across them (a copy's move stays moved), no back-edge audit.
        // A per-instance loop audits the largest expansion's copy count —
        // every expansion unrolls the same structural body, so the
        // largest prefix covers each entering shape's moves exactly —
        // and a written loop nested inside a per-instance copy counts
        // through its expansions too, never falling to the runtime-loop
        // walk its straight-line discipline forbids.
        if let Some(&copies) = self.copy_counts.get(&span) {
            let _ = iter;
            for _ in 0..copies {
                self.scopes.push(Vec::new());
                self.bind_pattern(pat);
                self.expr(body, true);
                self.drop_scope(body.span());
            }
            return false;
        }
        // The iterable feeds a linear cursor: a consuming use.
        self.expr(iter, true);
        let mark = self.undo.len();
        self.scopes.push(Vec::new());
        self.bind_pattern(pat);
        self.expr(body, true);
        self.drop_scope(body.span());
        self.merge_loop(mark);
        // The body re-executes per iteration; pattern binders are fresh
        // each round, so only outer bindings matter here. A loop nested
        // inside an outer audit round skips its own round: the audit
        // walk mutates nothing, so the round re-checks the same reads
        // against the same state, and the re-walk compounded per
        // nesting level (exponential in loop depth).
        if !self.audit {
            self.audit = true;
            self.expr(body, true);
            self.audit = false;
        }
        false
    }

    // ---- calls -----------------------------------------------------------------

    fn call(&mut self, e: &'a Expr) -> bool {
        let Expr::Call {
            callee,
            args,
            trailing,
            ..
        } = e
        else {
            return false;
        };
        let callee = peel(callee);
        if let Expr::Field { recv, name, .. } = callee {
            // The field tier: the call is the indirect call of the
            // field-read's word, so the callee walks as the read it is
            // (a borrow of the field's word), the arguments behind it.
            if matches!(
                self.res.and_then(|res| res.get(name.span)),
                Some(Res::Dot(DotRes::Field))
            ) {
                self.expr(callee, false);
                self.borrow_args(args, trailing.as_deref());
                return self.expr_diverges(e.span());
            }
            let _ = self.dot_call(recv, name, args, trailing.as_deref());
            return self.expr_diverges(e.span());
        }
        if let Expr::Path { path, .. } = callee {
            match path.segments.as_slice() {
                [only] => {
                    if let Some(Res::Def(def)) =
                        self.res.and_then(|res| res.get(only.span)).cloned()
                        && let Some(sig) = self.decls.fns.get(&def).cloned()
                    {
                        let _ = self.sig_call(&sig, None, args, trailing.as_deref());
                        return self.expr_diverges(e.span());
                    }
                },
                [_, method] => {
                    if let Some(Res::TraitMethod(method_id)) =
                        self.res.and_then(|res| res.get(method.span)).cloned()
                        && let Some(sig) = self.member_sig(method_id)
                    {
                        let _ = self.sig_call(&sig, None, args, trailing.as_deref());
                        return self.expr_diverges(e.span());
                    }
                },
                _ => {},
            }
        }
        // An indirect or unknown callee: borrow everything (call-scoped
        // windows close at the call).
        self.expr(callee, false);
        for arg in args {
            self.expr(&arg.value, false);
        }
        if let Some(tail) = trailing.as_deref() {
            self.expr(tail, false);
        }
        self.expr_diverges(e.span())
    }

    fn dot_call(
        &mut self,
        recv: &'a Expr,
        name: &'a Ident,
        args: &'a [CallArg],
        trailing: Option<&'a Expr>,
    ) -> bool {
        let Some(res) = self.res.and_then(|res| res.get(name.span)).cloned() else {
            self.expr(recv, false);
            self.borrow_args(args, trailing);
            return false;
        };
        match res {
            Res::Handler => {
                // `.err(h)`/`.ok(h)`: the operand's value feeds the split.
                self.expr(recv, true);
                for arg in args {
                    self.expr(&arg.value, true);
                }
                false
            },
            Res::Dot(DotRes::Free { func, fed_param }) => {
                if let Some(sig) = self.decls.fns.get(&func).cloned() {
                    self.sig_call(&sig, (fed_param == 0).then_some(recv), args, trailing)
                } else {
                    self.expr(recv, false);
                    self.borrow_args(args, trailing);
                    false
                }
            },
            Res::Dot(DotRes::Methods { candidates }) => {
                let sigs: Vec<Arc<FnSig>> = candidates
                    .iter()
                    .filter_map(|id| self.member_sig(*id))
                    .collect();
                let receiver_consumes = sigs.iter().all(|sig| {
                    sig.params
                        .first()
                        .is_some_and(|p| matches!(p.mode, ParamMode::Consume(_)))
                });
                self.expr(recv, receiver_consumes);
                self.borrow_args(args, trailing);
                false
            },
            Res::Dot(DotRes::Static { candidates }) => {
                // The type-keyed member call: the receiver is a type name,
                // never a value use. The check converged to one member, so
                // the arguments walk per its signature like a free call — a
                // `move` parameter consumes, or the use-after-move behind it
                // stays live and the scope exit double-drops.
                if let Some(sig) = candidates.first().and_then(|id| self.member_sig(*id)) {
                    self.sig_call(&sig, None, args, trailing)
                } else {
                    self.borrow_args(args, trailing);
                    false
                }
            },
            _ => {
                self.expr(recv, false);
                self.borrow_args(args, trailing);
                false
            },
        }
    }

    fn borrow_args(&mut self, args: &'a [CallArg], trailing: Option<&'a Expr>) {
        for arg in args {
            self.expr(&arg.value, false);
        }
        if let Some(tail) = trailing {
            self.expr(tail, false);
        }
    }

    fn member_sig(&self, method: MethodId) -> Option<Arc<FnSig>> {
        self.decls
            .members
            .get(&method)
            .or_else(|| self.decls.inherent.get(&method))
            .cloned()
    }

    /// Walks one call's arguments per the callee's parameter modes: an
    /// argument consumes its binding only in `move` mode.
    fn sig_call(
        &mut self,
        sig: &FnSig,
        fed: Option<&'a Expr>,
        args: &'a [CallArg],
        trailing: Option<&'a Expr>,
    ) -> bool {
        let slots = Self::slots(fed, 0, args, trailing, sig);
        for (slot, param) in slots.iter().zip(sig.params.iter()) {
            if let Some(arg) = slot {
                let consume = matches!(param.mode, ParamMode::Consume(_));
                let outer = self.escaping;
                if consume {
                    self.escaping = true;
                }
                self.expr(arg, consume);
                self.escaping = outer;
            }
        }
        false
    }

    /// Maps arguments to parameters like the type walk does (fed receiver,
    /// named, positional, trailing), without its diagnostics.
    fn slots<'x>(
        fed: Option<&'x Expr>,
        skip: usize,
        args: &'x [CallArg],
        trailing: Option<&'x Expr>,
        sig: &FnSig,
    ) -> Vec<Option<&'x Expr>> {
        let mut slots: Vec<Option<&'x Expr>> = vec![None; sig.params.len()];
        let mut next = skip;
        if let Some(fed) = fed
            && let Some(first) = slots.first_mut()
        {
            *first = Some(fed);
            next = 1;
        }
        for arg in args {
            match &arg.name {
                Some(name) => {
                    if let Some(index) = sig.params.iter().position(|p| p.name == name.name)
                        && slots[index].is_none()
                    {
                        slots[index] = Some(&arg.value);
                    }
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
            && slots[next].is_none()
        {
            slots[next] = Some(tail);
        }
        slots
    }
}
