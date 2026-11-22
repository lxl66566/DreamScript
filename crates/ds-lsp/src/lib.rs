//! The DreamScript language-server slice: a thin protocol layer over the
//! memoized query engine, with one dedicated thread owning the engine.
//!
//! # Model
//!
//! The query database is `!Send` (`Analyzed` bundles hold `Rc` declaration
//! tables, so engine values never cross threads), so the server is the
//! REPL's session shape with an editor instead of stdin: one worker
//! thread owns the [`Db`] for the server's life, with the stdlib ambient
//! registered as the package's first member, and the protocol layer talks
//! to it through `Send` commands and replies over an mpsc channel with
//! oneshot answer channels. Editing swaps a document's content in place
//! through `Files::set_source` (the per-keystroke path the registry is
//! built for); file ids are never reused, and a reopened document
//! re-registers its content onto its original id.
//!
//! # Capabilities of this slice
//!
//! Incremental synchronization (a change notification's ranged edits
//! spliced into the held document through the LSP UTF-16 position
//! columns, a rangeless change replacing the whole text, so full-text
//! clients ride the same path), diagnostics from the package-grain query
//! values (parse, resolve, check, linearity, platform classes) published
//! on open and on the trailing edge of a short debounce window after
//! change and save, hover over the checked expression types rendered
//! through the analysis universe with the enclosing definition's effect
//! row, go-to-definition through the resolution side table, completion of
//! the names visible at the position through the scope-index query
//! (ambient declarations first, then the package's, then the enclosing
//! local scopes innermost-first), the members of a dot receiver with a
//! checked type (fields, cases, inherent and trait members, UFCS
//! candidates), the type variables in scope inside a type position, the
//! fixed declaration and expression skeletons behind a typed keyword
//! prefix (the site — item level, an impl or trait member list, a body —
//! decided by probe parses of the source with the prefix replaced, so
//! every offered expansion parses where it lands), prefix ranking over
//! every completion answer, and auto-import offers behind an unresolved
//! typed prefix (the exported ambient members and the `@std` package's
//! own members whose names prefix-match, each item carrying the edit
//! that inserts or merges the `@std` import row the file needs; private
//! and `pub(pkg)` names never offered),
//! references through the
//! resolve value's
//! reverse-edge index (every use resolving to the same target across the
//! package, the declaration included when asked), rename and
//! prepare-rename as a plain identifier substitution over a definition,
//! local binder, or registered member and every use of it — a
//! dot-call's callee names its member through the checker's recorded
//! type-directed selection, and a member's edit set covers its own
//! declaration, each of its trait's impl members, its qualified
//! `Trait::f` uses, and every dot-call that selects it — document symbols
//! from the parse tree, whole-document formatting through `ds-fmt`,
//! on-type formatting
//! of the tightest declaration region the trigger names (a newline
//! inside a body, a closing `}`) through `ds-fmt`'s per-item seam, and
//! `textDocument/semanticTokens/full`: a whole-document token stream over
//! the lossless parse tree plus the analysis the engine holds (keywords
//! and operators where the tree or a verified gap spells them, comments
//! including `///` doc runs, string/char/byte and number literals, and
//! identifiers classified by resolution — locals by their scope-index
//! frame kind, package value names by their definition kind, type
//! positions through the walk's variable scopes, dot-call members by
//! tier, `self` as a keyword where it binds).
//!
//! # Honest boundaries
//!
//! * One package is all open documents (G3's shared namespace): a duplicate name across two open
//!   files is reported in both, which is correct package semantics, not a server defect.
//! * Script-mode files fail the library-mode parse (top-level statements are syntax errors); that
//!   stays honest until the loading round.
//! * Semantic tokens serve full documents only (no delta, no range); a document whose parse failed
//!   answers an empty stream. Punctuation is never emitted, and keywords and operators answer only
//!   where the tree or a verified source gap spells them — a gap a comment occupies answers
//!   nothing. A name the resolution side table records nothing for stays a plain identifier (an
//!   unresolved name, a bare field access, `none`, import spellings, attribute contents), and a
//!   bare field access is exactly that: member tokens answer dot-call positions alone. Char and
//!   byte literals fold into the string class; the only modifiers are `declaration` and the
//!   `readonly` of package-level const bindings (a local const/var distinction would need new
//!   tables on the use side); the walk reads the raw parse tree, so derive-synthesized trees (whose
//!   spans lie inside the attributed declaration) never emit.
//! * Completion answers members after a dot only when the checker's expression-type table answers
//!   the receiver (the tightest expression ending at the dot): a trailing dot on a document the
//!   parse refuses, a poisoned receiver, or a type narrowed to empty falls back to the ordinary
//!   scope answer and names no member. UFCS candidates match the first parameter's written head
//!   name against the receiver's atoms (no argument unification). Plain items carry label and kind
//!   only; a skeleton item adds its snippet insert text, answers behind a typed keyword prefix
//!   alone (never on the empty prefix, never at a member or type position), and only where its
//!   probe-decided site makes the expansion parse — a document broken beyond the prefix answers
//!   none. Auto-import serves a bare-name or type position behind an unresolved typed prefix alone
//!   (never at a member position, never on the empty prefix): the exported ambient members and the
//!   `@std` package's own members (the tree's sources, the same export gate) whose names
//!   prefix-match carry the edit that merges into a plain existing `@std` row or inserts a fresh
//!   one grouped with the file's imports — private and `pub(pkg)` names never offered. Under the
//!   merged registration the members ride inside the ambient, so every such name is visible anyway
//!   and the edit is the forward-compatible spelling; a core-shaped registration (a declaring
//!   graph's) carries none of them, and each member joins the answer as its own item — while the
//!   session runs no link pass, so the row's own validation stays in the graph's compile.
//! * Rename is a plain identifier substitution over the target's declaration and the uses the
//!   resolution table recorded — no formatting, and no comment or string awareness (comments and
//!   strings sit outside every recorded span by construction). A member's edit set covers its own
//!   declaration, each of its trait's impl members, its qualified `Trait::f` uses, and every
//!   dot-call whose checker-recorded type-directed selection names it (a receiverless member
//!   through the type-keyed tier included); a dot-call whose selection is not recorded failed its
//!   own check — an ambiguous candidate set even type-direction cannot settle, a receiver no member
//!   accepts — and refuses, as does renaming the member it would need. Extern names refuse (they
//!   are platform link spellings), and so does any edit reaching the ambient stdlib (no open
//!   document carries it — including a trait member an ambient impl implements); references of an
//!   ambient name answer only the open documents' uses.
//! * On-type formatting reformats the tightest declaration region the trigger names — the
//!   declaration enclosing a typed newline or the one a typed `}` just closed — through `ds-fmt`'s
//!   zero-option canonical form: the client's tab-size and insert-spaces options are unaccepted, a
//!   poisoned parse answers nothing (broken code never reformats its neighbors), and one trimmed
//!   edit covers the changed middle of the region alone, never the whole document.
//! * `.dsi` hover serves three tiers: the checked type of the expression at the position, the `pub`
//!   package declaration's signature line when the position names one, and — for a name one of the
//!   package's imports selects that the session's package-free resolution left unknown — the linked
//!   tier's line from the embedder's surface provider, the `.dsi` artifact a run of the same
//!   package graph cached beside the dep's compiled module (the ambient path answers through the
//!   ambient's own parse, never the provider).
//! * Ambient stdlib names resolve (the ambient registers first), but the ambient file has no client
//!   URI, so definitions pointing into it answer none.
//! * Change and save notifications defer their diagnostics publish through a short trailing
//!   debounce window, so the memoized queries recompute at most once per window; the edits
//!   themselves apply immediately, so a request mid-window answers the client's current text. Save
//!   notifications carry no content (the synced text is authoritative), and a change for a document
//!   never opened syncs nothing (the protocol orders `didOpen` first).

#![warn(missing_docs)]

mod autoimport;
mod completion;
mod convert;
mod server;
mod tokens;

use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    path::{Path, PathBuf},
    sync::mpsc,
};

use anyhow::Result;
use ds_check::{DefId as PackageDef, DotRes, MethodId, MethodOwner, Res};
use ds_diag::{FileId, Span};
use ds_query::{
    Analyzed, BlockIndex, CheckedPackage, Db, DefId, EffectsOf, PackageEffects, PackageId,
    PackageLinearity, ParseFile, PlatformCheck, PooledCheck, QueryError, Resolve, Revision,
    ScopeIndex,
};
use rustc_hash::{FxHashMap, FxHashSet};
use tokio::sync::oneshot;
use tower_lsp::{LspService, Server, lsp_types as lsp};

/// The hover answer the worker ships back: the rendered type (with the
/// enclosing definition's effect row) and the covered source range.
struct HoverAnswer {
    /// The markdown contents of the hover popup.
    contents: String,
    /// The source range the answer covers.
    range: lsp::Range,
}

/// One linked package's served `.dsi` surface: each exported name with
/// its signature line, the rendering a run of the same package graph
/// cached beside the dep's compiled module. The embedder builds this
/// from the written artifact; the server only reads it.
#[derive(Debug, Clone)]
pub struct LinkedSurface {
    /// Each exported definition's `.dsi` signature line, keyed by name.
    pub signatures: std::collections::HashMap<String, String>,
}

/// The embedder's linked-surface provider: one document's path and one
/// import's link name to the surface that name serves, `None` when
/// nothing serves it (no manifest beside the document, no resolved
/// graph, no cached compile). The call must not re-enter the session.
pub type SurfaceProvider =
    std::sync::Arc<dyn Fn(&Path, &str) -> Option<std::sync::Arc<LinkedSurface>> + Send + Sync>;

/// The provider that serves nothing: the bare server's linked tier.
fn no_surface() -> SurfaceProvider {
    std::sync::Arc::new(|_doc: &Path, _link: &str| None)
}

/// The reply channel of one worker command.
type Reply<T> = oneshot::Sender<Result<T, String>>;

/// One command for the engine's worker thread, with the oneshot the
/// answer goes back through.
enum Request {
    /// A document opened: register it (or re-register a reopened id)
    /// under the client's own URI spelling.
    Open {
        path: PathBuf,
        uri: lsp::Url,
        text: String,
        reply: Reply<FileId>,
    },
    /// A document's whole content replaced (did_close ships the empty
    /// close text).
    Set {
        path: PathBuf,
        text: String,
        reply: Reply<()>,
    },
    /// A document's didChange content array: each change in array order,
    /// a ranged one spliced into the held text through the LSP UTF-16
    /// position columns, a rangeless one replacing it whole.
    Edit {
        path: PathBuf,
        changes: Vec<lsp::TextDocumentContentChangeEvent>,
        reply: Reply<()>,
    },
    /// The document's diagnostics.
    Diagnostics {
        path: PathBuf,
        reply: Reply<Vec<lsp::Diagnostic>>,
    },
    /// Hover at one LSP position.
    Hover {
        path: PathBuf,
        line: u32,
        character: u32,
        reply: Reply<Option<HoverAnswer>>,
    },
    /// Definition locations at one LSP position.
    Definition {
        path: PathBuf,
        line: u32,
        character: u32,
        reply: Reply<Vec<lsp::Location>>,
    },
    /// The completion items at one LSP position.
    Completion {
        path: PathBuf,
        line: u32,
        character: u32,
        reply: Reply<Vec<lsp::CompletionItem>>,
    },
    /// The references at one LSP position.
    References {
        path: PathBuf,
        line: u32,
        character: u32,
        include_declaration: bool,
        reply: Reply<Vec<lsp::Location>>,
    },
    /// Prepare-rename at one LSP position.
    PrepareRename {
        path: PathBuf,
        line: u32,
        character: u32,
        reply: Reply<RenameAnswer>,
    },
    /// Rename at one LSP position.
    Rename {
        path: PathBuf,
        line: u32,
        character: u32,
        new_name: String,
        reply: Reply<RenameAnswer>,
    },
    /// The document's top-level symbols.
    Symbols {
        path: PathBuf,
        reply: Reply<Vec<lsp::DocumentSymbol>>,
    },
    /// The document's canonical-format edits.
    Format {
        path: PathBuf,
        reply: Reply<Option<Vec<lsp::TextEdit>>>,
    },
    /// The on-type format edits at one position after one trigger
    /// character.
    OnTypeFormat {
        path: PathBuf,
        line: u32,
        character: u32,
        ch: String,
        reply: Reply<Option<Vec<lsp::TextEdit>>>,
    },
    /// The document's whole-file semantic token stream.
    Tokens {
        path: PathBuf,
        reply: Reply<Vec<lsp::SemanticToken>>,
    },
    /// The worker drops the database and answers; the thread then exits.
    Shutdown { reply: Reply<()> },
}

/// The resolved target at one position: what references answer and
/// rename edits or refuses.
#[derive(Debug)]
enum Target {
    /// A package-level definition.
    Def(PackageDef),
    /// A local binder: its introducing span.
    Local(Span),
    /// A registered method (a trait member or an inherent impl fn).
    Method(MethodId),
    /// A dot-call's method candidates: the checker's type-directed
    /// selection recorded none for the call, so it names no single
    /// member.
    Methods(Vec<MethodId>),
}

