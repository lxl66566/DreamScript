//! The `.dsi` signature spellings: a package's reviewable API
//! surface — every `pub` item of a parse tree rendered as its
//! re-parsable declaration line, never a body. Three consumers share the
//! renderer: `ds info` prints a whole package's surface, the LSP hover
//! answers one declaration's line, and the `ds doc` dumper additionally
//! renders impl faces — the signature spelling alone wherever the
//! consumer wants less text than the surface prints.

use std::fmt::Write as _;

use ds_parse::{
    Ast, Delim, EnumCase, EnumPayload, FieldDecl, FnDecl, ImplDecl, Item, ItemKind, Pattern,
    StructMember, TraitMember, Type,
};

use crate::{ctx::Ctx, expr, ty};

/// One member's public surface: every `pub` item's signature, in source
/// order. The `src` is the member's own source text — a binding's
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
        ItemKind::Fn(f) => fn_signature(out, ctx, f, "pub fn"),
        // the bind target rides between `extern` and `fn`, the
        // declaration's own re-quote discipline
        ItemKind::ExternFn(f) => {
            let prefix = match &f.bind {
                Some(bind) => format!("pub extern \"{}\" fn", bind.name),
                None => "pub extern fn".to_owned(),
            };
            fn_signature(out, ctx, f, &prefix);
        },
        ItemKind::Local(local) => {
            // A bodied top-level fn is the converted const-arrow binding
            // of one lambda, so the .dsi renders its fn signature;
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
                let mut line = format!("pub fn {}", name.name);
                ty::write_generics(ctx, &mut line, generics);
                ty::write_params(ctx, &mut line, params);
                if let Some(ret) = ret {
                    line.push_str(" -> ");
                    ty::write_ty(ctx, &mut line, ret);
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
            if let Some(t) = &local.ty {
                line.push_str(": ");
                ty::write_ty(ctx, &mut line, t);
            }
            line.push_str(" = ");
            expr::write_expr(ctx, &mut line, 0, &local.init);
            let _ = writeln!(out, "{line}");
        },
        ItemKind::Struct(s) => {
            let _ = write!(out, "pub struct {}", s.name.name);
            ty::write_generics(ctx, out, &s.generics);
            if !s.supers.is_empty() {
                out.push_str(": ");
                write_type_list(ctx, out, &s.supers);
            }
            let fields: Vec<String> = s
                .members
                .iter()
                .filter_map(|member| match member {
                    StructMember::Field(field) => Some(field_line(ctx, field)),
                    StructMember::Deinit { .. } => None,
                })
                .collect();
            render_body(out, &fields);
        },
        ItemKind::Enum(e) => {
            let _ = write!(out, "pub enum {}", e.name.name);
            ty::write_generics(ctx, out, &e.generics);
            if !e.supers.is_empty() {
                out.push_str(": ");
                write_type_list(ctx, out, &e.supers);
            }
            let members = e
                .cases
                .iter()
                .map(|case| case_line(ctx, case))
                .collect::<Vec<_>>();
            render_body(out, &members);
        },
        ItemKind::Tag(t) => match &t.payload {
            EnumPayload::None => {
                let _ = writeln!(out, "pub tag {}", t.name.name);
            },
            EnumPayload::Tuple(types) => {
                let mut line = format!("pub tag {}(", t.name.name);
                write_type_list(ctx, &mut line, types);
                line.push(')');
                let _ = writeln!(out, "{line}");
            },
            EnumPayload::Struct(fields) => {
                let mut line = format!("pub tag {} {{ ", t.name.name);
                write_field_list(ctx, &mut line, fields);
                line.push_str(" }");
                let _ = writeln!(out, "{line}");
            },
        },
        ItemKind::TypeAlias(a) => {
            let _ = write!(out, "pub type {}", a.name.name);
            ty::write_generics(ctx, out, &a.generics);
            out.push_str(" = ");
            ty::write_ty(ctx, out, &a.ty);
            out.push('\n');
        },
        ItemKind::Trait(t) => {
            let _ = write!(out, "pub trait {}", t.name.name);
            ty::write_generics(ctx, out, &t.generics);
            if !t.supers.is_empty() {
                out.push_str(": ");
                write_type_list(ctx, out, &t.supers);
            }
            let members = t
                .members
                .iter()
                .map(|member| match member {
                    TraitMember::Fn(f) => fn_line(ctx, f),
                    TraitMember::AssocType { name, .. } => format!("  type {}", name.name),
                })
                .collect::<Vec<_>>();
            render_body(out, &members);
        },
        ItemKind::Import(_) | ItemKind::Impl(_) | ItemKind::Platform(_) => {},
    }
}

