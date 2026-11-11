//! The one pattern-binder collection (F4): every name a binding pattern
//! binds, as (name span, name) pairs in walk order.
//!
//! Declaration-side passes — package def collection, const registration,
//! effect nodes, and the linearity binders — must all collect pattern
//! binders through this walker so their coverage cannot drift (a pun
//! field binds, a case pattern's sub-patterns bind, wildcards and test
//! positions never do). The typing walks in [`crate::type_of`] assign
//! types over the same shapes and are separate by design.

use ds_diag::Span;
use ds_parse::Pattern;

/// Every binder one pattern declares, in walk order.
pub(crate) fn pattern_binders(pat: &Pattern) -> Vec<(Span, &str)> {
    let mut out = Vec::new();
    binders_into(pat, &mut out);
    out
}

fn binders_into<'p>(pat: &'p Pattern, out: &mut Vec<(Span, &'p str)>) {
    match pat {
        Pattern::Binding { span, name } => out.push((*span, name.name.as_str())),
        Pattern::Tuple { pats, .. } | Pattern::Or { pats, .. } => {
            for sub in pats {
                binders_into(sub, out);
            }
        },
        Pattern::Struct { fields, .. } => {
            for (name, sub) in fields {
                match sub {
                    Some(sub) => binders_into(sub, out),
                    // Punning binds the field's name.
                    None => out.push((name.span, name.name.as_str())),
                }
            }
        },
        Pattern::Call { args, .. } => {
            for sub in args {
                binders_into(sub, out);
            }
        },
        Pattern::Wildcard(_)
        | Pattern::Literal { .. }
        | Pattern::Str { .. }
        | Pattern::RawStr { .. }
        | Pattern::Path { .. } => {},
    }
}
