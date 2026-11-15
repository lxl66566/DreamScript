//! Pins cross-run id determinism of the ambient analyze: two independent
//! passes through the run path's entry (fresh `Files` registry, fresh
//! trees, fresh `Universe` and `Package`) must assign identical ids —
//! every minted atom and trait in order, every package def and method,
//! every declared instance, and the solved span-keyed tables rendered
//! through the pass's own universe (solved handles are meaningful only
//! inside it, so the render is the comparable form).
//!
//! The checked-ambient artifact serves one pass's tables to the next, so
//! a mint order that depends on anything but the trees and their order
//! (hash iteration reaching a minting path, address order) would hand the
//! artifact an id space the next run does not reproduce — that is a hard
//! blocker for the whole artifact story, not a failure to paper over.
//! The user file exercises every minting table: a derive synthesizing an
//! impl, an enum with cases, a written trait impl, a conditional generic
//! impl (minted variables), and an inherent impl member.

use ds_check::{Analyzed, AtomKind, Universe};
use ds_stdlib::{AMBIENT_FILE, AMBIENT_SOURCE};

/// The user half of the pinned program, beside the ambient.
const USER: &str = "#[derive(show)]\nstruct point { x: int, y: int }\n\nenum color \
                    {\nred\ngreen\n}\n\ntrait local { fn m(self) -> int }\n\nimpl local for color \
                    {\nfn m(self) -> int { 1 }\n}\n\ntrait keyed { fn key(self) -> int \
                    }\n\nimpl[t: show] keyed for t {\nfn key(self) -> int { 2 }\n}\n\nimpl point \
                    {\nfn norm(self) -> int { (self.x * self.x + self.y * self.y) }\n}\n\nfn \
                    main() -> () {\nconst p = point { x: 3, y: 4 }\nprintln(\"${p.show()} \
                    ${p.norm()} ${p.key()}\")\n}\n";

/// Everything about one pass that names an id, each fact rendered to one
/// line (spans as file/start/end, solved types through the pass's own
/// universe).
#[derive(Debug, PartialEq, Eq)]
struct MintFacts {
    /// Every minted atom in mint order: index, name, rendered shape.
    atoms: Vec<String>,
    /// Every declared trait in declaration order: index, name, member
    /// atom indexes.
    traits: Vec<String>,
    /// Every package def in collection order: index, name, kind, span.
    defs: Vec<String>,
    /// Every registered method in registration order: index, name, owner,
    /// span.
    methods: Vec<String>,
    /// The decls atoms table: def index to atom index.
    atoms_table: Vec<String>,
    /// Every declared instance in id order: trait index, rendered head,
    /// rendered written head, rendered conditions, impl-item span.
    instances: Vec<String>,
    /// The instance-to-impl-item pairs, in instance id order.
    instance_impls: Vec<String>,
    /// The inherent method-to-impl-item pairs, in method id order.
    inherent_impls: Vec<String>,
    /// The solved trait-reference arguments per impl span, rendered.
    trait_impl_args: Vec<String>,
    /// The solved inherent bounds per impl span: head plus rendered
    /// conditions.
    inherent_impl_bounds: Vec<String>,
    /// The solved return type of every impl-member body, rendered.
    impl_fn_rets: Vec<String>,
    /// The shared type-variable counter's final value.
    var_counter: u32,
}

/// One independent pass, mirroring how the run path builds its bundle:
/// fresh registry, ambient registered first (its tree marked
/// pre-expanded, the blob discipline), the user file parsed in script
/// mode after it.
fn one_pass() -> MintFacts {
    let mut files = ds_diag::Files::new();
    let ambient = files.add_source(AMBIENT_FILE, AMBIENT_SOURCE);
    let user = files.add_source("user.ds", USER);
    let ambient_ast = ds_parse::parse(&files, ambient).expect("the ambient parses");
    let user_ast = ds_parse::parse_script(&files, user).expect("the user source parses");
    let analyzed = ds_meta::analyze_owned(&files, vec![ambient_ast, user_ast], &[true, false]);
    assert!(
        analyzed.checked.diags.is_empty(),
        "the pinned program must check clean: {:?}",
        analyzed.checked.diags
    );
    facts(&analyzed)
}

