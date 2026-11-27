//! The reserve pass: every item's def atom and binder rows mint ahead of the
//! shape pass, and the served prefix's const rows build once per artifact.
//!
//! Coherence runs per declaration, then once more over the closed
//! instance set after the last impl declares: a bound-providing impl
//! landing after two generic impls it intersects can be proven by no
//! declaration-time probe, so the closure pass re-solves the whole
//! environment and reports the first overlap it finds.

use super::*;

/// Whether a def's plain declaration tables arrived pre-built from the
/// checked-ambient artifact: the plain-table arms skip the def, its rows
/// already installed by the serve, while its atoms and traits still
/// mint — the merged walk owns those spaces. A user item duplicating a
/// served name resolves to the served def and skips the plain tables
/// too — the fresh path skips it as well (the duplicate mint fails),
/// so the outcome is the same.
pub(super) fn is_served(decls: &Decls, def: DefId) -> bool {
    def.index() < decls.served_defs
}

/// The package-level coherence pass: one closure run over the instance
/// environment after every impl has declared. A declaration-time
/// probe proves a generic-generic pair only through bounds already
/// inhabited at that moment, so a bound-providing impl declared after
/// two generic impls it intersects hides the pair from every
/// per-declaration probe; re-solving the closed set surfaces it here.
/// First-error-only is the accepted tradeoff — per-declaration reporting
/// stays complete, this pass catches only what declaration order hid.
pub(super) fn package_coherence(decls: &mut Decls, diags: &mut Vec<Diagnostic>) {
    // Over a served prefix this stays the closure pass over the fresh
    // rows alone: the artifact is hash-validated content whose clean
    // generation ran the prefix's own declarations and closure against a
    // byte-identical universe state, and the drift gate pins that byte
    // identity, so re-proving the prefix's own pairs buys nothing. Every
    // pair and cycle a fresh row takes part in still probes here (and
    // the fresh row's own declaration probed it against the installed
    // served rows first).
    let Err(err) = decls.instances.check_all(&mut decls.universe) else {
        return;
    };
    diags.push(match &err {
        crate::Error::InstanceOverlap {
            head_a,
            head_b,
            witness,
            later,
            ..
        } => {
            let diag = Diagnostic::error(
                ErrorCode::InstanceOverlap,
                format!(
                    "instance head `{}` intersects the `{}` instance; witness value type `{}`",
                    decls.universe.render(*head_b),
                    decls.universe.render(*head_a),
                    decls.universe.render_witness(witness)
                ),
            )
            .with_note(format!(
                "the earlier instance's head covers `{}`; two instances of one trait must be \
                 pairwise disjoint [B6]",
                decls.universe.render(*head_a)
            ))
            .with_note(
                "neither declaration could prove the pair; the closure pass over the closed \
                 instance set reports it"
                    .to_owned(),
            );
            match decls.instance_impls.get(later) {
                Some(span) => diag.with_primary(*span, "overlapping instance"),
                None => diag,
            }
        },
        crate::Error::ConditionCycle { cycle } => {
            let diag = Diagnostic::error(
                ErrorCode::ConditionCycle,
                "a conditional instance's conditions reach back to itself [B6]",
            );
            match cycle.first().and_then(|id| decls.instance_impls.get(id)) {
                Some(span) => diag.with_primary(*span, "cyclic instance"),
                None => diag,
            }
        },
        other => Diagnostic::error(
            other.code(),
            format!("instance environment closure rejected: {other}"),
        ),
    });
}

/// The definition an item declares, in its kind's namespace.
pub(super) fn item_def(package: &Package, decls: &Decls, item: &Item) -> Option<DefId> {
    let name = item_name(item);
    if matches!(
        item.kind,
        ItemKind::Struct(_)
            | ItemKind::Enum(_)
            | ItemKind::Tag(_)
            | ItemKind::TypeAlias(_)
            | ItemKind::Trait(_)
            | ItemKind::Platform(_)
    ) {
        type_def_for_use(package, decls, name)
    } else {
        package.lookup_value(name)
    }
}

/// The type def a name carries for the walk's current file: the
/// file's own shadowing tag declaration beats the first declaration per
/// label. The foreign installs carry no file and read the shared
/// namespace alone.
pub(super) fn type_def_for_use(package: &Package, decls: &Decls, name: &str) -> Option<DefId> {
    match decls.current_file {
        Some(file) => package.lookup_type_in(file, name),
        None => package.lookup_type(name),
    }
}

