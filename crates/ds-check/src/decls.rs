//! Declaration pass: AST items into universe atoms, trait instances, and
//! AST-shaped signature tables (B1, B3, B6, G3).
//!
//! Collection is two-phase over the whole package (G3): every type-level
//! name is reserved first (tags, structs, enums, aliases, traits), then
//! shapes are filled, so intra- and cross-file declaration cycles resolve
//! by construction. The universe carries what its set algebra can express:
//! non-generic struct/enum shapes as solved field/case signatures (witness
//! fidelity (B6)) and nominal identities for everything. What the algebra
//! cannot express stays AST-shaped here and is solved per use by the
//! checker (crate::type_of) against the use-site's scopes:
//!
//! * generic shapes (`struct stack[t] { items: list[t] }`) — element types contain applications of
//!   type variables, which have no [`crate::expr::TypeExpr`] form; the checker solves the written
//!   fields with its own variable substitution;
//! * function signatures — solved per call site, with the callee's generic parameters matched
//!   structurally against the argument types (one-body generics, the M2 half of (H3));
//! * instance heads of generic impls — minted into the [`crate::instance::InstanceEnv`] as solved
//!   singletons over freshly minted variable atoms, so coherence checking (B6) runs unchanged.
//!
//! Instance-head resolution under substitution (`shows for list[int]`
//! because `shows for int` exists) is not here: lookup stays
//! containment-only, and [`crate::instance_solve`] owns the unification
//! and bound-recursion half at call sites.
//!
//! Coherence (B6) runs per declaration, then once more over the closed
//! instance set after the last impl declares: a bound-providing impl
//! landing after two generic impls it intersects can be proven by no
//! declaration-time probe, so the closure pass re-solves the whole
//! environment and reports the first overlap it finds.
//! The `~Copyable` marker bound on generic parameters (C1) is carried on
//! the parameter and otherwise unchecked here; the affine pass owns it at
//! M4.

use std::sync::Arc;

use ds_diag::{Diagnostic, ErrorCode, Span};
use ds_parse::{
    Ast, EnumPayload, Expr, FieldDecl, FnDecl, Ident, ImplDecl, Item, ItemKind, LocalDecl,
    ParamMode, Pattern, StructMember, TraitMember, Type, TypeParam,
};
use rustc_hash::FxHashMap;

use crate::{
    TypeExpr,
    atom::{Case, Field, TraitId, TypeId},
    id_table::IdTable,
    instance::{Condition, InherentBounds, InstanceDecl, InstanceEnv, InstanceId},
    resolve::{DefId, MethodId, MethodOwner, Package},
    universe::{Ty, Universe},
};

/// One declared generic parameter, AST-shaped.
#[derive(Clone, Debug)]
pub struct GenericParam {
    /// Parameter name.
    pub name: String,
    /// Span of the name.
    pub span: Span,
    /// Declared bound (`t: json`), as written; `~Copyable` markers are the
    /// affine layer's (C1, M4).
    pub bound: Option<Type>,
}

impl GenericParam {
    fn from(param: &TypeParam) -> Self {
        Self {
            name: param.name.name.clone(),
            span: param.name.span,
            bound: param.bound.clone(),
        }
    }
}

/// One parameter of a stored signature, as written.
#[derive(Clone, Debug)]
pub struct ParamSig {
    /// Parameter name.
    pub name: String,
    /// Span of the name.
    pub span: Span,
    /// Passing mode (C2); the affine pass reads it to decide which call
    /// arguments consume their source binding (C1).
    pub mode: ParamMode,
    /// Written type; absent for inferred lambda parameters.
    pub ty: Option<Type>,
    /// Literal-only default (F7), as written.
    pub default: Option<Expr>,
}

/// A function signature as written: generic parameters, parameter types,
/// and the return type. The checker solves these per call site against the
/// call's argument types; nothing here is pre-solved.
#[derive(Clone, Debug, Default)]
pub struct FnSig {
    /// Declared generic parameters, in order.
    pub generics: Vec<GenericParam>,
    /// Parameters, in order.
    pub params: Vec<ParamSig>,
    /// The written return type; absent means inferred or unit.
    pub ret: Option<Type>,
}

impl FnSig {
    fn of(f: &FnDecl) -> Self {
        Self {
            generics: f.generics.iter().map(GenericParam::from).collect(),
            params: f.params.iter().map(ParamSig::from).collect(),
            ret: f.ret.clone(),
        }
    }

    fn of_lambda(lambda: &Expr) -> Option<Self> {
        let Expr::Lambda {
            generics,
            params,
            ret,
            ..
        } = lambda
        else {
            return None;
        };
        Some(Self {
            generics: generics.iter().map(GenericParam::from).collect(),
            params: params.iter().map(ParamSig::from).collect(),
            ret: ret.clone(),
        })
    }
}

impl ParamSig {
    fn from(p: &ds_parse::Param) -> Self {
        Self {
            name: p.name.name.clone(),
            span: p.name.span,
            mode: p.mode,
            ty: p.ty.clone(),
            default: p.default.clone(),
        }
    }
}

/// A struct's declared shape, AST-shaped: generic parameter names plus
/// named fields as written, solved per use by the checker.
#[derive(Clone, Debug, Default)]
pub struct StructShape {
    /// Generic parameter names, in order (substituted by name at use).
    pub params: Vec<GenericParam>,
    /// Named fields, in declaration order.
    pub fields: Vec<FieldDecl>,
    /// Whether the declaration subtracts the copyability constraint
    /// (`~Copyable` among the supers) (C1).
    pub linear: bool,
}

/// An enum's declared shape, AST-shaped like [`StructShape`].
#[derive(Clone, Debug, Default)]
pub struct EnumShape {
    /// Generic parameter names, in order.
    pub params: Vec<GenericParam>,
    /// Cases in declaration order: name, span, payload.
    pub cases: Vec<(String, Span, EnumPayload)>,
    /// Whether the declaration subtracts the copyability constraint
    /// (`~Copyable` among the supers) (C1).
    pub linear: bool,
}

