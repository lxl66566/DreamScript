//! Semantic tokens: the whole-document stream
//! `textDocument/semanticTokens/full` answers, computed over the lossless
//! parse tree plus the analysis the engine already holds — the resolution
//! side table (span-exact use lookups), the scope-index frames (parameter
//! versus local binder), and the declaration tables (a const binding of a
//! function). No extra checking runs.
//!
//! Identifier classes answer by resolution: a package value name by its
//! definition's kind, a local by its binder's frame kind, a dot-call's
//! member by its tier (the free-function tier a function, the method and
//! type-keyed tiers a method, the handler forms keywords), and a type
//! position by the shared walk's variable scopes ahead of the type
//! namespace. Declaration names answer from the tree. `self` answers as a
//! keyword wherever it binds (a receiver parameter or a use resolving to
//! one); `Self` answers as a type parameter inside impl and trait
//! regions.
//!
//! Honest boundaries: keywords and operators answer only where the tree
//! or a verified source gap spells them (a gap that a comment occupies
//! answers nothing); punctuation is never emitted; a name the side table
//! records nothing for stays a plain identifier (an unresolved name, a
//! bare field access, `none`, import spellings, attribute contents);
//! char and byte literals fold into the string class; the walk reads the
//! raw parse tree, so derive-synthesized trees (whose spans lie inside
//! the attributed declaration) never emit; no delta or range serving.

use ds_check::{DefId, DefKind, DotRes, Package, Res, Resolutions, ScopeFrame, ScopeKind};
use ds_diag::{FileId, Files, Span};
use ds_parse::{
    AssignOp, Ast, BinaryOp, BlockExpr, EnumCase, EnumDecl, Expr, FieldDecl, FnDecl, Ident,
    ImplDecl, ImportDecl, Item, Literal, LocalDecl, Param, ParamMode, Pattern, PlatformDecl, Stmt,
    StmtKind, StrPart, StructDecl, TagDecl, TraitDecl, Trivia, Type, TypeAliasDecl, TypeParam,
    UnaryOp,
};
use rustc_hash::{FxHashMap, FxHashSet};
use tower_lsp::lsp_types as lsp;

use crate::{
    convert,
    walk::{TypeScopes, Visit, Walk},
};

/// The token classes this slice answers, in legend order.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Tok {
    /// A keyword or surface form (`const`, `fn`, `self`, `true`, `is`...).
    Keyword,
    /// An operator the tree or a verified gap spells.
    Operator,
    /// A comment, `///` doc runs included.
    Comment,
    /// A string, char, or byte literal (char and byte fold here).
    Str,
    /// A number literal.
    Number,
    /// A local binding or a package const/var name.
    Variable,
    /// A parameter name.
    Parameter,
    /// A package value name that declares callable code.
    Function,
    /// A trait or inherent impl member name.
    Method,
    /// A field label or field declaration.
    Property,
    /// A struct name.
    Struct,
    /// An enum name.
    Enum,
    /// A trait name.
    Interface,
    /// A type alias name.
    Type,
    /// A type variable or `Self` in a type position.
    TypeParam,
    /// An enum case or unit tag name.
    EnumMember,
}

impl Tok {
    /// The legend entry of the class.
    fn spelling(self) -> lsp::SemanticTokenType {
        match self {
            Self::Keyword => lsp::SemanticTokenType::KEYWORD,
            Self::Operator => lsp::SemanticTokenType::OPERATOR,
            Self::Comment => lsp::SemanticTokenType::COMMENT,
            Self::Str => lsp::SemanticTokenType::STRING,
            Self::Number => lsp::SemanticTokenType::NUMBER,
            Self::Variable => lsp::SemanticTokenType::VARIABLE,
            Self::Parameter => lsp::SemanticTokenType::PARAMETER,
            Self::Function => lsp::SemanticTokenType::FUNCTION,
            Self::Method => lsp::SemanticTokenType::METHOD,
            Self::Property => lsp::SemanticTokenType::PROPERTY,
            Self::Struct => lsp::SemanticTokenType::STRUCT,
            Self::Enum => lsp::SemanticTokenType::ENUM,
            Self::Interface => lsp::SemanticTokenType::INTERFACE,
            Self::Type => lsp::SemanticTokenType::TYPE,
            Self::TypeParam => lsp::SemanticTokenType::TYPE_PARAMETER,
            Self::EnumMember => lsp::SemanticTokenType::ENUM_MEMBER,
        }
    }

