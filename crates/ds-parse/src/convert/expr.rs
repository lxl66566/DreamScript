//! Expression, statement, and block conversion.

use ds_diag::{ErrorCode, Span};
use pest::iterators::Pair;

use super::{Ctx, empty_block, inner, inner_or_invalid, is_op_rule, is_operand_rule, join};
use crate::{Rule, ast::*};

impl Ctx {
    // ===== expressions =====

    pub(super) fn expr(&mut self, p: Pair<'_, Rule>) -> Expr {
        match p.as_rule() {
            Rule::assign_lvl | Rule::nos_assign => self.assign(p),
            Rule::range_lvl | Rule::nos_range | Rule::in_rhs | Rule::nos_in_rhs => self.range(p),
            Rule::isin_lvl | Rule::nos_isin => self.isin(p),
            Rule::logor_lvl | Rule::nos_logor => self.fold_binary(p, BinaryOp::Or),
            Rule::logand_lvl | Rule::nos_logand => self.fold_binary(p, BinaryOp::And),
            Rule::bitor_lvl | Rule::nos_bitor => self.fold_binary(p, BinaryOp::BitOr),
            Rule::bitxor_lvl | Rule::nos_bitxor => self.fold_binary(p, BinaryOp::BitXor),
            Rule::bitand_lvl | Rule::nos_bitand => self.fold_binary(p, BinaryOp::BitAnd),
            Rule::cmp_lvl | Rule::nos_cmp => self.cmp(p),
            Rule::shift_lvl | Rule::nos_shift => self.level_with_op(p, |op| {
                if op.starts_with("<<") { BinaryOp::Shl } else { BinaryOp::Shr }
            }),
            Rule::add_lvl | Rule::nos_add => self.level_with_op(p, |op| match op {
                "+%" => BinaryOp::WrapAdd,
                "-" => BinaryOp::Sub,
                "-%" => BinaryOp::WrapSub,
                _ => BinaryOp::Add,
            }),
            Rule::mul_lvl | Rule::nos_mul => self.level_with_op(p, |op| match op {
                "*%" => BinaryOp::WrapMul,
                "/" => BinaryOp::Div,
                "%" => BinaryOp::Rem,
                _ => BinaryOp::Mul,
            }),
            Rule::unary_lvl | Rule::nos_unary => {
                let span = self.span(&p);
                let children: Vec<Pair<'_, Rule>> = inner(&p).collect();
                let ops_count = children
                    .iter()
                    .filter(|c| c.as_rule() == Rule::neg_op)
                    .count();
                if self.chain_guard(ops_count, span) {
                    return Expr::Invalid(span);
                }
                let mut ops = Vec::new();
                let mut operand = Expr::Invalid(span);
                for c in children {
                    match c.as_rule() {
                        Rule::neg_op => ops.push((
                            match c.as_str() {
                                "-%" => UnaryOp::WrapNeg,
                                "!" => UnaryOp::Not,
                                _ => UnaryOp::Neg,
                            },
                            self.span(&c),
                        )),
                        _ => operand = self.expr(c),
                    }
                }
                // Fold innermost-first so every node spans exactly its own
                // operator token through the operand's end (`!`x`` inside
                // ``!!x``), not the whole level.
                ops.into_iter().rev().fold(operand, |acc, (op, ospan)| {
                    let span = join(ospan, acc.span());
                    Expr::Unary {
                        span,
                        op,
                        expr: Box::new(acc),
                    }
                })
            },
            Rule::postfix_lvl | Rule::nos_postfix => self.postfix(p),
            Rule::struct_lit => self.struct_lit(p),
            Rule::lambda => inner(&p)
                .next()
                .map_or(Expr::Invalid(self.span(&p)), |c| self.lambda_form(c)),
            Rule::lambda_paren | Rule::lambda_bare => self.lambda_form(p),
            Rule::list_lit => {
                let span = self.span(&p);
                let items = inner(&p)
                    .filter(|c| c.as_rule() == Rule::expr)
                    .map(|c| self.expr(c))
                    .collect();
                Expr::List { span, items }
            },
            Rule::paren_tuple => self.tuple(p),
            Rule::if_expr => self.if_expr(p),
            Rule::match_expr => self.match_expr(p),
            Rule::while_expr => self.while_expr(p),
            Rule::for_expr => self.for_expr(p),
            Rule::return_expr => {
                let span = self.span(&p);
                let value = inner(&p).nth(1).map(|c| Box::new(self.expr(c)));
                Expr::Return { span, expr: value }
            },
            Rule::break_expr => Expr::Break(self.span(&p)),
            Rule::continue_expr => Expr::Continue(self.span(&p)),
            Rule::nursery_expr => self.kw_block(p, |span, b| Expr::Nursery { span, body: b }),
            Rule::spawn_expr => self.kw_block(p, |span, b| Expr::Spawn { span, body: b }),
            Rule::defer_expr => self.kw_block(p, |span, b| Expr::Defer { span, body: b }),
            Rule::float_lit => {
                let span = self.span(&p);
                let lit = self.float_lit(p, false);
                Expr::Literal { span, lit }
            },
            Rule::int_lit => {
                let span = self.span(&p);
                let lit = self.int_lit(p, false);
                Expr::Literal { span, lit }
            },
            Rule::string_lit => self.string_lit(p),
            Rule::raw_hashed | Rule::raw_zero => self.raw_str(p),
            Rule::char_lit => {
                let span = self.span(&p);
                let lit = self.char_lit(p);
                Expr::Literal { span, lit }
            },
            Rule::byte_lit => {
                let span = self.span(&p);
                let lit = self.byte_lit(p);
                Expr::Literal { span, lit }
            },
            Rule::kw_true => Expr::Literal {
                span: self.span(&p),
                lit: Literal::Bool(true),
            },
            Rule::kw_false => Expr::Literal {
                span: self.span(&p),
                lit: Literal::Bool(false),
            },
            Rule::block_expr => Expr::Block(self.block(p)),
            Rule::map_apply => {
                let span = self.span(&p);
                let mut children = inner(&p);
                let head = children.next().filter(|c| c.as_rule() == Rule::map_head);
                let mut args =
                    children.filter(|c| matches!(c.as_rule(), Rule::expr | Rule::fn_type));
                match (head, args.next(), args.next()) {
                    (Some(head), Some(key), Some(value)) => Expr::MapApply {
                        span,
                        head: self.ident(head),
                        key: Box::new(self.bracket_arg(key)),
                        value: Box::new(self.bracket_arg(value)),
                    },
                    _ => Expr::Invalid(span),
                }
            },
            Rule::path_atom | Rule::path => {
                let span = self.span(&p);
                let path = self.conv_path(p);
                Expr::Path { span, path }
            },
            _ => inner_or_invalid(self, &p),
        }
    }

