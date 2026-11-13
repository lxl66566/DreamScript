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
//! Full-text synchronization (the client ships the whole document per
//! edit; no incremental deltas), diagnostics from the package-grain query
//! values (parse, resolve, check, linearity, platform classes) published
//! on open and change, hover over the checked expression types rendered
//! through the analysis universe with the enclosing definition's effect
//! row, go-to-definition through the resolution side table, document
//! symbols from the parse tree, and whole-document formatting through
//! `ds-fmt`.
//!
//! # Honest boundaries
//!
//! * One package is all open documents (G3's shared namespace): a duplicate name across two open
//!   files is reported in both, which is correct package semantics, not a server defect.
//! * Script-mode files fail the library-mode parse (top-level statements are syntax errors); that
//!   stays honest until the loading round.
//! * Completion, rename, find-references, and semantic tokens are unimplemented: nothing in the
//!   queries answers them (references need a reverse edge over the resolution table; completion
//!   needs scope iteration).
//! * `.dsi` hover waits on the interface-artifact round (M7); hover answers the checked type of the
//!   expression at the position.
//! * Ambient stdlib names resolve (the ambient registers first), but the ambient file has no client
//!   URI, so definitions pointing into it answer none.
//! * No debounce: every open and change re-queries and publishes. The engine's memoization is the
//!   latency story for now.

#![warn(missing_docs)]

mod convert;
mod server;

use std::sync::mpsc;

use anyhow::Result;
use ds_check::{DefId as PackageDef, DotRes, Res};
use ds_diag::{FileId, Span};
use ds_query::{
    Analyzed, BlockIndex, CheckedPackage, Db, DefId, EffectsOf, PackageId, PackageLinearity,
    ParseFile, PlatformCheck, QueryError, Resolve,
};
use rustc_hash::FxHashMap;
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

/// The reply channel of one worker command.
type Reply<T> = oneshot::Sender<Result<T, String>>;

/// One command for the engine's worker thread, with the oneshot the
/// answer goes back through.
enum Request {
    /// A document opened: register it (or re-register a reopened id).
    Open {
        path: String,
        text: String,
        reply: Reply<FileId>,
    },
    /// A document's whole content replaced (did_change; did_close ships
    /// an empty text).
    Set {
        path: String,
        text: String,
        reply: Reply<()>,
    },
    /// The document's diagnostics.
    Diagnostics {
        path: String,
        reply: Reply<Vec<lsp::Diagnostic>>,
    },
    /// Hover at one LSP position.
    Hover {
        path: String,
        line: u32,
        character: u32,
        reply: Reply<Option<HoverAnswer>>,
    },
    /// Definition locations at one LSP position.
    Definition {
        path: String,
        line: u32,
        character: u32,
        reply: Reply<Vec<lsp::Location>>,
    },
    /// The document's top-level symbols.
    Symbols {
        path: String,
        reply: Reply<Vec<lsp::DocumentSymbol>>,
    },
    /// The document's canonical-format edits.
    Format {
        path: String,
        reply: Reply<Option<Vec<lsp::TextEdit>>>,
    },
    /// The worker drops the database and answers; the thread then exits.
    Shutdown { reply: Reply<()> },
}

/// The worker-thread state: the query database and the open-document
/// table. Everything runs on the worker thread; values never leave it
/// except through the `Send` reply payloads.
struct Session {
    db: Db,
    /// Document path (the decoded on-disk spelling of its `file://`
    /// URI) to file id; one entry per document the server has ever
    /// opened, so a close/reopen cycle reuses the id.
    docs: FxHashMap<String, FileId>,
}

impl Session {
    /// A session with the stdlib ambient registered as the package's
    /// first member (G3): stdlib names resolve in every document, and the
    /// ambient's file id never gains a client URI.
    fn new() -> Self {
        let mut db = Db::with_declared_rows(ds_stdlib::declared_rows().clone())
            .with_world_caps(ds_stdlib::world_caps().clone());
        let _ambient = db.add_file(ds_stdlib::AMBIENT_FILE, ds_stdlib::AMBIENT_SOURCE);
        Self {
            db,
            docs: FxHashMap::default(),
        }
    }

