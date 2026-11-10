//! Declaration pass: AST items into universe atoms, trait instances, and
//! AST-shaped signature tables [B1, B3, B6, G3].
//!
//! Collection is two-phase over the whole package [G3]: every type-level
//! name is reserved first (tags, structs, enums, aliases, traits), then
//! shapes are filled, so intra- and cross-file declaration cycles resolve
//! by construction. The universe carries what its set algebra can express:
//! non-generic struct/enum shapes as solved field/case signatures (witness
//! fidelity [B6]) and nominal identities for everything. What the algebra
//! cannot express stays AST-shaped here and is solved per use by the
//! checker (crate::type_of) against the use-site's scopes:
//!
//! * generic shapes (`struct stack[t] { items: list[t] }`) — element types contain applications of
//!   type variables, which have no [`crate::expr::TypeExpr`] form; the checker solves the written
//!   fields with its own variable substitution;
//! * function signatures — solved per call site, with the callee's generic parameters matched
//!   structurally against the argument types (one-body generics, the M2 half of [H3]);
//! * instance heads of generic impls — minted into the [`crate::instance::InstanceEnv`] as solved
//!   singletons over freshly minted variable atoms, so coherence checking [B6] runs unchanged.
//!
//! Instance-head resolution under substitution (`json for list[int]`
//! because `json for int` exists) is deliberately not here: lookup stays
//! containment-only, exact for concrete heads and conservative for
//! variable heads; seeing through a conditional head is H3 checker work.
//! The `~Copyable` marker bound on generic parameters [C1] is carried on
//! the parameter and otherwise unchecked here; the affine pass owns it at
//! M4.

use std::collections::HashMap;

use ds_diag::{Diagnostic, ErrorCode, Span};
use ds_parse::{
    Ast, EnumPayload, Expr, FieldDecl, FnDecl, Ident, Item, ItemKind, LocalDecl, ParamMode,
    Pattern, StructMember, TraitMember, Type, TypeParam,
};

use crate::{
    TypeExpr,
    atom::{Case, Field, TraitId, TypeId},
    instance::{Condition, InstanceDecl, InstanceEnv},
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
    /// affine layer's [C1, M4].
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
    /// Passing mode [C2]; the affine pass reads it to decide which call
    /// arguments consume their source binding [C1].
    pub mode: ParamMode,
    /// Written type; absent for inferred lambda parameters.
    pub ty: Option<Type>,
    /// Literal-only default [F7], as written.
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
    /// (`~Copyable` among the supers) [C1].
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
    /// (`~Copyable` among the supers) [C1].
    pub linear: bool,
}

/// One case minted by a non-generic enum block [B3]: the case's def maps
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
    /// The atom and trait registry [B1].
    pub universe: Universe,
    /// The trait instance environment [B6].
    pub instances: InstanceEnv,
    /// DefId to atom identity for tags, structs, and enums.
    pub atoms: HashMap<DefId, TypeId>,
    /// Atom identity back to the declaring DefId (field access on
    /// applications decomposes through this).
    pub defs_of_atoms: HashMap<TypeId, DefId>,
    /// DefId to trait identity.
    pub traits: HashMap<DefId, TraitId>,
    /// Alias bodies: DefId to generic parameters and the written type.
    pub aliases: HashMap<DefId, (Vec<GenericParam>, Type)>,
    /// Struct shapes for every struct def (generic and not).
    pub structs: HashMap<DefId, StructShape>,
    /// Enum shapes for every enum def.
    pub enums: HashMap<DefId, EnumShape>,
    /// Minted cases of non-generic enums, keyed by the case's def.
    pub cases: HashMap<DefId, CaseDecl>,
    /// Signatures of named functions, extern functions, and const-arrow
    /// function bindings.
    pub fns: HashMap<DefId, FnSig>,
    /// Trait member signatures keyed by their registered method id [F3].
    pub members: HashMap<MethodId, FnSig>,
    /// Member names of each trait, for impl signature matching [B6].
    pub trait_members: HashMap<DefId, Vec<String>>,
    /// Inherent impl-function signatures keyed by method id [F3].
    pub inherent: HashMap<MethodId, FnSig>,
    /// Top-level `const`/`var` initializers keyed by every name the
    /// binding pattern declares (lazy memoized typing [G3]).
    pub consts: HashMap<DefId, LocalDecl>,
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
    let _ = resolutions;
    let trees: Vec<&Ast> = asts.iter().map(std::borrow::Borrow::borrow).collect();
    let mut decls = Decls::default();
    let mut diags = Vec::new();
    for ast in &trees {
        for block in &ast.blocks {
            for item in &block.items {
                reserve_item(package, item, &mut decls);
            }
        }
    }
    for ast in &trees {
        for block in &ast.blocks {
            for item in &block.items {
                shape_item(package, item, &mut decls, &mut diags);
            }
        }
    }
    (decls, diags)
}