    fn assign(&mut self, p: Pair<'_, Rule>) -> Expr {
        let span = self.span(&p);
        let children: Vec<Pair<'_, Rule>> = inner(&p).collect();
        // Children alternate operand / op, so operands are half the run.
        if self.chain_guard(children.len().div_ceil(2), span) {
            return Expr::Invalid(span);
        }
        let mut operands = Vec::new();
        let mut ops = Vec::new();
        let mut expect_operand = true;
        for c in children {
            if expect_operand {
                operands.push(self.expr(c));
            } else {
                ops.push(match c.as_str() {
                    "+=" => AssignOp::Add,
                    "-=" => AssignOp::Sub,
                    "*=" => AssignOp::Mul,
                    "/=" => AssignOp::Div,
                    "%=" => AssignOp::Rem,
                    "<<=" => AssignOp::Shl,
                    ">>=" => AssignOp::Shr,
                    _ => AssignOp::Plain,
                });
            }
            expect_operand = !expect_operand;
        }
        // Right fold: `a = b = c` is `a = (b = c)`.
        let mut acc = operands.pop().unwrap_or(Expr::Invalid(span));
        while let (Some(op), Some(lhs)) = (ops.pop(), operands.pop()) {
            let s = join(lhs.span(), acc.span());
            acc = Expr::Assign {
                span: s,
                op,
                left: Box::new(lhs),
                right: Box::new(acc),
            };
        }
        acc
    }

    fn range(&mut self, p: Pair<'_, Rule>) -> Expr {
        let span = self.span(&p);
        let mut children = inner(&p);
        let start = children
            .next()
            .map_or(Expr::Invalid(span), |c| self.expr(c));
        match children.next() {
            Some(op) if op.as_rule() == Rule::range_op => {
                let inclusive = op.as_str() == "..=";
                let end = children
                    .next()
                    .map_or(Expr::Invalid(span), |c| self.expr(c));
                Expr::Range {
                    span,
                    inclusive,
                    start: Box::new(start),
                    end: Box::new(end),
                }
            },
            _ => start,
        }
    }

