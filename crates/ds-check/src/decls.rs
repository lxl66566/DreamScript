//! Declaration pass: AST items into universe atoms, trait instances, and
//! AST-shaped signature tables.
//!
//! Collection is two-phase over the whole package: every type-level
//! name is reserved first (tags, structs, enums, aliases, traits), then
//! shapes are filled, so intra- and cross-file declaration cycles resolve
//! by construction. The universe carries what its set algebra can express:
//! non-generic struct/enum shapes as solved field/case signatures (witness
//! fidelity) and nominal identities for everything. What the algebra
//! cannot express stays AST-shaped here and is solved per use by the
//! checker (crate::type_of) against the use-site's scopes:
//!
//! * generic shapes (`struct stack[t] { items: list[t] }`) — element types contain applications of
//!   type variables, which have no [`crate::expr::TypeExpr`] form; the checker solves the written
//!   fields with its own variable substitution;
//! * function signatures — solved per call site, with the callee's generic parameters matched
//!   structurally against the argument types (one-body generics);
//! * instance heads of generic impls — minted into the [`crate::instance::InstanceEnv`] as solved
//!   singletons over freshly minted variable atoms, so coherence checking runs unchanged.
//!
//! Instance-head resolution under substitution (`shows for list[int]`
//! because `shows for int` exists) is not here: lookup stays
//! containment-only, and [`crate::instance_solve`] owns the unification
//! and bound-recursion half at call sites.
//!
//! Coherence runs per declaration, then once more over the closed
//! instance set after the last impl declares: a bound-providing impl
//! landing after two generic impls it intersects can be proven by no
//! declaration-time probe, so the closure pass re-solves the whole
//! environment and reports the first overlap it finds.
//! The `~Copyable` marker bound on generic parameters is carried on the
//! parameter and otherwise unchecked here; the affine pass owns it.

use std::sync::Arc;

use ds_diag::{Diagnostic, ErrorCode, Span};
use ds_parse::{
    Ast, Delim, EnumPayload, Expr, FieldDecl, FnDecl, Ident, ImplDecl, Item, ItemKind, LocalDecl,
    ParamMode, Pattern, StructMember, TraitMember, Type, TypeParam,
};
use rustc_hash::FxHashMap;

use crate::{
    TypeExpr,
    atom::{Case, Field, TraitId, TypeId},
    id_table::IdTable,
    instance::{Condition, InherentBounds, InstanceDecl, InstanceEnv, InstanceId},
    resolve::{DefId, DefKind, MethodId, MethodOwner, Package},
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
    /// affine layer's.
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
    /// Passing mode; the affine pass reads it to decide which call
    /// arguments consume their source binding.
    pub mode: ParamMode,
    /// Written type; absent for inferred lambda parameters.
    pub ty: Option<Type>,
    /// Literal-only default, as written.
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
    /// The extern's bind target: the `[artifacts]` name a bound
    /// extern declares, `None` on every other signature. Pure
    /// declaration data — the graph fold that resolves the name to an
    /// installed library is the lowerer's seam.
    pub bind: Option<String>,
}

