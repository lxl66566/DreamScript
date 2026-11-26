//! The linked install half of the declaration pass: mints one linked member's
//! exported type, trait, impl, and binding declarations into this compile's
//! tables, forcing a referenced member's install forward when the def order
//! demands it, and resolving the fixed-layer overlaps the link leaves.

use super::*;

/// Mints one linked type's structural fact into the declaration tables:
/// exactly the products a local declaration of the same shape carries,
/// so every later pass consumes the linked type like a local one and no
/// second type kind exists — a struct's atom and field signature, an
/// enum's case rows, a case's own atom, struct row, and case row, an
/// alias's written body, an opaque struct's reserved atom, and a
/// tag's unit atom. The word kinds resolve by name in this compile's
/// own universe (the ambient's atoms), the same trust the const fact's
/// scalar atom enjoys — except the Named word, which resolves through
/// the link table onto the def this compile's own registration minted
/// for the producer's declaration. The link pass's registration gates
/// already proved the mint's name free, so a failed mint or an
/// unresolvable word skips the def silently — its uses then fail at
/// their own sites rather than mis-shaping.
pub(super) fn install_foreign_type(
    ctx: &mut MintCtx,
    def: DefId,
    name: &str,
    span: Span,
    link: u32,
    fact: &crate::import::TypeFact,
    decls: &mut Decls,
) {
    match fact {
        crate::import::TypeFact::Struct { fields, linear } => {
            install_foreign_struct(ctx, def, name, span, fields, *linear, decls);
        },
        crate::import::TypeFact::Enum { cases, linear } => {
            install_foreign_enum(ctx, def, name, span, cases, *linear, decls);
        },
        crate::import::TypeFact::Case(fact) => {
            install_foreign_case(ctx, def, name, span, fact, decls);
        },
        crate::import::TypeFact::Alias(word) => {
            install_foreign_alias(ctx, def, name, span, (**word).clone(), decls);
        },
        crate::import::TypeFact::Opaque => {
            install_foreign_opaque(def, name, span, decls);
        },
        crate::import::TypeFact::Tag => {
            install_foreign_tag(def, name, span, decls);
        },
        crate::import::TypeFact::Trait(fact) => {
            install_foreign_trait(ctx, def, name, link, fact, decls);
        },
    }
}

/// The trait fact's mint: the trait id, the member rows keyed by their
/// registered method ids, and the member-name table an impl checks
/// against — exactly the products a local trait declaration carries.
/// A member whose default body crossed is omittable by an impl and
/// dispatches into the linked body, so the crossed defaults ride the
/// omission gate's table and the linked default-body links. A mint
/// whose member row names no registered method skips the row silently
/// — its uses then fail at their own sites.
pub(super) fn install_foreign_trait(
    ctx: &mut MintCtx,
    def: DefId,
    name: &str,
    link: u32,
    fact: &crate::import::TraitFact,
    decls: &mut Decls,
) {
    if decls.traits.get(&def).is_some() {
        return; // a forced mint already ran this install
    }
    let Ok(trait_id) = decls.universe.declare_trait(name) else {
        return;
    };
    decls.traits.insert(def, trait_id);
    decls.foreign_type_names.insert(name.to_owned(), def);
    for member in &fact.members {
        if let Some(method) = member_id(ctx.package, def, &member.name) {
            decls.members.insert(method, member.sig.clone());
        }
    }
    decls
        .trait_members
        .insert(def, fact.members.iter().map(|m| m.name.clone()).collect());
    let defaults: Vec<crate::import::ImplMemberFact> = fact
        .members
        .iter()
        .filter_map(|member| {
            member
                .default_func
                .map(|func| crate::import::ImplMemberFact {
                    name: member.name.clone(),
                    func,
                })
        })
        .collect();
    decls.trait_member_defaults.insert(
        def,
        defaults.iter().map(|member| member.name.clone()).collect(),
    );
    if !defaults.is_empty() {
        decls.linked_trait_defaults.insert(
            def,
            Arc::new(ForeignImplLink {
                position: link,
                members: defaults,
                // the trait's own default rows: no impl identity rides
                // them, so the layering's fields stay neutral
                pkg: String::new(),
                exercised: false,
            }),
        );
    }
    decls.trait_generics.insert(def, Vec::new());
}

