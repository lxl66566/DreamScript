//! The feature answers over the session: the position queries and the
//! document-wide ones, each reading the engine's memoized queries through the
//! session's database, with the helpers the position answers share.

use std::path::Path;

use ds_check::{DefId as PackageDef, DotRes, MethodId, MethodOwner, Res};
use ds_diag::{FileId, Span};
use ds_query::{
    Analyzed, BlockIndex, DefId, EffectsOf, PackageEffects, PackageId, ParseFile, QueryError,
    Resolve, ScopeIndex,
};
use rustc_hash::FxHashSet;
use tower_lsp::lsp_types as lsp;

use crate::{HoverAnswer, RenameAnswer, Session, Target, autoimport, completion, convert, tokens};

/// The member `fn` declaration of one registered method: the trait
/// member or impl function whose span holds the method's registered name
/// span, the declaration the hover tier renders.
fn member_decl<'a>(item: &'a ds_parse::Item, name: Span) -> Option<&'a ds_parse::FnDecl> {
    let mut fns: Vec<&'a ds_parse::FnDecl> = Vec::new();
    match &item.kind {
        ds_parse::ItemKind::Trait(t) => {
            for member in &t.members {
                if let ds_parse::TraitMember::Fn(fd) = member {
                    fns.push(fd);
                }
            }
        },
        ds_parse::ItemKind::Impl(i) => fns.extend(i.fns.iter()),
        _ => {},
    }
    fns.into_iter()
        .find(|fd| fd.span.start <= name.start && name.end <= fd.span.end)
}

/// Whether `name` spells a fresh binding name: the grammar itself decides
/// (a probe parse of one binding against a scratch registry, so keywords
/// and malformed spellings fail exactly as they would in source), and the
/// reserved language names stay out.
fn fresh_name(name: &str) -> bool {
    if name == "self" || name == "Self" || name == "none" {
        return false;
    }
    let mut files = ds_diag::Files::new();
    let file = files.add_source("probe.ds", &format!("const {name} = 1\n"));
    ds_parse::parse(&files, file).is_ok()
}

/// The word covering the byte offset (an identifier's contiguous
/// alphanumeric-and-underscore run), when the offset sits inside one or
/// just past its last character. Word characters are ASCII, so the run's
/// ends are always char boundaries.
fn word_at(src: &str, offset: u32) -> Option<(String, u32, u32)> {
    let bytes = src.as_bytes();
    let mut at = (offset as usize).min(bytes.len());
    let is_word = |byte: u8| byte.is_ascii_alphanumeric() || byte == b'_';
    // a cursor just past the word's last character still names it
    if at == bytes.len() || !is_word(bytes[at]) {
        at = at.checked_sub(1)?;
    }
    if at >= bytes.len() || !is_word(bytes[at]) {
        return None;
    }
    let mut start = at;
    while start > 0 && is_word(bytes[start - 1]) {
        start -= 1;
    }
    let mut end = at + 1;
    while end < bytes.len() && is_word(bytes[end]) {
        end += 1;
    }
    Some((
        src.get(start..end)?.to_owned(),
        u32::try_from(start).expect("the offset came from a u32"),
        u32::try_from(end).expect("the offset came from a u32"),
    ))
}

/// The link name one import declaration links under: an
/// at-sigil path's package stem with the sigil, a bare path whole — the
/// one rule the dep-list writer, the link positions, and the surface
/// builder all derive from.
fn link_name(import: &ds_parse::ImportDecl) -> String {
    let mut name = String::new();
    if import.path.at_sign {
        name.push('@');
        name.push_str(&import.path.segments[0].name);
    } else {
        for (at, segment) in import.path.segments.iter().enumerate() {
            if at > 0 {
                name.push('/');
            }
            name.push_str(&segment.name);
        }
    }
    name
}