/// One rename-family answer: the payload, or a refusal the protocol
/// layer answers as an invalid-params error with its message (kept out
/// of the command error channel, which is internal failures only).
#[derive(Debug)]
enum RenameAnswer {
    /// Prepare-rename: the name's range and current spelling.
    Prepare(lsp::Range, String),
    /// Rename: the edit set.
    Edit(lsp::WorkspaceEdit),
    /// A refusal carrying its message.
    Refused(String),
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

/// The link name one import declaration links under (G3.5): an
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

/// One open (or once-opened) document: its file id and the client's own
/// URI spelling, which answers echo verbatim instead of re-synthesizing
/// a URI from the decoded path (a path that is not valid UTF-8 cannot
/// round-trip through one).
struct Doc {
    /// The registry file id, stable across close/reopen cycles.
    file: FileId,
    /// The URI spelling the client shipped at the last open.
    uri: lsp::Url,
}

/// The worker-thread state: the query database, the open-document
/// table, and the per-file semantic-token cache. Everything runs on the
/// worker thread; values never leave it except through the `Send` reply
/// payloads.
struct Session {
    db: Db,
    /// Each document the server has ever opened, keyed by its decoded
    /// on-disk path; a close/reopen cycle reuses the entry.
    docs: FxHashMap<PathBuf, Doc>,
    /// The ambient's file id: its declarations answer first in
    /// completion, wherever the session registered it.
    ambient: FileId,
    /// The last computed semantic-token stream per file, stamped with
    /// the engine revision it was computed at. The revision is the
    /// queries' own invalidation clock — bumped by every real input
    /// change (an edit with new text, a registration) and nothing else —
    /// so an unchanged revision answers from the cache and any change
    /// invalidates every entry. The grain is honest, not conservative:
    /// the stream reads the package-grain resolve and analysis queries,
    /// so another document's edit can move this one's stream.
    token_cache: FxHashMap<FileId, (Revision, Vec<lsp::SemanticToken>)>,
    /// How many times the token walk has actually run (test
    /// introspection; cache hits are invisible here by design).
    token_walks: usize,
    /// The embedder's linked-surface tier: the `.dsi` artifacts a run of
    /// the document's own package graph cached, consulted only after the
    /// session's own resolution tiers answer nothing (the session
    /// carries no package graph, so an import-bound def resolves
    /// nothing).
    linked: SurfaceProvider,
}

impl Session {
    /// A session with the stdlib ambient registered as the package's
    /// first member (G3): stdlib names resolve in every document, and
    /// the ambient's file id never gains a client URI.
    #[cfg(test)]
    fn new() -> Self {
        Self::with_linked(no_surface())
    }

    /// [`Session::new`] over the embedder's linked-surface provider.
    fn with_linked(linked: SurfaceProvider) -> Self {
        Self::with_ambient(linked, ds_stdlib::AMBIENT_SOURCE)
    }

    /// A session over the shrunk ambient core: a declaring graph's
    /// registration shape, where the migrated `@std` members are not
    /// ambient members and stay unknown bare — the session runs no link
    /// pass, so an `@std` row's selection keeps its validation in the
    /// graph's compile alone. The auto-import tier's members' table
    /// serves the offers over this shape.
    #[cfg(test)]
    fn over_core_ambient() -> Self {
        Self::with_ambient(no_surface(), ds_stdlib::AMBIENT_CORE_SOURCE)
    }

    /// [`Session::with_linked`] over a chosen ambient registration: the
    /// merged source every undeclaring registration serves, or the shrunk
    /// core a declaring graph's registration takes.
    fn with_ambient(linked: SurfaceProvider, ambient_source: &str) -> Self {
        let mut db = Db::with_declared_rows(ds_stdlib::declared_rows().clone())
            .with_world_caps(ds_stdlib::world_caps().clone());
        let ambient = db.add_file(ds_stdlib::AMBIENT_FILE, ambient_source);
        Self {
            db,
            docs: FxHashMap::default(),
            ambient,
            token_cache: FxHashMap::default(),
            token_walks: 0,
            linked,
        }
    }

    /// The file id of an open (or once-opened) document.
    fn file(&self, path: &Path) -> Option<FileId> {
        self.docs.get(path).map(|doc| doc.file)
    }

    /// The client URI of one file id, when the document was ever opened
    /// (the ambient answers none, so definitions into it stay honest).
    fn uri(&self, file: FileId) -> Option<lsp::Url> {
        self.docs
            .values()
            .find(|doc| doc.file == file)
            .map(|doc| doc.uri.clone())
    }

    /// Registers a document (or restores a reopened one onto its
    /// original file id). The registry name is the path's lossy spelling
    /// (it only renders inside diagnostics); the exact path and the
    /// client's URI live in the document table.
    fn open(&mut self, path: &Path, uri: lsp::Url, text: &str) -> Result<FileId, QueryError> {
        if let Some(doc) = self.docs.get_mut(path) {
            let file = doc.file;
            // A reopen may ship a different URI spelling; answers echo
            // the newest one.
            doc.uri = uri;
            self.db.set_file_content(file, text)?;
            return Ok(file);
        }
        let file = self.db.add_file(path.to_string_lossy(), text);
        self.docs.insert(path.to_owned(), Doc { file, uri });
        Ok(file)
    }

    /// Replaces a document's content; a change for a document never
    /// opened syncs nothing (the protocol orders `didOpen` first).
    fn set(&mut self, path: &Path, text: &str) -> Result<(), QueryError> {
        if let Some(file) = self.docs.get(path).map(|doc| doc.file) {
            return self.db.set_file_content(file, text);
        }
        Ok(())
    }

    /// Applies a didChange content array to the document: each change
    /// in array order — a range-bearing one splices into the held text
    /// through the LSP UTF-16 position columns, a rangeless one
    /// replaces the whole content (full-text clients ride the same
    /// command). A change for a document never opened syncs nothing
    /// (the protocol orders `didOpen` first).
    fn edit(
        &mut self,
        path: &Path,
        changes: &[lsp::TextDocumentContentChangeEvent],
    ) -> Result<(), String> {
        let Some(file) = self.docs.get(path).map(|doc| doc.file) else {
            return Ok(());
        };
        for change in changes {
            let new_text = match change.range {
                Some(range) => {
                    convert::apply_change(self.db.files().source(file), range, &change.text)?
                },
                None => change.text.clone(),
            };
            self.db
                .set_file_content(file, &new_text)
                .map_err(|error| error.to_string())?;
        }
        Ok(())
    }

    /// The document's diagnostics: the parse, resolve, check, linearity,
    /// platform, and pooled classes out of the package-grain query values,
    /// each class's set filtered to this file's labels (the ambient's
    /// diagnostics carry no label here and drop out), sorted by range.
    fn diagnostics(&mut self, file: FileId) -> Result<Vec<lsp::Diagnostic>, QueryError> {
        let Self { db, docs, .. } = self;
        let uri_of = |file: FileId| -> Option<lsp::Url> {
            docs.values()
                .find(|doc| doc.file == file)
                .map(|doc| doc.uri.clone())
        };
        let parse = db.get::<ParseFile>(file)?;
        let resolved = db.get::<Resolve>(PackageId::new(0))?;
        let checked = db.get::<CheckedPackage>(PackageId::new(0))?;
        let linear = db.get::<PackageLinearity>(PackageId::new(0))?;
        let platform = db.get::<PlatformCheck>(PackageId::new(0))?;
        let pooled = db.get::<PooledCheck>(PackageId::new(0))?;
        let mut out = Vec::new();
        for diag in parse
            .diagnostics
            .iter()
            .chain(resolved.diags.iter())
            .chain(checked.diags.iter())
            .chain(linear.diags.iter())
            .chain(platform.diags.iter())
            .chain(pooled.diags.iter())
        {
            if let Some(mapped) = convert::diagnostic(db.files(), &uri_of, diag, file) {
                out.push(mapped);
            }
        }
        out.sort_by_key(|diag| (diag.range.start.line, diag.range.start.character));
        Ok(out)
    }

