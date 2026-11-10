//! Parser for DreamScript: lexer, pest grammar, and spanned AST for the
//! decided surface of docs/en/design-decisions.md (F1-F7, B3, B4, B7, G3,
//! G4).
//!
//! The public surface is one query, [`parse`], shaped for the M1 memoized
//! compiler-as-query-library: it takes the source registry plus a file id and
//! returns either the [`Ast`] or the diagnostics. Invalid source never
//! panics: a lexical pre-scan guards nesting depth (recursive-descent stack
//! safety) and produces precise two-span diagnostics for the string and
//! bracket classes; remaining structural failures map onto the pest
//! furthest-failure position.
//!
//! # Script entry (G3)
//!
//! Module bodies are pure declarations; the one exception is the entry file
//! under script mode, whose top-level statements fold into one synthesized
//! `main` binding ([`parse_script`]). Library mode ([`parse`]) accepts the
//! same grammar and rejects top-level statements with a structured
//! diagnostic, so every non-entry path through the pipeline stays
//! pure-declaration.
//!
//! # Newline sensitivity (F1)
//!
//! Statements are newline-terminated; there are no semicolons. A binary
//! operator must sit on the same line as its left operand, so `a +\nb`
//! continues while `a\n+ b` is two statements. Only `.`-continuation may
//! open on the next line (method chains); calls, indexes, and `?` must open
//! on the same line as their operand.
//!
//! # File structure (G3)
//!
//! A file is a `Vec` of `##`-separated [`Block`]s; a file without
//! separators is one block. Top-level definitions are order-independent
//! data; no ordering validation happens here.
//!
//! ```
//! use ds_diag::{ErrorCode, Files};
//! use ds_parse::parse;
//!
//! let mut files = Files::new();
//! let file = files.add_source("m.ds", "tag timeout\nconst one = 1_000i64\n");
//! let ast = parse(&files, file).expect("parses");
//! assert_eq!(ast.blocks.len(), 1);
//! assert_eq!(ast.blocks[0].items.len(), 2);
//!
//! let bad = files.add_source("bad.ds", "const x = \"open");
//! let diags = parse(&files, bad).expect_err("unterminated string");
//! assert_eq!(diags[0].code, ErrorCode::UnterminatedString);
//! assert_eq!(diags[0].labels.len(), 2);
//! ```

#![warn(missing_docs)]

mod ast;
mod convert;
mod lexscan;

use ds_diag::{Diagnostic, ErrorCode, FileId, Files, Span};
use pest::{
    Parser,
    error::{ErrorVariant, InputLocation},
};

// The derive generates a public `Rule` enum plus a parser struct; their docs
// are the grammar file itself.
mod pest_gen {
    #![allow(missing_docs)]
    #[derive(pest_derive::Parser)]
    #[grammar = "grammar.pest"]
    pub(super) struct DsParser;
}
pub use ast::{
    AssignOp, Ast, Attribute, BinaryOp, Block, BlockExpr, CallArg, EnumCase, EnumDecl, EnumPayload,
    Expr, FieldDecl, FnDecl, Ident, ImplDecl, ImportDecl, ImportPath, ImportSel, Item, ItemKind,
    Literal, LocalDecl, MatchArm, Param, ParamMode, Path, Pattern, Stmt, StmtKind, StrPart,
    StructDecl, StructMember, TagDecl, TraitDecl, TraitMember, Trivia, TriviaKind, Type,
    TypeAliasDecl, TypeParam, TypeRef, UnaryOp, Vis,
};
use pest_gen::DsParser;
pub(crate) use pest_gen::Rule;

/// Parses one registered source file into its blocks, or the syntax
/// diagnostics. Two-span diagnostics cover the pairs the record names
/// (unclosed interpolation holes, mismatched delimiters); raw-string hash
/// counting is exact via PUSH/POP, so it never mis-lexes.
///
/// The grammar and its conversion run on a dedicated 8 MiB stack: pest's
/// recursive descent costs roughly 12-30 KiB per bracket-nesting level (the
/// precedence chain is ~19 rules deep per level), which a default 1 MiB
/// main-thread stack cannot host even for modestly nested sources. The
/// scanner's depth guard stays as a second line of defense.
///
/// # Panics
///
/// Never panics on invalid source; panics only if `file` was never registered
/// in `files` (an upstream contract violation, delegated to [`Files`]).
pub fn parse(files: &Files, file: FileId) -> Result<Ast, Vec<Diagnostic>> {
    parse_mode(files, file, false)
}