/// Renders a linked impl's refused instance declaration: the same
/// sentences the local shape pass renders, the impl row's own span the
/// primary site.
pub(super) fn linked_instance_diag(
    universe: &Universe,
    err: &crate::Error,
    fact: &crate::import::ImplFact,
) -> Diagnostic {
    match err {
        crate::Error::InstanceOverlap {
            head_a, witness, ..
        } => Diagnostic::error(
            ErrorCode::InstanceOverlap,
            format!(
                "linked instance head `{}` intersects the `{}` instance; witness value type `{}`",
                crate::import::render_word(&fact.head),
                universe.render(*head_a),
                universe.render_witness(witness)
            ),
        )
        .with_primary(fact.span, "overlapping linked head")
        .with_note(
            "the earlier instance's head covers this value type; two instances of one trait must \
             be pairwise disjoint [B6]"
                .to_owned(),
        ),
        other => Diagnostic::error(
            ErrorCode::AmbiguousInstance,
            format!("linked instance declaration rejected: {other}"),
        )
        .with_primary(fact.span, "rejected linked impl"),
    }
}

/// One impl's fixed layer in the link resolution: the root package's own impls, an origin
/// impl (the package declaring the type or the trait), and every other dependency's.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum ImplLayer {
    Root,
    Origin,
    ThirdParty,
}

/// One stored impl's side of an overlap the link resolves: its layer,
/// the declaring package's identity the diagnostics spell, whether the
/// declaring package's own artifact statically exercises the head, and
/// the solved-table rows its declaration carries. `span` groups sibling
/// branch rows of one local impl item; `ptr` groups one linked fact's
/// (one `ForeignImplLink` allocation).
#[derive(Clone)]
pub(super) struct LinkSide {
    pub(super) layer: ImplLayer,
    pub(super) pkg: Option<String>,
    pub(super) exercised: bool,
    pub(super) span: Option<Span>,
    pub(super) ptr: Option<usize>,
    pub(super) rows: Vec<InstanceId>,
}

/// How a linked impl row's overlap with the stored instances resolved.
pub(super) enum LinkOutcome {
    /// No overlap: the row declares.
    Declare,
    /// The row wins; every stored side it overlaps leaves the tables.
    Wins(Vec<LinkSide>),
    /// The row loses to a stronger stored side; the spelling names it.
    Loses(String),
    /// No layer separates the row from the stored sides; the link
    /// refuses naming both packages.
    Refuses(Vec<LinkSide>),
}

/// The diagnostics' spelling of one impl's declaring side.
pub(super) fn impl_side_spell(pkg: Option<&str>) -> String {
    match pkg {
        Some(pkg) => format!("`{pkg}`'s impl"),
        None => "the root package's own impl".to_owned(),
    }
}

/// The internal-exercise refusal: the shadowed impl's declaring package
/// statically dispatches its own head (its linked artifact baked a
/// statically-resolved instance word naming it), so the shadowed body
/// would keep its behavior while the rest of the program follows the
/// winner — one value two behaviors. The link refuses naming the loser.
pub(super) fn shadowed_exercised_diag(
    loser_pkg: &str,
    fact: &crate::import::ImplFact,
) -> Diagnostic {
    Diagnostic::error(
        ErrorCode::ShadowedImplExercised,
        format!(
            "`{loser_pkg}`'s impl of `{}` for `{}` cannot be shadowed: its own artifact \
             statically dispatches the head, so the shadowed body would keep its behavior under \
             the winner",
            fact.trait_name,
            crate::import::render_word(&fact.head),
        ),
    )
    .with_primary(fact.span, "the shadowed impl's declaring row")
}

