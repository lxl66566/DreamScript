//! Queries and rendering: the nursery and scope-frame stacks, the atom
//! decomposition helpers, and the type names the diagnostics render.

use super::*;

impl Bodies<'_> {
    /// The method's registered signature, shared (the handle clones; the
    /// AST-shaped signature never deep-copies at call sites).
    pub(super) fn member_sig(&self, method: MethodId) -> Option<Arc<FnSig>> {
        self.decls
            .members
            .get(&method)
            .or_else(|| self.decls.inherent.get(&method))
            .cloned()
    }

    /// Whether the solve that just failed already reported its precise
    /// diagnostic (the tuple distribution cap); a generic fallback then
    /// stays silent so the golden carries one code, not two.
    /// Whether a qualified pattern label already reported its refusal,
    /// consumed by the caller about to print the unknown-pattern word.
    pub(super) fn take_pattern_label_reported(&mut self) -> bool {
        std::mem::take(&mut self.pattern_label_reported)
    }

    pub(super) fn take_solve_reported(&mut self) -> bool {
        std::mem::take(&mut self.solve_reported)
    }

    /// The type def a name carries for the walked file: the
    /// file's own shadowing tag declaration beats the first declaration
    /// per label; the foreign-defs pass reads the shared namespace.
    pub(super) fn type_def_of(&self, name: &str) -> Option<DefId> {
        match self.file {
            Some(file) => self.package.lookup_type_in(file, name),
            None => self.package.lookup_type(name),
        }
    }

    /// A nursery block: the body checks in a fresh scope over the
    /// block's own nursery scope entry, every `spawn` inside it records
    /// its body's answer, and the block's value is the collected
    /// answers' list — the join channel's checker face. The element
    /// type is the union of the spawned bodies' answers (a body that
    /// only cancels never answers, so it contributes nothing); a block
    /// whose spawns never answer at all keeps the unit element, the
    /// empty list's honest inhabitant.
    pub(super) fn nursery_block(&mut self, body: &BlockExpr, span: Span) -> Val {
        self.nursery_scopes.push(NurseryScope {
            outlived: self.locals.live_keys(),
            at: span,
        });
        self.spawn_answers.push(Vec::new());
        let saved = self.snapshot();
        self.refined.clear();
        self.value_pos = false;
        self.block(body);
        self.restore(saved);
        self.nursery_scopes.pop();
        let answers = self
            .spawn_answers
            .pop()
            .expect("the nursery pushed its collector");
        let mut elem = self.empty();
        let mut answering = false;
        for answer in answers {
            if !answer.diverges {
                elem = self.decls.universe.union(elem, answer.ty);
                answering = true;
            }
        }
        // a block whose spawns never answer at all (every body cancels,
        // or no spawn stands) keeps the unit element — the empty list's
        // honest inhabitant
        if !answering {
            elem = self.unit();
        }
        let ty = self.applied_list_ty(elem).unwrap_or_else(|| self.unit());
        Val {
            ty,
            diverges: false,
            poisoned: false,
        }
    }

    /// A spawned body: its own frame, its tail checked as the
    /// child's answer, and the spawn-scoped `cancel` refinement — a
    /// `cancel` arm inside a spawned body never answers, so it drops
    /// out of the body's own unions instead of polluting the answer
    /// channel with the staging signature's unit. The expression itself
    /// stays unit: the answers collect at the enclosing nursery's join.
    pub(super) fn spawned_block(&mut self, body: &BlockExpr) -> Val {
        let saved = self.snapshot();
        self.refined.clear();
        let frame = self.enter_frame();
        self.spawn_depth += 1;
        self.value_pos = true;
        let val = self.block(body);
        self.spawn_depth -= 1;
        self.exit_frame(frame);
        self.restore(saved);
        // the body's answer lands on the span the lowerer plans the
        // child function's return from
        self.record_val(body.span, val);
        if let Some(answers) = self.spawn_answers.last_mut() {
            answers.push(val);
        }
        Val {
            ty: self.unit(),
            diverges: false,
            poisoned: false,
        }
    }

    /// Enters one lambda or spawned-body frame: the lexical loop depth
    /// stays with the enclosing frame because loop exits never cross a
    /// function or lambda frame boundary. Returns the saved depth for the
    /// paired [`Self::exit_frame`].
    pub(super) fn enter_frame(&mut self) -> (usize, Vec<usize>) {
        self.frame_depth += 1;
        (
            std::mem::replace(&mut self.loop_depth, 0),
            std::mem::take(&mut self.loop_breaks),
        )
    }

    /// The paired exit of [`Self::enter_frame`].
    pub(super) fn exit_frame(&mut self, saved: (usize, Vec<usize>)) {
        self.frame_depth -= 1;
        self.loop_depth = saved.0;
        self.loop_breaks = saved.1;
    }

    pub(super) fn snapshot(&self) -> ScopeMark {
        ScopeMark {
            locals: self.locals.mark(),
            refined: self.refined.mark(),
            assumptions: self.assumptions.len(),
            ret: self.ret,
        }
    }

    pub(super) fn restore(&mut self, saved: ScopeMark) {
        self.locals.restore_to(saved.locals);
        self.refined.restore_to(saved.refined);
        self.assumptions.truncate(saved.assumptions);
        self.ret = saved.ret;
    }

    pub(super) fn local_ty(&self, bind: Span) -> Option<Ty> {
        self.refined
            .get(bind)
            .copied()
            .or_else(|| self.locals.get(bind).map(|b| b.ty))
    }

    /// Whether a solved type is exactly the builtin `type` atom: the
    /// comptime type-value type.
    pub(super) fn is_type_atom(&self, ty: Ty) -> bool {
        matches!(self.decls.universe.members(ty), [atom] if *atom == TypeId::TYPE)
    }

    /// Whether a package definition names a declared type — the type-
    /// namespace kinds a type value can denote (a struct, enum, tag, or
    /// alias; a case keeps its constructor-value reading).
    pub(super) fn names_declared_type(&self, def: DefId) -> bool {
        matches!(
            self.package.def(def).kind,
            DefKind::Struct | DefKind::Enum | DefKind::Tag | DefKind::TypeAlias
        )
    }

    /// The binder span when an expression is a local path.
    pub(super) fn path_local(&self, expr: &Expr) -> Option<Span> {
        let Expr::Path { path, .. } = peel_paren(expr) else {
            return None
        };
        let [only] = path.segments.as_slice() else {
            return None
        };
        match self.res.get(only.span) {
            Some(Res::Local { bind }) => Some(*bind),
            _ => None,
        }
    }

    pub(super) fn cover_ty(&mut self, covered: &[TypeId]) -> Ty {
        let mut ty = self.empty();
        for atom in covered {
            if let Ok(atom_ty) = self.decls.universe.atom_ty(*atom) {
                ty = self.decls.universe.union(ty, atom_ty);
            }
        }
        ty
    }

    /// The scrutinee members no arm covers.
    pub(super) fn missing_ty(&mut self, scrut: Ty, covered: &[TypeId]) -> Ty {
        let covered_ty = self.cover_ty(covered);
        self.decls.universe.diff(scrut, covered_ty)
    }

    pub(super) fn render_members(&mut self, members: &[TypeId]) -> String {
        let ty = self.cover_ty(members);
        format!("`{}`", self.decls.universe.render(ty))
    }

    pub(super) fn struct_shape_of(&self, atom: TypeId) -> Option<&decls::StructShape> {
        let def = self.decls.defs_of_atoms.get(&atom).copied()?;
        self.decls.structs.get(&def)
    }

    /// Payload types of a struct (named fields, positionally matched) or an
    /// enum case, for call patterns.
    pub(super) fn payload_tys(&mut self, _path: &Path, atom: TypeId) -> Option<Vec<Ty>> {
        let def = self.decls.defs_of_atoms.get(&atom).copied()?;
        if let Some(shape) = self.decls.structs.get(&def).cloned() {
            let mut out = Vec::with_capacity(shape.fields.len());
            for field in &shape.fields {
                out.push(self.solve_here(&field.ty)?);
            }
            return Some(out);
        }
        // A minted tuple-payload case carries no struct shape; its
        // payload types come from the case table in declaration order.
        let tuple_payload = match self.decls.cases.get(&def).map(|c| &c.payload) {
            Some(EnumPayload::Tuple(types)) => Some(types.clone()),
            _ => None,
        };
        if let Some(types) = tuple_payload {
            let mut out = Vec::with_capacity(types.len());
            for ty in &types {
                out.push(self.solve_here(ty)?);
            }
            return Some(out);
        }
        None
    }

    pub(super) fn atom_name(&self, atom: TypeId) -> String {
        self.decls
            .universe
            .atom_decl(atom)
            .map_or_else(|_| "<unknown>".to_owned(), |d| d.name.clone())
    }

    /// The application shape of a singleton type, as constructor plus
    /// argument types.
    pub(super) fn single_apply_of(&self, atom: TypeId) -> Option<(TypeId, Vec<Ty>)> {
        match self.decls.universe.atom_decl(atom).ok()?.kind {
            AtomKind::Apply(ctor, ref args) => Some((ctor, args.to_vec())),
            _ => None,
        }
    }

    /// The application shape of a whole type, when it is one application.
    pub(super) fn single_apply(&mut self, ty: Ty) -> Option<(TypeId, Vec<Ty>)> {
        let [only] = self.decls.universe.members(ty) else {
            return None
        };
        self.single_apply_of(*only)
    }

    /// The applied enum head of a scrutinee type with its minted case
    /// atoms: `(head atom, case atoms, any unminted case)` — the head
    /// atom is the application itself, the member a catchall covers.
    /// Applications stay opaque to the set algebra, so case patterns
    /// over an enum application cover it case-wise — a unit case's atom
    /// carries no substitution, while a case minting no atom (a
    /// payloaded case over a generic head) can never be covered. `None`
    /// when the scrutinee is not one application of a declared enum
    /// head.
    pub(super) fn applied_enum_head(&mut self, scrut: Ty) -> Option<(TypeId, Vec<TypeId>, bool)> {
        let [only] = self.decls.universe.members(scrut) else {
            return None
        };
        let (ctor, _) = self.single_apply_of(*only)?;
        let enum_def = self.decls.defs_of_atoms.get(&ctor).copied()?;
        let shape = self.decls.enums.get(&enum_def)?.clone();
        let mut atoms = Vec::with_capacity(shape.cases.len());
        let mut unminted = false;
        for (name, ..) in &shape.cases {
            match self
                .type_def_of(name)
                .or_else(|| self.decls.foreign_type(name))
                .and_then(|def| self.decls.atoms.get(&def).copied())
            {
                Some(atom) => atoms.push(atom),
                None => unminted = true,
            }
        }
        Some((*only, atoms, unminted))
    }

    pub(super) fn decompose_fn(&mut self, ty: Ty) -> Option<(Vec<Ty>, Ty)> {
        let [only] = self.decls.universe.members(ty) else {
            return None
        };
        self.decompose_fn_atom(*only)
    }

    pub(super) fn decompose_fn_atom(&self, atom: TypeId) -> Option<(Vec<Ty>, Ty)> {
        let (params, ret) = self.decls.universe.fn_sig(atom)?;
        Some((params.to_vec(), ret))
    }

    pub(super) fn fn_typed(&self, ty: Ty) -> bool {
        self.decls
            .universe
            .members(ty)
            .iter()
            .any(|atom| self.decls.universe.fn_sig(*atom).is_some())
    }

    /// The per-element expected types of one expected tuple type: the
    /// expectation must be a single tuple atom of the construction's
    /// arity (a union of tuples or a wider member set carries no
    /// element-wise expectation).
    pub(super) fn expected_tuple_elems(&mut self, expected: Ty, arity: usize) -> Option<Vec<Ty>> {
        let [only] = self.decls.universe.members(expected) else {
            return None;
        };
        let elements = self.tuple_elements(*only)?;
        if elements.len() != arity {
            return None;
        }
        elements
            .iter()
            .map(|e| self.decls.universe.atom_ty(*e).ok())
            .collect()
    }

    pub(super) fn tuple_elements(&self, atom: TypeId) -> Option<Vec<TypeId>> {
        match self.decls.universe.atom_decl(atom).ok()?.kind {
            AtomKind::Tuple(ref elements) => Some(elements.clone()),
            _ => None,
        }
    }

    pub(super) fn named_atom_ty(&mut self, span: Span, name: &str, what: &str) -> Option<Ty> {
        let ty = self
            .decls
            .universe
            .atom_by_name(name)
            .and_then(|atom| self.decls.universe.atom_ty(atom).ok());
        if ty.is_none() {
            self.error(
                ErrorCode::InferenceFailed,
                format!("`{what}` needs a declared `{name}` type [B1]"),
                span,
                "no such type",
            );
        }
        ty
    }

    pub(super) fn is_string_ty(&self, ty: Ty) -> bool {
        // By name through the atom table: `atom_ty` interns and needs
        // exclusivity this shared-context check does not have.
        match self.decls.universe.members(ty) {
            [only] => self
                .decls
                .universe
                .atom_decl(*only)
                .is_ok_and(|decl| decl.name == "string"),
            _ => false,
        }
    }

    pub(super) fn none_ty(&mut self) -> Ty {
        self.decls
            .universe
            .atom_ty(TypeId::NONE)
            .unwrap_or_else(|_| self.empty())
    }

    pub(super) fn bool_ty(&mut self) -> Ty {
        self.decls
            .universe
            .atom_by_name("bool")
            .and_then(|atom| self.decls.universe.atom_ty(atom).ok())
            .unwrap_or_else(|| self.empty())
    }

    pub(super) fn unit(&mut self) -> Ty {
        self.decls.universe.unit_ty()
    }

    pub(super) fn empty(&mut self) -> Ty {
        self.decls.universe.empty_ty()
    }

    /// The empty-typed value of an errored expression.
    pub(super) fn empty_val(&mut self) -> Val {
        Val {
            ty: self.empty(),
            diverges: false,
            poisoned: false,
        }
    }

    pub(super) fn fn_atom_of(&mut self, sig: &FnSig) -> Val {
        let mut param_tys = Vec::with_capacity(sig.params.len());
        for param in &sig.params {
            param_tys.push(
                param
                    .ty
                    .as_ref()
                    .and_then(|t| self.solve_here(t))
                    .unwrap_or_else(|| self.empty()),
            );
        }
        let ret = sig
            .ret
            .as_ref()
            .and_then(|r| self.solve_here(r))
            .unwrap_or_else(|| self.unit());
        let params_boxed: Box<[Ty]> = param_tys.into();
        let ty = self
            .decls
            .universe
            .fn_atom(&params_boxed, ret)
            .ok()
            .and_then(|atom| self.decls.universe.atom_ty(atom).ok())
            .unwrap_or_else(|| self.empty());
        Val {
            ty,
            diverges: false,
            poisoned: false,
        }
    }

    pub(super) fn span_text(&self, span: Span) -> String {
        self.files
            .source(span.file)
            .get(span.start as usize..span.end as usize)
            .unwrap_or("")
            .to_owned()
    }
}