/// One member function's signature: the `fn` spelling of a trait member
/// or an impl function, over the declaring file's bytes — the hover
/// face of one callable member, the same shape `item` renders for a
/// top-level declaration.
pub fn member_fn(out: &mut String, src: &str, f: &FnDecl) {
    let ctx = Ctx::new(src);
    fn_signature(out, &ctx, f, "fn");
}

/// One function's signature: `prefix name[g](params) -> ret`, a defaulted
/// parameter's literal spelled after its type.
fn fn_signature(out: &mut String, ctx: &Ctx, f: &FnDecl, prefix: &str) {
    let _ = write!(out, "{prefix} {}", f.name.name);
    ty::write_generics(ctx, out, &f.generics);
    ty::write_params(ctx, out, &f.params);
    if let Some(ret) = &f.ret {
        out.push_str(" -> ");
        ty::write_ty(ctx, out, ret);
    }
    out.push('\n');
}

/// One trait member's signature, indented and unprefixed.
fn fn_line(ctx: &Ctx, f: &FnDecl) -> String {
    let mut line = format!("fn {}", f.name.name);
    ty::write_generics(ctx, &mut line, &f.generics);
    ty::write_params(ctx, &mut line, &f.params);
    if let Some(ret) = &f.ret {
        line.push_str(" -> ");
        ty::write_ty(ctx, &mut line, ret);
    }
    format!("  {line}")
}

/// One struct field's line: `name: type`.
fn field_line(ctx: &Ctx, field: &FieldDecl) -> String {
    let mut line = format!("  {}: ", field.name.name);
    ty::write_ty(ctx, &mut line, &field.ty);
    line
}

/// One enum case's line: the bare name, a tuple payload's types, or a
/// struct payload's fields.
fn case_line(ctx: &Ctx, case: &EnumCase) -> String {
    let annotation = if case.default.is_some() { "#[default] " } else { "" };
    match &case.payload {
        EnumPayload::None => format!("  {}{}", annotation, case.name.name),
        EnumPayload::Tuple(types) => {
            let mut line = format!("  {}{}(", annotation, case.name.name);
            write_type_list(ctx, &mut line, types);
            line.push(')');
            line
        },
        EnumPayload::Struct(fields) => {
            let mut line = format!("  {}{} {{ ", annotation, case.name.name);
            write_field_list(ctx, &mut line, fields);
            line.push_str(" }");
            line
        },
    }
}

/// A brace body over member lines: one line when it holds at most two
/// members, one indented line per member otherwise.
fn render_body(out: &mut String, lines: &[String]) {
    if lines.is_empty() {
        let _ = writeln!(out, " {{ }}");
        return;
    }
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

/// One impl block's face: `impl[g]? Trait? for? Target` over its member
/// signatures, never bodies — the doc dumper's spelling (`ds info`'s
/// `.dsi` carries no impl rows; the dumper documents them). The trait
/// head rides the generic-application spelling, the members exactly the
/// trait face's lines.
pub fn impl_face(out: &mut String, src: &str, decl: &ImplDecl) {
    let ctx = Ctx::new(src);
    let mut head = String::from("impl");
    ty::write_generics(&ctx, &mut head, &decl.generics);
    if let Some(trait_ref) = &decl.trait_ref {
        head.push(' ');
        ty::write_ty(&ctx, &mut head, &Type::Path {
            span: trait_ref.span,
            path: trait_ref.path.clone(),
            args: trait_ref.args.clone(),
            delim: Delim::Bracket,
        });
        head.push_str(" for ");
    } else {
        head.push(' ');
    }
    ty::write_ty(&ctx, &mut head, &decl.target);
    if let Some(name) = &decl.name {
        head.push_str(" as ");
        head.push_str(&name.name);
    }
    out.push_str(&head);
    let members = decl
        .fns
        .iter()
        .map(|f| fn_line(&ctx, f))
        .collect::<Vec<_>>();
    render_body(out, &members);
}

/// A comma-joined type list: supers, tuple payloads.
fn write_type_list(ctx: &Ctx, out: &mut String, types: &[Type]) {
    for (i, t) in types.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        ty::write_ty(ctx, out, t);
    }
}