/// The positions of the packages whose declarations make a linked impl
/// an origin: the declaring package of the trait and of every named
/// type the solved head carries. A fact whose own link position sits in
/// the set is an origin impl; every other dependency's is a third
/// party's. The root package's positions never enter — its impls are
/// the top layer on their own.
pub(super) fn origin_positions_of(
    package: &Package,
    decls: &Decls,
    trait_def: DefId,
    solved_head: Ty,
) -> rustc_hash::FxHashSet<u32> {
    let mut positions = rustc_hash::FxHashSet::default();
    if let Some(entry) = package.foreign().get(&trait_def) {
        positions.insert(entry.link);
    }
    let mut atoms: Vec<TypeId> = decls.universe.members(solved_head).to_vec();
    let mut seen = 0;
    while seen < atoms.len() {
        let atom = atoms[seen];
        seen += 1;
        if let Some(&def) = decls.defs_of_atoms.get(&atom)
            && let Some(entry) = package.foreign().get(&def)
        {
            positions.insert(entry.link);
        }
        if let Ok(decl) = decls.universe.atom_decl(atom) {
            match &decl.kind {
                crate::atom::AtomKind::Apply(_, args) => {
                    for arg in args {
                        atoms.extend(decls.universe.members(*arg));
                    }
                },
                crate::atom::AtomKind::Tuple(elements) => atoms.extend(elements.iter().copied()),
                _ => {},
            }
        }
    }
    positions
}

/// The linked mint's resolution context: the consuming package's link
/// table — the registered rows a Named word resolves through, each the
/// producer's own declaration — and the forcing stack that breaks
/// reference cycles while one install pulls another's declaration
/// forward.
pub(super) struct MintCtx<'a> {
    /// The consuming compile's package.
    pub(super) package: &'a Package,
    /// The defs whose installs are in flight: a second pull of the same
    /// def is a cycle the early-declared atoms already serve (or an
    /// inline cycle the producer's own fixpoint refused).
    pub(super) forcing: Vec<DefId>,
}

/// Ensures one linked def's mint ran, forcing it forward when its def
/// order sits ahead of the referencing install: an install declares its
/// atom ahead of resolving its fields, so a forced chain's members
/// resolve while the first install is still in flight, and a def whose
/// mint never completes answers `None` — the referencing def's silent
/// skip.
pub(super) fn force_minted(ctx: &mut MintCtx, def: DefId, decls: &mut Decls) -> Option<()> {
    if decls.atoms.get(&def).is_some() || decls.enums.get(&def).is_some() {
        return Some(());
    }
    if ctx.forcing.contains(&def) {
        return None;
    }
    let entry = ctx.package.foreign().get(&def)?;
    let fact = Arc::clone(entry.ty.as_ref()?);
    let name = entry.name.clone();
    let span = entry.decl_span;
    let link = entry.link;
    ctx.forcing.push(def);
    install_foreign_type(ctx, def, &name, span, link, &fact, decls);
    ctx.forcing.pop();
    (decls.atoms.get(&def).is_some() || decls.enums.get(&def).is_some()).then_some(())
}

/// Whether one import-bound def's install already ran: an atom (a
/// struct, a case, a tag), an enum's case rows, an alias's written
/// body, or a trait's id.
pub(super) fn linked_minted(decls: &Decls, def: DefId) -> bool {
    decls.atoms.contains_key(&def)
        || decls.enums.contains_key(&def)
        || decls.aliases.contains_key(&def)
        || decls.traits.contains_key(&def)
}

/// Ensures one linked def's install ran for a written type spelling —
/// the shape pass names linked types ahead of the linked mint loop, and
/// a field whose type missed would silently drop out of the
/// referencing declaration's universe atom, mis-shaping both this
/// module's own lowering and the link fact the export walk derives
/// over the same fields. An enum forces its cases with it (the union
/// the name spells needs their atoms); a def whose install never
/// completes answers `None`, and the caller keeps the field dropped.
pub(super) fn force_linked(package: &Package, decls: &mut Decls, def: DefId) -> Option<()> {
    let mut ctx = MintCtx {
        package,
        forcing: Vec::new(),
    };
    if !linked_minted(decls, def) {
        let entry = package.foreign().get(&def)?;
        let fact = Arc::clone(entry.ty.as_ref()?);
        let name = entry.name.clone();
        let span = entry.decl_span;
        let link = entry.link;
        ctx.forcing.push(def);
        install_foreign_type(&mut ctx, def, &name, span, link, &fact, decls);
        ctx.forcing.pop();
    }
    if decls.enums.contains_key(&def) {
        // the enum's own cases: their entries sit at their own def
        // positions ahead of the late mint loop, so each forces here
        let enum_name = package.def(def).name.clone();
        let cases: Vec<DefId> = package
            .foreign()
            .iter()
            .filter(|(_, entry)| {
                matches!(
                    entry.ty.as_deref(),
                    Some(crate::import::TypeFact::Case(fact)) if fact.enum_name == enum_name
                )
            })
            .map(|(def, _)| *def)
            .collect();
        for case_def in cases {
            force_minted(&mut ctx, case_def, decls)?;
        }
    }
    Some(())
}

