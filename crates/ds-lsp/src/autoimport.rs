//! The auto-import completion tier: the edit that binds a name through
//! the file's `@std` import surface, attached to the scope answer's items
//! when the typed prefix names no visible name.
//!
//! The candidate set composes the registered ambient's exported members
//! with the `@std` package's own members: the production registration is
//! the session composition (the run path's implicit-resolution shape), so
//! the migrated faces ride inside the ambient while the data-shaped
//! members — like a registration over the shrunk core (a declaring
//! graph's ambient) — stay link-bound, and the table serves their offers
//! alone, each member joining the answer as its own item. The table is the
//! tree's own sources — the package's member file resolved beside the
//! core, and the migrated json and string faces composed into the @std
//! bundle's core member (their free seam calls stay same-file there) —
//! the reserved package's members are a toolchain constant like the
//! ambient itself, never a served dep artifact. The visibility gate is
//! the checker's own export predicate ([`ds_check::Vis::exported`], the
//! same one an `@std` import selection validates against): a private or
//! `pub(pkg)` name is never a candidate, so no offer ever imports one.
//!
//! The edit merges into a plain `@std` row when the file carries one — a
//! selection appended after the last entry, or a selection list added to
//! a bare row — else inserts a fresh row placed per the corpus's
//! prevailing style: grouped after the last import row, or ahead of the
//! first item past its doc comment. A row that already selects the name
//! answers no edit at all.

use std::sync::LazyLock;

use ds_check::{DefKind, Package};
use ds_diag::{FileId, Files};
use ds_parse::{Ast, ImportDecl, Item, ItemKind};
use tower_lsp::lsp_types as lsp;

use crate::convert;

/// Which namespace a completion position serves: the tier filters its
/// candidates by it, since a type position imports type names alone.
#[derive(Clone, Copy)]
pub(crate) enum Namespace {
    /// A plain name position: every namespace's members serve.
    Value,
    /// A type position: the type namespace's members alone.
    Type,
}

impl Namespace {
    /// Whether one definition kind's members serve the namespace.
    fn serves(self, kind: DefKind) -> bool {
        match self {
            Self::Value => true,
            Self::Type => kind.in_type_namespace(),
        }
    }
}

/// The inputs one attach call reads: the document being completed and
/// the package tables its candidates come from.
pub(crate) struct Inputs<'a> {
    /// The registry's source coordinates.
    pub(crate) files: &'a Files,
    /// The document being completed.
    pub(crate) file: FileId,
    /// The document's current text.
    pub(crate) source: &'a str,
    /// The document's parse tree (its import rows).
    pub(crate) ast: &'a Ast,
    /// The resolved package tables.
    pub(crate) package: &'a Package,
    /// The ambient's file id: the candidate set's home.
    pub(crate) ambient: FileId,
}

/// Attaches the import edit to every item naming an exported ambient
/// member whose spelling prefix-matches the typed prefix, then to the
/// `@std` package's own members over the same gate: a registration that
/// already lists them (the session composition lists every face) sees
/// the table enrich the same items, a link-bound shape joins each as its
/// own item. Accepting an offer inserts the name
/// and applies the edit alongside; a row that already selects the name
/// answers no edit, and a private or `pub(pkg)` name is never a
/// candidate.
pub(crate) fn attach(
    items: &mut Vec<lsp::CompletionItem>,
    inputs: &Inputs<'_>,
    prefix: &str,
    namespace: Namespace,
) {
    let Inputs {
        files,
        file,
        source,
        ast,
        package,
        ambient,
    } = *inputs;
    for def in package.defs() {
        if def.span.file != ambient
            || !def.vis.exported()
            || !namespace.serves(def.kind)
            || !def.name.starts_with(prefix)
        {
            continue;
        }
        // A row that already selects the name answers no edit: the file
        // imports it.
        let Some((start, end, text)) = row_edit(source, ast, &def.name) else {
            continue;
        };
        let edit = lsp::TextEdit {
            range: lsp::Range::new(
                convert::position(files, file, start),
                convert::position(files, file, end),
            ),
            new_text: text,
        };
        for item in items.iter_mut() {
            // One label may serve two namespaces (`struct map` and
            // `fn map`): both items are the same name's spellings, and
            // either acceptance inserts the one row.
            if item.label == def.name {
                item.additional_text_edits = Some(vec![edit.clone()]);
            }
        }
    }
    for member in std_members() {
        if !namespace.serves(member.kind) || !member.name.starts_with(prefix) {
            continue;
        }
        let Some((start, end, text)) = row_edit(source, ast, &member.name) else {
            continue;
        };
        let edit = lsp::TextEdit {
            range: lsp::Range::new(
                convert::position(files, file, start),
                convert::position(files, file, end),
            ),
            new_text: text,
        };
        match items.iter_mut().find(|item| item.label == member.name) {
            // A registration that already lists the member (the session
            // composition lists every face) enriches it: the same
            // enrichment the ambient's own defs gave it.
            Some(item) => item.additional_text_edits = Some(vec![edit]),
            // A core-shaped registration lists none of them: the member
            // joins the answer as its own item, the offer the import row
            // binds the name through.
            None => items.push(lsp::CompletionItem {
                label: member.name.clone(),
                kind: Some(convert::completion_kind(member.kind.into())),
                additional_text_edits: Some(vec![edit]),
                ..lsp::CompletionItem::default()
            }),
        }
    }
}

