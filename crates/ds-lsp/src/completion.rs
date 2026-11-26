//! Completion's position slices beyond flat scope names: the member
//! surface after a dot, type-variable scopes in type positions, the
//! fixed declaration and expression skeletons behind a typed keyword
//! prefix, and prefix ranking over every answer.
//!
//! The member slice rides the checked expression-type table the hover
//! answers read: the receiver is the tightest expression whose span ends
//! exactly at the dot, and its members are the declaration tables' own
//! data (struct fields, enum cases, inherent impl members keyed by the
//! owner name, trait members through solved instances, UFCS free fns
//! whose first parameter names the receiver's type). Where no checked
//! type answers — no unpoisoned entry, or the receiver narrowed to the
//! empty type — the ordinary scope completion serves the position and
//! nothing member-specific is guessed.
//!
//! Type positions answer through the shared scope walk: every type
//! node is one, the generic-argument brackets of a constructor
//! application in expression position (`list[t]`) included, and the
//! answer lists the enclosing declarations' type variables (innermost
//! first, `Self` inside impl and trait member regions) ahead of the
//! type-namespace names the scope-index reports visible.
//!
//! The skeletons are the fixed shapes the grammar's own spellings make
//! unambiguous, and the grammar decides where each one parses, by probe:
//! the source with the typed prefix's span replaced by one probe token
//! re-parses on a scratch registry (the same pattern `fresh_name` uses
//! for name legality), a `tag` probe naming item level, a literal probe
//! naming an expression slot, a body-less `fn` probe naming an impl or
//! trait member list. Every offered expansion is therefore valid surface
//! at the position, and a document broken beyond the prefix answers no
//! snippet at all.

use ds_check::{Analyzed, DefKind, MethodOwner, Package, ScopeEntry, ScopeKind, Val};
use ds_diag::{FileId, Span};
use ds_parse::{Ast, Expr, Type};
use rustc_hash::FxHashSet;
use tower_lsp::lsp_types as lsp;

use crate::{
    convert,
    walk::{TypeScopes, Visit, Walk},
};

/// Whether one byte is an identifier character of the scan-back (ASCII
/// letters, digits, underscore, and every non-ASCII byte, so a multibyte
/// identifier never splits a prefix).
fn ident_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_' || byte >= 0x80
}

/// The byte offset where the identifier being typed at `at` starts: the
/// scan stops at the first non-identifier byte behind the position.
fn ident_start(source: &str, at: u32) -> u32 {
    let bytes = source.as_bytes();
    let mut start = (at as usize).min(bytes.len());
    while start > 0 && ident_byte(bytes[start - 1]) {
        start -= 1;
    }
    u32::try_from(start).expect("a source offset fits u32")
}

/// The typed prefix at one offset: the identifier characters immediately
/// before it, empty when the position sits behind no identifier.
pub(crate) fn typed_prefix(source: &str, at: u32) -> String {
    source[ident_start(source, at) as usize..at as usize].to_owned()
}

/// The dot's offset when the position is a member position — right after
/// the dot or inside the member name being typed after it — else `None`.
pub(crate) fn member_dot(source: &str, at: u32) -> Option<u32> {
    let bytes = source.as_bytes();
    let dot = ident_start(source, at).checked_sub(1)?;
    (bytes[dot as usize] == b'.').then_some(dot)
}

/// The receiver's checked value at one dot: the tightest unpoisoned
/// expression-type entry whose span ends exactly at the dot. `None` when
/// no entry answers or the solved type is empty (the checker narrowed it
/// away, or the state is an error) — the honest fallback's trigger.
pub(crate) fn receiver_val(analyzed: &Analyzed, file: FileId, dot: u32) -> Option<Val> {
    let mut best: Option<(u32, Val)> = None;
    for (span, val) in &analyzed.checked.expr_types {
        if span.file != file || span.end != dot || val.poisoned {
            continue;
        }
        if best.is_none_or(|(start, _)| span.start > start) {
            best = Some((span.start, *val));
        }
    }
    let (_, val) = best?;
    let empty = analyzed.decls.universe.members(val.ty).is_empty();
    (!empty).then_some(val)
}