impl Session {
    /// The hover answer at the position: the checked type of the covering
    /// expression, rendered through the analysis universe, with its
    /// effect row appended (the enclosing definition's row when no
    /// recorded expression covers the position), preceded by the
    /// declaration tier — the signature spelling and the `///` doc run
    /// of the `pub` package declaration the position names, or of the
    /// registered member its use selects — the same line `ds info`
    /// prints for that declaration. A name one of the package's imports
    /// selects that resolution could not bind answers the linked tier's
    /// line with its doc run when the embedder serves that package's
    /// cached `.dsi`.
    pub(super) fn hover(
        &mut self,
        path: &Path,
        file: FileId,
        line: u32,
        character: u32,
    ) -> Result<Option<HoverAnswer>, QueryError> {
        let offset = convert::offset(self.db.files(), file, lsp::Position { line, character });
        let analyzed = self.db.get::<Analyzed>(PackageId::new(0))?.analyzed;
        // The smallest covering expression span answers: spans nest, so
        // the tightest one is the expression the cursor sits in.
        let mut best: Option<(u32, u32)> = None;
        let mut best_val = None;
        for (span, val) in &analyzed.checked.expr_types {
            if span.file != file {
                continue;
            }
            let covers = if span.start == span.end {
                offset == span.start
            } else {
                span.start <= offset && offset < span.end
            };
            if covers && best.is_none_or(|(start, end)| span.end - span.start < end - start) {
                best = Some((span.start, span.end));
                best_val = Some(val);
            }
        }
        // The checked-expression half, unchanged in shape: the rendered
        // type plus the row (the expression's own, the enclosing
        // definition's the fallback).
        let mut range = None;
        let mut contents = String::new();
        if let Some(val) = best_val {
            let universe = &analyzed.decls.universe;
            contents = format!("```dreamscript\n{}\n```", universe.render(val.ty));
            // The effect row prefers the tightest covering expression's own
            // row (the effects pass's per-span table, the same covering
            // predicate as the type lookup above); the enclosing definition's
            // row is the fallback when no recorded span covers the position.
            let effects = self.db.get::<PackageEffects>(PackageId::new(0))?;
            if let Some(row) = effects.effects.row_at(file, offset) {
                contents = format!("{contents}\n~ {}", row.render());
            } else {
                let index = self.db.get::<BlockIndex>(file)?;
                let block = index
                    .blocks
                    .iter()
                    .find(|b| b.span.start <= offset && offset <= b.span.end);
                if let Some(block) = block {
                    for raw in 0..analyzed.package.len() {
                        let Ok(raw) = u32::try_from(raw) else { break };
                        let def = analyzed.package.def(PackageDef::from_raw(raw));
                        if def.span.file == file
                            && block.span.start <= def.span.start
                            && def.span.end <= block.span.end
                        {
                            if let Ok(row) = self.db.get::<EffectsOf>(DefId::new(raw)) {
                                contents = format!("{contents}\n~ {}", row.render());
                            }
                            break;
                        }
                    }
                }
            }
            let (start, end) = best.expect("a covering span exists with a value");
            range = Some(convert::range(self.db.files(), Span::new(file, start, end)));
        }
        // The declaration tier: a name resolving to a `pub` package
        // declaration — or to a registered member — answers its
        // signature spelling with its `///` doc run first; a name one of
        // the package's imports selects — the import-bound def the
        // session's package-free resolution left unknown — answers the
        // linked tier's line instead, never displacing a resolved name.
        let linked = match self.surface(file, offset)? {
            Some(found) => Some(found),
            None => self.linked_surface(path, file, offset),
        };
        if let Some((signature, doc, name_range)) = linked {
            let mut block = format!("```dreamscript\n{signature}\n```");
            if !doc.is_empty() {
                block.push_str("\n\n");
                block.push_str(&doc.join("\n"));
            }
            contents = if contents.is_empty() { block } else { format!("{block}\n\n{contents}") };
            range = range.or(Some(name_range));
        }
        match range {
            Some(range) => Ok(Some(HoverAnswer { contents, range })),
            // Neither a checked expression, a package-declared name, nor
            // an import-selected one: no answer.
            None => Ok(None),
        }
    }

    /// The linked-name tier: a word one of the open documents' imports
    /// selects — the import-bound def the session's package-free
    /// resolution left unknown — answers its `.dsi` signature line with
    /// its `///` doc run from the embedder's surface provider, both read
    /// off the cached artifact the provider serves, covering the word's
    /// range. A selection's alias binds the name the way the checker's
    /// link pass would, so both spellings serve; a word nothing selects
    /// answers nothing, exactly as before.
    fn linked_surface(
        &mut self,
        path: &Path,
        file: FileId,
        offset: u32,
    ) -> Option<(String, Vec<String>, lsp::Range)> {
        let (word, start, end) = word_at(self.db.files().source(file), offset)?;
        // every open document's imports: the selection may sit in another
        // member of the package (the cross-document hover precedent)
        let mut hits = Vec::new();
        for doc in self.docs.values() {
            let Ok(parse) = self.db.get::<ParseFile>(doc.file) else {
                continue;
            };
            let Some(ast) = &parse.ast else {
                continue;
            };
            for import in ast
                .blocks
                .iter()
                .flat_map(|block| block.items.iter())
                .filter_map(|item| match &item.kind {
                    ds_parse::ItemKind::Import(import) => Some(import),
                    _ => None,
                })
            {
                for selection in &import.select {
                    let binds = selection
                        .alias
                        .as_ref()
                        .unwrap_or(&selection.name)
                        .name
                        .as_str();
                    if binds == word {
                        hits.push((link_name(import), selection.name.name.clone()));
                    }
                }
            }
        }
        let (package, selected) = hits.first()?;
        let surface = (self.linked)(path, package)?;
        let line = surface.signatures.get(selected)?.clone();
        let doc = surface.docs.get(selected).cloned().unwrap_or_default();
        Some((
            line,
            doc,
            convert::range(self.db.files(), Span::new(file, start, end)),
        ))
    }