impl FnSig {
    pub(crate) fn of(f: &FnDecl) -> Self {
        Self {
            generics: f.generics.iter().map(GenericParam::from).collect(),
            params: f.params.iter().map(ParamSig::from).collect(),
            ret: f.ret.clone(),
            bind: f.bind.as_ref().map(|bind| bind.name.clone()),
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
            bind: None,
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

/// One solved field of a user-declared generic struct's application:
/// the declared field's name and its type solved under the
/// application's arguments — the same substitution field access and
/// construction solve their sites with. The whole application's
/// signature rides [`Decls::apply_structs`].
#[derive(Clone, Debug)]
pub struct SolvedField {
    /// The declared field name.
    pub name: String,
    /// The field's type under the application's arguments.
    pub ty: Ty,
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
    /// (`~Copyable` among the supers).
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
    /// (`~Copyable` among the supers).
    pub linear: bool,
}

/// One case minted by a non-generic enum block: the case's def maps
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
    /// The atom and trait registry.
    pub universe: Universe,
    /// The file whose declarations the walk is minting or converting,
    /// `None` in the foreign installs and the serve replay: a tag
    /// label's atom resolves through the declaring FILE's shadow entry
    /// first, and only the local walks carry a file.
    pub current_file: Option<ds_diag::FileId>,
    /// The trait instance environment.
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
    /// Trait member signatures keyed by their registered method id.
    pub members: IdTable<MethodId, Arc<FnSig>>,
    /// Member names of each trait, for impl signature matching.
    pub trait_members: IdTable<DefId, Vec<String>>,
    /// The member names of each trait carrying a default body: the
    /// members an impl may omit (the default body serves their calls).
    /// Every other member must arrive with a bodied impl member.
    pub trait_member_defaults: IdTable<DefId, Vec<String>>,
    /// Generic parameter names of each trait, in declaration order: an
    /// impl's trait reference substitutes them positionally when it
    /// spells its arguments.
    pub trait_generics: IdTable<DefId, Vec<String>>,
    /// The solved trait-reference arguments of every trait impl that
    /// spells them, keyed by the impl item's span and positional over the
    /// trait's declared generics: each argument is solved against
    /// the impl's own minted variable atoms, so a generic trait's default
    /// body knows which type its own variables substitute to at a site
    /// entering through this impl. Recorded only when every argument
    /// converts and solves; an impl spelling no arguments records nothing.
    pub trait_impl_args: FxHashMap<Span, Vec<Ty>>,
    /// Inherent impl-function signatures keyed by method id.
    pub inherent: IdTable<MethodId, Arc<FnSig>>,
    /// The impl item each inherent impl function lives in, keyed by its
    /// registered method id: the mirror of [`Decls::instance_impls`] for
    /// inherent dispatch — a solved inherent call site reaches its impl's
    /// bodies through this tie, never through a package def.
    pub inherent_impls: IdTable<MethodId, Span>,
    /// The solved head pattern and declared bounds of every generic
    /// inherent impl, keyed by the impl item's span: the inherent mirror
    /// of [`Decls::instance_impls`] plus its instances' conditions — an
    /// inherent impl declares no instance, so this record is the only
    /// place its head and bounds live. A bounds-free generic impl records
    /// its head with no conditions (the head pattern is what a call
    /// site's signature substitution reads). Call sites verify the bounds
    /// and stage their runtime evidence from it; the lowering pass reads
    /// it for the evidence-demand walk.
    pub inherent_impl_bounds: FxHashMap<Span, InherentBounds>,
    /// Top-level `const`/`var` initializers keyed by every name the
    /// binding pattern declares (lazy memoized typing). Shared
    /// handles one row apiece, and the served prefix's rows arrive as
    /// one shared base (see [`consts_base`]): the ambient's bodied fns
    /// are const-of-lambda bindings, so a by-value row cloned per member
    /// compile deep-cloned every fn body — the reserve walk now borrows
    /// the base's rows read-only and only the compiled file's own
    /// bindings insert.
    pub consts: IdTable<DefId, Arc<LocalDecl>>,
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
    /// installed those products. The hard tables (aliases, the solved
    /// span-keyed records) build fresh for every def either way; the
    /// signature and instance tables slice two serves, and the const
    /// rows share the caller's parked base where one is installed.
    pub served_defs: u32,
    /// The count of leading registered methods the checked-ambient
    /// artifact served signatures for: the inherent-registration arm
    /// skips a served method, its products already installed.
    pub served_methods: u32,
    /// The linked-function facts of every import-bound def: the
    /// dep-list position and the linked module's function index a call
    /// site addresses through its pool FuncRef. The signature rides
    /// [`Decls::fns`] like any def's.
    pub foreign_links: IdTable<DefId, ForeignLink>,
    /// The literal fact of every import-bound const: the value a use
    /// site embeds as the literal a user would have written — the fact is
    /// the value, so no linked-module machinery serves it. The checked
    /// type mints from the fact's scalar atom at the use.
    pub foreign_consts: IdTable<DefId, crate::import::ConstFact>,
    /// The name index of every import-bound type whose fact minted:
    /// the def a type spelling resolves to when the package's own
    /// namespace does not declare the name. The resolver gates a
    /// consumer's own uses behind the file's import selections; this
    /// index serves the linked signatures' rebased spellings, which no
    /// resolution pass covers.
    pub foreign_type_names: FxHashMap<String, DefId>,
    /// The member links of every linked trait impl, keyed by the
    /// instance the declaration pass declared over the minted head: the
    /// dep-list position and each member body's module-table index a
    /// solved dispatch site addresses through its CALLG — the linked
    /// mirror of `instance_impls`, whose span tie no linked impl owns.
    pub linked_impls: IdTable<InstanceId, Arc<ForeignImplLink>>,
    /// The member links of every linked inherent impl, keyed by
    /// the method id the link pass registered: the dep-list position
    /// and the member body's module-table index a type-keyed dispatch
    /// site addresses through its plain call — the linked mirror of
    /// `inherent_impls`, whose span tie no linked impl owns.
    pub linked_inherent: IdTable<MethodId, ForeignLink>,
    /// The default-body links of every linked trait, keyed by the
    /// minted trait def: the dep-list position and each crossed default
    /// body's module-table index — the dispatch target a site whose
    /// solving impl spells no member addresses, the linked mirror of
    /// the plan's own default-body table.
    pub linked_trait_defaults: IdTable<DefId, Arc<ForeignImplLink>>,
    /// The solved field signature of every minted application of a
    /// user-declared generic struct: the declared fields' names with
    /// each field type solved under the application's arguments.
    /// Recorded once per application atom after the whole package
    /// resolves; the lowering pass shapes the application's values
    /// through it, and an instantiation derives the substituted
    /// application's entry from its source's.
    pub apply_structs: std::collections::HashMap<TypeId, std::rc::Rc<[SolvedField]>>,
}

impl Decls {
    /// The import-bound type a name spells, when the package's own type
    /// namespace does not declare it and a linked fact minted one.
    #[must_use]
    pub fn foreign_type(&self, name: &str) -> Option<DefId> {
        self.foreign_type_names.get(name).copied()
    }
}

/// Where one import-bound function's call site points: the linked
/// dep entry's position in the consuming module's recorded list and
/// the function's index inside the linked module's table.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ForeignLink {
    /// The linked dep entry's position in the consuming module's
    /// recorded dep list.
    pub position: u32,
    /// The function's index inside the linked module's table.
    pub func: u32,
}

/// Where one linked trait body's dispatch sites point: the linked
/// dep entry's position and each body's index inside the linked
/// module's table — keyed by the instance the consumer declared for an
/// impl's member bodies, or by the minted trait def for the trait's
/// crossed default bodies.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ForeignImplLink {
    /// The linked dep entry's position in the consuming module's
    /// recorded dep list.
    pub position: u32,
    /// The impl's member rows, one per bodied member.
    pub members: Vec<crate::import::ImplMemberFact>,
    /// The declaring package's identity (`name@version`): the link
    /// layering's package identity, the loser's name a shadowing
    /// warning or refusal spells.
    pub pkg: String,
    /// The declaring package's own artifact statically dispatches to
    /// this impl (a baked instance word names it), so the link cannot
    /// shadow it — the internal-exercise refusal's fact.
    pub exercised: bool,
}

impl ForeignImplLink {
    /// One member's module-table index, when the impl owns the member.
    #[must_use]
    pub fn member(&self, name: &str) -> Option<u32> {
        self.members
            .iter()
            .find(|member| member.name == name)
            .map(|member| member.func)
    }
}

/// Why a checked prefix's name-keyed rows refused to resolve over the
/// merged compile's universe: a name that does not resolve, a raw id the
/// merged walk did not reproduce inside the def-named identity range, an
/// intern landing off its recorded index, or a mint that could not
/// replay. The artifact is a pure cache, so a drift refusal is a cache
/// miss — the declaration build falls back to the fresh path over the
/// same trees, never diagnosing and never serving a wrong fact.
#[derive(Clone, Debug)]
pub struct ServedDrift {
    /// What refused, for the drift gates' failure output.
    pub detail: String,
}

/// The per-serve resolution of a checked prefix's name-keyed rows over
/// the merged compile's own universe: the artifact layer builds it
/// beside the shared bases (it owns the rows), and the declaration
/// build runs it once the reserve pass has minted the prefix's atoms —
/// the first moment every name the rows carry resolves. Mints replay
/// through the declaring pass's own operations, the interned sets
/// re-intern over name-resolved members in the generation's intern
/// order, and every recorded raw id rides beside its name as a checked
/// fast path — a disagreement is the [`ServedDrift`] the build falls
/// back from.
pub type ServedResolve = Box<dyn FnOnce(&mut Decls) -> Result<(), ServedDrift>>;

