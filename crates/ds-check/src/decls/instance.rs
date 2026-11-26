//! Instance declaration: one impl item's head mints its instance row — the
//! generic head as variables, the declared bounds as conditions — after the
//! closure refuses an overlap with the instances already declared.

use super::*;

/// Records one generic inherent impl's solved head pattern and declared
/// bounds: minted variables, the solved head, and one condition per
/// declared bound — the same shapes the trait arm builds, so the two impl
/// forms read through the same machinery downstream. A bounds-free generic
/// impl records its head with no conditions: the head pattern is what a
/// call site's signature substitution and the type-keyed surface's
/// generic-impl refusal read, and an empty bound list stages nothing. A
/// head the type core cannot solve records nothing (the impl's own body
/// checking reports the target).
pub(super) fn declare_inherent_bounds(
    package: &Package,
    item: &Item,
    imp: &ImplDecl,
    decls: &mut Decls,
) {
    if imp.generics.is_empty() {
        return;
    }
    let mut vars = FxHashMap::default();
    for param in &imp.generics {
        // the same minting rule as the trait arm: a bare `$name` would
        // collide across impls (the universe is package-wide), so minted
        // variables carry the shared counter
        decls.var_counter += 1;
        let unique = format!("${}#{}", param.name.name, decls.var_counter);
        if let Ok(var) = decls.universe.declare_tag(&unique)
            && let Ok(ty) = decls.universe.atom_ty(var)
        {
            vars.insert(param.name.name.clone(), ty);
        }
    }
    let Some(head_expr) = conv_node(package, &imp.target, &vars, decls) else {
        return;
    };
    let Ok(head) = decls.universe.solve(&head_expr) else {
        return;
    };
    let mut conditions = Vec::new();
    for param in &imp.generics {
        let Some(bound) = &param.bound else { continue };
        for bound_trait in bound_traits(package, bound, decls) {
            let Some(&ty) = vars.get(&param.name.name) else {
                continue
            };
            conditions.push(Condition::new(bound_trait, ty));
        }
    }
    decls
        .inherent_impl_bounds
        .insert(item.span, InherentBounds { head, conditions });
}

/// Declares one impl's instances into the environment: mints variable
/// atoms for the generic parameters, converts the head (written unions
/// decompose inside `declare`), attaches trait bounds as conditions, and
/// maps coherence failures onto witness-carrying diagnostics. A failed
/// declaration leaves the environment unchanged; the impl's methods are
/// still body-checked (the checker works from the AST).
pub(super) fn declare_instance(
    package: &Package,
    item: &Item,
    imp: &ImplDecl,
    decls: &mut Decls,
    diags: &mut Vec<Diagnostic>,
) {
    let Some(trait_ref) = &imp.trait_ref else {
        declare_inherent_bounds(package, item, imp, decls);
        return;
    };
    let [trait_only] = trait_ref.path.segments.as_slice() else {
        return
    };
    // The trait reference resolves through the local type namespace
    // first, then the link table — a linked def rides no type_names
    // row. A local impl of a linked trait may shape ahead of the
    // trait's late mint loop, so the missing id forces the mint
    // forward here — the same pull a written field naming a linked
    // type takes
    let Some(owner) = package
        .lookup_type(&trait_only.name)
        .or_else(|| package.foreign_type_def(&trait_only.name))
    else {
        return;
    };
    let trait_id = match decls.traits.get(&owner) {
        Some(&trait_id) => trait_id,
        None if package.foreign().get(&owner).is_some() => {
            let Some(&trait_id) =
                force_linked(package, decls, owner).and_then(|()| decls.traits.get(&owner))
            else {
                return;
            };
            trait_id
        },
        None => return,
    };

    let mut vars = FxHashMap::default();
    for param in &imp.generics {
        // A bare `$name` would collide with every other impl naming its
        // parameter the same (the universe is package-wide), so minted
        // variables carry the shared counter.
        decls.var_counter += 1;
        let unique = format!("${}#{}", param.name.name, decls.var_counter);
        if let Ok(var) = decls.universe.declare_tag(&unique)
            && let Ok(ty) = decls.universe.atom_ty(var)
        {
            vars.insert(param.name.name.clone(), ty);
        }
    }

    let Some(head) = conv_node(package, &imp.target, &vars, decls) else {
        diags.push(
            Diagnostic::error(
                ErrorCode::TypeMismatch,
                format!(
                    "impl head `{}` does not name atoms the type core can check",
                    render_type(&imp.target)
                ),
            )
            .with_primary(imp.target.span(), "unsupported impl head"),
        );
        return;
    };

    let mut conditions = Vec::new();
    for param in &imp.generics {
        let Some(bound) = &param.bound else { continue };
        for bound_trait in bound_traits(package, bound, decls) {
            if let Some(&ty) = vars.get(&param.name.name)
                && let Some(&atom) = decls.universe.members(ty).first()
            {
                conditions.push(Condition::new(bound_trait, TypeExpr::atom(atom)));
            }
        }
    }

    // The trait reference's arguments, solved over the impl's minted
    // variables: the record a generic trait's default body reads to know
    // its own variables' substitution at a site entering through this
    // impl. A non-converting argument records nothing — the body check's
    // own trait-reference diagnostics own that shape, and the lowerer
    // refuses a missing record honestly.
    if !trait_ref.args.is_empty() {
        let mut solved_args = Vec::with_capacity(trait_ref.args.len());
        let mut solved_all = true;
        for arg in &trait_ref.args {
            let Some(solved) = conv_node(package, arg, &vars, decls)
                .and_then(|expr| decls.universe.solve(&expr).ok())
            else {
                solved_all = false;
                break;
            };
            solved_args.push(solved);
        }
        if solved_all {
            decls.trait_impl_args.insert(item.span, solved_args);
        }
    }

    // The impl's name moves the instance out of the default vocabulary:
    // coherence probes and call-site resolution run per slot, so a named
    // impl coexists with an unnamed one over the same head.
    let name = imp.name.as_ref().map(|n| Arc::from(n.name.as_str()));
    let decl = InstanceDecl::new(trait_id, head)
        .with_name(name)
        .with_conditions(conditions);
    match decls.instances.declare(&mut decls.universe, &decl) {
        Ok(ids) => {
            for id in ids {
                decls.instance_impls.insert(id, item.span);
            }
        },
        Err(err) => diags.push(instance_diag(&decls.universe, &err, item, &imp.target)),
    }
}

