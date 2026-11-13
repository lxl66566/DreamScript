//! Translations between the engine's source coordinates and LSP's: byte
//! offsets against Unicode-scalar columns and UTF-16 code-unit columns
//! (never assumed equal — an astral character is one scalar but two
//! units), diagnostic and symbol mappings over the parse tree.

use ds_diag::{Diagnostic, FileId, Files, LabelStyle, Severity, Span};
use ds_parse::{Ast, Expr, Ident, Item, ItemKind, Pattern};
use tower_lsp::lsp_types as lsp;

/// The LSP position of one byte offset: 0-based line, character counted
/// in UTF-16 code units from the line's start.
pub(crate) fn position(files: &Files, file: FileId, byte: u32) -> lsp::Position {
    let location = files.location(file, byte);
    let line = files.line_span(file, location.line);
    let source = files.source(file);
    let units = source[line.start as usize..byte as usize]
        .chars()
        .map(|ch| u32::try_from(ch.len_utf16()).expect("a UTF-16 length fits u32"))
        .sum();
    lsp::Position::new(location.line - 1, units)
}

/// The byte offset of one LSP position, clamped into the document: a
/// line past the end lands on the last line, a character past the line
/// on the line's end, and a character inside a surrogate pair (a
/// position the client should never send) on the character's start.
pub(crate) fn offset(files: &Files, file: FileId, pos: lsp::Position) -> u32 {
    let line = (pos.line + 1).min(files.line_count(file));
    let span = files.line_span(file, line);
    let source = files.source(file);
    let mut byte = span.start;
    let mut units = 0;
    for ch in source[span.start as usize..span.end as usize].chars() {
        let units_next = units + u32::try_from(ch.len_utf16()).expect("a UTF-16 length fits u32");
        if units_next > pos.character {
            break;
        }
        units = units_next;
        byte += u32::try_from(ch.len_utf8()).expect("a UTF-8 length fits u32");
    }
    byte
}

/// The LSP range of one span, folded into the file's current bounds (a
/// span that predates an edit degrades to the end, like the render
/// paths).
pub(crate) fn range(files: &Files, span: Span) -> lsp::Range {
    let span = files.clamp_span(span);
    lsp::Range::new(
        position(files, span.file, span.start),
        position(files, span.file, span.end),
    )
}

/// The LSP severity of one engine severity: only error and warning exist
/// in the engine.
fn severity(severity: Severity) -> lsp::DiagnosticSeverity {
    match severity {
        Severity::Error => lsp::DiagnosticSeverity::ERROR,
        Severity::Warning => lsp::DiagnosticSeverity::WARNING,
    }
}

/// One engine diagnostic as an LSP diagnostic of `file`: the range is
/// the file's own label (the primary one, else the first), the other
/// labels become related locations when their file has a client URI, and
/// the notes join the message. `None` when no label addresses `file`, so
/// per-file publication is the ambient filter: the ambient's own
/// diagnostics never carry a label in a document.
pub(crate) fn diagnostic(
    files: &Files,
    uri_of: &dyn Fn(FileId) -> Option<lsp::Url>,
    diag: &Diagnostic,
    file: FileId,
) -> Option<lsp::Diagnostic> {
    let focus = diag
        .labels
        .iter()
        .enumerate()
        .find(|(_, label)| label.span.file == file && label.style == LabelStyle::Primary)
        .or_else(|| {
            diag.labels
                .iter()
                .enumerate()
                .find(|(_, label)| label.span.file == file)
        })
        .map(|(index, _)| index)?;
    let focus_span = diag.labels[focus].span;
    let mut message = diag.message.clone();
    for note in &diag.notes {
        message.push('\n');
        message.push_str(note);
    }
    let related = diag
        .labels
        .iter()
        .enumerate()
        .filter(|(index, label)| {
            *index != focus && label.span.file != file && uri_of(label.span.file).is_some()
        })
        .filter_map(|(_, label)| {
            uri_of(label.span.file).map(|uri| lsp::DiagnosticRelatedInformation {
                location: lsp::Location {
                    uri,
                    range: range(files, label.span),
                },
                message: label.label.clone(),
            })
        })
        .collect::<Vec<_>>();
    Some(lsp::Diagnostic {
        range: range(files, focus_span),
        severity: Some(severity(diag.severity)),
        code: Some(lsp::NumberOrString::String(diag.code.to_string())),
        source: Some("ds".to_owned()),
        message,
        related_information: (!related.is_empty()).then_some(related),
        ..lsp::Diagnostic::default()
    })
}

/// The names one pattern binds, in source order (a plain binding has
/// one).
fn pattern_names(pattern: &Pattern, out: &mut Vec<Ident>) {
    match pattern {
        Pattern::Binding { name, .. } => out.push(name.clone()),
        Pattern::Call { args, .. } => {
            for arg in args {
                pattern_names(arg, out);
            }
        },
        Pattern::Struct { fields, .. } => {
            for (field, pat) in fields {
                match pat {
                    Some(pat) => pattern_names(pat, out),
                    // Punning: the field name binds itself.
                    None => out.push(field.clone()),
                }
            }
        },
        Pattern::Tuple { pats, .. } | Pattern::Or { pats, .. } => {
            for pat in pats {
                pattern_names(pat, out);
            }
        },
        // Wildcard, literal, and path patterns bind nothing.
        _ => {},
    }
}