/// The completion items of one checked receiver type: struct fields and
/// enum cases of every member atom (applications through their
/// constructor's declaration), the inherent impl members owned by those
/// atoms' type names, the members of every trait the type provably
/// implements through solved instances, and the UFCS free functions whose
/// first parameter's written type names the receiver — the dot spelling
/// of a free function's name.
pub(crate) fn members(analyzed: &Analyzed, recv: Val) -> Vec<lsp::CompletionItem> {
    let decls = &analyzed.decls;
    let universe = &decls.universe;
    let package = &analyzed.package;
    let mut out: Vec<lsp::CompletionItem> = Vec::new();
    // Dedup on (label, kind) by scan: one receiver's member list is small
    // and `CompletionItemKind` derives no `Hash`.
    let mut push = |label: &str, kind: lsp::CompletionItemKind| {
        if !out
            .iter()
            .any(|item: &lsp::CompletionItem| item.label == label && item.kind == Some(kind))
        {
            out.push(lsp::CompletionItem {
                label: label.to_owned(),
                kind: Some(kind),
                ..lsp::CompletionItem::default()
            });
        }
    };
    // The declaring def of every member atom: nominal atoms map directly,
    // applications through their constructor. The def names key the
    // inherent and UFCS tiers; the shapes key the fields and cases.
    let mut owner_names: Vec<&str> = Vec::new();
    for &atom in universe.members(recv.ty) {
        let Ok(decl) = universe.atom_decl(atom) else {
            continue;
        };
        let def = match &decl.kind {
            ds_check::AtomKind::Apply(ctor, _) => decls.defs_of_atoms.get(ctor).copied(),
            _ => decls.defs_of_atoms.get(&atom).copied(),
        };
        let Some(def) = def else {
            continue;
        };
        let owner = &package.def(def);
        match owner.kind {
            DefKind::Struct => {
                if let Some(shape) = decls.structs.get(&def) {
                    for field in &shape.fields {
                        push(&field.name.name, lsp::CompletionItemKind::FIELD);
                    }
                }
            },
            DefKind::Enum => {
                if let Some(shape) = decls.enums.get(&def) {
                    for (name, ..) in &shape.cases {
                        push(name, lsp::CompletionItemKind::ENUM_MEMBER);
                    }
                }
            },
            // Tags carry no shape; traits and aliases are not receivers.
            _ => {},
        }
        owner_names.push(owner.name.as_str());
    }
    // Inherent impl members: owned by the target type's name.
    for method in package.methods() {
        if let MethodOwner::Inherent(owner) = &method.owner
            && owner_names.iter().any(|name| name == owner)
        {
            push(&method.name, lsp::CompletionItemKind::METHOD);
        }
    }
    // Trait members through solved instances: every trait whose instance
    // set contains the receiver's type.
    for (raw, def) in package.defs().enumerate() {
        if def.kind != DefKind::Trait {
            continue;
        }
        let Ok(id) = u32::try_from(raw) else {
            break;
        };
        let id = ds_check::DefId::from_raw(id);
        let Some(trait_id) = decls.traits.get(&id) else {
            continue;
        };
        if decls
            .instances
            .resolve(universe, *trait_id, recv.ty)
            .is_empty()
        {
            continue;
        }
        if let Some(names) = decls.trait_members.get(&id) {
            for name in names {
                push(name, lsp::CompletionItemKind::METHOD);
            }
        }
    }
    // UFCS candidates: free fns whose first parameter's written type is a
    // path naming the receiver's type — the head alone, no argument
    // unification (a generic `list[t]` parameter matches a `list[int]`
    // receiver through its constructor's name).
    for (id, sig) in &decls.fns {
        let Some(first) = sig.params.first() else {
            continue;
        };
        let Some(Type::Path { path, .. }) = &first.ty else {
            continue;
        };
        let [only] = path.segments.as_slice() else {
            continue;
        };
        if owner_names.iter().any(|name| *name == only.name) {
            push(&package.def(*id).name, lsp::CompletionItemKind::FUNCTION);
        }
    }
    out
}

/// The type-variable answer at one offset: `Some` when the position sits
/// inside a type node (or a constructor application's argument brackets
/// in expression position), carrying the in-scope type variables — every
/// enclosing declaration's own, innermost first — and whether `Self` is
/// bound (impl and trait member regions).
pub(crate) struct TypeVars {
    /// In-scope type variables with their defining spans, innermost
    /// declaration first.
    pub names: Vec<(String, Span)>,
    /// Whether `Self` names the enclosing self type here.
    pub self_in_scope: bool,
}

/// Walks the file's parse tree over the shared scope walk and answers
/// the type variables in scope at one offset, `None` when the offset
/// sits in no type position.
pub(crate) fn type_vars(ast: &Ast, package: &Package, file: FileId, at: u32) -> Option<TypeVars> {
    let mut visitor = TypePos {
        package,
        file,
        at,
        found: None,
    };
    Walk::new(&mut visitor).root(ast);
    visitor.found
}