/// The `@std` package's exported members, the composed candidate set's
/// other half: each member's spelling and the definition kind its
/// declaration mints.
struct StdMember {
    /// The declared name.
    name: String,
    /// What the declaration defines: the namespace gate and the item kind
    /// read it.
    kind: DefKind,
}

/// The `@std` package's own members from the tree's sources, built once
/// per process: the members' file resolved beside the shrunk core, and
/// the migrated json and string faces read out of the bundle's own
/// compile shape — the reserved package's own registrations. The
/// session's in-process knowledge of the reserved package, mirroring
/// the ambient's own-parse precedent: a served dep artifact never feeds
/// the tier.
fn std_members() -> &'static [StdMember] {
    static MEMBERS: LazyLock<Vec<StdMember>> = LazyLock::new(|| {
        let mut members = exported_std_members(
            ds_stdlib::AMBIENT_CORE_SOURCE,
            ds_stdlib::STD_MEMBERS_SOURCE,
        );
        members.extend(exported_face_members(
            ds_stdlib::AMBIENT_CORE_SOURCE,
            ds_stdlib::BUNDLE_CORE_SOURCE,
            ds_stdlib::CODEC_SOURCE,
        ));
        members
    });
    &MEMBERS
}

/// One members-source pair's exported members: the core resolved ahead
/// of the members' file, the exported defs of the members' file alone —
/// the checker's own rows, so the export gate is the one an `@std`
/// selection validates against. A source pair that does not resolve is
/// a broken tree, a generation-side failure.
fn exported_std_members(core: &str, members: &str) -> Vec<StdMember> {
    let mut files = Files::new();
    let core_file = files.add_source(ds_stdlib::AMBIENT_FILE, core);
    let members_file = files.add_source(ds_stdlib::STD_MEMBERS_FILE, members);
    let core_ast = ds_parse::parse(&files, core_file).expect("the ambient core parses");
    let members_ast = ds_parse::parse(&files, members_file).expect("the members parse");
    let (package, _, diags) = ds_check::resolve_package(&[core_ast, members_ast]);
    assert!(diags.is_empty(), "the members' source resolves: {diags:?}");
    package
        .defs()
        .filter(|def| def.span.file == members_file && def.vis.exported())
        .map(|def| StdMember {
            name: def.name.clone(),
            kind: def.kind,
        })
        .collect()
}

/// The migrated faces' exported members: the json and string faces ride
/// composed into the @std bundle's core member (the json face's free
/// calls into the core's private collection externs stay same-file with
/// them, and its driver impls name the codec face's traits), so their
/// names read out of the bundle's own compile shape — `composed` (the
/// bundle core, beginning with `core` verbatim) resolved beside the
/// codec file, the exported defs past the core's own extent alone, the
/// boundary the composing separator sits at. The same export gate and
/// the same generation-side failure discipline as the pair shape above.
fn exported_face_members(core: &str, composed: &str, codec: &str) -> Vec<StdMember> {
    let mut files = Files::new();
    let composed_file = files.add_source(ds_stdlib::AMBIENT_FILE, composed);
    let codec_file = files.add_source(ds_stdlib::CODEC_FILE, codec);
    let composed_ast = ds_parse::parse(&files, composed_file).expect("the bundle core parses");
    let codec_ast = ds_parse::parse(&files, codec_file).expect("the codec face parses");
    let (package, _, diags) = ds_check::resolve_package(&[composed_ast, codec_ast]);
    assert!(diags.is_empty(), "the composed shape resolves: {diags:?}");
    let boundary = u32::try_from(core.len()).expect("the core fits u32 byte offsets");
    package
        .defs()
        .filter(|def| {
            def.span.file == composed_file && def.span.start > boundary && def.vis.exported()
        })
        .map(|def| StdMember {
            name: def.name.clone(),
            kind: def.kind,
        })
        .collect()
}

