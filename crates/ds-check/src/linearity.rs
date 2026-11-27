//! The affine pass: one intraprocedural forward walk computing
//! definite-initialization and last-use together.
//!
//! # deinit well-formedness
//!
//! The static half: `deinit` is declared on `~Copyable` types only and at
//! most once per struct. Every owned non-copyable binding either moves
//! (ownership, and deinit with it, transfers) or reaches its scope end
//! live, where the walk records the drop site the runtime half must
//! honor on every path — early return, panic, and cancellation included.
use std::{
    collections::{HashMap, HashSet},
    rc::Rc,
    sync::Arc,
};

use ds_diag::{Diagnostic, ErrorCode, Span};
use ds_parse::{
    Ast, BinaryOp, BlockExpr, CallArg, Expr, FnDecl, Ident, Item, ItemKind, LocalDecl, MatchArm,
    Param, ParamMode, Path, Pattern, Stmt, StructDecl, StructMember, TraitMember,
};
use rustc_hash::FxHashMap;

use crate::{
    atom::{AtomKind, Field, TypeId},
    decls::{Decls, FnSig, linear_marker},
    expr::TypeExpr,
    resolve::{DefId, DefKind, DotRes, MethodId, Package, Res, Resolutions},
    type_of::{Checked, TraitCall, Val},
    universe::Ty,
};

mod copyable;
mod state;
mod walk;

pub(crate) use copyable::ty_copyable;
use copyable::*;
use state::*;
use walk::*;

/// Affine facts of one local binder, recorded by the bidirectional walk at
/// the binding site and consumed here.
#[derive(Clone, Copy, Debug)]
pub struct BinderFact {
    /// Whether uses of the binding copy freely; the `~Copyable` marker, a
    /// non-copyable field, or a linear generic argument make it false.
    pub copyable: bool,
    /// The solved type at the binding site: the pass reads it to decide
    /// whether a field projection copies (a scalar read) or transfers
    /// (a linear field).
    pub ty: Ty,
    /// Whether the binding is a `var` (a mut parameter or window binder
    /// counts too): a lambda capturing a mutable binding moves it into
    /// the capture block, so the binding is dead in the enclosing scope
    /// from the construction on.
    pub mutable: bool,
}

/// One consuming use of an owned non-copyable binding: the value flows
/// elsewhere and the binding dies here. A lowerer emits the transfer with
/// no dup and owes no drop for this binding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Move {
    /// The binder's name span.
    pub binder: Span,
    /// The use that consumed it.
    pub at: Span,
}

/// One scope-exit death of a live owned non-copyable binding: where the
/// runtime half must run deinit on every path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DropSite {
    /// The binder's name span.
    pub binder: Span,
    /// The scope exit: block or lambda body end, fn body end, match arm,
    /// or the assignment replacing the value.
    pub at: Span,
}

/// The final use of one tracked binding, borrowed or consuming.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LastUse {
    /// The binder's name span.
    pub binder: Span,
    /// The final use.
    pub at: Span,
    /// Whether the use leaves the binding live (a borrow, or any use of a
    /// copyable binding).
    pub borrowed: bool,
}

/// The affine pass over one package: diagnostics plus the placement facts
/// a lowerer relies on.
#[derive(Default, Debug, Clone)]
pub struct Linearity {
    /// Diagnostics in walk order.
    pub diags: Vec<Diagnostic>,
    /// Consuming uses of owned non-copyable bindings.
    pub moves: Vec<Move>,
    /// Scope-exit deaths of live owned non-copyable bindings.
    pub drops: Vec<DropSite>,
    /// The final use of every tracked binding.
    pub last_uses: Vec<LastUse>,
}

