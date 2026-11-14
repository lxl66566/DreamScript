//! The one pattern-binder collection (F4): every name a binding pattern
//! binds, as (name span, name) pairs in walk order.
//!
//! Declaration-side passes — package def collection, const registration,
//! effect nodes, and the linearity binders — must all collect pattern
//! binders through this walker so their coverage cannot drift (a pun
//! field binds, a case pattern's sub-patterns bind, wildcards and test
//! positions never do). The typing walks in [`crate::type_of`] assign
//! types over the same shapes and are separate by design. The arm walk
//! (F9) shares the recursion and adds the member-test rule: in a
//! match-arm pattern a bare identifier naming a constructor type or the
//! predeclared `none` is a member test that binds nothing.

use ds_diag::Span;
use ds_parse::Pattern;
use rustc_hash::FxHashSet;

/// Every binder one pattern declares, in walk order.
pub(crate) fn pattern_binders(pat: &Pattern) -> Vec<(Span, &str)> {
    let mut out = Vec::new();
    binders_into(pat, None, &mut out);
    out
}

/// Every binder one match-arm pattern declares: like
/// [`pattern_binders`], except a bare identifier naming a member test
/// (`member_tests`) binds nothing; punned fields bind regardless, since
/// they name fields, never types.
pub(crate) fn arm_pattern_binders<'p>(
    pat: &'p Pattern,
    member_tests: &FxHashSet<&'p str>,
) -> Vec<(Span, &'p str)> {
    let mut out = Vec::new();
    binders_into(pat, Some(member_tests), &mut out);
    out
}

/// `member_tests` is `Some` in the arm walk: there a bare identifier
/// naming a member test binds nothing (F9); `None` walks the declaration
/// shapes, where every `Binding` binds.
fn binders_into<'p>(
    pat: &'p Pattern,
    member_tests: Option<&FxHashSet<&'p str>>,
    out: &mut Vec<(Span, &'p str)>,
) {
    match pat {
        Pattern::Binding { span, name } => {
            if !member_tests.is_some_and(|tests| tests.contains(name.name.as_str())) {
                out.push((*span, name.name.as_str()));
            }
        },
        Pattern::Tuple { pats, .. } | Pattern::Or { pats, .. } => {
            for sub in pats {
                binders_into(sub, member_tests, out);
            }
        },
        Pattern::Struct { fields, .. } => {
            for (name, sub) in fields {
                match sub {
                    Some(sub) => binders_into(sub, member_tests, out),
                    // Punning binds the field's name.
                    None => out.push((name.span, name.name.as_str())),
                }
            }
        },
        Pattern::Call { args, .. } => {
            for sub in args {
                binders_into(sub, member_tests, out);
            }
        },
        Pattern::Wildcard(_)
        | Pattern::Literal { .. }
        | Pattern::Str { .. }
        | Pattern::RawStr { .. }
        | Pattern::Path { .. } => {},
    }
}