    /// The declaration tier of the name at the position: the resolution's
    /// target when the position names a use, the declaration itself when
    /// it names the definition. A `pub` package-level definition answers
    /// its signature spelling and its `///` doc run (locals and non-
    /// exported declarations stay out, `pub(pkg)` included); a registered
    /// member — a trait's member fn, an inherent impl's function — answers
    /// the same pair over its own `fn` declaration, the callable surface
    /// the call sites select. The renderer is the one `ds info` prints,
    /// one declaration's signature over its own parse tree.
    fn surface(
        &mut self,
        file: FileId,
        offset: u32,
    ) -> Result<Option<(String, Vec<String>, lsp::Range)>, QueryError> {
        let Some((name_span, target)) = self.target_at(file, offset)? else {
            return Ok(None);
        };
        let answer = match target {
            Target::Def(def) => {
                let resolved = self.db.get::<Resolve>(PackageId::new(0))?;
                let decl = resolved.package.def(def);
                let parse = self.db.get::<ParseFile>(decl.span.file)?;
                let Some(ast) = &parse.ast else {
                    return Ok(None);
                };
                // Top-level items never nest, so the item whose span holds
                // the defining name occurrence is the declaration.
                let Some(item) =
                    ast.blocks
                        .iter()
                        .flat_map(|block| block.items.iter())
                        .find(|item| {
                            item.span.start <= decl.span.start && decl.span.end <= item.span.end
                        })
                else {
                    return Ok(None);
                };
                if !matches!(item.vis, Some(ds_parse::Vis::Pub(_))) {
                    return Ok(None);
                }
                let src = self.db.files().source(item.span.file);
                let mut line = String::new();
                ds_fmt::signature::item(&mut line, src, item);
                if line.trim_end().is_empty() {
                    return Ok(None);
                }
                let doc = item
                    .doc
                    .iter()
                    .map(|t| ds_parse::doc_body(src, t).to_owned())
                    .collect();
                Some((line.trim_end().to_owned(), doc))
            },
            Target::Method(id) => {
                let resolved = self.db.get::<Resolve>(PackageId::new(0))?;
                let method = resolved.package.method(id);
                let parse = self.db.get::<ParseFile>(method.span.file)?;
                let Some(ast) = &parse.ast else {
                    return Ok(None);
                };
                let Some(item) =
                    ast.blocks
                        .iter()
                        .flat_map(|block| block.items.iter())
                        .find(|item| {
                            item.span.start <= method.span.start && method.span.end <= item.span.end
                        })
                else {
                    return Ok(None);
                };
                let src = self.db.files().source(method.span.file);
                let Some(fd) = member_decl(item, method.span) else {
                    return Ok(None);
                };
                let mut line = String::new();
                ds_fmt::signature::member_fn(&mut line, src, fd);
                if line.trim_end().is_empty() {
                    return Ok(None);
                }
                let doc = fd
                    .doc
                    .iter()
                    .map(|t| ds_parse::doc_body(src, t).to_owned())
                    .collect();
                Some((line.trim_end().to_owned(), doc))
            },
            _ => None,
        };
        Ok(answer.map(|(line, doc)| (line, doc, convert::range(self.db.files(), name_span))))
    }

    /// The definitions of the name use at the position, through the
    /// resolution side table's covering lookup.
    pub(super) fn definition(
        &mut self,
        file: FileId,
        line: u32,
        character: u32,
    ) -> Result<Vec<lsp::Location>, QueryError> {
        let offset = convert::offset(self.db.files(), file, lsp::Position { line, character });
        let resolved = self.db.get::<Resolve>(PackageId::new(0))?;
        let package = &resolved.package;
        let Some(index) = package.file_position(file) else {
            return Ok(Vec::new());
        };
        let Some(res) = resolved.resolutions[index].enclosing(Span::new(file, offset, offset))
        else {
            return Ok(Vec::new());
        };
        let targets = match res {
            Res::Def(id) => vec![package.def(*id).span],
            Res::Local { bind } => vec![*bind],
            Res::TraitMethod(id) => vec![package.method(*id).span],
            Res::Dot(DotRes::Free { func, .. } | DotRes::Module { func }) => {
                vec![package.def(*func).span]
            },
            Res::Dot(DotRes::Methods { candidates } | DotRes::Static { candidates }) => candidates
                .iter()
                .map(|id| package.method(*id).span)
                .collect(),
            // A field call's name is the receiver's field label, a
            // declaration the value-namespace tables never hold. The
            // handler forms and a bare module alias name no definition
            // behind them (the alias's members are the cross-package
            // scope round's).
            Res::Dot(DotRes::Field) | Res::Handler | Res::Module(_) => Vec::new(),
        };
        let files = self.db.files();
        Ok(targets
            .into_iter()
            .filter_map(|span| {
                self.uri(span.file).map(|uri| lsp::Location {
                    uri,
                    range: convert::range(files, span),
                })
            })
            .collect())
    }