    /// The class's index into the legend.
    pub(crate) fn index(self) -> u32 {
        self as u32
    }
}

/// Every class, in legend order.
const ALL: [Tok; 16] = [
    Tok::Keyword,
    Tok::Operator,
    Tok::Comment,
    Tok::Str,
    Tok::Number,
    Tok::Variable,
    Tok::Parameter,
    Tok::Function,
    Tok::Method,
    Tok::Property,
    Tok::Struct,
    Tok::Enum,
    Tok::Interface,
    Tok::Type,
    Tok::TypeParam,
    Tok::EnumMember,
];

/// The `declaration` modifier bit: the name's own defining occurrence.
pub(crate) const DECLARATION: u32 = 1;
/// The `readonly` modifier bit: package-level const bindings (decl and
/// uses — the one const/var line both sides know without new tables).
pub(crate) const READONLY: u32 = 2;

/// The token-type legend the capability registers.
pub(crate) fn legend() -> Vec<lsp::SemanticTokenType> {
    ALL.iter().map(|tok| tok.spelling()).collect()
}

/// The token-modifier legend the capability registers.
pub(crate) fn modifier_legend() -> Vec<lsp::SemanticTokenModifier> {
    vec![
        lsp::SemanticTokenModifier::DECLARATION,
        lsp::SemanticTokenModifier::READONLY,
    ]
}

/// The analysis the token walk reads: everything already memoized.
pub(crate) struct Inputs<'a> {
    /// The file's resolution side table.
    pub resolutions: &'a Resolutions,
    /// The package's declaration tables.
    pub package: &'a Package,
    /// The package's function signatures (a const binding of a function
    /// is a `Function`, not a `Variable`).
    pub fn_defs: &'a FxHashSet<DefId>,
    /// The file's scope-index frames.
    pub frames: &'a [ScopeFrame],
}

/// The delta-encoded whole-document token stream.
pub(crate) fn stream(
    files: &Files,
    file: FileId,
    ast: &Ast,
    inputs: &Inputs<'_>,
) -> Vec<lsp::SemanticToken> {
    let source = files.source(file);
    let mut locals = FxHashMap::default();
    for frame in inputs.frames {
        for binding in &frame.bindings {
            locals.insert(binding.span, binding.kind);
        }
    }
    let mut walker = Walker {
        file,
        source,
        resolutions: inputs.resolutions,
        package: inputs.package,
        fn_defs: inputs.fn_defs,
        locals: &locals,
        out: Vec::new(),
    };
    Walk::new(&mut walker).root(ast);
    let mut raw = walker.out;
    raw.sort_by_key(|token| (token.start, token.len));
    // Doc runs ride twice (the attachment slot and the leading trivia), so
    // identical spans collapse; no two distinct classes ever share a span.
    raw.dedup_by_key(|token| (token.start, token.len));
    encode(files, file, source, &raw)
}

/// One token before encoding: byte start, byte length, class, bits.
struct Raw {
    start: u32,
    len: u32,
    tok: Tok,
    mods: u32,
}

/// Sorts by span and delta-encodes: line and UTF-16 character deltas
/// against the previous token, lengths in UTF-16 units clamped to the
/// start line (a block comment spanning lines ends at its first line's
/// end).
fn encode(files: &Files, file: FileId, source: &str, raw: &[Raw]) -> Vec<lsp::SemanticToken> {
    let mut out = Vec::with_capacity(raw.len());
    let mut last_line = 0;
    let mut last_char = 0;
    for token in raw {
        let start = convert::position(files, file, token.start);
        // The length stops at the start line's end: the protocol carries
        // one line's run per token.
        let line = files.line_span(file, files.location(file, token.start).line);
        let end = (token.start + token.len).min(line.end);
        let units: u32 = source[token.start as usize..end as usize]
            .chars()
            .map(|ch| u32::try_from(ch.len_utf16()).expect("a UTF-16 length fits u32"))
            .sum();
        let delta_line = start.line - last_line;
        let delta_char =
            if delta_line == 0 { start.character - last_char } else { start.character };
        out.push(lsp::SemanticToken {
            delta_line,
            delta_start: delta_char,
            length: units,
            token_type: token.tok.index(),
            token_modifiers_bitset: token.mods,
        });
        last_line = start.line;
        last_char = start.character;
    }
    out
}