/// The completion visitor over the shared walk: a type position is the
/// first covering node the walk enters, carrying the scope snapshot it
/// entered with.
struct TypePos<'a> {
    package: &'a Package,
    file: FileId,
    at: u32,
    found: Option<TypeVars>,
}

impl TypePos<'_> {
    /// Marks one span as a type position when it covers the offset.
    fn mark(&mut self, span: Span, scopes: &TypeScopes) {
        if self.found.is_none()
            && span.file == self.file
            && span.start <= self.at
            && self.at <= span.end
        {
            self.found = Some(TypeVars {
                names: scopes.vars.iter().rev().cloned().collect(),
                self_in_scope: scopes.self_bound,
            });
        }
    }
}

impl Visit for TypePos<'_> {
    fn package(&self) -> &Package {
        self.package
    }

    /// Every type node is a type position.
    fn at_type(&mut self, ty: &Type, scopes: &TypeScopes) {
        self.mark(ty.span(), scopes);
    }

    /// A bracketed constructor argument is a type position, whatever its
    /// inner shape.
    fn at_ctor_arg(&mut self, arg: &Expr, scopes: &TypeScopes) {
        self.mark(arg.span(), scopes);
    }
}

/// Whether one scope kind declares into the type namespace: the
/// type-position filter over the scope-index's answer.
pub(crate) fn type_namespace(kind: ScopeKind) -> bool {
    matches!(
        kind,
        ScopeKind::Struct
            | ScopeKind::Enum
            | ScopeKind::Tag
            | ScopeKind::Case
            | ScopeKind::Trait
            | ScopeKind::TypeAlias
    )
}

/// The items of one type position: the in-scope type variables (`Self`
/// included when bound) ahead of the type-namespace names visible per
/// the scope-index's rules, a variable shadowing the package names of
/// its spelling.
pub(crate) fn type_items(vars: &TypeVars, entries: &[ScopeEntry]) -> Vec<lsp::CompletionItem> {
    let mut out = Vec::new();
    let mut shadowed: FxHashSet<&str> = vars.names.iter().map(|(name, _)| name.as_str()).collect();
    for (name, _) in &vars.names {
        out.push(lsp::CompletionItem {
            label: name.clone(),
            kind: Some(lsp::CompletionItemKind::TYPE_PARAMETER),
            ..lsp::CompletionItem::default()
        });
    }
    if vars.self_in_scope {
        shadowed.insert("Self");
        out.push(lsp::CompletionItem {
            label: "Self".to_owned(),
            kind: Some(lsp::CompletionItemKind::TYPE_PARAMETER),
            ..lsp::CompletionItem::default()
        });
    }
    for entry in entries {
        if !type_namespace(entry.kind) || shadowed.contains(entry.name.as_str()) {
            continue;
        }
        out.push(lsp::CompletionItem {
            label: entry.name.clone(),
            kind: Some(convert::completion_kind(entry.kind)),
            ..lsp::CompletionItem::default()
        });
    }
    out
}

/// Where one skeleton's expansion is valid surface: the probe parses'
/// verdict on the position the typed prefix sits at.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Site {
    /// Top level, where every declaration skeleton parses.
    Item,
    /// An impl or trait member list, where the fn skeleton alone parses.
    Member,
    /// Inside a block, where the expression skeletons parse as statements.
    Body,
}

/// One fixed skeleton the grammar's spellings make unambiguous: the site
/// its expansion belongs to, the label the typed prefix filters on, and
/// the expansion in LSP snippet syntax — every placeholder's default a
/// valid spelling of its slot, so the placeholder-resolved expansion
/// parses as-is (the test suite's validity gate).
pub(crate) struct Snippet {
    /// The site whose skeletons include this one.
    pub(crate) site: Site,
    /// The keyword (or shape) label the typed prefix filters on.
    pub(crate) label: &'static str,
    /// The expansion: tab stops `$0`/`$n`, placeholders `${n:default}`.
    pub(crate) text: &'static str,
}