use crate::resolve::Resolutions;

/// The definition an item declares, in its kind's namespace [F9].
fn item_def(package: &Package, item: &Item) -> Option<DefId> {
    let name = item_name(item);
    if matches!(
        item.kind,
        ItemKind::Struct(_)
            | ItemKind::Enum(_)
            | ItemKind::Tag(_)
            | ItemKind::TypeAlias(_)
            | ItemKind::Trait(_)
    ) {
        package.lookup_type(name)
    } else {
        package.lookup_value(name)
    }
}

fn reserve_item(package: &Package, item: &Item, decls: &mut Decls) {
    let Some(def) = item_def(package, item) else {
        return; // a duplicate definition was reported at collection
    };
    match &item.kind {
        ItemKind::Tag(t) => {
            if let Ok(id) = decls.universe.declare_tag(&t.name.name) {
                decls.atoms.insert(def, id);
                decls.defs_of_atoms.insert(id, def);
            }
        },
        ItemKind::Struct(s) => {
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
                        EnumPayload::None => decls.universe.declare_tag(&case.name.name),
                        EnumPayload::Tuple(_) | EnumPayload::Struct(_) => {
                            decls.universe.declare_struct(&case.name.name)
                        },
                    };
                    if let Ok(id) = atom {
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
            if let Ok(id) = decls.universe.declare_trait(&t.name.name) {
                decls.traits.insert(def, id);
                let mut names = Vec::new();
                for member in &t.members {
                    if let TraitMember::Fn(f) = member {
                        names.push(f.name.name.clone());
                        if let Some(method) = member_id(package, def, &f.name.name) {
                            decls.members.insert(method, FnSig::of(f));
                        }
                    }
                }
                decls.trait_members.insert(def, names);
            }
        },
        ItemKind::Fn(f) | ItemKind::ExternFn(f) => {
            decls.fns.insert(def, FnSig::of(f));
        },
        ItemKind::Local(local) => {
            if def_kind_is_binding(package, def) {
                for binder in pattern_names(&local.pat) {
                    if let Some(binder_def) = package.lookup_value(&binder) {
                        decls.consts.insert(binder_def, local.clone());
                    }
                }
            }
            if let Some(mut sig) = FnSig::of_lambda(&local.init) {
                // A const-arrow function binding: the signature is the
                // lambda's own annotations; a written binding annotation
                // supplies the return where the lambda omits one.
                if sig.ret.is_none() {
                    sig.ret.clone_from(&local.ty);
                }
                decls.fns.insert(def, sig);
            }
        },
        ItemKind::Impl(imp) => {
            // Inherent impl functions register as method candidates [F3];
            // their signatures are checked from the impl bodies directly.
            if imp.trait_ref.is_none() {
                for f in &imp.fns {
                    if let Some(method) = package
                        .method_candidates(&f.name.name)
                        .iter()
                        .copied()
                        .find(|&id| package.method(id).owner == MethodOwner::Inherent)
                    {
                        decls.inherent.insert(method, FnSig::of(f));
                    }
                }
            }
        },
        ItemKind::Import(_) => {},
    }
}

