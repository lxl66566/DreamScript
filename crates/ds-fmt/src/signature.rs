//! The `.dsi` signature spellings (G4): a package's reviewable API
//! surface — every `pub` item of a parse tree rendered as its
//! re-parsable declaration line, never a body. Two consumers share the
//! renderer: `ds info` prints a whole package's surface, and the LSP
//! hover answers one declaration's line — the signature spelling alone
//! wherever the consumer wants less text than the surface prints.

use std::fmt::Write as _;

use ds_parse::{
    Ast, EnumCase, EnumPayload, FieldDecl, FnDecl, Ident, Item, ItemKind, Param, ParamMode,
    Pattern, StructMember, TraitMember, Type, TypeParam,
};

use crate::{ctx::Ctx, expr};

/// One member's public surface: every `pub` item's signature, in source
/// order (G4). The `src` is the member's own source text — a binding's
/// pattern and initializer render through the expression renderer, which
/// reads string literals verbatim.
pub fn member(out: &mut String, src: &str, ast: &Ast) {
    // One Ctx over the whole member: its line table is a scan of the
    // full source, so per-item construction made a binding-dense member
    // pay declarations x file length.
    let ctx = Ctx::new(src);
    for entry in ast.blocks.iter().flat_map(|block| block.items.iter()) {
        if matches!(entry.vis, Some(ds_parse::Vis::Pub(_))) {
            render_item(out, &ctx, entry);
        }
    }
}

/// One item's signature lines, spelled as the exported tier reads them
/// (the `pub` prefix the `.dsi` carries); the caller owns the
/// visibility gate — `member` applies the `pub` tier for the surface, a
/// consumer showing one declaration it already resolved filters by its
/// own rule.
pub fn item(out: &mut String, src: &str, item: &Item) {
    // The one-item entry builds its own context; a caller rendering many
    // items over one source builds one and shares it through
    // `render_item` instead.
    let ctx = Ctx::new(src);
    render_item(out, &ctx, item);
}