/// The shipped skeletons. Indentation is the canonical two spaces, so an
/// accepted expansion is already the formatter's own form.
pub(crate) const SNIPPETS: &[Snippet] = &[
    // Item level: the declaration skeletons.
    Snippet {
        site: Site::Item,
        label: "fn",
        text: "fn ${1:name}(${2:p}) -> ${3:t} {\n  $0\n}",
    },
    Snippet {
        site: Site::Item,
        label: "struct",
        text: "struct ${1:name} {\n  ${2:field}: ${3:t},\n}",
    },
    Snippet {
        site: Site::Item,
        label: "enum",
        text: "enum ${1:name} {\n  ${2:case},\n}",
    },
    Snippet {
        site: Site::Item,
        label: "impl",
        text: "impl ${1:target} {\n  fn ${2:member}(self) -> ${3:t} {\n    $0\n  }\n}",
    },
    Snippet {
        site: Site::Item,
        label: "trait",
        text: "trait ${1:name} {\n  fn ${2:member}(self) -> ${3:t}\n}",
    },
    // Member level: the fn skeleton alone (impl and trait member lists).
    Snippet {
        site: Site::Member,
        label: "fn",
        text: "fn ${1:name}(${2:p}) -> ${3:t} {\n  $0\n}",
    },
    // Body level: the expression skeletons.
    Snippet {
        site: Site::Body,
        label: "lambda",
        text: "(${1:x}) => ${2:expr}",
    },
    Snippet {
        site: Site::Body,
        label: "if",
        text: "if ${1:cond} {\n  $0\n}",
    },
    Snippet {
        site: Site::Body,
        label: "while",
        text: "while ${1:cond} {\n  $0\n}",
    },
    Snippet {
        site: Site::Body,
        label: "match",
        text: "match ${1:expr} {\n  ${2:pat} => ${3:expr},\n}",
    },
    Snippet {
        site: Site::Body,
        label: "nursery",
        text: "nursery {\n  $0\n}",
    },
    Snippet {
        site: Site::Body,
        label: "spawn",
        text: "spawn {\n  $0\n}",
    },
];

/// Whether the whole source parses with the typed prefix's span replaced
/// by one probe token, on a scratch registry: the grammar itself decides
/// the position exactly as it would decide in source.
fn parses_with(source: &str, start: u32, at: u32, probe: &str) -> bool {
    let mut text = String::with_capacity(source.len() + probe.len());
    text.push_str(&source[..start as usize]);
    text.push_str(probe);
    text.push_str(&source[at as usize..]);
    let mut files = ds_diag::Files::new();
    let file = files.add_source("probe.ds", &text);
    ds_parse::parse(&files, file).is_ok()
}

/// The site of one position, by probe parses over the source with the
/// typed prefix replaced: the `tag` probe (an item spelling no other
/// position admits) names item level, the literal probe names an
/// expression slot, the body-less `fn` probe names an impl or trait
/// member list — three places no probe shares with another. `None` when
/// no probe parses: a document broken beyond the prefix answers no site.
fn site(source: &str, start: u32, at: u32) -> Option<Site> {
    if parses_with(source, start, at, "tag zz") {
        Some(Site::Item)
    } else if parses_with(source, start, at, "1") {
        Some(Site::Body)
    } else if parses_with(source, start, at, "fn zz()") {
        Some(Site::Member)
    } else {
        None
    }
}

/// The snippet items at one position: nothing until the typed prefix
/// names a label (a non-empty prefix of some snippet's), then the
/// probe-decided site picks the skeletons whose expansion parses there.
/// No probe runs when no label matches.
pub(crate) fn snippets(source: &str, at: u32, prefix: &str) -> Vec<lsp::CompletionItem> {
    if prefix.is_empty() || !SNIPPETS.iter().any(|s| s.label.starts_with(prefix)) {
        return Vec::new();
    }
    let start = ident_start(source, at);
    let Some(site) = site(source, start, at) else {
        return Vec::new();
    };
    SNIPPETS
        .iter()
        .filter(|s| s.site == site && s.label.starts_with(prefix))
        .map(|s| lsp::CompletionItem {
            label: s.label.to_owned(),
            kind: Some(lsp::CompletionItemKind::SNIPPET),
            insert_text: Some(s.text.to_owned()),
            insert_text_format: Some(lsp::InsertTextFormat::SNIPPET),
            ..lsp::CompletionItem::default()
        })
        .collect()
}