/// The declaration-side prefix a checked-ambient artifact serves: the
/// pre-built [`Decls`] — its universe empty, its def-keyed tables the
/// shared bases the serve installs, [`Decls::served_defs`] and
/// [`Decls::served_methods`] set — plus one resolve step the
/// declaration build runs after its own reserve pass mints the
/// prefix's def atoms in the merged walk's order. The rows the resolve
/// step installs are name-addressed: the shapes, memberships, mints,
/// interned sets, instance rows, and solved span-keyed records of the
/// prefix's own declaration pass, every atom reference through one
/// interned name table with the generation's raw id beside it as a
/// checked fast path.
pub struct ServedDecls {
    /// The prefix's declaration tables (the shared bases installed,
    /// `served_defs` and `served_methods` set, the universe empty).
    pub decls: Decls,
    /// The name-addressed rows' resolution over the merged universe.
    pub resolve: ServedResolve,
    /// The count of leading trees the prefix covers: the shape pass
    /// skips these trees' own solving and instance declarations (the
    /// resolve step installed those products), and the caller aligns
    /// one served resolution table per covered tree ahead of the fresh
    /// ones.
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
    build_over(package, asts, resolutions, None).expect("a fresh build carries no serve to drift")
}

/// [`build`] over a checked-ambient prefix: the pass starts from the
/// served [`Decls`] (its shared bases installed, its universe empty),
/// the reserve pass mints every tree's def atoms in the merged walk's
/// own order — a served def's atom mints exactly as a fresh pass's
/// does, so the atom space answers the fresh run's — and the serve's
/// name-addressed rows resolve over that universe between the reserve
/// and shape passes: the def-named identity checks (a name resolving
/// off its recorded raw id is a walk divergence), the shape-phase mints
/// replayed through the declaring pass's own operations, the interned
/// sets re-interned over name-resolved members in the generation's
/// intern order, the memberships, the instance rows, and the solved
/// span-keyed records installed. The shape pass then skips every
/// served tree's own solving (the resolve step installed those
/// products); every table the serve carries no rows for — aliases —
/// builds fresh over all files as in [`build`]; the const rows answer
/// from the shared base the caller installed (the ambient serve parks
/// one, a user prefix-cache serve has none and builds fresh exactly as
/// before). A drift refusal answers [`Err`]: the artifact is a pure
/// cache, so the caller falls back to the fresh path over the same
/// trees.
pub fn build_over<T>(
    package: &Package,
    asts: &[T],
    resolutions: &[Resolutions],
    served: Option<ServedDecls>,
) -> Result<(Decls, Vec<Diagnostic>), ServedDrift>
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
    let (mut decls, resolve) = match served {
        Some(served) => (served.decls, Some(served.resolve)),
        None => (Decls::default(), None),
    };
    let mut diags = Vec::new();
    decls.current_file = None;
    for ast in &trees {
        decls.current_file = Some(ast.file);
        for block in &ast.blocks {
            for item in &block.items {
                reserve_item(package, item, &mut decls);
            }
        }
    }
    decls.current_file = None;
    // The serve's name-addressed rows resolve over the merged walk's
    // universe — the one moment every name the rows carry exists: the
    // def atoms the reserve pass minted and the mints this step replays.
    // Every recorded raw id rides beside its name; inside the def-named
    // identity range a disagreement is a walk divergence the resolution
    // refuses loudly, and any name that does not resolve is a corrupt
    // or foreign artifact — both a cache miss the caller falls back
    // from, never a run-path condition to diagnose.
    if let Some(resolve) = resolve {
        resolve(&mut decls)?;
    }
    for (index, ast) in trees.iter().enumerate() {
        let served_item = index < served_files;
        decls.current_file = Some(ast.file);
        for block in &ast.blocks {
            for item in &block.items {
                shape_item(package, item, &mut decls, &mut diags, served_item);
            }
        }
    }
    decls.current_file = None;
    // The import-bound definitions: a linked function's signature
    // registers from the link pass's surface exactly like an extern's — a
    // call site checks against the written types — and its link fact
    // rides the table the call lowering reads; a linked const's literal
    // fact rides the table the type minting and the embed read; a linked
    // type's structural fact mints the declaration products a local
    // struct would carry, so every later pass consumes it unchanged.
    // The minted atom rides the serve's mint log: it is minted past the
    // shape phase, so neither a fresh walk nor the serve's def-named
    // identity range places it, and the resolve step's rows address it
    // by name — the re-run takes the minted atom through those rows.
    // A def the shape pass already forced (a written field named it
    // ahead of this loop) re-installs idempotently below — the same
    // replay-convergent path a served prefix's already-minted defs
    // take, so the one late loop serves both.
    let mut linked: Vec<(&DefId, &crate::import::ForeignEntry)> =
        package.foreign().iter().collect();
    linked.sort_by_key(|(def, _)| def.index());
    let mut ctx = MintCtx {
        package,
        forcing: Vec::new(),
    };
    for (def, entry) in linked {
        if let Some(sig) = &entry.sig {
            decls.fns.insert(*def, sig.clone());
        }
        if entry.kind == crate::import::ForeignKind::Fn {
            // a bodiless linked declaration (an extern) carries no link
            // fact: its callee is the native the consumer's own lowering
            // answers by name, so the call resolution falls through the
            // table to the native lookup
            if let Some(func) = entry.func {
                decls.foreign_links.insert(*def, ForeignLink {
                    position: entry.link,
                    func,
                });
            }
        } else if let Some(fact) = &entry.value {
            decls.foreign_consts.insert(*def, fact.clone());
        } else if let Some(fact) = &entry.ty {
            install_foreign_type(
                &mut ctx,
                *def,
                &entry.name,
                entry.decl_span,
                entry.link,
                fact,
                &mut decls,
            );
        }
    }
    // The linked trait impls declare after every foreign type and trait
    // minted: each row's head resolves through the minted atoms and its
    // trait through the minted trait id, so the instance the row
    // declares covers exactly the head the producer's impl did. A row
    // whose trait this compile never selected stays out — its dispatch
    // is unobservable here. An overlap with a stored instance resolves
    // by package identity in fixed layers: the root package's own impl
    // over every dependency's, an origin impl over a third party's, two
    // impls no layer separates refusing with both packages named. The
    // losing rows leave the solved tables and every shadowing warns,
    // but a loser whose own artifact statically exercises the head
    // refuses — its baked instance words would keep its behavior under
    // the winner.
    for fact in package.linked_impl_facts() {
        let Some(&trait_def) = decls.foreign_type_names.get(&fact.trait_name) else {
            continue;
        };
        let Some(&trait_id) = decls.traits.get(&trait_def) else {
            continue;
        };
        let Some((head, _)) = foreign_field_type(
            &mut MintCtx {
                package,
                forcing: Vec::new(),
            },
            fact.head.clone(),
            fact.span,
            &mut decls,
        ) else {
            continue;
        };
        let Some(solved) = decls.universe.solve(&head).ok() else {
            continue;
        };
        let origin_positions = origin_positions_of(package, &decls, trait_def, solved);
        // The election runs per vocabulary slot: a row in another slot
        // (a named impl beside an unnamed one, or two distinct names)
        // never conflicts with this row — each stays selectable in its
        // own vocabulary.
        let slot = fact.name.as_deref();
        let layer = |position: u32| {
            if origin_positions.contains(&position) {
                ImplLayer::Origin
            } else {
                ImplLayer::ThirdParty
            }
        };
        let fact_layer = layer(fact.link);
        let mut sides: Vec<LinkSide> = Vec::new();
        for id in decls
            .instances
            .overlapping(&mut decls.universe, trait_id, solved)
        {
            if decls.instances.name(id) != slot {
                continue;
            }
            if let Some(&span) = decls.instance_impls.get(&id) {
                match sides.iter_mut().find(|side| side.span == Some(span)) {
                    Some(side) => side.rows.push(id),
                    None => sides.push(LinkSide {
                        layer: ImplLayer::Root,
                        pkg: None,
                        exercised: false,
                        span: Some(span),
                        ptr: None,
                        rows: vec![id],
                    }),
                }
            } else if let Some(link) = decls.linked_impls.get(&id) {
                let ptr = Arc::as_ptr(link) as usize;
                match sides.iter_mut().find(|side| side.ptr == Some(ptr)) {
                    Some(side) => side.rows.push(id),
                    None => sides.push(LinkSide {
                        layer: layer(link.position),
                        pkg: Some(link.pkg.clone()),
                        exercised: link.exercised,
                        span: None,
                        ptr: Some(ptr),
                        rows: vec![id],
                    }),
                }
            }
        }
        let outcome = if sides.is_empty() {
            LinkOutcome::Declare
        } else if sides.iter().any(|side| side.layer == ImplLayer::Root) {
            LinkOutcome::Loses(impl_side_spell(None))
        } else if let Some(rival) = sides.iter().find(|side| side.layer == ImplLayer::Origin) {
            if fact_layer == ImplLayer::Origin {
                LinkOutcome::Refuses(vec![rival.clone()])
            } else {
                LinkOutcome::Loses(impl_side_spell(rival.pkg.as_deref()))
            }
        } else if fact_layer == ImplLayer::ThirdParty {
            LinkOutcome::Refuses(sides.iter().cloned().collect())
        } else {
            LinkOutcome::Wins(sides.iter().cloned().collect())
        };
        match outcome {
            LinkOutcome::Declare => {
                let name = fact.name.as_ref().map(|n| Arc::from(n.as_str()));
                let decl = InstanceDecl::new(trait_id, head).with_name(name);
                match decls.instances.declare(&mut decls.universe, &decl) {
                    Ok(ids) => {
                        for id in ids {
                            decls.linked_impls.insert(
                                id,
                                Arc::new(ForeignImplLink {
                                    position: fact.link,
                                    members: fact.members.clone(),
                                    pkg: fact.pkg.clone(),
                                    exercised: fact.exercised,
                                }),
                            );
                        }
                    },
                    Err(err) => diags.push(linked_instance_diag(&decls.universe, &err, fact)),
                }
            },
            LinkOutcome::Wins(losers) => {
                for loser in &losers {
                    for id in &loser.rows {
                        decls.instances.shadow(*id);
                    }
                    let Some(loser_pkg) = &loser.pkg else {
                        continue;
                    };
                    if loser.exercised {
                        diags.push(shadowed_exercised_diag(loser_pkg, fact));
                    } else {
                        diags.push(
                            Diagnostic::warning(
                                ErrorCode::LinkShadowedImpl,
                                format!(
                                    "{} shadows {}'s impl of `{}` for `{}`; drop {}'s impl to \
                                     adopt it, or keep the current selection",
                                    impl_side_spell(Some(&fact.pkg)),
                                    loser_pkg,
                                    fact.trait_name,
                                    crate::import::render_word(&fact.head),
                                    loser_pkg,
                                ),
                            )
                            .with_primary(fact.span, "the shadowing impl"),
                        );
                    }
                }
            },
            LinkOutcome::Loses(winner) => {
                if fact.exercised {
                    diags.push(shadowed_exercised_diag(&fact.pkg, fact));
                } else {
                    diags.push(
                        Diagnostic::warning(
                            ErrorCode::LinkShadowedImpl,
                            format!(
                                "{} is shadowed: {} takes the head; drop {}'s impl to adopt it, \
                                 or keep the current selection",
                                impl_side_spell(Some(&fact.pkg)),
                                winner,
                                fact.pkg,
                            ),
                        )
                        .with_primary(fact.span, "the shadowed impl"),
                    );
                }
            },
            LinkOutcome::Refuses(rivals) => {
                let rivals = rivals
                    .iter()
                    .filter_map(|side| side.pkg.as_deref())
                    .collect::<Vec<&str>>()
                    .join("`, `");
                diags.push(
                    Diagnostic::error(
                        ErrorCode::LinkLayerConflict,
                        format!(
                            "{} and {} both declare impls of `{}` for `{}` in the same layer: no \
                             package-identity layer separates them, so the link takes neither and \
                             refuses naming both packages",
                            fact.pkg,
                            rivals,
                            fact.trait_name,
                            crate::import::render_word(&fact.head),
                        ),
                    )
                    .with_primary(fact.span, "the refused impl"),
                );
            },
        }
    }
    // The linked inherent impls install after every foreign type minted
    // and the trait rows above: each member row keys onto the method id
    // the link pass registered, the written signature serving the
    // consumer's call check exactly like a local inherent member's row
    // and the member link serving the dispatch. A member a root
    // registration owns installs nothing (`None` below) — the call
    // sites select the root's row and the linked body never serves
    // here.
    for (fact, methods) in package.linked_inherent_facts() {
        for (member, method) in fact.members.iter().zip(methods) {
            let Some(method) = method else {
                continue;
            };
            decls.inherent.insert(*method, member.sig.clone());
            decls.linked_inherent.insert(*method, ForeignLink {
                position: fact.link,
                func: member.func,
            });
        }
    }
    package_coherence(&mut decls, &mut diags);
    Ok((decls, diags))
}

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
fn install_foreign_type(
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
fn install_foreign_trait(
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
fn linked_instance_diag(
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
enum ImplLayer {
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
struct LinkSide {
    layer: ImplLayer,
    pkg: Option<String>,
    exercised: bool,
    span: Option<Span>,
    ptr: Option<usize>,
    rows: Vec<InstanceId>,
}

/// How a linked impl row's overlap with the stored instances resolved.
enum LinkOutcome {
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
fn impl_side_spell(pkg: Option<&str>) -> String {
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
fn shadowed_exercised_diag(loser_pkg: &str, fact: &crate::import::ImplFact) -> Diagnostic {
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
fn origin_positions_of(
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
struct MintCtx<'a> {
    /// The consuming compile's package.
    package: &'a Package,
    /// The defs whose installs are in flight: a second pull of the same
    /// def is a cycle the early-declared atoms already serve (or an
    /// inline cycle the producer's own fixpoint refused).
    forcing: Vec<DefId>,
}

/// Ensures one linked def's mint ran, forcing it forward when its def
/// order sits ahead of the referencing install: an install declares its
/// atom ahead of resolving its fields, so a forced chain's members
/// resolve while the first install is still in flight, and a def whose
/// mint never completes answers `None` — the referencing def's silent
/// skip.
fn force_minted(ctx: &mut MintCtx, def: DefId, decls: &mut Decls) -> Option<()> {
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
fn linked_minted(decls: &Decls, def: DefId) -> bool {
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
fn force_linked(package: &Package, decls: &mut Decls, def: DefId) -> Option<()> {
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
fn foreign_fields(
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
fn install_foreign_struct(
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
fn install_foreign_enum(
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
fn install_foreign_case(
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
fn install_foreign_alias(
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
fn install_foreign_opaque(def: DefId, name: &str, _span: Span, decls: &mut Decls) {
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
fn install_foreign_tag(def: DefId, name: &str, _span: Span, decls: &mut Decls) {
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
fn foreign_field_type(
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

/// Whether a def's plain declaration tables arrived pre-built from the
/// checked-ambient artifact: the plain-table arms skip the def, its rows
/// already installed by the serve, while its atoms and traits still
/// mint — the merged walk owns those spaces. A user item duplicating a
/// served name resolves to the served def and skips the plain tables
/// too — the fresh path skips it as well (the duplicate mint fails),
/// so the outcome is the same.
fn is_served(decls: &Decls, def: DefId) -> bool {
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

/// The definition an item declares, in its kind's namespace.
fn item_def(package: &Package, decls: &Decls, item: &Item) -> Option<DefId> {
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
fn type_def_for_use(package: &Package, decls: &Decls, name: &str) -> Option<DefId> {
    match decls.current_file {
        Some(file) => package.lookup_type_in(file, name),
        None => package.lookup_type(name),
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
            if let Ok(id) = decls.universe.declare_struct(&s.name.name) {
                decls.atoms.insert(def, id);
                decls.defs_of_atoms.insert(id, def);
                if !is_served(decls, def) {
                    decls.structs.insert(def, StructShape {
                        params: s.generics.iter().map(GenericParam::from).collect(),
                        fields: struct_fields_ast(&s.members),
                        linear: s.supers.iter().any(linear_marker),
                    });
                }
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
            // per-instantiation facts.
            let declared = if generic {
                decls.universe.declare_enum(&e.name.name).map(|id| {
                    decls.atoms.insert(def, id);
                    decls.defs_of_atoms.insert(id, def);
                })
            } else {
                Ok(())
            };
            if declared.is_ok() && !is_served(decls, def) {
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
            // per-instantiation fact (the pattern seam the derive gate
            // names).
            for case in &e.cases {
                if generic && !matches!(case.payload, EnumPayload::None) {
                    continue;
                }
                // The def's name span proves this item owns the def; a
                // cross-file same-label case resolves through its own
                // file's shadow entry first.
                let Some(case_def) = type_def_for_use(package, decls, &case.name.name)
                    .filter(|&d| package.def(d).span == case.name.span)
                else {
                    continue;
                };
                let atom = match &case.payload {
                    EnumPayload::None => Some(declare_unit_name(decls, &case.name.name)),
                    // the re-keyed mint: the declaring case's own atom,
                    // never a name lookup; a payloaded case named `none`
                    // claims no atom
                    EnumPayload::Tuple(_) | EnumPayload::Struct(_) => (case.name.name != "none")
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
            if let Ok(id) = decls.universe.declare_trait(&t.name.name) {
                decls.traits.insert(def, id);
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
fn reserve_impl_fns(package: &Package, imp: &ImplDecl, impl_site: Span, decls: &mut Decls) {
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

fn def_kind_is_binding(package: &Package, def: DefId) -> bool {
    matches!(package.def(def).kind, DefKind::Const | DefKind::Var)
}

/// One top-level binding: every binder the pattern declares maps back to
/// this initializer — a destructuring pattern declares many defs, so
/// the item's own def is not the gate (a tuple pattern names no single
/// def). The const-of-lambda registration is the single-name shape only:
/// a lambda value binds one name. A row the serve already carries — the
/// shared base's, or an earlier file's — stays put: the base owns the
/// prefix's rows and a compiled file inserts only its own.
fn reserve_local(package: &Package, local: &LocalDecl, decls: &mut Decls) {
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
                let fields = universe_fields(package, &struct_fields_ast(&s.members), decls, diags);
                let _ = decls.universe.set_struct_fields(atom, fields);
            }
        },
        ItemKind::Enum(e) => {
            let Some(def) = type_def_for_use(package, decls, &e.name.name)
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
                // the set algebra and the lowerer.
                for case in &e.cases {
                    let Some(case_def) = type_def_for_use(package, decls, &case.name.name)
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
                            .filter_map(|t| {
                                let subject = format!("case `{}`'s payload slot", case.name.name);
                                field_expr(package, t, t.span(), &subject, decls, diags)
                            })
                            .map(Field::positional)
                            .collect(),
                        EnumPayload::Struct(fields) => {
                            universe_fields(package, fields, decls, diags)
                        },
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
                            .filter_map(|t| {
                                let subject = format!("case `{}`'s payload slot", case.name.name);
                                field_expr(package, t, t.span(), &subject, decls, diags)
                            })
                            .map(Field::positional)
                            .collect(),
                        EnumPayload::Struct(fields) => {
                            universe_fields(package, fields, decls, diags)
                        },
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
        ItemKind::Tag(t) => {
            let Some(def) = type_def_for_use(package, decls, &t.name.name)
                .filter(|&d| package.def(d).span == t.name.span)
            else {
                return
            };
            if is_served(decls, def) {
                return;
            }
            match &t.payload {
                // the tuple form refused before any mint: a payloaded
                // tag's atom is the struct shape alone
                EnumPayload::Tuple(_) => {
                    diags.push(
                        Diagnostic::error(
                            ErrorCode::TagPayloadTuple,
                            "a tag payload spells named fields".to_owned(),
                        )
                        .with_primary(
                            t.span,
                            "a tuple payload is not a tag form; write `tag name { field: type }`",
                        ),
                    );
                },
                EnumPayload::None => {},
                EnumPayload::Struct(fields) => {
                    let Some(&atom) = decls.atoms.get(&def) else {
                        return
                    };
                    let solved = universe_fields(package, fields, decls, diags);
                    let _ = decls.universe.set_struct_fields(atom, solved);
                },
            }
        },
        ItemKind::TypeAlias(_)
        | ItemKind::Trait(_)
        | ItemKind::Fn(_)
        | ItemKind::ExternFn(_)
        | ItemKind::Local(_)
        | ItemKind::Import(_)
        | ItemKind::Platform(_) => {},
    }
}

/// Whether a written type is the `~Copyable` marker: the negation of
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
/// path registers the atom as a trait member.
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

/// The traits one written bound names: a path its trait, an
/// intersection every arm's traits — an intersect bound's every arm
/// reads as trait evidence, the same rule the admission's check runs,
/// so a synthesized impl's bound keeps its condition and the member's
/// dispatch stages its evidence word.
fn bound_traits(package: &Package, ty: &Type, decls: &Decls) -> Vec<TraitId> {
    match ty {
        Type::Intersect { left, right, .. } => {
            let mut out = bound_traits(package, left, decls);
            out.extend(bound_traits(package, right, decls));
            out
        },
        _ => trait_of_type(package, ty, decls).into_iter().collect(),
    }
}

/// Converts annotated struct/enum fields into solved universe fields; a
/// field whose type does not live in the [`TypeExpr`] world (function
/// types, foreign types) contributes nothing — the checker works from the
/// AST shape regardless, only witnesses degrade.
fn universe_fields(
    package: &Package,
    fields: &[FieldDecl],
    decls: &mut Decls,
    diags: &mut Vec<Diagnostic>,
) -> Vec<Field> {
    let mut out = Vec::with_capacity(fields.len());
    for field in fields {
        let subject = format!("field `{}`", field.name.name);
        if let Some(expr) = field_expr(package, &field.ty, field.span, &subject, decls, diags) {
            out.push(Field::named(&field.name.name, expr));
        }
    }
    out
}

/// One field type's conversion behind the none door: a written type
/// whose solution is the `none` atom alone refuses at its own span —
/// `none` is the optional's absent member, never a field's whole
/// type, and a field that enters no atom would surface later as a
/// lowering refusal over the wrong vocabulary.
fn field_expr(
    package: &Package,
    ty: &Type,
    span: Span,
    subject: &str,
    decls: &mut Decls,
    diags: &mut Vec<Diagnostic>,
) -> Option<TypeExpr> {
    let expr = conv_expr(package, ty, decls, diags)?;
    let none_alone = decls
        .universe
        .solve(&expr)
        .is_ok_and(|solved| decls.universe.members(solved) == [TypeId::NONE]);
    if none_alone {
        diags.push(
            Diagnostic::error(
                ErrorCode::NoneTypedField,
                format!(
                    "{subject} cannot be typed `none`: `none` is the optional's absent member, a \
                     member of `t?` and `t | none` spellings, never a standalone field type"
                ),
            )
            .with_primary(span, "none-typed field"),
        );
        return None;
    }
    Some(expr)
}

/// A written type application's shape refusal against its head
/// declaration: the applied argument count against the head's declared
/// parameter count, or a head whose kind never takes type arguments.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ApplicationMismatch {
    /// A head applied with the wrong argument count.
    Arity {
        /// The head's declared type-parameter count.
        declared: usize,
        /// The argument count the application spells.
        given: usize,
    },
    /// A head that takes no type arguments at all.
    Kind { word: &'static str },
}

impl ApplicationMismatch {
    /// The refusal sentence, naming the declaration and both counts.
    pub(crate) fn message(&self, name: &str) -> String {
        match self {
            Self::Arity { declared, given } => format!(
                "`{name}` declares {declared} type parameter(s); the application spells {given}"
            ),
            Self::Kind { word } => format!("`{name}` is a {word} and takes no type arguments"),
        }
    }
}

/// Validates one written type application against its head declaration
/// by `DefKind`: struct, enum, alias, and trait heads carry their declared
/// parameter count, while tags, cases, value-namespace names, and every
/// non-generic head take zero arguments. The conversion seams call this
/// ahead of `apply_atom`, which internal structural application also
/// reuses.
pub(crate) fn check_application(
    package: &Package,
    def: DefId,
    given: usize,
    decls: &Decls,
) -> Result<(), ApplicationMismatch> {
    let declared = match package.def(def).kind {
        DefKind::Struct => decls
            .structs
            .get(&def)
            .map_or(0, |shape| shape.params.len()),
        DefKind::Enum => decls.enums.get(&def).map_or(0, |shape| shape.params.len()),
        DefKind::TypeAlias => decls
            .aliases
            .get(&def)
            .map_or(0, |(params, _)| params.len()),
        DefKind::Trait => decls.trait_generics.get(&def).map_or(0, Vec::len),
        kind => return Err(ApplicationMismatch::Kind { word: kind.word() }),
    };
    if declared == given { Ok(()) } else { Err(ApplicationMismatch::Arity { declared, given }) }
}

/// Converts a written type into the [`TypeExpr`] world for non-generic
/// universe shapes; `None` marks a type the core cannot express.
fn conv_expr(
    package: &Package,
    ty: &Type,
    decls: &mut Decls,
    diags: &mut Vec<Diagnostic>,
) -> Option<TypeExpr> {
    Some(match ty {
        Type::Path {
            span, path, args, ..
        } => {
            let [only] = path.segments.as_slice() else {
                return None
            };
            if args.is_empty() {
                path_expr(package, only, decls, diags)?
            } else {
                let Some(def) = package.lookup_type(&only.name) else {
                    // `none` applied with type arguments: the predeclared
                    // unit atom takes none, the same refusal a local
                    // tag's application hears, instead of the field
                    // silently missing its atom.
                    if only.name == "none" && !package.declares("none") {
                        diags.push(
                            Diagnostic::error(
                                ErrorCode::TypeApplicationMismatch,
                                ApplicationMismatch::Kind { word: "tag" }.message(&only.name),
                            )
                            .with_primary(*span, "type application"),
                        );
                    }
                    return None;
                };
                if let Err(mismatch) = check_application(package, def, args.len(), decls) {
                    diags.push(
                        Diagnostic::error(
                            ErrorCode::TypeApplicationMismatch,
                            mismatch.message(&only.name),
                        )
                        .with_primary(*span, "type application"),
                    );
                    return None;
                }
                let ctor = decls.atoms.get(&def).copied()?;
                let args = conv_args(package, args, &FxHashMap::default(), decls, diags)?;
                TypeExpr::Atom(decls.universe.apply_atom(ctor, &args).ok()?)
            }
        },
        Type::Optional { inner, .. } => {
            conv_expr(package, inner, decls, diags)?.union(TypeId::NONE.into())
        },
        Type::Negation { inner, .. } => TypeExpr::Not(trait_of_type(package, inner, decls)?),
        Type::Paren { inner, .. } => conv_expr(package, inner, decls, diags)?,
        Type::Union { left, right, .. } => {
            conv_expr(package, left, decls, diags)?.union(conv_expr(package, right, decls, diags)?)
        },
        Type::Intersect { left, right, .. } => {
            conv_expr(package, left, decls, diags)?.inter(conv_expr(package, right, decls, diags)?)
        },
        Type::Difference { left, right, .. } => {
            conv_expr(package, left, decls, diags)?.diff(conv_expr(package, right, decls, diags)?)
        },
        Type::Tuple { items, .. } => {
            let mut elements = Vec::new();
            for item in items {
                elements.push(conv_expr(package, item, decls, diags)?);
            }
            TypeExpr::Tuple(elements)
        },
        Type::Unit(_) => TypeExpr::Tuple(Vec::new()),
        // the comptime type-value atom: the `type` keyword's
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
    diags: &mut Vec<Diagnostic>,
) -> Option<Vec<Ty>> {
    let _ = vars;
    let mut out = Vec::with_capacity(args.len());
    for arg in args {
        let expr = conv_expr(package, arg, decls, diags)?;
        out.push(decls.universe.solve(&expr).ok()?);
    }
    Some(out)
}

/// A single-segment type path in expression position: a declared atom, a
/// trait as its member set, or a non-generic alias expanded to its full
/// written union — never a set's single member.
fn path_expr(
    package: &Package,
    name: &Ident,
    decls: &mut Decls,
    diags: &mut Vec<Diagnostic>,
) -> Option<TypeExpr> {
    // `none` is the universe's predeclared unit atom with no def,
    // so both namespace lookups miss the name; the optional arm unions
    // this same atom for `t?`, and a package declaration claiming the
    // name keeps its own resolution below.
    if name.name == "none" && !package.declares("none") {
        return Some(TypeExpr::Atom(TypeId::NONE));
    }
    // An import-bound type the shape pass names ahead of the linked
    // mint loop: its def sits outside the local type namespace (the
    // link pass's registration), so the foreign table answers by
    // name, and the install forces forward — or the referencing
    // field would silently drop out of its declaration's universe
    // atom, a shape both this module's own lowering and the export
    // walk's link fact read.
    let Some(def) = package.lookup_type(&name.name) else {
        let def = package
            .foreign()
            .iter()
            .find(|(_, entry)| entry.name == name.name && entry.ty.is_some())
            .map(|(def, _)| *def)?;
        force_linked(package, decls, def)?;
        return path_def_expr(package, def, decls, diags);
    };
    path_def_expr(package, def, decls, diags)
}

/// One resolved def's universe form behind a written path segment: an
/// atom (a struct, a case, a tag), a trait reference, a non-generic
/// enum's case-atom union, or a non-generic alias's expanded
/// body. `None` marks a def the universe cannot express here.
fn path_def_expr(
    package: &Package,
    def: DefId,
    decls: &mut Decls,
    diags: &mut Vec<Diagnostic>,
) -> Option<TypeExpr> {
    if let Some(&atom) = decls.atoms.get(&def) {
        return Some(TypeExpr::Atom(atom));
    }
    if let Some(&trait_id) = decls.traits.get(&def) {
        return Some(TypeExpr::Trait(trait_id));
    }
    // A non-generic enum names the union of its minted case atoms.
    // The union's labels and an alias's body are the declaration's own
    // text: they resolve under the declaring file's shadow map, never
    // the use site's.
    if let Some(shape) = decls.enums.get(&def) {
        if shape.params.is_empty() {
            let saved = decls.current_file;
            decls.current_file = Some(package.def(def).span.file);
            let union = enum_case_union(package, &shape.cases, decls);
            decls.current_file = saved;
            return union;
        }
        return None;
    }
    if let Some((params, body)) = decls.aliases.get(&def).cloned()
        && params.is_empty()
    {
        let saved = decls.current_file;
        decls.current_file = Some(package.def(def).span.file);
        let expr = conv_expr(package, &body, decls, diags);
        decls.current_file = saved;
        return expr;
    }
    None
}

/// The union of one enum's minted case atoms; `None` when a case did
/// not mint. A zero-case enum is the canonical empty spelling — the
/// uninhabited union of zero atoms, so heads and fields over it convert.
/// A linked enum's case defs sit outside the package's own namespace, so
/// the name index the linked install filled answers them.
fn enum_case_union(
    package: &Package,
    cases: &[(String, Span, EnumPayload)],
    decls: &Decls,
) -> Option<TypeExpr> {
    if cases.is_empty() {
        return Some(TypeExpr::Empty);
    }
    let mut union: Option<TypeExpr> = None;
    for (name, ..) in cases {
        let atom = type_def_for_use(package, decls, name)
            .or_else(|| decls.foreign_type(name))
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
    let def = type_def_for_use(package, decls, &name.name)?;
    if decls.atoms.get(&def).is_none() && package.foreign().contains_key(&def) {
        // A written type naming a linked declaration shapes ahead of the
        // link install (an impl head, an annotation the local namespace
        // cannot answer): the same pull a written field and a trait
        // reference take, and the install replays idempotently.
        force_linked(package, decls, def)?;
    }
    decls.atoms.get(&def).copied()
}

/// Mints the unit atom for one local tag (or unit enum case): the
/// atom is the declaring declaration's own, so the mint never refuses and
/// two declarations may share a label as distinct atoms. The universe
/// predeclares `none` as its distinguished unit atom with no def, so
/// a `tag none` (or `enum e { none }`) restating the name maps onto the
/// predeclared atom instead of minting a second — a skipped mint would
/// silently orphan the def and every written `none` type or value would
/// stop solving.
fn declare_unit_name(decls: &mut Decls, name: &str) -> TypeId {
    if name == "none" {
        return TypeId::NONE;
    }
    decls.universe.mint_tag(name)
}

/// The linked case's unit mint keeps the name-keyed contract: a label already declared in this
/// compile reuses that atom, `none` maps onto the predeclared atom, and
/// any other refused name leaves the def for the replayed atom.
fn declare_foreign_unit_name(decls: &mut Decls, name: &str) -> Option<TypeId> {
    match decls.universe.declare_tag(name) {
        Ok(id) => Some(id),
        Err(_) if name == "none" => Some(TypeId::NONE),
        Err(_) => None,
    }
}

/// Records the solved field signature of every application of a
/// user-declared generic struct, after the whole package resolves: the
/// walk enumerates the universe's atoms in id order (growing as field
/// solving mints nested applications), reads the constructor's
/// declared shape, and solves each declared field under the
/// parameter-to-argument substitution — the same solving field access
/// and construction run at their sites, the field types resolving
/// under the declaring file's shadow map. An application whose fields
/// do not all solve records nothing, and its values keep the lowering
/// refusal they always carried; a self-containing application records
/// the entry its fields name, and the shape layer's cycle budget
/// refuses the value shape the entry describes.
pub(crate) fn record_apply_structs(package: &Package, decls: &mut Decls) {
    let mut at = 0u32;
    while at < decls.universe.atom_count() {
        let id = TypeId::from_raw(at);
        at += 1;
        let kind = match decls.universe.atom_decl(id) {
            Ok(decl) => decl.kind.clone(),
            Err(_) => continue,
        };
        let crate::AtomKind::Apply(ctor, args) = kind else {
            continue;
        };
        let Some(&def) = decls.defs_of_atoms.get(&ctor) else {
            continue;
        };
        let Some(shape) = decls.structs.get(&def) else {
            continue;
        };
        if shape.params.is_empty() || shape.params.len() != args.len() {
            continue;
        }
        let mut vars: FxHashMap<String, Ty> = FxHashMap::default();
        for (param, arg) in shape.params.iter().zip(args.iter()) {
            vars.insert(param.name.clone(), *arg);
        }
        // the shape's own rows copy ahead of the solving, whose minting
        // borrows the tables mutably
        let written: Vec<Type> = shape.fields.iter().map(|field| field.ty.clone()).collect();
        let names: Vec<String> = shape
            .fields
            .iter()
            .map(|field| field.name.name.clone())
            .collect();
        let saved = decls.current_file;
        decls.current_file = Some(package.def(def).span.file);
        let mut fields = Vec::with_capacity(written.len());
        let mut solved_all = true;
        for (name, ty_written) in names.into_iter().zip(written.iter()) {
            match solve_written(package, ty_written, &vars, decls) {
                Some(ty) => fields.push(SolvedField { name, ty }),
                None => solved_all = false,
            }
        }
        decls.current_file = saved;
        if solved_all {
            decls.apply_structs.insert(id, fields.into());
        }
    }
}

/// Converts one written type into a solved singleton [`TypeExpr`] for
/// instance heads and conditions: unions stay unions in [`conv_head`],
/// everything else becomes its solved atom set.
fn conv_node(
    package: &Package,
    ty: &Type,
    vars: &FxHashMap<String, Ty>,
    decls: &mut Decls,
) -> Option<TypeExpr> {
    Some(match ty {
        Type::Union { left, right, .. } => {
            conv_node(package, left, vars, decls)?.union(conv_node(package, right, vars, decls)?)
        },
        Type::Paren { inner, .. } => conv_node(package, inner, vars, decls)?,
        other => {
            // Everything else: solve to a Ty, then re-express as a union of
            // atoms the TypeExpr world can carry — the empty set through
            // its canonical spelling, so a head naming a zero-case enum
            // (or any solved-empty shape) converts.
            let solved = solve_written(package, other, vars, decls)?;
            let mut expr = None::<TypeExpr>;
            for atom in decls.universe.members(solved) {
                expr = Some(match expr {
                    None => TypeExpr::atom(*atom),
                    Some(e) => e.union((*atom).into()),
                });
            }
            expr.unwrap_or(TypeExpr::Empty)
        },
    })
}

/// Solves one written type (path, optional, tuple, unit) to a [`Ty`] in a
/// conversion context; type variables resolve to their minted atoms.
fn solve_written(
    package: &Package,
    ty: &Type,
    vars: &FxHashMap<String, Ty>,
    decls: &mut Decls,
) -> Option<Ty> {
    Some(match ty {
        Type::Path { path, args, .. } => {
            let [only] = path.segments.as_slice() else {
                return None
            };
            if let Some(&ty) = vars.get(&only.name) {
                ty
            } else if args.is_empty() {
                if let Some(atom) = atom_of_path(package, only, decls) {
                    decls.universe.atom_ty(atom).ok()?
                } else {
                    // A trait as a set, or a non-generic alias: solve its
                    // written expression in full. An alias body expanded
                    // here reports no application mismatch: an impl-head
                    // conversion degrades silently like its other
                    // unsolvable shapes, and the impl's own body checking
                    // reports the unsolved target.
                    path_expr(package, only, decls, &mut Vec::new())
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
        // the comptime type-value atom, in the written-variable
        // conversion context
        Type::TypeKw(_) => decls.universe.atom_ty(TypeId::TYPE).ok()?,
    })
}

/// Records one generic inherent impl's solved head pattern and declared
/// bounds: minted variables, the solved head, and one condition per
/// declared bound — the same shapes the trait arm builds, so the two impl
/// forms read through the same machinery downstream. A bounds-free generic
/// impl records its head with no conditions: the head pattern is what a
/// call site's signature substitution and the type-keyed surface's
/// generic-impl refusal read, and an empty bound list stages nothing. A
/// head the type core cannot solve records nothing (the impl's own body
/// checking reports the target).
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

/// The registered method id of one trait member.
fn member_id(package: &Package, trait_def: DefId, name: &str) -> Option<MethodId> {
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
