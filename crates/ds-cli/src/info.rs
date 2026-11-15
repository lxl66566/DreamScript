//! The `ds info` subcommand: render the `.dsi` — one package's
//! reviewable API surface (G4). The `pub` declarations of the target's
//! members print as their signature spellings, `pub(pkg)` and private
//! declarations stay out, and the rendering is re-parsable DreamScript:
//! each line is the declaration a dependent's author reads, never a
//! body. A file target renders its own surface alone; a directory target
//! renders every .ds member below it (the package's members, the same
//! walk `ds save` collects), one surface in member order.

use std::{
    fmt::Write as _,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use clap::Args;
use ds_diag::{Files, render};
use ds_parse::{
    Ast, EnumCase, EnumPayload, FieldDecl, FnDecl, Ident, Item, ItemKind, Param, ParamMode,
    Pattern, StructMember, TraitMember, Type, TypeParam, parse,
};

/// The `ds info` surface: one package root or one .ds source file.
#[derive(Debug, Args)]
pub struct InfoArgs {
    /// Path of the package root directory or one .ds source file
    target: PathBuf,
}

/// Renders the target's `.dsi` to stdout.
///
/// # Errors
///
/// An unreadable target, a member that fails to parse, and a root holding
/// no .ds source.
pub fn info(args: &InfoArgs) -> Result<()> {
    let members = members(&args.target)?;
    let mut files = Files::new();
    let mut asts = Vec::new();
    for path in &members {
        let source =
            fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let file = files.add_source(path.to_string_lossy().into_owned(), &source);
        let ast = parse(&files, file).map_err(|diags| plain(&diags, &files))?;
        asts.push(ast);
    }
    let mut out = String::new();
    for ast in &asts {
        render_member(&mut out, ast);
    }
    print!("{out}");
    Ok(())
}

/// Every diagnostic rendered as plain text, the run's error to print.
fn plain(diags: &[ds_diag::Diagnostic], files: &Files) -> anyhow::Error {
    anyhow::anyhow!(
        "{}",
        diags
            .iter()
            .map(|diag| render::human(diag, files))
            .collect::<String>()
    )
}

/// The target's member files: the file itself, or every .ds below a root
/// in ascending path order.
///
/// # Errors
///
/// A missing target and a root holding no .ds source.
fn members(target: &Path) -> Result<Vec<PathBuf>> {
    if target.is_file() {
        return Ok(vec![target.to_path_buf()]);
    }
    anyhow::ensure!(target.exists(), "{} does not exist", target.display());
    let mut members = Vec::new();
    let mut stack = vec![target.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).with_context(|| format!("reading {}", dir.display()))? {
            let path = entry
                .with_context(|| format!("reading {}", dir.display()))?
                .path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().and_then(|ext| ext.to_str()) == Some("ds") {
                members.push(path);
            }
        }
    }
    if members.is_empty() {
        bail!("{} holds no .ds source member", target.display());
    }
    members.sort();
    Ok(members)
}

/// One member's public surface: every `pub` item's signature, in source
/// order (G4).
fn render_member(out: &mut String, ast: &Ast) {
    for item in ast.blocks.iter().flat_map(|block| block.items.iter()) {
        if matches!(item.vis, Some(ds_parse::Vis::Pub(_))) {
            render_item(out, item);
        }
    }
}

/// One `pub` item's signature lines.
fn render_item(out: &mut String, item: &Item) {
    match &item.kind {
        ItemKind::Fn(f) => fn_signature(out, f, "pub fn"),
        ItemKind::ExternFn(f) => fn_signature(out, f, "pub extern fn"),
        ItemKind::Local(local) => {
            // A bodied top-level fn is the converted const-arrow binding
            // of one lambda (F2), so the .dsi renders its fn signature;
            // a plain binding renders its names and annotation alone.
            if let (Pattern::Binding { name, .. }, ds_parse::Expr::Lambda { params, ret, .. }) =
                (&local.pat, &local.init)
            {
                let mut line = format!("pub fn {}", name.name);
                write_params(&mut line, params);
                if let Some(ret) = ret {
                    let _ = write!(line, " -> {}", render_type(ret));
                }
                let _ = writeln!(out, "{line}");
                return;
            }
            let keyword = if local.mutable { "var" } else { "const" };
            let ty = local
                .ty
                .as_ref()
                .map_or_else(String::new, |ty| format!(": {}", render_type(ty)));
            let _ = writeln!(
                out,
                "pub {keyword} {}{ty}",
                binder_names(&local.pat).join(", ")
            );
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
        ItemKind::Tag(t) => {
            let _ = writeln!(out, "pub tag {}", t.name.name);
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

/// The names a top-level binding pattern binds, in pattern order: plain
/// names and tuple destructures, the shapes a package-level binding can
/// spell.
fn binder_names(pat: &Pattern) -> Vec<String> {
    match pat {
        Pattern::Binding { name, .. } => vec![name.name.clone()],
        Pattern::Tuple { pats, .. } => pats.iter().flat_map(binder_names).collect(),
        _ => Vec::new(),
    }
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
