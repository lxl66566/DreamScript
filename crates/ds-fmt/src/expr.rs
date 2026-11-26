//! Expression, statement, block, and pattern rendering.
//!
//! Precedence levels mirror the grammar chain (assign 1, range 3, is/in 4,
//! `||` 5, `&&` 6, `|` 7, `^` 8, `&` 9, cmp 10, shift 11, add 12,
//! mul 13, unary 14, postfix 15, primary 16). A child renders parenthesized
//! when its level falls below the slot's minimum, which equals the grammar
//! level of the slot's sub-rule — so trees re-parse to themselves.
//!
//! Layout is source-driven, never width-driven: a braced container renders
//! inline iff its span lies on one source line, `.`-chain links the author
//! broke across lines keep their continuation lines (at base indent + 2),
//! and there is no wrapping — long lines stay long.

use ds_diag::Span;
use ds_parse::{
    AssignOp, BinaryOp, BlockExpr, Expr, Ident, Literal, LocalDecl, MatchArm, Param, Pattern, Stmt,
    UnaryOp,
};

use crate::{
    ctx::Ctx,
    ty::{write_generics, write_params, write_path, write_ty},
};

/// Two spaces per indent level.
pub(crate) fn push_indent(out: &mut String, level: usize) {
    for _ in 0..level {
        out.push_str("  ");
    }
}

/// Element separator state for one element run: every element writes straight
/// into the output, and the run's source extents drive the blank-line
/// preservation between consecutive elements.
pub(crate) struct ElemStream {
    /// Source end of the last element written, `None` before the first.
    prev_end: Option<u32>,
    /// Whether the first element starts on a fresh line (block bodies after
    /// `{`); file-level content joins without a leading newline.
    leading: bool,
}

impl ElemStream {
    pub(crate) fn new(leading: bool) -> Self {
        Self {
            prev_end: None,
            leading,
        }
    }

    /// Opens the element starting at `start`, writing the separator before it.
    pub(crate) fn begin(&mut self, ctx: &Ctx, out: &mut String, start: u32) {
        match self.prev_end {
            Some(p) => out.push_str(if ctx.blank_between(p, start) { "\n\n" } else { "\n" }),
            None if self.leading => out.push('\n'),
            None => {},
        }
    }

    /// Closes the element written since [`Self::begin`], which ends at `end`.
    pub(crate) fn end(&mut self, end: u32) {
        self.prev_end = Some(end);
    }

    /// Whether any element was written.
    pub(crate) fn is_empty(&self) -> bool {
        self.prev_end.is_none()
    }
}

/// One comment line at `indent`: its separator plus the verbatim comment text.
pub(crate) fn push_comment(
    ctx: &Ctx,
    out: &mut String,
    stream: &mut ElemStream,
    indent: usize,
    t: &ds_parse::Trivia,
) {
    stream.begin(ctx, out, t.span.start);
    push_indent(out, indent);
    ctx.push_verbatim(out, t.span);
    stream.end(t.span.end);
}

pub(crate) fn stmt_span_end(stmt: &Stmt) -> u32 {
    stmt.trailing_trivia
        .last()
        .map_or(stmt.span.end, |t| t.span.end.max(stmt.span.end))
}

pub(crate) fn stmt_span_start(stmt: &Stmt) -> u32 {
    stmt.leading_trivia
        .first()
        .map_or(stmt.span.start, |t| t.span.start.min(stmt.span.start))
}

