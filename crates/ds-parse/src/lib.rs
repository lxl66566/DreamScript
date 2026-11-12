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
mod guardtail;
mod lexscan;
mod worker;

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
    Literal, LocalDecl, MatchArm, Param, ParamMode, Path, Pattern, PlatformDecl, PlatformName,
    Stmt, StmtKind, StrPart, StructDecl, StructMember, TagDecl, TraitDecl, TraitMember, Trivia,
    TriviaKind, Type, TypeAliasDecl, TypeParam, TypeRef, UnaryOp, Vis,
};
pub(crate) use pest_gen::{DsParser, Rule};

/// Parses one registered source file into its blocks, or the syntax
/// diagnostics. Two-span diagnostics cover the pairs the record names
/// (unclosed interpolation holes, mismatched delimiters); raw-string hash
/// counting is exact via PUSH/POP, so it never mis-lexes.
///
/// The grammar and its conversion run on a worker thread that owns an 8 MiB
/// stack and is cached per caller thread, so the segment is paid once per
/// caller: pest's recursive descent costs roughly 12-30 KiB per bracket-nesting
/// level (the precedence chain is ~19 rules deep per level), which a default
/// 1 MiB main-thread stack cannot host even for modestly nested sources. The
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
    worker::run(file, src, script)
}

fn run_grammar(file: FileId, src: &str, script: bool) -> Result<Ast, Vec<Diagnostic>> {
    match DsParser::parse(Rule::file, src) {
        Ok(pairs) => convert::convert(file, src, pairs, script),
        Err(err) => Err(match guardtail::detect(file, src) {
            Some(d) => vec![d],
            None => vec![pest_error(file, src, err)],
        }),
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

/// Human phrasing of one grammar rule for parse-error messages: the
/// user-reachable vocabulary is named, grammar-internal helpers keep their
/// variant name as the fallback.
fn format_rule(rule: Rule) -> std::borrow::Cow<'static, str> {
    let named = match rule {
        // Trivia and sentinels.
        Rule::line_comment | Rule::block_comment => "a comment",
        Rule::EOI => "end of input",
        // Keywords: their own spelling.
        Rule::kw_const => "`const`",
        Rule::kw_var => "`var`",
        Rule::kw_fn => "`fn`",
        Rule::kw_if => "`if`",
        Rule::kw_else => "`else`",
        Rule::kw_match => "`match`",
        Rule::kw_while => "`while`",
        Rule::kw_for => "`for`",
        Rule::kw_in => "`in`",
        Rule::kw_break => "`break`",
        Rule::kw_continue => "`continue`",
        Rule::kw_return => "`return`",
        Rule::kw_is => "`is`",
        Rule::kw_struct => "`struct`",
        Rule::kw_enum => "`enum`",
        Rule::kw_tag => "`tag`",
        Rule::kw_type => "`type`",
        Rule::kw_trait => "`trait`",
        Rule::kw_impl => "`impl`",
        Rule::kw_import => "`import`",
        Rule::kw_pub => "`pub`",
        Rule::kw_extern => "`extern`",
        Rule::kw_mut => "`mut`",
        Rule::kw_nursery => "`nursery`",
        Rule::kw_spawn => "`spawn`",
        Rule::kw_defer => "`defer`",
        Rule::kw_true => "`true`",
        Rule::kw_false => "`false`",
        Rule::kw_platform => "`platform`",
        Rule::local_kw => "`const` or `var`",
        // Operators.
        Rule::assign_op
        | Rule::range_op
        | Rule::cmp_op
        | Rule::shift_op
        | Rule::add_op
        | Rule::mul_op => "an operator",
        Rule::neg_op => "a unary operator",
        Rule::opt_mark => "`?`",
        // Expressions: the precedence chain collapses to one description.
        Rule::expr
        | Rule::expr_nos
        | Rule::assign_lvl
        | Rule::nos_assign
        | Rule::range_lvl
        | Rule::nos_range
        | Rule::logor_lvl
        | Rule::nos_logor
        | Rule::logand_lvl
        | Rule::nos_logand
        | Rule::isin_lvl
        | Rule::nos_isin
        | Rule::bitor_lvl
        | Rule::nos_bitor
        | Rule::bitxor_lvl
        | Rule::nos_bitxor
        | Rule::bitand_lvl
        | Rule::nos_bitand
        | Rule::cmp_lvl
        | Rule::nos_cmp
        | Rule::shift_lvl
        | Rule::nos_shift
        | Rule::add_lvl
        | Rule::nos_add
        | Rule::mul_lvl
        | Rule::nos_mul
        | Rule::unary_lvl
        | Rule::nos_unary
        | Rule::postfix_lvl
        | Rule::nos_postfix
        | Rule::primary
        | Rule::primary_nos => "an expression",
        // Types, patterns, and the declaration vocabulary.
        Rule::type_expr
        | Rule::type_union
        | Rule::type_inter
        | Rule::type_diff
        | Rule::type_postfix
        | Rule::type_unary
        | Rule::type_atom => "a type",
        Rule::pattern | Rule::pat_or => "a pattern",
        Rule::ident => "an identifier",
        Rule::path | Rule::path_atom | Rule::type_path | Rule::field_name => "a name",
        Rule::generics => "generic parameters",
        Rule::gparam => "a generic parameter",
        Rule::params => "a parameter list",
        Rule::param => "a parameter",
        Rule::param_mode => "a parameter mode",
        Rule::args => "call arguments",
        Rule::arg => "an argument",
        Rule::arm => "a match arm",
        Rule::arm_list => "match arms",
        Rule::arm_body => "a match arm body",
        Rule::block_expr => "a block",
        Rule::stmt => "a statement",
        Rule::stmt_list => "statements",
        Rule::item | Rule::file_item => "a declaration",
        Rule::attr => "an attribute",
        Rule::import_item => "an import",
        Rule::import_path => "an import path",
        Rule::import_select => "an import selection",
        Rule::import_alias => "an import alias",
        Rule::lambda => "a lambda",
        Rule::case_tuple | Rule::case_struct => "an enum case payload",
        Rule::impl_body => "an impl body",
        Rule::literal => "a literal",
        Rule::int_lit => "an integer literal",
        Rule::float_lit => "a float literal",
        Rule::string_lit => "a string literal",
        Rule::raw_string => "a raw string literal",
        Rule::char_lit => "a character literal",
        Rule::byte_lit => "a byte literal",
        // Grammar-internal helpers keep their variant name as the fallback.
        _ => return std::borrow::Cow::Owned(format!("{rule:?}")),
    };
    std::borrow::Cow::Borrowed(named)
}
