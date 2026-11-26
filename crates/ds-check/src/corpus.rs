//! The conformance corpus's stage expectations: the `// items:` header
//! every corpus file carries, mapping each item to the milestone that
//! owns it.
//!
//! The stages: `typecheck` (the checker must accept the item clean),
//! `run` (the checker accepts it, and the stdlib types what the static
//! check leaves, so a run item may carry its documented leftovers),
//! `reject <what>` (exactly one named rejection is expected), `m<phase>`
//! (the construct lands at that milestone; the checker does not model
//! it), and `unresolved` (commented out in the body, aligning with no
//! live item).
//!
//! One scoping rule serves every consumer: only `typecheck` items gate
//! checking. Diagnostics confined to items staged anything else are
//! corpus-owned noise — the conformance gate pins which and why per
//! file, and the `ds check` driver treats them as non-fatal, so a corpus
//! file with its documented stage layout never fails the command while a
//! plain source file (no header) stays fully fatal.

use ds_diag::Span;
use ds_parse::{Ast, Item};

/// One corpus item's header expectation.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Stage {
    /// The item's header id (its hyphenated corpus name).
    pub id: String,
    /// The stage verbatim: `typecheck`, `run`, `reject <what>`,
    /// `m<phase>`, or `unresolved`.
    pub stage: String,
}

/// Parses the mechanical `// items:` header block. The stages appear in
/// item order and carry no spans; pair them with the parsed items through
/// [`item_stages`].
#[must_use]
pub fn stages(source: &str) -> Vec<Stage> {
    let mut stages = Vec::new();
    let mut in_items = false;
    for line in source.lines() {
        let Some(comment) = line.strip_prefix("// ") else {
            if in_items && !line.trim().is_empty() {
                break;
            }
            continue;
        };
        if comment.trim() == "items:" {
            in_items = true;
        } else if in_items {
            let Some((id, rest)) = comment.split_once(": stage: ") else {
                break;
            };
            stages.push(Stage {
                id: id.trim().to_owned(),
                stage: rest.trim().to_owned(),
            });
        }
    }
    stages
}

/// One item's expectation aligned with its source extent.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ItemStage {
    /// The item's header id.
    pub id: String,
    /// The stage verbatim.
    pub stage: String,
    /// The item's byte span; diagnostics addressing it are the stage's to
    /// own.
    pub span: Span,
}

/// Aligns the header stages with the file's items, in order. Returns
/// `None` when the file has no `// items:` header (a plain source file:
/// nothing is staged) or when header and body disagree (a corrupt header;
/// callers treat the file as unstaged rather than guess an alignment).
/// `unresolved` entries are skipped by every runner (their bodies are
/// comments), so they align with no item.
#[must_use]
pub fn item_stages(source: &str, ast: &Ast) -> Option<Vec<ItemStage>> {
    let stages: Vec<_> = stages(source)
        .into_iter()
        .filter(|stage| !stage.stage.starts_with("unresolved"))
        .collect();
    if stages.is_empty() {
        return None;
    }
    let items: Vec<&Item> = ast.blocks.iter().flat_map(|b| b.items.iter()).collect();
    if items.len() != stages.len() {
        return None;
    }
    Some(
        items
            .into_iter()
            .zip(stages)
            .map(|(item, stage)| ItemStage {
                span: item.span,
                id: stage.id,
                stage: stage.stage,
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An unresolved header entry aligns with no item, so a file carrying
    /// one still stages its live items (the body keeps the unresolved one
    /// as comments).
    #[test]
    fn unresolved_entries_align_with_no_item() {
        let source = "// source: x\n// purpose: y\n// items:\n// a: stage: typecheck\n// b: \
                      stage: unresolved: v2 additive\n// c: stage: run\ntag a\nconst c = a\n";
        let mut files = ds_diag::Files::new();
        let file = files.add_source("m.ds", source);
        let ast = ds_parse::parse(&files, file).expect("parses");
        let staged = item_stages(source, &ast).expect("the live items stage");
        assert_eq!(staged.len(), 2);
        assert_eq!(staged[0].id, "a");
        assert_eq!(staged[0].stage, "typecheck");
        assert_eq!(staged[1].id, "c");
        assert_eq!(staged[1].stage, "run");
    }

    /// Header and body still must agree outside unresolved entries.
    #[test]
    fn a_corrupt_header_stages_nothing() {
        let source = "// source: x\n// purpose: y\n// items:\n// a: stage: typecheck\n// b: \
                      stage: run\ntag a\n";
        let mut files = ds_diag::Files::new();
        let file = files.add_source("m.ds", source);
        let ast = ds_parse::parse(&files, file).expect("parses");
        assert!(item_stages(source, &ast).is_none());
    }
}