/// One statement's full element rendering — leading comments as their own
/// lines, the statement at `indent`, same-line trailing comments inline,
/// later-line trailing comments back on their own lines — appended through
/// the caller's element stream.
pub(crate) fn write_stmt_elem(
    ctx: &Ctx,
    out: &mut String,
    stream: &mut ElemStream,
    indent: usize,
    stmt: &Stmt,
) {
    for t in &stmt.leading_trivia {
        push_comment(ctx, out, stream, indent, t);
    }
    stream.begin(ctx, out, stmt_span_start(stmt));
    push_indent(out, indent);
    write_stmt(ctx, out, indent, stmt);
    let end_line = ctx.line(stmt.span.end);
    for t in &stmt.trailing_trivia {
        if ctx.line(t.span.start) == end_line {
            out.push(' ');
            ctx.push_verbatim(out, t.span);
        }
    }
    stream.end(stmt_span_end(stmt));
    for t in &stmt.trailing_trivia {
        if ctx.line(t.span.start) != end_line {
            push_comment(ctx, out, stream, indent, t);
        }
    }
}

/// Writes a block body: statements (with their leading comments as separate
/// lines, trailing same-line comments appended inline) plus the comments
/// before the closing brace. A trailing comment that sits on a later line than
/// the statement's end (deep trivia the parser attached to the statement)
/// becomes its own line after the statement — content never disappears.
/// Answers whether the body wrote any element.
fn write_block_elems(
    ctx: &Ctx,
    out: &mut String,
    indent: usize,
    stmts: &[Stmt],
    trailing: &[ds_parse::Trivia],
) -> bool {
    let mut stream = ElemStream::new(true);
    for stmt in stmts {
        write_stmt_elem(ctx, out, &mut stream, indent, stmt);
    }
    for t in trailing {
        push_comment(ctx, out, &mut stream, indent, t);
    }
    !stream.is_empty()
}

// ===== blocks =====

/// Renders a block; inline iff its whole body lies on one source line. Every
/// braced container decides on its own span, so a one-line body inside a
/// multi-line construct (e.g. a chain-broken header) stays inline.
pub(crate) fn write_block(ctx: &Ctx, out: &mut String, base: usize, b: &BlockExpr) {
    if block_one_line(ctx, b) {
        write_block_inline(ctx, out, b);
    } else {
        write_block_multi(ctx, out, base, b);
    }
}

/// The inline probe: the block's span on one source line AND at most one
/// statement — statements are newline-separated, so a multi-statement body
/// is never inline regardless of the span. A zero-width block span marks the
/// synthesized script-entry block (no real node owns one); its empty span
/// would always read as one line, so the probe falls back to the statements'
/// own extent.
fn block_one_line(ctx: &Ctx, b: &BlockExpr) -> bool {
    if b.stmts.len() > 1 {
        return false;
    }
    if b.span.is_empty()
        && let Some(s) = b.stmts.first()
    {
        let span = Span::new(s.span.file, stmt_span_start(s), stmt_span_end(s));
        return ctx.one_line(span);
    }
    ctx.one_line(b.span)
}

fn write_block_inline(ctx: &Ctx, out: &mut String, b: &BlockExpr) {
    if b.stmts.is_empty() && b.trailing_trivia.is_empty() {
        out.push_str("{}");
        return;
    }
    out.push_str("{ ");
    for (i, stmt) in b.stmts.iter().enumerate() {
        if i > 0 {
            // Grammar: multiple statements need newlines; total anyway.
            out.push(' ');
        }
        write_stmt(ctx, out, 0, stmt);
    }
    for t in &b.trailing_trivia {
        out.push(' ');
        ctx.push_verbatim(out, t.span);
    }
    out.push_str(" }");
}

fn write_block_multi(ctx: &Ctx, out: &mut String, base: usize, b: &BlockExpr) {
    out.push('{');
    if write_block_elems(ctx, out, base + 1, &b.stmts, &b.trailing_trivia) {
        out.push('\n');
        push_indent(out, base);
    }
    out.push('}');
}

// ===== statements =====

pub(crate) fn write_stmt(ctx: &Ctx, out: &mut String, base: usize, stmt: &Stmt) {
    match &stmt.kind {
        ds_parse::StmtKind::Local(l) => write_local(ctx, out, base, l),
        ds_parse::StmtKind::Expr(e) => write_expr(ctx, out, base, e),
    }
}

