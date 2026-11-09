//! Pattern typing [F4]: coverage as atom sets for exhaustive matching and
//! arm remainder typing, binder collection through the same walk, and the
//! case/enum pattern tables.

use super::*;

impl Bodies<'_> {
    /// One pattern against a scrutinee type: the atoms it covers.
    pub(crate) fn pattern_cover(
        &mut self,
        pat: &Pattern,
        scrut: Ty,
        ann: Option<&Type>,
    ) -> Vec<TypeId> {
        let mut covered = Vec::new();
        self.pattern_walk(pat, scrut, ann, &mut covered, &mut Vec::new(), 0);
        covered
    }

    /// Binds one pattern's binders into the current scope.
    pub(crate) fn bind_pattern(&mut self, pat: &Pattern, scrut: Ty, mutable: bool) {
        let mut binders = Vec::new();
        self.pattern_walk(pat, scrut, None, &mut Vec::new(), &mut binders, 0);
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
        self.pattern_walk(pat, scrut, None, &mut Vec::new(), &mut binders, 0);
        binders
            .into_iter()
            .map(|(name, _, ty)| {
                let def = self.package.lookup(&name);
                (name, def, ty)
            })
            .collect()
    }

    pub(crate) fn pattern_def(&self, local: &LocalDecl) -> Option<DefId> {
        match &local.pat {
            Pattern::Binding { name, .. } => self.package.lookup(&name.name),
            _ => None,
        }
    }

    /// One pattern walk: coverage accumulates into `covered`, binder
    /// tuples `(name, span, ty)` into `binders`. `depth` guards against
    /// pathological or-pattern nesting.
    pub(crate) fn pattern_walk(
        &mut self,
        pat: &Pattern,
        scrut: Ty,
        ann: Option<&Type>,
        covered: &mut Vec<TypeId>,
        binders: &mut Vec<(String, Span, Ty)>,
        depth: usize,
    ) {
        if depth > 64 {
            return;
        }
        let members = self.decls.universe.members(scrut).to_vec();
        match pat {
            Pattern::Wildcard(_) => covered.extend(members),
            Pattern::Binding { name, .. } => {
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
                            // it covers only its intersection, the way the
                            // record's `.err` handler arms split the union
                            // [E4, F4].
                            covered.extend(self.decls.universe.members(refined).to_vec());
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
            Pattern::Path { span, path } => match self.pattern_path_atom(path) {
                Some(atom) => self.case_cover(*span, atom, &members, scrut, covered),
                None => self.error(
                    ErrorCode::UnresolvedName,
                    format!("pattern `{}` names no declared atom", path.text()),
                    *span,
                    "unknown pattern",
                ),
            },
            Pattern::Call { span, path, args } => {
                let Some(atom) = self.pattern_path_atom(path) else {
                    self.error(
                        ErrorCode::UnresolvedName,
                        format!("pattern `{}` names no declared atom", path.text()),
                        *span,
                        "unknown pattern",
                    );
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
                            self.pattern_walk(sub, *field_ty, None, covered, binders, depth + 1);
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
                let Some(atom) = self.pattern_path_atom(path) else {
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
                // scrutinee application, like field access.
                let mut subst = HashMap::new();
                if let Some((ctor, args)) = members.first().and_then(|m| self.single_apply_of(*m)) {
                    if ctor == atom {
                        for (param, arg) in shape.params.iter().zip(args.iter()) {
                            subst.insert(param.name.clone(), *arg);
                        }
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
                            self.pattern_walk(sub, scrut_field, None, covered, binders, depth + 1);
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
                    self.pattern_walk(&pats[0], scrut, ann, covered, binders, depth + 1);
                    return;
                }
                let mut matched = Vec::new();
                for member in &members {
                    if let Some(elements) = self.tuple_elements(*member) {
                        if elements.len() == pats.len() {
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
                                );
                            }
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
    /// exhaustiveness is value-level, out of the set core's reach).
    fn case_cover(
        &mut self,
        span: Span,
        atom: TypeId,
        members: &[TypeId],
        scrut: Ty,
        covered: &mut Vec<TypeId>,
    ) {
        let is_member = self
            .decls
            .universe
            .atom_ty(atom)
            .is_ok_and(|ty| self.decls.universe.contains(scrut, ty));
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

    fn pattern_path_atom(&mut self, path: &Path) -> Option<TypeId> {
        match path.segments.as_slice() {
            [only] => {
                if only.name == "none" && self.package.lookup("none").is_none() {
                    return Some(TypeId::NONE);
                }
                let def = self.package.lookup(&only.name)?;
                self.decls.atoms.get(&def).copied()
            },
            [head, tail] => {
                let def = self.package.lookup(&head.name)?;
                let shape = self.decls.enums.get(&def)?;
                if !shape.cases.iter().any(|(name, ..)| name == &tail.name) {
                    return None;
                }
                self.decls.atoms.get(&def).copied()
            },
            _ => None,
        }
    }
}