    /// The completion items at the position, ranked by the typed prefix:
    /// a member position with a checked receiver answers the receiver's
    /// members; a type position answers the in-scope type variables ahead
    /// of the visible type names; every other position answers the names
    /// visible there — the ambient's declarations, the package's others,
    /// then the enclosing local scopes innermost-first — plus, behind a
    /// typed keyword prefix, the fixed skeletons the position's
    /// probe-decided site admits, and, behind an unresolved typed prefix
    /// at a bare-name or type position, the auto-import tier's edits over
    /// the exported ambient members whose names prefix-match.
    pub(super) fn completion(
        &mut self,
        file: FileId,
        line: u32,
        character: u32,
    ) -> Result<Vec<lsp::CompletionItem>, QueryError> {
        let offset = convert::offset(self.db.files(), file, lsp::Position { line, character });
        let (prefix, dot) = {
            let source = self.db.files().source(file);
            (
                completion::typed_prefix(source, offset),
                completion::member_dot(source, offset),
            )
        };
        // A member position with a checked receiver answers the members
        // alone; one with no checked receiver falls through to the scope
        // answer below (never a guess at members).
        if let Some(dot) = dot {
            let analyzed = self.db.get::<Analyzed>(PackageId::new(0))?.analyzed;
            if let Some(recv) = completion::receiver_val(&analyzed, file, dot) {
                return Ok(completion::rank(
                    completion::members(&analyzed, recv),
                    &prefix,
                ));
            }
        }
        let parse = self.db.get::<ParseFile>(file)?;
        if let Some(ast) = &parse.ast {
            let resolved = self.db.get::<Resolve>(PackageId::new(0))?;
            if let Some(vars) = completion::type_vars(ast, &resolved.package, file, offset) {
                let frames = self.db.get::<ScopeIndex>(file)?.frames;
                let entries =
                    ds_check::scope::visible_at(&frames, &resolved.package, self.ambient, offset);
                let mut items = completion::type_items(&vars, &entries);
                // The auto-import tier serves a type position behind an
                // unresolved typed prefix: no in-scope type variable,
                // `Self`, or visible type-namespace name carries the
                // prefix's own spelling.
                if !prefix.is_empty()
                    && vars.names.iter().all(|(name, _)| name != &prefix)
                    && (!vars.self_in_scope || prefix != "Self")
                    && entries.iter().all(|entry| {
                        !completion::type_namespace(entry.kind) || entry.name != prefix
                    })
                {
                    autoimport::attach(
                        &mut items,
                        &autoimport::Inputs {
                            files: self.db.files(),
                            file,
                            source: self.db.files().source(file),
                            ast,
                            package: &resolved.package,
                            ambient: self.ambient,
                        },
                        &prefix,
                        autoimport::Namespace::Type,
                    );
                }
                return Ok(completion::rank(items, &prefix));
            }
        }
        let frames = self.db.get::<ScopeIndex>(file)?.frames;
        let resolved = self.db.get::<Resolve>(PackageId::new(0))?;
        let entries = ds_check::scope::visible_at(&frames, &resolved.package, self.ambient, offset);
        let mut items: Vec<lsp::CompletionItem> = entries
            .iter()
            .map(|entry| lsp::CompletionItem {
                label: entry.name.clone(),
                kind: Some(convert::completion_kind(entry.kind)),
                ..lsp::CompletionItem::default()
            })
            .collect();
        // The skeletons ride the scope answer behind a typed keyword
        // prefix — never at a member position, where the dot names a
        // field or member spelling no skeleton expands into — and the
        // rank's stable sort keeps each behind every same-prefix name.
        if dot.is_none() {
            let source = self.db.files().source(file);
            items.extend(completion::snippets(source, offset, &prefix));
            // The auto-import tier serves a bare-name position behind an
            // unresolved typed prefix: no visible name carries the
            // prefix's own spelling. A member position never takes it —
            // importing a name serves no dot — and a poisoned parse
            // answers no edit (no rows to read).
            if !prefix.is_empty()
                && entries.iter().all(|entry| entry.name != prefix)
                && let Some(ast) = &parse.ast
            {
                autoimport::attach(
                    &mut items,
                    &autoimport::Inputs {
                        files: self.db.files(),
                        file,
                        source,
                        ast,
                        package: &resolved.package,
                        ambient: self.ambient,
                    },
                    &prefix,
                    autoimport::Namespace::Value,
                );
            }
        }
        Ok(completion::rank(items, &prefix))
    }

    /// The resolved target at the position together with the name
    /// occurrence that names it: the covering use's resolution, else the
    /// definition, local binder, or registered method whose own name the
    /// position names (declaration positions record no use). `None`
    /// answers for positions on no name.
    fn target_at(
        &mut self,
        file: FileId,
        offset: u32,
    ) -> Result<Option<(Span, Target)>, QueryError> {
        let resolved = self.db.get::<Resolve>(PackageId::new(0))?;
        let package = &resolved.package;
        let covering = package.file_position(file).and_then(|index| {
            resolved.resolutions[index].covering(Span::new(file, offset, offset))
        });
        if let Some((use_span, res)) = covering {
            return Ok(match res {
                Res::Def(id) => Some((use_span, Target::Def(*id))),
                Res::Local { bind } => Some((use_span, Target::Local(*bind))),
                Res::TraitMethod(id) => Some((use_span, Target::Method(*id))),
                Res::Dot(DotRes::Free { func, .. }) => Some((use_span, Target::Def(*func))),
                Res::Dot(DotRes::Methods { candidates } | DotRes::Static { candidates }) => {
                    // The checker's type-directed selection names the
                    // member behind the dot; a call whose selection is
                    // missing failed its own check, and the raw
                    // candidates name no single member.
                    let analyzed = self.db.get::<Analyzed>(PackageId::new(0))?.analyzed;
                    Some((
                        use_span,
                        match analyzed.checked.method_selections.get(&use_span) {
                            Some(&method) => Target::Method(method),
                            None => Target::Methods(candidates.clone()),
                        },
                    ))
                },
                // The field tier, the module-qualified tier, the
                // handler forms, and a module alias name no single
                // target behind them.
                Res::Dot(DotRes::Field | DotRes::Module { .. }) | Res::Handler | Res::Module(_) => {
                    None
                },
            });
        }
        for raw in 0..package.len() {
            let def = package.def(PackageDef::from_raw(
                u32::try_from(raw).expect("def count fits u32"),
            ));
            if def.span.file == file && def.span.start <= offset && offset < def.span.end {
                return Ok(Some((
                    def.span,
                    Target::Def(PackageDef::from_raw(
                        u32::try_from(raw).expect("def count fits u32"),
                    )),
                )));
            }
        }
        for (raw, method) in package.methods().enumerate() {
            if method.span.file == file && method.span.start <= offset && offset < method.span.end {
                return Ok(Some((
                    method.span,
                    Target::Method(MethodId::from_raw(
                        u32::try_from(raw).expect("method count fits u32"),
                    )),
                )));
            }
        }
        // A trait impl's own member fn name is not registered (the
        // trait's member is the one candidate); a position on it names
        // that member through the impl's trait reference.
        if let Some((name_span, id)) = self.impl_member_at(file, offset)? {
            return Ok(Some((name_span, Target::Method(id))));
        }
        // A local binder's own name: the scope frames carry every binder.
        let frames = self.db.get::<ScopeIndex>(file)?.frames;
        Ok(frames
            .iter()
            .flat_map(|frame| frame.bindings.iter())
            .find(|binding| binding.span.start <= offset && offset < binding.span.end)
            .map(|binding| (binding.span, Target::Local(binding.span))))
    }

