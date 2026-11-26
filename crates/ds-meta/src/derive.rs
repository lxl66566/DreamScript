//! The derive engine: the meta-language is the language, so a
//! derive attribute expands into an ordinary impl declaration —
//! AST-level synthesis after parse, before resolution — and the
//! synthesized impl flows through the normal checker (trait coherence,
//! signature agreement) and the normal lowering. Nothing in the
//! emission paths knows a derive exists.
//!
//! `show`, `ord`, `hash`, `clone`, `default`, `from_json`, and
//! `to_json` are the landed families, one module each; this module
//! dispatches a derive argument to the family that owns it, and every
//! family mints through the shared vocabulary of `mint`.
//!
//! `==` refuses (equality is universal and never
//! derived); unknown names refuse naming the vocabulary; a
//! function-typed or `~Copyable` part refuses naming the part; a
//! generic declaration passes its check-side doors (a generic struct
//! through the added bound, a unit-only generic enum through arms that
//! resolve their case atoms over every application of the head) and the
//! runtime seam owns the refusal (one member body per impl, so a
//! receiver whose shape spans those parameters waits on the JIT's
//! per-instantiation frames); a payloaded case over a generic head
//! refuses at the attribute (no spelling resolves its atom), and so
//! does a generic enum under the constructing families (a constructed
//! case types as its own atom, never the applied head).

use ds_diag::{Diagnostic, ErrorCode, Span};
use ds_parse::{
    AssignOp, Ast, Attribute, BinaryOp, BlockExpr, CallArg, Delim, EnumCase, EnumDecl, EnumPayload,
    Expr, FieldDecl, FnDecl, Ident, ImplDecl, Item, ItemKind, Literal, LocalDecl, MatchArm, Param,
    ParamMode, Path, Pattern, Stmt, StmtKind, StrPart, StructMember, Type, TypeParam, TypeRef,
    UnaryOp,
};

mod clone;
mod default;
mod from_json;
mod hash;
mod mint;
mod ord;
mod show;
mod to_json;

use clone::clone_impl;
use default::{UnionDefaults, default_impl};
use from_json::from_json_impl;
use hash::hash_impl;
use mint::Mint;
use ord::ord_impl;
use show::show_impl;
use to_json::to_json_impl;

/// The derive vocabulary: the family names a `#[derive(...)]` list accepts.
pub const RULED_SET: &str = "hash, show, ord, clone, to_json, from_json, default";

/// The hash family's fold constants: FNV-1a's offset basis and prime,
/// the same pair the builtin str/float/char hash natives hash under —
/// the basis exceeds i64's range, so the fold spells its 2^64 wrap as a
/// negated literal (the fold is mod-2^64 arithmetic either way).
const HASH_SEED: &str = "3750763034362895579";

const HASH_MIX: &str = "1099511628211";

/// The written type names the interpolation context prints itself;
/// every other part derives through a `show` dispatch.
const PRINTABLE: &[&str] = &["int", "float", "bool", "char", "string"];

/// Expands every derive attribute in the package: consumed attributes
/// are stripped and their impls appended beside the attributed item,
/// refused names keep the attribute and contribute a diagnostic. Files
/// without derive attributes come back byte-identical (a plain copy).
/// `asts` may hold shared trees (`Borrow<Ast>`): the query layer
/// expands its parse memos, copying once per tree.
#[must_use]
pub fn expand<T>(asts: &[T]) -> (Vec<std::sync::Arc<Ast>>, Vec<Diagnostic>)
where
    T: std::borrow::Borrow<Ast>,
{
    let trees: Vec<&Ast> = asts.iter().map(std::borrow::Borrow::borrow).collect();
    expand_trees(&trees)
}

