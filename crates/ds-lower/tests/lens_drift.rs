//! The lens discipline pin: an instance's walk lowers a generic body by
//! reading every checked record through the FnLower lens helpers (the
//! substitution applied at the record's read site) or through the
//! written_* helpers where a dispatch decision intentionally reads the
//! body as its own checking wrote it. This test pins the lowerer's own
//! sources so a new direct `checked.<record>` read outside the helpers
//! drifts loudly here instead of silently lowering a variable's word
//! under an instantiation.

/// The lowerer sources whose walks lower bodies through the lens,
/// scanned alongside the function names allowed to read the checked
/// records directly.
struct Scanned {
    path: &'static str,
    src: &'static str,
    allowed: &'static [&'static str],
}

/// The lowerer files whose walks lower bodies through the lens.
const FILES: &[Scanned] = &[
    // the lens helpers themselves plus the by-design written readers:
    // the plan-time const registration, the discovery-time spawn/capture
    // derivation, the staging decisions (dispatch existence, method
    // candidates, evidence chains, hole demands), and the walk entry's
    // own fn-return derivation (the plan hands an instance its return
    // already substituted)
    Scanned {
        path: "../src/func.rs",
        src: include_str!("../src/func.rs"),
        allowed: &[
            "val",
            "written_val",
            "binder_fact",
            "def_type",
            "trait_call",
            "written_trait_call",
            "is_test",
            "generic_call",
            "written_generic_call_bounds",
            "member_call_params",
            "handler_handled",
            "place_store",
            "push_const",
            "spawn_return",
            "captures_under",
            "inlines_lambda_arg",
            "call",
            "hole_demand",
            "callee_demands",
            "lower",
            "hole_show_dispatch",
        ],
    },
    // the call staging was repointed wholesale except the dispatch
    // existence decisions and the capture scan's span map, which read
    // records as the body's own checking wrote them; the ord-routed sort
    // reads the checker's per-call bound record for the served instance,
    // a decision the lens cannot re-answer (a generic body's element
    // type never carries one at check time)
    Scanned {
        path: "../src/func/calls.rs",
        src: include_str!("../src/func/calls.rs"),
        allowed: &["call_impl", "member_default_exprs", "tail_builds_a_closure"],
    },
    Scanned {
        path: "../src/func/pattern.rs",
        src: include_str!("../src/func/pattern.rs"),
        allowed: &[],
    },
    Scanned {
        path: "../src/func/sort.rs",
        src: include_str!("../src/func/sort.rs"),
        allowed: &["sort_call"],
    },
];

/// The checked record maps whose reads carry a type a body's lowering
/// shapes words by.
const RECORDS: &[&str] = &[
    "expr_types",
    "binders",
    "def_types",
    "trait_calls",
    "is_tests",
    "generic_calls",
    "generic_call_bounds",
    "member_call_params",
    "handler_handled",
    "place_stores",
];

/// One function-name boundary per `fn` declaration line, so a read
/// attributes to the function that contains it.
fn boundaries(src: &str) -> Vec<(usize, &str)> {
    src.lines()
        .enumerate()
        .filter_map(|(line, text)| {
            let mut rest = text.trim_start();
            while let Some(stripped) = rest
                .strip_prefix("pub ")
                .or_else(|| rest.strip_prefix("pub(crate) "))
                .or_else(|| rest.strip_prefix("pub(super) "))
            {
                rest = stripped;
            }
            let rest = rest.strip_prefix("fn ")?;
            let name = rest
                .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
                .next()?;
            Some((line + 1, name))
        })
        .collect()
}

#[test]
fn typed_checked_reads_stay_behind_the_lens_helpers() {
    for file in FILES {
        let path = file.path;
        let allowed = file.allowed;
        let src = file.src;
        // newline-free view so a record access split across lines still
        // reads as one `.checked.<record>` chain, with the line each
        // offset falls on still findable
        let joined = src.replace('\n', " ");
        let mut line_starts = Vec::with_capacity(src.lines().count());
        let mut offset = 0;
        for line in src.split('\n') {
            line_starts.push(offset);
            offset += line.len() + 1;
        }
        let line_of = |at: usize| {
            let (line, _) = line_starts
                .iter()
                .enumerate()
                .rev()
                .find(|(_, start)| **start <= at)
                .expect("every offset falls on a line");
            line + 1
        };
        let bounds = boundaries(src);
        let owner_of = |line: usize| -> &str {
            bounds
                .iter()
                .rev()
                .find(|(start, _)| *start <= line)
                .map_or("", |(_, name)| *name)
        };
        let mut pos = 0;
        while let Some(hit) = joined[pos..].find(".checked") {
            let at = pos + hit;
            let rest = &joined[at + ".checked".len()..];
            pos = at + 1;
            // `.checked .<record>` (split) or `.checked.<record>`
            let Some(tail) = rest.trim_start().strip_prefix('.') else {
                continue;
            };
            let Some(record) = RECORDS.iter().find(|record| {
                tail.starts_with(**record)
                    && !tail[record.len()..]
                        .trim_start()
                        .starts_with(|c: char| c.is_ascii_alphanumeric() || c == '_')
            }) else {
                continue;
            };
            let line = line_of(at);
            let owner = owner_of(line);
            assert!(
                allowed.contains(&owner),
                "{path}:{line} reads checked.{record} directly from `{owner}`, off the lens \
                 helpers; route the read through a val/written_* helper (or extend the by-design \
                 readers here)",
            );
        }
    }
}