    /// Every binder of the local's defining scope that shares its name:
    /// or-pattern alternatives rebind one name, so a local's declaration
    /// side is possibly several binder spans. The defining scope is the
    /// innermost frame holding the binder the resolution recorded.
    fn local_binders(&mut self, bind: Span) -> Result<Vec<Span>, QueryError> {
        let frames = self.db.get::<ScopeIndex>(bind.file)?.frames;
        let name = self.source_name(bind);
        let Some(frame) = frames
            .iter()
            .filter(|frame| {
                frame.span.file == bind.file
                    && frame.span.start <= bind.start
                    && bind.end <= frame.span.end
            })
            .min_by_key(|frame| frame.span.end - frame.span.start)
        else {
            return Ok(vec![bind]);
        };
        let binders: Vec<Span> = frame
            .bindings
            .iter()
            .filter(|binding| binding.name == name)
            .map(|binding| binding.span)
            .collect();
        Ok(if binders.is_empty() { vec![bind] } else { binders })
    }

    /// Every trait impl's own member fn name occurrence with the trait its
    /// impl resolves to: `(trait def, name span, name text)`, in file then
    /// source order. The trait reference resolves through the same
    /// resolution table every other answer reads, so a same-named impl of
    /// another trait never lands in the set. Trait impls do not
    /// re-register their members — the trait's own member is the one
    /// candidate — so this walk is the only place an impl's member name
    /// occurrence is named.
    fn impl_member_names(&mut self) -> Result<Vec<(PackageDef, Span, String)>, QueryError> {
        let resolved = self.db.get::<Resolve>(PackageId::new(0))?;
        let mut out = Vec::new();
        for (index, file) in resolved.package.files().enumerate() {
            let Some(res) = resolved.resolutions.get(index) else {
                continue;
            };
            let parse = self.db.get::<ParseFile>(file)?;
            let Some(ast) = &parse.ast else {
                continue;
            };
            for imp in ast
                .blocks
                .iter()
                .flat_map(|block| block.items.iter())
                .filter_map(|item| match &item.kind {
                    ds_parse::ItemKind::Impl(imp) => Some(imp),
                    _ => None,
                })
            {
                let Some(trait_ref) = &imp.trait_ref else {
                    continue;
                };
                let [head] = trait_ref.path.segments.as_slice() else {
                    continue;
                };
                let Some(Res::Def(def)) = res.get(head.span) else {
                    continue;
                };
                out.extend(
                    imp.fns
                        .iter()
                        .map(|f| (*def, f.name.span, f.name.name.clone())),
                );
            }
        }
        Ok(out)
    }

    /// The registered trait member a trait impl's own member fn name
    /// occurrence names at the position.
    fn impl_member_at(
        &mut self,
        file: FileId,
        offset: u32,
    ) -> Result<Option<(Span, MethodId)>, QueryError> {
        let resolved = self.db.get::<Resolve>(PackageId::new(0))?;
        let package = &resolved.package;
        for &(def, span, ref name) in &self.impl_member_names()? {
            if span.file != file || !(span.start <= offset && offset < span.end) {
                continue;
            }
            let owns = |id: MethodId| {
                matches!(&package.method(id).owner,
                    MethodOwner::Trait(owner) if *owner == def)
            };
            let Some(id) = package.method_candidates(name).find(|&id| owns(id)) else {
                continue;
            };
            return Ok(Some((span, id)));
        }
        Ok(None)
    }