/// Expands the package's caller-owned trees by moving each one
/// through: nothing clones while the caller holds the sole reference
/// (`Arc::make_mut` mutates in place; a tree shared past the caller —
/// a query memo's — clones once for the expansion, exactly the copy
/// [`expand`] pays), so the cold run path never pays a deep copy.
/// `preexpanded` marks, aligned with `asts`, the trees already
/// derive-expanded (the ambient blob's decode); a marked tree must
/// carry no derive attribute and passes through untouched — the mark
/// asserts the artifact held its end of the contract, it never gates
/// behavior, because expansion consumes derive attributes and a
/// second expansion over a pre-expanded tree is the identity either
/// way. Linear heads are collected over the whole package first, marked
/// trees included, so a user derive over a linear ambient part refuses
/// identically to the borrowed expansion.
#[must_use]
pub fn expand_owned(
    mut asts: Vec<std::sync::Arc<Ast>>,
    preexpanded: &[bool],
) -> (Vec<std::sync::Arc<Ast>>, Vec<Diagnostic>) {
    let trees: Vec<&Ast> = asts.iter().map(|ast| &**ast).collect();
    let linear = linear_heads(&trees);
    let mut diags = Vec::new();
    for (index, ast) in asts.iter_mut().enumerate() {
        if preexpanded.get(index).copied().unwrap_or(false) {
            debug_assert!(
                !has_expansion(ast),
                "a pre-expanded tree carries no expansion work"
            );
        } else if has_expansion(ast) {
            expand_tree_in_place(std::sync::Arc::make_mut(ast), &linear, &mut diags);
        }
    }
    (asts, diags)
}

/// The expansion over the collected trees; see [`expand`].
fn expand_trees(asts: &[&Ast]) -> (Vec<std::sync::Arc<Ast>>, Vec<Diagnostic>) {
    if !asts.iter().any(|ast| has_expansion(ast)) {
        let cloned: Vec<std::sync::Arc<Ast>> = asts
            .iter()
            .map(|ast| std::sync::Arc::new((*ast).clone()))
            .collect();
        return (cloned, Vec::new());
    }
    let linear = linear_heads(asts);
    let mut diags = Vec::new();
    let mut out = Vec::with_capacity(asts.len());
    for &ast in asts {
        if !has_expansion(ast) {
            out.push(std::sync::Arc::new(ast.clone()));
            continue;
        }
        let mut tree = ast.clone();
        expand_tree_in_place(&mut tree, &linear, &mut diags);
        out.push(std::sync::Arc::new(tree));
    }
    (out, diags)
}

/// The in-place expansion of one tree: consumed attributes stripped,
/// their impls appended beside the attributed item.
fn expand_tree_in_place(ast: &mut Ast, linear: &[String], diags: &mut Vec<Diagnostic>) {
    // The `#[default]` member annotations are declaration data of this
    // file's enums, so the context collects once per tree ahead of the
    // walk: a field key reads an annotation only when the annotated
    // enum is declared in the same file.
    let unions = UnionDefaults::collect(
        &ast.blocks
            .iter()
            .flat_map(|b| b.items.iter())
            .collect::<Vec<_>>(),
        diags,
    );
    for block in &mut ast.blocks {
        // Drain and rebuild: synthesized impls insert beside their
        // attributed item without index arithmetic, so a derive late
        // in the block is never shifted past the walk.
        let mut rebuilt = Vec::with_capacity(block.items.len());
        for item in block.items.drain(..) {
            let (extra, strip) = expand_item(&item, &unions, linear, diags);
            let mut item = item;
            if strip {
                item.attrs.retain(|a| a.name.name != "derive");
            }
            item.attrs.retain(|a| a.name.name != "default");
            rebuilt.push(item);
            rebuilt.extend(extra);
        }
        block.items = rebuilt;
    }
}

/// Whether any item of the file carries work the expansion owns: a
/// derive attribute, a `#[default]` member annotation on an enum case,
/// or a `#[default]` attribute on an item (which the expansion refuses
/// — the annotation belongs inside an enum).
fn has_expansion(ast: &Ast) -> bool {
    ast.blocks.iter().flat_map(|b| b.items.iter()).any(|i| {
        i.attrs.iter().any(|a| a.name.name == "derive" || a.name.name == "default")
            || matches!(&i.kind, ItemKind::Enum(decl) if decl.cases.iter().any(|c| c.default.is_some()))
    })
}

/// The names of struct/enum declarations whose supers carry a negated
/// constraint (the `~Copyable` linear layer): a part of such a type
/// refuses structural derivation.
fn linear_heads(asts: &[&Ast]) -> Vec<String> {
    let mut heads = Vec::new();
    for item in asts
        .iter()
        .flat_map(|a| a.blocks.iter())
        .flat_map(|b| b.items.iter())
    {
        let (name, supers) = match &item.kind {
            ItemKind::Struct(s) => (&s.name, &s.supers),
            ItemKind::Enum(e) => (&e.name, &e.supers),
            _ => continue,
        };
        if supers.iter().any(|t| matches!(t, Type::Negation { .. })) {
            heads.push(name.name.clone());
        }
    }
    heads
}