/// The case def a case label carries for the walk's current file: the
/// case namespace, the file's own tag-family shadow first. Cases claim
/// no entry of the shared type namespace, so only this lookup answers
/// them.
pub(super) fn case_def_for_use(package: &Package, decls: &Decls, name: &str) -> Option<DefId> {
    match decls.current_file {
        Some(file) => package.lookup_case_in(file, name),
        None => package.lookup_case(name),
    }
}

pub(super) fn reserve_item(
    package: &Package,
    item: &Item,
    decls: &mut Decls,
    diags: &mut Vec<Diagnostic>,
) {
    // Locals and impl fns declare no single package def: a local
    // registers one entry per pattern binder, an impl fn registers its
    // method signature by registration span — both bypass the item-name
    // gate (an impl item names no value of its own).
    if let ItemKind::Local(local) = &item.kind {
        reserve_local(package, local, decls);
        return;
    }
    if let ItemKind::Impl(imp) = &item.kind {
        reserve_impl_fns(package, imp, item.span, decls);
        return;
    }
    let Some(def) = item_def(package, decls, item) else {
        return; // a duplicate definition was reported at collection
    };
    match &item.kind {
        ItemKind::Tag(t) => {
            // The atom mints even for a served def — the merged walk owns
            // the atom space — while the plain-table rows the serve
            // installs as its shared base stay skipped (see `is_served`).
            match &t.payload {
                EnumPayload::None => {
                    let id = declare_unit_name(decls, &t.name.name);
                    decls.atoms.insert(def, id);
                    decls.defs_of_atoms.insert(id, def);
                },
                // A payloaded tag is the minted payload case lifted to a
                // declaration: a struct-shaped atom carrying its payload
                // fields, constructed by call form and matched by struct
                // pattern. The payload spells named fields alone — a
                // tuple payload has no field-name tier to ride.
                EnumPayload::Struct(fields) => {
                    // the re-keyed mint: the declaring declaration's own
                    // atom, never a name lookup. `none` is the predeclared
                    // unit atom with no payload
                    // of its own: a payloaded declaration of the name
                    // claims no atom, the construction refusing at its
                    // own seam.
                    if let Some(id) = (t.name.name != "none")
                        .then(|| decls.universe.mint_payload_tag(&t.name.name))
                    {
                        decls.atoms.insert(def, id);
                        decls.defs_of_atoms.insert(id, def);
                        if !is_served(decls, def) {
                            decls.structs.insert(def, StructShape {
                                params: Vec::new(),
                                fields: fields.clone(),
                                linear: false,
                            });
                        }
                    }
                },
                EnumPayload::Tuple(_) => {},
            }
        },
        ItemKind::Struct(s) => {
            // the per-declaration mint: a struct of a label a case atom
            // (or any other declaration) holds shares the name index's
            // first entry instead of refusing
            let id = decls.universe.mint_struct(&s.name.name);
            decls.atoms.insert(def, id);
            decls.defs_of_atoms.insert(id, def);
            if !is_served(decls, def) {
                decls.structs.insert(def, StructShape {
                    params: s.generics.iter().map(GenericParam::from).collect(),
                    fields: struct_fields_ast(&s.members),
                    linear: s.supers.iter().any(linear_marker),
                });
            }
        },
        ItemKind::Enum(e) => {
            // A duplicate name keeps the first def (reported at
            // collection); the name span proves this item owns the def.
            if package.def(def).span != e.name.span {
                return;
            }
            let generic = !e.generics.is_empty();
            // A non-generic enum mints one atom per case into the tag
            // namespace (the enum name then names their union); a
            // generic enum keeps the opaque nominal atom with its case
            // signature, its unit cases minting below (a unit word
            // carries no substitution) while payloaded case shapes stay
            // per-instantiation facts. A generic head's mint is the
            // per-declaration form: a label a case atom or any other
            // declaration holds shares the name index's first entry.
            if generic {
                let id = decls.universe.mint_enum(&e.name.name);
                decls.atoms.insert(def, id);
                decls.defs_of_atoms.insert(id, def);
            }
            if !is_served(decls, def) {
                decls.enums.insert(def, EnumShape {
                    params: e.generics.iter().map(GenericParam::from).collect(),
                    cases: e
                        .cases
                        .iter()
                        .map(|c| (c.name.name.clone(), c.name.span, c.payload.clone()))
                        .collect(),
                    linear: e.supers.iter().any(linear_marker),
                });
            }
            // A generic enum's unit cases mint beside the non-generic
            // walk: a unit word carries no substitution, so its atom
            // tests and covers over every application of the head. A
            // payloaded case keeps no atom — its shape is a
            // per-instantiation fact the application's own derivation
            // mints — while its case row lands for every door (the
            // construction, the member arm) that resolves the def
            // first.
            for case in &e.cases {
                // The def's name span proves this item owns the def; a
                // cross-file same-label case resolves through its own
                // file's shadow entry first, the case namespace the
                // only namespace carrying case defs.
                let Some(case_def) = case_def_for_use(package, decls, &case.name.name)
                    .filter(|&d| package.def(d).span == case.name.span)
                else {
                    continue;
                };
                let atom = match &case.payload {
                    EnumPayload::None => Some(declare_unit_name(decls, &case.name.name)),
                    // the re-keyed mint: the declaring case's own atom,
                    // never a name lookup; a payloaded case named `none`
                    // claims no atom
                    EnumPayload::Tuple(_) | EnumPayload::Struct(_) => (!generic
                        && case.name.name != "none")
                        .then(|| decls.universe.mint_payload_tag(&case.name.name)),
                };
                if let Some(id) = atom {
                    decls.atoms.insert(case_def, id);
                    decls.defs_of_atoms.insert(id, case_def);
                }
                if !generic {
                    let linear = e.supers.iter().any(linear_marker);
                    if let EnumPayload::Struct(fields) = &case.payload
                        && !is_served(decls, case_def)
                    {
                        // A named-payload case carries the struct shape
                        // field access and struct patterns read.
                        decls.structs.insert(case_def, StructShape {
                            params: Vec::new(),
                            fields: fields.clone(),
                            linear,
                        });
                    }
                }
                if !is_served(decls, case_def) {
                    decls.cases.insert(case_def, CaseDecl {
                        enum_def: def,
                        payload: case.payload.clone(),
                    });
                }
            }
        },
        ItemKind::TypeAlias(a) => {
            decls.aliases.insert(
                def,
                (
                    a.generics.iter().map(GenericParam::from).collect(),
                    a.ty.clone(),
                ),
            );
        },
        ItemKind::Trait(t) => {
            // the trait registry shares one label space with the atoms,
            // so a label a case atom (or any other declaration) holds
            // refuses loudly here — the shared type namespace does not
            // answer for it
            let Ok(id) = decls.universe.declare_trait(&t.name.name) else {
                let held = t.name.name.clone();
                let mut diag = Diagnostic::error(
                    ErrorCode::DuplicateDefinition,
                    format!("`{held}` is already defined"),
                )
                .with_primary(t.name.span, "redefined here");
                if let Some(atom) = decls.universe.atom_by_name(&held)
                    && let Some(holder) = decls.defs_of_atoms.get(&atom)
                {
                    diag = diag.with_secondary(package.def(*holder).span, "first defined here");
                }
                diags.push(diag);
                return;
            };
            {
                decls.traits.insert(def, id);
                // The origin attribute records beside the trait id: the
                // seal's span is the declaring name occurrence, the
                // label the impl refusals name. The record reads the
                // item's own text, so a served def carries it the same
                // way a fresh one does.
                if item.attrs.iter().any(|a| a.name.name == "sealed") {
                    decls.sealed_traits.insert(def, t.name.span);
                }
                if !is_served(decls, def) {
                    let mut names = Vec::new();
                    for member in &t.members {
                        if let TraitMember::Fn(f) = member {
                            names.push(f.name.name.clone());
                            if let Some(method) = member_id(package, def, &f.name.name) {
                                decls.members.insert(method, Arc::new(FnSig::of(f)));
                            }
                        }
                    }
                    decls.trait_members.insert(def, names);
                    decls.trait_member_defaults.insert(
                        def,
                        t.members
                            .iter()
                            .filter_map(|member| {
                                let TraitMember::Fn(f) = member else {
                                    return None;
                                };
                                f.body.is_some().then(|| f.name.name.clone())
                            })
                            .collect(),
                    );
                    decls.trait_generics.insert(
                        def,
                        t.generics.iter().map(|p| p.name.name.clone()).collect(),
                    );
                }
            }
        },
        ItemKind::Fn(f) | ItemKind::ExternFn(f) => {
            if !is_served(decls, def) {
                decls.fns.insert(def, Arc::new(FnSig::of(f)));
            }
        },
        ItemKind::Import(_) | ItemKind::Local(_) | ItemKind::Impl(_) | ItemKind::Platform(_) => {},
    }
}