/// `const x = e` / `var n: t = e` — shared by top-level items and statements.
pub(crate) fn write_local(ctx: &Ctx, out: &mut String, base: usize, l: &LocalDecl) {
    out.push_str(if l.mutable { "var " } else { "const " });
    write_pattern(ctx, out, &l.pat);
    if let Some(t) = &l.ty {
        out.push_str(": ");
        write_ty(ctx, out, t);
    }
    out.push_str(" = ");
    write_expr(ctx, out, base, &l.init);
}

// ===== precedence =====

fn prec(e: &Expr) -> u8 {
    match e {
        Expr::Assign { .. } => 1,
        Expr::Range { .. } => 3,
        Expr::Is { .. } => 4,
        Expr::Binary { op, .. } => match op {
            BinaryOp::Or => 5,
            BinaryOp::And => 6,
            BinaryOp::BitOr => 7,
            BinaryOp::BitXor => 8,
            BinaryOp::BitAnd => 9,
            BinaryOp::Eq
            | BinaryOp::Ne
            | BinaryOp::Lt
            | BinaryOp::Le
            | BinaryOp::Gt
            | BinaryOp::Ge
            | BinaryOp::In => 10,
            BinaryOp::Shl | BinaryOp::Shr => 11,
            BinaryOp::Add | BinaryOp::WrapAdd | BinaryOp::Sub | BinaryOp::WrapSub => 12,
            BinaryOp::Mul | BinaryOp::WrapMul | BinaryOp::Div | BinaryOp::Rem => 13,
        },
        Expr::Unary { .. } => 14,
        Expr::Call { .. } | Expr::Field { .. } | Expr::Index { .. } | Expr::Try { .. } => 15,
        _ => 16,
    }
}

pub(crate) fn write_expr(ctx: &Ctx, out: &mut String, base: usize, e: &Expr) {
    write_expr_min(ctx, out, base, e, 1);
}

pub(crate) fn write_expr_min(ctx: &Ctx, out: &mut String, base: usize, e: &Expr, min: u8) {
    if prec(e) < min {
        out.push('(');
        write_expr(ctx, out, base, e);
        out.push(')');
    } else {
        write_expr_inner(ctx, out, base, e);
    }
}

fn bin_op_str(op: BinaryOp) -> &'static str {
    match op {
        BinaryOp::Add => "+",
        BinaryOp::WrapAdd => "+%",
        BinaryOp::Sub => "-",
        BinaryOp::WrapSub => "-%",
        BinaryOp::Mul => "*",
        BinaryOp::WrapMul => "*%",
        BinaryOp::Div => "/",
        BinaryOp::Rem => "%",
        BinaryOp::Shl => "<<",
        BinaryOp::Shr => ">>",
        BinaryOp::Eq => "==",
        BinaryOp::Ne => "!=",
        BinaryOp::Lt => "<",
        BinaryOp::Le => "<=",
        BinaryOp::Gt => ">",
        BinaryOp::Ge => ">=",
        BinaryOp::In => "in",
        BinaryOp::And => "&&",
        BinaryOp::Or => "||",
        BinaryOp::BitAnd => "&",
        BinaryOp::BitOr => "|",
        BinaryOp::BitXor => "^",
    }
}

fn assign_op_str(op: AssignOp) -> &'static str {
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

/// Unary operands that start with `-` need a space after a `-`/`-%` operator
/// so the two tokens do not merge.
fn starts_minus(e: &Expr) -> bool {
    match e {
        Expr::Unary { op, .. } => matches!(op, UnaryOp::Neg | UnaryOp::WrapNeg),
        Expr::Literal {
            lit: Literal::Int { text, .. } | Literal::Float { text, .. },
            ..
        } => text.starts_with('-'),
        _ => false,
    }
}