/// The token walk's visitor over the shared walk: each hook emits the
/// token classes one covered node owns.
struct Walker<'a> {
    file: FileId,
    source: &'a str,
    resolutions: &'a Resolutions,
    package: &'a Package,
    fn_defs: &'a FxHashSet<DefId>,
    /// Binder span to its frame kind (parameter versus local).
    locals: &'a FxHashMap<Span, ScopeKind>,
    out: Vec<Raw>,
}

impl Walker<'_> {
    /// Emits one span of this file; empty and foreign spans drop.
    fn push(&mut self, span: Span, tok: Tok, mods: u32) {
        if span.file == self.file && span.start < span.end {
            self.out.push(Raw {
                start: span.start,
                len: span.end - span.start,
                tok,
                mods,
            });
        }
    }

    /// Emits one comment's span.
    fn trivia(&mut self, trivia: &Trivia) {
        self.push(trivia.span, Tok::Comment, 0);
    }

    /// Emits the keyword `word` at `at`, only when the source spells it
    /// there as a whole word (the declaration spans start at their
    /// keywords; the check keeps a mangled span from painting noise).
    fn keyword(&mut self, at: u32, word: &str) {
        let start = at as usize;
        let end = start + word.len();
        let whole = self.source.get(start..end) == Some(word)
            && self
                .source
                .as_bytes()
                .get(end)
                .is_none_or(|b| !(b.is_ascii_alphanumeric() || *b == b'_' || *b >= 0x80));
        if whole {
            self.push(
                Span::new(
                    self.file,
                    at,
                    at + u32::try_from(word.len()).expect("a keyword length fits u32"),
                ),
                Tok::Keyword,
                0,
            );
        }
    }

    /// Emits the keyword `word` at the first non-whitespace byte after
    /// `from` (the `fn` behind `extern`).
    fn keyword_after(&mut self, from: u32, word: &str) {
        let rest = &self.source[from as usize..];
        let lead = rest.len() - rest.trim_start().len();
        self.keyword(
            from + u32::try_from(lead).expect("an offset fits u32"),
            word,
        );
    }

    /// Emits `text` (an operator or a mid-source keyword) when it is the
    /// whole trimmed gap between two offsets — a gap that a comment
    /// occupies answers nothing.
    fn between(&mut self, from: u32, to: u32, text: &str, tok: Tok) {
        let Some(gap) = self.source.get(from as usize..to as usize) else {
            return;
        };
        if gap.trim() == text {
            let lead = gap.len() - gap.trim_start().len();
            let start = from + u32::try_from(lead).expect("an offset fits u32");
            self.push(
                Span::new(
                    self.file,
                    start,
                    start + u32::try_from(text.len()).expect("an operator length fits u32"),
                ),
                tok,
                0,
            );
        }
    }

    /// Emits the operator `text` at its only occurrence inside the
    /// separator gap between two offsets — the gaps that legally carry
    /// punctuation beside the operator (the `->` behind an empty
    /// parameter list, the `=` behind an annotation).
    fn op_in_gap(&mut self, from: u32, to: u32, text: &str) {
        let Some(gap) = self.source.get(from as usize..to as usize) else {
            return;
        };
        if let Some(at) = gap.find(text) {
            let start = from + u32::try_from(at).expect("an offset fits u32");
            self.push(
                Span::new(
                    self.file,
                    start,
                    start + u32::try_from(text.len()).expect("an operator length fits u32"),
                ),
                Tok::Operator,
                0,
            );
        }
    }

    /// The class of one package definition, at its declaration or a use.
    fn def_tok(&self, id: DefId, declaration: bool) -> (Tok, u32) {
        let mut mods = if declaration { DECLARATION } else { 0 };
        let tok = match self.package.def(id).kind {
            DefKind::Fn => Tok::Function,
            // A const binding whose value is a function (a written `fn`
            // converts to one) stays a function at uses and decl alike.
            DefKind::Const if self.fn_defs.contains(&id) => Tok::Function,
            DefKind::Const => {
                mods |= READONLY;
                Tok::Variable
            },
            DefKind::Var => Tok::Variable,
            DefKind::Struct => Tok::Struct,
            DefKind::Enum => Tok::Enum,
            DefKind::Tag | DefKind::Case => Tok::EnumMember,
            DefKind::Trait => Tok::Interface,
            DefKind::TypeAlias => Tok::Type,
        };
        (tok, mods)
    }

    /// The class of one local binder: its frame kind decides parameter
    /// versus variable.
    fn local_tok(&self, bind: Span) -> Tok {
        match self.locals.get(&bind) {
            Some(ScopeKind::Param) => Tok::Parameter,
            _ => Tok::Variable,
        }
    }

    /// One identifier use in value position: the side table's answer at
    /// the exact use span classifies it; no record, no token.
    fn value_use(&mut self, ident: &Ident) {
        let Some(res) = self.resolutions.get(ident.span) else {
            return;
        };
        let (tok, mods) = match res {
            Res::Def(id) => self.def_tok(*id, false),
            // `self` is the language's receiver spelling wherever it
            // resolves to a binder.
            Res::Local { .. } if ident.name == "self" => (Tok::Keyword, 0),
            Res::Local { bind } => (self.local_tok(*bind), 0),
            Res::TraitMethod(_) | Res::Dot(DotRes::Methods { .. } | DotRes::Static { .. }) => {
                (Tok::Method, 0)
            },
            Res::Dot(DotRes::Free { .. } | DotRes::Module { .. }) => (Tok::Function, 0),
            // The `.err`/`.ok` handler forms are surface syntax, and a
            // bare module alias is not a value this round.
            Res::Handler | Res::Module(_) => (Tok::Keyword, 0),
        };
        self.push(ident.span, tok, mods);
    }

    /// One identifier use in type position: the in-scope type variables
    /// (and a bound `Self`) answer as type parameters ahead of the type
    /// namespace; the universe builtins (`none`) answer nothing.
    fn type_use(&mut self, ident: &Ident, scopes: &TypeScopes) {
        let tok = if scopes.binds(&ident.name) {
            Tok::TypeParam
        } else if let Some(id) = self.package.lookup_type(&ident.name) {
            self.def_tok(id, false).0
        } else {
            return;
        };
        self.push(ident.span, tok, 0);
    }

    /// Emits one literal's class (bool is a keyword; char and byte fold
    /// into string).
    fn literal(&mut self, span: Span, lit: &Literal) {
        let tok = match lit {
            Literal::Int { .. } | Literal::Float { .. } => Tok::Number,
            Literal::Bool(_) => Tok::Keyword,
            Literal::Char(_) | Literal::Byte(_) => Tok::Str,
        };
        self.push(span, tok, 0);
    }
}