/// A comma-joined `name: type` field list: tag and case struct payloads.
fn write_field_list(ctx: &Ctx, out: &mut String, fields: &[FieldDecl]) {
    for (i, field) in fields.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        out.push_str(&field.name.name);
        out.push_str(": ");
        ty::write_ty(ctx, out, &field.ty);
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

    /// A foreign type's name cooks its escapes in the AST, so the surface
    /// renders the literal verbatim from its span — a re-spelling of the
    /// cooked text would emit the raw quote and fail the re-parse.
    #[test]
    fn foreign_type_names_render_verbatim() {
        let src = "pub type db = c[\"a\\\"b\"]\n";
        let mut files = Files::new();
        let file = files.add_source("m.ds", src);
        let ast = parse(&files, file).expect("the source parses");
        let mut out = String::new();
        member(&mut out, src, &ast);
        assert_eq!(out, src, "the foreign name keeps its own escaping");
        let mut files = Files::new();
        let file = files.add_source("surface.dsi", &out);
        let ast = parse(&files, file)
            .unwrap_or_else(|diags| panic!("the surface re-parses: {}", diags[0].message));
        let mut again = String::new();
        member(&mut again, &out, &ast);
        assert_eq!(again, out, "the surface is a fixed point");
    }

    /// A case-type reference (`badnumber(string)`) keeps its parens in the
    /// surface: the delimiter is the source's own spelling, so the line
    /// re-parses as the same case-type reference, not a generic
    /// application.
    #[test]
    fn case_type_refs_keep_their_parens_in_the_surface() {
        let src = "pub type e = badnumber(string) | missing\n";
        let mut files = Files::new();
        let file = files.add_source("m.ds", src);
        let ast = parse(&files, file).expect("the source parses");
        let mut out = String::new();
        member(&mut out, src, &ast);
        assert_eq!(out, src, "the parens survive the surface spelling");
        let mut files = Files::new();
        let file = files.add_source("surface.dsi", &out);
        let ast = parse(&files, file)
            .unwrap_or_else(|diags| panic!("the surface re-parses: {}", diags[0].message));
        let mut again = String::new();
        member(&mut again, &out, &ast);
        assert_eq!(again, out, "the surface is a fixed point");
    }

    /// A bodied generic fn is the converted const-arrow binding,
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

    /// A defaulted parameter's literal renders through the expression
    /// renderer, so the awkward spellings survive the surface: a char
    /// default escapes its quote and its newline, a byte default keeps
    /// one escaped char (never a decimal run), and string/raw defaults
    /// re-render verbatim from their own source text — never a
    /// placeholder the grammar refuses.
    #[test]
    fn defaulted_literals_reparse_in_their_rendered_spellings() {
        let src = "pub fn tricky(q: char = '\\'', n: char = '\\n', x: byte = b'x', d: byte = \
                   b'\\u{7f}', s: string = \"a\\\"b\\\\c\\nd\", r: string = r#\"he said \
                   \"hi\"\"#) -> int\n";
        let mut files = Files::new();
        let file = files.add_source("m.ds", src);
        let ast = parse(&files, file).expect("the source parses");
        let mut out = String::new();
        member(&mut out, src, &ast);
        assert_eq!(
            out,
            "pub fn tricky(q: char = '\\'', n: char = '\\n', x: byte = b'x', d: byte = \
             b'\\u{7f}', s: string = \"a\\\"b\\\\c\\nd\", r: string = r#\"he said \"hi\"\"#) -> \
             int\n",
            "the defaults render in their re-parsable spellings"
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
