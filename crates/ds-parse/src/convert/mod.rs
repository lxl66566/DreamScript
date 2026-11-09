//! Conversion of the pest pair tree into the spanned AST.
//!
//! Split by area: this module owns the conversion state ([`Ctx`]), the
//! file/block/item/declaration walk, and shared helpers; [`expr`],
//! [`r#type`], and [`lit`] hold expression, type/pattern, and literal
//! conversion.
//!
//! The grammar is total over accepted input, so conversion is total too:
//! shapes the grammar cannot produce record a diagnostic and yield a
//! placeholder instead of panicking. Value-level validation also happens here
//! (raw-string hash counts, byte-literal ASCII, `\u{...}` range), producing
//! two-span diagnostics where the record names two locations.
// Span offsets are u32 by contract (Files::add_source rejects longer
// sources), so narrowing casts from usize are total.
#![allow(clippy::cast_possible_truncation)]
// pest `Pair` is a cheap shared handle; passing it by value is idiomatic.
#![allow(clippy::needless_pass_by_value)]
#![allow(clippy::unused_self)]

use ds_diag::{Diagnostic, ErrorCode, FileId, Span};
use pest::iterators::{Pair, Pairs};

use crate::{Rule, ast::*};

/// Cap on the terms one unbracketed operator chain may fold. Each folded
/// term adds one nested `Expr` level, and dropping a `Box` chain recurses at
/// roughly 100-250 bytes of stack per level on the caller's thread (not the
/// 8 MiB parse thread) — a ~10^4-term chain aborts there. 128 terms is far
/// past any written chain and keeps worst-case stacked folds (every
/// precedence level at the cap is ~14 × 128 levels) well inside a 1 MiB
/// stack.
pub(crate) const MAX_CHAIN_TERMS: usize = 128;

/// Conversion state: the file every span belongs to, plus diagnostics found
/// while converting (value-level errors the pair tree alone cannot express).
pub(crate) struct Ctx<'a> {
    file: FileId,
    src: &'a str,
    diags: Vec<Diagnostic>,
}

/// Converts the `file` pair into an [`Ast`].
pub(crate) fn convert(file: FileId, src: &str, pairs: Pairs<Rule>) -> Result<Ast, Vec<Diagnostic>> {
    let mut ctx = Ctx {
        file,
        src,
        diags: Vec::new(),
    };
    let Some(file_pair) = pairs.into_iter().next() else {
        return Err(vec![Diagnostic::error(
            ErrorCode::UnexpectedEof,
            "parser produced no tree",
        )]);
    };
    let ast = ctx.conv_file(file_pair);
    if ctx.diags.is_empty() { Ok(ast) } else { Err(ctx.diags) }
}

/// Non-trivia children of a pair.
fn inner<'a>(p: &Pair<'a, Rule>) -> impl Iterator<Item = Pair<'a, Rule>> {
    p.clone().into_inner().filter(|c| !is_trivia(c))
}

fn is_trivia(p: &Pair<'_, Rule>) -> bool {
    matches!(p.as_rule(), Rule::line_comment | Rule::block_comment)
}

fn join(a: Span, b: Span) -> Span {
    Span::new(a.file, a.start.min(b.start), a.end.max(b.end))
}

fn empty_block(span: Span) -> BlockExpr {
    BlockExpr {
        span,
        stmts: Vec::new(),
        trailing_trivia: Vec::new(),
    }
}