/// One case minted by a non-generic enum block (B3): the case's def maps
/// back to its enum (the case table) and carries the written payload.
#[derive(Clone, Debug)]
pub struct CaseDecl {
    /// The enum declaration the case belongs to.
    pub enum_def: DefId,
    /// The written payload; `None` marks a bare case.
    pub payload: EnumPayload,
}

/// The declaration tables one package checks against: the universe and
/// instance environment plus the AST-shaped shapes and signatures.
#[derive(Default)]
pub struct Decls {
    /// The atom and trait registry (B1).
    pub universe: Universe,
    /// The trait instance environment (B6).
    pub instances: InstanceEnv,
    /// The declaring impl item's span for every declared instance, keyed
    /// by the instance the environment allocated (per-branch instances of
    /// one written head all name their shared impl item). This is the tie a
    /// trait-method call site's solved instance needs to reach the impl's
    /// bodies; the environment itself stores only heads and conditions.
    pub instance_impls: IdTable<InstanceId, Span>,
    /// DefId to atom identity for tags, structs, and enums.
    pub atoms: IdTable<DefId, TypeId>,
    /// Atom identity back to the declaring DefId (field access on
    /// applications decomposes through this).
    pub defs_of_atoms: IdTable<TypeId, DefId>,
    /// DefId to trait identity.
    pub traits: IdTable<DefId, TraitId>,
    /// Alias bodies: DefId to generic parameters and the written type.
    pub aliases: IdTable<DefId, (Vec<GenericParam>, Type)>,
    /// Struct shapes for every struct def (generic and not).
    pub structs: IdTable<DefId, StructShape>,
    /// Enum shapes for every enum def.
    pub enums: IdTable<DefId, EnumShape>,
    /// Minted cases of non-generic enums, keyed by the case's def.
    pub cases: IdTable<DefId, CaseDecl>,
    /// Signatures of named functions, extern functions, and const-arrow
    /// function bindings. Shared behind [`Arc`] because call sites clone
    /// the handle per lookup and `ParamSig::default` carries an AST
    /// subtree — a per-call deep clone dominated the checker's hot path.
    pub fns: IdTable<DefId, Arc<FnSig>>,
    /// Trait member signatures keyed by their registered method id (F3).
    pub members: IdTable<MethodId, Arc<FnSig>>,
    /// Member names of each trait, for impl signature matching (B6).
    pub trait_members: IdTable<DefId, Vec<String>>,
    /// The member names of each trait carrying a default body: the
    /// members an impl may omit (the default body serves their calls).
    /// Every other member must arrive with a bodied impl member.
    pub trait_member_defaults: IdTable<DefId, Vec<String>>,
    /// Generic parameter names of each trait, in declaration order: an
    /// impl's trait reference substitutes them positionally when it
    /// spells its arguments (B6).
    pub trait_generics: IdTable<DefId, Vec<String>>,
    /// The solved trait-reference arguments of every trait impl that
    /// spells them, keyed by the impl item's span and positional over the
    /// trait's declared generics (B6): each argument is solved against
    /// the impl's own minted variable atoms, so a generic trait's default
    /// body knows which type its own variables substitute to at a site
    /// entering through this impl. Recorded only when every argument
    /// converts and solves; an impl spelling no arguments records nothing.
    pub trait_impl_args: FxHashMap<Span, Vec<Ty>>,
    /// Inherent impl-function signatures keyed by method id (F3).
    pub inherent: IdTable<MethodId, Arc<FnSig>>,
    /// The impl item each inherent impl function lives in, keyed by its
    /// registered method id: the mirror of [`Decls::instance_impls`] for
    /// inherent dispatch — a solved inherent call site reaches its impl's
    /// bodies through this tie, never through a package def.
    pub inherent_impls: IdTable<MethodId, Span>,
    /// The declared generic bounds of every generic inherent impl, keyed by
    /// the impl item's span: the inherent mirror of [`Decls::instance_impls`]
    /// plus its instances' conditions — an inherent impl declares no
    /// instance, so this record is the only place its bounds live. Call
    /// sites verify the bounds and stage their runtime evidence from it;
    /// the lowering pass reads it for the evidence-demand walk.
    pub inherent_impl_bounds: FxHashMap<Span, InherentBounds>,
    /// Top-level `const`/`var` initializers keyed by every name the
    /// binding pattern declares (lazy memoized typing (G3)).
    pub consts: IdTable<DefId, LocalDecl>,
    /// The solved return type of every impl-member function body (trait
    /// and inherent alike), keyed by the declaring `FnDecl`'s span: the
    /// written return solved against `Self`, or the body's own checked
    /// type when the declaration infers it. The lowering pass reads this
    /// when it plans impl bodies — a member function has no package
    /// `DefId`, so no other solved-signature table names it.
    pub impl_fn_rets: FxHashMap<Span, Ty>,
    /// Shared counter for minted type-variable tags (`$t#3`): instance
    /// declaration and body checking draw from it so the package universe
    /// never sees one name twice.
    pub var_counter: u32,
    /// The count of leading defs whose plain declaration tables arrived
    /// pre-built from the checked-ambient artifact: zero in a fresh build
    /// (the gates below never fire), and the reserve/shape arms skip a
    /// served def's minting and shape solving because the serve already
    /// installed those products. The hard tables (aliases, consts, the
    /// solved span-keyed records) build fresh for every def either way;
    /// the signature and instance tables slice two serves.
    pub served_defs: u32,
    /// The count of leading registered methods the checked-ambient
    /// artifact served signatures for: the inherent-registration arm
    /// skips a served method, its products already installed.
    pub served_methods: u32,
}

