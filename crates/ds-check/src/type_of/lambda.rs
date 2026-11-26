//! Lambda typing: parameter and return types inferred against the expected
//! fn type, parameter binding, and the body walk under its own scope frame.

use super::{item::collect_type_names, *};

impl Bodies<'_> {
    pub(super) fn lambda(&mut self, e: &Expr, expected: Option<Ty>) -> Val {
        let Expr::Lambda {
            span,
            generics,
            params,
            ret,
            body,
            ..
        } = e
        else {
            return Val {
                ty: self.empty(),
                diverges: false,
                poisoned: false,
            };
        };
        // The script-entry fold's synthesized `main` reaches here as a
        // lambda the user never wrote; its own span is empty (no real node
        // owns an empty span — the synthesis's alias guard), so the
        // boundary label names the script entry, not a lambda.
        let body_label = if span.start == span.end { "script main body" } else { "lambda body" };
        let expected_sig = expected.and_then(|ty| self.decompose_fn(ty));
        // A lambda literal's own arity matches the expected fn type's
        // exactly: the call ABI stages the declared parameter count, so a
        // lambda taking fewer parameters would trap at the arity
        // validation the moment it is called — the mismatch refuses at
        // the literal's own check, never at the call.
        let arity_reported = expected_sig
            .as_ref()
            .is_some_and(|(expected_params, _)| expected_params.len() != params.len());
        if arity_reported {
            let list_span = match (params.first(), params.last()) {
                (Some(first), Some(last)) => {
                    Span::new(first.span.file, first.span.start, last.span.end)
                },
                _ => *span,
            };
            let expected_len = expected_sig
                .as_ref()
                .map_or(0, |(expected_params, _)| expected_params.len());
            let mut diagnostic = Diagnostic::error(
                ErrorCode::ArityMismatch,
                format!(
                    "the lambda takes {} parameters, the expected fn type takes {}",
                    params.len(),
                    expected_len
                ),
            )
            .with_primary(list_span, "lambda parameter list");
            // The feeding call site knows where its expectation was
            // written (the callee's declared fn type); an expectation
            // arriving by propagation (an arm, a tuple element) has no
            // written site and the message alone carries the fact.
            if let Some(ty_span) = self.expected_ty_span.take() {
                diagnostic = diagnostic.with_secondary(ty_span, "expected fn type");
            }
            self.diags.push(diagnostic);
        }
        // The body is a fresh expression world: a stale citation from
        // this lambda's own feeding site must not reach a nested lambda.
        self.expected_ty_span = None;
        // The comptime parameter surface: a free fn only, with every
        // parameter comptime and annotated
        // `type` (the evaluator stages one reflection index per
        // parameter) or one of the four scalar atoms — the evaluator
        // stages a plain literal of the annotation — so any other shape
        // refuses at the declaration instead of at each call site.
        let on_fn_decl = self.comptime.declaring_fn;
        self.comptime.declaring_fn = false;
        let comptime_fn = params
            .iter()
            .any(|p| matches!(p.mode, ds_parse::ParamMode::Comptime(_)));
        if comptime_fn {
            let broken = if !on_fn_decl {
                Some("a comptime parameter is a free-fn surface; nested lambdas take none")
            } else if !generics.is_empty() {
                Some("a comptime function declares no type parameters this round")
            } else if params
                .iter()
                .any(|p| !matches!(p.mode, ds_parse::ParamMode::Comptime(_)))
            {
                Some("every parameter of a comptime function is comptime this round")
            } else if params.iter().any(|p| !comptime_annotation(p.ty.as_ref())) {
                Some(
                    "a comptime parameter is annotated `type` or one of the scalar atoms int, \
                     float, bool, string this round",
                )
            } else {
                None
            };
            if let Some(broken) = broken {
                self.error(
                    ErrorCode::ComptimeParamShape,
                    broken.to_owned(),
                    params
                        .iter()
                        .find_map(|p| match p.mode {
                            ds_parse::ParamMode::Comptime(span) => Some(span),
                            _ => None,
                        })
                        .unwrap_or(*span),
                    "comptime parameter",
                );
            }
        }
        self.push_generics(&generics.iter().map(generic_of).collect::<Vec<_>>());
        let saved = self.snapshot();
        let mut param_tys = Vec::with_capacity(params.len());
        for (index, param) in params.iter().enumerate() {
            let solved = param
                .ty
                .as_ref()
                .and_then(|t| self.solve_here(t))
                .or_else(|| {
                    expected_sig
                        .as_ref()
                        .and_then(|(ps, _)| ps.get(index).copied())
                });
            let Some(solved) = solved else {
                // A parameter past a mismatched expectation's length
                // already has its root cause (the arity refusal); an
                // uninferable-parameter report would be a follow-on.
                if !arity_reported && !self.take_solve_reported() {
                    self.error(
                        ErrorCode::InferenceFailed,
                        format!(
                            "parameter `{}` has no expected type to infer from; annotate it [B2]",
                            param.name.name
                        ),
                        param.name.span,
                        "uninferable parameter",
                    );
                }
                let empty = self.empty();
                param_tys.push(empty);
                self.bind_local(param.name.span, Binding {
                    ty: empty,
                    mutable: false,
                });
                continue;
            };
            param_tys.push(solved);
            // A parameter typed the empty set holds a value no call ever
            // supplies: no value inhabits `never`, so the annotation
            // refuses at its own span.
            // The refusal reads the written annotation alone: a type the
            // expected fn signature threads in (an un-annotated
            // parameter) inherits whatever its caller solved, and an
            // empty arrival there is the poisoned chain's own cascade,
            // never this position's root cause.
            if param.ty.is_some() && self.decls.universe.is_empty(solved) {
                self.error(
                    ErrorCode::NeverTypedPosition,
                    format!(
                        "parameter `{}` is typed `never`: no value inhabits the type, so no call \
                         ever supplies one",
                        param.name.name
                    ),
                    param
                        .ty
                        .as_ref()
                        .map(Type::span)
                        .expect("the refusal reads the written annotation"),
                    "parameter typed never",
                );
            }
            // A `type`-annotated parameter without the comptime mode has
            // no runtime spelling: nothing constructs the builtin atom's
            // values outside comptime evaluation, so the annotation
            // refuses here rather than at every call.
            if self.is_type_atom(solved) && !matches!(param.mode, ds_parse::ParamMode::Comptime(_))
            {
                self.error(
                    ErrorCode::TypeParamNotComptime,
                    format!(
                        "parameter `{}` is annotated `type`; a type value exists only inside \
                         comptime evaluation, so spell the parameter `comptime {}`",
                        param.name.name, param.name.name
                    ),
                    param.name.span,
                    "type parameter without the comptime mode",
                );
            }
            self.bind_local(param.name.span, Binding {
                ty: solved,
                mutable: param_is_mut(param),
            });
        }
        let declared_ret = ret.as_ref().and_then(|r| self.solve_here(r));
        // The retired wildcard return: a generic whose return type names
        // a variable no parameter mentions never solves from a call's
        // arguments — the escape made every diverging ambient spell a
        // fake generic answer, and `never` is the real spelling. A
        // lambda literal carries no generics, so the check reads only
        // the named-fn spelling's own parameters.
        if !generics.is_empty()
            && let Some(ret_ty) = ret.as_ref()
        {
            let generic_names: Vec<String> = generics.iter().map(|g| g.name.name.clone()).collect();
            let mut unbound = Vec::new();
            collect_type_names(ret_ty, &mut unbound);
            unbound.retain(|name| generic_names.contains(name));
            for param in params {
                if let Some(param_ty) = &param.ty {
                    let mut mentioned = Vec::new();
                    collect_type_names(param_ty, &mut mentioned);
                    unbound.retain(|name| !mentioned.contains(name));
                }
            }
            for name in unbound {
                self.error(
                    ErrorCode::UnconstrainedReturnVariable,
                    format!(
                        "return variable `{name}` names no parameter, so no call's arguments bind \
                         it; a diverging declaration answers `never`"
                    ),
                    ret_ty.span(),
                    "unconstrained return variable",
                );
            }
        }
        let fn_ret_expected = declared_ret.or_else(|| expected_sig.as_ref().map(|(_, r)| *r));
        // A still-unbound variable as the expected return is the
        // instantiation's inference hole (a generic callee's `r`): the
        // lambda's own body type solves it, checked by the caller's
        // match against the fn-typed parameter.
        let expected_is_var = fn_ret_expected.is_some_and(|r| self.is_var_ty(r));
        let old_ret = self.ret;
        self.ret = if expected_is_var { None } else { fn_ret_expected };
        // A lambda body starts its own frame: the lexical loop depth
        // stays with the enclosing frame. The synthesized script entry
        // keeps the file walk's depth; its stray exits have no loop, and
        // the boundary message would name a lambda the user never wrote.
        let frame = (span.start != span.end).then(|| self.enter_frame());
        // A comptime function's body checks under the comptime context:
        // type names are values of the builtin atom there and the
        // reflection builtins answer. Nested lambdas inside the body keep
        // the context — they evaluate inside the same compile-time run.
        let outer_ctx = self.comptime;
        self.comptime.ctx |= comptime_fn;
        // A real lambda's body is its own frame: a `spawn` lexically
        // inside it names no nursery (the entry lowers as its own
        // function), so the answer collector clears for the body and
        // restores after. The script entry's synthesized `main` keeps
        // the stack — its body is the file's top level, where a nursery
        // stands exactly like one inside a declared fn.
        let outer_answers =
            (span.start != span.end).then(|| std::mem::take(&mut self.spawn_answers));
        // The lambda's body is its own frame's statement stream: its tail
        // may exit early like any function body's.
        self.value_pos = false;
        let body_val = self.expr(body, self.ret);
        if let Some(outer_answers) = outer_answers {
            self.spawn_answers = outer_answers;
        }
        self.comptime = outer_ctx;
        if let Some(frame) = frame {
            self.exit_frame(frame);
        }
        let fn_ret = match fn_ret_expected {
            Some(r) if !expected_is_var => {
                if !body_val.diverges {
                    self.expect_contains(body_val, r, body.span(), body_label);
                }
                r
            },
            _ => body_val.ty,
        };
        self.ret = old_ret;
        self.restore(saved);
        self.pop_generics();
        let params_boxed: Box<[Ty]> = param_tys.into();
        let ty = self
            .decls
            .universe
            .fn_atom(&params_boxed, fn_ret)
            .ok()
            .and_then(|atom| self.decls.universe.atom_ty(atom).ok())
            .unwrap_or_else(|| self.empty());
        Val {
            ty,
            diverges: false,
            // The arity refusal is the root cause; enclosing containment
            // checks consume the poisoned value without re-reporting.
            poisoned: arity_reported,
        }
    }
}