/// One linked field list as the pair of forms the tables carry: the
/// solved universe fields and the written AST fields (the spelling a
/// local field of the same word would carry). `None` when a word names
/// no atom of this universe — the caller skips the whole def silently.
pub(super) fn foreign_fields(
    ctx: &mut MintCtx,
    fields: &[crate::import::TypeField],
    span: Span,
    decls: &mut Decls,
) -> Option<(Vec<Field>, Vec<FieldDecl>)> {
    let mut universe_fields = Vec::with_capacity(fields.len());
    let mut ast_fields = Vec::with_capacity(fields.len());
    for field in fields {
        let (expr, written) = foreign_field_type(ctx, field.word.clone(), span, decls)?;
        universe_fields.push(Field::named(&field.name, expr));
        ast_fields.push(FieldDecl {
            span,
            name: Ident {
                span,
                name: field.name.clone(),
            },
            ty: written,
            doc: Vec::new(),
        });
    }
    Some((universe_fields, ast_fields))
}

/// The struct fact's mint: the reserved atom, its solved field
/// signature, the def tables the construction and field-access paths
/// read, and the name index the signature solving falls back to.
pub(super) fn install_foreign_struct(
    ctx: &mut MintCtx,
    def: DefId,
    name: &str,
    span: Span,
    fields: &[crate::import::TypeField],
    linear: bool,
    decls: &mut Decls,
) {
    // The atom: a fresh run declares it, a served prefix's replay
    // already declared it (the artifact's mint log and def-atom tables
    // carry this def's atom), so the re-run takes the replayed atom and
    // still installs the rows below — the name index is the one row the
    // serve restores through no other table. Any other taken name keeps
    // the silent skip. The atom declares AHEAD of the fields'
    // resolution: a field naming this very struct (a container-of-self
    // word), or one whose own fields force this def forward, resolves
    // the declared atom while the install is still in flight.
    let atom = match decls.universe.declare_struct(name) {
        Ok(atom) => atom,
        Err(_) => match decls.atoms.get(&def) {
            Some(&atom) => atom,
            None => return,
        },
    };
    decls.atoms.insert(def, atom);
    decls.defs_of_atoms.insert(atom, def);
    let Some((universe_fields, ast_fields)) = foreign_fields(ctx, fields, span, decls) else {
        return;
    };
    // The replayed atom already carries its shape — the serve set it —
    // so both a fresh set and the replay's refusal answer the same
    // fields; any other shape refusal keeps the silent skip.
    if !matches!(
        decls.universe.set_struct_fields(atom, universe_fields),
        Ok(()) | Err(crate::Error::ShapeAlreadySet(_))
    ) {
        return;
    }
    decls.structs.insert(def, StructShape {
        params: Vec::new(),
        fields: ast_fields,
        linear,
    });
    decls.foreign_type_names.insert(name.to_owned(), def);
}

/// The enum fact's mint: the AST-shaped case rows the enum's own union
/// solving and match coverage read, plus the name index — the case rows
/// themselves mint at each case's own def (a non-generic enum carries
/// no atom of its own; its type is the union of its case atoms).
pub(super) fn install_foreign_enum(
    ctx: &mut MintCtx,
    def: DefId,
    name: &str,
    span: Span,
    cases: &[crate::import::TypeCase],
    linear: bool,
    decls: &mut Decls,
) {
    let mut ast_cases = Vec::with_capacity(cases.len());
    for case in cases {
        let payload = match &case.payload {
            crate::import::CasePayload::Unit => EnumPayload::None,
            crate::import::CasePayload::Tuple(words) => {
                let mut ast_items = Vec::with_capacity(words.len());
                for word in words {
                    let Some((_, written)) = foreign_field_type(ctx, word.clone(), span, decls)
                    else {
                        return;
                    };
                    ast_items.push(written);
                }
                EnumPayload::Tuple(ast_items)
            },
            crate::import::CasePayload::Struct(fields) => {
                let Some((_, ast_fields)) = foreign_fields(ctx, fields, span, decls) else {
                    return;
                };
                EnumPayload::Struct(ast_fields)
            },
        };
        ast_cases.push((case.name.clone(), span, payload));
    }
    decls.enums.insert(def, EnumShape {
        params: Vec::new(),
        cases: ast_cases,
        linear,
    });
    decls.foreign_type_names.insert(name.to_owned(), def);
}