fn def_kind_is_binding(package: &Package, def: DefId) -> bool {
    matches!(
        package.def(def).kind,
        crate::resolve::DefKind::Const | crate::resolve::DefKind::Var
    )
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

fn shape_item(package: &Package, item: &Item, decls: &mut Decls, diags: &mut Vec<Diagnostic>) {
    match &item.kind {
        ItemKind::Struct(s) => {
            let Some(def) = package.lookup_type(&s.name.name) else {
                return
            };
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
        ItemKind::Impl(imp) => declare_instance(package, item, imp, decls, diags),
        ItemKind::Tag(_)
        | ItemKind::TypeAlias(_)
        | ItemKind::Trait(_)
        | ItemKind::Fn(_)
        | ItemKind::ExternFn(_)
        | ItemKind::Local(_)
        | ItemKind::Import(_) => {},
    }
}

/// Whether a written type is the `~Copyable` marker [C1]: the negation of
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
/// path registers the atom as a trait member [B1].
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
                let args = conv_args(package, args, &HashMap::new(), decls)?;
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
        Type::Fn { .. } | Type::Ptr { .. } | Type::TypeKw(_) | Type::Foreign { .. } => return None,
    })
}

/// Solves written application arguments in conversion contexts.
fn conv_args(
    package: &Package,
    args: &[Type],
    vars: &HashMap<String, TypeId>,
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

/// Converts one written type into a solved singleton [`TypeExpr`] for
/// instance heads and conditions: unions stay unions in [`conv_head`],
/// everything else becomes its solved atom set.
fn conv_node(
    package: &Package,
    ty: &Type,
    vars: &HashMap<String, TypeId>,
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
    vars: &HashMap<String, TypeId>,
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
        | Type::TypeKw(_)
        | Type::Foreign { .. } => return None,
    })
}

/// Declares one impl's instances into the environment [B6]: mints variable
/// atoms for the generic parameters, converts the head (written unions
/// decompose inside `declare`), attaches trait bounds as conditions, and
/// maps coherence failures onto witness-carrying diagnostics. A failed
/// declaration leaves the environment unchanged; the impl's methods are
/// still body-checked (the checker works from the AST).
fn declare_instance(
    package: &Package,
    item: &Item,
    imp: &ds_parse::ImplDecl,
    decls: &mut Decls,
    diags: &mut Vec<Diagnostic>,
) {
    let Some(trait_ref) = &imp.trait_ref else {
        return
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

    let mut vars = HashMap::new();
    for param in &imp.generics {
        if let Ok(var) = decls.universe.declare_tag(&format!("${}", param.name.name)) {
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

    let decl = InstanceDecl::new(trait_id, head).with_conditions(conditions);
    if let Err(err) = decls.instances.declare(&mut decls.universe, &decl) {
        diags.push(instance_diag(&decls.universe, &err, item, &imp.target));
    }
}

/// Renders a coherence failure as one diagnostic naming the written head,
/// the intersecting earlier head, and the witness value type [B6]; the
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

/// The registered method id of one trait member [F3].
fn member_id(package: &Package, trait_def: DefId, name: &str) -> Option<MethodId> {
    package
        .method_candidates(name)
        .iter()
        .copied()
        .find(|&id| package.method(id).owner == MethodOwner::Trait(trait_def))
}

/// Every name a binding pattern binds.
fn pattern_names(pat: &Pattern) -> Vec<String> {
    let mut out = Vec::new();
    walk_pattern_names(pat, &mut out);
    out
}

fn walk_pattern_names(pat: &Pattern, out: &mut Vec<String>) {
    match pat {
        Pattern::Binding { name, .. } => out.push(name.name.clone()),
        Pattern::Tuple { pats, .. } | Pattern::Or { pats, .. } => {
            for sub in pats {
                walk_pattern_names(sub, out);
            }
        },
        Pattern::Struct { fields, .. } => {
            for (name, field) in fields {
                match field {
                    None => out.push(name.name.clone()),
                    Some(sub) => walk_pattern_names(sub, out),
                }
            }
        },
        _ => {},
    }
}

/// The declared name of an item, for package lookup.
fn item_name(item: &Item) -> &str {
    match &item.kind {
        ItemKind::Import(_) | ItemKind::Impl(_) => "",
        ItemKind::Local(local) => match &local.pat {
            Pattern::Binding { name, .. } => name.name.as_str(),
            _ => "",
        },
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