/// The declaration-side prefix a checked-ambient artifact serves: the
/// pre-built [`Decls`] (its universe replayed through the mint log, its
/// interned sets replayed in intern order, with [`Decls::served_defs`]
/// and [`Decls::served_methods`] set), the trait memberships the
/// skipped shape arms would have registered, and the shape-phase atom
/// mints the skipped impl arms would have performed — all keyed by
/// index against the spaces the replays reproduce. Memberships and
/// mints apply after the reserve pass — the mints continue the atom
/// space exactly where the declaring pass's impl arms did, after every
/// file's reserve mints — and before the shape pass, whose solving
/// reads them.
pub struct ServedDecls {
    /// The prefix's declaration tables; `served_defs` and
    /// `served_methods` must be set.
    pub decls: Decls,
    /// `(trait index, atom index)` membership records in any order.
    pub memberships: Vec<(TraitId, TypeId)>,
    /// The shape-phase atom mints, in mint order.
    pub mints: Vec<crate::universe::ReplayMint>,
    /// The count of leading trees the prefix covers: the rebuild below
    /// walks exactly these, and the caller aligns one served resolution
    /// table per covered tree ahead of the fresh ones.
    pub files: usize,
}

/// Builds the declaration tables over every collected file of one package,
/// in file order, returning the pass's diagnostics (instance-head
/// rejections with witnesses). Name-resolution diagnostics are the
/// resolver's; an unknown name here simply skips the dependent shape.
#[must_use]
pub fn build<T>(
    package: &Package,
    asts: &[T],
    resolutions: &[Resolutions],
) -> (Decls, Vec<Diagnostic>)
where
    T: std::borrow::Borrow<Ast>,
{
    build_over(package, asts, resolutions, None)
}

/// [`build`](self::build) over a checked-ambient prefix: the pass starts
/// from the served [`Decls`] instead of an empty one, the reserve and
/// shape arms skip every served def's minting and shape solving (the
/// serve installed those products), the served member's own impl items
/// skip their instance declarations (the artifact's rows are installed),
/// the served trait memberships and shape-phase mints apply between the
/// reserve pass (which mints every file's def-named atoms first, so the
/// mints continue the atom space where the declaring pass's impl arms
/// did) and the shape pass (whose solving reads them), and every table
/// the serve carries no rows for — aliases, consts, the solved
/// span-keyed records — builds fresh over all files as in [`build`].
#[must_use]
pub fn build_over<T>(
    package: &Package,
    asts: &[T],
    resolutions: &[Resolutions],
    served: Option<ServedDecls>,
) -> (Decls, Vec<Diagnostic>)
where
    T: std::borrow::Borrow<Ast>,
{
    let _ = resolutions;
    let trees: Vec<&Ast> = asts.iter().map(std::borrow::Borrow::borrow).collect();
    let served_files = served.as_ref().map_or(0, |s| s.files);
    assert!(
        served_files <= trees.len(),
        "a served prefix cannot cover more trees than the caller passed"
    );
    let (mut decls, memberships, mints) = match served {
        Some(served) => (served.decls, served.memberships, served.mints),
        None => (Decls::default(), Vec::new(), Vec::new()),
    };
    let mut diags = Vec::new();
    for ast in &trees {
        for block in &ast.blocks {
            for item in &block.items {
                reserve_item(package, item, &mut decls);
            }
        }
    }
    for (trait_id, atom) in memberships {
        let _ = decls.universe.impl_trait(trait_id, atom);
    }
    // The replay's failures are ignored like the memberships' above: the
    // artifact's bytes are drift-gate-pinned, so a replay that cannot
    // land is a corrupt artifact (a generation-side failure), never a
    // run-path condition to diagnose.
    for mint in &mints {
        let _ = decls.universe.replay_mint(mint);
    }
    for (index, ast) in trees.iter().enumerate() {
        let served_item = index < served_files;
        for block in &ast.blocks {
            for item in &block.items {
                shape_item(package, item, &mut decls, &mut diags, served_item);
            }
        }
    }
    rebuild_solved_records(&trees[..served_files], package, &mut decls);
    package_coherence(&mut decls, &mut diags);
    (decls, diags)
}

/// Rebuilds the solved span-keyed records of the served prefix's impl
/// items — the trait-reference arguments and the generic inherent
/// bounds — over the served trees and the replayed universe. The serve
/// skips these items' instance declarations (the artifact's rows are
/// installed), and no served table carries the records, so the
/// declaration arms' own derivations replay here: each generic
/// parameter maps to the variable atom the mint log replayed, its
/// unique name reconstructed through the counter sequence the
/// declaring pass minted under (the served prefix is one leading run
/// of files, so the sequence restarts where that pass started), and
/// every solve lands on the handle the set log's intern-order replay
/// already answers. A written expression that names anything but a
/// minted variable or a declared atom records nothing — solving it
/// could read a mid-growth state the replay cannot reproduce, so the
/// artifact's clean generation refuses such a prefix outright (the
/// drift gate's served-vs-fresh record comparison).
fn rebuild_solved_records(trees: &[&Ast], package: &Package, decls: &mut Decls) {
    let mut var_seq = 0u32;
    for tree in trees {
        for block in &tree.blocks {
            for item in &block.items {
                let ItemKind::Impl(imp) = &item.kind else {
                    continue;
                };
                let mut vars = FxHashMap::default();
                // The trait arm mints only once the referenced trait
                // resolves (the declaring pass returns before the mint
                // otherwise), and records its arguments only when the
                // head converted — the same control flow, mirrored.
                let record = if let Some(trait_ref) = &imp.trait_ref {
                    let [trait_only] = trait_ref.path.segments.as_slice() else {
                        continue;
                    };
                    let Some(owner) = package.lookup_type(&trait_only.name) else {
                        continue;
                    };
                    if !decls.traits.contains_key(&owner) {
                        continue;
                    }
                    for param in &imp.generics {
                        var_seq += 1;
                        minted_var(&param.name.name, var_seq, decls, &mut vars);
                    }
                    if conv_node(package, &imp.target, &vars, decls).is_none() {
                        continue;
                    }
                    if trait_ref.args.is_empty()
                        || !trait_ref
                            .args
                            .iter()
                            .all(|arg| names_atoms_only(package, arg, &vars, decls))
                    {
                        continue;
                    }
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
                    solved_all.then_some(Record::Args(solved_args))
                // The inherent arm mints only for a generic impl and
                // records only when the head and bounds solve.
                } else {
                    if imp.generics.is_empty() {
                        continue;
                    }
                    for param in &imp.generics {
                        var_seq += 1;
                        minted_var(&param.name.name, var_seq, decls, &mut vars);
                    }
                    if !names_atoms_only(package, &imp.target, &vars, decls) {
                        continue;
                    }
                    let Some(head_expr) = conv_node(package, &imp.target, &vars, decls) else {
                        continue;
                    };
                    let Ok(head) = decls.universe.solve(&head_expr) else {
                        continue;
                    };
                    let mut conditions = Vec::new();
                    for param in &imp.generics {
                        let Some(bound) = &param.bound else { continue };
                        let Some(bound_trait) = trait_of_type(package, bound, decls) else {
                            continue;
                        };
                        let Some(&var) = vars.get(&param.name.name) else {
                            continue;
                        };
                        if let Ok(ty) = decls.universe.atom_ty(var) {
                            conditions.push(Condition::new(bound_trait, ty));
                        }
                    }
                    (!conditions.is_empty()).then_some(Record::Inherent { head, conditions })
                };
                match record {
                    Some(Record::Args(args)) => {
                        decls.trait_impl_args.insert(item.span, args);
                    },
                    Some(Record::Inherent { head, conditions }) => {
                        decls
                            .inherent_impl_bounds
                            .insert(item.span, InherentBounds { head, conditions });
                    },
                    None => {},
                }
            }
        }
    }
}