/// The LSP symbol kind of one top-level item. LSP has no alias kind, so
/// a type alias takes the closest named-type kind (type parameter).
fn symbol_kind(kind: &ItemKind, mutable: bool) -> lsp::SymbolKind {
    match kind {
        // A written `fn` converts onto a const binding of a lambda at
        // parse time, so the initializer decides the function kind.
        ItemKind::Local(local) => {
            if matches!(local.init, Expr::Lambda { .. }) {
                lsp::SymbolKind::FUNCTION
            } else if local.mutable {
                lsp::SymbolKind::VARIABLE
            } else {
                lsp::SymbolKind::CONSTANT
            }
        },
        ItemKind::Fn(_) | ItemKind::ExternFn(_) => lsp::SymbolKind::FUNCTION,
        ItemKind::Struct(_) => lsp::SymbolKind::STRUCT,
        ItemKind::Enum(_) => lsp::SymbolKind::ENUM,
        ItemKind::Tag(_) => lsp::SymbolKind::ENUM_MEMBER,
        ItemKind::TypeAlias(_) => lsp::SymbolKind::TYPE_PARAMETER,
        ItemKind::Trait(_) => lsp::SymbolKind::INTERFACE,
        ItemKind::Import(_) => lsp::SymbolKind::NAMESPACE,
        ItemKind::Impl(_) | ItemKind::Platform(_) => lsp::SymbolKind::MODULE,
    }
}