/// Inherent impl functions register their signatures against the method
/// registration their own span names — the same span discipline as
/// the effects walk. Trait impls register nothing here: the trait's
/// member signatures already provide the candidates, and the instance
/// declaration is the shape pass's work. Call sites check against each
/// impl's own signature; a same-name second inherent impl never gets a
/// registration (collection reports the duplicate and keeps the first).
/// A served method's signature and impl-site arrived with the artifact.
pub(super) fn reserve_impl_fns(
    package: &Package,
    imp: &ImplDecl,
    impl_site: Span,
    decls: &mut Decls,
) {
    if imp.trait_ref.is_some() {
        return;
    }
    for f in &imp.fns {
        if let Some(method) = package
            .method_candidates(&f.name.name)
            .find(|&id| package.method(id).span == f.name.span)
        {
            if method.index() < decls.served_methods {
                continue;
            }
            decls.inherent.insert(method, Arc::new(FnSig::of(f)));
            decls.inherent_impls.insert(method, impl_site);
        }
    }
}

pub(super) fn def_kind_is_binding(package: &Package, def: DefId) -> bool {
    matches!(package.def(def).kind, DefKind::Const | DefKind::Var)
}

/// One top-level binding: every binder the pattern declares maps back to
/// this initializer — a destructuring pattern declares many defs, so
/// the item's own def is not the gate (a tuple pattern names no single
/// def). The const-of-lambda registration is the single-name shape only:
/// a lambda value binds one name. A row the serve already carries — the
/// shared base's, or an earlier file's — stays put: the base owns the
/// prefix's rows and a compiled file inserts only its own.
pub(super) fn reserve_local(package: &Package, local: &LocalDecl, decls: &mut Decls) {
    for binder in pattern_names(&local.pat) {
        if let Some(def) = package.lookup_value(&binder)
            && def_kind_is_binding(package, def)
            && decls.consts.get(&def).is_none()
        {
            decls.consts.insert(def, Arc::new(local.clone()));
        }
    }
    let Pattern::Binding { name, .. } = &local.pat else {
        return;
    };
    let Some(def) = package
        .lookup_value(&name.name)
        .filter(|def| decls.fns.get(def).is_none())
    else {
        return;
    };
    if let Some(mut sig) = FnSig::of_lambda(&local.init) {
        // A const-arrow function binding: the signature is the lambda's
        // own annotations; a written binding annotation supplies the
        // return where the lambda omits one.
        if sig.ret.is_none() {
            sig.ret.clone_from(&local.ty);
        }
        decls.fns.insert(def, Arc::new(sig));
    }
}

/// The served prefix's shared const rows, built once per artifact from the
/// prefix's own resident tree: one row per binding-pattern binder, exactly
/// the rows the reserve walk's own loop would insert, cloned once instead
/// of per member compile. The caller parks the result behind a
/// [`Park`](crate::Park) cell and installs it as every serve's
/// [`Decls::consts`] base, so each member compile's teardown drops a
/// refcount instead of the rows.
#[must_use]
pub fn consts_base(tree: &Ast, package: &Package) -> Arc<IdTable<DefId, Arc<LocalDecl>>> {
    let mut table = IdTable::default();
    for block in &tree.blocks {
        for item in &block.items {
            let ItemKind::Local(local) = &item.kind else {
                continue;
            };
            for binder in pattern_names(&local.pat) {
                if let Some(def) = package.lookup_value(&binder)
                    && def_kind_is_binding(package, def)
                {
                    table.insert(def, Arc::new(local.clone()));
                }
            }
        }
    }
    Arc::new(table)
}