    /// The declaration-side name occurrences of one registered member: its
    /// own declaration, plus — when it belongs to a trait — every impl's
    /// same-named member fn (its own name is the trait member's spelling,
    /// so a rename covers them alike).
    fn member_decl_spans(&mut self, id: MethodId) -> Result<Vec<Span>, QueryError> {
        let resolved = self.db.get::<Resolve>(PackageId::new(0))?;
        let package = &resolved.package;
        let method = package.method(id);
        let MethodOwner::Trait(trait_def) = &method.owner else {
            return Ok(vec![method.span]);
        };
        let mut spans = vec![method.span];
        spans.extend(
            self.impl_member_names()?
                .iter()
                .filter(|&&(def, _, ref name)| def == *trait_def && name == &method.name)
                .map(|&(_, span, _)| span),
        );
        Ok(spans)
    }

    /// The source spelling of one span's text.
    fn source_name(&self, span: Span) -> String {
        self.db.files().source(span.file)[span.start as usize..span.end as usize].to_owned()
    }

    /// Whether the definition at `span` declares an `extern` function:
    /// the surface fact at its own file's parse tree (extern names are
    /// platform link spellings, not fresh names).
    fn is_extern(&mut self, span: Span) -> Result<bool, QueryError> {
        let parse = self.db.get::<ParseFile>(span.file)?;
        Ok(parse.ast.as_deref().is_some_and(|ast| {
            ast.blocks.iter().any(|block| {
                block.items.iter().any(|item| {
                    matches!(&item.kind, ds_parse::ItemKind::ExternFn(f) if f.name.span == span)
                })
            })
        }))
    }

    /// The references of the name at the position: every use resolving
    /// to the same target across the package, plus the target's own
    /// declaration when the client asks. Uses inside the ambient answer
    /// no location: no open document carries their file.
    pub(super) fn references(
        &mut self,
        file: FileId,
        line: u32,
        character: u32,
        include_declaration: bool,
    ) -> Result<Vec<lsp::Location>, QueryError> {
        let offset = convert::offset(self.db.files(), file, lsp::Position { line, character });
        let Some((_, target)) = self.target_at(file, offset)? else {
            return Ok(Vec::new());
        };
        let resolved = self.db.get::<Resolve>(PackageId::new(0))?;
        let mut spans: Vec<Span> = Vec::new();
        match target {
            Target::Def(id) => {
                if include_declaration {
                    spans.push(resolved.package.def(id).span);
                }
                spans.extend(resolved.uses.def_uses(id).iter().copied());
            },
            Target::Local(bind) => {
                let binders = self.local_binders(bind)?;
                if include_declaration {
                    spans.extend(binders.iter().copied());
                }
                for binder in &binders {
                    spans.extend(resolved.uses.local_uses(*binder).iter().copied());
                }
            },
            Target::Method(id) => {
                if include_declaration {
                    spans.extend(self.member_decl_spans(id)?);
                }
                spans.extend(resolved.uses.method_uses(id).iter().copied());
                let analyzed = self.db.get::<Analyzed>(PackageId::new(0))?.analyzed;
                spans.extend(
                    analyzed
                        .checked
                        .method_selections
                        .iter()
                        .filter(|&(_, &selected)| selected == id)
                        .map(|(&span, _)| span),
                );
            },
            Target::Methods(candidates) => {
                for id in candidates {
                    if include_declaration {
                        spans.push(resolved.package.method(id).span);
                    }
                    spans.extend(resolved.uses.method_uses(id).iter().copied());
                }
            },
        }
        spans.sort_unstable_by_key(|span| (span.file.as_u32(), span.start));
        spans.dedup();
        let files = self.db.files();
        Ok(spans
            .into_iter()
            .filter_map(|span| {
                self.uri(span.file).map(|uri| lsp::Location {
                    uri,
                    range: convert::range(files, span),
                })
            })
            .collect())
    }

    /// The rename-ready answer at the position: the name's range and its
    /// current spelling, or a refusal message.
    pub(super) fn prepare_rename(
        &mut self,
        file: FileId,
        line: u32,
        character: u32,
    ) -> Result<RenameAnswer, QueryError> {
        let offset = convert::offset(self.db.files(), file, lsp::Position { line, character });
        let Some((name_span, target)) = self.target_at(file, offset)? else {
            return Ok(RenameAnswer::Refused(
                "the position names no definition or use".to_owned(),
            ));
        };
        match target {
            Target::Def(id) => {
                let resolved = self.db.get::<Resolve>(PackageId::new(0))?;
                let def = resolved.package.def(id);
                if self.uri(def.span.file).is_none() {
                    return Ok(RenameAnswer::Refused(
                        "the name lives in the stdlib ambient, which no open document can edit"
                            .to_owned(),
                    ));
                }
                if self.is_extern(def.span)? {
                    return Ok(RenameAnswer::Refused(
                        "extern names are platform link spellings; renaming them is refused"
                            .to_owned(),
                    ));
                }
                if Self::is_language_name(&def.name) {
                    return Ok(RenameAnswer::Refused(
                        "`self` is a language name; renaming is refused".to_owned(),
                    ));
                }
                let range = convert::range(self.db.files(), name_span);
                Ok(RenameAnswer::Prepare(range, def.name.clone()))
            },
            Target::Local(bind) => {
                let name = self.source_name(bind);
                if Self::is_language_name(&name) {
                    return Ok(RenameAnswer::Refused(
                        "`self` is a language name; renaming is refused".to_owned(),
                    ));
                }
                let range = convert::range(self.db.files(), name_span);
                Ok(RenameAnswer::Prepare(range, name))
            },
            Target::Method(id) => {
                let resolved = self.db.get::<Resolve>(PackageId::new(0))?;
                let method = resolved.package.method(id);
                if self.uri(method.span.file).is_none() {
                    return Ok(RenameAnswer::Refused(
                        "the member lives in the stdlib ambient, which no open document can edit"
                            .to_owned(),
                    ));
                }
                let range = convert::range(self.db.files(), name_span);
                Ok(RenameAnswer::Prepare(range, method.name.clone()))
            },
            Target::Methods(_) => Ok(RenameAnswer::Refused(
                "the dot-call's type-directed selection did not settle; the call itself fails its \
                 check, so it names no member"
                    .to_owned(),
            )),
        }
    }