    /// The file id of an open (or once-opened) document.
    fn file(&self, path: &str) -> Option<FileId> {
        self.docs.get(path).copied()
    }

    /// The client URI of one file id, when the document was ever opened
    /// (the ambient answers none, so definitions into it stay honest).
    fn uri(&self, file: FileId) -> Option<lsp::Url> {
        let (path, _) = self.docs.iter().find(|(_, id)| **id == file)?;
        lsp::Url::from_file_path(path).ok()
    }

    /// Registers a document (or restores a reopened one onto its
    /// original file id).
    fn open(&mut self, path: &str, text: &str) -> Result<FileId, QueryError> {
        if let Some(&file) = self.docs.get(path) {
            self.db.set_file_content(file, text)?;
            return Ok(file);
        }
        let file = self.db.add_file(path, text);
        self.docs.insert(path.to_owned(), file);
        Ok(file)
    }

    /// Replaces a document's content; a change for a document never
    /// opened syncs nothing (the protocol orders `didOpen` first).
    fn set(&mut self, path: &str, text: &str) -> Result<(), QueryError> {
        if let Some(&file) = self.docs.get(path) {
            return self.db.set_file_content(file, text);
        }
        Ok(())
    }

    /// The document's diagnostics: the parse, resolve, check, linearity,
    /// and platform classes out of the package-grain query values, each
    /// class's set filtered to this file's labels (the ambient's
    /// diagnostics carry no label here and drop out), sorted by range.
    fn diagnostics(&mut self, file: FileId) -> Result<Vec<lsp::Diagnostic>, QueryError> {
        let Self { db, docs } = self;
        let uri_of = |file: FileId| -> Option<lsp::Url> {
            docs.iter()
                .find(|(_, id)| **id == file)
                .and_then(|(path, _)| lsp::Url::from_file_path(path).ok())
        };
        let parse = db.get::<ParseFile>(file)?;
        let resolved = db.get::<Resolve>(PackageId::new(0))?;
        let checked = db.get::<CheckedPackage>(PackageId::new(0))?;
        let linear = db.get::<PackageLinearity>(PackageId::new(0))?;
        let platform = db.get::<PlatformCheck>(PackageId::new(0))?;
        let mut out = Vec::new();
        for diag in parse
            .diagnostics
            .iter()
            .chain(resolved.diags.iter())
            .chain(checked.diags.iter())
            .chain(linear.diags.iter())
            .chain(platform.diags.iter())
        {
            if let Some(mapped) = convert::diagnostic(db.files(), &uri_of, diag, file) {
                out.push(mapped);
            }
        }
        out.sort_by_key(|diag| (diag.range.start.line, diag.range.start.character));
        Ok(out)
    }

