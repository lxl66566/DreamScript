//! Parser for DreamScript: lexer, pest grammar, and spanned AST.
//!
//! The surface is closed: bindings, `fn` declarations, traits and impls, one
//! fixed operator set. Custom operators, subscript slicing, vararg/spread,
//! `or`, `??` and `?.` have no spelling, so they do not parse.
//!
//! The public surface is one query, [`parse`], shaped for a memoized
//! compiler-as-query-library: it takes the source registry plus a file id and
//! returns either the [`Ast`] or the diagnostics. Invalid source never
//! panics: a lexical pre-scan guards nesting depth (recursive-descent stack
//! safety) and produces precise two-span diagnostics for the string and
//! bracket classes; remaining structural failures map onto the pest
//! furthest-failure position.
//!
//! # Script entry
//!
//! Module bodies are pure declarations; the one exception is the entry file
//! under script mode, whose top-level statements fold into one synthesized
//! `main` binding ([`parse_script`]). Library mode ([`parse`]) accepts the
//! same grammar and rejects top-level statements with a structured
//! diagnostic, so every non-entry path through the pipeline stays
//! pure-declaration.
//!
//! # Newline sensitivity
//!
//! Statements are newline-terminated; there are no semicolons. A binary
//! operator must sit on the same line as its left operand, so `a +\nb`
//! continues while `a\n+ b` is two statements. Only `.`-continuation may
//! open on the next line (method chains); calls, indexes, and `?` must open
//! on the same line as their operand.
//!
//! # File structure
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

mod arrowbudget;
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
    AssignOp, Ast, Attribute, BinaryOp, BindName, Block, BlockExpr, CallArg, Delim, EnumCase,
    EnumDecl, EnumPayload, Expr, FieldDecl, FnDecl, Ident, ImplDecl, ImportDecl, ImportPath,
    ImportSel, Item, ItemKind, Literal, LocalDecl, MatchArm, Param, ParamMode, Path, Pattern,
    PlatformDecl, PlatformName, Stmt, StmtKind, StrPart, StructDecl, StructMember, TagDecl,
    TraitDecl, TraitMember, Trivia, TriviaKind, Type, TypeAliasDecl, TypeParam, TypeRef, UnaryOp,
    Vis,
};
pub(crate) use pest_gen::{DsParser, Rule};

/// Parses one registered source file into its blocks, or the syntax
/// diagnostics. Two-span diagnostics cover the pairs the record names
/// (unclosed interpolation holes, mismatched delimiters); raw-string hash
/// counting is exact via PUSH/POP, so it never mis-lexes.
///
/// The grammar and its conversion run on a worker thread that owns a 32 MiB
/// stack and is cached per caller thread, so the segment is paid once per
/// caller: pest's recursive descent costs roughly 12-30 KiB per bracket-nesting
/// level (the precedence chain is ~19 rules deep per level), and the
/// conversion pass that follows costs several times that per level, which a
/// default 1 MiB main-thread stack cannot host even for modestly nested
/// sources. The scanner's depth guard stays as a second line of defense.
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
            // Positives repeat the same rule across the precedence chain's
            // attempts, so each phrase is kept once, in report order.
            let mut expected: Vec<String> = Vec::new();
            for rule in positives {
                let phrase = format_rule(rule);
                if !expected.iter().any(|seen| seen.as_str() == phrase) {
                    expected.push(phrase.to_owned());
                }
            }
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