/// One item as a symbol, or `None` when it carries no name (`impl` and
/// `platform` blocks: the impl's target is a type expression, the
/// platform's name a plain string literal — both wait for a later slice).
fn symbol(files: &Files, item: &Item) -> Option<lsp::DocumentSymbol> {
    let (name, name_span, kind) = match &item.kind {
        ItemKind::Local(local) => {
            let mut names = Vec::new();
            pattern_names(&local.pat, &mut names);
            let name = names
                .iter()
                .map(|ident| ident.name.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            let span = names.first().map_or(item.span, |ident| ident.span);
            (name, span, symbol_kind(&item.kind, local.mutable))
        },
        ItemKind::Fn(decl) | ItemKind::ExternFn(decl) => (
            decl.name.name.clone(),
            decl.name.span,
            symbol_kind(&item.kind, false),
        ),
        ItemKind::Struct(decl) => (
            decl.name.name.clone(),
            decl.name.span,
            symbol_kind(&item.kind, false),
        ),
        ItemKind::Enum(decl) => (
            decl.name.name.clone(),
            decl.name.span,
            symbol_kind(&item.kind, false),
        ),
        ItemKind::Tag(decl) => (
            decl.name.name.clone(),
            decl.name.span,
            symbol_kind(&item.kind, false),
        ),
        ItemKind::TypeAlias(decl) => (
            decl.name.name.clone(),
            decl.name.span,
            symbol_kind(&item.kind, false),
        ),
        ItemKind::Trait(decl) => (
            decl.name.name.clone(),
            decl.name.span,
            symbol_kind(&item.kind, false),
        ),
        ItemKind::Import(decl) => {
            let name = decl.alias.as_ref().map_or_else(
                || {
                    let mut path = String::new();
                    if decl.path.at_sign {
                        path.push('@');
                    }
                    path.push_str(
                        &decl
                            .path
                            .segments
                            .iter()
                            .map(|segment| segment.name.as_str())
                            .collect::<Vec<_>>()
                            .join("/"),
                    );
                    path
                },
                |alias| alias.name.clone(),
            );
            (name, decl.path.span, symbol_kind(&item.kind, false))
        },
        ItemKind::Impl(_) | ItemKind::Platform(_) => return None,
    };
    if name.is_empty() {
        return None;
    }
    Some(lsp::DocumentSymbol {
        name,
        detail: None,
        kind,
        tags: None,
        // The field is required but superseded by tags; the slice emits
        // neither.
        #[allow(deprecated)]
        deprecated: None,
        range: range(files, item.span),
        selection_range: range(files, name_span),
        children: None,
    })
}

/// The top-level symbols of one parse tree: every named top-level item of
/// every block, flat (members stay inside the item's range until a
/// nested slice answers them).
pub(crate) fn symbols(files: &Files, ast: &Ast) -> Vec<lsp::DocumentSymbol> {
    ast.blocks
        .iter()
        .flat_map(|block| block.items.iter())
        .filter_map(|item| symbol(files, item))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files(source: &str) -> (Files, FileId) {
        let mut files = Files::new();
        let file = files.add_source("t.ds", source);
        (files, file)
    }

    #[test]
    fn ascii_positions_round_trip() {
        let (files, file) = files("abc\ndef\n");
        assert_eq!(offset(&files, file, lsp::Position::new(0, 2)), 2);
        assert_eq!(offset(&files, file, lsp::Position::new(1, 0)), 4);
        assert_eq!(position(&files, file, 6), lsp::Position::new(1, 2));
        assert_eq!(position(&files, file, 8), lsp::Position::new(2, 0));
    }

    #[test]
    fn astral_characters_count_two_utf16_units() {
        let (files, file) = files("🚀x\ny");
        // The rocket is 4 bytes and 2 UTF-16 units; `x` starts at byte 4.
        assert_eq!(offset(&files, file, lsp::Position::new(0, 2)), 4);
        assert_eq!(offset(&files, file, lsp::Position::new(0, 3)), 5);
        assert_eq!(position(&files, file, 4), lsp::Position::new(0, 2));
        assert_eq!(position(&files, file, 5), lsp::Position::new(0, 3));
        assert_eq!(position(&files, file, 6), lsp::Position::new(1, 0));
        // A position inside the surrogate pair clamps to the rocket's
        // start, never into its bytes.
        assert_eq!(offset(&files, file, lsp::Position::new(0, 1)), 0);
    }

    #[test]
    fn positions_clamp_into_the_document() {
        let (files, file) = files("one\ntwo\n");
        // A line past the end lands on the last (empty) line; a character
        // past the line end on the line's end.
        assert_eq!(offset(&files, file, lsp::Position::new(9, 0)), 8);
        assert_eq!(offset(&files, file, lsp::Position::new(0, 99)), 3);
        assert_eq!(offset(&files, file, lsp::Position::new(1, 99)), 7);
    }

    #[test]
    fn crlf_terminators_stay_out_of_lines() {
        let (files, file) = files("a\r\nb\r\n");
        assert_eq!(files.line_count(file), 3);
        assert_eq!(offset(&files, file, lsp::Position::new(1, 0)), 3);
        assert_eq!(position(&files, file, 4), lsp::Position::new(1, 1));
    }

    #[test]
    fn a_diagnostic_maps_its_own_file_label() {
        let mut files = Files::new();
        let here = files.add_source("a.ds", "const one = 1\n");
        let there = files.add_source("b.ds", "const one = 2\n");
        let diag = Diagnostic::error(
            ds_diag::ErrorCode::DuplicateDefinition,
            "two definitions claim `one`",
        )
        .with_primary(Span::new(here, 6, 9), "a later definition")
        .with_secondary(Span::new(there, 6, 9), "the first definition");
        let uri_of =
            |file: FileId| (file == there).then(|| lsp::Url::parse("file:///b.ds").unwrap());
        let mapped = diagnostic(&files, &uri_of, &diag, here)
            .expect("the primary label addresses this file");
        assert_eq!(mapped.range.start.line, 0);
        assert_eq!(mapped.range.start.character, 6);
        assert_eq!(mapped.range.end.character, 9);
        assert_eq!(mapped.severity, Some(lsp::DiagnosticSeverity::ERROR));
        assert_eq!(
            mapped.code,
            Some(lsp::NumberOrString::String("DS2005".to_owned()))
        );
        assert_eq!(
            mapped.related_information.as_ref().map(Vec::len),
            Some(1),
            "the other file's label rides along when it has a URI"
        );
        // Publishing the same diagnostic into the other file maps its own
        // label there; a diagnostic with no label in the file maps to
        // nothing (the ambient filter).
        let other = diagnostic(&files, &uri_of, &diag, there).expect("the secondary label maps");
        assert_eq!(other.range.start.character, 6);
        let elsewhere = files.add_source("c.ds", "\n");
        assert!(
            diagnostic(&files, &uri_of, &diag, elsewhere).is_none(),
            "a diagnostic with no label in the file publishes nothing"
        );
    }

    #[test]
    fn symbols_carry_names_and_kinds() {
        let source = concat!(
            "import @std/csv\n",
            "const one = 1\n",
            "fn show_it(x: int) -> int { x }\n",
            "struct rec { v: int }\n",
            "enum pick { yes, no }\n",
            "tag mark\n",
            "type alias_t = int\n",
            "trait pretty { fn show(self) -> int }\n",
            "platform \"std\" provides <console>\n",
        );
        let mut files = Files::new();
        let file = files.add_source("t.ds", source);
        let ast = ds_parse::parse(&files, file).expect("the source parses");
        let symbols = symbols(&files, &ast);
        let names: Vec<(&str, lsp::SymbolKind)> = symbols
            .iter()
            .map(|symbol| (symbol.name.as_str(), symbol.kind))
            .collect();
        assert_eq!(
            names,
            vec![
                ("@std/csv", lsp::SymbolKind::NAMESPACE),
                ("one", lsp::SymbolKind::CONSTANT),
                ("show_it", lsp::SymbolKind::FUNCTION),
                ("rec", lsp::SymbolKind::STRUCT),
                ("pick", lsp::SymbolKind::ENUM),
                ("mark", lsp::SymbolKind::ENUM_MEMBER),
                ("alias_t", lsp::SymbolKind::TYPE_PARAMETER),
                ("pretty", lsp::SymbolKind::INTERFACE),
            ],
            "the platform block carries no symbol name"
        );
    }
}
