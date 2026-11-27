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
    resolve::{DefId, DefKind, MethodId, MethodOwner, Package, Resolutions},
    universe::{Ty, Universe},
};

mod conv;
mod foreign;
mod instance;
mod reserve;
mod shape;

use conv::*;
pub(crate) use conv::{
    applications_of_head, applied_enum_cases, check_application, record_applications,
};
use foreign::*;
pub(crate) use instance::trait_member_sig;
use instance::*;
pub use reserve::consts_base;
use reserve::*;
pub(crate) use shape::linear_marker;
use shape::*;

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

/// One payload slot of a generic enum's applied case: the declared
/// slot's name (`None` for a tuple payload's positional slot) and its
/// type solved under the application's arguments.
#[derive(Clone, Debug)]
pub struct AppliedSlot {
    /// The declared slot name; `None` marks a positional tuple slot.
    pub name: Option<String>,
    /// The slot's type under the application's arguments.
    pub ty: Ty,
}

/// One case of a user-declared generic enum's application: the case's
/// def, the atom its values carry under the application's arguments —
/// a bare case the shared unit atom it always minted, a payloaded case
/// the application's own atom over its solved slots — and the solved
/// payload slots (`None` for a bare case). The whole application's
/// case list rides [`Decls::apply_enums`].
#[derive(Clone, Debug)]
pub struct AppliedCase {
    /// The case declaration's own def.
    pub def: DefId,
    /// The member atom the case's values carry under this application.
    pub atom: TypeId,
    /// The solved payload slots; `None` for a bare case.
    pub slots: Option<std::rc::Rc<[AppliedSlot]>>,
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
    /// The per-case facts of every minted application of a user-declared
    /// generic enum: each case's def, the atom its values carry under
    /// the application's arguments, and the case's solved payload
    /// slots. Recorded once per application atom (the checking walk
    /// minting on demand at the doors that need the member identity,
    /// the post-resolution walk covering the rest); the lowering pass
    /// shapes the application's union through it, and an instantiation
    /// derives the substituted application's entry from its source's.
    pub apply_enums: std::collections::HashMap<TypeId, std::rc::Rc<[AppliedCase]>>,
    /// The `#[sealed]` traits, keyed by the trait def with the sealed
    /// name occurrence's span: the origin attribute whose seal admits
    /// only the trait's own declaring file's impls. A local trait
    /// records at its declaration; a linked trait records from its
    /// crossed fact, the span rebased into this compile's file space.
    /// The impl seams read it: a local impl whose file differs from
    /// the declaration's refuses, and a linked impl row declares only
    /// from the trait's own link position.
    pub sealed_traits: IdTable<DefId, Span>,
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
        if let Some(diag) = sealed_row_refusal(package, &decls, trait_def, fact) {
            diags.push(diag);
            continue;
        }
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
            LinkOutcome::Refuses(sides.clone())
        } else {
            LinkOutcome::Wins(sides.clone())
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