    /// The hover answer at the position: the checked type of the covering
    /// expression, rendered through the analysis universe, with its
    /// effect row appended (the enclosing definition's row when no
    /// recorded expression covers the position), preceded by the `.dsi`
    /// signature spelling when the position names a `pub` package
    /// declaration — the same line `ds info` prints for that declaration.
    /// A name one of the package's imports selects that resolution could
    /// not bind answers the linked tier's line when the embedder serves
    /// that package's cached `.dsi`.
    fn hover(
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
        // The exported-surface half: a name resolving to a `pub` package
        // declaration answers its signature spelling first; a name one of
        // the package's imports selects — the import-bound def the
        // session's package-free resolution left unknown — answers the
        // linked tier's line instead, never displacing a resolved name.
        let linked = match self.surface(file, offset)? {
            Some(found) => Some(found),
            None => self.linked_surface(path, file, offset),
        };
        if let Some((signature, name_range)) = linked {
            contents = if contents.is_empty() {
                format!("```dreamscript\n{signature}\n```")
            } else {
                format!("```dreamscript\n{signature}\n```\n\n{contents}")
            };
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
    /// resolution left unknown — answers its `.dsi` signature line from
    /// the embedder's surface provider, covering the word's range. A
    /// selection's alias binds the name the way the checker's link pass
    /// would, so both spellings serve; a word nothing selects answers
    /// nothing, exactly as before.
    fn linked_surface(
        &mut self,
        path: &Path,
        file: FileId,
        offset: u32,
    ) -> Option<(String, lsp::Range)> {
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
        let line = (self.linked)(path, package)?
            .signatures
            .get(selected)?
            .clone();
        Some((
            line,
            convert::range(self.db.files(), Span::new(file, start, end)),
        ))
    }

    /// The `.dsi` signature spelling of the name at the position: the
    /// resolution's target when the position names a use, the
    /// declaration itself when it names the definition — answered only
    /// for a `pub` package-level definition, the surface the spelling
    /// renders (locals, impl and trait members, and non-exported
    /// declarations have no `.dsi` line; `pub(pkg)` and private stay out
    /// by G4). The renderer is the one `ds info` prints, one item's
    /// signature over its own parse tree.
    fn surface(
        &mut self,
        file: FileId,
        offset: u32,
    ) -> Result<Option<(String, lsp::Range)>, QueryError> {
        let Some((name_span, target)) = self.target_at(file, offset)? else {
            return Ok(None);
        };
        let Target::Def(def) = target else {
            return Ok(None);
        };
        let resolved = self.db.get::<Resolve>(PackageId::new(0))?;
        let decl = resolved.package.def(def);
        let parse = self.db.get::<ParseFile>(decl.span.file)?;
        let Some(ast) = &parse.ast else {
            return Ok(None);
        };
        // Top-level items never nest, so the item whose span holds the
        // defining name occurrence is the declaration.
        let Some(item) = ast
            .blocks
            .iter()
            .flat_map(|block| block.items.iter())
            .find(|item| item.span.start <= decl.span.start && decl.span.end <= item.span.end)
        else {
            return Ok(None);
        };
        if !matches!(item.vis, Some(ds_parse::Vis::Pub(_))) {
            return Ok(None);
        }
        let mut line = String::new();
        ds_fmt::signature::item(&mut line, self.db.files().source(file), item);
        if line.trim_end().is_empty() {
            return Ok(None);
        }
        Ok(Some((
            line.trim_end().to_owned(),
            convert::range(self.db.files(), name_span),
        )))
    }

    /// The definitions of the name use at the position, through the
    /// resolution side table's covering lookup.
    fn definition(
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
            // The handler forms and a bare module alias name no
            // definition behind them (the alias's members are the
            // cross-package scope round's).
            Res::Handler | Res::Module(_) => Vec::new(),
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
    fn completion(
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
                // The module-qualified tier, the handler forms, and a
                // module alias name no single target behind them.
                Res::Dot(DotRes::Module { .. }) | Res::Handler | Res::Module(_) => None,
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
    fn references(
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
    fn prepare_rename(
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
    fn rename(
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
    fn symbols(&mut self, file: FileId) -> Result<Vec<lsp::DocumentSymbol>, QueryError> {
        let parse = self.db.get::<ParseFile>(file)?;
        let Some(ast) = &parse.ast else {
            return Ok(Vec::new());
        };
        Ok(convert::symbols(self.db.files(), ast))
    }

    /// The document's canonical-format whole-file edit, or an empty edit
    /// list when the source already carries the canonical form.
    fn format(&mut self, file: FileId) -> Result<Option<Vec<lsp::TextEdit>>, QueryError> {
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
    fn on_type_format(
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
    fn semantic_tokens(&mut self, file: FileId) -> Result<Vec<lsp::SemanticToken>, QueryError> {
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

/// Spawns the engine's worker thread and returns its command channel.
///
/// The thread owns the query database for its whole life: engine values
/// hold `Rc` tables and never cross threads, and the protocol layer
/// holds only the `Send` channel. The stack is generous (16 MiB) for the
/// checker's recursive walks, mirroring the parse seam's own
/// dedicated-stack arrangement.
fn spawn_worker(linked: SurfaceProvider) -> mpsc::Sender<Request> {
    let (tx, rx) = mpsc::channel::<Request>();
    std::thread::Builder::new()
        .stack_size(16 << 20)
        .spawn(move || worker(&rx, linked))
        .expect("the engine worker thread spawns");
    tx
}

/// The worker loop: one session, one command at a time, replies through
/// each command's oneshot. Engine errors and panics become reply
/// strings, never thread deaths; `shutdown` drops the session (so its
/// database dies before the reply lands) and ends the thread.
fn worker(rx: &mpsc::Receiver<Request>, linked: SurfaceProvider) {
    let mut session = Some(Session::with_linked(linked));
    while let Ok(request) = rx.recv() {
        if let Request::Shutdown { reply } = request {
            session.take();
            reply.send(Ok(())).ok();
            return;
        }
        handle(
            request,
            session.as_mut().expect("shutdown is the last command"),
        );
    }
}

/// Runs one command body, answering an internal error when the engine
/// panics: the worker must outlive any command bug, so the session's
/// database keeps serving later commands.
fn guarded<T>(body: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    catch_unwind(AssertUnwindSafe(body))
        .unwrap_or_else(|_| Err("internal error: the command panicked".to_owned()))
}

/// Answers one non-shutdown command against the session. Each body runs
/// behind [`guarded`] with its reply channel held outside the guard, so
/// a panic answers an internal error and the worker keeps serving.
fn handle(request: Request, session: &mut Session) {
    match request {
        Request::Open {
            path,
            uri,
            text,
            reply,
        } => {
            let answer = guarded(move || {
                session
                    .open(&path, uri, &text)
                    .map_err(|error| error.to_string())
            });
            reply.send(answer).ok();
        },
        Request::Set { path, text, reply } => {
            let answer =
                guarded(move || session.set(&path, &text).map_err(|error| error.to_string()));
            reply.send(answer).ok();
        },
        Request::Edit {
            path,
            changes,
            reply,
        } => {
            let answer = guarded(move || session.edit(&path, &changes));
            reply.send(answer).ok();
        },
        Request::Diagnostics { path, reply } => {
            let answer = guarded(move || {
                session.file(&path).map_or_else(
                    || Ok(Vec::new()),
                    |file| session.diagnostics(file).map_err(|error| error.to_string()),
                )
            });
            reply.send(answer).ok();
        },
        Request::Hover {
            path,
            line,
            character,
            reply,
        } => {
            let answer = guarded(move || {
                session.file(&path).map_or(Ok(None), |file| {
                    session
                        .hover(&path, file, line, character)
                        .map_err(|error| error.to_string())
                })
            });
            reply.send(answer).ok();
        },
        Request::Definition {
            path,
            line,
            character,
            reply,
        } => {
            let answer = guarded(move || {
                session.file(&path).map_or_else(
                    || Ok(Vec::new()),
                    |file| {
                        session
                            .definition(file, line, character)
                            .map_err(|error| error.to_string())
                    },
                )
            });
            reply.send(answer).ok();
        },
        Request::Completion {
            path,
            line,
            character,
            reply,
        } => {
            let answer = guarded(move || {
                session.file(&path).map_or_else(
                    || Ok(Vec::new()),
                    |file| {
                        session
                            .completion(file, line, character)
                            .map_err(|error| error.to_string())
                    },
                )
            });
            reply.send(answer).ok();
        },
        Request::References {
            path,
            line,
            character,
            include_declaration,
            reply,
        } => {
            let answer = guarded(move || {
                session.file(&path).map_or_else(
                    || Ok(Vec::new()),
                    |file| {
                        session
                            .references(file, line, character, include_declaration)
                            .map_err(|error| error.to_string())
                    },
                )
            });
            reply.send(answer).ok();
        },
        Request::PrepareRename {
            path,
            line,
            character,
            reply,
        } => {
            let answer = guarded(move || {
                session.file(&path).map_or_else(
                    || {
                        Ok(RenameAnswer::Refused(
                            "no open document carries the position".to_owned(),
                        ))
                    },
                    |file| {
                        session
                            .prepare_rename(file, line, character)
                            .map_err(|error| error.to_string())
                    },
                )
            });
            reply.send(answer).ok();
        },
        Request::Rename {
            path,
            line,
            character,
            new_name,
            reply,
        } => {
            let answer = guarded(move || {
                session.file(&path).map_or_else(
                    || {
                        Ok(RenameAnswer::Refused(
                            "no open document carries the position".to_owned(),
                        ))
                    },
                    |file| {
                        session
                            .rename(file, line, character, &new_name)
                            .map_err(|error| error.to_string())
                    },
                )
            });
            reply.send(answer).ok();
        },
        Request::Symbols { path, reply } => {
            let answer = guarded(move || {
                session.file(&path).map_or_else(
                    || Ok(Vec::new()),
                    |file| session.symbols(file).map_err(|error| error.to_string()),
                )
            });
            reply.send(answer).ok();
        },
        Request::Format { path, reply } => {
            let answer = guarded(move || {
                session.file(&path).map_or(Ok(None), |file| {
                    session.format(file).map_err(|error| error.to_string())
                })
            });
            reply.send(answer).ok();
        },
        Request::OnTypeFormat {
            path,
            line,
            character,
            ch,
            reply,
        } => {
            let answer = guarded(move || {
                session.file(&path).map_or(Ok(None), |file| {
                    session
                        .on_type_format(file, line, character, &ch)
                        .map_err(|error| error.to_string())
                })
            });
            reply.send(answer).ok();
        },
        Request::Tokens { path, reply } => {
            let answer = guarded(move || {
                session.file(&path).map_or_else(
                    || Ok(Vec::new()),
                    |file| {
                        session
                            .semantic_tokens(file)
                            .map_err(|error| error.to_string())
                    },
                )
            });
            reply.send(answer).ok();
        },
        Request::Shutdown { .. } => unreachable!("shutdown is handled in the loop"),
    }
}

/// Serves the language-server protocol over the given streams until the
/// client shuts the server down or disconnects.
///
/// The streams are the CLI's stdin/stdout and the tests' in-process
/// duplex pair; one engine worker thread backs the server.
///
/// # Errors
///
/// Stream failures only; the engine's diagnostics are published as data.
pub async fn serve<I, O>(stdin: I, stdout: O) -> Result<()>
where
    I: tokio::io::AsyncRead + Unpin,
    O: tokio::io::AsyncWrite + Unpin,
{
    serve_with(stdin, stdout, no_surface()).await
}

/// [`serve`] over the embedder's linked-surface provider: the CLI hands
/// the provider that serves the `.dsi` artifacts a run of the edited
/// package's graph cached, so hover over an import-bound name answers
/// its signature line.
///
/// # Errors
///
/// Stream failures only; the engine's diagnostics are published as data.
pub async fn serve_with<I, O>(stdin: I, stdout: O, linked: SurfaceProvider) -> Result<()>
where
    I: tokio::io::AsyncRead + Unpin,
    O: tokio::io::AsyncWrite + Unpin,
{
    let (service, socket) = LspService::new(move |client| server::Backend::spawn(client, linked));
    Server::new(stdin, stdout, socket).serve(service).await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open(session: &mut Session, path: &str, text: &str) -> FileId {
        let uri = lsp::Url::parse("file:///tmp/ds_lsp_session.ds").expect("the test URI parses");
        session
            .open(Path::new(path), uri, text)
            .expect("the document opens")
    }

    /// The ambient is the package's first member: stdlib names resolve
    /// in every document, and its own diagnostics carry no label in the
    /// documents, so nothing ambient publishes anywhere.
    #[test]
    fn ambient_resolves_and_stays_filtered() {
        let mut session = Session::new();
        // `trim` is an ambient spelling; a document using it checks clean
        // because the ambient resolves the name.
        let file = open(
            &mut session,
            "a.ds",
            "const text: string = trim(\" hi \")\n",
        );
        let diags = session.diagnostics(file).expect("the pass runs");
        assert!(diags.is_empty(), "{diags:?}");
    }

    /// The pooled-body refusal (E3) publishes in the editor like the
    /// other classes: a `par_map` element body touching the world
    /// surface answers its diagnostic in the document.
    #[test]
    fn a_pooled_body_refusal_publishes_in_the_document() {
        let mut session = Session::new();
        let file = open(
            &mut session,
            "pooled.ds",
            "fn main() -> list[int] {\n  const xs = [1, 2]\n  xs.par_map((x: int) => {\n    \
             println(\"elem\")\n    x\n  })\n}\n",
        );
        let diags = session.diagnostics(file).expect("the pass runs");
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert!(
            diags[0].message.contains("world surface"),
            "{}",
            diags[0].message
        );
    }

    /// A clean pooled program publishes nothing: the clean callable's
    /// walk answers no diagnostic.
    #[test]
    fn a_clean_pooled_program_publishes_nothing() {
        let mut session = Session::new();
        let file = open(
            &mut session,
            "pooled.ds",
            "fn main() -> list[int] {\n  const xs = [1, 2]\n  xs.par_map((x: int) => x * 2)\n}\n",
        );
        let diags = session.diagnostics(file).expect("the pass runs");
        assert!(diags.is_empty(), "{diags:?}");
    }

    /// Two open documents share one package namespace, so a duplicate
    /// name is reported in both (correct G3 semantics, not a defect).
    #[test]
    fn duplicate_across_documents_reports_in_both() {
        let mut session = Session::new();
        let a = open(&mut session, "a.ds", "const one = 1\n");
        let b = open(&mut session, "b.ds", "const one = 2\n");
        let da = session.diagnostics(a).expect("the pass runs");
        let db = session.diagnostics(b).expect("the pass runs");
        assert_eq!(da.len(), 1, "the first file owns its duplicate: {da:?}");
        assert_eq!(db.len(), 1, "the second file owns its duplicate: {db:?}");
        assert!(da[0].message.contains("one"), "{}", da[0].message);
    }

    /// File ids are never reused: closing empties the content and
    /// reopening restores the same id, so the package keeps its shape.
    #[test]
    fn close_and_reopen_keeps_the_file_id() {
        let mut session = Session::new();
        let a = open(&mut session, "a.ds", "const one = 1\n");
        session.set(Path::new("a.ds"), "").expect("the close runs");
        let again = open(&mut session, "a.ds", "const one = 2\n");
        assert_eq!(a, again);
    }

    /// One ranged change splices into the held document and a rangeless
    /// change replaces the whole text, so diagnostics track the edit.
    #[test]
    fn incremental_edits_move_the_diagnostics() {
        let mut session = Session::new();
        let file = open(&mut session, "a.ds", "const one: string = \"hi\"\n");
        assert!(
            session.diagnostics(file).expect("the pass runs").is_empty(),
            "the source opens clean"
        );
        let edit = |line, start, end, text: &str| lsp::TextDocumentContentChangeEvent {
            range: Some(lsp::Range::new(
                lsp::Position::new(line, start),
                lsp::Position::new(line, end),
            )),
            range_length: None,
            text: text.to_owned(),
        };
        // `string` sits at columns 12..18: replacing it with `int`
        // mismatches the string initializer.
        session
            .edit(Path::new("a.ds"), &[edit(0, 11, 17, "int")])
            .expect("the edit applies");
        let diags = session.diagnostics(file).expect("the pass runs");
        assert_eq!(diags.len(), 1, "{diags:?}");
        // An inverted range is refused instead of spliced.
        assert!(
            session
                .edit(Path::new("a.ds"), &[edit(0, 5, 2, "x")])
                .is_err(),
            "an inverted range is refused"
        );
        // A rangeless change replaces the whole text (the full-text
        // client's path) and the mismatch clears.
        session
            .edit(Path::new("a.ds"), &[lsp::TextDocumentContentChangeEvent {
                range: None,
                range_length: None,
                text: "const one: string = \"hi\"\n".to_owned(),
            }])
            .expect("the change applies");
        assert!(
            session.diagnostics(file).expect("the pass runs").is_empty(),
            "the rangeless change replaced the whole text"
        );
    }

    /// The content array applies in order: an insertion shifts what a
    /// later change's range addresses.
    #[test]
    fn the_change_array_applies_in_order() {
        let mut session = Session::new();
        let file = open(&mut session, "a.ds", "const one: string = \"hi\"\n");
        let edit = |line, start, end, text: &str| lsp::TextDocumentContentChangeEvent {
            range: Some(lsp::Range::new(
                lsp::Position::new(line, start),
                lsp::Position::new(line, end),
            )),
            range_length: None,
            text: text.to_owned(),
        };
        // First the annotation breaks on line 0, then a leading comment
        // line shifts the mismatch down: each range addresses the text
        // as it stands when its change applies.
        session
            .edit(Path::new("a.ds"), &[
                edit(0, 11, 17, "int"),
                edit(0, 0, 0, "// lead\n"),
            ])
            .expect("the edits apply");
        let diags = session.diagnostics(file).expect("the pass runs");
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].range.start.line, 1);
    }

    /// A multibyte document edits through UTF-16 columns: an insert
    /// before an astral character never lands inside its bytes, and the
    /// character's own two-column range deletes it whole.
    #[test]
    fn incremental_edits_count_utf16_columns() {
        let mut session = Session::new();
        let file = open(&mut session, "a.ds", "const flag: string = \"\u{1f680}\"\n");
        let edit = |line, start, end, text: &str| lsp::TextDocumentContentChangeEvent {
            range: Some(lsp::Range::new(
                lsp::Position::new(line, start),
                lsp::Position::new(line, end),
            )),
            range_length: None,
            text: text.to_owned(),
        };
        // The literal opens at column 21, so the rocket occupies UTF-16
        // columns 22..24: an insert at 22 lands before it, and slicing
        // inside its bytes would fail the parse outright.
        session
            .edit(Path::new("a.ds"), &[edit(0, 22, 22, "x")])
            .expect("the insert applies");
        assert!(
            session.diagnostics(file).expect("the pass runs").is_empty(),
            "the string literal stays whole"
        );
        // The insert moved the rocket to columns 23..25, so its own
        // two-column range deletes it whole and leaves a clean literal.
        session
            .edit(Path::new("a.ds"), &[edit(0, 23, 25, "")])
            .expect("the delete applies");
        assert!(
            session.diagnostics(file).expect("the pass runs").is_empty(),
            "the rocket deleted whole leaves a clean literal"
        );
    }

    /// A change for a document never opened syncs nothing (the protocol
    /// orders `didOpen` first).
    #[test]
    fn edits_for_never_opened_documents_sync_nothing() {
        let mut session = Session::new();
        session
            .edit(Path::new("b.ds"), &[lsp::TextDocumentContentChangeEvent {
                range: None,
                range_length: None,
                text: "const one = 1\n".to_owned(),
            }])
            .expect("a change before any open syncs nothing");
    }

    /// Hover answers the checked type at the position, rendered through
    /// the analysis universe, with the definition's effect row.
    #[test]
    fn hover_answers_the_checked_type() {
        let mut session = Session::new();
        let file = open(&mut session, "a.ds", "const msg: string = \"hi\"\n");
        // The position sits inside the string literal (chars 21..25),
        // whose checked type is the ambient's `string`.
        let answer = session
            .hover(Path::new("a.ds"), file, 0, 22)
            .expect("the pass runs")
            .expect("the string literal covers the position");
        assert!(answer.contents.contains("string"), "{}", answer.contents);
        assert!(answer.contents.contains("~ pure"), "{}", answer.contents);
    }

    /// Hovering an ambient name answers its `.dsi` signature spelling —
    /// the line `ds info` prints for the declaration — ahead of the
    /// checked-expression answer.
    #[test]
    fn hover_answers_the_dsi_signature_of_an_ambient_name() {
        let mut session = Session::new();
        let file = open(
            &mut session,
            "a.ds",
            "const text: string = trim(\" hi \")\n",
        );
        // `trim` sits at chars 21..25; the cursor lands on its `r`.
        let answer = session
            .hover(Path::new("a.ds"), file, 0, 22)
            .expect("the pass runs")
            .expect("the call covers the position");
        assert!(
            answer
                .contents
                .contains("pub extern fn trim(s: string) -> string"),
            "{}",
            answer.contents
        );
        // The checked-expression answer still follows the signature.
        assert!(answer.contents.contains("string"), "{}", answer.contents);
    }

    /// Hovering a use of another user document's `pub` declaration
    /// answers its signature spelling; the defining document stays
    /// unopened, so the declaration's own parse still serves the line.
    #[test]
    fn hover_answers_the_dsi_signature_across_documents() {
        let mut session = Session::new();
        open(
            &mut session,
            "lib.ds",
            "pub fn open_api(mut xs: list[int]) -> int | none {\n  first(xs)\n}\n",
        );
        let user = open(
            &mut session,
            "main.ds",
            "fn main() -> int | none {\n  open_api([1])\n}\n",
        );
        // `open_api` sits at line 1 chars 2..10; the cursor lands on its `p`.
        let answer = session
            .hover(Path::new("main.ds"), user, 1, 5)
            .expect("the pass runs")
            .expect("the call covers the position");
        assert!(
            answer
                .contents
                .contains("pub fn open_api(mut xs: list[int]) -> int | none"),
            "{}",
            answer.contents
        );
    }

    /// A non-exported declaration has no `.dsi` line: hovering a use of
    /// it answers the checked expression alone, never a guessed surface
    /// (`pub(pkg)` and private stay out of the .dsi by G4).
    #[test]
    fn hover_stays_honest_for_non_exported_declarations() {
        let mut session = Session::new();
        let file = open(
            &mut session,
            "a.ds",
            "fn hidden() -> int {\n  7\n}\n\nfn main() -> int {\n  hidden()\n}\n",
        );
        // `hidden` in main's body sits at line 5 chars 2..8; the cursor
        // lands on its `i`.
        let answer = session
            .hover(Path::new("a.ds"), file, 5, 4)
            .expect("the pass runs")
            .expect("the call covers the position");
        assert!(
            !answer.contents.contains("pub "),
            "a private fn renders no .dsi line: {}",
            answer.contents
        );
        assert!(answer.contents.contains("int"), "{}", answer.contents);
    }

    /// One linked surface the provider tier serves: the `@lib` fixture's
    /// exported `pick`, spelled as its cached `.dsi` renders it.
    fn lib_surface() -> std::sync::Arc<LinkedSurface> {
        std::sync::Arc::new(LinkedSurface {
            signatures: std::collections::HashMap::from([(
                "pick".to_owned(),
                "pub fn pick(n: int) -> int".to_owned(),
            )]),
        })
    }

    fn linking_session() -> Session {
        Session::with_linked(std::sync::Arc::new(|doc: &Path, link: &str| {
            (link == "@lib" && doc.file_name().is_some_and(|name| name == "main.ds"))
                .then(lib_surface)
        }))
    }

    /// Hovering a name an import selects — a use the session's
    /// package-free resolution leaves unknown — answers the linked
    /// tier's `.dsi` line from the embedder's provider, at the use and
    /// at the selection itself alike.
    #[test]
    fn hover_answers_a_linked_names_signature() {
        let mut session = linking_session();
        let file = open(
            &mut session,
            "main.ds",
            "import @lib (pick)\n\nfn main() -> int {\n  pick(3)\n}\n",
        );
        // `pick` in the body sits at line 3 chars 2..6; the cursor lands
        // on its `i`.
        let answer = session
            .hover(Path::new("main.ds"), file, 3, 4)
            .expect("the pass runs")
            .expect("the linked tier answers");
        assert!(
            answer.contents.contains("pub fn pick(n: int) -> int"),
            "{}",
            answer.contents
        );
        // the selection in the import row answers the same line
        let answer = session
            .hover(Path::new("main.ds"), file, 0, 15)
            .expect("the pass runs")
            .expect("the selection answers");
        assert!(
            answer.contents.contains("pub fn pick(n: int) -> int"),
            "{}",
            answer.contents
        );
    }

    /// The linked tier never displaces the session's own answers: an
    /// ambient name resolves through the ambient's parse and keeps its
    /// own `.dsi` line even when a provider offers a bogus one for the
    /// same name, and a word no import selects answers nothing.
    #[test]
    fn the_linked_tier_never_displaces_resolution() {
        let mut session = Session::with_linked(std::sync::Arc::new(|_doc, link| {
            (link == "@std").then(|| {
                std::sync::Arc::new(LinkedSurface {
                    signatures: std::collections::HashMap::from([(
                        "trim".to_owned(),
                        "pub fn trim(s: bogus) -> bogus".to_owned(),
                    )]),
                })
            })
        }));
        let file = open(
            &mut session,
            "a.ds",
            "const text: string = trim(\" hi \")\n",
        );
        let answer = session
            .hover(Path::new("a.ds"), file, 0, 22)
            .expect("the pass runs")
            .expect("the ambient name answers");
        assert!(
            answer
                .contents
                .contains("pub extern fn trim(s: string) -> string"),
            "the ambient path answers through the ambient's own parse: {}",
            answer.contents
        );
        assert!(
            !answer.contents.contains("bogus"),
            "the provider never displaces a resolved name: {}",
            answer.contents
        );
        // a word no import selects answers no `.dsi` line: an unresolved
        // name's hover keeps whatever checked-expression answer covers it
        // (here the enclosing body's annotated type) and never a
        // provider's guess
        let file = open(&mut session, "c.ds", "fn main() -> int {\n  nowhere\n}\n");
        let answer = session
            .hover(Path::new("c.ds"), file, 1, 4)
            .expect("the pass runs");
        if let Some(answer) = answer {
            assert!(
                !answer.contents.contains("pub "),
                "an unknown name no import selects renders no surface line: {}",
                answer.contents
            );
        }
    }

    /// Completion answers the names visible at the position: the
    /// ambient's declarations, the package's others, then the local
    /// scopes innermost-first, with shadowing.
    #[test]
    fn completion_answers_the_visible_names() {
        let mut session = Session::new();
        let file = open(
            &mut session,
            "a.ds",
            concat!(
                "const top = 1\n",
                "fn add(n: int) -> int {\n",
                "  const m = n + top\n",
                "  m + n\n",
                "}\n",
            ),
        );
        let items = session.completion(file, 3, 5).expect("the pass runs");
        let labels: Vec<&str> = items.iter().map(|item| item.label.as_str()).collect();
        let idx = |name: &str| {
            labels
                .iter()
                .position(|got| *got == name)
                .unwrap_or_else(|| panic!("`{name}` answers: {labels:?}"))
        };
        // An ambient spelling answers ahead of the document's own
        // declarations (the ambient registers as the first member).
        assert!(idx("trim") < idx("top"), "{labels:?}");
        assert!(idx("top") < idx("add"), "{labels:?}");
        // Locals come last, innermost-first: the block binding ahead of
        // the enclosing parameter.
        assert!(idx("add") < idx("m"), "{labels:?}");
        assert!(idx("m") < idx("n"), "{labels:?}");
        // Kinds answer too: the parameter and the block binding are
        // variables, the top-level binding a constant.
        assert_eq!(
            items[idx("n")].kind,
            Some(tower_lsp::lsp_types::CompletionItemKind::VARIABLE)
        );
        assert_eq!(
            items[idx("top")].kind,
            Some(tower_lsp::lsp_types::CompletionItemKind::CONSTANT)
        );
    }

    /// A local shadows the package value declaration of its name: one
    /// item answers, the local's.
    #[test]
    fn completion_drops_shadowed_names() {
        let mut session = Session::new();
        let file = open(
            &mut session,
            "a.ds",
            "const same = 1\nfn f(same: int) -> int { same }\n",
        );
        let items = session
            .completion(file, 1, 25)
            .expect("the pass runs")
            .into_iter()
            .filter(|item| item.label == "same")
            .collect::<Vec<_>>();
        assert_eq!(items.len(), 1, "{items:?}");
        assert_eq!(
            items[0].kind,
            Some(tower_lsp::lsp_types::CompletionItemKind::VARIABLE)
        );
    }

    /// A member position answers the receiver's members: the struct's
    /// fields, the inherent impl's member fns, and the UFCS free fns
    /// whose first parameter takes the receiver's type.
    #[test]
    fn completion_answers_members_of_a_struct_receiver() {
        let mut session = Session::new();
        let file = open(
            &mut session,
            "a.ds",
            concat!(
                "struct point { x: int, y: int }\n",
                "impl point {\n",
                "  fn norm(self) -> int {\n",
                "    self.x\n",
                "  }\n",
                "}\n",
                "fn pick(p: point) -> int {\n",
                "  p.x\n",
                "}\n",
            ),
        );
        // The cursor sits between the dot and the member of `p.x`.
        let items = session.completion(file, 7, 4).expect("the pass runs");
        let labels: Vec<&str> = items.iter().map(|item| item.label.as_str()).collect();
        // Fields first (declaration order), then the inherent member, then
        // the UFCS candidate (`p.pick()` feeds `pick`'s first parameter).
        assert_eq!(labels, ["x", "y", "norm", "pick"], "{items:?}");
        assert_eq!(items[0].kind, Some(lsp::CompletionItemKind::FIELD));
        assert_eq!(items[2].kind, Some(lsp::CompletionItemKind::METHOD));
        assert_eq!(items[3].kind, Some(lsp::CompletionItemKind::FUNCTION));
    }

    /// A `self` receiver inside an inherent impl answers the impl's own
    /// target members: the fields and the impl's fns.
    #[test]
    fn completion_answers_a_self_receiver_inside_an_inherent_impl() {
        let mut session = Session::new();
        let file = open(
            &mut session,
            "a.ds",
            concat!(
                "struct point { x: int, y: int }\n",
                "impl point {\n",
                "  fn norm(self) -> int {\n",
                "    self.x\n",
                "  }\n",
                "}\n",
            ),
        );
        // The cursor sits between the dot and the member of `self.x`.
        let items = session.completion(file, 3, 9).expect("the pass runs");
        let labels: Vec<&str> = items.iter().map(|item| item.label.as_str()).collect();
        assert_eq!(labels, ["x", "y", "norm"], "{items:?}");
        assert_eq!(items[0].kind, Some(lsp::CompletionItemKind::FIELD));
        assert_eq!(items[2].kind, Some(lsp::CompletionItemKind::METHOD));
    }

    /// A primitive receiver answers the ambient surface: trait members
    /// through the solved instances, the inherent `default` member, and
    /// the UFCS extern fns taking the primitive.
    #[test]
    fn completion_answers_ufcs_and_trait_candidates_of_a_string_receiver() {
        let mut session = Session::new();
        let file = open(
            &mut session,
            "a.ds",
            "fn clean(s: string) -> string {\n  s.trim()\n}\n",
        );
        // The cursor sits between the dot and the member of `s.trim`.
        let items = session.completion(file, 1, 4).expect("the pass runs");
        let kind = |name: &str| {
            items
                .iter()
                .find(|item| item.label == name)
                .map(|item| item.kind)
        };
        assert_eq!(
            kind("trim"),
            Some(Some(lsp::CompletionItemKind::FUNCTION)),
            "the UFCS extern fn answers as a function: {items:?}"
        );
        assert_eq!(
            kind("show"),
            Some(Some(lsp::CompletionItemKind::METHOD)),
            "the ambient `show` impl answers as a method: {items:?}"
        );
        assert_eq!(
            kind("default"),
            Some(Some(lsp::CompletionItemKind::METHOD)),
            "the ambient inherent member answers: {items:?}"
        );
    }

    /// A type position answers the in-scope type variables ahead of the
    /// visible type names, and never the value names; `Self` answers
    /// inside an impl.
    #[test]
    fn completion_answers_type_variables_in_type_positions() {
        let mut session = Session::new();
        let file = open(
            &mut session,
            "a.ds",
            concat!(
                "fn first[t](v: t) -> t? {\n",
                "  none\n",
                "}\n",
                "struct point { x: int }\n",
                "impl point {\n",
                "  fn twin(self) -> Self {\n",
                "    self\n",
                "  }\n",
                "}\n",
            ),
        );
        // The cursor sits inside the return annotation's `t` of `t?`.
        let items = session.completion(file, 0, 22).expect("the pass runs");
        assert_eq!(items[0].label, "t", "{items:?}");
        assert_eq!(items[0].kind, Some(lsp::CompletionItemKind::TYPE_PARAMETER));
        let labels: Vec<&str> = items.iter().map(|item| item.label.as_str()).collect();
        // Type names answer; the value names of the package never do.
        for name in ["string", "int", "list", "point"] {
            assert!(labels.contains(&name), "`{name}` answers: {labels:?}");
        }
        for name in ["first", "trim"] {
            assert!(!labels.contains(&name), "`{name}` stays out: {labels:?}");
        }
        // The cursor sits inside the `Self` return of the impl member:
        // `Self` is a type variable in scope there.
        let items = session.completion(file, 5, 21).expect("the pass runs");
        let labels: Vec<&str> = items.iter().map(|item| item.label.as_str()).collect();
        assert!(labels.contains(&"Self"), "{labels:?}");
        let self_item = items
            .iter()
            .find(|item| item.label == "Self")
            .expect("pinned");
        assert_eq!(
            self_item.kind,
            Some(lsp::CompletionItemKind::TYPE_PARAMETER)
        );
    }

    /// Every answer ranks by the typed prefix: exact prefix first, then
    /// word-boundary matches, then substrings, then the rest.
    #[test]
    fn completion_ranks_by_the_typed_prefix() {
        let mut session = Session::new();
        let file = open(
            &mut session,
            "a.ds",
            concat!(
                "const my_trim = 1\n",
                "const extra = 2\n",
                "const other = 3\n",
                "fn probe() -> int {\n",
                "  tr\n",
                "}\n",
            ),
        );
        // The cursor sits right after the typed prefix `tr`.
        let items = session.completion(file, 4, 4).expect("the pass runs");
        let labels: Vec<&str> = items.iter().map(|item| item.label.as_str()).collect();
        let idx = |name: &str| {
            labels
                .iter()
                .position(|got| *got == name)
                .unwrap_or_else(|| panic!("`{name}` answers: {labels:?}"))
        };
        // The ambient's `trim` and `trap` carry the exact prefix — the
        // two ranking ahead of the underscore boundary match in either
        // order (their relative order follows the ambient's declaration
        // order, which a composed-tail migration moves); the substring
        // match follows that; the non-matching rest answers last.
        assert!(idx("trim") < idx("my_trim"), "{labels:?}");
        assert!(idx("trap") < idx("my_trim"), "{labels:?}");
        assert!(idx("my_trim") < idx("extra"), "{labels:?}");
        assert!(idx("extra") < idx("other"), "{labels:?}");
    }

    /// A member position with no checked receiver falls back to the
    /// ordinary scope answer and never guesses members: an unresolved
    /// receiver, and a trailing dot the parse refuses, both answer scope
    /// names without panicking.
    #[test]
    fn completion_falls_back_honestly_without_a_checked_receiver() {
        let mut session = Session::new();
        let file = open(&mut session, "a.ds", "const broken = missing.x\n");
        // The cursor sits between the dot and the member of `missing.x`;
        // the receiver has no checked type, so the scope answer serves.
        let items = session.completion(file, 0, 22).expect("the pass runs");
        let labels: Vec<&str> = items.iter().map(|item| item.label.as_str()).collect();
        assert!(
            labels.contains(&"trim"),
            "an ambient name answers: {labels:?}"
        );
        assert!(
            labels.contains(&"broken"),
            "the package name answers: {labels:?}"
        );
        // A trailing dot refuses the parse: the whole file answers through
        // the degraded path, and the position still answers scope names.
        let tail = open(&mut session, "b.ds", "const tail = \"hi\".\n");
        let items = session.completion(tail, 0, 18).expect("the pass runs");
        let labels: Vec<&str> = items.iter().map(|item| item.label.as_str()).collect();
        assert!(
            labels.contains(&"trim"),
            "an ambient name answers: {labels:?}"
        );
    }

    /// The fixed skeletons answer behind a typed keyword prefix at the
    /// site the probe parses name: the declaration spellings at item
    /// level, the expression skeletons inside a body, the fn skeleton
    /// alone inside an impl or trait member list — each carrying its
    /// expansion in snippet format.
    #[test]
    fn snippets_answer_at_the_probe_decided_site() {
        fn snippet<'a>(
            items: &'a [lsp::CompletionItem],
            label: &str,
        ) -> Option<&'a lsp::CompletionItem> {
            items.iter().find(|item| {
                item.label == label && item.kind == Some(lsp::CompletionItemKind::SNIPPET)
            })
        }
        let mut session = Session::new();
        // Top level after `st`: the struct skeleton, never a body's
        // expression skeletons.
        let item = open(&mut session, "a.ds", "const one = 1\nst\n");
        let items = session.completion(item, 1, 2).expect("the pass runs");
        let skeleton = snippet(&items, "struct").expect("the struct skeleton answers");
        assert!(
            skeleton
                .insert_text
                .as_deref()
                .is_some_and(|text| text.starts_with("struct ${1:name}"))
        );
        assert_eq!(
            skeleton.insert_text_format,
            Some(lsp::InsertTextFormat::SNIPPET)
        );
        assert!(
            snippet(&items, "if").is_none(),
            "a body skeleton stays out: {items:?}"
        );
        assert!(snippet(&items, "spawn").is_none(), "{items:?}");
        // Inside a fn body after `if`: the expression skeleton, never an
        // item-level declaration.
        let body = open(&mut session, "b.ds", "fn probe() {\n  if\n}\n");
        let items = session.completion(body, 1, 4).expect("the pass runs");
        let skeleton = snippet(&items, "if").expect("the if skeleton answers");
        assert!(
            skeleton
                .insert_text
                .as_deref()
                .is_some_and(|text| text.starts_with("if ${1:cond}"))
        );
        assert!(snippet(&items, "struct").is_none(), "{items:?}");
        // Inside an impl member list after `fn`: the fn skeleton alone.
        let member = open(&mut session, "c.ds", "impl point {\n  fn\n}\n");
        let items = session.completion(member, 1, 4).expect("the pass runs");
        let skeleton = snippet(&items, "fn").expect("the member fn skeleton answers");
        assert!(
            skeleton
                .insert_text
                .as_deref()
                .is_some_and(|text| text.starts_with("fn ${1:name}"))
        );
        assert!(snippet(&items, "struct").is_none(), "{items:?}");
        assert!(snippet(&items, "if").is_none(), "{items:?}");
    }

    /// The skeletons ride the ranked scope answer — behind every
    /// same-prefix name match — and nothing snippet-shaped answers on an
    /// empty prefix.
    #[test]
    fn snippets_rank_behind_prefix_matches_and_skip_the_empty_prefix() {
        let mut session = Session::new();
        let file = open(
            &mut session,
            "a.ds",
            "const spree = 1\nfn probe() {\n  sp\n}\n",
        );
        // The cursor sits right after the typed prefix `sp`.
        let items = session.completion(file, 2, 4).expect("the pass runs");
        let labels: Vec<&str> = items.iter().map(|item| item.label.as_str()).collect();
        let idx = |name: &str| {
            labels
                .iter()
                .position(|got| *got == name)
                .unwrap_or_else(|| panic!("`{name}` answers: {labels:?}"))
        };
        assert!(
            idx("spree") < idx("spawn"),
            "the name match outranks the skeleton: {labels:?}"
        );
        assert!(
            idx("spawn") < idx("probe"),
            "the skeleton still outranks the non-matching rest: {labels:?}"
        );
        // The same position before any typed character (the cursor behind
        // the word start) answers no snippet at all.
        let items = session.completion(file, 2, 2).expect("the pass runs");
        assert!(
            items
                .iter()
                .all(|item| item.kind != Some(lsp::CompletionItemKind::SNIPPET)),
            "{items:?}"
        );
    }

    /// No skeleton answers where the server serves a position-specific
    /// answer instead of the scope list: a member position (the checked
    /// receiver's members, and the honest scope fallback behind an
    /// unchecked dot alike) and a type position.
    #[test]
    fn snippets_stay_out_of_member_and_type_positions() {
        let mut session = Session::new();
        let file = open(
            &mut session,
            "a.ds",
            concat!(
                "struct point { x: int, y: int }\n",
                "fn pick(p: point) -> int {\n",
                "  p.x\n",
                "}\n",
                "fn first[t](v: t) -> t? {\n",
                "  none\n",
                "}\n",
            ),
        );
        let no_snippets = |items: &[lsp::CompletionItem]| {
            assert!(
                items
                    .iter()
                    .all(|item| item.kind != Some(lsp::CompletionItemKind::SNIPPET)),
                "{items:?}"
            );
        };
        // The checked member position of `p.x`.
        no_snippets(&session.completion(file, 2, 4).expect("the pass runs"));
        // The type position inside the `t?` return annotation.
        no_snippets(&session.completion(file, 4, 22).expect("the pass runs"));
        // The unchecked-dot fallback: a member position no receiver
        // answers, however keyword-shaped the typed prefix.
        let broken = open(&mut session, "b.ds", "const broken = missing.fn\n");
        no_snippets(&session.completion(broken, 0, 24).expect("the pass runs"));
    }

    /// The auto-import tier offers the exported ambient members behind an
    /// unresolved typed prefix: each prefix-matching item carries the edit
    /// that opens the document with the `@std` import row, and accepting
    /// the offer leaves a document that checks clean. A private ambient
    /// name — the scope answer still lists it — never carries an edit.
    #[test]
    fn auto_import_offers_the_exported_ambient_members() {
        let mut session = Session::new();
        let file = open(
            &mut session,
            "a.ds",
            "fn probe() -> string {\n  to_up(\"x\")\n}\n",
        );
        // The cursor sits right after the typed prefix `to_up` on line 1.
        let items = session.completion(file, 1, 7).expect("the pass runs");
        let item = items
            .iter()
            .find(|item| item.label == "to_upper")
            .expect("the exported ambient member answers");
        let edits = item
            .additional_text_edits
            .as_ref()
            .expect("the offer imports");
        assert_eq!(edits.len(), 1, "{edits:?}");
        assert_eq!(
            edits[0].range,
            lsp::Range::new(lsp::Position::new(0, 0), lsp::Position::new(0, 0))
        );
        assert_eq!(edits[0].new_text, "import @std (to_upper)\n\n");
        // Accepting the offer — the client inserts the name and applies
        // the edit — parses and validates clean: the row selects a `pub`
        // ambient member.
        let accepted = "import @std (to_upper)\n\nfn probe() -> string {\n  to_upper(\"x\")\n}\n";
        session
            .set(Path::new("a.ds"), accepted)
            .expect("the accept applies");
        assert!(
            session.diagnostics(file).expect("the pass runs").is_empty(),
            "the imported document checks clean"
        );
        // The private ambient helper never carries an edit: the tier's
        // gate is the checker's own export predicate.
        let file = open(&mut session, "b.ds", "fn probe() -> int {\n  json_w\n}\n");
        let items = session.completion(file, 1, 8).expect("the pass runs");
        let item = items
            .iter()
            .find(|item| item.label == "json_ws")
            .expect("the scope answer still lists the private name");
        assert!(
            item.additional_text_edits.is_none(),
            "a private name never imports: {item:?}"
        );
    }

    /// The offer merges into a plain existing `@std` row instead of
    /// duplicating it: the edit appends the name to the row's selection,
    /// and the accepted document validates clean.
    #[test]
    fn auto_import_merges_into_a_plain_std_row() {
        let mut session = Session::new();
        let file = open(
            &mut session,
            "a.ds",
            "import @std/io (println)\nconst text: string = to_up\n",
        );
        // The cursor sits right after the typed prefix on line 1.
        let items = session.completion(file, 1, 26).expect("the pass runs");
        let item = items
            .iter()
            .find(|item| item.label == "to_upper")
            .expect("the exported ambient member answers");
        let edits = item
            .additional_text_edits
            .as_ref()
            .expect("the offer imports");
        assert_eq!(
            edits[0].range,
            lsp::Range::new(lsp::Position::new(0, 23), lsp::Position::new(0, 23)),
            "the edit anchors after the row's last selection"
        );
        assert_eq!(edits[0].new_text, ", to_upper");
        let accepted = "import @std/io (println, to_upper)\nconst text: string = to_upper(\"x\")\n";
        session
            .set(Path::new("a.ds"), accepted)
            .expect("the accept applies");
        assert!(
            session.diagnostics(file).expect("the pass runs").is_empty(),
            "the merged row checks clean"
        );
    }

    /// The tier stays off behind a resolved prefix (the typed prefix
    /// names a visible name), an empty prefix, a member position, and a
    /// row that already selects the name: the offers serve unknown bare
    /// names alone.
    #[test]
    fn auto_import_stays_off_behind_resolved_positions() {
        let mut session = Session::new();
        // The file's row already selects `to_upper`: the item answers
        // plain, nothing to import.
        let file = open(
            &mut session,
            "a.ds",
            "import @std (to_upper)\n\nconst text: string = to_up\n",
        );
        let items = session.completion(file, 2, 26).expect("the pass runs");
        let item = items
            .iter()
            .find(|item| item.label == "to_upper")
            .expect("the name answers");
        assert!(
            item.additional_text_edits.is_none(),
            "an already-imported name carries no edit: {item:?}"
        );
        // A resolved prefix: the typed prefix spells the visible
        // `to_upper` itself.
        let file = open(&mut session, "b.ds", "const text: string = to_upper\n");
        let items = session.completion(file, 0, 29).expect("the pass runs");
        let item = items
            .iter()
            .find(|item| item.label == "to_upper")
            .expect("the name answers");
        assert!(
            item.additional_text_edits.is_none(),
            "a resolved prefix carries no edit: {item:?}"
        );
        // An empty prefix (the cursor before the word) attaches nothing.
        let file = open(&mut session, "c.ds", "const text: string = nope\n");
        let items = session.completion(file, 0, 21).expect("the pass runs");
        assert!(
            items
                .iter()
                .all(|item| item.additional_text_edits.is_none()),
            "{items:?}"
        );
        // A member position never takes the tier — the unchecked-dot
        // fallback included: importing a name serves no dot.
        let file = open(&mut session, "d.ds", "const broken = missing.to_up\n");
        let items = session.completion(file, 0, 28).expect("the pass runs");
        let item = items
            .iter()
            .find(|item| item.label == "to_upper")
            .expect("the scope answer serves the fallback");
        assert!(
            item.additional_text_edits.is_none(),
            "a member position carries no edit: {item:?}"
        );
    }

    /// A type position takes the tier over the type namespace alone: the
    /// prefix-matching exported type carries the import edit, and no
    /// value-namespace item does.
    #[test]
    fn auto_import_serves_type_positions() {
        let mut session = Session::new();
        let file = open(
            &mut session,
            "a.ds",
            "fn probe(x: file_han) -> int {\n  1\n}\n",
        );
        // The cursor sits right after `file_han` in the parameter's type.
        let items = session.completion(file, 0, 20).expect("the pass runs");
        let item = items
            .iter()
            .find(|item| item.label == "file_handle")
            .expect("the ambient type answers");
        let edits = item
            .additional_text_edits
            .as_ref()
            .expect("the offer imports");
        assert_eq!(edits[0].new_text, "import @std (file_handle)\n\n");
        assert!(
            items.iter().all(|item| {
                item.additional_text_edits.is_none() || item.label == "file_handle"
            }),
            "only the type's own item carries the edit: {items:?}"
        );
    }

    /// The composed candidate set serves the `@std` package's own members
    /// under the merged registration: the members ride inside the ambient,
    /// so the scope answer lists them and the tier enriches those items
    /// with the same row edit — a prefix that spells a member exactly names
    /// a visible name, so nothing imports. Accepting an offer leaves a
    /// document that checks clean: the merged namespace validates the
    /// row's selection.
    #[test]
    fn auto_import_offers_the_std_members_under_the_merged_registration() {
        let mut session = Session::new();
        // The value-namespace member behind a partial prefix.
        let file = open(
            &mut session,
            "a.ds",
            "fn probe_urls() -> list[string] {\n  url\n}\n",
        );
        // The cursor sits right after the typed prefix `url` on line 1.
        let items = session.completion(file, 1, 5).expect("the pass runs");
        let item = items
            .iter()
            .find(|item| item.label == "urls")
            .expect("the std member answers");
        let edits = item
            .additional_text_edits
            .as_ref()
            .expect("the offer imports");
        assert_eq!(edits[0].new_text, "import @std (urls)\n\n");
        // Accepting the offer checks clean: the row selects the exported
        // member, and the body reads it.
        let accepted = "import @std (urls)\n\nfn probe_urls() -> list[string] {\n  urls\n}\n";
        session
            .set(Path::new("a.ds"), accepted)
            .expect("the accept applies");
        assert!(
            session.diagnostics(file).expect("the pass runs").is_empty(),
            "the imported document checks clean"
        );
        // The type-namespace members answer at a type position behind a
        // partial prefix: the config struct constructs and reads, while
        // the opaque response binds its name in a signature — its value
        // uses keep the M3 refusal, a checker matter that never gates the
        // offer.
        let file = open(
            &mut session,
            "b.ds",
            "fn probe_config(c: confi) -> int {\n  1\n}\n",
        );
        // The cursor sits right after `confi` in the parameter's type.
        let items = session.completion(file, 0, 24).expect("the pass runs");
        let item = items
            .iter()
            .find(|item| item.label == "config")
            .expect("the std member answers");
        assert_eq!(
            item.additional_text_edits
                .as_ref()
                .expect("the offer imports")[0]
                .new_text,
            "import @std (config)\n\n"
        );
        let accepted =
            "import @std (config)\n\nfn probe_config(c: config) -> int {\n  c.retries\n}\n";
        session
            .set(Path::new("b.ds"), accepted)
            .expect("the accept applies");
        assert!(
            session.diagnostics(file).expect("the pass runs").is_empty(),
            "the imported annotation checks clean"
        );
        // The opaque member's name binds the same way: the offer carries
        // the edit, the accepted signature the name alone — no value use
        // exists to construct over.
        let file = open(
            &mut session,
            "d.ds",
            "fn probe_response(r: respo) -> int {\n  1\n}\n",
        );
        let items = session.completion(file, 0, 26).expect("the pass runs");
        let item = items
            .iter()
            .find(|item| item.label == "response")
            .expect("the std member answers");
        assert_eq!(
            item.additional_text_edits
                .as_ref()
                .expect("the offer imports")[0]
                .new_text,
            "import @std (response)\n\n"
        );
        let accepted =
            "import @std (response)\n\nfn probe_response(r: response) -> int {\n  1\n}\n";
        session
            .set(Path::new("d.ds"), accepted)
            .expect("the accept applies");
        assert!(
            session.diagnostics(file).expect("the pass runs").is_empty(),
            "the opaque member's name binds in a signature"
        );
        // A prefix that spells a member exactly names a visible name under
        // the merged reading: nothing imports.
        let file = open(
            &mut session,
            "c.ds",
            "fn probe_row(x: row) -> int {\n  1\n}\n",
        );
        let items = session.completion(file, 0, 19).expect("the pass runs");
        let item = items
            .iter()
            .find(|item| item.label == "row")
            .expect("the merged reading keeps the member visible");
        assert!(
            item.additional_text_edits.is_none(),
            "a resolved prefix carries no edit: {item:?}"
        );
    }

    /// The core-shaped registration (a declaring graph's ambient) carries
    /// none of the migrated members: the tier's members' table serves
    /// their offers there, each member joining the answer as its own item
    /// behind any prefix it matches — an exact spelling included, since
    /// the name stays unknown bare over the core. The ambient's own
    /// members keep their offers, a row that already selects a member
    /// answers none, and the session runs no link pass, so the row's own
    /// validation stays in the graph's compile (never asserted clean
    /// here).
    #[test]
    fn auto_import_offers_the_std_members_over_the_core_ambient() {
        let mut session = Session::over_core_ambient();
        // The exact spelling is the widening's own case: over the core
        // `row` is unknown bare, so the tier fires and offers the member.
        let file = open(&mut session, "a.ds", "fn probe(x: row) -> int {\n  1\n}\n");
        // The cursor sits right after `row` in the parameter's type.
        let items = session.completion(file, 0, 15).expect("the pass runs");
        let item = items
            .iter()
            .find(|item| item.label == "row")
            .expect("the members' table offers the unknown name");
        let edits = item
            .additional_text_edits
            .as_ref()
            .expect("the offer imports");
        assert_eq!(edits[0].new_text, "import @std (row)\n\n");
        // The member's own kind rides the item: an alias renders through
        // the protocol's struct kind.
        assert_eq!(item.kind, Some(lsp::CompletionItemKind::STRUCT));
        // The value-namespace member joins at a bare-name position the
        // same way, its fresh row opening the document.
        let file = open(
            &mut session,
            "b.ds",
            "fn probe() -> list[string] {\n  url\n}\n",
        );
        let items = session.completion(file, 1, 5).expect("the pass runs");
        let item = items
            .iter()
            .find(|item| item.label == "urls")
            .expect("the value member offers");
        let edits = item
            .additional_text_edits
            .as_ref()
            .expect("the offer imports");
        assert_eq!(
            edits[0].range,
            lsp::Range::new(lsp::Position::new(0, 0), lsp::Position::new(0, 0))
        );
        assert_eq!(edits[0].new_text, "import @std (urls)\n\n");
        // A row that already selects the member answers no edit: the
        // file imports it, the graph's link pass owning the binding.
        let file = open(
            &mut session,
            "c.ds",
            "import @std (row)\n\nfn probe(x: row) -> int {\n  1\n}\n",
        );
        let items = session.completion(file, 2, 15).expect("the pass runs");
        assert!(
            items.iter().all(|item| item.label != "row"),
            "an already-selected member offers nothing: {items:?}"
        );
        // The ambient's own members keep their offers over the core: the
        // composed set's ambient half. `concat` is the representative —
        // the core string face's own seam, still core-owned after the
        // string face's migration (the migrated spellings stay unoffered
        // over a core-shaped registration until the LSP gains implicit
        // resolution, the same state the json face's names ride).
        let file = open(
            &mut session,
            "d.ds",
            "fn probe() -> string {\n  conc(\"x\")\n}\n",
        );
        let items = session.completion(file, 1, 6).expect("the pass runs");
        let item = items
            .iter()
            .find(|item| item.label == "concat")
            .expect("the ambient member answers");
        assert_eq!(
            item.additional_text_edits
                .as_ref()
                .expect("the offer imports")[0]
                .new_text,
            "import @std (concat)\n\n"
        );
    }

    /// Definition follows the resolution side table to the defining name
    /// occurrence, as a location in the same document.
    #[test]
    fn definition_answers_the_defining_name() {
        let mut session = Session::new();
        // An absolute path: the client's file:// URIs always decode to
        // absolute on-disk paths, and the definition answer needs the
        // reverse mapping.
        let path =
            std::env::temp_dir().join(format!("ds_lsp_definition-{}.ds", std::process::id()));
        let uri = lsp::Url::from_file_path(&path).expect("an absolute path has a URI");
        let file = session
            .open(
                &path,
                uri.clone(),
                "fn greet(name: string) -> string { name }\nconst shout = greet(\"hi\")\n",
            )
            .expect("the document opens");
        // Position inside the `greet` use of the second line.
        let locations = session.definition(file, 1, 16).expect("the pass runs");
        assert_eq!(locations.len(), 1, "{locations:?}");
        // The defining `greet` sits on line 1 (0-based 0) at chars 3..9
        // of the same document.
        assert_eq!(locations[0].uri, uri);
        assert_eq!(locations[0].range.start.line, 0);
        assert_eq!(locations[0].range.start.character, 3);
        assert_eq!(locations[0].range.end.character, 8);
    }

    /// Opens one document under its own URI spelling, so answers
    /// distinguish files.
    fn open_uri(session: &mut Session, path: &str, text: &str) -> (FileId, lsp::Url) {
        let uri = lsp::Url::parse(&format!("file:///tmp/{path}")).expect("the test URI parses");
        let file = session
            .open(Path::new(path), uri.clone(), text)
            .expect("the document opens");
        (file, uri)
    }

    /// References answer every use resolving to the same target across
    /// documents, plus the declaration when asked, and never another
    /// scope's same-named target.
    #[test]
    fn references_answer_across_documents_and_skip_same_named_targets() {
        let mut session = Session::new();
        let (_a, uri_a) = open_uri(
            &mut session,
            "a.ds",
            "const base = 1\nfn uses() -> int { base }\n",
        );
        let (b, uri_b) = open_uri(
            &mut session,
            "b.ds",
            "const copy = base\nfn shadows(base: int) -> int { base }\n",
        );
        // The position sits on b's use of a's definition.
        let locations = session.references(b, 0, 14, true).expect("the pass runs");
        let got: Vec<(lsp::Url, lsp::Position)> = locations
            .iter()
            .map(|location| (location.uri.clone(), location.range.start))
            .collect();
        assert_eq!(
            got,
            vec![
                (uri_a.clone(), lsp::Position::new(0, 6)),
                (uri_a.clone(), lsp::Position::new(1, 19)),
                (uri_b.clone(), lsp::Position::new(0, 13)),
            ],
            "the declaration and both uses answer, sorted by file"
        );
        // Without the declaration only the uses answer.
        let locations = session.references(b, 0, 14, false).expect("the pass runs");
        assert_eq!(locations.len(), 2, "{locations:?}");
        // References on the parameter answer the binder and its body use
        // alone, both inside b, never the package definition's uses.
        let locations = session.references(b, 1, 13, true).expect("the pass runs");
        assert!(
            locations.iter().all(|location| location.uri == uri_b),
            "the parameter's references stay in its own file: {locations:?}"
        );
        let got: Vec<lsp::Position> = locations
            .iter()
            .map(|location| location.range.start)
            .collect();
        assert_eq!(
            got,
            vec![lsp::Position::new(1, 11), lsp::Position::new(1, 31)],
            "the binder and its own use only"
        );
    }

    /// Rename answers a plain identifier substitution over the
    /// declaration and every use, across documents, from both the
    /// declaration and a use.
    #[test]
    fn rename_edits_the_declaration_and_every_use() {
        let mut session = Session::new();
        let (a, uri_a) = open_uri(
            &mut session,
            "a.ds",
            "const base = 1\nfn uses() -> int { base }\n",
        );
        let (b, uri_b) = open_uri(&mut session, "b.ds", "const copy = base\n");
        // Prepare from the declaration answers its range and spelling.
        match session.prepare_rename(a, 0, 8).expect("the pass runs") {
            RenameAnswer::Prepare(range, name) => {
                assert_eq!(name, "base");
                assert_eq!(
                    range,
                    lsp::Range::new(lsp::Position::new(0, 6), lsp::Position::new(0, 10))
                );
            },
            other => panic!("the declaration prepares: {other:?}"),
        }
        // Rename from the use in b answers the same edit set.
        match session
            .rename(b, 0, 14, "foundation")
            .expect("the pass runs")
        {
            RenameAnswer::Edit(edit) => {
                let changes = edit.changes.expect("the edit carries changes");
                let a_edits = &changes[&uri_a];
                assert_eq!(a_edits.len(), 2, "{a_edits:?}");
                assert_eq!(
                    a_edits[0].range,
                    lsp::Range::new(lsp::Position::new(0, 6), lsp::Position::new(0, 10))
                );
                assert_eq!(
                    a_edits[1].range,
                    lsp::Range::new(lsp::Position::new(1, 19), lsp::Position::new(1, 23))
                );
                assert_eq!(a_edits[0].new_text, "foundation");
                let b_edits = &changes[&uri_b];
                assert_eq!(b_edits.len(), 1, "{b_edits:?}");
                assert_eq!(
                    b_edits[0].range,
                    lsp::Range::new(lsp::Position::new(0, 13), lsp::Position::new(0, 17))
                );
            },
            other => panic!("the use renames: {other:?}"),
        }
    }

    /// Rename of a local answers the binder and its uses, including
    /// every or-pattern alternative of the binder's scope.
    #[test]
    fn rename_edits_a_local_binder_and_its_uses() {
        let mut session = Session::new();
        let (a, uri_a) = open_uri(
            &mut session,
            "a.ds",
            "fn pick(v: int) -> int {\n  match v { a | a => a }\n}\n",
        );
        for (line, character) in [(1, 12), (1, 21)] {
            match session
                .rename(a, line, character, "both")
                .expect("the pass runs")
            {
                RenameAnswer::Edit(edit) => {
                    let edits = &edit.changes.expect("the edit carries changes")[&uri_a];
                    // Both alternatives bind one name, so all three
                    // occurrences edit from either the binder or the use:
                    // the two binders and the body use.
                    assert_eq!(edits.len(), 3, "{edits:?}");
                    let starts: Vec<lsp::Position> =
                        edits.iter().map(|edit| edit.range.start).collect();
                    assert_eq!(starts, vec![
                        lsp::Position::new(1, 12),
                        lsp::Position::new(1, 16),
                        lsp::Position::new(1, 21),
                    ]);
                },
                other => panic!("the binder and the use rename alike: {other:?}"),
            }
        }
    }

    /// Rename of a registered member covers its own declaration, every
    /// impl of its trait, its qualified `Trait::f` use, and the dot-call
    /// sites the checker's type-directed selection names — across
    /// documents, from the declaration and the call site alike.
    #[test]
    fn rename_edits_a_member_declaration_impls_and_call_sites() {
        let mut session = Session::new();
        let (a, uri_a) = open_uri(
            &mut session,
            "a.ds",
            concat!(
                "trait pretty {\n",
                "  fn show(self) -> string { \"pretty\" }\n",
                "}\n",
                "struct dot { v: int }\n",
            ),
        );
        let (b, uri_b) = open_uri(
            &mut session,
            "b.ds",
            concat!(
                "impl pretty for dot {\n",
                "  fn show(self) -> string { \"dot\" }\n",
                "}\n",
                "fn render(d: dot) -> string {\n",
                "  pretty::show(d)\n",
                "}\n",
                "fn call(d: dot) -> string {\n",
                "  d.show()\n",
                "}\n",
            ),
        );
        // Prepare at the dot-call names the callee and its spelling.
        match session.prepare_rename(b, 7, 6).expect("the pass runs") {
            RenameAnswer::Prepare(range, name) => {
                assert_eq!(name, "show");
                assert_eq!(
                    range,
                    lsp::Range::new(lsp::Position::new(7, 4), lsp::Position::new(7, 8))
                );
            },
            other => panic!("a selected dot-call prepares: {other:?}"),
        }
        for (file, line, character) in [(b, 7, 6), (b, 4, 10), (a, 1, 6), (b, 1, 6)] {
            match session
                .rename(file, line, character, "display")
                .expect("the pass runs")
            {
                RenameAnswer::Edit(edit) => {
                    let changes = edit.changes.expect("the edit carries changes");
                    let a_edits = &changes[&uri_a];
                    assert_eq!(a_edits.len(), 1, "{a_edits:?}");
                    assert_eq!(
                        a_edits[0].range,
                        lsp::Range::new(lsp::Position::new(1, 5), lsp::Position::new(1, 9)),
                        "the trait member declaration renames"
                    );
                    let b_edits = &changes[&uri_b];
                    assert_eq!(b_edits.len(), 3, "{b_edits:?}");
                    assert_eq!(
                        b_edits[0].range,
                        lsp::Range::new(lsp::Position::new(1, 5), lsp::Position::new(1, 9)),
                        "the impl's member renames"
                    );
                    assert_eq!(
                        b_edits[1].range,
                        lsp::Range::new(lsp::Position::new(4, 10), lsp::Position::new(4, 14)),
                        "the qualified use renames"
                    );
                    assert_eq!(
                        b_edits[2].range,
                        lsp::Range::new(lsp::Position::new(7, 4), lsp::Position::new(7, 8)),
                        "the dot-call renames"
                    );
                    assert_eq!(a_edits[0].new_text, "display");
                },
                other => panic!("every member occurrence renames from {other:?}"),
            }
        }
        // References at the dot-call answer the same set, the declaration
        // side included when asked: the trait member, the impl member, the
        // qualified use, and the call site.
        let locations = session.references(b, 7, 6, true).expect("the pass runs");
        let got: Vec<(lsp::Url, lsp::Position)> = locations
            .iter()
            .map(|location| (location.uri.clone(), location.range.start))
            .collect();
        assert_eq!(
            got,
            vec![
                (uri_a.clone(), lsp::Position::new(1, 5)),
                (uri_b.clone(), lsp::Position::new(1, 5)),
                (uri_b.clone(), lsp::Position::new(4, 10)),
                (uri_b.clone(), lsp::Position::new(7, 4)),
            ],
            "the declaration side, the qualified use, and the call site answer"
        );
        let locations = session.references(b, 7, 6, false).expect("the pass runs");
        assert_eq!(locations.len(), 2, "{locations:?}");
    }

    /// Rename of a receiverless inherent member covers its declaration
    /// and its type-keyed call site (the bare type name as the
    /// receiver), from either side.
    #[test]
    fn rename_edits_a_receiverless_member_and_its_type_keyed_call() {
        let mut session = Session::new();
        let (a, uri_a) = open_uri(
            &mut session,
            "a.ds",
            concat!(
                "struct point { x: int }\n",
                "impl point {\n",
                "  fn origin_x() -> int { 0 }\n",
                "}\n",
                "const fixed = point.origin_x()\n",
            ),
        );
        for (line, character) in [(2, 6), (4, 22)] {
            match session
                .rename(a, line, character, "x0")
                .expect("the pass runs")
            {
                RenameAnswer::Edit(edit) => {
                    let edits = &edit.changes.expect("the edit carries changes")[&uri_a];
                    assert_eq!(edits.len(), 2, "{edits:?}");
                    let starts: Vec<lsp::Position> =
                        edits.iter().map(|edit| edit.range.start).collect();
                    assert_eq!(
                        starts,
                        vec![lsp::Position::new(2, 5), lsp::Position::new(4, 20)],
                        "the declaration and the type-keyed call edit alike"
                    );
                },
                other => panic!("the receiverless member renames: {other:?}"),
            }
        }
    }

    /// Rename refuses honestly: positions on no name, ambient names,
    /// extern names, ambient members, ambiguous dot-calls, and
    /// non-identifiers.
    #[test]
    fn rename_refusals_answer_honestly() {
        let mut session = Session::new();
        let (a, _uri) = open_uri(
            &mut session,
            "a.ds",
            concat!(
                "const text: string = trim(\" hi \")\n",
                "extern fn foreign(x: int) -> int\n",
                "const shown = text.show()\n",
                "trait a { fn f(self) -> int { 1 } }\n",
                "trait b { fn f(self) -> int { 2 } }\n",
                "tag unit\n",
                "impl a for unit { fn f(self) -> int { 1 } }\n",
                "impl b for unit { fn f(self) -> int { 2 } }\n",
                "const v = unit\n",
                "const n = v.f()\n",
            ),
        );
        // A position on the `const` keyword names nothing.
        match session.prepare_rename(a, 0, 2).expect("the pass runs") {
            RenameAnswer::Refused(message) => {
                assert!(message.contains("names no definition or use"), "{message}");
            },
            other => panic!("a keyword position refuses: {other:?}"),
        }
        // The ambient's `trim` names a stdlib definition: no open
        // document can edit it, and both rename requests refuse with
        // the same message.
        match session.prepare_rename(a, 0, 22).expect("the pass runs") {
            RenameAnswer::Refused(message) => assert!(message.contains("ambient"), "{message}"),
            other => panic!("an ambient name refuses: {other:?}"),
        }
        match session.rename(a, 0, 22, "renamed").expect("the pass runs") {
            RenameAnswer::Refused(message) => assert!(message.contains("ambient"), "{message}"),
            other => panic!("an ambient name refuses the rename: {other:?}"),
        }
        // An extern's own declaration position refuses: extern names are
        // platform link spellings.
        match session.rename(a, 1, 12, "renamed").expect("the pass runs") {
            RenameAnswer::Refused(message) => assert!(message.contains("extern"), "{message}"),
            other => panic!("an extern name refuses: {other:?}"),
        }
        // A trait member of the ambient (selected at the dot-call
        // through the checker's own instance solve) lives in the stdlib
        // source: no open document can edit it, and both requests refuse.
        match session.prepare_rename(a, 2, 20).expect("the pass runs") {
            RenameAnswer::Refused(message) => assert!(message.contains("ambient"), "{message}"),
            other => panic!("an ambient member refuses: {other:?}"),
        }
        match session.rename(a, 2, 20, "renamed").expect("the pass runs") {
            RenameAnswer::Refused(message) => assert!(message.contains("ambient"), "{message}"),
            other => panic!("an ambient member refuses the rename: {other:?}"),
        }
        // A dot-call whose candidate set even type-direction cannot
        // settle (two traits claiming the same member name for one
        // receiver) failed its own check, so it names no member.
        match session.rename(a, 9, 12, "renamed").expect("the pass runs") {
            RenameAnswer::Refused(message) => {
                assert!(message.contains("did not settle"), "{message}");
            },
            other => panic!("an ambiguous dot-call refuses: {other:?}"),
        }
        // A fresh name must be a fresh identifier.
        match session.rename(a, 0, 8, "bad name").expect("the pass runs") {
            RenameAnswer::Refused(message) => {
                assert!(message.contains("not a fresh identifier"), "{message}");
            },
            other => panic!("a malformed name refuses: {other:?}"),
        }
        match session.rename(a, 0, 8, "match").expect("the pass runs") {
            RenameAnswer::Refused(message) => {
                assert!(message.contains("not a fresh identifier"), "{message}");
            },
            other => panic!("a keyword name refuses: {other:?}"),
        }
    }

    /// The panic harness answers an internal error instead of unwinding,
    /// so a panicking command cannot kill the worker thread.
    #[test]
    fn a_panicking_command_answers_an_internal_error() {
        let answer = guarded(|| -> Result<(), String> { panic!("an engine bug") });
        assert_eq!(answer.unwrap_err(), "internal error: the command panicked");
    }

    /// Positions past every bound answer instead of panicking: the clamp
    /// covers the hover, definition, and completion seams alike.
    #[test]
    fn extreme_positions_answer_instead_of_panicking() {
        let mut session = Session::new();
        let file = open(&mut session, "a.ds", "const one = 1\n");
        assert!(
            session
                .hover(Path::new("a.ds"), file, u32::MAX, u32::MAX)
                .is_ok(),
            "hover survives a u32::MAX position"
        );
        assert!(
            session.definition(file, u32::MAX, u32::MAX).is_ok(),
            "definition survives a u32::MAX position"
        );
        assert!(
            session.completion(file, u32::MAX, u32::MAX).is_ok(),
            "completion survives a u32::MAX position"
        );
        assert!(
            session.references(file, u32::MAX, u32::MAX, true).is_ok(),
            "references survive a u32::MAX position"
        );
        assert!(
            session.prepare_rename(file, u32::MAX, u32::MAX).is_ok(),
            "prepare-rename survives a u32::MAX position"
        );
        assert!(
            session.rename(file, u32::MAX, u32::MAX, "x").is_ok(),
            "rename survives a u32::MAX position"
        );
        assert!(
            session
                .on_type_format(file, u32::MAX, u32::MAX, "\n")
                .is_ok(),
            "on-type formatting survives a u32::MAX position"
        );
    }

    /// Applies one edit list to the document's source.
    fn apply(session: &Session, file: FileId, edits: &[lsp::TextEdit]) -> String {
        let files = session.db.files();
        let mut out = files.source(file).to_owned();
        for edit in edits.iter().rev() {
            let start = convert::offset(files, file, edit.range.start) as usize;
            let end = convert::offset(files, file, edit.range.end) as usize;
            out.replace_range(start..end, &edit.new_text);
        }
        out
    }

    /// A newline typed inside a declaration body formats the enclosing
    /// declaration: the body's mis-indentation repairs through one edit
    /// whose range starts past the shared header — never a
    /// whole-document edit — and applying it yields the canonical form.
    #[test]
    fn on_type_newline_formats_the_enclosing_declaration() {
        let mut session = Session::new();
        let source = "fn area(width: int) -> int {\nconst s = width\ns\n}\n";
        let file = open(&mut session, "a.ds", source);
        // Enter typed at the end of the first body line: the cursor
        // opens the next line inside the body.
        let edits = session
            .on_type_format(file, 2, 0, "\n")
            .expect("the pass runs")
            .expect("the parse is clean");
        assert_eq!(edits.len(), 1, "{edits:?}");
        // The shared header stays out of the edit: it starts on the
        // first body line, not at the document start.
        assert_eq!(edits[0].range.start.line, 1, "{edits:?}");
        assert_eq!(edits[0].range.start.character, 0, "{edits:?}");
        assert_eq!(
            apply(&session, file, &edits),
            "fn area(width: int) -> int {\n  const s = width\n  s\n}\n"
        );
    }

    /// A `}` closing a declaration formats that declaration alone: the
    /// header's spacing and the body's indent repair, and a
    /// mis-formatted neighbor stays untouched (the region discipline).
    #[test]
    fn on_type_brace_formats_the_closed_declaration() {
        let mut session = Session::new();
        let source = "const  one=1\nfn  greet( name:string ) ->string{\n\"hi\"\n}";
        let file = open(&mut session, "a.ds", source);
        // The typed `}` closes the fn; the cursor sits right behind it.
        let edits = session
            .on_type_format(file, 3, 1, "}")
            .expect("the pass runs")
            .expect("the parse is clean");
        assert_eq!(edits.len(), 1, "{edits:?}");
        assert_eq!(
            apply(&session, file, &edits),
            "const  one=1\nfn greet(name: string) -> string {\n  \"hi\"\n}",
            "the closed declaration repairs, the neighbor keeps its own form"
        );
    }

    /// A region already carrying the canonical form answers no edits,
    /// not an identity edit; so do an unregistered trigger character and
    /// a newline in the whitespace between declarations (no region names
    /// the position).
    #[test]
    fn on_type_answers_no_edits_when_nothing_changes() {
        let mut session = Session::new();
        let file = open(
            &mut session,
            "a.ds",
            "fn area(width: int) -> int {\n  const s = width\n  s\n}\n",
        );
        let edits = session
            .on_type_format(file, 2, 0, "\n")
            .expect("the pass runs")
            .expect("the parse is clean");
        assert!(edits.is_empty(), "an already-canonical region: {edits:?}");
        let edits = session
            .on_type_format(file, 2, 0, ";")
            .expect("the pass runs")
            .expect("the parse is clean");
        assert!(edits.is_empty(), "an unregistered trigger: {edits:?}");
        let other = open(&mut session, "b.ds", "const one = 1\n\n\nconst two = 2\n");
        let edits = session
            .on_type_format(other, 2, 0, "\n")
            .expect("the pass runs")
            .expect("the parse is clean");
        assert!(
            edits.is_empty(),
            "a gap position names no region: {edits:?}"
        );
    }

    /// A poisoned parse answers no edits: broken code never reformats
    /// the valid declarations around it (the whole-document formatter
    /// refuses the same tree).
    #[test]
    fn on_type_poisoned_parse_answers_no_edits() {
        let mut session = Session::new();
        let file = open(&mut session, "a.ds", "const one = 1\nconst broken = ,\n");
        match session.on_type_format(file, 1, 15, "\n") {
            Ok(None) => {},
            other => panic!("a poisoned parse answers no edits: {other:?}"),
        }
    }

    /// A duplicate within one document publishes both the primary range
    /// and the earlier definition as a related location under the
    /// document's own URI.
    #[test]
    fn a_same_file_duplicate_publishes_both_ranges() {
        let mut session = Session::new();
        let file = open(&mut session, "a.ds", "const one = 1\nconst one = 2\n");
        let diags = session.diagnostics(file).expect("the pass runs");
        assert_eq!(diags.len(), 1, "{diags:?}");
        let related = diags[0]
            .related_information
            .as_ref()
            .expect("the first definition rides as a related location");
        assert_eq!(related.len(), 1, "{related:?}");
        assert_eq!(related[0].location.range.start.line, 0);
        assert_eq!(related[0].location.range.start.character, 6);
        assert_eq!(related[0].location.range.end.character, 9);
    }

    /// Answers echo the client's own URI spelling: a spelling the decoded
    /// path cannot reproduce (explicit percent-encoding the path
    /// round-trip never emits) still comes back verbatim.
    #[test]
    fn answers_echo_the_client_uri_verbatim() {
        let mut session = Session::new();
        let path = std::env::temp_dir().join(format!("ds+lsp-{}.ds", std::process::id()));
        let uri = lsp::Url::from_file_path(&path).expect("an absolute path has a URI");
        let spelled = lsp::Url::parse(&uri.as_str().replace('+', "%2B"))
            .expect("the percent-encoded spelling parses");
        assert_ne!(
            spelled.as_str(),
            uri.as_str(),
            "the test needs a spelling the path round-trip cannot reproduce"
        );
        let file = session
            .open(
                &spelled.to_file_path().expect("the spelling decodes"),
                spelled.clone(),
                "const one = 1\nconst two = one\n",
            )
            .expect("the document opens");
        let locations = session.definition(file, 1, 13).expect("the pass runs");
        assert_eq!(locations.len(), 1, "{locations:?}");
        assert_eq!(locations[0].uri, spelled);
    }

    /// Byte offset of `units` UTF-16 code units into `slice`.
    fn utf16_to_byte(slice: &str, units: u32) -> usize {
        let mut seen = 0u32;
        for (i, ch) in slice.char_indices() {
            if seen >= units {
                return i;
            }
            seen += u32::try_from(ch.len_utf16()).expect("a UTF-16 length fits u32");
        }
        slice.len()
    }

    /// Decodes a delta stream back into (source text, type index,
    /// modifiers), so the exact token stream asserts in source terms.
    fn decode_tokens(source: &str, data: &[lsp::SemanticToken]) -> Vec<(String, u32, u32)> {
        let mut line_starts = vec![0usize];
        for (i, b) in source.bytes().enumerate() {
            if b == b'\n' {
                line_starts.push(i + 1);
            }
        }
        let mut line = 0usize;
        let mut char = 0u32;
        let mut out = Vec::new();
        for token in data {
            line += token.delta_line as usize;
            char = if token.delta_line == 0 { char + token.delta_start } else { token.delta_start };
            let line_start = line_starts[line];
            let start = line_start + utf16_to_byte(&source[line_start..], char);
            let end = start + utf16_to_byte(&source[start..], token.length);
            out.push((
                source[start..end].to_owned(),
                token.token_type,
                token.token_modifiers_bitset,
            ));
        }
        out
    }

    /// The whole-document token stream over a representative document:
    /// keywords, operators, comments (a doc run included), literals,
    /// locals with shadowing, declarations of every kind, an impl's
    /// `self`, a dot-call, and a type position with a variable — the
    /// exact stream, in source order.
    #[test]
    fn semantic_tokens_answer_the_whole_document() {
        use tokens::Tok as T;

        let mut session = Session::new();
        let source = concat!(
            "// a leading comment\n",
            "/// the greeting\n",
            "const msg: string = \"hi \u{1F680}\"\n",
            "const shouty = \"hey ${msg}\"\n",
            "const ready = true\n",
            "enum color { red, green }\n",
            "struct point { x: int, y: int }\n",
            "trait pretty {\n",
            "  fn show(self) -> string {\n",
            "    \"pretty\"\n",
            "  }\n",
            "}\n",
            "impl pretty for point {\n",
            "  fn show(self) -> string {\n",
            "    \"pt\"\n",
            "  }\n",
            "}\n",
            "impl point {\n",
            "  fn norm(self) -> int {\n",
            "    self.x + self.y\n",
            "  }\n",
            "}\n",
            "fn area(p: point, scale: int) -> int {\n",
            "  var total = p.x * scale + p.y\n",
            "  const scale = 2\n",
            "  total + scale\n",
            "}\n",
            "fn wrap[t](v: t) -> t? {\n",
            "  none\n",
            "}\n",
            "fn clean() -> string {\n",
            "  msg.trim()\n",
            "}\n",
            "tag marker\n",
            "const tint = red\n",
            "const mark = marker\n",
        );
        let file = open(&mut session, "a.ds", source);
        let data = session.semantic_tokens(file).expect("the pass runs");
        // The first token pins the coordinate math: the leading comment at
        // line 0, character 0, twenty UTF-16 units long.
        assert_eq!(data[0].delta_line, 0);
        assert_eq!(data[0].delta_start, 0);
        assert_eq!(data[0].length, 20);
        assert_eq!(data[0].token_type, tokens::Tok::Comment.index());

        let d = tokens::DECLARATION;
        let r = tokens::READONLY;
        let expected: Vec<(&str, u32, u32)> = vec![
            ("// a leading comment", T::Comment.index(), 0),
            ("/// the greeting", T::Comment.index(), 0),
            ("const", T::Keyword.index(), 0),
            ("msg", T::Variable.index(), d | r),
            ("string", T::EnumMember.index(), 0),
            ("=", T::Operator.index(), 0),
            ("\"hi \u{1F680}\"", T::Str.index(), 0),
            ("const", T::Keyword.index(), 0),
            ("shouty", T::Variable.index(), d | r),
            ("=", T::Operator.index(), 0),
            ("hey ", T::Str.index(), 0),
            ("msg", T::Variable.index(), r),
            ("const", T::Keyword.index(), 0),
            ("ready", T::Variable.index(), d | r),
            ("=", T::Operator.index(), 0),
            ("true", T::Keyword.index(), 0),
            ("enum", T::Keyword.index(), 0),
            ("color", T::Enum.index(), d),
            ("red", T::EnumMember.index(), d),
            ("green", T::EnumMember.index(), d),
            ("struct", T::Keyword.index(), 0),
            ("point", T::Struct.index(), d),
            ("x", T::Property.index(), d),
            ("int", T::EnumMember.index(), 0),
            ("y", T::Property.index(), d),
            ("int", T::EnumMember.index(), 0),
            ("trait", T::Keyword.index(), 0),
            ("pretty", T::Interface.index(), d),
            ("fn", T::Keyword.index(), 0),
            ("show", T::Method.index(), d),
            ("self", T::Keyword.index(), d),
            ("->", T::Operator.index(), 0),
            ("string", T::EnumMember.index(), 0),
            ("\"pretty\"", T::Str.index(), 0),
            ("impl", T::Keyword.index(), 0),
            ("pretty", T::Interface.index(), 0),
            ("for", T::Keyword.index(), 0),
            ("point", T::Struct.index(), 0),
            ("fn", T::Keyword.index(), 0),
            ("show", T::Method.index(), d),
            ("self", T::Keyword.index(), d),
            ("->", T::Operator.index(), 0),
            ("string", T::EnumMember.index(), 0),
            ("\"pt\"", T::Str.index(), 0),
            ("impl", T::Keyword.index(), 0),
            ("point", T::Struct.index(), 0),
            ("fn", T::Keyword.index(), 0),
            ("norm", T::Method.index(), d),
            ("self", T::Keyword.index(), d),
            ("->", T::Operator.index(), 0),
            ("int", T::EnumMember.index(), 0),
            ("self", T::Keyword.index(), 0),
            ("+", T::Operator.index(), 0),
            ("self", T::Keyword.index(), 0),
            ("fn", T::Keyword.index(), 0),
            ("area", T::Function.index(), d),
            ("p", T::Parameter.index(), d),
            ("point", T::Struct.index(), 0),
            ("scale", T::Parameter.index(), d),
            ("int", T::EnumMember.index(), 0),
            ("->", T::Operator.index(), 0),
            ("int", T::EnumMember.index(), 0),
            ("var", T::Keyword.index(), 0),
            ("total", T::Variable.index(), d),
            ("=", T::Operator.index(), 0),
            ("p", T::Parameter.index(), 0),
            ("*", T::Operator.index(), 0),
            ("scale", T::Parameter.index(), 0),
            ("+", T::Operator.index(), 0),
            ("p", T::Parameter.index(), 0),
            ("const", T::Keyword.index(), 0),
            ("scale", T::Variable.index(), d),
            ("=", T::Operator.index(), 0),
            ("2", T::Number.index(), 0),
            ("total", T::Variable.index(), 0),
            ("+", T::Operator.index(), 0),
            ("scale", T::Variable.index(), 0),
            ("fn", T::Keyword.index(), 0),
            ("wrap", T::Function.index(), d),
            ("t", T::TypeParam.index(), d),
            ("v", T::Parameter.index(), d),
            ("t", T::TypeParam.index(), 0),
            ("->", T::Operator.index(), 0),
            ("t", T::TypeParam.index(), 0),
            ("?", T::Operator.index(), 0),
            ("fn", T::Keyword.index(), 0),
            ("clean", T::Function.index(), d),
            ("->", T::Operator.index(), 0),
            ("string", T::EnumMember.index(), 0),
            ("msg", T::Variable.index(), r),
            ("trim", T::Function.index(), 0),
            ("tag", T::Keyword.index(), 0),
            ("marker", T::EnumMember.index(), d),
            ("const", T::Keyword.index(), 0),
            ("tint", T::Variable.index(), d | r),
            ("=", T::Operator.index(), 0),
            ("red", T::EnumMember.index(), 0),
            ("const", T::Keyword.index(), 0),
            ("mark", T::Variable.index(), d | r),
            ("=", T::Operator.index(), 0),
            ("marker", T::EnumMember.index(), 0),
        ];
        let got = decode_tokens(source, &data);
        let expected: Vec<(String, u32, u32)> = expected
            .into_iter()
            .map(|(text, ty, mods)| (text.to_owned(), ty, mods))
            .collect();
        assert_eq!(got, expected, "the exact token stream answers");
    }

    /// The honest boundaries: a name the side table records nothing for
    /// stays plain — an unresolved receiver, a bare field access, an
    /// unresolved dot-call member, and `none` — and a derive's
    /// synthesized trees never emit (the walk reads the raw parse tree,
    /// so a `derive(show)` struct carries no method tokens).
    #[test]
    fn semantic_tokens_stay_plain_where_nothing_answers() {
        let mut session = Session::new();
        let file = open(
            &mut session,
            "a.ds",
            "const broken = missing.x\nconst also = missing.y()\n",
        );
        let data = session.semantic_tokens(file).expect("the pass runs");
        let got = decode_tokens(
            "const broken = missing.x\nconst also = missing.y()\n",
            &data,
        );
        let texts: Vec<&str> = got.iter().map(|(text, ..)| text.as_str()).collect();
        for name in ["missing", "x", "y"] {
            assert!(!texts.contains(&name), "`{name}` stays plain: {got:?}");
        }
        assert!(
            texts.contains(&"also"),
            "the second binding's own declaration answers: {got:?}"
        );
        assert!(
            got.iter().any(|(text, ..)| text == "broken"),
            "the resolved binding still answers: {got:?}"
        );
        // The derive pin: a synthesized member would answer a method
        // token inside the attributed declaration's span.
        let derived = open(
            &mut session,
            "b.ds",
            "#[derive(show)]\nstruct pt { v: int }\n",
        );
        let data = session.semantic_tokens(derived).expect("the pass runs");
        let got = decode_tokens("#[derive(show)]\nstruct pt { v: int }\n", &data);
        assert!(
            got.iter()
                .all(|(_, ty, _)| *ty != tokens::Tok::Method.index()),
            "a derive's synthesized members never emit: {got:?}"
        );
        // `none` names the universe's unit atom; the resolver records no
        // use for it, so it stays plain.
        let optional = open(&mut session, "c.ds", "fn pick() -> int? {\n  none\n}\n");
        let data = session.semantic_tokens(optional).expect("the pass runs");
        let got = decode_tokens("fn pick() -> int? {\n  none\n}\n", &data);
        assert!(
            !got.iter().any(|(text, ..)| text == "none"),
            "`none` stays plain: {got:?}"
        );
    }

    /// The token stream is cached on the engine revision: two requests
    /// over the same revision answer equal streams and the second never
    /// walks again, an edit or a registration bumps the revision and the
    /// next request walks the changed state, and a didChange shipping
    /// identical content bumps nothing (the revision is the queries' own
    /// clock, not a second one).
    #[test]
    fn semantic_tokens_answer_from_the_revision_cache() {
        let mut session = Session::new();
        let file = open(&mut session, "a.ds", "const one = 1\n");
        let first = session.semantic_tokens(file).expect("the pass runs");
        assert_eq!(session.token_walks, 1, "the first request walks");
        let second = session.semantic_tokens(file).expect("the pass runs");
        assert_eq!(second, first, "the same revision answers the same stream");
        assert_eq!(session.token_walks, 1, "the second request hits the cache");
        // A didChange shipping identical content bumps no revision, so
        // the cache still answers.
        session
            .set(Path::new("a.ds"), "const one = 1\n")
            .expect("the no-op change runs");
        let third = session.semantic_tokens(file).expect("the pass runs");
        assert_eq!(third, first);
        assert_eq!(session.token_walks, 1, "an unchanged revision still hits");
        // An edit with new text bumps the revision: the cached stream is
        // stale and the next request walks the edited document.
        session
            .set(Path::new("a.ds"), "const one = 2\nconst two = one\n")
            .expect("the change runs");
        let fourth = session.semantic_tokens(file).expect("the pass runs");
        assert_eq!(session.token_walks, 2, "the edit invalidates the cache");
        assert_ne!(fourth, first, "the edited document answers its own stream");
        let got = decode_tokens("const one = 2\nconst two = one\n", &fourth);
        let texts: Vec<&str> = got.iter().map(|(text, ..)| text.as_str()).collect();
        assert!(texts.contains(&"two"), "the new binding colors: {got:?}");
        // Opening another document bumps the same global revision (the
        // stream reads package-grain queries), so this document's entry
        // invalidates too and the next request walks.
        let _other = open(&mut session, "b.ds", "const extra = one\n");
        let fifth = session.semantic_tokens(file).expect("the pass runs");
        assert_eq!(session.token_walks, 3, "a registration invalidates alike");
        assert_eq!(
            fifth, fourth,
            "the unchanged document still answers its stream"
        );
    }
}