/// Collects the id-naming facts of one finished pass.
fn facts(analyzed: &Analyzed) -> MintFacts {
    let decls = &analyzed.decls;
    let universe = &decls.universe;
    let atoms = universe
        .atom_ids()
        .enumerate()
        .map(|(index, id)| {
            let decl = universe.atom_decl(id).expect("every id has a decl");
            format!(
                "{} {} {}",
                index,
                decl.name,
                render_kind(universe, &decl.kind)
            )
        })
        .collect();
    let mut traits = Vec::new();
    for (_, &trait_id) in &decls.traits {
        let decl = universe
            .trait_decl(trait_id)
            .expect("registered traits exist");
        let members: Vec<String> = decl
            .members
            .iter()
            .map(|atom| atom.index().to_string())
            .collect();
        traits.push(format!(
            "{} {} [{}]",
            trait_id.index(),
            decl.name,
            members.join(", ")
        ));
    }
    let defs = analyzed
        .package
        .defs()
        .iter()
        .enumerate()
        .map(|(index, def)| format!("{index} {} {:?} {}", def.name, def.kind, span(def.span)))
        .collect();
    let methods = analyzed
        .package
        .methods()
        .iter()
        .enumerate()
        .map(|(index, method)| {
            let owner = match &method.owner {
                ds_check::MethodOwner::Trait(id) => {
                    format!("trait {}", analyzed.package.def(*id).name)
                },
                ds_check::MethodOwner::Inherent(target) => format!("inherent {target}"),
            };
            format!("{index} {} {owner} {}", method.name, span(method.span))
        })
        .collect();
    let atoms_table = decls
        .atoms
        .iter()
        .map(|(def, atom)| format!("{} {}", def.index(), atom.index()))
        .collect();
    let mut instances = Vec::new();
    let mut instance_impls = Vec::new();
    let instance_count = u32::try_from(decls.instances.len()).expect("instance count fits u32");
    assert!(instance_count > 0, "the program declares instances");
    for id in 0..instance_count {
        let instance = ds_check::InstanceId::from_raw(id);
        let trait_id = decls.instances.trait_of(instance).expect("in range");
        let head = universe.render(decls.instances.head(instance).expect("in range"));
        let written = universe.render(decls.instances.written(instance).expect("in range"));
        let conditions: Vec<String> = decls
            .instances
            .conditions(instance)
            .expect("in range")
            .iter()
            .map(|cond| render_condition(universe, cond.trait_id, cond.ty))
            .collect();
        instance_impls.push(format!(
            "{id} {}",
            decls
                .instance_impls
                .get(&instance)
                .copied()
                .map_or_else(|| "-".to_owned(), span)
        ));
        instances.push(format!(
            "{id} {} {head} | {written} [{}]",
            trait_id.index(),
            conditions.join(", ")
        ));
    }
    let inherent_impls = decls
        .inherent_impls
        .iter()
        .map(|(method, impl_span)| format!("{} {}", method.index(), span(*impl_span)))
        .collect();
    let mut trait_impl_args = Vec::new();
    for (impl_span, args) in &decls.trait_impl_args {
        let rendered: Vec<String> = args.iter().map(|ty| universe.render(*ty)).collect();
        trait_impl_args.push(format!("{} [{}]", span(*impl_span), rendered.join(", ")));
    }
    let mut inherent_impl_bounds = Vec::new();
    for (impl_span, bounds) in &decls.inherent_impl_bounds {
        let conditions: Vec<String> = bounds
            .conditions
            .iter()
            .map(|cond| render_condition(universe, cond.trait_id, cond.ty))
            .collect();
        inherent_impl_bounds.push(format!(
            "{} {} [{}]",
            span(*impl_span),
            universe.render(bounds.head),
            conditions.join(", ")
        ));
    }
    let mut impl_fn_rets = Vec::new();
    for (fn_span, ret) in &decls.impl_fn_rets {
        impl_fn_rets.push(format!("{} {}", span(*fn_span), universe.render(*ret)));
    }
    MintFacts {
        atoms,
        traits,
        defs,
        methods,
        atoms_table,
        instances,
        instance_impls,
        inherent_impls,
        trait_impl_args,
        inherent_impl_bounds,
        impl_fn_rets,
        var_counter: decls.var_counter,
    }
}

/// One span rendered as file/start/end.
fn span(span: ds_diag::Span) -> String {
    format!("{}/{}/{}", span.file.as_u32(), span.start, span.end)
}

/// One trait constraint rendered through the pass's universe.
fn render_condition(universe: &Universe, trait_id: ds_check::TraitId, ty: ds_check::Ty) -> String {
    format!(
        "{} for {}",
        universe.trait_decl(trait_id).expect("registered").name,
        universe.render(ty)
    )
}

/// Renders one atom shape through the pass's own universe.
fn render_kind(universe: &Universe, kind: &AtomKind) -> String {
    match kind {
        AtomKind::Unit => "unit".to_owned(),
        AtomKind::Reserved => "reserved".to_owned(),
        AtomKind::Struct(fields) => {
            let names: Vec<String> = fields.iter().map(render_field).collect();
            format!("struct({})", names.join(", "))
        },
        AtomKind::Enum(cases) => {
            let names: Vec<String> = cases
                .iter()
                .map(|case| {
                    let fields: Vec<String> = case.fields.iter().map(render_field).collect();
                    format!("{}({})", case.name, fields.join(", "))
                })
                .collect();
            format!("enum({})", names.join(", "))
        },
        AtomKind::Tuple(elements) => {
            let names: Vec<String> = elements.iter().map(|el| el.index().to_string()).collect();
            format!("tuple({})", names.join(", "))
        },
        AtomKind::Apply(ctor, args) => {
            let names: Vec<String> = args.iter().map(|ty| universe.render(*ty)).collect();
            format!("apply({}, {})", ctor.index(), names.join(", "))
        },
        AtomKind::FnSig(params, ret) => {
            let names: Vec<String> = params.iter().map(|ty| universe.render(*ty)).collect();
            format!("fn({}) -> {}", names.join(", "), universe.render(*ret))
        },
    }
}

/// One field's type expression, its atom and trait references by raw
/// index.
fn render_field(field: &ds_check::Field) -> String {
    format!("{}: {:?}", field.name.clone().unwrap_or_default(), field.ty)
}

/// Two fully independent passes through the run path's entry assign
/// identical ids everywhere.
#[test]
fn two_independent_passes_assign_identical_ids() {
    let first = one_pass();
    let second = one_pass();
    assert_eq!(first, second);
}