/// Human phrasing of one grammar rule for parse-error messages: every
/// rule a failure can report maps to a user-facing phrase, and anything
/// unmapped falls back to "more syntax" instead of leaking the internal
/// variant name.
fn format_rule(rule: Rule) -> &'static str {
    match rule {
        // Trivia and sentinels.
        Rule::line_comment | Rule::block_comment => "a comment",
        Rule::EOI => "end of input",
        // Keywords and their control forms: their own spelling.
        Rule::kw_const => "`const`",
        Rule::kw_var => "`var`",
        Rule::kw_fn => "`fn`",
        Rule::kw_if | Rule::if_expr => "`if`",
        Rule::kw_else => "`else`",
        Rule::kw_match | Rule::match_expr => "`match`",
        Rule::kw_while | Rule::while_expr => "`while`",
        Rule::kw_for | Rule::for_expr | Rule::for_plain | Rule::for_paren => "`for`",
        Rule::kw_in => "`in`",
        Rule::kw_break | Rule::break_expr => "`break`",
        Rule::kw_continue | Rule::continue_expr => "`continue`",
        Rule::kw_return | Rule::return_expr => "`return`",
        Rule::kw_is => "`is`",
        Rule::kw_struct => "`struct`",
        Rule::kw_enum => "`enum`",
        Rule::kw_tag => "`tag`",
        Rule::kw_type | Rule::type_kw => "`type`",
        Rule::kw_trait => "`trait`",
        Rule::kw_impl => "`impl`",
        Rule::kw_import => "`import`",
        Rule::kw_pub | Rule::vis => "`pub`",
        Rule::kw_extern => "`extern`",
        Rule::kw_mut => "`mut`",
        Rule::kw_nursery | Rule::nursery_expr => "`nursery`",
        Rule::kw_spawn | Rule::spawn_expr => "`spawn`",
        Rule::kw_defer | Rule::defer_expr => "`defer`",
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
        | Rule::primary_nos
        | Rule::in_rhs
        | Rule::pt_item => "an expression",
        // Types, patterns, and the declaration vocabulary.
        Rule::type_expr
        | Rule::type_union
        | Rule::type_inter
        | Rule::type_diff
        | Rule::type_postfix
        | Rule::type_unary
        | Rule::type_atom
        | Rule::path_type
        | Rule::case_type => "a type",
        Rule::fn_type => "a function type",
        Rule::ptr_type => "a pointer type",
        Rule::tuple_type => "a tuple type",
        Rule::type_args_bracket | Rule::type_args_angle => "type arguments",
        Rule::ret_ann => "a return type",
        Rule::neg_prefix => "`~`",
        Rule::pattern
        | Rule::pat_or
        | Rule::pat_wild
        | Rule::pat_ident
        | Rule::pat_neg
        | Rule::pat_literal
        | Rule::pat_head
        | Rule::pat_call
        | Rule::pat_struct
        | Rule::pat_field
        | Rule::tuple_pattern
        | Rule::local_pat => "a pattern",
        Rule::ident | Rule::ident_start | Rule::ident_cont => "an identifier",
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
        Rule::sel_item => "a selection item",
        Rule::import_alias => "an import alias",
        Rule::import_cond => "an import condition",
        Rule::lambda | Rule::lambda_paren | Rule::lambda_bare => "a lambda",
        Rule::lambda_rhs => "a lambda body",
        Rule::case_tuple | Rule::case_struct => "an enum case payload",
        Rule::impl_body => "an impl body",
        Rule::literal => "a literal",
        Rule::int_lit => "an integer literal",
        Rule::float_lit => "a float literal",
        Rule::string_lit => "a string literal",
        Rule::char_lit => "a character literal",
        Rule::byte_lit => "a byte literal",
        // Postfix and literal internals.
        Rule::postfix_op | Rule::nos_postfix_op => "a call, index, field access, or `?`",
        Rule::postfix_index => "an index",
        Rule::struct_lit => "a struct literal",
        Rule::list_lit => "a list literal",
        Rule::paren_tuple => "a parenthesized expression",
        Rule::pt_trail => "a trailing comma",
        Rule::arg_named => "a named argument",
        Rule::field_init => "a field initializer",
        Rule::map_apply | Rule::map_head => "a map type application",
        // Bindings.
        Rule::local => "a `const` or `var` binding",
        Rule::param_default => "a default value",
        Rule::for_body => "a loop body",
        Rule::guard => "a match guard",
        // Declarations and file structure.
        Rule::fn_item => "a `fn` declaration",
        Rule::extern_fn_item => "an extern declaration",
        Rule::struct_item => "a struct declaration",
        Rule::enum_item => "an enum declaration",
        Rule::tag_item => "a tag declaration",
        Rule::type_item => "a type alias declaration",
        Rule::trait_item => "a trait declaration",
        Rule::impl_item => "an impl declaration",
        Rule::platform_item => "a platform declaration",
        Rule::super_list => "a list of types",
        Rule::struct_body => "a struct body",
        Rule::field_member => "a field",
        Rule::deinit_member => "a `deinit` member",
        Rule::enum_case => "an enum case",
        Rule::case_default => "a `#[default]` annotation",
        Rule::member_list => "a member list",
        Rule::member_const => "a const member",
        Rule::member_sig => "a member signature",
        Rule::assoc_type => "an associated type",
        Rule::impl_trait => "a trait path",
        Rule::attr_args => "attribute arguments",
        Rule::attr_arg => "an attribute argument",
        Rule::provides_list => "a provides list",
        Rule::provides_cap => "a capability",
        Rule::file | Rule::file_blocks | Rule::file_block => "a top-level declaration",
        Rule::block_sep => "a `##` block separator",
        // Identifier and literal internals.
        Rule::keyword => "a keyword",
        Rule::digit => "a digit",
        Rule::int_suffix => "an integer suffix",
        Rule::fsuffix => "a float suffix",
        Rule::exp_part => "an exponent",
        Rule::radix_int | Rule::dec_int | Rule::float_rest => "a number",
        Rule::esc => "an escape sequence",
        Rule::str_text => "string text",
        Rule::interp_simple | Rule::interp_brace => "an interpolation",
        Rule::raw_string | Rule::raw_hashed | Rule::raw_hash_body | Rule::raw_zero => {
            "a raw string literal"
        },
        Rule::char_single => "a character",
        // Any rule this table never names: no internal name reaches the
        // user.
        _ => "more syntax",
    }
}
