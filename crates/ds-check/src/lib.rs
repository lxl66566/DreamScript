//! Set-theoretic type core over nominal and structural atoms [B1, B3].
//!
//! Declarations are nominal — every tag, struct, and enum gets one
//! [`TypeId`] — and distinct atoms denote disjoint value sets. Anonymous
//! tuples `(t0, t1, ...)` are the structural exception [B3]: the universe
//! mints one atom per distinct element-atom list, hash-consed so tuple
//! equality is structural. The type language above the atoms is
//! set-theoretic: unions `|`, intersections `&`, differences `-`, and
//! negated marker-trait constraints `~T` are ordinary type expressions
//! ([`TypeExpr`]). `none` is a distinguished unit atom [B4], so `T?` is
//! exactly `T | none` ([`Universe::optional`]) and optionals, error unions,
//! and enums share one mechanism.
//!
//! # Normal form
//!
//! Every type expression is *solved* against the current state of a
//! [`Universe`] into one canonical form: the sorted, deduplicated set of the
//! atoms it denotes, hash-consed so [`Ty`] handle equality is set equality.
//! This is the entire normal form — the decision recorded here for [B1]
//! ("difference types are compile-time only and erase entirely"):
//!
//! * difference nodes never survive solving: `a - b` lowers to the solved shape of `a` minus `b`'s
//!   members ([`Universe::diff`]);
//! * negated constraints never survive either: `~T` solves to the complement of `T`'s members over
//!   the universe's atoms;
//! * tuple products distribute over unions element-wise [B3]: `(a | b, c)` solves to `(a, c) | (b,
//!   c)`, one structural atom per combination, so every minted tuple atom is a product of single
//!   atoms and distinct atoms stay disjoint — the `()` of the surface is the arity-0 atom, a
//!   sibling of `none`, never its alias;
//! * structural equality of two types is equality of their solved forms, which handle equality
//!   gives for free.
//!
//! Solved forms are snapshots of the universe state they were solved
//! against. Declaring atoms can change what `~T` or a trait used as a type
//! solves to, so consumers re-solve after declarations change (the memoized
//! query layer owns that invalidation). Minted handles stay valid and
//! immutable forever.
//!
//! # Decision procedures [B1, B6, section 10 item 7]
//!
//! * [`Universe::is_empty`]: empty iff the solved member set is empty. An intersection of distinct
//!   nominal atoms is empty, an atom minus itself is empty, `t & ~t` is empty, and a union is empty
//!   only if all members are — all special cases of the solved-set check. A tuple whose element
//!   solves empty never mints an atom, so the empty product is the empty set.
//! * [`Universe::is_disjoint`]: disjoint iff `a & b` is empty. Distribution makes this exact for
//!   tuples with no tuple-specific code: two same-arity tuples are disjoint iff some element pair
//!   is disjoint, because then no combination mints the same atom; arity mismatch is trivially
//!   disjoint.
//! * [`Universe::contains`]: `contains(sup, sub)` is the subset check `sub <= sup`; `?` propagation
//!   checks containment against the written return union [B2]. For tuples this is exact
//!   element-wise containment after distribution: `(a, b) <= (a | a', b)` because the superset's
//!   combinations cover the subset's.
//! * [`Universe::witness`]: for a non-empty overlap, a concrete witness value type — an atom plus
//!   one level of field/case shape, and recursively for tuple elements [B3] — which B6 overlap
//!   errors must name alongside both heads.
//!
//! Termination: every procedure is grounded in a finite atom universe. Each
//! type denotes a subset of the universe's atoms; solving is a single
//! structural fold over the finite expression tree performing finite set
//! operations — a tuple mints at most the product of its element set sizes,
//! each an existing atom, and a product over the distribution cap is
//! refused as [`crate::error::Error::TupleDistributionCap`] before any atom
//! is minted; emptiness, disjointness, containment, and witness
//! are linear scans over sorted sets, and tuple witnesses recurse into
//! strictly earlier-minted element atoms. No fixpoint iteration exists
//! anywhere, so the procedures decide in time linear in expression size and
//! set size. B6's link-time check re-runs the same procedures over the final
//! closed instance environment, which satisfies section 10 item 7's fixed
//! requirements; [`instance`] assembles that environment (union-head
//! decomposition, conditional instances, the structural cycle check over
//! self-satisfying instances).
//!
//! # Union lowering contract [BL-2]
//!
//! A solved union of atoms lowers to the unboxed tag+payload layout: every
//! member is an atom with a distinct identity (nominal or a minted tuple),
//! so the runtime tag is the discriminant. Layout is owned by `ds-abi` and
//! `ds-lower`; this crate guarantees only that union members are distinct
//! atoms and computes no layout.
//!
//! # Scope
//!
//! M2 type core (nominal atoms and anonymous tuples), traits environment,
//! and package-level name resolution over the parsed surface ([`resolve`],
//! G3/F3). Bidirectional checking and refinement are later work; the type
//! core models declarations as data and exposes the algebra as a library
//! with no I/O. Container types of the corpus (`list[json]`,
//! `map[string, json]`) are not tuples and stay unmodeled.
//!
//! # Example
//!
//! The `cfg_err` shape from the seed corpus (`conformance/types_hybrid.ds`):
//!
//! ```
//! use ds_check::{Field, TypeExpr, Universe};
//!
//! # fn main() -> Result<(), ds_check::Error> {
//! let mut u = Universe::new();
//! let int = u.declare_tag("int")?;
//! let io_err = u.declare_tag("io_err")?;
//! let missing = u.declare_tag("missing")?;
//! let bad_port = u.declare_struct("bad_port")?;
//! u.set_struct_fields(bad_port, vec![Field::named("got", TypeExpr::atom(int))])?;
//!
//! // type cfg_err = io_err | missing | bad_port
//! let cfg_err = u.solve(
//!     &(TypeExpr::atom(io_err)
//!         .union(missing.into())
//!         .union(bad_port.into())),
//! )?;
//!
//! // get_port's return union contains each of its members [B2]; it carries
//! // missing | bad_port from cfg_err but not io_err.
//! let ret = u.solve(
//!     &TypeExpr::atom(int)
//!         .union(missing.into())
//!         .union(bad_port.into()),
//! )?;
//! let missing_only = u.atom_ty(missing)?;
//! assert!(u.contains(ret, missing_only));
//! let io_only = u.atom_ty(io_err)?;
//! assert!(!u.contains(ret, io_only));
//! let int_only = u.atom_ty(int)?;
//! assert!(u.is_disjoint(cfg_err, int_only));
//!
//! // difference erases: (config | cfg_err) - cfg_err is exactly config [B1].
//! let config = u.declare_struct("config")?;
//! let config_ty = u.atom_ty(config)?;
//! let widened = u.union(config_ty, cfg_err);
//! assert_eq!(u.diff(widened, cfg_err), config_ty);
//!
//! // witness names a concrete value type for overlap errors [B6].
//! let w = u.witness(cfg_err)?;
//! assert_eq!(u.render_witness(&w), "io_err");
//! # Ok(())
//! # }
//! ```