/// Renders one side of a binary at chain level `k`, inserting parentheses
/// exactly when a flat rendering would re-associate. Legal flat operands come
/// from the grammar's chain: same-level left folds and every tighter level
/// (`p < 4` marks the looser `is`/range shapes that only parens produce).
fn write_bin_child(ctx: &Ctx, out: &mut String, base: usize, e: &Expr, k: u8, left: bool) {
    let p = prec(e);
    let parens = if left { p < 4 || (p >= 5 && p < k) } else { p <= 3 || (p >= 5 && p <= k) };
    if parens {
        out.push('(');
        write_expr(ctx, out, base, e);
        out.push(')');
    } else {
        write_expr(ctx, out, base, e);
    }
}

fn write_expr_inner(ctx: &Ctx, out: &mut String, base: usize, e: &Expr) {
    match e {
        Expr::Literal { lit, .. } => write_literal(out, lit),
        // String literals re-render verbatim from their source spans; the
        // formatter never re-escapes or re-flows them. A conversion
        // placeholder only exists beside conversion errors and keeps its
        // rejected bytes as written.
        Expr::Str { span, .. } | Expr::RawStr { span, .. } | Expr::Invalid(span) => {
            ctx.push_verbatim(out, *span);
        },
        Expr::Lambda {
            generics,
            params,
            ret,
            arrow,
            body,
            ..
        } => write_lambda(ctx, out, base, generics, params, ret.as_ref(), *arrow, body),
        Expr::Path { path, .. } => write_path(out, path),
        Expr::Struct { path, fields, .. } => {
            write_path(out, path);
            write_struct_fields(ctx, out, fields);
        },
        Expr::List { items, .. } => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                write_expr(ctx, out, base, item);
            }
            out.push(']');
        },
        Expr::Tuple {
            items,
            trailing_comma,
            ..
        } => {
            out.push('(');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                write_expr(ctx, out, base, item);
            }
            // Trailing commas are not emitted — except the one-element case,
            // where the comma is the only thing keeping it a tuple.
            if items.len() == 1 && *trailing_comma {
                out.push(',');
            }
            out.push(')');
        },
        Expr::Paren { inner, .. } => {
            out.push('(');
            write_expr(ctx, out, base, inner);
            out.push(')');
        },
        Expr::Call {
            callee,
            args,
            trailing,
            ..
        } => {
            write_expr_min(ctx, out, base, callee, 15);
            // Trailing-block sugar [F7]: a call whose written argument list
            // is empty renders no parenthesis pair — the canonical
            // spelling of the no-parens source form `f { ... }`.
            let sugar = args.is_empty()
                && trailing
                    .as_deref()
                    .is_some_and(|t| trailing_block(t).is_some());
            if !sugar {
                out.push('(');
                for (i, a) in args.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    if let Some(n) = &a.name {
                        out.push_str(&n.name);
                        out.push_str(": ");
                    }
                    write_expr(ctx, out, base, &a.value);
                }
                out.push(')');
            }
            if let Some(t) = trailing {
                out.push(' ');
                // The trailing block argument is a zero-param block lambda;
                // its braces are the call's, so render the block directly.
                if let Some(b) = trailing_block(t) {
                    write_block(ctx, out, base, b);
                } else {
                    write_expr(ctx, out, base, t);
                }
            }
        },
        Expr::Field { recv, name, .. } => {
            write_expr_min(ctx, out, base, recv, 15);
            if ctx.newline_between(recv.span().end, name.span.start) {
                out.push('\n');
                push_indent(out, base + 1);
            }
            out.push('.');
            out.push_str(&name.name);
        },
        Expr::Index { recv, index, .. } => {
            write_expr_min(ctx, out, base, recv, 15);
            out.push('[');
            write_expr(ctx, out, base, index);
            out.push(']');
        },
        Expr::MapApply {
            head, key, value, ..
        } => {
            out.push_str(&head.name);
            out.push('[');
            write_expr(ctx, out, base, key);
            out.push_str(", ");
            write_expr(ctx, out, base, value);
            out.push(']');
        },
        // A fn type in the constructor brackets renders through the type
        // writer, the annotation spelling.
        Expr::FnType { ty, .. } => write_ty(ctx, out, ty),
        Expr::Try { expr, .. } => {
            write_expr_min(ctx, out, base, expr, 15);
            out.push('?');
        },
        Expr::Unary { op, expr, .. } => {
            out.push_str(match op {
                UnaryOp::Neg => "-",
                UnaryOp::WrapNeg => "-%",
                UnaryOp::Not => "!",
            });
            let needs_space = matches!(op, UnaryOp::Neg | UnaryOp::WrapNeg) && starts_minus(expr);
            if needs_space {
                out.push(' ');
            }
            write_expr_min(ctx, out, base, expr, 14);
        },
        Expr::Is { expr, ty, .. } => {
            write_expr_min(ctx, out, base, expr, 4);
            out.push_str(" is ");
            write_ty(ctx, out, ty);
        },
        Expr::Binary {
            op, left, right, ..
        } => {
            let k = prec(e);
            write_bin_child(ctx, out, base, left, k, true);
            out.push(' ');
            out.push_str(bin_op_str(*op));
            out.push(' ');
            if *op == BinaryOp::In {
                // `in`'s right side is the in_rhs shape: a shift-level
                // operand, or one bare range (`x in a..b`).
                let p = prec(right);
                if p != 3 && p < 11 {
                    out.push('(');
                    write_expr(ctx, out, base, right);
                    out.push(')');
                } else {
                    write_expr(ctx, out, base, right);
                }
            } else {
                write_bin_child(ctx, out, base, right, k, false);
            }
        },
        Expr::Range {
            inclusive,
            start,
            end,
            ..
        } => {
            write_expr_min(ctx, out, base, start, 5);
            out.push_str(if *inclusive { "..=" } else { ".." });
            write_expr_min(ctx, out, base, end, 5);
        },
        Expr::Assign {
            op, left, right, ..
        } => {
            write_expr_min(ctx, out, base, left, 3);
            out.push(' ');
            out.push_str(assign_op_str(*op));
            out.push(' ');
            write_expr_min(ctx, out, base, right, 1);
        },
        Expr::If { .. } => write_if(ctx, out, base, e),
        Expr::Match { .. } => write_match(ctx, out, base, e),
        Expr::While { cond, body, .. } => {
            out.push_str("while ");
            write_expr(ctx, out, base, cond);
            out.push(' ');
            write_block(ctx, out, base, body);
        },
        Expr::For {
            pat, iter, body, ..
        } => {
            // A block body renders in the plain form; any other body keeps
            // the parenthesized form (its re-parse shape).
            if matches!(&**body, Expr::Block(_)) {
                out.push_str("for ");
                write_pattern(ctx, out, pat);
                out.push_str(" in ");
                write_expr(ctx, out, base, iter);
                out.push(' ');
                write_expr(ctx, out, base, body);
            } else {
                out.push_str("for (");
                write_pattern(ctx, out, pat);
                out.push_str(" in ");
                write_expr(ctx, out, base, iter);
                out.push_str(") ");
                write_expr(ctx, out, base, body);
            }
        },
        Expr::Return { expr, .. } => {
            out.push_str("return");
            if let Some(v) = expr {
                out.push(' ');
                write_expr(ctx, out, base, v);
            }
        },
        Expr::Break(_) => out.push_str("break"),
        Expr::Continue(_) => out.push_str("continue"),
        Expr::Nursery { body, .. } | Expr::Spawn { body, .. } | Expr::Defer { body, .. } => {
            let kw = match e {
                Expr::Nursery { .. } => "nursery",
                Expr::Spawn { .. } => "spawn",
                _ => "defer",
            };
            out.push_str(kw);
            out.push(' ');
            write_block(ctx, out, base, body);
        },
        Expr::Block(b) => write_block(ctx, out, base, b),
    }
}