impl Ctx<'_> {
    fn span(&self, p: &Pair<'_, Rule>) -> Span {
        let s = p.as_span();
        Span::new(self.file, s.start() as u32, s.end() as u32)
    }

    fn err(&mut self, span: Span, code: ErrorCode, msg: impl Into<String>) {
        self.diags
            .push(Diagnostic::error(code, msg).with_primary(span, "here"));
    }

    /// Records a [`ErrorCode::NestingTooDeep`] diagnostic and returns true
    /// when a flat fold of `terms` operands would exceed
    /// [`MAX_CHAIN_TERMS`]; the caller returns a placeholder without
    /// folding, so no oversized chain is ever built (and later dropped).
    fn chain_guard(&mut self, terms: usize, span: Span) -> bool {
        if terms <= MAX_CHAIN_TERMS {
            return false;
        }
        self.err(
            span,
            ErrorCode::NestingTooDeep,
            format!(
                "an operator chain of {terms} terms exceeds the parser limit of {MAX_CHAIN_TERMS}"
            ),
        );
        true
    }

    // ===== file / blocks / items =====

    fn conv_file(&mut self, p: Pair<'_, Rule>) -> Ast {
        let span = self.span(&p);
        let mut blocks: Vec<Block> = Vec::new();
        let mut pending: Vec<Trivia> = Vec::new();
        let mut trailing: Vec<Trivia> = Vec::new();
        for c in p.into_inner() {
            match c.as_rule() {
                Rule::line_comment | Rule::block_comment => pending.push(self.trivia(&c)),
                Rule::file_blocks => {
                    self.conv_file_blocks(c, &mut blocks, &mut pending, &mut trailing);
                },
                _ => {},
            }
        }
        if blocks.is_empty() {
            blocks.push(Block {
                span,
                items: Vec::new(),
                trailing_trivia: std::mem::take(&mut pending),
            });
        } else if !pending.is_empty() {
            // Comments before the first block lead its first item, if any.
            if let Some(first) = blocks.first_mut() {
                if let Some(item) = first.items.first_mut() {
                    let mut lead = std::mem::take(&mut item.leading_trivia);
                    lead.splice(0..0, pending.drain(..));
                    item.leading_trivia = lead;
                } else {
                    first.trailing_trivia.append(&mut pending);
                }
            }
        }
        if let Some(last) = blocks.last_mut() {
            last.trailing_trivia.extend(trailing);
        }
        Ast {
            file: self.file,
            span,
            blocks,
        }
    }

    fn conv_file_blocks(
        &mut self,
        p: Pair<'_, Rule>,
        blocks: &mut Vec<Block>,
        pending: &mut Vec<Trivia>,
        trailing: &mut Vec<Trivia>,
    ) {
        for c in p.into_inner() {
            match c.as_rule() {
                Rule::file_block => {
                    let mut block = self.conv_block_unit(c, pending);
                    block.trailing_trivia.append(pending);
                    blocks.push(block);
                },
                Rule::block_sep => {
                    if let Some(last) = blocks.last_mut() {
                        last.trailing_trivia.append(pending);
                    } else {
                        trailing.append(pending);
                    }
                },
                Rule::line_comment | Rule::block_comment => pending.push(self.trivia(&c)),
                _ => {},
            }
        }
    }

    fn conv_block_unit(&mut self, p: Pair<'_, Rule>, pending: &mut Vec<Trivia>) -> Block {
        let span = self.span(&p);
        let mut items = Vec::new();
        for c in p.into_inner() {
            match c.as_rule() {
                Rule::item => {
                    let mut item = self.conv_item(c.clone());
                    item.leading_trivia = std::mem::take(pending);
                    let end_line = self.line(item.span.end);
                    items.push((end_line, item));
                },
                Rule::line_comment | Rule::block_comment => {
                    let t = self.trivia(&c);
                    let start_line = self.line(t.span.start);
                    // A comment on the same line as the previous item's end
                    // trails that item; anything else leads the next one.
                    let attaches_behind = start_line == items.last().map_or(u32::MAX, |(l, _)| *l);
                    if attaches_behind {
                        items
                            .last_mut()
                            .expect("guarded by map_or")
                            .1
                            .trailing_trivia
                            .push(t);
                    } else {
                        pending.push(t);
                    }
                },
                _ => {},
            }
        }
        let items = items.into_iter().map(|(_, item)| item).collect();
        Block {
            span,
            items,
            trailing_trivia: Vec::new(),
        }
    }

    /// 1-based line number of a byte offset.
    fn line(&self, offset: u32) -> u32 {
        1 + self.src[..offset as usize]
            .bytes()
            .filter(|&b| b == b'\n')
            .count() as u32
    }

    fn trivia(&self, p: &Pair<'_, Rule>) -> Trivia {
        Trivia {
            span: self.span(p),
            kind: match p.as_rule() {
                Rule::line_comment => TriviaKind::Line,
                _ => TriviaKind::Block,
            },
        }
    }

    /// Recursively collects comment pairs not owned by a nested container.
    fn deep_trivia(&self, p: &Pair<'_, Rule>, out: &mut Vec<Trivia>) {
        for c in p.clone().into_inner() {
            match c.as_rule() {
                Rule::line_comment | Rule::block_comment => out.push(self.trivia(&c)),
                // Nested blocks own their statement trivia themselves.
                Rule::block_expr | Rule::file_block => {},
                _ => self.deep_trivia(&c, out),
            }
        }
    }

    fn conv_item(&mut self, p: Pair<'_, Rule>) -> Item {
        let span = self.span(&p);
        let mut attrs = Vec::new();
        let mut vis = None;
        let mut kind = None;
        for c in inner(&p) {
            match c.as_rule() {
                Rule::attr => attrs.push(self.conv_attr(c)),
                Rule::vis => vis = Some(self.conv_vis(c)),
                Rule::import_item => kind = Some(ItemKind::Import(self.conv_import(c))),
                Rule::local => kind = Some(ItemKind::Local(self.conv_local(c))),
                Rule::fn_item => kind = Some(ItemKind::Fn(self.conv_fn(c, false))),
                Rule::extern_fn_item => kind = Some(ItemKind::ExternFn(self.conv_fn(c, true))),
                Rule::struct_item => kind = Some(ItemKind::Struct(self.conv_struct(c))),
                Rule::enum_item => kind = Some(ItemKind::Enum(self.conv_enum(c))),
                Rule::tag_item => kind = Some(ItemKind::Tag(self.conv_tag(c))),
                Rule::type_item => kind = Some(ItemKind::TypeAlias(self.conv_type_alias(c))),
                Rule::trait_item => kind = Some(ItemKind::Trait(self.conv_trait(c))),
                Rule::impl_item => kind = Some(ItemKind::Impl(self.conv_impl(c))),
                _ => {},
            }
        }
        let mut trailing = Vec::new();
        self.deep_trivia(&p, &mut trailing);
        Item {
            span,
            attrs,
            vis,
            kind: kind.unwrap_or(ItemKind::Tag(TagDecl {
                span,
                name: Ident {
                    span,
                    name: String::new(),
                },
            })),
            leading_trivia: Vec::new(),
            trailing_trivia: trailing,
        }
    }

    fn conv_attr(&mut self, p: Pair<'_, Rule>) -> Attribute {
        let span = self.span(&p);
        let mut name = None;
        let mut args = Vec::new();
        for c in inner(&p) {
            match c.as_rule() {
                Rule::ident => name = Some(self.ident(c)),
                Rule::attr_args => {
                    for a in inner(&c) {
                        if a.as_rule() == Rule::attr_arg {
                            if let Some(path) = inner(&a).next() {
                                args.push(self.conv_path(path));
                            }
                        }
                    }
                },
                _ => {},
            }
        }
        Attribute {
            span,
            name: name.unwrap_or(Ident {
                span,
                name: String::new(),
            }),
            args,
        }
    }

    fn conv_vis(&mut self, p: Pair<'_, Rule>) -> Vis {
        let span = self.span(&p);
        if p.as_str().contains('(') { Vis::PubPkg(span) } else { Vis::Pub(span) }
    }

    fn conv_import(&mut self, p: Pair<'_, Rule>) -> ImportDecl {
        let span = self.span(&p);
        let mut path = None;
        let mut select = Vec::new();
        let mut alias = None;
        let mut cond = None;
        for c in inner(&p) {
            match c.as_rule() {
                Rule::import_path => {
                    let at_sign = c.as_str().starts_with('@');
                    let pspan = self.span(&c);
                    let mut segments = Vec::new();
                    for s in inner(&c) {
                        if s.as_rule() == Rule::ident {
                            segments.push(self.ident(s));
                        }
                    }
                    path = Some(ImportPath {
                        span: pspan,
                        at_sign,
                        segments,
                    });
                },
                Rule::import_select => {
                    for s in inner(&c) {
                        if s.as_rule() == Rule::sel_item {
                            let sel_span = self.span(&s);
                            let mut it = inner(&s);
                            let name = it
                                .next()
                                .map(|i| self.ident(i))
                                .unwrap_or(self.bad_ident(sel_span));
                            let sel_alias = it.next().map(|i| self.ident(i));
                            select.push(ImportSel {
                                span: sel_span,
                                name,
                                alias: sel_alias,
                            });
                        }
                    }
                },
                Rule::import_alias => {
                    if let Some(i) = inner(&c).next() {
                        alias = Some(self.ident(i));
                    }
                },
                Rule::import_cond => {
                    // The first child is the `kw_if` pair; the condition
                    // follows it.
                    if let Some(e) = inner(&c).find(|c| c.as_rule() == Rule::expr_nos) {
                        cond = Some(self.expr(e));
                    }
                },
                _ => {},
            }
        }
        ImportDecl {
            span,
            path: path.unwrap_or(ImportPath {
                span,
                at_sign: false,
                segments: Vec::new(),
            }),
            select,
            alias,
            cond,
        }
    }

    fn bad_ident(&self, span: Span) -> Ident {
        Ident {
            span,
            name: String::new(),
        }
    }

    fn conv_local(&mut self, p: Pair<'_, Rule>) -> LocalDecl {
        let span = self.span(&p);
        let mut mutable = false;
        let mut pat = Pattern::Wildcard(span);
        let mut ty = None;
        let mut init = Expr::Break(span);
        for c in inner(&p) {
            match c.as_rule() {
                Rule::local_kw => mutable = c.as_str().starts_with("var"),
                Rule::local_pat => {
                    for lp in inner(&c) {
                        match lp.as_rule() {
                            Rule::pattern => pat = self.pattern(lp),
                            Rule::type_expr => ty = Some(self.type_expr(lp)),
                            _ => {},
                        }
                    }
                },
                _ => init = self.expr(c),
            }
        }
        LocalDecl {
            span,
            mutable,
            pat,
            ty,
            init,
        }
    }

    fn conv_fn(&mut self, p: Pair<'_, Rule>, extern_: bool) -> FnDecl {
        let span = self.span(&p);
        let mut name = None;
        let mut generics = Vec::new();
        let mut params = Vec::new();
        let mut ret = None;
        let mut body = None;
        for c in inner(&p) {
            match c.as_rule() {
                Rule::ident => name = Some(self.ident(c)),
                Rule::generics => generics = self.generics(c),
                Rule::params => params = self.params(c),
                Rule::ret_ann => {
                    if let Some(t) = inner(&c).next() {
                        ret = Some(self.type_expr(t));
                    }
                },
                Rule::block_expr => body = Some(self.block(c)),
                _ => {},
            }
        }
        if extern_ {
            body = None;
        }
        FnDecl {
            span,
            name: name.unwrap_or_else(|| self.bad_ident(span)),
            generics,
            params,
            ret,
            body,
        }
    }

    fn generics(&mut self, p: Pair<'_, Rule>) -> Vec<TypeParam> {
        let mut out = Vec::new();
        for g in inner(&p) {
            if g.as_rule() != Rule::gparam {
                continue;
            }
            let span = self.span(&g);
            let mut name = None;
            let mut bound = None;
            for c in inner(&g) {
                match c.as_rule() {
                    Rule::ident => name = Some(self.ident(c)),
                    Rule::type_expr => bound = Some(self.type_expr(c)),
                    _ => {},
                }
            }
            out.push(TypeParam {
                span,
                name: name.unwrap_or_else(|| self.bad_ident(span)),
                bound,
            });
        }
        out
    }

    fn params(&mut self, p: Pair<'_, Rule>) -> Vec<Param> {
        let mut out = Vec::new();
        for c in inner(&p) {
            if c.as_rule() != Rule::param {
                continue;
            }
            let span = self.span(&c);
            let mut mode = ParamMode::Borrow;
            let mut name = None;
            let mut ty = None;
            let mut default = None;
            for pc in inner(&c) {
                match pc.as_rule() {
                    Rule::param_mode => {
                        let mspan = self.span(&pc);
                        mode = match pc.as_str() {
                            "mut" => ParamMode::Mut(mspan),
                            "move" => ParamMode::Consume(mspan),
                            "comptime" => ParamMode::Comptime(mspan),
                            _ => ParamMode::Borrow,
                        };
                    },
                    Rule::ident => name = Some(self.ident(pc)),
                    Rule::type_expr => ty = Some(self.type_expr(pc)),
                    Rule::param_default => {
                        if let Some(e) = inner(&pc).next() {
                            let dspan = self.span(&pc);
                            let value = self.expr(e);
                            default = Some(self.literal_default(value, dspan));
                        }
                    },
                    _ => {},
                }
            }
            out.push(Param {
                span,
                mode,
                name: name.unwrap_or_else(|| self.bad_ident(span)),
                ty,
                default,
            });
        }
        out
    }

    /// Validates a default argument value as literal-only [F7]: any literal
    /// (a plain string must be hole-free), or a `-`/`-%` applied to one
    /// numeric literal (a negative constant). Anything else records a
    /// diagnostic; the value still carries through.
    fn literal_default(&mut self, expr: Expr, span: Span) -> Expr {
        let literal_only = match &expr {
            Expr::Literal { .. } | Expr::RawStr { .. } => true,
            // An interpolating string is literal only without holes.
            Expr::Str { parts, .. } => parts.iter().all(|p| matches!(p, StrPart::Text { .. })),
            Expr::Unary {
                op: UnaryOp::Neg | UnaryOp::WrapNeg,
                expr: inner,
                ..
            } => matches!(inner.as_ref(), Expr::Literal {
                lit: Literal::Int { .. } | Literal::Float { .. },
                ..
            }),
            _ => false,
        };
        if !literal_only {
            self.err(
                span,
                ErrorCode::NonLiteralDefault,
                "default arguments are literal-only; compute non-literal values in the body",
            );
        }
        expr
    }

    fn conv_struct(&mut self, p: Pair<'_, Rule>) -> StructDecl {
        let span = self.span(&p);
        let mut name = None;
        let mut generics = Vec::new();
        let mut supers = Vec::new();
        let mut members = Vec::new();
        let mut has_body = false;
        for c in inner(&p) {
            match c.as_rule() {
                Rule::ident => name = Some(self.ident(c)),
                Rule::generics => generics = self.generics(c),
                Rule::super_list => {
                    for t in inner(&c) {
                        supers.push(self.type_expr(t));
                    }
                },
                Rule::struct_body => {
                    has_body = true;
                    for m in inner(&c) {
                        match m.as_rule() {
                            Rule::field_member => members.push(StructMember::Field(self.field(m))),
                            Rule::deinit_member => {
                                if let Some(b) = inner(&m).next() {
                                    members.push(StructMember::Deinit {
                                        span: self.span(&m),
                                        body: self.block(b),
                                    });
                                }
                            },
                            _ => {},
                        }
                    }
                },
                _ => {},
            }
        }
        StructDecl {
            span,
            name: name.unwrap_or_else(|| self.bad_ident(span)),
            generics,
            supers,
            members,
            has_body,
        }
    }

    fn field(&mut self, p: Pair<'_, Rule>) -> FieldDecl {
        let span = self.span(&p);
        let mut name = None;
        let mut ty = Type::TypeKw(span);
        for c in inner(&p) {
            match c.as_rule() {
                Rule::ident => name = Some(self.ident(c)),
                Rule::type_expr => ty = self.type_expr(c),
                _ => {},
            }
        }
        FieldDecl {
            span,
            name: name.unwrap_or_else(|| self.bad_ident(span)),
            ty,
        }
    }

    fn conv_enum(&mut self, p: Pair<'_, Rule>) -> EnumDecl {
        let span = self.span(&p);
        let mut name = None;
        let mut generics = Vec::new();
        let mut supers = Vec::new();
        let mut cases = Vec::new();
        for c in inner(&p) {
            match c.as_rule() {
                Rule::ident => name = Some(self.ident(c)),
                Rule::generics => generics = self.generics(c),
                Rule::super_list => {
                    for t in inner(&c) {
                        supers.push(self.type_expr(t));
                    }
                },
                Rule::enum_case => {
                    let cspan = self.span(&c);
                    let mut cname = None;
                    let mut payload = EnumPayload::None;
                    for cc in inner(&c) {
                        match cc.as_rule() {
                            Rule::ident => cname = Some(self.ident(cc)),
                            Rule::case_tuple => {
                                let mut types = Vec::new();
                                for t in inner(&cc) {
                                    types.push(self.type_expr(t));
                                }
                                payload = EnumPayload::Tuple(types);
                            },
                            Rule::case_struct => {
                                let mut fields = Vec::new();
                                for f in inner(&cc) {
                                    if f.as_rule() == Rule::field_member {
                                        fields.push(self.field(f));
                                    }
                                }
                                payload = EnumPayload::Struct(fields);
                            },
                            _ => {},
                        }
                    }
                    cases.push(EnumCase {
                        span: cspan,
                        name: cname.unwrap_or_else(|| self.bad_ident(cspan)),
                        payload,
                    });
                },
                _ => {},
            }
        }
        EnumDecl {
            span,
            name: name.unwrap_or_else(|| self.bad_ident(span)),
            generics,
            supers,
            cases,
        }
    }

    fn conv_tag(&mut self, p: Pair<'_, Rule>) -> TagDecl {
        let span = self.span(&p);
        let name = inner(&p)
            .find(|c| c.as_rule() == Rule::ident)
            .map(|c| self.ident(c));
        TagDecl {
            span,
            name: name.unwrap_or_else(|| self.bad_ident(span)),
        }
    }

    fn conv_type_alias(&mut self, p: Pair<'_, Rule>) -> TypeAliasDecl {
        let span = self.span(&p);
        let mut name = None;
        let mut generics = Vec::new();
        let mut ty = Type::TypeKw(span);
        for c in inner(&p) {
            match c.as_rule() {
                Rule::ident => name = Some(self.ident(c)),
                Rule::generics => generics = self.generics(c),
                Rule::type_expr => ty = self.type_expr(c),
                _ => {},
            }
        }
        TypeAliasDecl {
            span,
            name: name.unwrap_or_else(|| self.bad_ident(span)),
            generics,
            ty,
        }
    }

    fn conv_trait(&mut self, p: Pair<'_, Rule>) -> TraitDecl {
        let span = self.span(&p);
        let mut name = None;
        let mut generics = Vec::new();
        let mut supers = Vec::new();
        let mut members = Vec::new();
        for c in inner(&p) {
            match c.as_rule() {
                Rule::ident => name = Some(self.ident(c)),
                Rule::generics => generics = self.generics(c),
                Rule::super_list => {
                    for t in inner(&c) {
                        supers.push(self.type_expr(t));
                    }
                },
                Rule::member_list => {
                    for m in inner(&c) {
                        match m.as_rule() {
                            Rule::assoc_type => {
                                let mspan = self.span(&m);
                                let aname = inner(&m)
                                    .find(|a| a.as_rule() == Rule::ident)
                                    .map(|a| self.ident(a));
                                members.push(TraitMember::AssocType {
                                    span: mspan,
                                    name: aname.unwrap_or_else(|| self.bad_ident(mspan)),
                                });
                            },
                            Rule::fn_item => members.push(TraitMember::Fn(self.conv_fn(m, false))),
                            Rule::member_const => {
                                members.push(TraitMember::Fn(self.member_const_fn(m)));
                            },
                            _ => {},
                        }
                    }
                },
                _ => {},
            }
        }
        TraitDecl {
            span,
            name: name.unwrap_or_else(|| self.bad_ident(span)),
            generics,
            supers,
            members,
        }
    }

    /// Converts a `const name = (params) -> ret` trait signature (no body)
    /// or a bodied const-arrow member into a [`FnDecl`].
    fn member_const_fn(&mut self, p: Pair<'_, Rule>) -> FnDecl {
        let span = self.span(&p);
        let mut name = None;
        let mut fd = FnDecl {
            span,
            name: Ident {
                span,
                name: String::new(),
            },
            generics: Vec::new(),
            params: Vec::new(),
            ret: None,
            body: None,
        };
        for c in inner(&p) {
            match c.as_rule() {
                Rule::ident => {
                    name = Some(self.ident(c));
                },
                Rule::lambda => {
                    if let Expr::Lambda {
                        generics,
                        params,
                        ret,
                        body,
                        ..
                    } = self.expr(c.clone())
                    {
                        fd.generics = generics;
                        fd.params = params;
                        fd.ret = ret;
                        fd.body = match *body {
                            Expr::Block(b) => Some(b),
                            other => Some(BlockExpr {
                                span: other.span(),
                                stmts: vec![Stmt {
                                    span: other.span(),
                                    kind: StmtKind::Expr(other),
                                    leading_trivia: Vec::new(),
                                    trailing_trivia: Vec::new(),
                                }],
                                trailing_trivia: Vec::new(),
                            }),
                        };
                    }
                },
                Rule::member_sig => {
                    for sc in inner(&c) {
                        match sc.as_rule() {
                            Rule::params => fd.params = self.params(sc),
                            Rule::ret_ann => {
                                if let Some(t) = inner(&sc).next() {
                                    fd.ret = Some(self.type_expr(t));
                                }
                            },
                            _ => {},
                        }
                    }
                },
                _ => {},
            }
        }
        fd.name = name.unwrap_or_else(|| self.bad_ident(span));
        fd
    }

    /// Converts an impl-body `const name = (params) -> ret { body }` local
    /// into a [`FnDecl`]. A non-lambda init (`const x = 1`) is not an impl
    /// member form; it records a diagnostic and yields None — nothing is
    /// silently dropped.
    fn local_as_fn(&mut self, p: Pair<'_, Rule>) -> Option<FnDecl> {
        let local = self.conv_local(p.clone());
        let Expr::Lambda {
            generics,
            params,
            ret,
            body,
            ..
        } = &local.init
        else {
            self.err(
                local.span,
                ErrorCode::InvalidImplMember,
                "impl members are `fn` declarations or const-arrow lambdas, not plain values",
            );
            return None;
        };
        let body_block = match &**body {
            Expr::Block(b) => b.clone(),
            other => BlockExpr {
                span: other.span(),
                stmts: vec![Stmt {
                    span: other.span(),
                    kind: StmtKind::Expr(other.clone()),
                    leading_trivia: Vec::new(),
                    trailing_trivia: Vec::new(),
                }],
                trailing_trivia: Vec::new(),
            },
        };
        let name = match &local.pat {
            Pattern::Binding { name, .. } => name.clone(),
            _ => self.bad_ident(local.span),
        };
        Some(FnDecl {
            span: local.span,
            name,
            generics: generics.clone(),
            params: params.clone(),
            ret: ret.clone(),
            body: Some(body_block),
        })
    }

    fn conv_impl(&mut self, p: Pair<'_, Rule>) -> ImplDecl {
        let span = self.span(&p);
        let mut generics = Vec::new();
        let mut trait_ref = None;
        let mut target = Type::TypeKw(span);
        let mut fns = Vec::new();
        for c in inner(&p) {
            match c.as_rule() {
                Rule::generics => generics = self.generics(c),
                Rule::impl_trait => {
                    let tspan = self.span(&c);
                    let mut path = None;
                    let mut args = Vec::new();
                    for tc in inner(&c) {
                        match tc.as_rule() {
                            Rule::path => path = Some(self.conv_path(tc)),
                            Rule::type_args_bracket | Rule::type_args_angle => {
                                args = self.type_args(tc);
                            },
                            _ => {},
                        }
                    }
                    if let Some(path) = path {
                        trait_ref = Some(TypeRef {
                            span: tspan,
                            path,
                            args,
                        });
                    }
                },
                Rule::type_expr => target = self.type_expr(c),
                Rule::impl_body => {
                    for f in inner(&c) {
                        match f.as_rule() {
                            Rule::fn_item => fns.push(self.conv_fn(f, false)),
                            // `const name = (params) -> t { body }` members
                            // become function declarations.
                            Rule::local => {
                                if let Some(fd) = self.local_as_fn(f.clone()) {
                                    fns.push(fd);
                                }
                            },
                            _ => {},
                        }
                    }
                },
                _ => {},
            }
        }
        ImplDecl {
            span,
            generics,
            trait_ref,
            target,
            fns,
        }
    }

    // ===== paths, idents, type args =====

    fn ident(&mut self, p: Pair<'_, Rule>) -> Ident {
        Ident {
            span: self.span(&p),
            name: p.as_str().to_owned(),
        }
    }

    fn conv_path(&mut self, p: Pair<'_, Rule>) -> Path {
        // `path_atom` is a thin wrapper around `path` in primary position.
        let inner_pair = inner(&p).next();
        let p = match inner_pair {
            Some(inner_p) if p.as_rule() == Rule::path_atom => inner_p,
            _ => p,
        };
        let span = self.span(&p);
        let mut segments = Vec::new();
        for c in inner(&p) {
            if c.as_rule() == Rule::ident {
                segments.push(self.ident(c));
            }
        }
        Path { span, segments }
    }

    fn type_args(&mut self, p: Pair<'_, Rule>) -> Vec<Type> {
        // `type_arg` is a silent wrapper: its children bubble up directly.
        let mut out = Vec::new();
        for t in inner(&p) {
            match t.as_rule() {
                Rule::type_expr => out.push(self.type_expr(t)),
                Rule::string_lit | Rule::raw_hashed | Rule::raw_zero => {
                    out.push(self.foreign_name(t));
                },
                _ => {},
            }
        }
        out
    }

    /// A string-literal type argument names an opaque foreign type
    /// (`c["sqlite3"]`); interpolation holes are rejected.
    fn foreign_name(&mut self, lit: Pair<'_, Rule>) -> Type {
        let span = self.span(&lit);
        match lit.as_rule() {
            Rule::raw_hashed | Rule::raw_zero => {
                let Expr::RawStr { text, .. } = self.raw_str(lit) else {
                    return Type::Foreign {
                        span,
                        name: String::new(),
                    };
                };
                Type::Foreign { span, name: text }
            },
            _ => {
                let Expr::Str { parts, .. } = self.string_lit(lit) else {
                    return Type::Foreign {
                        span,
                        name: String::new(),
                    };
                };
                let mut name = String::new();
                for part in parts {
                    match part {
                        StrPart::Text { text, .. } => name.push_str(&text),
                        _ => self.err(
                            span,
                            ErrorCode::InvalidLiteral,
                            "interpolation is not allowed in a foreign type name",
                        ),
                    }
                }
                Type::Foreign { span, name }
            },
        }
    }
}