/// The case fact's mint: the case's atom (a unit tag for a bare case, a
/// struct atom with its field signature for a payloaded one), the
/// struct row a payloaded case's field access reads, the case row that
/// maps back to the enum's def, and the name index — exactly the
/// products a local non-generic enum's case declaration carries. The
/// enum's own install ran earlier in def order (the link walk registers
/// the enum before its cases), so the enum def resolves through the
/// name index here.
pub(super) fn install_foreign_case(
    ctx: &mut MintCtx,
    def: DefId,
    name: &str,
    span: Span,
    fact: &crate::import::CaseFact,
    decls: &mut Decls,
) {
    let Some(&enum_def) = decls.foreign_type_names.get(&fact.enum_name) else {
        return;
    };
    // the enum's own linearity governs its payloaded cases' struct rows,
    // exactly the local mint's rule: the enum installed earlier in def
    // order (the link walk registers the enum before its cases), so its
    // installed shape answers here
    let linear = decls.enums.get(&enum_def).is_some_and(|shape| shape.linear);
    let payload = match &fact.payload {
        crate::import::CasePayload::Unit => EnumPayload::None,
        crate::import::CasePayload::Tuple(words) => {
            let mut universe_fields = Vec::with_capacity(words.len());
            let mut ast_items = Vec::with_capacity(words.len());
            for word in words {
                let Some((expr, written)) = foreign_field_type(ctx, word.clone(), span, decls)
                else {
                    return;
                };
                universe_fields.push(Field::positional(expr));
                ast_items.push(written);
            }
            let atom = match decls.universe.declare_struct(name) {
                Ok(atom) => atom,
                // a served prefix's replay already declared it; the
                // fresh run's mint would duplicate — take the replayed
                // atom and still install the rows below
                Err(_) => match decls.atoms.get(&def) {
                    Some(&atom) => atom,
                    None => return,
                },
            };
            if !matches!(
                decls.universe.set_struct_fields(atom, universe_fields),
                Ok(()) | Err(crate::Error::ShapeAlreadySet(_))
            ) {
                return;
            }
            decls.atoms.insert(def, atom);
            decls.defs_of_atoms.insert(atom, def);
            EnumPayload::Tuple(ast_items)
        },
        crate::import::CasePayload::Struct(fields) => {
            let Some((universe_fields, ast_fields)) = foreign_fields(ctx, fields, span, decls)
            else {
                return;
            };
            let atom = match decls.universe.declare_struct(name) {
                Ok(atom) => atom,
                // a served prefix's replay already declared it; the
                // fresh run's mint would duplicate — take the replayed
                // atom and still install the rows below
                Err(_) => match decls.atoms.get(&def) {
                    Some(&atom) => atom,
                    None => return,
                },
            };
            if !matches!(
                decls.universe.set_struct_fields(atom, universe_fields),
                Ok(()) | Err(crate::Error::ShapeAlreadySet(_))
            ) {
                return;
            }
            decls.atoms.insert(def, atom);
            decls.defs_of_atoms.insert(atom, def);
            decls.structs.insert(def, StructShape {
                params: Vec::new(),
                fields: ast_fields.clone(),
                linear,
            });
            EnumPayload::Struct(ast_fields)
        },
    };
    if matches!(payload, EnumPayload::None) {
        // the reserved-atom mapping owns both spellings: a case named
        // `none` maps to the predeclared none atom exactly like a local
        // case's mint, so the linked case's values are the consumer's
        // own `none` words
        let atom = match declare_foreign_unit_name(decls, name) {
            Some(atom) => Some(atom),
            None => decls.atoms.get(&def).copied(),
        };
        let Some(atom) = atom else {
            return;
        };
        decls.atoms.insert(def, atom);
        decls.defs_of_atoms.insert(atom, def);
    }
    decls.cases.insert(def, CaseDecl { enum_def, payload });
    decls.foreign_type_names.insert(name.to_owned(), def);
}

