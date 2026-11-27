//! Pattern typing: coverage as atom sets for exhaustive matching and
//! arm remainder typing, binder collection through the same walk, and the
//! case/enum pattern tables.

use super::*;

impl Bodies<'_> {
    /// One match-arm pattern against a scrutinee type: the atoms it
    /// covers plus its binder tuples `(name, span, ty)` from the same
    /// walk.
    pub(crate) fn pattern_cover(
        &mut self,
        pat: &Pattern,
        scrut: Ty,
        ann: Option<&Type>,
    ) -> (Vec<TypeId>, Vec<(String, Span, Ty)>) {
        let mut covered = Vec::new();
        let mut binders = Vec::new();
        self.pattern_walk(pat, scrut, ann, &mut covered, &mut binders, 0, true);
        (covered, binders)
    }

    /// Binds one declaration pattern's binders into the current scope.
    pub(crate) fn bind_pattern(&mut self, pat: &Pattern, scrut: Ty, mutable: bool) {
        let mut binders = Vec::new();
        self.pattern_walk(pat, scrut, None, &mut Vec::new(), &mut binders, 0, false);
        for (_, span, ty) in binders {
            self.bind_local(span, Binding { ty, mutable });
        }
    }

    /// The (name, def, ty) triples a top-level binding pattern declares.
    pub(crate) fn pattern_defs(
        &mut self,
        pat: &Pattern,
        scrut: Ty,
    ) -> Vec<(String, Option<DefId>, Ty)> {
        let mut binders = Vec::new();
        self.pattern_walk(pat, scrut, None, &mut Vec::new(), &mut binders, 0, false);
        binders
            .into_iter()
            .map(|(name, _, ty)| {
                let def = self.package.lookup_value(&name);
                (name, def, ty)
            })
            .collect()
    }

    /// One pattern walk: coverage accumulates into `covered`, binder
    /// tuples `(name, span, ty)` into `binders`. `depth` guards against
    /// pathological or-pattern nesting. `arm` marks a test position
    /// (match arms); declarations keep every identifier a binding.
    pub(crate) fn pattern_walk(
        &mut self,
        pat: &Pattern,
        scrut: Ty,
        ann: Option<&Type>,
        covered: &mut Vec<TypeId>,
        binders: &mut Vec<(String, Span, Ty)>,
        depth: usize,
        arm: bool,
    ) {
        if depth > 64 {
            return;
        }
        let members = self.decls.universe.members(scrut).to_vec();
        match pat {
            Pattern::Wildcard(_) => covered.extend(members),
            Pattern::Binding { name, .. } => {
                self.pattern_binding(name, scrut, ann, &members, covered, binders, arm);
            },
            Pattern::Literal { span, lit } => {
                let name = match lit {
                    Literal::Int { suffix, .. } | Literal::Float { suffix, .. }
                        if !suffix.is_empty() =>
                    {
                        suffix.clone()
                    },
                    Literal::Int { .. } => "int".to_owned(),
                    Literal::Float { .. } => "float".to_owned(),
                    Literal::Bool(_) => "bool".to_owned(),
                    Literal::Char(_) => "char".to_owned(),
                    Literal::Byte(_) => "byte".to_owned(),
                };
                self.literal_pattern(*span, &name, &members, scrut);
            },
            Pattern::Str { span, .. } | Pattern::RawStr { span, .. } => {
                self.literal_pattern(*span, "string", &members, scrut);
            },
            Pattern::Path { span, path } => match self.pattern_path_atom(path, scrut) {
                Some(atom) => self.case_cover(*span, atom, &members, scrut, covered),
                None if self.take_pattern_label_reported() => {},
                None => self.error(
                    ErrorCode::UnresolvedName,
                    format!("pattern `{}` names no declared atom", path.text()),
                    *span,
                    "unknown pattern",
                ),
            },
            Pattern::Call { span, path, args } => {
                let Some(atom) = self.pattern_path_atom(path, scrut) else {
                    if !self.take_pattern_label_reported() {
                        self.error(
                            ErrorCode::UnresolvedName,
                            format!("pattern `{}` names no declared atom", path.text()),
                            *span,
                            "unknown pattern",
                        );
                    }
                    return;
                };
                let payload = self.payload_tys(path, atom);
                match payload {
                    Some(fields) => {
                        if fields.len() != args.len() {
                            self.diags.push(
                                Diagnostic::error(
                                    ErrorCode::ArityMismatch,
                                    format!(
                                        "pattern `{}` takes {} payload(s), {} given",
                                        path.text(),
                                        fields.len(),
                                        args.len()
                                    ),
                                )
                                .with_primary(*span, "wrong arity"),
                            );
                        }
                        for (sub, field_ty) in args.iter().zip(fields.iter()) {
                            self.pattern_walk(
                                sub,
                                *field_ty,
                                None,
                                covered,
                                binders,
                                depth + 1,
                                arm,
                            );
                        }
                        self.case_cover(*span, atom, &members, scrut, covered);
                    },
                    None => self.error(
                        ErrorCode::TypeMismatch,
                        format!("`{}` carries no payload", path.text()),
                        *span,
                        "payload on a bare case",
                    ),
                }
            },
            Pattern::Struct { span, path, fields } => {
                let Some(atom) = self.pattern_path_atom(path, scrut) else {
                    self.error(
                        ErrorCode::UnresolvedName,
                        format!("pattern `{}` names no declared atom", path.text()),
                        *span,
                        "unknown pattern",
                    );
                    return;
                };
                let Some(shape) = self.struct_shape_of(atom).cloned() else {
                    self.error(
                        ErrorCode::TypeMismatch,
                        format!("pattern `{}` does not destructure a struct", path.text()),
                        *span,
                        "not a struct pattern",
                    );
                    return;
                };
                // Substitute the constructor's parameters from the
                // scrutinee application, like field access: the member
                // the head names — its own application when the head is
                // the member itself, else the applied member the head's
                // constructor application matches.
                let applied = self
                    .applied_struct_member(atom, scrut, *span)
                    .and_then(|member| self.single_apply_of(member).map(|(_, args)| args))
                    .or_else(|| {
                        members
                            .first()
                            .and_then(|m| self.single_apply_of(*m))
                            .and_then(|(ctor, args)| (ctor == atom).then_some(args))
                    });
                let mut subst = FxHashMap::default();
                if let Some(args) = applied {
                    for (param, arg) in shape.params.iter().zip(args.iter()) {
                        subst.insert(param.name.clone(), *arg);
                    }
                }
                self.type_vars.push(subst);
                for (name, sub) in fields {
                    let Some(field) = shape.fields.iter().find(|f| f.name.name == name.name) else {
                        self.error(
                            ErrorCode::TypeMismatch,
                            format!("`{}` has no field `{}`", path.text(), name.name),
                            name.span,
                            "no such field",
                        );
                        continue;
                    };
                    let field_ty = self.solve_here(&field.ty);
                    match sub {
                        Some(sub) => {
                            let scrut_field = field_ty.unwrap_or_else(|| self.empty());
                            // the field's own sub-pattern proves nothing
                            // about the scrutinee's members: its types are
                            // the field's, and one sharing an atom with a
                            // sibling member would cover the member the
                            // arm never named. The member's cover is this
                            // arm's own case decision, taken below.
                            let mut field_covered = Vec::new();
                            self.pattern_walk(
                                sub,
                                scrut_field,
                                None,
                                &mut field_covered,
                                binders,
                                depth + 1,
                                arm,
                            );
                        },
                        None => binders.push((
                            name.name.clone(),
                            name.span,
                            field_ty.unwrap_or_else(|| self.empty()),
                        )),
                    }
                }
                self.type_vars.pop();
                self.case_cover(*span, atom, &members, scrut, covered);
            },
            Pattern::Tuple {
                span,
                pats,
                trailing_comma,
            } => {
                if pats.len() == 1 && !trailing_comma {
                    self.pattern_walk(&pats[0], scrut, ann, covered, binders, depth + 1, arm);
                    return;
                }
                if !arm && members.len() != 1 {
                    // Declaration patterns bind without a coverage
                    // proof: picking the one tuple member out of a union
                    // would bind unproven types, so the scrutinee must
                    // be exactly one tuple. Match arms split unions
                    // instead, with the checker proving coverage.
                    self.error(
                        ErrorCode::TypeMismatch,
                        format!(
                            "a destructuring declaration over `{}` needs one tuple of {} \
                             element(s); narrow the union with a match first [B3, F4]",
                            self.decls.universe.render(scrut),
                            pats.len()
                        ),
                        *span,
                        "not one tuple type",
                    );
                    return;
                }
                let mut matched = Vec::new();
                for member in &members {
                    if let Some(elements) = self.tuple_elements(*member)
                        && elements.len() == pats.len()
                    {
                        matched.push(*member);
                        for (sub, element) in pats.iter().zip(elements.iter()) {
                            let element_ty = self
                                .decls
                                .universe
                                .atom_ty(*element)
                                .unwrap_or_else(|_| self.empty());
                            self.pattern_walk(
                                sub,
                                element_ty,
                                None,
                                covered,
                                binders,
                                depth + 1,
                                arm,
                            );
                        }
                    }
                }
                if matched.is_empty() && !members.is_empty() {
                    self.error(
                        ErrorCode::TypeMismatch,
                        format!(
                            "tuple pattern of {} element(s) matches no member of `{}` [B3]",
                            pats.len(),
                            self.decls.universe.render(scrut)
                        ),
                        *span,
                        "arity mismatch",
                    );
                }
                covered.extend(matched);
            },
            Pattern::Or { span, pats } => {
                let mut alternative: Option<Vec<String>> = None;
                for sub in pats {
                    let mut sub_covered = Vec::new();
                    let mut sub_binders: Vec<(String, Span, Ty)> = Vec::new();
                    self.pattern_walk(
                        sub,
                        scrut,
                        None,
                        &mut sub_covered,
                        &mut sub_binders,
                        depth + 1,
                        arm,
                    );
                    let names: Vec<String> = sub_binders.iter().map(|(n, ..)| n.clone()).collect();
                    match &alternative {
                        None => alternative = Some(names),
                        Some(expected) if expected != &names => {
                            self.error(
                                ErrorCode::TypeMismatch,
                                "or-pattern alternatives bind different names [F4]".to_owned(),
                                *span,
                                "inconsistent alternatives",
                            );
                        },
                        _ => {},
                    }
                    binders.extend(sub_binders);
                    covered.extend(sub_covered);
                }
            },
        }
    }

    /// The bare-identifier arm of pattern_walk: a member test by name in
    /// an arm position, a whole-scrutinee binder otherwise.
    fn pattern_binding(
        &mut self,
        name: &Ident,
        scrut: Ty,
        ann: Option<&Type>,
        members: &[TypeId],
        covered: &mut Vec<TypeId>,
        binders: &mut Vec<(String, Span, Ty)>,
        arm: bool,
    ) {
        // A bare identifier in a test position names a member test
        // when the type namespace carries it: bare tags are
        // constructor values, so the type entry wins over any
        // same-named local and nothing binds.
        //
        // The remaining scrutinee directs the label first: one
        // expected member carrying the label is the atom, an expected
        // union without it refuses unknown-member when the label is a
        // declaration of this compile, and several refuse ambiguous.
        if arm
            && let Some(()) =
                self.directed_pattern_label(&name.name, name.span, members, scrut, covered)
        {
            return;
        }
        if arm && let Some(atom) = self.unit_pattern_atom(&name.name) {
            // the applied-head escape keeps the member reading: a
            // unit case over an applied generic enum head tests over
            // every application, though the head is opaque to the label
            // probe
            let is_member = self
                .decls
                .universe
                .atom_ty(atom)
                .is_ok_and(|ty| self.decls.universe.contains(scrut, ty))
                || self.case_of_applied_head(atom, scrut);
            if !is_member {
                self.error(
                    ErrorCode::UnknownTagMember,
                    format!(
                        "a bare `{}` names no member of `{}` [F9.5]",
                        name.name,
                        self.decls.universe.render(scrut)
                    ),
                    name.span,
                    "unknown member",
                );
                return;
            }
            self.case_cover(name.span, atom, members, scrut, covered);
            return;
        }
        let ty = match ann {
            Some(ann) => match self.solve_here(ann) {
                Some(solved) => {
                    let refined = self.decls.universe.inter(scrut, solved);
                    if self.decls.universe.is_empty(refined) {
                        self.error(
                            ErrorCode::TypeMismatch,
                            format!(
                                "binding annotation `{}` intersects `{}` emptily",
                                decls::render_type(ann),
                                self.decls.universe.render(scrut)
                            ),
                            name.span,
                            "empty intersection",
                        );
                    }
                    // An annotated binding is a narrowing filter:
                    // it covers only its intersection, the way a `.err`
                    // handler arm splits the union.
                    covered.extend_from_slice(self.decls.universe.members(refined));
                    refined
                },
                None => scrut,
            },
            None => scrut,
        };
        binders.push((name.name.clone(), name.span, ty));
        if ann.is_none() {
            covered.extend(members);
        }
    }

    /// Literal patterns narrow nothing and prove no coverage (value-level);
    /// they must still name a declared member atom of the scrutinee.
    fn literal_pattern(&mut self, span: Span, name: &str, members: &[TypeId], scrut: Ty) {
        let atom_ty = self
            .decls
            .universe
            .atom_by_name(name)
            .and_then(|a| self.decls.universe.atom_ty(a).ok());
        match atom_ty {
            Some(atom_ty) => {
                if !members.is_empty() && !self.decls.universe.contains(scrut, atom_ty) {
                    self.error(
                        ErrorCode::TypeMismatch,
                        format!(
                            "a `{name}` literal cannot match `{}`",
                            self.decls.universe.render(scrut)
                        ),
                        span,
                        "not a member",
                    );
                }
            },
            None => self.error(
                ErrorCode::InferenceFailed,
                format!("literal patterns need a declared `{name}` type"),
                span,
                "no such type",
            ),
        }
    }

    /// One case-pattern's coverage: the named atom must be a scrutinee
    /// member; enum cases cover their whole enum atom (case-level
    /// exhaustiveness is value-level, out of the set core's reach). A
    /// unit case over an applied enum head is a member too — the head is
    /// opaque to the set algebra, so the membership reads the head's
    /// minted case atoms — and a user-declared generic struct's ctor
    /// head names the applied member of the scrutinee it matches, the
    /// coverage reading the member the head spells.
    fn case_cover(
        &mut self,
        span: Span,
        head: TypeId,
        members: &[TypeId],
        scrut: Ty,
        covered: &mut Vec<TypeId>,
    ) {
        let atom = self
            .applied_struct_member(head, scrut, span)
            .unwrap_or(head);
        let is_member = self
            .decls
            .universe
            .atom_ty(atom)
            .is_ok_and(|ty| self.decls.universe.contains(scrut, ty))
            || self.case_of_applied_head(atom, scrut);
        if is_member {
            covered.push(atom);
        } else {
            let _ = members;
            self.error(
                ErrorCode::TypeMismatch,
                format!(
                    "pattern `{}` is not a member of `{}`",
                    self.atom_name(atom),
                    self.decls.universe.render(scrut)
                ),
                span,
                "not a member",
            );
        }
    }

    /// The applied member a user-declared generic struct's ctor head
    /// names in a scrutinee: the head's own atom keys no application,
    /// so the membership reads the member whose atom is an application
    /// of it. One application answers the member, several report the
    /// ambiguity (the head names two members), and none answers `None`
    /// for the caller's own membership error.
    fn applied_struct_member(&mut self, head: TypeId, scrut: Ty, span: Span) -> Option<TypeId> {
        let def = self.decls.defs_of_atoms.get(&head).copied()?;
        let shape = self.decls.structs.get(&def)?;
        if shape.params.is_empty() {
            return None;
        }
        let matches: Vec<TypeId> = self
            .decls
            .universe
            .members(scrut)
            .iter()
            .copied()
            .filter(|&member| {
                matches!(
                    self.decls.universe.atom_decl(member).ok().map(|d| &d.kind),
                    Some(AtomKind::Apply(ctor, _)) if *ctor == head
                )
            })
            .collect();
        match matches.as_slice() {
            [one] => Some(*one),
            [] => None,
            many => {
                let spelled: Vec<String> = many.iter().map(|&m| self.atom_name(m)).collect();
                self.error(
                    ErrorCode::AmbiguousTagMember,
                    format!(
                        "the head `{}` matches several members of `{}`: {}; name the head's \
                         application",
                        self.atom_name(head),
                        self.decls.universe.render(scrut),
                        spelled.join(", ")
                    ),
                    span,
                    "ambiguous member",
                );
                None
            },
        }
    }

    /// Whether the case atom belongs to the scrutinee's applied enum
    /// head: a unit case's atom carries no substitution, so it tests over
    /// every application of its enum. Applied heads stay opaque to the
    /// set algebra, so the membership reads the head's case atoms.
    fn case_of_applied_head(&mut self, atom: TypeId, scrut: Ty) -> bool {
        matches!(self.applied_enum_head(scrut), Some((_, atoms, _)) if atoms.contains(&atom))
    }

    /// The per-application case atom one enum case def names over a
    /// scrutinee: the head's own applications in the scrutinee pick
    /// the one the case's values carry — a bare case keeps its shared
    /// unit atom, a payloaded case the application's minted atom —
    /// answering `(application, case atom)`. One application answers,
    /// several refuse ambiguous (the case names a different shape per
    /// application), and none answers `None` for the caller's own
    /// resolution error. The lowerer reads the atom through the
    /// span-keyed directed-tag record, so both layers share one
    /// mapping.
    pub(super) fn applied_case_atom(
        &mut self,
        case_def: DefId,
        scrut: Ty,
        span: Span,
    ) -> Option<(TypeId, TypeId)> {
        let enum_def = self.decls.cases.get(&case_def).map(|case| case.enum_def)?;
        let shape = self.decls.enums.get(&enum_def)?;
        if shape.params.is_empty() {
            return None;
        }
        let head_atom = self.decls.atoms.get(&enum_def).copied()?;
        let apps = decls::applications_of_head(self.decls, scrut, head_atom);
        let app = match apps.as_slice() {
            [one] => *one,
            [] => return None,
            many => {
                let spelled: Vec<String> = many.iter().map(|&m| self.atom_name(m)).collect();
                self.error(
                    ErrorCode::AmbiguousTagMember,
                    format!(
                        "the case matches several applications of the head: {}; name the \
                         application",
                        spelled.join(", ")
                    ),
                    span,
                    "ambiguous application",
                );
                return None;
            },
        };
        let cases = decls::applied_enum_cases(self.package, self.decls, app)?;
        let case = cases.iter().find(|case| case.def == case_def)?;
        self.record_directed_tag(span, case.atom);
        Some((app, case.atom))
    }

    fn pattern_path_atom(&mut self, path: &Path, scrut: Ty) -> Option<TypeId> {
        match path.segments.as_slice() {
            [only] => self.unit_pattern_atom(&only.name).or_else(|| {
                let def = self.type_def_of(&only.name)?;
                self.applied_case_atom(def, scrut, only.span)
                    .map(|(_, atom)| atom)
            }),
            [head, tail] => {
                let def = self
                    .type_def_of(&head.name)
                    .or_else(|| self.decls.foreign_type(&head.name))?;
                if self.package.def(def).kind == DefKind::Enum {
                    // A qualified case path tests the CASE's own minted
                    // atom, never the enum's name. A linked enum and
                    // its cases answer through the name index the linked
                    // install filled; the cases are the enum's own written
                    // members, so they resolve under its declaring file.
                    let shape = self.decls.enums.get(&def)?;
                    let (case_name, ..) =
                        shape.cases.iter().find(|(name, ..)| name == &tail.name)?;
                    let enum_file = self.package.def(def).span.file;
                    let case_def = self
                        .package
                        .lookup_type_in(enum_file, case_name)
                        .or_else(|| self.decls.foreign_type(case_name))?;
                    let atom = match self.decls.atoms.get(&case_def).copied() {
                        Some(atom) => {
                            self.record_directed_tag(tail.span, atom);
                            atom
                        },
                        None => self
                            .applied_case_atom(case_def, scrut, tail.span)
                            .map(|(_, atom)| atom)?,
                    };
                    return Some(atom);
                }
                if matches!(
                    self.package.def(def).kind,
                    DefKind::Tag | DefKind::TypeAlias
                ) {
                    return self.qualified_pattern_label(head, tail);
                }
                None
            },
            _ => None,
        }
    }

    /// The qualified `type::label` in a pattern: the head's own
    /// members carrying the label pick the atom, recorded at the
    /// label's span for the lowering. None reports unknown-member
    /// naming the head and several the ambiguity naming both
    /// declarations; the latch keeps the caller's unknown-pattern word
    /// from doubling the report.
    fn qualified_pattern_label(&mut self, head: &Ident, tail: &Ident) -> Option<TypeId> {
        let head_ty = self.head_type_of(head)?;
        let labeled = self.members_labeled(head_ty, &tail.name);
        match labeled.as_slice() {
            [one] => {
                self.record_directed_tag(tail.span, *one);
                Some(*one)
            },
            [] => {
                self.error(
                    ErrorCode::UnknownTagMember,
                    format!(
                        "the qualified `{}::{}` names no member of `{}` [F9.5]",
                        head.name,
                        tail.name,
                        self.decls.universe.render(head_ty)
                    ),
                    tail.span,
                    "unknown member",
                );
                self.pattern_label_reported = true;
                None
            },
            many => {
                let mut diag = Diagnostic::error(
                    ErrorCode::AmbiguousTagMember,
                    format!(
                        "the qualified `{}::{}` matches several members of `{}` [F9.5]; name a \
                         head that carries one",
                        head.name,
                        tail.name,
                        self.decls.universe.render(head_ty)
                    ),
                )
                .with_primary(tail.span, "ambiguous member");
                for atom in many {
                    if let Some(decl) = self.decls.defs_of_atoms.get(atom) {
                        diag = diag.with_secondary(
                            self.package.def(*decl).span,
                            "one declaration of the label",
                        );
                    }
                }
                self.diags.push(diag);
                self.pattern_label_reported = true;
                None
            },
        }
    }

    /// Whether one arm's pattern carries value-level tests — a literal
    /// or string anywhere in the pattern, at any depth through tuples,
    /// struct fields, case payloads, and or-alternatives. Such an arm
    /// can fail at run, so the remainder walk keeps the full scrutinee
    /// for later arms.
    pub(crate) fn carries_value_tests(pat: &Pattern) -> bool {
        match pat {
            Pattern::Literal { .. } | Pattern::Str { .. } | Pattern::RawStr { .. } => true,
            Pattern::Tuple { pats, .. } | Pattern::Or { pats, .. } => {
                pats.iter().any(Self::carries_value_tests)
            },
            Pattern::Struct { fields, .. } => fields
                .iter()
                .any(|(_, sub)| sub.as_ref().is_some_and(Self::carries_value_tests)),
            Pattern::Call { args, .. } => args.iter().any(Self::carries_value_tests),
            Pattern::Wildcard(_) | Pattern::Binding { .. } | Pattern::Path { .. } => false,
        }
    }

    /// The member atom a bare identifier names in a pattern: the
    /// predeclared `none`, or a type-namespace constructor's atom —
    /// aliases and traits carry none, so those names bind instead. The
    /// declaring file's own shadowing tag declaration answers first.
    fn unit_pattern_atom(&self, name: &str) -> Option<TypeId> {
        if name == "none" && !self.package.declares("none") {
            return Some(TypeId::NONE);
        }
        let def = self
            .type_def_of(name)
            .or_else(|| self.decls.foreign_type(name))?;
        self.decls.atoms.get(&def).copied()
    }

    /// The directed pattern label: the remaining scrutinee's
    /// members carrying the bare label pick the atom. One member wins —
    /// even when the label names no declaration of this compile; none
    /// or several report and leave the arm uncovered.
    fn directed_pattern_label(
        &mut self,
        name: &str,
        span: Span,
        members: &[TypeId],
        scrut: Ty,
        covered: &mut Vec<TypeId>,
    ) -> Option<()> {
        let labeled: Vec<TypeId> = members
            .iter()
            .copied()
            .filter(|&atom| {
                self.decls
                    .universe
                    .atom_decl(atom)
                    .is_ok_and(|decl| decl.name == name)
            })
            .collect();
        match labeled.as_slice() {
            [one] => {
                self.record_directed_tag(span, *one);
                self.case_cover(span, *one, members, scrut, covered);
                Some(())
            },
            [] => None,
            many => {
                let mut diag = Diagnostic::error(
                    ErrorCode::AmbiguousTagMember,
                    format!(
                        "the pattern label `{}` matches several members of `{}` [F9.5]; qualify \
                         which declaration you mean",
                        name,
                        self.decls.universe.render(scrut)
                    ),
                )
                .with_primary(span, "ambiguous member");
                for atom in many {
                    if let Some(decl) = self.decls.defs_of_atoms.get(atom) {
                        diag = diag.with_secondary(
                            self.package.def(*decl).span,
                            "one declaration of the label",
                        );
                    }
                }
                self.diags.push(diag);
                Some(())
            },
        }
    }
}