/// What one rebuilt impl item records.
enum Record {
    /// The solved trait-reference arguments.
    Args(Vec<Ty>),
    /// The solved inherent head and its bounds.
    Inherent {
        /// The solved head pattern.
        head: Ty,
        /// One condition per declared bound.
        conditions: Vec<Condition<Ty>>,
    },
}

/// Maps one impl generic parameter to the variable atom the mint log
/// replayed, through the unique name the declaring pass minted it
/// under — `#counter` over the parameter's name, the counter this
/// rebuild reconstructs. A name the replay does not carry leaves the
/// parameter unmapped, as a mint that failed left it.
fn minted_var(name: &str, seq: u32, decls: &Decls, vars: &mut FxHashMap<String, TypeId>) {
    let unique = format!("${name}#{seq}");
    if let Some(var) = decls.universe.atom_by_name(&unique) {
        vars.insert(name.to_owned(), var);
    }
}

/// Whether a written type the rebuild solves names only minted
/// variables and declared atoms, positionally: the expressions whose
/// solving a later declaration cannot widen, so the replay answers
/// exactly what the declaring pass solved. A trait spelled as a set,
/// an alias, a negation, and every other non-atom form name state the
/// replay does not carry.
fn names_atoms_only(
    package: &Package,
    ty: &Type,
    vars: &FxHashMap<String, TypeId>,
    decls: &Decls,
) -> bool {
    use ds_parse::Type as Written;
    match ty {
        Written::Path { path, args, .. } => {
            let [only] = path.segments.as_slice() else {
                return false;
            };
            let known = vars.contains_key(&only.name)
                || package
                    .lookup_type(&only.name)
                    .is_some_and(|def| decls.atoms.get(&def).is_some());
            known
                && args
                    .iter()
                    .all(|arg| names_atoms_only(package, arg, vars, decls))
        },
        Written::Optional { inner, .. } | Written::Paren { inner, .. } => {
            names_atoms_only(package, inner, vars, decls)
        },
        Written::TypeKw(_) | Written::Unit(_) => true,
        Written::Union { left, right, .. } => {
            names_atoms_only(package, left, vars, decls)
                && names_atoms_only(package, right, vars, decls)
        },
        Written::Tuple { items, .. } => items
            .iter()
            .all(|item| names_atoms_only(package, item, vars, decls)),
        Written::Intersect { .. }
        | Written::Difference { .. }
        | Written::Negation { .. }
        | Written::Fn { .. }
        | Written::Ptr { .. }
        | Written::Foreign { .. } => false,
    }
}

/// Whether a def's plain declaration tables arrived pre-built from the
/// checked-ambient artifact: the minting arms skip it, its products
/// already installed by the serve. A user item duplicating a served name
/// resolves to the served def and skips too — the fresh path skips it as
/// well (the duplicate mint fails), so the outcome is the same.
fn is_served(decls: &Decls, def: DefId) -> bool {
    def.index() < decls.served_defs
}

/// The package-level coherence pass: one closure run over the instance
/// environment after every impl has declared (B6). A declaration-time
/// probe proves a generic-generic pair only through bounds already
/// inhabited at that moment, so a bound-providing impl declared after
/// two generic impls it intersects hides the pair from every
/// per-declaration probe; re-solving the closed set surfaces it here.
/// First-error-only is the accepted tradeoff — per-declaration reporting
/// stays complete, this pass catches only what declaration order hid.
fn package_coherence(decls: &mut Decls, diags: &mut Vec<Diagnostic>) {
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

use crate::resolve::Resolutions;

/// The definition an item declares, in its kind's namespace (F9).
fn item_def(package: &Package, item: &Item) -> Option<DefId> {
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
        package.lookup_type(name)
    } else {
        package.lookup_value(name)
    }
}