    /// The checked type of the expression at the position, rendered
    /// through the analysis universe, with the enclosing definition's
    /// effect row appended.
    fn hover(
        &mut self,
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
        let Some(val) = best_val else {
            return Ok(None);
        };
        let universe = &analyzed.decls.universe;
        let mut contents = format!("```dreamscript\n{}\n```", universe.render(val.ty));
        // The effect row rides the definition grain the checker exposes:
        // the top-level definition whose name sits in the position's
        // block.
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
        let (start, end) = best.expect("a covering span exists with a value");
        Ok(Some(HoverAnswer {
            contents,
            range: convert::range(self.db.files(), Span::new(file, start, end)),
        }))
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
        let Some(index) = package.files().iter().position(|f| *f == file) else {
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
            Res::Dot(DotRes::Free { func, .. }) => vec![package.def(*func).span],
            Res::Dot(DotRes::Methods { candidates }) => candidates
                .iter()
                .map(|id| package.method(*id).span)
                .collect(),
            // The handler forms name no definition behind them.
            Res::Handler => Vec::new(),
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
}

/// Spawns the engine's worker thread and returns its command channel.
///
/// The thread owns the query database for its whole life: engine values
/// hold `Rc` tables and never cross threads, and the protocol layer
/// holds only the `Send` channel. The stack is generous (16 MiB) for the
/// checker's recursive walks, mirroring the parse seam's own
/// dedicated-stack arrangement.
fn spawn_worker() -> mpsc::Sender<Request> {
    let (tx, rx) = mpsc::channel::<Request>();
    std::thread::Builder::new()
        .stack_size(16 << 20)
        .spawn(move || worker(&rx))
        .expect("the engine worker thread spawns");
    tx
}

/// The worker loop: one session, one command at a time, replies through
/// each command's oneshot. Engine errors become reply strings, never
/// thread deaths; `shutdown` drops the session (so its database dies
/// before the reply lands) and ends the thread.
fn worker(rx: &mpsc::Receiver<Request>) {
    let mut session = Some(Session::new());
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

/// Answers one non-shutdown command against the session.
fn handle(request: Request, session: &mut Session) {
    match request {
        Request::Open { path, text, reply } => {
            let answer = session
                .open(&path, &text)
                .map_err(|error| error.to_string());
            reply.send(answer).ok();
        },
        Request::Set { path, text, reply } => {
            let answer = session.set(&path, &text).map_err(|error| error.to_string());
            reply.send(answer).ok();
        },
        Request::Diagnostics { path, reply } => {
            let answer = session.file(&path).map_or_else(
                || Ok(Vec::new()),
                |file| session.diagnostics(file).map_err(|error| error.to_string()),
            );
            reply.send(answer).ok();
        },
        Request::Hover {
            path,
            line,
            character,
            reply,
        } => {
            let answer = session.file(&path).map_or_else(
                || Ok(None),
                |file| {
                    session
                        .hover(file, line, character)
                        .map_err(|error| error.to_string())
                },
            );
            reply.send(answer).ok();
        },
        Request::Definition {
            path,
            line,
            character,
            reply,
        } => {
            let answer = session.file(&path).map_or_else(
                || Ok(Vec::new()),
                |file| {
                    session
                        .definition(file, line, character)
                        .map_err(|error| error.to_string())
                },
            );
            reply.send(answer).ok();
        },
        Request::Symbols { path, reply } => {
            let answer = session.file(&path).map_or_else(
                || Ok(Vec::new()),
                |file| session.symbols(file).map_err(|error| error.to_string()),
            );
            reply.send(answer).ok();
        },
        Request::Format { path, reply } => {
            let answer = session.file(&path).map_or_else(
                || Ok(None),
                |file| session.format(file).map_err(|error| error.to_string()),
            );
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
    let (service, socket) = LspService::new(server::Backend::spawn);
    Server::new(stdin, stdout, socket).serve(service).await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open(session: &mut Session, path: &str, text: &str) -> FileId {
        session.open(path, text).expect("the document opens")
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
        session.set("a.ds", "").expect("the close runs");
        let again = open(&mut session, "a.ds", "const one = 2\n");
        assert_eq!(a, again);
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
            .hover(file, 0, 22)
            .expect("the pass runs")
            .expect("the string literal covers the position");
        assert!(answer.contents.contains("string"), "{}", answer.contents);
        assert!(answer.contents.contains("~ pure"), "{}", answer.contents);
    }

    /// Definition follows the resolution side table to the defining name
    /// occurrence, as a location in the same document.
    #[test]
    fn definition_answers_the_defining_name() {
        let mut session = Session::new();
        // An absolute path: the client's file:// URIs always decode to
        // absolute on-disk paths, and the definition answer needs the
        // reverse mapping.
        let path = std::env::temp_dir().join("ds_lsp_definition.ds");
        let file = open(
            &mut session,
            &path.to_string_lossy(),
            "fn greet(name: string) -> string { name }\nconst shout = greet(\"hi\")\n",
        );
        // Position inside the `greet` use of the second line.
        let locations = session.definition(file, 1, 16).expect("the pass runs");
        assert_eq!(locations.len(), 1, "{locations:?}");
        // The defining `greet` sits on line 1 (0-based 0) at chars 3..9
        // of the same document.
        assert_eq!(
            locations[0].uri,
            session.uri(file).expect("the document has a URI")
        );
        assert_eq!(locations[0].range.start.line, 0);
        assert_eq!(locations[0].range.start.character, 3);
        assert_eq!(locations[0].range.end.character, 8);
    }
}