fn inner_or_break(ctx: &mut Ctx, p: &Pair<'_, Rule>) -> Expr {
    match inner(p).next() {
        Some(c) => ctx.expr(c),
        None => Expr::Break(ctx.span(p)),
    }
}

fn or_tail_span(tail: &OrTail) -> Span {
    match tail {
        OrTail::Expr(e) => e.span(),
        OrTail::Return { span, .. } | OrTail::Break(span) | OrTail::Continue(span) => *span,
    }
}

fn is_operand_rule(p: &Pair<'_, Rule>) -> bool {
    !matches!(p.as_rule(), Rule::kw_is)
}

fn is_op_rule(p: &Pair<'_, Rule>) -> bool {
    matches!(
        p.as_rule(),
        Rule::cmp_op | Rule::shift_op | Rule::add_op | Rule::mul_op
    )
}

fn is_type_rule(p: &Pair<'_, Rule>) -> bool {
    matches!(
        p.as_rule(),
        Rule::type_expr
            | Rule::type_union
            | Rule::type_inter
            | Rule::type_diff
            | Rule::type_postfix
            | Rule::type_unary
            | Rule::path_type
            | Rule::tuple_type
            | Rule::fn_type
            | Rule::ptr_type
            | Rule::type_kw
    )
}

/// Splits `0xffu8` / `1000000i64` into digits and suffix, mirroring the
/// grammar's greedy digit matching (suffixes start at the first character
/// that cannot belong to the radix's digit set).
fn split_int_suffix(text: &str) -> (String, String) {
    let (radix, prefix_len) = if let Some(rest) = text.strip_prefix("0x") {
        (16, text.len() - rest.len())
    } else if let Some(rest) = text.strip_prefix("0o") {
        (8, text.len() - rest.len())
    } else if let Some(rest) = text.strip_prefix("0b") {
        (2, text.len() - rest.len())
    } else {
        (10, 0)
    };
    let is_digit = |c: char| match radix {
        16 => c.is_ascii_hexdigit(),
        8 => ('0'..='7').contains(&c),
        2 => c == '0' || c == '1',
        _ => c.is_ascii_digit(),
    };
    let digits_end = text[prefix_len..]
        .find(|c: char| !is_digit(c))
        .map_or(text.len(), |i| prefix_len + i);
    (text[..digits_end].to_owned(), text[digits_end..].to_owned())
}