impl Visit for Walker<'_> {
    fn package(&self) -> &Package {
        self.package
    }

    fn enter_item(&mut self, item: &Item) -> bool {
        // Derive-synthesized trees (whose spans lie inside the
        // attributed declaration) never emit.
        if item.synthesized {
            return false;
        }
        for trivia in item
            .leading_trivia
            .iter()
            .chain(item.doc.iter())
            .chain(item.trailing_trivia.iter())
        {
            self.trivia(trivia);
        }
        if let Some(vis) = item.vis {
            self.push(vis.span(), Tok::Keyword, 0);
        }
        true
    }

    fn at_local_item(&mut self, local: &LocalDecl) {
        // A fn-converted binding names one function; every other binder
        // is a package const/var of the pattern's spelling.
        let fn_form = matches!(local.init, Expr::Lambda { .. });
        self.keyword(
            local.span.start,
            if fn_form {
                "fn"
            } else if local.mutable {
                "var"
            } else {
                "const"
            },
        );
        let from = local
            .ty
            .as_ref()
            .map_or(pattern_span(&local.pat).end, |ty| ty.span().end);
        self.op_in_gap(from, local.init.span().start, "=");
        let (tok, mods) = if fn_form {
            (Tok::Function, DECLARATION)
        } else if local.mutable {
            (Tok::Variable, DECLARATION)
        } else {
            (Tok::Variable, DECLARATION | READONLY)
        };
        for span in binder_spans(&local.pat) {
            self.push(span, tok, mods);
        }
    }

    fn at_fn(&mut self, f: &FnDecl, member: bool) {
        if member {
            for trivia in &f.doc {
                self.trivia(trivia);
            }
        }
        self.keyword(f.span.start, "fn");
        let (tok, mods) =
            if member { (Tok::Method, DECLARATION) } else { (Tok::Function, DECLARATION) };
        self.push(f.name.span, tok, mods);
    }

    fn at_extern_fn(&mut self, f: &FnDecl) {
        self.keyword(f.span.start, "extern");
        // The bind target's literal sits between the keywords; `fn`
        // highlights past it when present.
        let fn_at = f.bind.as_ref().map_or(
            f.span.start + u32::try_from("extern".len()).expect("an offset fits u32"),
            |bind| bind.span.end,
        );
        self.keyword_after(fn_at, "fn");
        self.push(f.name.span, Tok::Function, DECLARATION);
    }

    fn at_struct(&mut self, s: &StructDecl) {
        self.keyword(s.span.start, "struct");
        self.push(s.name.span, Tok::Struct, DECLARATION);
    }

    fn at_field(&mut self, field: &FieldDecl) {
        for trivia in &field.doc {
            self.trivia(trivia);
        }
        self.push(field.name.span, Tok::Property, DECLARATION);
    }

    fn at_deinit(&mut self, span: Span) {
        self.keyword(span.start, "deinit");
    }

    fn at_enum(&mut self, e: &EnumDecl) {
        self.keyword(e.span.start, "enum");
        self.push(e.name.span, Tok::Enum, DECLARATION);
    }

    fn at_case(&mut self, case: &EnumCase) {
        self.push(case.name.span, Tok::EnumMember, DECLARATION);
    }

    fn at_alias(&mut self, a: &TypeAliasDecl) {
        self.keyword(a.span.start, "type");
        self.push(a.name.span, Tok::Type, DECLARATION);
        self.op_in_gap(a.name.span.end, a.ty.span().start, "=");
    }

    fn at_trait(&mut self, t: &TraitDecl) {
        self.keyword(t.span.start, "trait");
        self.push(t.name.span, Tok::Interface, DECLARATION);
    }

    fn at_assoc_type(&mut self, name: &Ident) {
        self.push(name.span, Tok::Type, DECLARATION);
    }

    fn at_impl(&mut self, imp: &ImplDecl) {
        self.keyword(imp.span.start, "impl");
        if let Some(trait_ref) = &imp.trait_ref {
            self.between(
                trait_ref.span.end,
                imp.target.span().start,
                "for",
                Tok::Keyword,
            );
        }
    }

    fn at_import(&mut self, decl: &ImportDecl) {
        self.keyword(decl.span.start, "import");
    }

    fn at_tag(&mut self, t: &TagDecl) {
        self.keyword(t.span.start, "tag");
        self.push(t.name.span, Tok::EnumMember, DECLARATION);
    }

    fn at_platform(&mut self, p: &PlatformDecl) {
        self.keyword(p.span.start, "platform");
        self.push(p.name.span, Tok::Str, 0);
    }

    fn at_type_param(&mut self, param: &TypeParam) {
        self.push(param.name.span, Tok::TypeParam, DECLARATION);
    }

    fn at_param(&mut self, param: &Param) {
        match param.mode {
            ParamMode::Borrow => {},
            ParamMode::Mut(span) | ParamMode::Consume(span) | ParamMode::Comptime(span) => {
                self.push(span, Tok::Keyword, 0);
            },
        }
        let (tok, mods) = if param.name.name == "self" {
            (Tok::Keyword, DECLARATION)
        } else {
            (Tok::Parameter, DECLARATION)
        };
        self.push(param.name.span, tok, mods);
        if let Some(default) = &param.default {
            let from = param
                .ty
                .as_ref()
                .map_or(param.name.span.end, |ty| ty.span().end);
            self.op_in_gap(from, default.span().start, "=");
        }
    }

    fn at_ret_arrow(&mut self, from: u32, ret: &Type) {
        self.op_in_gap(from, ret.span().start, "->");
    }

    fn at_block(&mut self, block: &BlockExpr) {
        for trivia in &block.trailing_trivia {
            self.trivia(trivia);
        }
    }

    fn at_stmt(&mut self, stmt: &Stmt) {
        for trivia in stmt
            .leading_trivia
            .iter()
            .chain(stmt.trailing_trivia.iter())
        {
            self.trivia(trivia);
        }
        if let StmtKind::Local(local) = &stmt.kind {
            self.keyword(
                local.span.start,
                if local.mutable { "var" } else { "const" },
            );
            let from = local
                .ty
                .as_ref()
                .map_or(pattern_span(&local.pat).end, |ty| ty.span().end);
            self.op_in_gap(from, local.init.span().start, "=");
            for span in binder_spans(&local.pat) {
                self.push(span, Tok::Variable, DECLARATION);
            }
        }
    }

    fn at_expr(&mut self, expr: &Expr) {
        match expr {
            Expr::Literal { span, lit } => self.literal(*span, lit),
            Expr::RawStr { span, .. } => self.push(*span, Tok::Str, 0),
            Expr::Path { path, .. } => {
                for segment in &path.segments {
                    self.value_use(segment);
                }
            },
            Expr::Struct { path, fields, .. } => {
                for segment in &path.segments {
                    self.value_use(segment);
                }
                for (field, value) in fields {
                    match value {
                        // Punning: the field name is a value use.
                        None => self.value_use(field),
                        Some(_) => self.push(field.span, Tok::Property, 0),
                    }
                }
            },
            Expr::Call { args, .. } => {
                for arg in args {
                    if let Some(name) = &arg.name {
                        self.push(name.span, Tok::Parameter, 0);
                    }
                }
            },
            // Bare field access records no resolution (the checker's
            // type-directed work), so the member name stays plain.
            Expr::Field { name, .. } => self.value_use(name),
            Expr::Try { span, expr } => self.between(expr.span().end, span.end, "?", Tok::Operator),
            Expr::Unary { span, op, expr } => self.between(
                span.start,
                expr.span().start,
                unary_spelling(*op),
                Tok::Operator,
            ),
            Expr::Is { expr, ty, .. } => {
                self.between(expr.span().end, ty.span().start, "is", Tok::Keyword);
            },
            Expr::Binary {
                op, left, right, ..
            } => {
                let (text, tok) = binary_spelling(*op);
                self.between(left.span().end, right.span().start, text, tok);
            },
            Expr::Range {
                inclusive,
                start,
                end,
                ..
            } => self.between(
                start.span().end,
                end.span().start,
                if *inclusive { "..=" } else { ".." },
                Tok::Operator,
            ),
            Expr::Assign {
                op, left, right, ..
            } => self.between(
                left.span().end,
                right.span().start,
                assign_spelling(*op),
                Tok::Operator,
            ),
            Expr::If {
                span, then, else_, ..
            } => {
                self.keyword(span.start, "if");
                if let Some(else_expr) = else_ {
                    self.between(then.span.end, else_expr.span().start, "else", Tok::Keyword);
                }
            },
            Expr::Match { span, .. } => self.keyword(span.start, "match"),
            Expr::While { span, .. } => self.keyword(span.start, "while"),
            Expr::For {
                span, pat, iter, ..
            } => {
                self.keyword(span.start, "for");
                self.between(pattern_span(pat).end, iter.span().start, "in", Tok::Keyword);
            },
            Expr::Return { span, .. } => self.keyword(span.start, "return"),
            Expr::Break(span) | Expr::Continue(span) => self.push(*span, Tok::Keyword, 0),
            Expr::Nursery { span, .. } => self.keyword(span.start, "nursery"),
            Expr::Spawn { span, .. } => self.keyword(span.start, "spawn"),
            Expr::Defer { span, .. } => self.keyword(span.start, "defer"),
            Expr::Lambda { .. }
            | Expr::Index { .. }
            | Expr::MapApply { .. }
            | Expr::List { .. }
            | Expr::Tuple { .. }
            | Expr::Paren { .. }
            | Expr::Str { .. }
            | Expr::FnType { .. }
            // a conversion placeholder's span is rejected source, not a
            // token
            | Expr::Invalid(_)
            | Expr::Block(_) => {},
        }
    }

    /// One pattern: its binders are declarations (a punned struct-pattern
    /// field binds), its literals color, and its constructor paths answer
    /// through the side table like any value use.
    /// One pattern: its binders are declarations (a punned struct-pattern
    /// field binds), its literals color, and its constructor paths answer
    /// through the side table like any value use.
    fn at_pattern(&mut self, pat: &Pattern) {
        match pat {
            Pattern::Binding { name, .. } => self.push(name.span, Tok::Variable, DECLARATION),
            Pattern::Literal { span, lit } => self.literal(*span, lit),
            Pattern::RawStr { span, .. } => self.push(*span, Tok::Str, 0),
            Pattern::Path { path, .. } | Pattern::Call { path, .. } => {
                for segment in &path.segments {
                    self.value_use(segment);
                }
            },
            Pattern::Struct { path, fields, .. } => {
                for segment in &path.segments {
                    self.value_use(segment);
                }
                for (field, pat) in fields {
                    match pat {
                        // Punning: the field name binds.
                        None => self.push(field.span, Tok::Variable, DECLARATION),
                        Some(_) => self.push(field.span, Tok::Property, 0),
                    }
                }
            },
            Pattern::Wildcard(_)
            | Pattern::Str { .. }
            | Pattern::Tuple { .. }
            | Pattern::Or { .. } => {},
        }
    }

    fn at_str(&mut self, span: Span, _parts: &[StrPart], holed: bool) {
        // A literal without holes is one token over its whole span
        // (quotes included).
        if !holed {
            self.push(span, Tok::Str, 0);
        }
    }

    fn at_str_text(&mut self, span: Span, holed: bool) {
        if holed {
            self.push(span, Tok::Str, 0);
        }
    }

    fn at_str_ident(&mut self, name: &Ident, holed: bool) {
        if holed {
            self.value_use(name);
        }
    }

    /// One type expression's own surface.
    fn at_type(&mut self, ty: &Type, scopes: &TypeScopes) {
        match ty {
            Type::Path { path, .. } => {
                // Multi-segment type paths are module paths the resolver
                // refuses; only the single form resolves.
                if path.segments.len() == 1 {
                    self.type_use(&path.segments[0], scopes);
                }
            },
            Type::Optional { span, inner, .. } => {
                self.between(inner.span().end, span.end, "?", Tok::Operator);
            },
            Type::Negation { span, inner, .. } => {
                self.between(span.start, inner.span().start, "~", Tok::Operator);
            },
            Type::Union { left, right, .. } => {
                self.between(left.span().end, right.span().start, "|", Tok::Operator);
            },
            Type::Intersect { left, right, .. } => {
                self.between(left.span().end, right.span().start, "&", Tok::Operator);
            },
            Type::Difference { left, right, .. } => {
                self.between(left.span().end, right.span().start, "-", Tok::Operator);
            },
            Type::Fn { span, params, ret } => {
                self.keyword(span.start, "fn");
                let after = params.last().map_or(span.start, |p| p.span.end);
                if let Some(ret) = ret {
                    self.op_in_gap(after, ret.span().start, "->");
                }
            },
            Type::Ptr {
                span,
                mutable,
                inner,
            } => self.between(
                span.start,
                inner.span().start,
                if *mutable { "*mut" } else { "*const" },
                Tok::Operator,
            ),
            Type::TypeKw(span) => self.keyword(span.start, "type"),
            // The unit type is punctuation and the opaque foreign form's
            // name is a string argument; neither answers.
            Type::Unit(_) | Type::Paren { .. } | Type::Tuple { .. } | Type::Foreign { .. } => {},
        }
    }

    fn at_type_name(&mut self, ident: &Ident, scopes: &TypeScopes) {
        self.type_use(ident, scopes);
    }
}