/// The file's import rows in source order, with their whole items.
fn rows(ast: &Ast) -> impl Iterator<Item = (&Item, &ImportDecl)> {
    ast.blocks
        .iter()
        .flat_map(|block| block.items.iter())
        .filter_map(|item| match &item.kind {
            ItemKind::Import(imp) => Some((item, imp)),
            _ => None,
        })
}

/// Whether one row's path names `@std` (the merged reading serves every
/// `@std`-sigiled path against the same surface, its further segments
/// advisory).
fn names_std(imp: &ImportDecl) -> bool {
    imp.path.at_sign && imp.path.segments.first().is_some_and(|s| s.name == "std")
}

/// The edit binding `name` through the file's `@std` import surface, as
/// byte offsets into `source` plus the inserted text, or `None` when the
/// file's `@std` rows already select the name (under its own spelling or
/// an alias) — nothing to import.
fn row_edit(source: &str, ast: &Ast, name: &str) -> Option<(u32, u32, String)> {
    let all: Vec<(&Item, &ImportDecl)> = rows(ast).collect();
    if all.iter().any(|(_, imp)| {
        names_std(imp)
            && imp.select.iter().any(|sel| {
                sel.name.name == name || sel.alias.as_ref().is_some_and(|a| a.name == name)
            })
    }) {
        return None;
    }
    // A merge needs a plain row: an alias or condition changes what a
    // selection would mean, and a visibility modifier is already a
    // refusal row (`pub import` names no re-export).
    let plain = all.iter().find(|(item, imp)| {
        names_std(imp) && imp.alias.is_none() && imp.cond.is_none() && item.vis.is_none()
    });
    if let Some((_, imp)) = plain {
        return match imp.select.last() {
            // After the last selection entry, ahead of any trailing
            // comment the row carries.
            Some(last) => Some((last.span.end, last.span.end, format!(", {name}"))),
            // A bare row gains its selection list right after the path.
            None => Some((imp.path.span.end, imp.path.span.end, format!(" ({name})"))),
        };
    }
    Some(fresh_row(source, ast, name))
}

/// The fresh-row edit: grouped after the file's last import row when one
/// exists, else ahead of the first item — past its doc comment, which an
/// anchor at the item's own start would steal — with a blank line
/// separating the row from the declaration it precedes. An empty or
/// comment-only file opens with the row.
fn fresh_row(source: &str, ast: &Ast, name: &str) -> (u32, u32, String) {
    let row = format!("import @std ({name})\n");
    if let Some(end) = rows(ast).last().map(|(item, _)| item.span.end) {
        // The anchor is the line break that ends the last row's line; a
        // file ending without one appends the row after a fresh break.
        return match source[end as usize..].find('\n') {
            Some(nl) => {
                let at = end + u32::try_from(nl).expect("an offset fits u32") + 1;
                (at, at, row)
            },
            None => (end, end, format!("\n{row}")),
        };
    }
    let first = ast
        .blocks
        .iter()
        .flat_map(|block| block.items.iter())
        .find(|item| !item.synthesized);
    if let Some(first) = first {
        let start = first
            .doc
            .first()
            .map_or(first.span.start, |doc| doc.span.start)
            .min(first.span.start);
        return (start, start, format!("{row}\n"));
    }
    (0, 0, row)
}

#[cfg(test)]
mod tests {
    use ds_check::Vis;

    use super::*;

    /// Parses one source into the AST the edit machinery reads.
    fn ast(source: &str) -> Ast {
        let mut files = Files::new();
        let file = files.add_source("probe.ds", source);
        ds_parse::parse(&files, file).expect("the probe source parses")
    }

    /// The edit for one name as `(start, end, text)` for quick asserts.
    fn edit(source: &str, name: &str) -> (u32, u32, String) {
        row_edit(source, &ast(source), name).expect("the name needs a row")
    }

