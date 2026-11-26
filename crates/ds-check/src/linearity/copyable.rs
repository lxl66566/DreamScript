//! # Copyability
//!
//! `~Copyable` subtracts the default copyability constraint, so everything
//! copies by default. A type is non-copyable when any of its member atoms
//! is: a declaration carrying the `~Copyable` marker among its supers, a
//! struct or enum whose (solved) field or payload types are non-copyable,
//! a generic application of a non-copyable argument (a container of linear
//! values is linear), a tuple of a non-copyable element, or a minted type
//! variable whose generic parameter bound is the marker (`[t: ~Copyable]`).
//! The bidirectional walk records each local binder's copyability at
//! binding time ([`BinderFact`], in scope-sensitive positions), so this
//! pass consumes recorded facts instead of re-solving types.

use super::*;

// ---- copyability ------------------------------------------------------------

/// Whether a solved type copies freely: every member atom does.
pub(crate) fn ty_copyable(decls: &Decls, ty: Ty, linear_vars: &FxHashMap<TypeId, bool>) -> bool {
    ty_copyable_seen(decls, ty, linear_vars, &mut Vec::new())
}

fn ty_copyable_seen(
    decls: &Decls,
    ty: Ty,
    linear_vars: &FxHashMap<TypeId, bool>,
    seen: &mut Vec<TypeId>,
) -> bool {
    decls
        .universe
        .members(ty)
        .iter()
        .all(|atom| atom_copyable(decls, *atom, linear_vars, seen))
}

fn atom_copyable(
    decls: &Decls,
    atom: TypeId,
    linear_vars: &FxHashMap<TypeId, bool>,
    seen: &mut Vec<TypeId>,
) -> bool {
    // A cyclic type is illegal in v0; stopping as copyable on a
    // revisit keeps the recursion finite without deciding legality here.
    if seen.contains(&atom) {
        return true;
    }
    seen.push(atom);
    let Ok(decl) = decls.universe.atom_decl(atom) else {
        return true;
    };
    // A minted type variable: the generic parameter's bound decides.
    if decl.name.starts_with('$') {
        return linear_vars.get(&atom).copied().unwrap_or(true);
    }
    if decls
        .defs_of_atoms
        .get(&atom)
        .is_some_and(|def| struct_linear(decls, *def))
    {
        return false;
    }
    match &decl.kind {
        AtomKind::Unit | AtomKind::Reserved | AtomKind::FnSig(..) => true,
        AtomKind::Struct(fields) => fields
            .iter()
            .all(|f| field_copyable(decls, f, linear_vars, seen)),
        AtomKind::Enum(cases) => cases.iter().all(|c| {
            c.fields
                .iter()
                .all(|f| field_copyable(decls, f, linear_vars, seen))
        }),
        AtomKind::Tuple(elements) => elements
            .iter()
            .all(|e| atom_copyable(decls, *e, linear_vars, seen)),
        // A container of linear values is linear: the application copies
        // freely only when the constructor and every argument do.
        AtomKind::Apply(ctor, args) => {
            args.iter()
                .all(|a| ty_copyable_seen(decls, *a, linear_vars, seen))
                && !decls
                    .defs_of_atoms
                    .get(ctor)
                    .is_some_and(|def| struct_linear(decls, *def))
        },
    }
}

/// The shared empty bound table for copyability probes outside generic
/// scopes (field projections): one static map, never a fresh table per
/// call.
pub(super) fn no_linear_vars() -> &'static FxHashMap<TypeId, bool> {
    static NONE: std::sync::LazyLock<FxHashMap<TypeId, bool>> =
        std::sync::LazyLock::new(FxHashMap::default);
    &NONE
}

fn field_copyable(
    decls: &Decls,
    field: &Field,
    linear_vars: &FxHashMap<TypeId, bool>,
    seen: &mut Vec<TypeId>,
) -> bool {
    expr_copyable(decls, &field.ty, linear_vars, seen)
}

pub(super) fn expr_copyable(
    decls: &Decls,
    expr: &TypeExpr,
    linear_vars: &FxHashMap<TypeId, bool>,
    seen: &mut Vec<TypeId>,
) -> bool {
    match expr {
        TypeExpr::Atom(atom) => atom_copyable(decls, *atom, linear_vars, seen),
        // A trait used as a type copies when every member atom does.
        TypeExpr::Trait(id) => decls.universe.trait_decl(*id).map_or(true, |decl| {
            decl.members
                .iter()
                .all(|atom| atom_copyable(decls, *atom, linear_vars, seen))
        }),
        // A negated constraint in field position subtracts copyability.
        TypeExpr::Not(_) => false,
        TypeExpr::Union(a, b) | TypeExpr::Inter(a, b) | TypeExpr::Diff(a, b) => {
            expr_copyable(decls, a, linear_vars, seen) && expr_copyable(decls, b, linear_vars, seen)
        },
        TypeExpr::Tuple(elements) => elements
            .iter()
            .all(|e| expr_copyable(decls, e, linear_vars, seen)),
        // No inhabitant ever moves, so the empty set copies vacuously.
        TypeExpr::Empty => true,
    }
}

fn struct_linear(decls: &Decls, def: DefId) -> bool {
    decls.structs.get(&def).is_some_and(|s| s.linear)
        || decls.enums.get(&def).is_some_and(|e| e.linear)
}