/// The alias fact's mint: the written body a local alias of the same
/// word would carry — no atom (an alias is its body's set, never a
/// constructor), so a use site's solving expands the body in this
/// compile's own universe exactly like the local spelling's.
pub(super) fn install_foreign_alias(
    ctx: &mut MintCtx,
    def: DefId,
    name: &str,
    span: Span,
    word: crate::import::TypeWord,
    decls: &mut Decls,
) {
    let Some((_, written)) = foreign_field_type(ctx, word, span, decls) else {
        return;
    };
    decls.aliases.insert(def, (Vec::new(), written));
    decls.foreign_type_names.insert(name.to_owned(), def);
}

/// The opaque fact's mint: exactly the products a local bodiless
/// `struct x` reserves — the shape-pending atom, the def tables the
/// construction and field-access paths read, and the name index — with
/// no field signature ever set, so the name binds in signatures while
/// every value use keeps the local spelling's own refusals.
pub(super) fn install_foreign_opaque(def: DefId, name: &str, _span: Span, decls: &mut Decls) {
    let atom = match decls.universe.declare_struct(name) {
        Ok(atom) => atom,
        // a served prefix's replay already declared it; take the
        // replayed atom and still install the rows below
        Err(_) => match decls.atoms.get(&def) {
            Some(&atom) => atom,
            None => return,
        },
    };
    decls.atoms.insert(def, atom);
    decls.defs_of_atoms.insert(atom, def);
    decls.structs.insert(def, StructShape {
        params: Vec::new(),
        fields: Vec::new(),
        linear: false,
    });
    decls.foreign_type_names.insert(name.to_owned(), def);
}

/// The tag fact's mint: the unit atom a local `tag` declaration
/// carries — no field signature, no case rows — plus the def tables
/// and the name index, exactly the local products. The producer's
/// tag-table row (when its own lowering minted one) seeds the
/// consuming compile's table at lowering, so union-typed values cross
/// under ids both modules agree on.
pub(super) fn install_foreign_tag(def: DefId, name: &str, _span: Span, decls: &mut Decls) {
    let atom = match decls.universe.declare_tag(name) {
        Ok(atom) => atom,
        // a served prefix's replay already declared it; take the
        // replayed atom and still install the rows below
        Err(_) => match decls.atoms.get(&def) {
            Some(&atom) => atom,
            None => return,
        },
    };
    decls.atoms.insert(def, atom);
    decls.defs_of_atoms.insert(atom, def);
    decls.foreign_type_names.insert(name.to_owned(), def);
}