/// Renders a coherence failure as one diagnostic naming the written head,
/// the intersecting earlier head, and the witness value type; the
/// corpus's golden rejection pins this render.
pub(super) fn instance_diag(
    universe: &Universe,
    err: &crate::Error,
    item: &Item,
    target: &Type,
) -> Diagnostic {
    match err {
        crate::Error::InstanceOverlap {
            head_a, witness, ..
        } => Diagnostic::error(
            ErrorCode::InstanceOverlap,
            format!(
                "instance head `{}` intersects the `{}` instance; witness value type `{}`",
                render_type(target),
                universe.render(*head_a),
                universe.render_witness(witness)
            ),
        )
        .with_primary(target.span(), "overlapping head")
        .with_note(format!(
            "the earlier instance's head covers `{}`; two instances of one trait must be pairwise \
             disjoint [B6]",
            universe.render(*head_a)
        )),
        crate::Error::ConditionCycle { .. } => Diagnostic::error(
            ErrorCode::ConditionCycle,
            "a conditional instance's conditions reach back to itself [B6]",
        )
        .with_primary(item.span, "cyclic instance"),
        crate::Error::EmptyInstanceHead(_) => Diagnostic::error(
            ErrorCode::EmptyInstanceHead,
            "an instance head denotes the empty type [B6]",
        )
        .with_primary(target.span(), "empty head"),
        other => Diagnostic::error(
            ErrorCode::AmbiguousInstance,
            format!("instance declaration rejected: {other}"),
        )
        .with_primary(item.span, "rejected impl"),
    }
}

/// The registered method id of one trait member.
pub(super) fn member_id(package: &Package, trait_def: DefId, name: &str) -> Option<MethodId> {
    package
        .method_candidates(name)
        .find(|&id| package.method(id).owner == MethodOwner::Trait(trait_def))
}

/// One trait member's registered signature and declaring span: the
/// signature an impl member must agree with, and the site an agreement
/// diagnostic points back to.
pub(crate) fn trait_member_sig(
    package: &Package,
    decls: &Decls,
    trait_def: DefId,
    name: &str,
) -> Option<(Arc<FnSig>, Span)> {
    let method = member_id(package, trait_def, name)?;
    let sig = decls.members.get(&method)?.clone();
    Some((sig, package.method(method).span))
}

/// Every name a binding pattern binds, through the shared binder walker.
pub(super) fn pattern_names(pat: &Pattern) -> Vec<String> {
    crate::pattern::pattern_binders(pat)
        .into_iter()
        .map(|(_, name)| name.to_owned())
        .collect()
}

/// The declared name of an item, for package lookup. Locals never route
/// here: they register one entry per pattern binder (see `reserve_local`).
pub(super) fn item_name(item: &Item) -> &str {
    match &item.kind {
        ItemKind::Import(_) | ItemKind::Impl(_) | ItemKind::Local(_) | ItemKind::Platform(_) => "",
        ItemKind::Fn(f) | ItemKind::ExternFn(f) => f.name.name.as_str(),
        ItemKind::Struct(s) => s.name.name.as_str(),
        ItemKind::Enum(e) => e.name.name.as_str(),
        ItemKind::Tag(t) => t.name.name.as_str(),
        ItemKind::TypeAlias(a) => a.name.name.as_str(),
        ItemKind::Trait(t) => t.name.name.as_str(),
    }
}