    /// Offsets equal and text set means insertion: assert the triple
    /// compactly.
    fn inserted(source: &str, name: &str) -> (u32, String) {
        let (start, end, text) = edit(source, name);
        assert_eq!(start, end, "an insertion edit is empty-ranged");
        (start, text)
    }

    #[test]
    fn a_fresh_row_opens_the_file_ahead_of_the_first_item() {
        // No import rows: the row lands ahead of the first item, blank
        // line included.
        let source = "fn probe() -> string {\n  to_upper\n}\n";
        let (at, text) = inserted(source, "to_upper");
        assert_eq!((at, text.as_str()), (0, "import @std (to_upper)\n\n"));
        // A doc comment leads the first item: the row lands ahead of the
        // doc, never between it and its item.
        let source = "/// the probe's doc\nfn probe() {\n}\n";
        let (at, text) = inserted(source, "to_upper");
        assert_eq!((at, text.as_str()), (0, "import @std (to_upper)\n\n"));
    }

    #[test]
    fn a_fresh_row_groups_after_the_last_import_row() {
        // A bare-path row (never an @std merge target) groups the fresh
        // row after itself on the next line.
        let source = "import acme/pipeline/schema (row)\n\nfn probe() {\n}\n";
        let (at, text) = inserted(source, "to_upper");
        assert_eq!((at, text.as_str()), (34, "import @std (to_upper)\n"));
        // A file whose last row ends without a newline appends after a
        // fresh break.
        let source = "import acme/pipeline/schema (row)";
        let (at, text) = inserted(source, "to_upper");
        assert_eq!((at, text.as_str()), (33, "\nimport @std (to_upper)\n"));
    }

    #[test]
    fn a_selection_merges_into_a_plain_std_row() {
        // After the last selection entry, ahead of a trailing comment.
        let source = "import @std/io (println) // the row's note\n";
        let (start, end, text) = edit(source, "to_upper");
        assert_eq!((start, end, text.as_str()), (23, 23, ", to_upper"));
        // A bare @std row gains its selection list after the path.
        let source = "import @std\n";
        let (start, end, text) = edit(source, "to_upper");
        assert_eq!((start, end, text.as_str()), (11, 11, " (to_upper)"));
        // A deeper @std path is the same row under the merged reading.
        let source = "import @std/csv\n";
        let (start, end, text) = edit(source, "to_upper");
        assert_eq!((start, end, text.as_str()), (15, 15, " (to_upper)"));
    }

    #[test]
    fn an_aliased_or_conditional_row_never_takes_a_merge() {
        // An aliased row is not a merge target: the fresh row groups
        // after it instead.
        let source = "import @std as s\n";
        let (at, text) = inserted(source, "to_upper");
        assert_eq!((at, text.as_str()), (17, "import @std (to_upper)\n"));
        // A conditional row is not one either.
        let source = "import @std if os == \"windows\"\n";
        let (at, text) = inserted(source, "to_upper");
        assert_eq!((at, text.as_str()), (31, "import @std (to_upper)\n"));
    }

    #[test]
    fn a_name_the_rows_already_select_needs_no_edit() {
        for source in [
            "import @std (to_upper)\n",
            "import @std/io (println, to_upper)\n",
            // A selection's alias binds the name just as well.
            "import @std (trim as to_upper)\n",
        ] {
            assert_eq!(row_edit(source, &ast(source), "to_upper"), None, "{source}");
        }
    }

    #[test]
    fn the_visibility_gate_reads_the_checker_export_predicate() {
        // The gate is `Vis::exported` exactly: `pub` alone passes, and
        // the private and `pub(pkg)` tiers never do — the same predicate
        // the checker's own @std import validation reads.
        assert!(Vis::Pub.exported());
        assert!(!Vis::Pkg.exported());
        assert!(!Vis::Private.exported());
    }

    #[test]
    fn the_std_members_table_carries_the_exported_members() {
        // The tree's own members: the demo residue's four names, kinds
        // and all — the table the composed candidate set reads.
        let members = exported_std_members(
            ds_stdlib::AMBIENT_CORE_SOURCE,
            ds_stdlib::STD_MEMBERS_SOURCE,
        );
        let rows: Vec<(&str, DefKind)> = members
            .iter()
            .map(|member| (member.name.as_str(), member.kind))
            .collect();
        assert_eq!(rows, [
            ("row", DefKind::TypeAlias),
            ("config", DefKind::Struct),
            ("response", DefKind::Struct),
            ("urls", DefKind::Const),
        ]);
        // The table the tier reads composes the members' file with the
        // migrated faces: every half's names ride it.
        let names: Vec<&str> = std_members()
            .iter()
            .map(|member| member.name.as_str())
            .collect();
        for name in ["urls", "parse_json", "to_upper", "replace"] {
            assert!(names.contains(&name), "`{name}` rides the table");
        }
    }