fn split_float_suffix(text: &str) -> (String, String) {
    if let Some(digits) = text
        .strip_suffix("f32")
        .or_else(|| text.strip_suffix("f64"))
    {
        return (digits.to_owned(), text[text.len() - 3..].to_owned());
    }
    (text.to_owned(), String::new())
}

enum Cooked {
    Char(char),
    Invalid(u32),
    Empty,
}

/// Cooks one escape or single-character token text into a `char`.
fn cook_escape(s: &str) -> Cooked {
    let simple = match s {
        "\\n" => Some('\n'),
        "\\t" => Some('\t'),
        "\\r" => Some('\r'),
        "\\0" => Some('\0'),
        "\\\\" => Some('\\'),
        "\\\"" => Some('"'),
        "\\$" => Some('$'),
        "\\'" => Some('\''),
        _ => None,
    };
    if let Some(c) = simple {
        return Cooked::Char(c);
    }
    if let Some(rest) = s.strip_prefix("\\u{").and_then(|r| r.strip_suffix('}')) {
        return match u32::from_str_radix(rest, 16).ok().and_then(char::from_u32) {
            Some(c) => Cooked::Char(c),
            None => Cooked::Invalid(3),
        };
    }
    if s.chars().count() == 1 {
        return Cooked::Char(s.chars().next().unwrap_or('\0'));
    }
    Cooked::Empty
}

mod expr;
mod lit;
mod r#type;