/// Expands one item's derive attributes. Returns the synthesized impl
/// items to insert after it and whether every derive argument was
/// consumed (the attribute is then stripped).
fn expand_item(
    item: &Item,
    unions: &UnionDefaults,
    linear: &[String],
    diags: &mut Vec<Diagnostic>,
) -> (Vec<Item>, bool) {
    let mut extra = Vec::new();
    let mut consumed = 0;
    let mut derive_attrs = 0;
    // One mint per attributed item, shared by every family expanding
    // it: two families minting from scratch would hand the same byte
    // range's spans to two different name uses, and the member fn
    // declarations mint here too, so two families' fns never share one
    // span.
    let mut mint = Mint::new(item);
    for attr in &item.attrs {
        if attr.name.name == "default" {
            diags.push(misplaced_default(attr));
            continue;
        }
        if attr.name.name != "derive" {
            continue;
        }
        derive_attrs += 1;
        let mut attr_consumed = true;
        for arg in &attr.args {
            let Some(name) = single_segment(arg) else {
                diags.push(unknown(arg.span, &arg.text()));
                attr_consumed = false;
                continue;
            };
            match name {
                "==" => {
                    diags.push(eq_refused(attr.span));
                    attr_consumed = false;
                },
                "show" => match show_impl(item, attr, &mut mint, linear, diags) {
                    Some(impl_item) => extra.push(impl_item),
                    None => attr_consumed = false,
                },
                "ord" => match ord_impl(item, attr, &mut mint, linear, diags) {
                    Some(impl_item) => extra.push(impl_item),
                    None => attr_consumed = false,
                },
                "hash" => match hash_impl(item, attr, &mut mint, linear, diags) {
                    Some(impl_item) => extra.push(impl_item),
                    None => attr_consumed = false,
                },
                "clone" => match clone_impl(item, attr, &mut mint, linear, diags) {
                    Some(impl_item) => extra.push(impl_item),
                    None => attr_consumed = false,
                },
                "default" => match default_impl(item, attr, &mut mint, unions, linear, diags) {
                    Some(impl_item) => extra.push(impl_item),
                    None => attr_consumed = false,
                },
                "from_json" => match from_json_impl(item, attr, &mut mint, linear, diags) {
                    Some(impl_item) => extra.push(impl_item),
                    None => attr_consumed = false,
                },
                "to_json" => match to_json_impl(item, attr, &mut mint, linear, diags) {
                    Some(impl_item) => extra.push(impl_item),
                    None => attr_consumed = false,
                },
                _ => {
                    diags.push(unknown(arg.span, name));
                    attr_consumed = false;
                },
            }
        }
        if attr_consumed {
            consumed += 1;
        }
    }
    (extra, consumed == derive_attrs && derive_attrs > 0)
}

/// The derive name: a single-segment path argument.
fn single_segment(path: &Path) -> Option<&str> {
    match path.segments.as_slice() {
        [only] => Some(only.name.as_str()),
        _ => None,
    }
}

fn unknown(span: Span, name: &str) -> Diagnostic {
    Diagnostic::error(
        ErrorCode::DeriveUnknown,
        format!("`{name}` is not a derive name [G1]"),
    )
    .with_primary(span, "not in the ruled set")
    .with_note(format!("the ruled set is: {RULED_SET}"))
}

/// A `#[default]` attribute on an item: the annotation belongs to an
/// enum case, the union member a default derivation reads.
fn misplaced_default(attr: &Attribute) -> Diagnostic {
    Diagnostic::error(
        ErrorCode::DeriveMemberAnnotation,
        "`#[default]` annotates an enum case, the union member a default derives over [G1]",
    )
    .with_primary(
        attr.span,
        "place the annotation on one case inside the enum",
    )
}

fn eq_refused(span: Span) -> Diagnostic {
    Diagnostic::error(
        ErrorCode::DeriveEqRefused,
        "`==` is universal structural equality and is never derived [B9]",
    )
    .with_primary(span, "remove `==` from the derive list")
    .with_note(
        "equality compares structurally over all data already; hash stays consistent with it",
    )
}