/// The zero-param block-lambda shape the parser builds for a call's trailing
/// block argument.
fn trailing_block(e: &Expr) -> Option<&BlockExpr> {
    let Expr::Lambda {
        params,
        ret: None,
        arrow: false,
        body,
        ..
    } = e
    else {
        return None;
    };
    if params.is_empty() {
        match &**body {
            Expr::Block(b) => Some(b),
            _ => None,
        }
    } else {
        None
    }
}

fn write_lambda(
    ctx: &Ctx,
    out: &mut String,
    base: usize,
    generics: &[ds_parse::TypeParam],
    params: &[Param],
    ret: Option<&ds_parse::Type>,
    arrow: bool,
    body: &Expr,
) {
    // Canonical form: always parenthesized parameters (the bare-ident lambda
    // the parser accepts is a spelling, not a shape).
    write_generics(ctx, out, generics);
    if !generics.is_empty() {
        out.push(' ');
    }
    write_params(ctx, out, params);
    if arrow {
        out.push_str(" => ");
        write_expr(ctx, out, base, body);
        return;
    }
    if let Some(r) = ret {
        out.push_str(" -> ");
        write_ty(ctx, out, r);
        out.push(' ');
    }
    match body {
        Expr::Block(b) => write_block(ctx, out, base, b),
        other => write_expr(ctx, out, base, other),
    }
}