/// Parses one registered source file as the script entry: same grammar as
/// [`parse`], plus the script-mode fold — the entry file's top-level
/// statements (expression statements and non-function `const`/`var`
/// bindings) become the body of one synthesized `main` binding, so the
/// declarations beside them stay pure-declaration and every later pass
/// (resolve, check, lower) sees the same shape a written `fn main() -> ()
/// { ... }` produces. A file whose statements would collide with a
/// written `main` is a diagnostic. Everything else — imported files,
/// package members — must go through [`parse`], which rejects top-level
/// statements outright.
///
/// # Panics
///
/// Same contract as [`parse`].
pub fn parse_script(files: &Files, file: FileId) -> Result<Ast, Vec<Diagnostic>> {
    parse_mode(files, file, true)
}

/// Shared body of the two parse modes; `script` threads the entry-file
/// flag through to the conversion fold.
fn parse_mode(files: &Files, file: FileId, script: bool) -> Result<Ast, Vec<Diagnostic>> {
    let src = files.source(file);
    if let Some(diags) = lexscan::scan(file, src) {
        return Err(diags);
    }
    std::thread::scope(|scope| {
        let worker = std::thread::Builder::new()
            .stack_size(PARSE_STACK)
            .spawn_scoped(scope, || run_grammar(file, src, script))
            .expect("spawning the parse thread cannot fail");
        // A pest panic would be a parser bug; surface it as a diagnostic
        // instead of unwinding through the caller.
        worker.join().unwrap_or_else(|_| {
            Err(vec![Diagnostic::error(
                ErrorCode::UnexpectedToken,
                "internal parser error: the grammar panicked",
            )])
        })
    })
}

/// Stack size for the grammar thread.
const PARSE_STACK: usize = 8 << 20;

fn run_grammar(file: FileId, src: &str, script: bool) -> Result<Ast, Vec<Diagnostic>> {
    match DsParser::parse(Rule::file, src) {
        Ok(pairs) => convert::convert(file, src, pairs, script),
        Err(err) => Err(vec![pest_error(file, src, err)]),
    }
}

/// Maps a pest failure onto a single-span diagnostic at the furthest
/// position that failed to match.
fn pest_error(file: FileId, src: &str, err: pest::error::Error<Rule>) -> Diagnostic {
    let pos = match err.location {
        InputLocation::Pos(p) => p,
        InputLocation::Span((s, _)) => s,
    };
    let at_eof = pos >= src.len();
    let end = if at_eof { pos } else { pos + src[pos..].chars().next().map_or(1, char::len_utf8) };
    let code = if at_eof { ErrorCode::UnexpectedEof } else { ErrorCode::UnexpectedToken };
    let message = match err.variant {
        ErrorVariant::ParsingError { positives, .. } => {
            let expected: Vec<String> = positives
                .iter()
                .map(|r| format!("{}", format_rule(*r)))
                .collect();
            if expected.is_empty() {
                "invalid syntax".to_owned()
            } else if at_eof {
                format!(
                    "input ended while more syntax was expected: {}",
                    expected.join(", ")
                )
            } else {
                format!("unexpected token, expected one of: {}", expected.join(", "))
            }
        },
        ErrorVariant::CustomError { message } => message,
    };
    let label = if at_eof { "end of input" } else { "unexpected token" };
    // Offsets are u32 by the Files contract.
    #[allow(clippy::cast_possible_truncation)]
    let span = Span::new(file, pos as u32, end as u32);
    Diagnostic::error(code, message).with_primary(span, label)
}

fn format_rule(rule: Rule) -> std::borrow::Cow<'static, str> {
    format!("{rule:?}").into()
}