/// The full static check of one package: bidirectional types then the
/// affine pass. Callers wanting every diagnostic read both lists.
pub fn check<T>(files: &ds_diag::Files, asts: &[T]) -> (Checked, Linearity)
where
    T: std::borrow::Borrow<Ast>,
{
    let crate::type_of::Analyzed {
        asts: expanded,
        package,
        resolutions,
        decls,
        artifacts: _,
        checked,
        ..
    } = crate::type_of::analyze(files, asts);
    let lin = linearity_check(&expanded, &package, &resolutions, &decls, &checked);
    // The bundle's refcount is one here, so the unwrap moves the tables.
    (Rc::unwrap_or_clone(checked), lin)
}

/// Runs the affine pass over a package whose bidirectional check already
/// ran: `checked` supplies binder facts and top-level types, the tables
/// come from the same walk (see `type_of::analyze`).
#[must_use]
pub fn linearity_check<T>(
    asts: &[T],
    package: &Package,
    resolutions: &[Resolutions],
    decls: &Decls,
    checked: &Checked,
) -> Linearity
where
    T: std::borrow::Borrow<Ast>,
{
    let mut lin = Lin {
        package,
        decls,
        facts: &checked.binders,
        def_types: &checked.def_types,
        copy_counts: comptime_copy_counts(checked),
        res: resolutions.first(),
        expr_types: &checked.expr_types,
        hole_dispatches: &checked.trait_calls,
        hole_inherents: &checked.inherent_calls,
        hole_runtime: &checked.hole_show_dispatches,
        out: Linearity::default(),
        state: FxHashMap::default(),
        undo: Vec::new(),
        info: FxHashMap::default(),
        scopes: vec![Vec::new()],
        audit: false,
        reported: HashSet::new(),
        last_use: FxHashMap::default(),
        capture_sink: None,
        epoch: 0,
        escaping: false,
    };
    // Two package-wide passes: every file's top-level binder
    // patterns come live before any initializer or body walks — bodied
    // top-level `fn`s are const-of-lambda Local items, so bodies walk in
    // the same pass as plain initializers, and a per-file interleave
    // would silently skip the move check whenever the consuming file is
    // processed before the declaring one.
    for (ast, res) in asts.iter().zip(resolutions.iter()) {
        lin.res = Some(res);
        lin.register_top_bindings(std::borrow::Borrow::borrow(ast));
    }
    for (ast, res) in asts.iter().zip(resolutions.iter()) {
        lin.res = Some(res);
        lin.top_level_and_bodies(std::borrow::Borrow::borrow(ast));
    }
    let mut last_uses: Vec<LastUse> = lin
        .last_use
        .iter()
        .map(|(place, &(at, borrowed))| LastUse {
            binder: place.binder_span(package),
            at,
            borrowed,
        })
        .collect();
    last_uses.sort_by_key(|use_| use_.binder.start);
    lin.out.last_uses = last_uses;
    lin.out
}

/// The parked affine facts over one package's served prefix: the prefix
/// files' output rows plus the walk state their files left behind — the
/// definition places' static facts, entry states, and last-use entries.
/// The first walk a process runs over the served trees fills the
/// [`Park`](crate::Park) cell; every later compile installs it and walks
/// only its own files. The rows name spans and the park's own definition
/// ids alone (each serve rebuilds the namespace identically, so the
/// prefix's ids are the same every compile).
#[derive(Debug)]
pub struct ServedLinearity {
    /// The prefix files' rows, in walk order.
    out: Linearity,
    /// The definition places the prefix registered or walked: static
    /// facts (the per-compile `info` half), names re-borrowed from the
    /// installing compile's package.
    defs: FxHashMap<DefId, DefFact>,
    /// The definition places' states at the prefix boundary — the entry
    /// state the compiled files' walk continues from.
    states: FxHashMap<DefId, State>,
    /// The last use recorded inside the prefix, per place: a compiled
    /// file's use of a served binding overwrites its entry, exactly as
    /// the one-walk order does.
    last_use: FxHashMap<Place, (Span, bool)>,
}

/// One served definition place's parked static facts.
#[derive(Clone, Copy, Debug)]
struct DefFact {
    copyable: bool,
    owned: bool,
}