    fn isin(&mut self, p: Pair<'_, Rule>) -> Expr {
        let span = self.span(&p);
        let children: Vec<Pair<'_, Rule>> = inner(&p).collect();
        let tails = children
            .iter()
            .filter(|c| c.as_rule() == Rule::kw_is)
            .count();
        if self.chain_guard(tails + 1, span) {
            return Expr::Invalid(span);
        }
        let mut iter = children.into_iter();
        let first = match iter.next() {
            Some(c) if is_operand_rule(&c) => self.expr(c),
            _ => return Expr::Invalid(span),
        };
        let mut acc = first;
        while let Some(c) = iter.next() {
            if c.as_rule() == Rule::kw_is
                && let Some(t) = iter.next()
            {
                let ty = Box::new(self.type_expr(t));
                let s = join(acc.span(), ty.span());
                acc = Expr::Is {
                    span: s,
                    expr: Box::new(acc),
                    ty,
                };
            }
        }
        acc
    }

    fn fold_binary(&mut self, p: Pair<'_, Rule>, op: BinaryOp) -> Expr {
        let span = self.span(&p);
        let children: Vec<Pair<'_, Rule>> = inner(&p).collect();
        if self.chain_guard(children.len(), span) {
            return Expr::Invalid(span);
        }
        let mut iter = children.into_iter();
        let mut acc = iter.next().map_or(Expr::Invalid(span), |c| self.expr(c));
        for c in iter {
            let e = self.expr(c);
            let s = join(acc.span(), e.span());
            acc = Expr::Binary {
                span: s,
                op,
                left: Box::new(acc),
                right: Box::new(e),
            };
        }
        acc
    }

    /// Comparison level: `cmp_op` operands, plus `in` tails whose right side
    /// is one optional bare range.
    fn cmp(&mut self, p: Pair<'_, Rule>) -> Expr {
        let span = self.span(&p);
        let children: Vec<Pair<'_, Rule>> = inner(&p).collect();
        let operands = children
            .iter()
            .filter(|c| !matches!(c.as_rule(), Rule::cmp_op | Rule::kw_in))
            .count();
        if self.chain_guard(operands, span) {
            return Expr::Invalid(span);
        }
        let mut iter = children.into_iter();
        let mut acc = iter.next().map_or(Expr::Invalid(span), |c| self.expr(c));
        while let Some(c) = iter.next() {
            match c.as_rule() {
                Rule::cmp_op => {
                    let op = match c.as_str() {
                        "==" => BinaryOp::Eq,
                        "!=" => BinaryOp::Ne,
                        "<=" => BinaryOp::Le,
                        ">=" => BinaryOp::Ge,
                        "<" => BinaryOp::Lt,
                        _ => BinaryOp::Gt,
                    };
                    if let Some(rhs) = iter.next() {
                        let e = self.expr(rhs);
                        let s = join(acc.span(), e.span());
                        acc = Expr::Binary {
                            span: s,
                            op,
                            left: Box::new(acc),
                            right: Box::new(e),
                        };
                    }
                },
                Rule::kw_in => {
                    if let Some(rhs) = iter.next() {
                        let e = self.expr(rhs);
                        let s = join(acc.span(), e.span());
                        acc = Expr::Binary {
                            span: s,
                            op: BinaryOp::In,
                            left: Box::new(acc),
                            right: Box::new(e),
                        };
                    }
                },
                _ => {},
            }
        }
        acc
    }