/// The first payloaded case of a generic enum: the pattern seam's
/// refusal site. A unit-only generic enum derives — its synthesized
/// arms resolve their atoms over every application of the head — while
/// a payloaded case's shape is a per-instantiation fact, so no spelling
/// resolves its atom ahead of any runtime question.
fn payloaded_case_of_generic(item: &Item) -> Option<&EnumCase> {
    let ItemKind::Enum(decl) = &item.kind else {
        return None;
    };
    (!decl.generics.is_empty())
        .then(|| {
            decl.cases
                .iter()
                .find(|case| !matches!(case.payload, EnumPayload::None))
        })
        .flatten()
}

/// A payloaded case over a generic head refuses the matching families:
/// the synthesized arms cannot check, since the case resolves no atom
/// in any spelling. A unit-only generic enum passes; the runtime seam
/// owns its refusal.
fn payloaded_refused(case: &EnumCase, family: &str) -> Diagnostic {
    Diagnostic::error(
        ErrorCode::DeriveGenericRefused,
        format!("derive `{family}` refuses a payloaded case over a generic head [G1]"),
    )
    .with_primary(
        case.span,
        "this case's pattern resolves no atom over the generic head",
    )
    .with_note(
        "the payload's shape is a per-instantiation fact; a unit-only generic enum derives, the \
         runtime seam owning its refusal",
    )
}

/// A generic enum refuses the constructing families: the synthesized
/// body constructs a case, and a constructed case types as its own
/// atom, never the applied head — the checker's seam, ahead of any
/// runtime question. A generic struct passes: its parts dispatch
/// through the added bound and the runtime seam owns the refusal.
fn generic_refused(span: Span, family: &str) -> Diagnostic {
    Diagnostic::error(
        ErrorCode::DeriveGenericRefused,
        format!("derive `{family}` refuses a generic enum [G1]"),
    )
    .with_primary(span, "this enum carries its own type parameters")
    .with_note(
        "the synthesized body constructs a case, and a constructed case types as its own atom, \
         never the applied head",
    )
}

/// A part (field or payload element) that refuses structural derivation.
fn part_refused(span: Span, family: &str, what: &str, part: &str, because: &str) -> Diagnostic {
    Diagnostic::error(
        ErrorCode::DerivePartRefused,
        format!("derive `{family}` refuses the {what} `{part}`: {because} [G1, B9]"),
    )
    .with_primary(span, format!("this part carries no structural {family}"))
}

/// The non-data refusal every family shares.
fn non_data_refused(attr: &Attribute) -> Diagnostic {
    Diagnostic::error(
        ErrorCode::DeriveNonDataItem,
        "a derive applies to a struct, enum, or tag declaration [G1]",
    )
    .with_primary(attr.span, "this declaration derives nothing")
}

#[cfg(test)]
mod tests {
    use ds_diag::FileId;

    use super::{Mint, Span};

    /// A length-1 item span is fully forbidden (the whole-span row
    /// `Mint::new` inserts), so the candidate space holds nothing: the
    /// search must degrade to the item's first byte rather than cycle
    /// its one rejected candidate forever. Real derives never mint over
    /// a declaration shorter than the derive's own spelling, so only
    /// this hand-built mint reaches the bound.
    #[test]
    fn a_fully_covered_item_span_degrades_instead_of_looping() {
        let mut mint = Mint {
            file: FileId::new(0),
            start: 7,
            next: 7,
            end: 7,
            len: 1,
            forbidden: [Span::new(FileId::new(0), 7, 8)].into_iter().collect(),
        };
        assert_eq!(mint.span(), Span::new(FileId::new(0), 7, 8));
        // a follow-up mint answers the same fallback, never a later byte
        assert_eq!(mint.span(), Span::new(FileId::new(0), 7, 8));
    }

    /// The ordinary mint walks the item's bytes in order, skipping the
    /// forbidden rows: the first free subspan answers and the next call
    /// continues from where the last one spent.
    #[test]
    fn the_mint_spends_free_spans_in_order() {
        let file = FileId::new(0);
        let mut mint = Mint {
            file,
            start: 3,
            next: 3,
            end: 9,
            len: 1,
            forbidden: [Span::new(file, 5, 6), Span::new(file, 7, 8)]
                .into_iter()
                .collect(),
        };
        assert_eq!(mint.span(), Span::new(file, 3, 4));
        assert_eq!(mint.span(), Span::new(file, 4, 5));
        assert_eq!(mint.span(), Span::new(file, 6, 7));
        assert_eq!(mint.span(), Span::new(file, 8, 9));
    }
}