/// Runs the affine pass over a package whose first `served_files` trees
/// arrive served behind a walk park: the first process run walks the
/// served files and parks their facts in `park` (one walk total — the
/// snapshot sits at the file boundary, before any compiled file walks),
/// later runs install the park and walk only the remaining files,
/// prepending the parked rows to their own. A caller without a served
/// prefix uses [`linearity_check`]; the cell belongs to the serve that
/// fills it, so two serves never share a park.
#[must_use]
pub fn linearity_check_served<T>(
    asts: &[T],
    package: &Package,
    resolutions: &[Resolutions],
    decls: &Decls,
    checked: &Checked,
    served_files: usize,
    park: &crate::Park<ServedLinearity>,
) -> Linearity
where
    T: std::borrow::Borrow<Ast>,
{
    let mut lin = Lin {
        package,
        decls,
        facts: &checked.binders,
        def_types: &checked.def_types,
        copy_counts: comptime_copy_counts(checked),
        res: resolutions.first(),
        expr_types: &checked.expr_types,
        hole_dispatches: &checked.trait_calls,
        hole_inherents: &checked.inherent_calls,
        hole_runtime: &checked.hole_show_dispatches,
        out: Linearity::default(),
        state: FxHashMap::default(),
        undo: Vec::new(),
        info: FxHashMap::default(),
        scopes: vec![Vec::new()],
        audit: false,
        reported: HashSet::new(),
        last_use: FxHashMap::default(),
        capture_sink: None,
        epoch: 0,
        escaping: false,
    };
    let parked = park.get();
    let mut prefix = None;
    if let Some(served) = parked {
        lin.install_served(served, package);
        prefix = Some(served.out.clone());
    } else {
        // The filling walk keeps going through the compiled files
        // below, so its total cost is the one plain walk's.
        for (ast, res) in asts.iter().zip(resolutions.iter()).take(served_files) {
            lin.res = Some(res);
            lin.register_top_bindings(std::borrow::Borrow::borrow(ast));
        }
        for (ast, res) in asts.iter().zip(resolutions.iter()).take(served_files) {
            lin.res = Some(res);
            lin.top_level_and_bodies(std::borrow::Borrow::borrow(ast));
        }
        let _ = park.fill(lin.snapshot_prefix());
    }
    for (ast, res) in asts.iter().zip(resolutions.iter()).skip(served_files) {
        lin.res = Some(res);
        lin.register_top_bindings(std::borrow::Borrow::borrow(ast));
    }
    for (ast, res) in asts.iter().zip(resolutions.iter()).skip(served_files) {
        lin.res = Some(res);
        lin.top_level_and_bodies(std::borrow::Borrow::borrow(ast));
    }
    let mut last_uses: Vec<LastUse> = lin
        .last_use
        .iter()
        .map(|(place, &(at, borrowed))| LastUse {
            binder: place.binder_span(package),
            at,
            borrowed,
        })
        .collect();
    last_uses.sort_by_key(|use_| use_.binder.start);
    lin.out.last_uses = last_uses;
    match prefix {
        Some(mut prefix) => {
            // The prefix's rows come first in walk order; the fresh rows
            // follow. Last uses were already merged per place by the
            // install, so the sorted concatenation is the one walk's.
            prefix.moves.append(&mut lin.out.moves);
            prefix.drops.append(&mut lin.out.drops);
            prefix.diags.append(&mut lin.out.diags);
            prefix.last_uses.append(&mut lin.out.last_uses);
            prefix.last_uses.sort_by_key(|use_| use_.binder.start);
            prefix
        },
        None => lin.out,
    }
}
/// The comptime copies each loop span audits: a written loop's own
/// field count, a per-instance loop's largest expansion (one
/// representative copy minimum — an uninstantiated loop audits its
/// body once), and a written loop nested inside a per-instance copy
/// the expansion's count. Every expansion unrolls the same structural
/// body over the same copy discipline, so auditing the largest count
/// covers each entering shape's copies exactly.
#[must_use]
pub fn comptime_copy_counts(checked: &Checked) -> HashMap<Span, usize> {
    let mut counts: HashMap<Span, usize> = HashMap::default();
    for (span, info) in &checked.comptime_loops {
        let copies = if info.per_instance.is_some() { 1 } else { info.fields.len() };
        counts.insert(*span, copies);
    }
    for ((.., span), info) in &checked.comptime_instance_loops {
        let entry = counts.entry(*span).or_insert(0);
        *entry = (*entry).max(info.fields.len());
    }
    counts
}
// ---- helpers ------------------------------------------------------------------