    #[test]
    fn the_std_members_table_carries_the_composed_faces() {
        // The migrated json and string faces: every exported name, kinds
        // and all, read out of the bundle's own compile shape — the
        // table's composed half, source order within the faces. The json
        // face's bodied declarations are const-of-lambda bindings,
        // the string face's extern entries fns.
        let members = exported_face_members(
            ds_stdlib::AMBIENT_CORE_SOURCE,
            ds_stdlib::BUNDLE_CORE_SOURCE,
            ds_stdlib::CODEC_SOURCE,
        );
        let rows: Vec<(&str, DefKind)> = members
            .iter()
            .map(|member| (member.name.as_str(), member.kind))
            .collect();
        assert_eq!(rows, [
            ("parse_json", DefKind::Const),
            ("to_json", DefKind::Const),
            ("json_member", DefKind::Const),
            ("json_as_int", DefKind::Const),
            ("json_as_float", DefKind::Const),
            ("json_as_bool", DefKind::Const),
            ("json_as_string", DefKind::Const),
            ("json_as_array", DefKind::Const),
            ("json_as_object", DefKind::Const),
            ("json_as_byte", DefKind::Const),
            ("json_as_u32", DefKind::Const),
            ("json_as_char", DefKind::Const),
            ("json_as_bytes", DefKind::Const),
            ("json_of_byte", DefKind::Const),
            ("json_of_u32", DefKind::Const),
            ("json_of_char", DefKind::Const),
            ("json_of_bytes", DefKind::Const),
            ("join", DefKind::Fn),
            ("trim", DefKind::Fn),
            ("parse_int", DefKind::Fn),
            ("parse_float", DefKind::Fn),
            ("split", DefKind::Fn),
            ("substring", DefKind::Fn),
            ("to_upper", DefKind::Fn),
            ("to_lower", DefKind::Fn),
            ("char_at", DefKind::Fn),
            ("contains", DefKind::Fn),
            ("index_of", DefKind::Fn),
            ("replace", DefKind::Fn),
            ("hex", DefKind::Const),
            ("pad_left", DefKind::Const),
            ("pad_right", DefKind::Const),
            ("string_view", DefKind::Struct),
            ("view", DefKind::Const),
            ("view_len", DefKind::Const),
            ("view_char_at", DefKind::Const),
            ("view_slice", DefKind::Const),
            ("view_eq", DefKind::Const),
            ("view_text", DefKind::Const),
        ]);
    }

    #[test]
    fn the_faces_table_reads_past_the_core_boundary_alone() {
        // The boundary is the whole filter's core half: a composed core
        // whose tail carries one exported and one private face member
        // answers the exported tail name alone — the core's own names
        // stay out (the spans never pass the boundary) and the
        // unexported tail name with them (the gate).
        let core = "pub tag int\nfn core_helper() -> int {\n  1\n}\n";
        let tail = "fn face_helper() -> int {\n  1\n}\npub fn face_call(n: int) -> int { n }\n";
        let composed = format!("{core}\n{tail}");
        let codec = "pub trait enc {\nfn e(self) -> int\n}\n";
        let members = exported_face_members(core, &composed, codec);
        let names: Vec<&str> = members.iter().map(|member| member.name.as_str()).collect();
        assert_eq!(names, ["face_call"]);
    }

    #[test]
    fn the_members_table_never_carries_an_unexported_name() {
        // The export gate holds at the table: a private or `pub(pkg)`
        // member of the members' file is no candidate, so no offer ever
        // imports one — the same gate the ambient's own defs pass.
        let members = exported_std_members(
            "pub tag int\nfn core_helper() -> int {\n  1\n}\n",
            "const hidden = 1\npub(pkg) fn pkg_only() -> int {\n  1\n}\npub const shown = 2\n",
        );
        let names: Vec<&str> = members.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(names, ["shown"]);
    }
}