/// Span of one pattern (every variant carries its own extent).
fn pattern_span(pat: &Pattern) -> Span {
    match pat {
        Pattern::Wildcard(span)
        | Pattern::Literal { span, .. }
        | Pattern::Str { span, .. }
        | Pattern::RawStr { span, .. }
        | Pattern::Path { span, .. }
        | Pattern::Call { span, .. }
        | Pattern::Struct { span, .. }
        | Pattern::Tuple { span, .. }
        | Pattern::Or { span, .. }
        | Pattern::Binding { span, .. } => *span,
    }
}

/// Every binder name span inside one pattern, in source order (a punned
/// struct-pattern field binds itself).
fn binder_spans(pat: &Pattern) -> Vec<Span> {
    fn walk(pat: &Pattern, out: &mut Vec<Span>) {
        match pat {
            Pattern::Binding { name, .. } => out.push(name.span),
            Pattern::Call { args, .. } => {
                for arg in args {
                    walk(arg, out);
                }
            },
            Pattern::Struct { fields, .. } => {
                for (field, pat) in fields {
                    match pat {
                        Some(pat) => walk(pat, out),
                        None => out.push(field.span),
                    }
                }
            },
            Pattern::Tuple { pats, .. } | Pattern::Or { pats, .. } => {
                for pat in pats {
                    walk(pat, out);
                }
            },
            _ => {},
        }
    }
    let mut out = Vec::new();
    walk(pat, &mut out);
    out
}

