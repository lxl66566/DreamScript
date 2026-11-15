//! Translations between the engine's source coordinates and LSP's: byte
//! offsets against Unicode-scalar columns and UTF-16 code-unit columns
//! (never assumed equal — an astral character is one scalar but two
//! units), diagnostic and symbol mappings over the parse tree.

use ds_check::ScopeKind;
use ds_diag::{Diagnostic, FileId, Files, LabelStyle, Severity, Span};
use ds_parse::{Ast, Expr, Ident, Item, ItemKind, Pattern};
use tower_lsp::lsp_types as lsp;

/// The LSP completion-item kind of one scope kind.
pub(crate) fn completion_kind(kind: ScopeKind) -> lsp::CompletionItemKind {
    match kind {
        ScopeKind::Fn => lsp::CompletionItemKind::FUNCTION,
        ScopeKind::Const => lsp::CompletionItemKind::CONSTANT,
        ScopeKind::Var | ScopeKind::Let | ScopeKind::Param => lsp::CompletionItemKind::VARIABLE,
        ScopeKind::Struct | ScopeKind::TypeAlias => lsp::CompletionItemKind::STRUCT,
        ScopeKind::Enum | ScopeKind::Tag => lsp::CompletionItemKind::ENUM,
        ScopeKind::Case => lsp::CompletionItemKind::ENUM_MEMBER,
        ScopeKind::Trait => lsp::CompletionItemKind::INTERFACE,
    }
}

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
    // The client's line is a raw u32 and may sit far past the document
    // (u32::MAX included); the 1-based registry line clamps in u64 before
    // it converts back.
    let line = (u64::from(pos.line) + 1).min(u64::from(files.line_count(file)));
    let line = u32::try_from(line).expect("the clamped line fits u32");
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