    /// Whether a name is a reserved language spelling.
    fn is_language_name(name: &str) -> bool {
        name == "self" || name == "Self"
    }

    /// The rename of the name at the position: a plain substitution of
    /// `new_name` over the target's declaration and every use, or a
    /// refusal message.
    pub(super) fn rename(
        &mut self,
        file: FileId,
        line: u32,
        character: u32,
        new_name: &str,
    ) -> Result<RenameAnswer, QueryError> {
        let offset = convert::offset(self.db.files(), file, lsp::Position { line, character });
        let Some((_, target)) = self.target_at(file, offset)? else {
            return Ok(RenameAnswer::Refused(
                "the position names no definition or use".to_owned(),
            ));
        };
        if !fresh_name(new_name) {
            return Ok(RenameAnswer::Refused(format!(
                "`{new_name}` is not a fresh identifier"
            )));
        }
        let resolved = self.db.get::<Resolve>(PackageId::new(0))?;
        let mut spans = match target {
            Target::Def(id) => {
                let def = resolved.package.def(id);
                // The refusal order matches prepare-rename's, so one name
                // refuses for the same reason on both requests.
                if self.uri(def.span.file).is_none() {
                    return Ok(RenameAnswer::Refused(
                        "the name lives in the stdlib ambient, which no open document can edit"
                            .to_owned(),
                    ));
                }
                if self.is_extern(def.span)? {
                    return Ok(RenameAnswer::Refused(
                        "extern names are platform link spellings; renaming them is refused"
                            .to_owned(),
                    ));
                }
                let mut spans = vec![def.span];
                spans.extend(resolved.uses.def_uses(id).iter().copied());
                spans
            },
            Target::Local(bind) => {
                let mut spans = self.local_binders(bind)?;
                let mut uses = Vec::new();
                for binder in &spans {
                    uses.extend(resolved.uses.local_uses(*binder).iter().copied());
                }
                spans.extend(uses);
                spans
            },
            Target::Method(id) => {
                let mut spans = self.member_decl_spans(id)?;
                spans.extend(resolved.uses.method_uses(id).iter().copied());
                let analyzed = self.db.get::<Analyzed>(PackageId::new(0))?.analyzed;
                spans.extend(
                    analyzed
                        .checked
                        .method_selections
                        .iter()
                        .filter(|&(_, &selected)| selected == id)
                        .map(|(&span, _)| span),
                );
                spans
            },
            Target::Methods(_) => {
                return Ok(RenameAnswer::Refused(
                    "the dot-call's type-directed selection did not settle; the call itself fails \
                     its check, so it names no member"
                        .to_owned(),
                ));
            },
        };
        spans.sort_unstable_by_key(|span| (span.file.as_u32(), span.start));
        spans.dedup();
        // An edit reaching a file with no open document (the ambient)
        // has nowhere to land: the rename refuses.
        if spans.iter().any(|span| self.uri(span.file).is_none()) {
            return Ok(RenameAnswer::Refused(
                "the rename reaches the stdlib ambient, which no open document can edit".to_owned(),
            ));
        }
        let files = self.db.files();
        let mut changes: std::collections::HashMap<lsp::Url, Vec<lsp::TextEdit>> =
            std::collections::HashMap::new();
        for span in spans {
            let uri = self
                .uri(span.file)
                .expect("every span's file carries an open document");
            changes.entry(uri).or_default().push(lsp::TextEdit {
                range: convert::range(files, span),
                new_text: new_name.to_owned(),
            });
        }
        Ok(RenameAnswer::Edit(lsp::WorkspaceEdit {
            changes: Some(changes),
            document_changes: None,
            change_annotations: None,
        }))
    }

    /// The document's top-level symbols out of its parse tree.
    pub(super) fn symbols(&mut self, file: FileId) -> Result<Vec<lsp::DocumentSymbol>, QueryError> {
        let parse = self.db.get::<ParseFile>(file)?;
        let Some(ast) = &parse.ast else {
            return Ok(Vec::new());
        };
        Ok(convert::symbols(self.db.files(), ast))
    }