#![warn(missing_docs)]

pub mod atom;
pub mod corpus;
pub mod decls;
pub mod effects;
pub mod error;
pub mod expr;
pub mod extraction;
pub mod instance;
pub mod linearity;
mod pattern;
pub mod resolve;
pub mod type_of;
pub mod universe;
pub mod witness;

pub use atom::{AtomDecl, AtomKind, Case, Field, TraitDecl, TraitId, TypeId};
pub use corpus::{ItemStage, Stage};
pub use decls::{Decls, FnSig, GenericParam};
pub use effects::{DeclaredRows, DefEffects, Effect, EffectRow, Effects, effects_of};
pub use error::Error;
pub use expr::TypeExpr;
pub use extraction::Extraction;
pub use instance::{Condition, InstanceBody, InstanceDecl, InstanceEnv, InstanceId};
pub use linearity::{BinderFact, DropSite, LastUse, Linearity, Move, check, linearity_check};
pub use resolve::{
    Def, DefId, DefKind, DotRes, MethodDecl, MethodId, MethodOwner, Package, Res, Resolutions,
    resolve_package,
};
pub use type_of::{
    Analyzed, Checked, TraitCall, TypedPackage, Val, analyze, check_resolved, type_check,
};
pub use universe::{Ty, Universe};
pub use witness::{Witness, WitnessField};