/// `path { field, field: expr }` — struct literals always render inline.
fn write_struct_fields(ctx: &Ctx, out: &mut String, fields: &[(Ident, Option<Expr>)]) {
    if fields.is_empty() {
        out.push_str(" {}");
        return;
    }
    out.push_str(" { ");
    for (i, (name, val)) in fields.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        out.push_str(&name.name);
        if let Some(v) = val {
            out.push_str(": ");
            write_expr(ctx, out, 0, v);
        }
    }
    out.push_str(" }");
}

fn write_if(ctx: &Ctx, out: &mut String, base: usize, e: &Expr) {
    let Expr::If {
        cond, then, else_, ..
    } = e
    else {
        return;
    };
    out.push_str("if ");
    write_expr(ctx, out, base, cond);
    out.push(' ');
    write_block(ctx, out, base, then);
    if let Some(el) = else_ {
        out.push_str(" else ");
        match &**el {
            // `else if` chains: the nested `if` keeps its own keyword.
            Expr::If { .. } => write_if(ctx, out, base, el),
            Expr::Block(b) => write_block(ctx, out, base, b),
            other => write_expr(ctx, out, base, other),
        }
    }
}

fn write_match(ctx: &Ctx, out: &mut String, base: usize, e: &Expr) {
    let Expr::Match {
        span,
        scrutinee,
        arms,
    } = e
    else {
        return;
    };
    if arms.is_empty() {
        out.push_str("match ");
        write_expr(ctx, out, base, scrutinee);
        out.push_str(" {}");
        return;
    }
    out.push_str("match ");
    write_expr(ctx, out, base, scrutinee);
    if ctx.one_line(*span) {
        out.push_str(" { ");
        for (i, arm) in arms.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            write_arm(ctx, out, base, arm);
        }
        out.push_str(" }");
        return;
    }
    out.push_str(" {");
    let arm_indent = base + 1;
    // Newline separation is canonical for arms of any body shape [6.1];
    // no commas in the multiline form.
    for arm in arms {
        out.push('\n');
        push_indent(out, arm_indent);
        write_arm(ctx, out, arm_indent, arm);
    }
    out.push('\n');
    push_indent(out, base);
    out.push('}');
}