fn reserve_item(package: &Package, item: &Item, decls: &mut Decls) {
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
    let Some(def) = item_def(package, item) else {
        return; // a duplicate definition was reported at collection
    };
    match &item.kind {
        ItemKind::Tag(t) => {
            if is_served(decls, def) {
                return;
            }
            if let Some(id) = declare_unit_name(decls, &t.name.name) {
                decls.atoms.insert(def, id);
                decls.defs_of_atoms.insert(id, def);
            }
        },
        ItemKind::Struct(s) => {
            if is_served(decls, def) {
                return;
            }
            if let Ok(id) = decls.universe.declare_struct(&s.name.name) {
                decls.atoms.insert(def, id);
                decls.defs_of_atoms.insert(id, def);
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
            if is_served(decls, def) {
                return;
            }
            let generic = !e.generics.is_empty();
            // A non-generic enum mints one atom per case into the tag
            // namespace [B3] (the enum name then names their union); a
            // generic enum keeps the opaque nominal atom until
            // substitution over minted case shapes lands.
            let declared = if generic {
                decls.universe.declare_enum(&e.name.name).map(|id| {
                    decls.atoms.insert(def, id);
                    decls.defs_of_atoms.insert(id, def);
                })
            } else {
                Ok(())
            };
            if declared.is_ok() {
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
            if !generic {
                let linear = e.supers.iter().any(linear_marker);
                for case in &e.cases {
                    // A duplicate case name keeps the first def; the
                    // collection pass already reported both spans. The
                    // def's name span proves this item owns the def.
                    let Some(case_def) = package
                        .lookup_type(&case.name.name)
                        .filter(|&d| package.def(d).span == case.name.span)
                    else {
                        continue;
                    };
                    let atom = match &case.payload {
                        EnumPayload::None => declare_unit_name(decls, &case.name.name),
                        EnumPayload::Tuple(_) | EnumPayload::Struct(_) => {
                            decls.universe.declare_struct(&case.name.name).ok()
                        },
                    };
                    if let Some(id) = atom {
                        decls.atoms.insert(case_def, id);
                        decls.defs_of_atoms.insert(id, case_def);
                    }
                    if let EnumPayload::Struct(fields) = &case.payload {
                        // A named-payload case carries the struct shape
                        // field access and struct patterns read [B3].
                        decls.structs.insert(case_def, StructShape {
                            params: Vec::new(),
                            fields: fields.clone(),
                            linear,
                        });
                    }
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
            if is_served(decls, def) {
                return;
            }
            if let Ok(id) = decls.universe.declare_trait(&t.name.name) {
                decls.traits.insert(def, id);
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
        },
        ItemKind::Fn(f) | ItemKind::ExternFn(f) => {
            if is_served(decls, def) {
                return;
            }
            decls.fns.insert(def, Arc::new(FnSig::of(f)));
        },
        ItemKind::Import(_) | ItemKind::Local(_) | ItemKind::Impl(_) | ItemKind::Platform(_) => {},
    }
}

/// Inherent impl functions register their signatures against the method
/// registration their own span names (F3) — the same span discipline as
/// the effects walk. Trait impls register nothing here: the trait's
/// member signatures already provide the candidates, and the instance
/// declaration is the shape pass's work. Call sites check against each
/// impl's own signature; a same-name second inherent impl never gets a
/// registration (collection reports the duplicate and keeps the first).
/// A served method's signature and impl-site arrived with the artifact.
fn reserve_impl_fns(package: &Package, imp: &ImplDecl, impl_site: Span, decls: &mut Decls) {
    if imp.trait_ref.is_some() {
        return;
    }
    for f in &imp.fns {
        if let Some(method) = package
            .method_candidates(&f.name.name)
            .iter()
            .copied()
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

fn def_kind_is_binding(package: &Package, def: DefId) -> bool {
    matches!(
        package.def(def).kind,
        crate::resolve::DefKind::Const | crate::resolve::DefKind::Var
    )
}

/// One top-level binding: every binder the pattern declares maps back to
/// this initializer (G3) — a destructuring pattern declares many defs, so
/// the item's own def is not the gate (a tuple pattern names no single
/// def). The const-of-lambda registration is the single-name shape only:
/// a lambda value binds one name.
fn reserve_local(package: &Package, local: &LocalDecl, decls: &mut Decls) {
    for binder in pattern_names(&local.pat) {
        if let Some(def) = package.lookup_value(&binder) {
            if def_kind_is_binding(package, def) {
                decls.consts.insert(def, local.clone());
            }
        }
    }
    let Pattern::Binding { name, .. } = &local.pat else {
        return;
    };
    if let Some(mut sig) = FnSig::of_lambda(&local.init) {
        // A const-arrow function binding: the signature is the lambda's
        // own annotations; a written binding annotation supplies the
        // return where the lambda omits one.
        if sig.ret.is_none() {
            sig.ret.clone_from(&local.ty);
        }
        if let Some(def) = package.lookup_value(&name.name) {
            decls.fns.insert(def, Arc::new(sig));
        }
    }
}

fn struct_fields_ast(members: &[StructMember]) -> Vec<FieldDecl> {
    members
        .iter()
        .filter_map(|m| match m {
            StructMember::Field(f) => Some(f.clone()),
            StructMember::Deinit { .. } => None,
        })
        .collect()
}

/// `served_item` marks an item of the checked-ambient member itself: its
/// impl items' instance declarations arrived installed with the artifact
/// (rows, spans, memberships, mints), so the impl arm skips.
fn shape_item(
    package: &Package,
    item: &Item,
    decls: &mut Decls,
    diags: &mut Vec<Diagnostic>,
    served_item: bool,
) {
    match &item.kind {
        ItemKind::Struct(s) => {
            let Some(def) = package.lookup_type(&s.name.name) else {
                return
            };
            if is_served(decls, def) {
                return;
            }
            let Some(&atom) = decls.atoms.get(&def) else {
                return
            };
            register_supers(package, &s.supers, atom, decls);
            if s.generics.is_empty() && s.has_body {
                let fields = universe_fields(package, &struct_fields_ast(&s.members), decls);
                let _ = decls.universe.set_struct_fields(atom, fields);
            }
        },
        ItemKind::Enum(e) => {
            let Some(def) = package
                .lookup_type(&e.name.name)
                .filter(|&d| package.def(d).span == e.name.span)
            else {
                return
            };
            if is_served(decls, def) {
                return;
            }
            if e.generics.is_empty() {
                // Minted cases: each case's atom takes its payload as the
                // struct field signature (positional slots for tuple
                // payloads), so a minted payload case IS a struct atom for
                // the set algebra and the lowerer [B3].
                for case in &e.cases {
                    let Some(case_def) = package
                        .lookup_type(&case.name.name)
                        .filter(|&d| package.def(d).span == case.name.span)
                    else {
                        continue
                    };
                    let Some(&atom) = decls.atoms.get(&case_def) else {
                        continue
                    };
                    register_supers(package, &e.supers, atom, decls);
                    let fields = match &case.payload {
                        EnumPayload::None => Vec::new(),
                        EnumPayload::Tuple(types) => types
                            .iter()
                            .filter_map(|t| conv_expr(package, t, decls))
                            .map(Field::positional)
                            .collect(),
                        EnumPayload::Struct(fields) => universe_fields(package, fields, decls),
                    };
                    if !fields.is_empty() {
                        let _ = decls.universe.set_struct_fields(atom, fields);
                    }
                }
            } else {
                // A generic enum keeps the opaque atom with its case
                // signatures until case minting grows substitution.
                let Some(&atom) = decls.atoms.get(&def) else {
                    return
                };
                register_supers(package, &e.supers, atom, decls);
                let mut cases = Vec::new();
                for case in &e.cases {
                    let fields = match &case.payload {
                        EnumPayload::None => Vec::new(),
                        EnumPayload::Tuple(types) => types
                            .iter()
                            .filter_map(|t| conv_expr(package, t, decls))
                            .map(Field::positional)
                            .collect(),
                        EnumPayload::Struct(fields) => universe_fields(package, fields, decls),
                    };
                    cases.push(Case::new(&case.name.name, fields));
                }
                let _ = decls.universe.set_enum_cases(atom, cases);
            }
        },
        ItemKind::Impl(imp) => {
            if !served_item {
                declare_instance(package, item, imp, decls, diags);
            }
        },
        ItemKind::Tag(_)
        | ItemKind::TypeAlias(_)
        | ItemKind::Trait(_)
        | ItemKind::Fn(_)
        | ItemKind::ExternFn(_)
        | ItemKind::Local(_)
        | ItemKind::Import(_)
        | ItemKind::Platform(_) => {},
    }
}

/// Whether a written type is the `~Copyable` marker (C1): the negation of
/// the fixed `Copyable` trait name. The marker subtracts the default
/// copyability constraint, so a declaration carrying it is linear.
pub(crate) fn linear_marker(ty: &Type) -> bool {
    let Type::Negation { inner, .. } = ty else {
        return false;
    };
    let Type::Path { path, .. } = inner.as_ref() else {
        return false;
    };
    path.segments.len() == 1 && path.segments[0].name == "Copyable"
}

/// `struct dog: animal` nominal subtyping: every non-negated supertrait
/// path registers the atom as a trait member (B1).
fn register_supers(package: &Package, supers: &[Type], atom: TypeId, decls: &mut Decls) {
    for super_ty in supers {
        if let Some(trait_id) = trait_of_type(package, super_ty, decls) {
            let _ = decls.universe.impl_trait(trait_id, atom);
        }
    }
}

fn trait_of_type(package: &Package, ty: &Type, decls: &Decls) -> Option<TraitId> {
    let Type::Path { path, .. } = ty else {
        return None;
    };
    let [only] = path.segments.as_slice() else {
        return None;
    };
    let def = package.lookup_type(&only.name)?;
    decls.traits.get(&def).copied()
}

/// Converts annotated struct/enum fields into solved universe fields; a
/// field whose type does not live in the [`TypeExpr`] world (function
/// types, foreign types) contributes nothing — the checker works from the
/// AST shape regardless, only witnesses degrade.
fn universe_fields(package: &Package, fields: &[FieldDecl], decls: &mut Decls) -> Vec<Field> {
    let mut out = Vec::with_capacity(fields.len());
    for field in fields {
        if let Some(expr) = conv_expr(package, &field.ty, decls) {
            out.push(Field::named(&field.name.name, expr));
        }
    }
    out
}

/// Converts a written type into the [`TypeExpr`] world for non-generic
/// universe shapes; `None` marks a type the core cannot express.
fn conv_expr(package: &Package, ty: &Type, decls: &mut Decls) -> Option<TypeExpr> {
    Some(match ty {
        Type::Path { path, args, .. } => {
            let [only] = path.segments.as_slice() else {
                return None
            };
            if args.is_empty() {
                path_expr(package, only, decls)?
            } else {
                let ctor = atom_of_path(package, only, decls)?;
                let args = conv_args(package, args, &FxHashMap::default(), decls)?;
                TypeExpr::Atom(decls.universe.apply_atom(ctor, &args).ok()?)
            }
        },
        Type::Optional { inner, .. } => {
            conv_expr(package, inner, decls)?.union(TypeId::NONE.into())
        },
        Type::Negation { inner, .. } => TypeExpr::Not(trait_of_type(package, inner, decls)?),
        Type::Paren { inner, .. } => conv_expr(package, inner, decls)?,
        Type::Union { left, right, .. } => {
            conv_expr(package, left, decls)?.union(conv_expr(package, right, decls)?)
        },
        Type::Intersect { left, right, .. } => {
            conv_expr(package, left, decls)?.inter(conv_expr(package, right, decls)?)
        },
        Type::Difference { left, right, .. } => {
            conv_expr(package, left, decls)?.diff(conv_expr(package, right, decls)?)
        },
        Type::Tuple { items, .. } => {
            let mut elements = Vec::new();
            for item in items {
                elements.push(conv_expr(package, item, decls)?);
            }
            TypeExpr::Tuple(elements)
        },
        Type::Unit(_) => TypeExpr::Tuple(Vec::new()),
        // the comptime type-value atom (G1, G2): the `type` keyword's
        // conversion form, so a `comptime t: type` parameter's solved
        // binder type is the builtin atom
        Type::TypeKw(_) => TypeExpr::Atom(TypeId::TYPE),
        Type::Fn { .. } | Type::Ptr { .. } | Type::Foreign { .. } => return None,
    })
}

/// Solves written application arguments in conversion contexts.
fn conv_args(
    package: &Package,
    args: &[Type],
    vars: &FxHashMap<String, TypeId>,
    decls: &mut Decls,
) -> Option<Vec<Ty>> {
    let _ = vars;
    let mut out = Vec::with_capacity(args.len());
    for arg in args {
        let expr = conv_expr(package, arg, decls)?;
        out.push(decls.universe.solve(&expr).ok()?);
    }
    Some(out)
}

/// A single-segment type path in expression position: a declared atom, a
/// trait as its member set, or a non-generic alias expanded to its full
/// written union — never a set's single member.
fn path_expr(package: &Package, name: &Ident, decls: &mut Decls) -> Option<TypeExpr> {
    let def = package.lookup_type(&name.name)?;
    if let Some(&atom) = decls.atoms.get(&def) {
        return Some(TypeExpr::Atom(atom));
    }
    if let Some(&trait_id) = decls.traits.get(&def) {
        return Some(TypeExpr::Trait(trait_id));
    }
    // A non-generic enum names the union of its minted case atoms [B3].
    if let Some(shape) = decls.enums.get(&def) {
        if shape.params.is_empty() {
            return enum_case_union(package, &shape.cases, decls);
        }
        return None;
    }
    if let Some((params, body)) = decls.aliases.get(&def).cloned() {
        if params.is_empty() {
            return conv_expr(package, &body, decls);
        }
    }
    None
}

/// The union of one enum's minted case atoms; `None` when a case did
/// not mint (or the enum is empty, which is uninhabited either way).
fn enum_case_union(
    package: &Package,
    cases: &[(String, Span, EnumPayload)],
    decls: &Decls,
) -> Option<TypeExpr> {
    let mut union: Option<TypeExpr> = None;
    for (name, ..) in cases {
        let atom = package
            .lookup_type(name)
            .and_then(|def| decls.atoms.get(&def).copied())?;
        let expr = TypeExpr::Atom(atom);
        union = Some(match union {
            Some(acc) => acc.union(expr),
            None => expr,
        });
    }
    union
}

/// The atom a single-segment type path names as a CONSTRUCTOR: declared
/// atoms only (aliases and traits are sets, not constructors).
fn atom_of_path(package: &Package, name: &Ident, decls: &mut Decls) -> Option<TypeId> {
    let def = package.lookup_type(&name.name)?;
    decls.atoms.get(&def).copied()
}

/// Mints the unit atom for one tag (or unit enum case). The universe
/// predeclares `none` as its distinguished unit atom (B4) with no def, so
/// a `tag none` (or `enum e { none }`) restating the name maps onto the
/// predeclared atom instead of minting a second — a skipped mint would
/// silently orphan the def and every written `none` type or value would
/// stop solving. Any other refused name is a collection-level duplicate
/// that was already reported; the def keeps the first item's atom.
fn declare_unit_name(decls: &mut Decls, name: &str) -> Option<TypeId> {
    match decls.universe.declare_tag(name) {
        Ok(id) => Some(id),
        Err(_) if name == "none" => Some(TypeId::NONE),
        Err(_) => None,
    }
}

/// Converts one written type into a solved singleton [`TypeExpr`] for
/// instance heads and conditions: unions stay unions in [`conv_head`],
/// everything else becomes its solved atom set.
fn conv_node(
    package: &Package,
    ty: &Type,
    vars: &FxHashMap<String, TypeId>,
    decls: &mut Decls,
) -> Option<TypeExpr> {
    Some(match ty {
        Type::Union { left, right, .. } => {
            conv_node(package, left, vars, decls)?.union(conv_node(package, right, vars, decls)?)
        },
        Type::Paren { inner, .. } => conv_node(package, inner, vars, decls)?,
        other => {
            // Everything else: solve to a Ty, then re-express as a union of
            // atoms the TypeExpr world can carry.
            let solved = solve_written(package, other, vars, decls)?;
            let mut expr = None::<TypeExpr>;
            for atom in decls.universe.members(solved) {
                expr = Some(match expr {
                    None => TypeExpr::atom(*atom),
                    Some(e) => e.union((*atom).into()),
                });
            }
            expr?
        },
    })
}

/// Solves one written type (path, optional, tuple, unit) to a [`Ty`] in a
/// conversion context; type variables resolve to their minted atoms.
fn solve_written(
    package: &Package,
    ty: &Type,
    vars: &FxHashMap<String, TypeId>,
    decls: &mut Decls,
) -> Option<Ty> {
    Some(match ty {
        Type::Path { path, args, .. } => {
            let [only] = path.segments.as_slice() else {
                return None
            };
            if let Some(&var) = vars.get(&only.name) {
                decls.universe.atom_ty(var).ok()?
            } else if args.is_empty() {
                if let Some(atom) = atom_of_path(package, only, decls) {
                    decls.universe.atom_ty(atom).ok()?
                } else {
                    // A trait as a set, or a non-generic alias: solve its
                    // written expression in full.
                    path_expr(package, only, decls)
                        .and_then(|expr| decls.universe.solve(&expr).ok())?
                }
            } else {
                let ctor = atom_of_path(package, only, decls)?;
                let mut arg_tys = Vec::new();
                for arg in args {
                    arg_tys.push(solve_written(package, arg, vars, decls)?);
                }
                let apply = decls.universe.apply_atom(ctor, &arg_tys).ok()?;
                decls.universe.atom_ty(apply).ok()?
            }
        },
        Type::Optional { inner, .. } => {
            let inner = solve_written(package, inner, vars, decls)?;
            decls.universe.optional(inner)
        },
        Type::Paren { inner, .. } => solve_written(package, inner, vars, decls)?,
        Type::Tuple { items, .. } => {
            let mut elements = Vec::new();
            for item in items {
                elements.push(solve_written(package, item, vars, decls)?);
            }
            decls.universe.product(&elements).ok()?
        },
        Type::Unit(_) => decls.universe.unit_ty(),
        Type::Union { left, right, .. } => {
            let (a, b) = (
                solve_written(package, left, vars, decls)?,
                solve_written(package, right, vars, decls)?,
            );
            decls.universe.union(a, b)
        },
        Type::Intersect { .. }
        | Type::Difference { .. }
        | Type::Negation { .. }
        | Type::Fn { .. }
        | Type::Ptr { .. }
        | Type::Foreign { .. } => return None,
        // the comptime type-value atom (G1, G2), in the written-variable
        // conversion context
        Type::TypeKw(_) => decls.universe.atom_ty(TypeId::TYPE).ok()?,
    })
}

/// Records one generic inherent impl's declared bounds (F3): minted
/// variables, the solved head pattern, and one condition per declared
/// bound — the same shapes the trait arm builds, so the two impl forms
/// read through the same machinery downstream. An impl declaring no trait
/// bounds records nothing (a bare `[t]` bounds nothing, and calls on its
/// variable never check clean); a head the type core cannot solve records
/// nothing (the impl's own body checking reports the target).
fn declare_inherent_bounds(package: &Package, item: &Item, imp: &ImplDecl, decls: &mut Decls) {
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
        if let Ok(var) = decls.universe.declare_tag(&unique) {
            vars.insert(param.name.name.clone(), var);
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
        let Some(bound_trait) = trait_of_type(package, bound, decls) else {
            continue;
        };
        let Some(&var) = vars.get(&param.name.name) else {
            continue
        };
        if let Ok(ty) = decls.universe.atom_ty(var) {
            conditions.push(Condition::new(bound_trait, ty));
        }
    }
    if !conditions.is_empty() {
        decls
            .inherent_impl_bounds
            .insert(item.span, InherentBounds { head, conditions });
    }
}

/// Declares one impl's instances into the environment (B6): mints variable
/// atoms for the generic parameters, converts the head (written unions
/// decompose inside `declare`), attaches trait bounds as conditions, and
/// maps coherence failures onto witness-carrying diagnostics. A failed
/// declaration leaves the environment unchanged; the impl's methods are
/// still body-checked (the checker works from the AST).
fn declare_instance(
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
    let Some(owner) = package.lookup_type(&trait_only.name) else {
        return;
    };
    let Some(&trait_id) = decls.traits.get(&owner) else {
        return
    };

    let mut vars = FxHashMap::default();
    for param in &imp.generics {
        // A bare `$name` would collide with every other impl naming its
        // parameter the same (the universe is package-wide), so minted
        // variables carry the shared counter.
        decls.var_counter += 1;
        let unique = format!("${}#{}", param.name.name, decls.var_counter);
        if let Ok(var) = decls.universe.declare_tag(&unique) {
            vars.insert(param.name.name.clone(), var);
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
        let Some(bound_trait) = trait_of_type(package, bound, decls) else {
            continue;
        };
        if let Some(&var) = vars.get(&param.name.name) {
            conditions.push(Condition::new(bound_trait, TypeExpr::atom(var)));
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

    let decl = InstanceDecl::new(trait_id, head).with_conditions(conditions);
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
/// the intersecting earlier head, and the witness value type (B6); the
/// corpus's golden rejection pins this render.
fn instance_diag(
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

/// The registered method id of one trait member (F3).
fn member_id(package: &Package, trait_def: DefId, name: &str) -> Option<MethodId> {
    package
        .method_candidates(name)
        .iter()
        .copied()
        .find(|&id| package.method(id).owner == MethodOwner::Trait(trait_def))
}

/// One trait member's registered signature and declaring span (B6): the
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
fn pattern_names(pat: &Pattern) -> Vec<String> {
    crate::pattern::pattern_binders(pat)
        .into_iter()
        .map(|(_, name)| name.to_owned())
        .collect()
}

/// The declared name of an item, for package lookup. Locals never route
/// here: they register one entry per pattern binder (see `reserve_local`).
fn item_name(item: &Item) -> &str {
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

/// Source-faithful rendering of a written type, for diagnostics.
pub fn render_type(ty: &Type) -> String {
    match ty {
        Type::Path { path, args, .. } => {
            if args.is_empty() {
                path.text()
            } else {
                let args: Vec<String> = args.iter().map(render_type).collect();
                format!("{}[{}]", path.text(), args.join(", "))
            }
        },
        Type::Optional { inner, .. } => format!("{}?", render_type(inner)),
        Type::Negation { inner, .. } => format!("~{}", render_type(inner)),
        Type::Union { left, right, .. } => {
            format!("{} | {}", render_type(left), render_type(right))
        },
        Type::Intersect { left, right, .. } => {
            format!("{} & {}", render_type(left), render_type(right))
        },
        Type::Difference { left, right, .. } => {
            format!("{} - {}", render_type(left), render_type(right))
        },
        Type::Paren { inner, .. } => format!("({})", render_type(inner)),
        Type::Tuple { items, .. } => {
            let items: Vec<String> = items.iter().map(render_type).collect();
            format!("({})", items.join(", "))
        },
        Type::Unit(_) => "()".to_owned(),
        Type::Fn { params, ret, .. } => {
            let params: Vec<String> = params
                .iter()
                .map(|p| match &p.ty {
                    Some(ty) => format!("{}: {}", p.name.name, render_type(ty)),
                    None => p.name.name.clone(),
                })
                .collect();
            match ret {
                Some(ret) => format!("fn({}) -> {}", params.join(", "), render_type(ret)),
                None => format!("fn({})", params.join(", ")),
            }
        },
        Type::Ptr { mutable, inner, .. } => {
            format!(
                "*{} {}",
                if *mutable { "mut" } else { "const" },
                render_type(inner)
            )
        },
        Type::TypeKw(_) => "type".to_owned(),
        Type::Foreign { name, .. } => format!("c[\"{name}\"]"),
    }
}