/// The source spelling of one unary operator.
fn unary_spelling(op: UnaryOp) -> &'static str {
    match op {
        UnaryOp::Neg => "-",
        UnaryOp::WrapNeg => "-%",
        UnaryOp::Not => "!",
    }
}

/// The source spelling of one binary operator (`in` is a keyword).
fn binary_spelling(op: BinaryOp) -> (&'static str, Tok) {
    match op {
        BinaryOp::Add => ("+", Tok::Operator),
        BinaryOp::WrapAdd => ("+%", Tok::Operator),
        BinaryOp::Sub => ("-", Tok::Operator),
        BinaryOp::WrapSub => ("-%", Tok::Operator),
        BinaryOp::Mul => ("*", Tok::Operator),
        BinaryOp::WrapMul => ("*%", Tok::Operator),
        BinaryOp::Div => ("/", Tok::Operator),
        BinaryOp::Rem => ("%", Tok::Operator),
        BinaryOp::Shl => ("<<", Tok::Operator),
        BinaryOp::Shr => (">>", Tok::Operator),
        BinaryOp::Eq => ("==", Tok::Operator),
        BinaryOp::Ne => ("!=", Tok::Operator),
        BinaryOp::Lt => ("<", Tok::Operator),
        BinaryOp::Le => ("<=", Tok::Operator),
        BinaryOp::Gt => (">", Tok::Operator),
        BinaryOp::Ge => (">=", Tok::Operator),
        BinaryOp::In => ("in", Tok::Keyword),
        BinaryOp::And => ("&&", Tok::Operator),
        BinaryOp::Or => ("||", Tok::Operator),
        BinaryOp::BitAnd => ("&", Tok::Operator),
        BinaryOp::BitOr => ("|", Tok::Operator),
        BinaryOp::BitXor => ("^", Tok::Operator),
    }
}

/// The source spelling of one assignment operator.
fn assign_spelling(op: AssignOp) -> &'static str {
    match op {
        AssignOp::Plain => "=",
        AssignOp::Add => "+=",
        AssignOp::Sub => "-=",
        AssignOp::Mul => "*=",
        AssignOp::Div => "/=",
        AssignOp::Rem => "%=",
        AssignOp::Shl => "<<=",
        AssignOp::Shr => ">>=",
    }
}