    /// The document's canonical-format whole-file edit, or an empty edit
    /// list when the source already carries the canonical form.
    pub(super) fn format(
        &mut self,
        file: FileId,
    ) -> Result<Option<Vec<lsp::TextEdit>>, QueryError> {
        let parse = self.db.get::<ParseFile>(file)?;
        let Some(ast) = &parse.ast else {
            return Ok(None);
        };
        let formatted = ds_fmt::format_source(self.db.files(), file, ast);
        if formatted == *parse.source {
            return Ok(Some(Vec::new()));
        }
        let len = u32::try_from(self.db.files().source(file).len())
            .expect("a registered source fits u32 byte offsets");
        let edit = lsp::TextEdit {
            range: lsp::Range::new(
                lsp::Position::new(0, 0),
                convert::position(self.db.files(), file, len),
            ),
            new_text: formatted,
        };
        Ok(Some(vec![edit]))
    }

    /// The on-type format edit at the position after one trigger
    /// keystroke: the tightest declaration region the trigger names —
    /// the enclosing declaration of a newline typed inside a body, the
    /// declaration a `}` just closed — rendered through `ds-fmt`'s
    /// per-item seam over the session's current text. One trimmed edit
    /// covers the reformatted region, never the whole document; a
    /// poisoned parse (no region is determinable, so nothing
    /// reformats), a trigger the server never registered, a position no
    /// region names, or a region already carrying the canonical form
    /// answers no edits.
    pub(super) fn on_type_format(
        &mut self,
        file: FileId,
        line: u32,
        character: u32,
        ch: &str,
    ) -> Result<Option<Vec<lsp::TextEdit>>, QueryError> {
        if ch != "\n" && ch != "}" {
            return Ok(Some(Vec::new()));
        }
        let parse = self.db.get::<ParseFile>(file)?;
        let Some(ast) = &parse.ast else {
            return Ok(None);
        };
        let offset = convert::offset(self.db.files(), file, lsp::Position { line, character });
        // The typed character sits just behind the cursor on both
        // registered triggers, so the position and its left neighbor
        // name the region: the declaration containing the position
        // first, then the one the typed character closed (a newline
        // directly behind an item's end).
        let item = [offset, offset.saturating_sub(1)]
            .iter()
            .find_map(|&probe| {
                ast.blocks
                    .iter()
                    .flat_map(|block| block.items.iter())
                    .filter(|item| !item.synthesized)
                    .find(|item| {
                        let (start, end) = ds_fmt::item_region(item);
                        start <= probe && probe <= end
                    })
            });
        let Some(item) = item else {
            return Ok(Some(Vec::new()));
        };
        let region = ds_fmt::format_item(self.db.files(), file, item);
        let source = self.db.files().source(file);
        // The shared prefix and suffix stay in place: the edit covers
        // exactly the changed middle of the region, the minimal range.
        let old = &source[region.start as usize..region.end as usize];
        let prefix = old
            .chars()
            .zip(region.text.chars())
            .take_while(|(a, b)| a == b)
            .map(|(a, _)| a.len_utf8())
            .sum::<usize>();
        let suffix = old[prefix..]
            .chars()
            .rev()
            .zip(region.text[prefix..].chars().rev())
            .take_while(|(a, b)| a == b)
            .map(|(a, _)| a.len_utf8())
            .sum::<usize>();
        let start = u32::try_from(region.start as usize + prefix)
            .expect("a region start fits u32 byte offsets");
        let end = u32::try_from(region.end as usize - suffix)
            .expect("a region end fits u32 byte offsets");
        let new_text = &region.text[prefix..region.text.len() - suffix];
        if start == end && new_text.is_empty() {
            return Ok(Some(Vec::new()));
        }
        let files = self.db.files();
        Ok(Some(vec![lsp::TextEdit {
            range: lsp::Range::new(
                convert::position(files, file, start),
                convert::position(files, file, end),
            ),
            new_text: new_text.to_owned(),
        }]))
    }

    /// The document's whole-file semantic token stream, delta-encoded,
    /// cached per file on the engine revision: a request over an
    /// unchanged revision answers the cached stream, and any input
    /// change bumps the revision and walks again — one pass over the
    /// parse tree reading the resolution side table, the scope-index
    /// frames, and the declaration tables the queries hold at ask time
    /// (a document whose parse failed answers empty, uncached).
    pub(super) fn semantic_tokens(
        &mut self,
        file: FileId,
    ) -> Result<Vec<lsp::SemanticToken>, QueryError> {
        let revision = self.db.revision();
        if let Some((cached_at, stream)) = self.token_cache.get(&file)
            && *cached_at == revision
        {
            return Ok(stream.clone());
        }
        let parse = self.db.get::<ParseFile>(file)?;
        let Some(ast) = &parse.ast else {
            return Ok(Vec::new());
        };
        let resolved = self.db.get::<Resolve>(PackageId::new(0))?;
        let Some(index) = resolved.package.file_position(file) else {
            return Ok(Vec::new());
        };
        let analyzed = self.db.get::<Analyzed>(PackageId::new(0))?.analyzed;
        let fn_defs: FxHashSet<PackageDef> = analyzed.decls.fns.iter().map(|(id, _)| *id).collect();
        let frames = self.db.get::<ScopeIndex>(file)?.frames;
        self.token_walks += 1;
        let stream = tokens::stream(self.db.files(), file, ast, &tokens::Inputs {
            resolutions: &resolved.resolutions[index],
            package: &resolved.package,
            fn_defs: &fn_defs,
            frames: &frames,
        });
        self.token_cache.insert(file, (revision, stream.clone()));
        Ok(stream)
    }
}