fn write_arm(ctx: &Ctx, out: &mut String, base: usize, arm: &MatchArm) {
    write_pattern(ctx, out, &arm.pat);
    if let Some(ann) = &arm.ann {
        out.push_str(": ");
        write_ty(ctx, out, ann);
    }
    if let Some(guard) = &arm.guard {
        out.push_str(" if ");
        write_expr(ctx, out, base, guard);
    }
    out.push_str(" => ");
    match &arm.body {
        Expr::Block(b) => write_block(ctx, out, base, b),
        other => write_expr(ctx, out, base, other),
    }
}

// ===== patterns =====

pub(crate) fn write_pattern(ctx: &Ctx, out: &mut String, p: &Pattern) {
    match p {
        Pattern::Wildcard(_) => out.push('_'),
        Pattern::Binding { name, .. } => out.push_str(&name.name),
        Pattern::Literal { lit, .. } => write_literal(out, lit),
        Pattern::Str { span, .. } | Pattern::RawStr { span, .. } => {
            ctx.push_verbatim(out, *span);
        },
        Pattern::Path { path, .. } => write_path(out, path),
        Pattern::Call { path, args, .. } => {
            write_path(out, path);
            out.push('(');
            for (i, a) in args.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                write_pattern(ctx, out, a);
            }
            out.push(')');
        },
        Pattern::Struct { path, fields, .. } => {
            write_path(out, path);
            if fields.is_empty() {
                out.push_str(" {}");
            } else {
                out.push_str(" { ");
                for (i, (name, pat)) in fields.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    out.push_str(&name.name);
                    if let Some(p) = pat {
                        out.push_str(": ");
                        write_pattern(ctx, out, p);
                    }
                }
                out.push_str(" }");
            }
        },
        Pattern::Tuple {
            pats,
            trailing_comma,
            ..
        } => {
            out.push('(');
            for (i, p) in pats.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                write_pattern(ctx, out, p);
            }
            // As with tuple expressions: no trailing commas except the
            // one-element disambiguation.
            if pats.len() == 1 && *trailing_comma {
                out.push(',');
            }
            out.push(')');
        },
        Pattern::Or { pats, .. } => {
            for (i, p) in pats.iter().enumerate() {
                if i > 0 {
                    out.push_str(" | ");
                }
                write_pattern(ctx, out, p);
            }
        },
    }
}

// ===== literals =====

pub(crate) fn write_literal(out: &mut String, lit: &Literal) {
    match lit {
        Literal::Int { text, suffix } | Literal::Float { text, suffix } => {
            out.push_str(text);
            out.push_str(suffix);
        },
        Literal::Char(c) => {
            out.push('\'');
            push_escaped_char(out, *c);
            out.push('\'');
        },
        Literal::Byte(b) => {
            out.push_str("b'");
            push_escaped_byte(out, *b);
            out.push('\'');
        },
        Literal::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
    }
}

fn push_escaped_char(out: &mut String, c: char) {
    match c {
        '\n' => out.push_str("\\n"),
        '\t' => out.push_str("\\t"),
        '\r' => out.push_str("\\r"),
        '\0' => out.push_str("\\0"),
        '\\' => out.push_str("\\\\"),
        '\'' => out.push_str("\\'"),
        c if (c as u32) < 0x20 || c == '\u{7f}' => {
            use std::fmt::Write as _;
            let _ = write!(out, "\\u{{{:x}}}", c as u32);
        },
        c => out.push(c),
    }
}

fn push_escaped_byte(out: &mut String, b: u8) {
    match b {
        b'\n' => out.push_str("\\n"),
        b'\t' => out.push_str("\\t"),
        b'\r' => out.push_str("\\r"),
        0 => out.push_str("\\0"),
        b'\\' => out.push_str("\\\\"),
        b'\'' => out.push_str("\\'"),
        0x20..=0x7e => out.push(b as char),
        b => {
            use std::fmt::Write as _;
            let _ = write!(out, "\\u{{{b:x}}}");
        },
    }
}