    /// Folds a level whose children alternate operand / op-pair / operand.
    fn level_with_op<F>(&mut self, p: Pair<'_, Rule>, map: F) -> Expr
    where
        F: Fn(&str) -> BinaryOp,
    {
        let span = self.span(&p);
        let children: Vec<Pair<'_, Rule>> = inner(&p).collect();
        let operands = children.iter().filter(|c| !is_op_rule(c)).count();
        if self.chain_guard(operands, span) {
            return Expr::Invalid(span);
        }
        let mut acc = Expr::Invalid(span);
        let mut pending_op: Option<BinaryOp> = None;
        let mut first = true;
        for c in children {
            if is_op_rule(&c) {
                pending_op = Some(map(c.as_str()));
                continue;
            }
            let e = self.expr(c);
            if first {
                acc = e;
                first = false;
            } else {
                let op = pending_op.take().unwrap_or_else(|| {
                    // The grammar alternates operand/op strictly, so a
                    // second operand without an operator is unreachable.
                    debug_assert!(false, "binary chain operand without an operator");
                    BinaryOp::Eq
                });
                let s = join(acc.span(), e.span());
                acc = Expr::Binary {
                    span: s,
                    op,
                    left: Box::new(acc),
                    right: Box::new(e),
                };
            }
        }
        acc
    }

    fn postfix(&mut self, p: Pair<'_, Rule>) -> Expr {
        let span = self.span(&p);
        let children: Vec<Pair<'_, Rule>> = inner(&p).collect();
        // Each postfix op nests one level (calls, indexes, `?`, fields), so
        // the op run is itself a chain.
        if self.chain_guard(children.len(), span) {
            return Expr::Invalid(span);
        }
        let mut iter = children.into_iter();
        let first = match iter.next() {
            Some(c) => self.expr(c),
            None => return Expr::Invalid(span),
        };
        let mut acc = first;
        for c in iter {
            acc = self.apply_postfix(acc, c);
        }
        acc
    }

    /// Applies one postfix op to `acc`; the op pair's children name the shape
    /// it carries.
    fn apply_postfix(&mut self, acc: Expr, p: Pair<'_, Rule>) -> Expr {
        let start = acc.span().start;
        let end = self.span(&p).end;
        let span = Span::new(self.file, start, end);
        let mut children = inner(&p).collect::<Vec<Pair<'_, Rule>>>().into_iter();
        let Some(shape) = children.next() else {
            // The postfix `?` carries no children of its own.
            return Expr::Try {
                span,
                expr: Box::new(acc),
            };
        };
        let mut rest = children;
        match shape.as_rule() {
            Rule::args => {
                let mut selection = None;
                if let Some(c) = rest.next() {
                    // The paren call's optional instance selection.
                    selection =
                        Some(self.ident(inner(&c).next().expect("call_select holds the name")));
                }
                Expr::Call {
                    span,
                    callee: Box::new(acc),
                    args: self.args(&shape),
                    trailing: None,
                    selection,
                }
            },
            Rule::block_expr => {
                let bspan = self.span(&shape);
                let lambda = Expr::Lambda {
                    span: bspan,
                    generics: Vec::new(),
                    params: Vec::new(),
                    ret: None,
                    arrow: false,
                    body: Box::new(Expr::Block(self.block(shape))),
                };
                match acc {
                    // The block feeds the directly preceding call's final
                    // parameter as its zero-argument lambda, so it
                    // attaches to that call instead of wrapping a second,
                    // computed-callee call around it.
                    Expr::Call {
                        callee,
                        args,
                        trailing: None,
                        selection,
                        ..
                    } => Expr::Call {
                        span,
                        callee,
                        args,
                        trailing: Some(Box::new(lambda)),
                        selection,
                    },
                    recv => Expr::Call {
                        span,
                        callee: Box::new(recv),
                        args: Vec::new(),
                        trailing: Some(Box::new(lambda)),
                        selection: None,
                    },
                }
            },
            Rule::postfix_index => match inner(&shape).next() {
                Some(arg) => Expr::Index {
                    span,
                    recv: Box::new(acc),
                    index: Box::new(self.bracket_arg(arg)),
                },
                None => acc,
            },
            Rule::keyed_field => {
                match inner(&shape).next() {
                    // The bracketed constant name expression converts as
                    // an ordinary expression; the checker admits only the
                    // comptime loop record's name member.
                    Some(index) => match inner(&index).next() {
                        Some(arg) => Expr::FieldKey {
                            span,
                            recv: Box::new(acc),
                            name: Box::new(self.bracket_arg(arg)),
                        },
                        None => acc,
                    },
                    None => acc,
                }
            },
            Rule::field_name => match inner(&shape).next().map(|i| self.ident(i)) {
                Some(name) => Expr::Field {
                    span,
                    recv: Box::new(acc),
                    name,
                },
                None => acc,
            },
            // Every postfix shape carries its own rule; the childless `?`
            // answered above, so an unmatched child is unreachable.
            _ => acc,
        }
    }

    /// One constructor bracket's content: a fn type converts onto its
    /// expression node, anything else is the expression walk.
    fn bracket_arg(&mut self, p: Pair<'_, Rule>) -> Expr {
        if p.as_rule() == Rule::fn_type {
            let span = self.span(&p);
            let ty = self.type_expr(p);
            Expr::FnType {
                span,
                ty: Box::new(ty),
            }
        } else {
            self.expr(p)
        }
    }

    /// Converts call arguments: named arguments (`f(x: 1)`) must follow
    /// positional ones, and a name may appear once. Both violations are
    /// diagnostics, not grammar failures. The discipline runs as a pass
    /// over the collected arguments, borrowing the idents already stored
    /// there instead of cloning the names.
    fn args(&mut self, p: &Pair<'_, Rule>) -> Vec<CallArg> {
        let mut out = Vec::new();
        for a in inner(p).filter(|c| c.as_rule() == Rule::arg) {
            let aspan = self.span(&a);
            let (name, value) = match inner(&a).next() {
                Some(head) if head.as_rule() == Rule::arg_named => {
                    let mut n = None;
                    let mut v = Expr::Invalid(aspan);
                    for c in inner(&head) {
                        match c.as_rule() {
                            Rule::ident => n = Some(self.ident(c)),
                            _ => v = self.expr(c),
                        }
                    }
                    (n, v)
                },
                Some(e) => (None, self.expr(e)),
                None => (None, Expr::Invalid(aspan)),
            };
            out.push(CallArg {
                span: aspan,
                name,
                value,
            });
        }
        let mut names: Vec<&str> = Vec::new();
        let mut named_seen = false;
        for a in &out {
            match &a.name {
                Some(n) => {
                    if names.contains(&n.name.as_str()) {
                        self.err(
                            n.span,
                            ErrorCode::DuplicateArgument,
                            format!("argument `{}` is passed twice by name", n.name),
                        );
                    } else {
                        names.push(n.name.as_str());
                    }
                    named_seen = true;
                },
                None if named_seen => self.err(
                    a.span,
                    ErrorCode::UnexpectedToken,
                    "positional argument follows a named argument; named arguments come last",
                ),
                None => {},
            }
        }
        out
    }

    fn struct_lit(&mut self, p: Pair<'_, Rule>) -> Expr {
        let span = self.span(&p);
        let mut path = None;
        let mut fields = Vec::new();
        for c in inner(&p) {
            match c.as_rule() {
                Rule::path => path = Some(self.conv_path(c)),
                Rule::field_init => {
                    let mut name = None;
                    let mut value = None;
                    for fc in inner(&c) {
                        match fc.as_rule() {
                            Rule::ident => name = Some(self.ident(fc)),
                            _ => value = Some(self.expr(fc)),
                        }
                    }
                    if let Some(name) = name {
                        fields.push((name, value));
                    }
                },
                _ => {},
            }
        }
        Expr::Struct {
            span,
            path: path.unwrap_or(Path {
                span,
                segments: Vec::new(),
            }),
            fields,
        }
    }

    fn tuple(&mut self, p: Pair<'_, Rule>) -> Expr {
        let span = self.span(&p);
        let mut items = Vec::new();
        let mut trailing = false;
        for c in inner(&p) {
            match c.as_rule() {
                Rule::pt_item => {
                    if let Some(e) = inner(&c).next() {
                        items.push(self.expr(e));
                    }
                },
                Rule::pt_trail => trailing = true,
                _ => {},
            }
        }
        if items.len() == 1 && !trailing {
            let inner_e = items.pop().unwrap_or(Expr::Invalid(span));
            return Expr::Paren {
                span,
                inner: Box::new(inner_e),
            };
        }
        Expr::Tuple {
            span,
            items,
            trailing_comma: trailing,
        }
    }

    fn lambda_form(&mut self, p: Pair<'_, Rule>) -> Expr {
        let span = self.span(&p);
        if p.as_rule() == Rule::lambda_bare {
            let mut name = None;
            let mut body = Expr::Invalid(span);
            for c in inner(&p) {
                match c.as_rule() {
                    Rule::ident => name = Some(self.ident(c)),
                    Rule::lambda_rhs => body = self.lambda_rhs(c),
                    _ => {},
                }
            }
            let param_span = name.as_ref().map_or(span, |n| n.span);
            let param = Param {
                span: param_span,
                mode: ParamMode::Borrow,
                name: name.unwrap_or_else(|| self.bad_ident(span)),
                ty: None,
                default: None,
            };
            Expr::Lambda {
                span,
                generics: Vec::new(),
                params: vec![param],
                ret: None,
                arrow: true,
                body: Box::new(body),
            }
        } else {
            let mut generics = Vec::new();
            let mut params = Vec::new();
            let mut ret = None;
            let mut body = Expr::Invalid(span);
            for c in inner(&p) {
                match c.as_rule() {
                    Rule::generics => generics = self.generics(c),
                    Rule::params => params = self.params(c),
                    Rule::type_expr => ret = Some(self.type_expr(c)),
                    Rule::lambda_rhs => body = self.lambda_rhs(c),
                    Rule::block_expr => body = Expr::Block(self.block(c)),
                    _ => {},
                }
            }
            let arrow = ret.is_none();
            Expr::Lambda {
                span,
                generics,
                params,
                ret,
                arrow,
                body: Box::new(body),
            }
        }
    }

    fn lambda_rhs(&mut self, p: Pair<'_, Rule>) -> Expr {
        match inner(&p).next() {
            Some(c) => self.expr(c),
            None => Expr::Invalid(self.span(&p)),
        }
    }

    fn kw_block<F>(&mut self, p: Pair<'_, Rule>, build: F) -> Expr
    where
        F: FnOnce(Span, BlockExpr) -> Expr,
    {
        let span = self.span(&p);
        // The first child is the keyword pair; the block follows it.
        match inner(&p).find(|c| c.as_rule() == Rule::block_expr) {
            Some(b) => build(span, self.block(b)),
            None => build(span, empty_block(span)),
        }
    }

    fn if_expr(&mut self, p: Pair<'_, Rule>) -> Expr {
        let span = self.span(&p);
        let mut cond = None;
        let mut then = None;
        let mut else_ = None;
        let mut after_else = false;
        for c in inner(&p) {
            match c.as_rule() {
                Rule::kw_else => after_else = true,
                Rule::expr_nos => cond = Some(self.expr(c)),
                Rule::block_expr => {
                    if after_else {
                        else_ = Some(Box::new(Expr::Block(self.block(c))));
                    } else if then.is_none() {
                        then = Some(self.block(c));
                    }
                },
                Rule::if_expr if after_else => {
                    else_ = Some(Box::new(self.if_expr(c)));
                },
                _ => {},
            }
        }
        Expr::If {
            span,
            cond: Box::new(cond.unwrap_or(Expr::Invalid(span))),
            then: then.unwrap_or_else(|| empty_block(span)),
            else_,
        }
    }

    fn match_expr(&mut self, p: Pair<'_, Rule>) -> Expr {
        let span = self.span(&p);
        let mut scrutinee = None;
        let mut arms = Vec::new();
        for c in inner(&p) {
            match c.as_rule() {
                Rule::expr_nos => scrutinee = Some(self.expr(c)),
                Rule::arm_list => {
                    for a in inner(&c) {
                        if a.as_rule() == Rule::arm {
                            arms.push(self.arm(a));
                        }
                    }
                },
                _ => {},
            }
        }
        Expr::Match {
            span,
            scrutinee: Box::new(scrutinee.unwrap_or(Expr::Invalid(span))),
            arms,
        }
    }

    fn arm(&mut self, p: Pair<'_, Rule>) -> MatchArm {
        let span = self.span(&p);
        let mut pat = Pattern::Wildcard(span);
        let mut ann = None;
        let mut guard = None;
        let mut body = Expr::Invalid(span);
        for c in inner(&p) {
            match c.as_rule() {
                Rule::arm_pat => {
                    for ac in inner(&c) {
                        match ac.as_rule() {
                            Rule::pattern => pat = self.pattern(ac),
                            Rule::type_expr => ann = Some(self.type_expr(ac)),
                            _ => {},
                        }
                    }
                },
                Rule::guard => {
                    // The first child is the `kw_if` pair; the expression
                    // follows it.
                    if let Some(g) = inner(&c).find(|c| c.as_rule() == Rule::expr) {
                        guard = Some(self.expr(g));
                    }
                },
                Rule::arm_body => {
                    if let Some(b) = inner(&c).next() {
                        body = self.expr(b);
                    }
                },
                _ => {},
            }
        }
        MatchArm {
            span,
            pat,
            ann,
            guard,
            body,
        }
    }

    fn while_expr(&mut self, p: Pair<'_, Rule>) -> Expr {
        let span = self.span(&p);
        let mut cond = None;
        let mut body = empty_block(span);
        for c in inner(&p) {
            match c.as_rule() {
                Rule::expr_nos => cond = Some(self.expr(c)),
                Rule::block_expr => body = self.block(c),
                _ => {},
            }
        }
        Expr::While {
            span,
            cond: Box::new(cond.unwrap_or(Expr::Invalid(span))),
            body,
        }
    }

    fn for_expr(&mut self, p: Pair<'_, Rule>) -> Expr {
        let span = self.span(&p);
        let Some(inner_f) =
            inner(&p).find(|c| matches!(c.as_rule(), Rule::for_paren | Rule::for_plain))
        else {
            return Expr::For {
                span,
                pat: Pattern::Wildcard(span),
                iter: Box::new(Expr::Invalid(span)),
                body: Box::new(Expr::Invalid(span)),
                comptime: false,
            };
        };
        let comptime = inner(&p).any(|c| c.as_rule() == Rule::kw_comptime);
        let mut pat = Pattern::Wildcard(span);
        let mut iter = None;
        let mut body = None;
        for c in inner(&inner_f) {
            match c.as_rule() {
                Rule::pattern => pat = self.pattern(c),
                Rule::expr_nos => iter = Some(self.expr(c)),
                Rule::block_expr => body = Some(Expr::Block(self.block(c))),
                Rule::for_body => body = inner(&c).next().map(|b| self.expr(b)),
                _ => {},
            }
        }
        Expr::For {
            span,
            pat,
            iter: Box::new(iter.unwrap_or(Expr::Invalid(span))),
            body: Box::new(body.unwrap_or(Expr::Invalid(span))),
            comptime,
        }
    }

    // ===== blocks and statements =====

    /// Attaches one comment trivia of a statement block: a comment on the
    /// same line as the last statement's end trails that statement; anything
    /// else stays pending (leading trivia of the next statement, or the
    /// block's trailing tail).
    fn attach_stmt_comment(&self, t: Trivia, stmts: &mut [Stmt], pending: &mut Vec<Trivia>) {
        let start_line = self.line(t.span.start);
        let attaches_behind = start_line
            == stmts
                .last()
                .map_or(u32::MAX, |st: &Stmt| self.line(st.span.end));
        if attaches_behind {
            stmts
                .last_mut()
                .expect("guarded by map_or")
                .trailing_trivia
                .push(t);
        } else {
            pending.push(t);
        }
    }

    pub(super) fn block(&mut self, p: Pair<'_, Rule>) -> BlockExpr {
        let span = self.span(&p);
        let mut stmts = Vec::new();
        let mut trailing = Vec::new();
        let mut pending: Vec<Trivia> = Vec::new();
        for c in p.clone().into_inner() {
            match c.as_rule() {
                Rule::line_comment | Rule::block_comment => {
                    let t = self.trivia(&c);
                    self.attach_stmt_comment(t, &mut stmts, &mut pending);
                },
                Rule::stmt_list => {
                    for s in c.into_inner() {
                        match s.as_rule() {
                            Rule::line_comment | Rule::block_comment => {
                                let t = self.trivia(&s);
                                self.attach_stmt_comment(t, &mut stmts, &mut pending);
                            },
                            Rule::stmt => {
                                if let Some(inner_s) = inner(&s).next() {
                                    let stmt_span = self.span(&s);
                                    let kind = match inner_s.as_rule() {
                                        Rule::local => {
                                            StmtKind::Local(self.conv_local(inner_s.clone()))
                                        },
                                        _ => StmtKind::Expr(self.expr(inner_s.clone())),
                                    };
                                    let mut trailing_t = Vec::new();
                                    self.deep_trivia(&inner_s, &mut trailing_t);
                                    stmts.push(Stmt {
                                        span: stmt_span,
                                        kind,
                                        leading_trivia: std::mem::take(&mut pending),
                                        trailing_trivia: trailing_t,
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
        trailing.append(&mut pending);
        BlockExpr {
            span,
            stmts,
            trailing_trivia: trailing,
        }
    }
}