/// One linked field's word kind as the pair of forms the tables carry:
/// the solved [`TypeExpr`] the set algebra and the lowerer read, and the
/// written [`Type`] the checker's per-use solving reads (a single-segment
/// path naming the atom — the spelling a local field of the same word
/// would carry). `None` when a word names no atom of this universe.
pub(super) fn foreign_field_type(
    ctx: &mut MintCtx,
    word: crate::import::TypeWord,
    span: Span,
    decls: &mut Decls,
) -> Option<(TypeExpr, Type)> {
    use crate::import::TypeWord;

    let named = move |name: &str, args: Vec<Type>| {
        let ident = Ident {
            span,
            name: name.to_owned(),
        };
        Type::Path {
            span,
            path: ds_parse::Path {
                span,
                segments: vec![ident],
            },
            args,
            delim: Delim::Bracket,
        }
    };
    match word {
        TypeWord::Scalar(word) => {
            let atom = decls.universe.atom_by_name(word.atom_name())?;
            Some((TypeExpr::Atom(atom), named(word.atom_name(), Vec::new())))
        },
        TypeWord::List(inner) => {
            let (element, written_element) = foreign_field_type(ctx, *inner, span, decls)?;
            let element_ty = decls.universe.solve(&element).ok()?;
            let ctor = decls.universe.atom_by_name("list")?;
            let apply = decls.universe.apply_atom(ctor, &[element_ty]).ok()?;
            Some((TypeExpr::Atom(apply), named("list", vec![written_element])))
        },
        TypeWord::Map(key, value) => {
            let (key_expr, written_key) = foreign_field_type(ctx, *key, span, decls)?;
            let (value_expr, written_value) = foreign_field_type(ctx, *value, span, decls)?;
            let key_ty = decls.universe.solve(&key_expr).ok()?;
            let value_ty = decls.universe.solve(&value_expr).ok()?;
            let ctor = decls.universe.atom_by_name("map")?;
            let apply = decls.universe.apply_atom(ctor, &[key_ty, value_ty]).ok()?;
            let written = named("map", vec![written_key, written_value]);
            Some((TypeExpr::Atom(apply), written))
        },
        TypeWord::Optional(inner) => {
            let (solved_inner, written_inner) = foreign_field_type(ctx, *inner, span, decls)?;
            // `t?` is exactly `t | none`: the union tree the local
            // optional's own conversion builds, the presence tag over
            // the word's payload word(s)
            let solved = solved_inner.union(TypeId::NONE.into());
            let written = Type::Optional {
                span,
                inner: Box::new(written_inner),
            };
            Some((solved, written))
        },
        TypeWord::Bytes => {
            let atom = decls.universe.atom_by_name("bytes")?;
            Some((TypeExpr::Atom(atom), named("bytes", Vec::new())))
        },
        TypeWord::Named(name) => {
            // Identity, not name lookup: the registration's own row for
            // the producer's declaration — never this compile's ambient
            // or local names — resolved onto the def the registration
            // minted, that def's install forced forward when def order
            // put it ahead.
            let def = ctx
                .package
                .foreign()
                .iter()
                .find(|(_, entry)| entry.name == name && entry.ty.is_some())
                .map(|(def, _)| *def)?;
            force_minted(ctx, def, decls)?;
            if let Some(&atom) = decls.atoms.get(&def) {
                return Some((TypeExpr::Atom(atom), named(&name, Vec::new())));
            }
            // An enum's own name: the union of its minted case atoms,
            // exactly the written spelling's solving. The enum's
            // install ran above (forced if need be) but its cases mint
            // at their own defs — entries def order may still hold
            // ahead, so each forces here too.
            let cases: Vec<(DefId, Arc<crate::import::TypeFact>)> = ctx
                .package
                .foreign()
                .iter()
                .filter(|(_, entry)| {
                    matches!(
                        entry.ty.as_deref(),
                        Some(crate::import::TypeFact::Case(fact)) if fact.enum_name == name
                    )
                })
                .map(|(def, entry)| Some((*def, Arc::clone(entry.ty.as_ref()?))))
                .collect::<Option<_>>()?;
            for (case_def, _) in &cases {
                force_minted(ctx, *case_def, decls)?;
            }
            let shape = decls.enums.get(&def)?;
            let solved = enum_case_union(ctx.package, &shape.cases, decls)?;
            Some((solved, named(&name, Vec::new())))
        },
        TypeWord::Union(members) => {
            // One union word per member spelling, the same pair a
            // local union annotation's own conversion builds: the
            // solved tree folds pairwise, the written tree spelling the
            // binary union the checker's solve reads back.
            let [first, rest @ ..] = &members[..] else {
                return None;
            };
            let (mut solved, mut written) = foreign_field_type(ctx, first.clone(), span, decls)?;
            for member in rest {
                let (next_solved, next_written) =
                    foreign_field_type(ctx, member.clone(), span, decls)?;
                solved = solved.union(next_solved);
                written = Type::Union {
                    span,
                    left: Box::new(written),
                    right: Box::new(next_written),
                };
            }
            Some((solved, written))
        },
        TypeWord::Tuple(members) => {
            // One positional word per member, the same pair a local
            // tuple field's conversion builds: the solved product the
            // universe distributes over union elements into one
            // structural atom per combination, the written tuple the
            // checker's per-use solving reads back. The empty tuple
            // never rides a fact (the walk refuses it), so it keeps
            // the silent skip here.
            let [first, rest @ ..] = &members[..] else {
                return None;
            };
            let mut solved = Vec::with_capacity(1 + rest.len());
            let mut items = Vec::with_capacity(1 + rest.len());
            for member in std::iter::once(first).chain(rest) {
                let (expr, written) = foreign_field_type(ctx, member.clone(), span, decls)?;
                solved.push(expr);
                items.push(written);
            }
            Some((TypeExpr::Tuple(solved), Type::Tuple { span, items }))
        },
    }
}