fn moved_diag(name: &str, at: Span, moved_at: Span) -> Diagnostic {
    if moved_at == at {
        Diagnostic::error(
            ErrorCode::MovedThenUsed,
            format!("value `{name}` is moved again on a re-executing path"),
        )
        .with_primary(at, "moved here")
        .with_note(format!(
            "`{name}` is `~Copyable`; re-initialize it before the path re-executes"
        ))
    } else {
        Diagnostic::error(
            ErrorCode::MovedThenUsed,
            format!("value `{name}` is no longer usable"),
        )
        .with_secondary(moved_at, "moved here")
        .with_primary(at, "used again here after move")
        .with_note(format!("`{name}` is `~Copyable` and was consumed"))
    }
}
#[cfg(test)]
mod tests {
    //! No surface form defers initialization (every binding carries an
    //! initializer and the resolver binds in flow order), so the
    //! uninitialized lattice base has no source-level test. This exercises
    //! it directly by placing a binder in the uninitialized state: DS3002
    //! fires on the first read.

    use super::*;

    #[test]
    fn an_uninitialized_read_reports_ds3002() {
        let mut files = ds_diag::Files::new();
        let file = files.add_source("t.ds", "tag int\nfn go(p: int) -> int { p }\n");
        let ast = ds_parse::parse(&files, file).expect("parses");
        let crate::type_of::Analyzed {
            package,
            resolutions,
            decls,
            checked,
            ..
        } = crate::type_of::analyze(&files, &[ast]);
        let [(&binder, &BinderFact { copyable, .. })] =
            checked.binders.iter().collect::<Vec<_>>()[..]
        else {
            panic!("one parameter binder, got {:#?}", checked.binders);
        };
        let place = Place::Local(binder);
        let mut lin = Lin {
            package: &package,
            decls: &decls,
            facts: &checked.binders,
            def_types: &checked.def_types,
            copy_counts: HashMap::default(),
            res: resolutions.first(),
            expr_types: &checked.expr_types,
            hole_dispatches: &checked.trait_calls,
            hole_inherents: &checked.inherent_calls,
            hole_runtime: &checked.hole_show_dispatches,
            out: Linearity::default(),
            state: FxHashMap::from_iter([(place, State::Uninit)]),
            undo: Vec::new(),
            info: FxHashMap::from_iter([(place, Info {
                name: "p",
                copyable,
                owned: true,
                epoch: 0,
            })]),
            scopes: vec![Vec::new()],
            audit: false,
            reported: HashSet::new(),
            last_use: FxHashMap::default(),
            capture_sink: None,
            epoch: 0,
            escaping: false,
        };
        lin.read(place, binder, false);
        let [diag] = lin.out.diags.as_slice() else {
            panic!("one uninit diagnostic, got {:#?}", lin.out.diags);
        };
        assert_eq!(diag.code.as_str(), "DS3002");
        assert_eq!(
            diag.message,
            "binding `p` may not be initialized on this path"
        );
        assert_eq!(diag.labels[0].label, "may be uninitialized here");
        assert_eq!(diag.labels[1].label, "bound here");
        assert_eq!(diag.labels[1].span, binder);
    }
}