/// [`item`]'s rendering body over a shared [`Ctx`].
fn render_item(out: &mut String, ctx: &Ctx, item: &Item) {
    match &item.kind {
        ItemKind::Fn(f) => fn_signature(out, f, "pub fn"),
        // the bind target rides between `extern` and `fn` (H5), the
        // declaration's own re-quote discipline
        ItemKind::ExternFn(f) => {
            let prefix = match &f.bind {
                Some(bind) => format!("pub extern \"{}\" fn", bind.name),
                None => "pub extern fn".to_owned(),
            };
            fn_signature(out, f, &prefix);
        },
        ItemKind::Local(local) => {
            // A bodied top-level fn is the converted const-arrow binding
            // of one lambda (F2), so the .dsi renders its fn signature;
            // a plain binding renders its names and annotation alone.
            if let (
                Pattern::Binding { name, .. },
                ds_parse::Expr::Lambda {
                    generics,
                    params,
                    ret,
                    ..
                },
            ) = (&local.pat, &local.init)
            {
                let mut line = format!("pub fn {}", head(name, generics));
                write_params(&mut line, params);
                if let Some(ret) = ret {
                    let _ = write!(line, " -> {}", render_type(ret));
                }
                let _ = writeln!(out, "{line}");
                return;
            }
            // A plain binding keeps its pattern, annotation, and
            // initializer — `const`/`var` without `= expr` does not
            // re-parse (the surface's every-line contract), so the
            // initializer renders through the expression renderer like
            // ds fmt spells it.
            let mut line = format!("pub {} ", if local.mutable { "var" } else { "const" });
            expr::write_pattern(ctx, &mut line, &local.pat);
            if let Some(ty) = &local.ty {
                let _ = write!(line, ": {}", render_type(ty));
            }
            line.push_str(" = ");
            expr::write_expr(ctx, &mut line, 0, &local.init);
            let _ = writeln!(out, "{line}");
        },
        ItemKind::Struct(s) => {
            let _ = write!(out, "pub struct {}", head(&s.name, &s.generics));
            if !s.supers.is_empty() {
                let _ = write!(out, ": {}", type_list(&s.supers));
            }
            let fields: Vec<String> = s
                .members
                .iter()
                .filter_map(|member| match member {
                    StructMember::Field(field) => Some(field_line(field)),
                    StructMember::Deinit { .. } => None,
                })
                .collect();
            render_body(out, &fields);
        },
        ItemKind::Enum(e) => {
            let _ = write!(out, "pub enum {}", head(&e.name, &e.generics));
            if !e.supers.is_empty() {
                let _ = write!(out, ": {}", type_list(&e.supers));
            }
            render_body(out, &e.cases.iter().map(case_line).collect::<Vec<_>>());
        },
        ItemKind::Tag(t) => match &t.payload {
            EnumPayload::None => {
                let _ = writeln!(out, "pub tag {}", t.name.name);
            },
            EnumPayload::Tuple(types) => {
                let _ = writeln!(out, "pub tag {}({})", t.name.name, type_list(types));
            },
            EnumPayload::Struct(fields) => {
                let _ = writeln!(
                    out,
                    "pub tag {} {{ {} }}",
                    t.name.name,
                    fields
                        .iter()
                        .map(|field| format!("{}: {}", field.name.name, render_type(&field.ty)))
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            },
        },
        ItemKind::TypeAlias(a) => {
            let _ = writeln!(
                out,
                "pub type {} = {}",
                head(&a.name, &a.generics),
                render_type(&a.ty)
            );
        },
        ItemKind::Trait(t) => {
            let _ = write!(out, "pub trait {}", head(&t.name, &t.generics));
            if !t.supers.is_empty() {
                let _ = write!(out, ": {}", type_list(&t.supers));
            }
            let members = t
                .members
                .iter()
                .map(|member| match member {
                    TraitMember::Fn(f) => fn_line(f),
                    TraitMember::AssocType { name, .. } => format!("  type {}", name.name),
                })
                .collect::<Vec<_>>();
            render_body(out, &members);
        },
        ItemKind::Import(_) | ItemKind::Impl(_) | ItemKind::Platform(_) => {},
    }
}

/// One function's signature: `prefix name[g](params) -> ret`, a defaulted
/// parameter's literal spelled after its type.
fn fn_signature(out: &mut String, f: &FnDecl, prefix: &str) {
    let mut line = format!("{prefix} {}", head(&f.name, &f.generics));
    write_params(&mut line, &f.params);
    if let Some(ret) = &f.ret {
        let _ = write!(line, " -> {}", render_type(ret));
    }
    let _ = writeln!(out, "{line}");
}

/// A parameter list in its parens: `(mut xs: list[int], ...)`.
fn write_params(line: &mut String, params: &[Param]) {
    line.push('(');
    for (index, param) in params.iter().enumerate() {
        if index > 0 {
            line.push_str(", ");
        }
        let _ = write!(line, "{}", param_line(param));
    }
    line.push(')');
}

/// One trait member's signature, indented and unprefixed.
fn fn_line(f: &FnDecl) -> String {
    let mut line = format!("fn {}", head(&f.name, &f.generics));
    write_params(&mut line, &f.params);
    if let Some(ret) = &f.ret {
        let _ = write!(line, " -> {}", render_type(ret));
    }
    format!("  {line}")
}

/// A declaration head with its generic parameters: `name[t, v: bound]`.
fn head(name: &Ident, generics: &[TypeParam]) -> String {
    if generics.is_empty() {
        return name.name.clone();
    }
    let params = generics
        .iter()
        .map(|param| match &param.bound {
            Some(bound) => format!("{}: {}", param.name.name, render_type(bound)),
            None => param.name.name.clone(),
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!("{}[{params}]", name.name)
}

/// One parameter's signature: the mode keyword, the name, the annotation,
/// and a defaulted parameter's literal (F7: defaults are literal-only).
fn param_line(param: &Param) -> String {
    let mode = match param.mode {
        ParamMode::Borrow => "",
        ParamMode::Mut(_) => "mut ",
        ParamMode::Consume(_) => "move ",
        ParamMode::Comptime(_) => "comptime ",
    };
    let mut line = format!("{mode}{}", param.name.name);
    if let Some(ty) = &param.ty {
        let _ = write!(line, ": {}", render_type(ty));
    }
    if let Some(default) = &param.default {
        let _ = write!(line, " = {}", literal(default));
    }
    line
}

/// The literal spelling of a defaulted parameter: the literal's own token
/// text where one exists, a placeholder for the forms a bare token
/// cannot spell.
fn literal(expr: &ds_parse::Expr) -> String {
    use ds_parse::{Expr, Literal};
    match expr {
        Expr::Literal {
            lit: Literal::Int { text, .. } | Literal::Float { text, .. },
            ..
        } => text.clone(),
        Expr::Literal {
            lit: Literal::Char(c),
            ..
        } => format!("'{c}'"),
        Expr::Literal {
            lit: Literal::Byte(b),
            ..
        } => format!("b'{b}'"),
        Expr::Literal {
            lit: Literal::Bool(b),
            ..
        } => format!("{b}"),
        Expr::Str { .. } => "\"...\"".to_owned(),
        Expr::RawStr { .. } => "`...`".to_owned(),
        _ => "<literal>".to_owned(),
    }
}

/// One struct field's line: `name: type`.
fn field_line(field: &FieldDecl) -> String {
    format!("  {}: {}", field.name.name, render_type(&field.ty))
}

/// One enum case's line: the bare name, a tuple payload's types, or a
/// struct payload's fields.
fn case_line(case: &EnumCase) -> String {
    let annotation = if case.default.is_some() { "#[default] " } else { "" };
    match &case.payload {
        EnumPayload::None => format!("  {}{}", annotation, case.name.name),
        EnumPayload::Tuple(types) => {
            format!("  {}{}({})", annotation, case.name.name, type_list(types))
        },
        EnumPayload::Struct(fields) => format!(
            "  {}{} {{ {} }}",
            annotation,
            case.name.name,
            fields
                .iter()
                .map(|field| format!("{}: {}", field.name.name, render_type(&field.ty)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

/// A brace body over member lines: one line when it holds at most two
/// members, one indented line per member otherwise.
fn render_body(out: &mut String, lines: &[String]) {
    if lines.len() <= 2 {
        let joined = lines
            .iter()
            .map(|line| line.trim_start())
            .collect::<Vec<_>>()
            .join(", ");
        let _ = writeln!(out, " {{ {joined} }}");
        return;
    }
    out.push_str(" {\n");
    for line in lines {
        let _ = writeln!(out, "{line}");
    }
    out.push_str("}\n");
}

/// A comma-joined type list: supers, tuple payloads.
fn type_list(types: &[Type]) -> String {
    types.iter().map(render_type).collect::<Vec<_>>().join(", ")
}

/// The signature spelling of one written type (F2, B3, H5): paths with
/// generic arguments, `?`, `~`, `&`, `|`, `-`, tuples, unit, fn types,
/// pointers, `type`, and opaque foreign names.
fn render_type(ty: &Type) -> String {
    match ty {
        Type::Path { path, args, .. } => {
            let mut text = path
                .segments
                .iter()
                .map(|segment| segment.name.clone())
                .collect::<Vec<_>>()
                .join("::");
            if !args.is_empty() {
                let _ = write!(text, "[{}]", type_list(args));
            }
            text
        },
        Type::Optional { inner, .. } => format!("{}?", render_type(inner)),
        Type::Negation { inner, .. } => format!("~{}", render_type(inner)),
        Type::Union { left, right, .. } => {
            format!("{} | {}", render_type(left), render_type(right))
        },
        Type::Intersect { left, right, .. } => {
            format!("{} & {}", render_type(left), render_type(right))
        },
        Type::Difference { left, right, .. } => {
            format!("{} - {}", render_type(left), render_type(right))
        },
        Type::Paren { inner, .. } => format!("({})", render_type(inner)),
        Type::Tuple { items, .. } => format!("({})", type_list(items)),
        Type::Unit(_) => "()".to_owned(),
        Type::Fn { params, ret, .. } => {
            let mut text = String::from("fn(");
            for (index, param) in params.iter().enumerate() {
                if index > 0 {
                    text.push_str(", ");
                }
                let mode = match param.mode {
                    ParamMode::Borrow => "",
                    ParamMode::Mut(_) => "mut ",
                    ParamMode::Consume(_) => "move ",
                    ParamMode::Comptime(_) => "comptime ",
                };
                let _ = write!(text, "{mode}{}", param.name.name);
                if let Some(ty) = &param.ty {
                    let _ = write!(text, ": {}", render_type(ty));
                }
            }
            text.push(')');
            if let Some(ret) = ret {
                let _ = write!(text, " -> {}", render_type(ret));
            }
            text
        },
        Type::Ptr { mutable, inner, .. } => {
            format!(
                "*{} {}",
                if *mutable { "mut" } else { "const" },
                render_type(inner)
            )
        },
        Type::TypeKw(_) => "type".to_owned(),
        Type::Foreign { name, .. } => format!("c[\"{name}\"]"),
    }
}

#[cfg(test)]
mod tests {
    use ds_diag::Files;
    use ds_parse::parse;

    use super::*;

    /// The surface's own contract: every `pub const`/`pub var` line
    /// re-parses — the binding carries its initializer (the bare
    /// `pub const name` spelling fails DS1002 and poisons the following
    /// line), a tuple pattern keeps its parentheses, and the rendering is
    /// a fixed point of the renderer.
    #[test]
    fn binding_lines_reparse_with_their_initializers() {
        let src = concat!(
            "pub const urls = [\"https://ds.dev\"]\n",
            "pub const tagged: list[string] = [\"a\"]\n",
            "pub const (a, b) = (1, 2)\n",
            "pub const text = \"kept\"\n",
        );
        let mut files = Files::new();
        let file = files.add_source("m.ds", src);
        let ast = parse(&files, file).expect("the source parses");
        let mut out = String::new();
        member(&mut out, src, &ast);
        assert_eq!(
            out,
            concat!(
                "pub const urls = [\"https://ds.dev\"]\n",
                "pub const tagged: list[string] = [\"a\"]\n",
                "pub const (a, b) = (1, 2)\n",
                "pub const text = \"kept\"\n",
            ),
            "the initializer renders in the .dsi spelling"
        );
        let mut files = Files::new();
        let file = files.add_source("surface.dsi", &out);
        let ast = parse(&files, file)
            .unwrap_or_else(|diags| panic!("the surface re-parses: {}", diags[0].message));
        let mut again = String::new();
        member(&mut again, &out, &ast);
        assert_eq!(again, out, "the surface is a fixed point");
    }

    /// A bodied generic fn is the converted const-arrow binding (F2),
    /// so its surface line renders through the lambda's own parts —
    /// the generic parameters included, a line missing them failing
    /// DS2004 at the re-parse (the type variables unknown).
    #[test]
    fn a_bodied_generic_fn_renders_its_type_parameters() {
        let src = concat!(
            "pub fn pick[t] (xs: list[t]) -> t { xs.first() }\n",
            "pub extern fn cons2[t] (x: t, xs: list[t]) -> list[t]\n",
        );
        let mut files = Files::new();
        let file = files.add_source("m.ds", src);
        let ast = parse(&files, file).expect("the source parses");
        let mut out = String::new();
        member(&mut out, src, &ast);
        assert_eq!(
            out,
            concat!(
                "pub fn pick[t](xs: list[t]) -> t\n",
                "pub extern fn cons2[t](x: t, xs: list[t]) -> list[t]\n",
            ),
            "the bodied pair keeps the extern's own bracket spelling"
        );
        let mut files = Files::new();
        let file = files.add_source("surface.dsi", &out);
        let ast = parse(&files, file)
            .unwrap_or_else(|diags| panic!("the surface re-parses: {}", diags[0].message));
        let mut again = String::new();
        member(&mut again, &out, &ast);
        assert_eq!(again, out, "the surface is a fixed point");
    }
}