/// The new text of one range-bearing change applied to `source`: the
/// range's positions (UTF-16 code-unit columns) fold onto byte offsets
/// through the same clamping as every other position, so an in-bounds
/// range splices exactly the covered characters, while an inverted
/// range is refused.
pub(crate) fn apply_change(
    source: &str,
    range: lsp::Range,
    new_text: &str,
) -> Result<String, String> {
    // The range addresses the text as the client sees it, so the line
    // index is scratch-local: the registry's own copy is untouched
    // until the spliced result replaces it.
    let mut files = Files::new();
    let file = files.add_source("change", source);
    let start = offset(&files, file, range.start);
    let end = offset(&files, file, range.end);
    if start > end {
        return Err(format!(
            "a change range ends before it starts: {start} > {end}"
        ));
    }
    let mut out = String::with_capacity(source.len() + new_text.len());
    out.push_str(&source[..start as usize]);
    out.push_str(new_text);
    out.push_str(&source[end as usize..]);
    Ok(out)
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
/// labels become related locations when their file has a client URI
/// (same-file labels included, under the document's own URI), and the
/// notes join the message. `None` when no label addresses `file`, so
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
        .filter(|(index, _)| *index != focus)
        .filter_map(|(_, label)| {
            let uri = uri_of(label.span.file)?;
            Some(lsp::DiagnosticRelatedInformation {
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
fn symbol_kind(kind: &ItemKind) -> lsp::SymbolKind {
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
            (name, span, symbol_kind(&item.kind))
        },
        ItemKind::Fn(decl) | ItemKind::ExternFn(decl) => (
            decl.name.name.clone(),
            decl.name.span,
            symbol_kind(&item.kind),
        ),
        ItemKind::Struct(decl) => (
            decl.name.name.clone(),
            decl.name.span,
            symbol_kind(&item.kind),
        ),
        ItemKind::Enum(decl) => (
            decl.name.name.clone(),
            decl.name.span,
            symbol_kind(&item.kind),
        ),
        ItemKind::Tag(decl) => (
            decl.name.name.clone(),
            decl.name.span,
            symbol_kind(&item.kind),
        ),
        ItemKind::TypeAlias(decl) => (
            decl.name.name.clone(),
            decl.name.span,
            symbol_kind(&item.kind),
        ),
        ItemKind::Trait(decl) => (
            decl.name.name.clone(),
            decl.name.span,
            symbol_kind(&item.kind),
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
            (name, decl.path.span, symbol_kind(&item.kind))
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
    fn extreme_positions_clamp_instead_of_overflow() {
        let (files, file) = files("one\ntwo\n");
        // A u32::MAX line must clamp to the last line (the 1-based
        // registry line would overflow the client's raw u32 otherwise).
        assert_eq!(offset(&files, file, lsp::Position::new(u32::MAX, 0)), 8);
        assert_eq!(
            offset(&files, file, lsp::Position::new(u32::MAX, u32::MAX)),
            8
        );
        // A character past a line's end lands on that line's end
        // (`one\ntwo\n` has an empty third line starting at byte 8).
        assert_eq!(offset(&files, file, lsp::Position::new(1, u32::MAX)), 7);
        assert_eq!(offset(&files, file, lsp::Position::new(2, u32::MAX)), 8);
    }

    #[test]
    fn same_file_secondary_labels_ride_as_related() {
        let mut files = Files::new();
        let here = files.add_source("a.ds", "const one = 1\nconst one = 2\n");
        let first = Span::new(here, 6, 9);
        let later = Span::new(here, 20, 23);
        let diag = Diagnostic::error(
            ds_diag::ErrorCode::DuplicateDefinition,
            "`one` is already defined",
        )
        .with_secondary(first, "first defined here")
        .with_primary(later, "redefined here");
        let uri_of =
            |file: FileId| (file == here).then(|| lsp::Url::parse("file:///a.ds").unwrap());
        let mapped = diagnostic(&files, &uri_of, &diag, here)
            .expect("the primary label addresses this file");
        assert_eq!(mapped.range.start.line, 1);
        assert_eq!(mapped.range.start.character, 6);
        assert_eq!(mapped.range.end.character, 9);
        // The same file's secondary label rides along under the
        // document's own URI instead of being dropped.
        let related = mapped
            .related_information
            .as_ref()
            .expect("the same-file label is a related location");
        assert_eq!(related.len(), 1, "{related:?}");
        assert_eq!(
            related[0].location.uri,
            lsp::Url::parse("file:///a.ds").unwrap()
        );
        assert_eq!(related[0].location.range.start.line, 0);
        assert_eq!(related[0].location.range.start.character, 6);
        assert_eq!(related[0].location.range.end.character, 9);
        assert_eq!(related[0].message, "first defined here");
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

    #[test]
    fn range_changes_splice_the_covered_characters() {
        let out = apply_change(
            "const one = 1\n",
            lsp::Range::new(lsp::Position::new(0, 6), lsp::Position::new(0, 9)),
            "value",
        )
        .expect("the range is in bounds");
        assert_eq!(out, "const value = 1\n");
        // An empty range inserts.
        let out = apply_change(
            "ab\n",
            lsp::Range::new(lsp::Position::new(0, 1), lsp::Position::new(0, 1)),
            "X",
        )
        .expect("an empty range inserts");
        assert_eq!(out, "aXb\n");
        // An empty new text deletes, spanning lines included.
        let out = apply_change(
            "one\ntwo\nthree\n",
            lsp::Range::new(lsp::Position::new(0, 2), lsp::Position::new(2, 5)),
            "",
        )
        .expect("the range spans lines");
        assert_eq!(out, "on\n");
    }

    #[test]
    fn multibyte_ranges_count_utf16_units() {
        // `const flag = "` is 15 characters, so the rocket occupies
        // UTF-16 units 15..17 across bytes 15..19.
        let out = apply_change(
            "const flag = \"\u{1f680} hi\"\n",
            lsp::Range::new(lsp::Position::new(0, 15), lsp::Position::new(0, 15)),
            "x",
        )
        .expect("the insertion sits on the rocket's start");
        assert_eq!(out, "const flag = \"x\u{1f680} hi\"\n");
        // The full two-unit range deletes the rocket whole.
        let out = apply_change(
            "a\u{1f680}b\n",
            lsp::Range::new(lsp::Position::new(0, 1), lsp::Position::new(0, 3)),
            "",
        )
        .expect("the rocket covers two units");
        assert_eq!(out, "ab\n");
        // A position inside the surrogate pair clamps to the rocket's
        // start, never into its bytes.
        let out = apply_change(
            "\u{1f680}x",
            lsp::Range::new(lsp::Position::new(0, 1), lsp::Position::new(0, 1)),
            "y",
        )
        .expect("an inside-pair position clamps");
        assert_eq!(out, "y\u{1f680}x");
    }

    #[test]
    fn out_of_bounds_ranges_clamp_into_the_document() {
        // A line past the end lands on the last line and a character
        // past its end on the line's end, so the append lands at the
        // document's very end.
        let out = apply_change(
            "ab\n",
            lsp::Range::new(lsp::Position::new(9, 9), lsp::Position::new(9, 9)),
            "z",
        )
        .expect("the position clamps");
        assert_eq!(out, "ab\nz");
    }

    #[test]
    fn inverted_ranges_are_refused() {
        let result = apply_change(
            "ab\n",
            lsp::Range::new(lsp::Position::new(0, 2), lsp::Position::new(0, 1)),
            "x",
        );
        assert!(result.is_err(), "{result:?}");
    }

    #[test]
    fn crlf_ranges_stay_terminator_aware() {
        // Line 0 of `one\r\n` ends at byte 3 (the terminator stays
        // out of the line), so the delete joins the lines without
        // touching the surviving terminator.
        let out = apply_change(
            "one\r\ntwo\r\n",
            lsp::Range::new(lsp::Position::new(0, 3), lsp::Position::new(1, 0)),
            "",
        )
        .expect("the range is in bounds");
        assert_eq!(out, "onetwo\r\n");
    }
}