/// The rank of one label against the typed prefix: an exact prefix
/// first, a word-boundary match next, a plain substring after, and the
/// rest last. Case-sensitive — identifiers are.
fn match_rank(label: &str, prefix: &str) -> u8 {
    if label.starts_with(prefix) {
        return 0;
    }
    let label_bytes = label.as_bytes();
    let mut at = 0;
    while let Some(found) = label[at..].find(prefix) {
        let boundary = at + found;
        let word_boundary = boundary > 0
            && (label_bytes[boundary - 1] == b'_'
                || (label_bytes[boundary - 1].is_ascii_lowercase()
                    && label_bytes[boundary].is_ascii_uppercase()));
        if word_boundary {
            return 1;
        }
        at = boundary + 1;
    }
    if label.contains(prefix) { 2 } else { 3 }
}

/// Ranks one completion answer by prefix match, stable within each group
/// by the existing order; an empty prefix keeps the answer unchanged.
pub(crate) fn rank(items: Vec<lsp::CompletionItem>, prefix: &str) -> Vec<lsp::CompletionItem> {
    if prefix.is_empty() {
        return items;
    }
    let mut ranked: Vec<(u8, lsp::CompletionItem)> = items
        .into_iter()
        .map(|item| (match_rank(&item.label, prefix), item))
        .collect();
    ranked.sort_by_key(|(rank, _)| *rank);
    ranked.into_iter().map(|(_, item)| item).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A constructor application's argument brackets are a type position
    /// through the shared type-head predicate: the cursor inside
    /// `box[t]` answers the enclosing declaration's type variable, while
    /// the same brackets over a name the package never declared as a type
    /// stay an ordinary expression position.
    #[test]
    fn constructor_argument_brackets_answer_type_variables() {
        let source =
            "struct box { v: int }\nfn wrap[t](v: t) -> t? {\n  const xs = box[t]()\n  none\n}\n";
        let mut files = ds_diag::Files::new();
        let file = files.add_source("probe.ds", source);
        let ast = ds_parse::parse(&files, file).expect("the probe parses");
        let (package, ..) = ds_check::resolve_package(std::slice::from_ref(&ast));
        let at = u32::try_from(
            source.find("box[t]").expect("the application is placed") + "box[".len() + 1,
        )
        .expect("an offset fits u32");
        let vars = type_vars(&ast, &package, file, at).expect("the brackets are a type position");
        assert_eq!(vars.names.first().map(|(name, _)| name.as_str()), Some("t"));
        assert!(!vars.self_in_scope);
        // The same brackets over an undeclared head mark nothing: the
        // walk answers no type position for an ordinary index.
        let plain = source.replace("box[t]", "xs[t]");
        let file = files.add_source("plain.ds", &plain);
        let ast = ds_parse::parse(&files, file).expect("the plain probe parses");
        let (package, ..) = ds_check::resolve_package(std::slice::from_ref(&ast));
        let at = u32::try_from(plain.find("xs[t]").expect("the index is placed") + "xs[".len() + 1)
            .expect("an offset fits u32");
        assert!(
            type_vars(&ast, &package, file, at).is_none(),
            "an undeclared head's brackets stay an expression position"
        );
    }

    /// A match arm's binding annotation is a type position (the shared
    /// walk carries the resolver's own arm shape): the cursor inside one
    /// answers the enclosing declaration's type variable.
    #[test]
    fn match_arm_annotations_answer_type_variables() {
        let source = "fn pick[t](v: t) -> t {\n  match v {\n    b: t? => b,\n  }\n}\n";
        let mut files = ds_diag::Files::new();
        let file = files.add_source("probe.ds", source);
        let ast = ds_parse::parse(&files, file).expect("the probe parses");
        let (package, ..) = ds_check::resolve_package(std::slice::from_ref(&ast));
        let at = u32::try_from(source.find("t?").expect("the annotation is placed") + 1)
            .expect("an offset fits u32");
        let vars = type_vars(&ast, &package, file, at).expect("the annotation is a type position");
        assert_eq!(vars.names.first().map(|(name, _)| name.as_str()), Some("t"));
    }

    /// Resolves one snippet's syntax to plain surface: every placeholder
    /// keeps its default spelling, bare tab stops drop out.
    fn resolve(snippet: &str) -> String {
        let mut out = String::new();
        let mut rest = snippet;
        while let Some(dollar) = rest.find('$') {
            out.push_str(&rest[..dollar]);
            let after = &rest[dollar + 1..];
            if let Some(inner) = after.strip_prefix('{') {
                let close = inner.find('}').expect("the placeholder closes");
                let (_, default) = inner[..close]
                    .split_once(':')
                    .expect("a placeholder carries its default");
                out.push_str(default);
                rest = &inner[close + 1..];
            } else {
                let stop = after.chars().next().expect("a tab stop follows $");
                rest = &after[stop.len_utf8()..];
            }
        }
        out.push_str(rest);
        out
    }

    /// Every snippet's expansion, its placeholders resolved to their
    /// default spellings, parses as surface at its site: item and member
    /// skeletons as whole sources, body skeletons as one statement inside
    /// a probe fn — the validity gate for shipping a shape.
    #[test]
    fn every_snippet_expansion_parses_at_its_site() {
        for snippet in SNIPPETS {
            let resolved = format!("{}\n", resolve(snippet.text));
            let source = match snippet.site {
                Site::Item | Site::Member => resolved,
                Site::Body => format!("fn probe() {{\n  {resolved}\n}}\n"),
            };
            let mut files = ds_diag::Files::new();
            let file = files.add_source("probe.ds", &source);
            ds_parse::parse(&files, file).unwrap_or_else(|diags| {
                panic!(
                    "the `{}` skeleton's expansion stays surface: {diags:?}",
                    snippet.label
                )
            });
        }
    }

    /// The probe parses separate the three positions, and a document
    /// broken beyond the typed prefix answers no site at all.
    #[test]
    fn probe_sites_separate_items_members_and_bodies() {
        let site_of = |source: &str, prefix: &str| {
            // The typed prefix is the last occurrence (an earlier line can
            // embed the same letters inside a keyword, like `st` in
            // `const`).
            let found = source.rfind(prefix).expect("the prefix is placed");
            let at = u32::try_from(found + prefix.len()).expect("an offset fits u32");
            site(source, ident_start(source, at), at)
        };
        assert_eq!(site_of("const one = 1\nst\n", "st"), Some(Site::Item));
        assert_eq!(site_of("fn probe() {\n  if\n}\n", "if"), Some(Site::Body));
        assert_eq!(site_of("impl point {\n  fn\n}\n", "fn"), Some(Site::Member));
        assert_eq!(
            site_of("trait pretty {\n  fn\n}\n", "fn"),
            Some(Site::Member),
            "a trait member list is the fn skeleton's site too"
        );
        // An expression slot mid-statement is the body site (the skeleton
        // is an expression there): `const g = ` names one.
        assert_eq!(
            site_of("const g = if\n", "if"),
            Some(Site::Body),
            "an expression slot answers the body site"
        );
        assert_eq!(
            site_of("st\nst\n", "st"),
            None,
            "a document broken beyond the prefix answers no site"
        );
    }

    #[test]
    fn member_positions_and_prefixes_scan_back() {
        let source = "box.field_name";
        // Right after the dot: an empty prefix and the dot behind it.
        assert_eq!(typed_prefix(source, 4), "");
        assert_eq!(member_dot(source, 4), Some(3));
        // Inside the member name: the typed prefix and the same dot.
        assert_eq!(typed_prefix(source, 7), "fie");
        assert_eq!(member_dot(source, 7), Some(3));
        // A position behind no dot names no member position.
        assert_eq!(member_dot(source, 2), None);
        assert_eq!(member_dot("a + b", 5), None);
    }

    #[test]
    fn ranking_groups_prefix_boundary_substring_rest() {
        let items = |labels: &[&str]| {
            labels
                .iter()
                .map(|label| lsp::CompletionItem {
                    label: (*label).to_owned(),
                    ..lsp::CompletionItem::default()
                })
                .collect::<Vec<_>>()
        };
        // Prefix first, then the word-boundary match, then the substring,
        // then the rest; each group keeps the incoming order.
        let ranked = rank(items(&["other", "extra", "my_trim", "trim"]), "tr");
        let labels: Vec<&str> = ranked.iter().map(|item| item.label.as_str()).collect();
        assert_eq!(labels, ["trim", "my_trim", "extra", "other"]);
        // A camel-case boundary is a word boundary, and matching stays
        // case-sensitive: `Tr` finds `myTrim` at its boundary while the
        // lowercase `trim` lands in the rest.
        let ranked = rank(items(&["other", "myTrim", "trim"]), "Tr");
        let labels: Vec<&str> = ranked.iter().map(|item| item.label.as_str()).collect();
        assert_eq!(labels, ["myTrim", "other", "trim"]);
        // An empty prefix keeps the answer's own order.
        let ranked = rank(items(&["b", "a"]), "");
        let labels: Vec<&str> = ranked.iter().map(|item| item.label.as_str()).collect();
        assert_eq!(labels, ["b", "a"]);
    }
}
