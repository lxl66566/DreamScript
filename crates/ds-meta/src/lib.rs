//! # ds-meta: the meta layer
//!
//! The derive engine and the analyze entries that apply its expansion: a
//! derive attribute expands into an ordinary impl declaration after
//! parse ([`derive::expand`]), and the expanding [`analyze`] entries fold
//! its refusals into the checker's bundle ahead of the resolution
//! classes, so raw trees enter through this crate and every downstream
//! pass sees only expanded ones.
//!
//! The comptime evaluator that drives the same uop VM is `ds-comptime`.

use ds_check::{Analyzed, type_of::analyze_resolved};
use ds_parse::Ast;

pub mod derive;

pub use derive::{expand as expand_derives, expand_owned};

/// The full bidirectional check over raw trees with the derive
/// expansion applied first: the checker's
/// [`analyze`](ds_check::type_of::analyze) over the expanded trees, the
/// expansion's refusals folded into the bundle ahead of the resolution
/// classes, so a derive-carrying package answers exactly as it always
/// did. A package with no derive attribute expands to an identical
/// clone. `asts` may hold shared trees (`Borrow<Ast>`): the query layer
/// expands its parse memos without cloning the trees.
pub fn analyze<T>(files: &ds_diag::Files, asts: &[T]) -> Analyzed
where
    T: std::borrow::Borrow<Ast>,
{
    // The trees arrive borrowed, so they copy once into the shared
    // shape; a caller holding the trees by value takes `analyze_owned`
    // and skips this copy.
    let owned: Vec<std::sync::Arc<Ast>> = asts
        .iter()
        .map(|ast| std::sync::Arc::new(std::borrow::Borrow::borrow(ast).clone()))
        .collect();
    analyze_owned(files, owned, &[])
}

/// The ownership variant of [`analyze`] for the cold run path: the
/// caller hands the package's trees as shared handles (`Arc::new` moves
/// an owned tree in without copying it), with `preexpanded` marks
/// aligned with them (a marked tree already carries its derive
/// expansion — the ambient blob's decode). The expansion moves every
/// sole-owner tree through without cloning (`Arc::make_mut`), so the
/// bundle adopts the caller's trees; a shared tree carrying expansion
/// work clones once, exactly the copy [`analyze`] pays. An unmarked
/// tree expands exactly like [`analyze`] expands it.
pub fn analyze_owned(
    files: &ds_diag::Files,
    asts: Vec<std::sync::Arc<Ast>>,
    preexpanded: &[bool],
) -> Analyzed {
    let (asts, mut diags) = expand_owned(asts, preexpanded);
    let (package, resolutions, resolve_diags) = ds_check::resolve_package(&asts);
    diags.extend(resolve_diags);
    analyze_resolved(
        files,
        asts.into(),
        package.into(),
        resolutions.into(),
        diags,
        std::sync::Arc::default(),
    )
}

/// [`analyze_owned`] with import surfaces linked (G4): the resolve half
/// runs through [`ds_check::resolve_package_linked`], so an import the
/// graph serves validates and binds — its selected names and module
/// aliases entering the importing file's namespace. A graph that names
/// no import keeps the pass inert and the answer identical to
/// [`analyze_owned`]'s.
pub fn analyze_owned_linked(
    files: &ds_diag::Files,
    asts: Vec<std::sync::Arc<Ast>>,
    preexpanded: &[bool],
    imports: &ds_check::ImportGraph,
) -> Analyzed {
    let (asts, mut diags) = expand_owned(asts, preexpanded);
    let (package, resolutions, resolve_diags) = ds_check::resolve_package_linked(&asts, imports);
    diags.extend(resolve_diags);
    analyze_resolved(
        files,
        asts.into(),
        package.into(),
        resolutions.into(),
        diags,
        // the graph's artifact tables ride the bundle (H5): a bound
        // extern's lowering resolves its declaration against exactly
        // this fold
        std::sync::Arc::new(imports.artifacts().clone()),
    )
}

/// The served variant of [`analyze_owned`] behind a checked-ambient
/// artifact: the ambient member's tree arrives first (marked in
/// `preexpanded`, the blob discipline — a shared handle, so the
/// process's resident tree serves without a clone) with its namespace,
/// per-file resolutions, and declaration-side prefix pre-served, and
/// the user trees after it expand like [`analyze_owned`] expands them,
/// the expansion's refusals folded ahead of the served resolution
/// classes.
pub fn analyze_served(
    files: &ds_diag::Files,
    asts: Vec<std::sync::Arc<Ast>>,
    preexpanded: &[bool],
    package: ds_check::Package,
    resolutions: Vec<ds_check::Resolutions>,
    served: ds_check::ServedDecls,
) -> Analyzed {
    let (asts, diags) = expand_owned(asts, preexpanded);
    ds_check::type_of::analyze_served(files, asts, package, resolutions, served, diags)
}

/// [`analyze_served`] with import surfaces linked (G4): the served
/// prefix serves as ever, and the link pass runs over every tree of the
/// unit — the served trees' import-bound defs re-register at the same
/// tail ids their served resolutions name, so the serve and the fresh
/// path see identical link facts.
pub fn analyze_served_linked(
    files: &ds_diag::Files,
    asts: Vec<std::sync::Arc<Ast>>,
    preexpanded: &[bool],
    package: ds_check::Package,
    resolutions: Vec<ds_check::Resolutions>,
    served: ds_check::ServedDecls,
    imports: &ds_check::ImportGraph,
) -> Analyzed {
    let (asts, diags) = expand_owned(asts, preexpanded);
    ds_check::type_of::analyze_served_linked(
        files,
        asts,
        package,
        resolutions,
        served,
        diags,
        imports,
    )
}
