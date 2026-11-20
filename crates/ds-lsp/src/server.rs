//! The protocol layer: a thin translation between JSON-RPC and commands
//! for the engine's worker thread. The handler holds only the command
//! channel and the client handle (both `Send + Sync`); every engine
//! value stays on the worker.

use std::{
    path::PathBuf,
    sync::{Arc, Mutex, PoisonError, mpsc},
    time::Duration,
};

use rustc_hash::FxHashMap;
use tokio::sync::{Mutex as AsyncMutex, oneshot};
use tower_lsp::{
    Client, LanguageServer, jsonrpc,
    lsp_types::{
        self as lsp, CompletionOptions, CompletionParams, CompletionResponse,
        DidChangeTextDocumentParams, DidCloseTextDocumentParams, DidOpenTextDocumentParams,
        DidSaveTextDocumentParams, DocumentFormattingParams, DocumentOnTypeFormattingOptions,
        DocumentOnTypeFormattingParams, DocumentSymbolParams, DocumentSymbolResponse,
        GotoDefinitionParams, GotoDefinitionResponse, Hover, HoverContents, HoverParams,
        HoverProviderCapability, InitializeParams, InitializeResult, InitializedParams,
        MarkupContent, MarkupKind, OneOf, PrepareRenameResponse, ReferenceParams, RenameOptions,
        RenameParams, SemanticTokensFullOptions, SemanticTokensLegend, SemanticTokensOptions,
        SemanticTokensParams, SemanticTokensResult, SemanticTokensServerCapabilities,
        ServerCapabilities, ServerInfo, TextDocumentPositionParams, TextDocumentSyncCapability,
        TextDocumentSyncKind, TextDocumentSyncOptions, TextDocumentSyncSaveOptions,
        WorkDoneProgressOptions, WorkspaceEdit,
    },
};

use crate::{RenameAnswer, Request, spawn_worker};

/// The trailing window coalescing rapid change and save notifications:
/// the memoized queries recompute at most once per window. Editor
/// typing bursts run far under it, so the publish lands one window
/// after the last notification.
const DEBOUNCE: Duration = Duration::from_millis(200);

/// One document's server-side state: the lock ordering concurrent
/// change notifications for the document, and the publish gate with
/// its debounce generation.
#[derive(Default)]
struct DocState {
    /// Held across one document's change application, so concurrent
    /// didChange notifications apply their edits in arrival order.
    changes: AsyncMutex<()>,
    /// The publish gate plus the debounce generation counting every
    /// scheduled publish: only the newest schedule survives its window.
    publish: AsyncMutex<Publish>,
}

/// The publish gate of one document: the last version tag shipped to
/// the client (a publish tagged older than it drops, so the client's
/// markers stay monotonic) and the debounce generation.
#[derive(Default)]
struct Publish {
    last: Option<i32>,
    generation: u64,
}

/// The language server: one command channel into the engine's worker
/// thread plus the client handle.
pub(crate) struct Backend {
    client: Client,
    tx: mpsc::Sender<Request>,
    /// The per-document state, one entry per URI the server has ever
    /// held (a close keeps its entry; a reopen reuses it).
    docs: Mutex<FxHashMap<lsp::Url, Arc<DocState>>>,
}

impl Backend {
    /// Builds the handler behind a freshly spawned engine worker, the
    /// embedder's linked-surface provider riding the worker's session.
    pub(crate) fn spawn(client: Client, linked: crate::SurfaceProvider) -> Self {
        Self {
            client,
            tx: spawn_worker(linked),
            docs: Mutex::new(FxHashMap::default()),
        }
    }

    /// The document's state entry.
    fn state(&self, uri: &lsp::Url) -> Arc<DocState> {
        let mut docs = self.docs.lock().unwrap_or_else(PoisonError::into_inner);
        docs.entry(uri.clone())
            .or_insert_with(|| Arc::new(DocState::default()))
            .clone()
    }

    /// Sends one command to the worker and awaits its reply. The worker
    /// outlives every single command (only `shutdown` ends it), so a
    /// dropped channel is a server bug, not a client-visible state.
    async fn call<T, F>(&self, make: F) -> jsonrpc::Result<T>
    where
        T: Send + 'static,
        F: FnOnce(oneshot::Sender<Result<T, String>>) -> Request,
    {
        call_tx(&self.tx, make).await
    }

    /// The decoded on-disk path of one document URI: this server syncs
    /// `file://` documents only, so anything else is logged and ignored.
    /// The exact `PathBuf` (never a lossy string) keys the document
    /// table, so paths that are not valid UTF-8 keep round-tripping.
    fn path_of(uri: &lsp::Url) -> Option<PathBuf> {
        uri.to_file_path().ok()
    }

    /// Logs a rejected or unopenable document and returns.
    async fn reject(&self, what: &str, uri: &lsp::Url) {
        self.client
            .log_message(
                lsp::MessageType::WARNING,
                format!("{what} refused: {uri} is not an open file:// document"),
            )
            .await;
    }

    /// Publishes the document's diagnostics now, superseding any
    /// scheduled publish (the generation bump retires its timer). Open
    /// and close ride this path: both are once-per-event notifications.
    async fn publish_now(&self, uri: &lsp::Url, version: Option<i32>) {
        let state = self.state(uri);
        let mut gate = state.publish.lock().await;
        gate.generation += 1;
        ship(&self.client, &self.tx, &mut gate, uri, version).await;
    }

    /// Schedules the document's diagnostics publish at the trailing
    /// edge of the debounce window: every change or save notification
    /// bumps the generation, so only the newest notification's timer
    /// fires and a typing burst pulls the memoized queries once. The
    /// edits themselves already applied — notifications defer the
    /// diagnostics pull alone — so a request arriving mid-window
    /// answers the client's current text.
    async fn schedule_publish(&self, uri: &lsp::Url, version: Option<i32>) {
        let state = self.state(uri);
        let generation = {
            let mut gate = state.publish.lock().await;
            gate.generation += 1;
            gate.generation
        };
        let client = self.client.clone();
        let tx = self.tx.clone();
        let uri = uri.clone();
        tokio::spawn(async move {
            tokio::time::sleep(DEBOUNCE).await;
            let mut gate = state.publish.lock().await;
            // A newer notification rescheduled this document's publish.
            if gate.generation != generation {
                return;
            }
            ship(&client, &tx, &mut gate, &uri, version).await;
        });
    }
}

/// Sends one command to the worker and awaits its reply. The worker
/// outlives every single command (only `shutdown` ends it), so a
/// dropped channel is a server bug, not a client-visible state.
async fn call_tx<T, F>(tx: &mpsc::Sender<Request>, make: F) -> jsonrpc::Result<T>
where
    T: Send + 'static,
    F: FnOnce(oneshot::Sender<Result<T, String>>) -> Request,
{
    let (reply, rx) = oneshot::channel();
    tx.send(make(reply))
        .map_err(|_| internal("the engine worker stopped"))?;
    match rx.await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(message)) => Err(internal(&message)),
        Err(_) => Err(internal("the engine worker dropped a reply")),
    }
}

/// Pulls the document's diagnostics from the worker and publishes them
/// (publishing an empty set clears the client's old markers, which is
/// also the close story) through the held gate: a publish tagged older
/// than the last shipped one is dropped (the worker's FIFO keeps the
/// diagnostic content right; the gate keeps the client's markers
/// monotonic).
async fn ship(
    client: &Client,
    tx: &mpsc::Sender<Request>,
    gate: &mut Publish,
    uri: &lsp::Url,
    version: Option<i32>,
) {
    let Some(path) = Backend::path_of(uri) else {
        client
            .log_message(
                lsp::MessageType::WARNING,
                format!("publishing refused: {uri} is not an open file:// document"),
            )
            .await;
        return;
    };
    if !publishable(gate.last, version) {
        return;
    }
    match call_tx(tx, |reply| Request::Diagnostics { path, reply }).await {
        Ok(diags) => {
            client
                .publish_diagnostics(uri.clone(), diags, version)
                .await;
            // A close's untagged publish resets the gate, so a reopen's
            // version numbering starts fresh.
            gate.last = version;
        },
        Err(error) => {
            client
                .log_message(
                    lsp::MessageType::ERROR,
                    format!("diagnostics failed: {error}"),
                )
                .await;
        },
    }
}

/// Whether one publish may go out given the document's last published
/// version: a version-tagged publish at or after the last shipped one
/// goes out while an older tag is dropped, and an untagged publish (the
/// close's clear, a save's refresh) always goes out.
fn publishable(last: Option<i32>, version: Option<i32>) -> bool {
    match (last, version) {
        (Some(last), Some(version)) => version >= last,
        _ => true,
    }
}

/// A JSON-RPC internal error carrying `message` (the stock constructor
/// would report only the generic description).
fn internal(message: &str) -> jsonrpc::Error {
    let mut error = jsonrpc::Error::internal_error();
    error.message = message.to_owned().into();
    error
}

#[tower_lsp::async_trait]
impl LanguageServer for Backend {
    async fn initialize(&self, _params: InitializeParams) -> jsonrpc::Result<InitializeResult> {
        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                // Incremental sync: a change notification's ranged edits
                // splice into the held document, a rangeless change
                // replaces it whole, and save notifications ride the
                // debounced publish.
                text_document_sync: Some(TextDocumentSyncCapability::Options(
                    TextDocumentSyncOptions {
                        open_close: Some(true),
                        change: Some(TextDocumentSyncKind::INCREMENTAL),
                        save: Some(TextDocumentSyncSaveOptions::Supported(true)),
                        ..TextDocumentSyncOptions::default()
                    },
                )),
                hover_provider: Some(HoverProviderCapability::Simple(true)),
                definition_provider: Some(OneOf::Left(true)),
                references_provider: Some(OneOf::Left(true)),
                // Prepare-rename first: the client learns the refusals
                // (an extern name, a member, the ambient) before it
                // sends the rename.
                rename_provider: Some(OneOf::Right(RenameOptions {
                    prepare_provider: Some(true),
                    work_done_progress_options: WorkDoneProgressOptions::default(),
                })),
                // The dot triggers the member surface; every other
                // request rides the client's own word-typing trigger.
                completion_provider: Some(CompletionOptions {
                    trigger_characters: Some(vec![".".to_owned()]),
                    ..CompletionOptions::default()
                }),
                document_symbol_provider: Some(OneOf::Left(true)),
                document_formatting_provider: Some(OneOf::Left(true)),
                // A newline inside a body formats the enclosing
                // declaration; a `}` formats the declaration it closed.
                document_on_type_formatting_provider: Some(DocumentOnTypeFormattingOptions {
                    first_trigger_character: "\n".to_owned(),
                    more_trigger_character: Some(vec!["}".to_owned()]),
                }),
                // Whole-document tokens only: no delta, no range this
                // slice (memoization is the latency story).
                semantic_tokens_provider: Some(
                    SemanticTokensServerCapabilities::SemanticTokensOptions(
                        SemanticTokensOptions {
                            legend: SemanticTokensLegend {
                                token_types: crate::tokens::legend(),
                                token_modifiers: crate::tokens::modifier_legend(),
                            },
                            range: Some(false),
                            full: Some(SemanticTokensFullOptions::Bool(true)),
                            work_done_progress_options: WorkDoneProgressOptions::default(),
                        },
                    ),
                ),
                ..ServerCapabilities::default()
            },
            server_info: Some(ServerInfo {
                name: "ds".to_owned(),
                version: Some(env!("CARGO_PKG_VERSION").to_owned()),
            }),
        })
    }

    async fn initialized(&self, _params: InitializedParams) {}

    async fn shutdown(&self) -> jsonrpc::Result<()> {
        self.call(|reply| Request::Shutdown { reply }).await
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let doc = params.text_document;
        let Some(path) = Self::path_of(&doc.uri) else {
            self.reject("opening", &doc.uri).await;
            return;
        };
        let uri = doc.uri.clone();
        let opened = self.call(move |reply| Request::Open {
            path,
            uri,
            text: doc.text,
            reply,
        });
        if opened.await.is_err() {
            return;
        }
        self.publish_now(&doc.uri, Some(doc.version)).await;
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let doc = params.text_document;
        let Some(path) = Self::path_of(&doc.uri) else {
            self.reject("changing", &doc.uri).await;
            return;
        };
        if params.content_changes.is_empty() {
            return;
        }
        let uri = doc.uri.clone();
        // The change lock keeps concurrent notifications for one
        // document in arrival order, so a client's ordered edits apply
        // in order.
        let state = self.state(&uri);
        let changes = state.changes.lock().await;
        let edited = self.call(move |reply| Request::Edit {
            path,
            changes: params.content_changes,
            reply,
        });
        if edited.await.is_err() {
            return;
        }
        drop(changes);
        self.schedule_publish(&uri, Some(doc.version)).await;
    }

    async fn did_save(&self, params: DidSaveTextDocumentParams) {
        let uri = params.text_document.uri;
        if Self::path_of(&uri).is_none() {
            self.reject("saving", &uri).await;
            return;
        }
        // The synced content is already current (this server owns the
        // incremental splices, so a client shipping text anyway is
        // ignored): a save asks for one fresh diagnostics publish.
        self.schedule_publish(&uri, None).await;
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        let uri = params.text_document.uri;
        let Some(path) = Self::path_of(&uri) else {
            self.reject("closing", &uri).await;
            return;
        };
        // The content empties in place; the file id stays for a reopen.
        let closed = self.call(move |reply| Request::Set {
            path,
            text: String::new(),
            reply,
        });
        if closed.await.is_err() {
            return;
        }
        self.publish_now(&uri, None).await;
    }

    async fn goto_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> jsonrpc::Result<Option<GotoDefinitionResponse>> {
        let doc = params.text_document_position_params.text_document;
        let pos = params.text_document_position_params.position;
        let Some(path) = Self::path_of(&doc.uri) else {
            return Ok(None);
        };
        let locations = self
            .call(move |reply| Request::Definition {
                path,
                line: pos.line,
                character: pos.character,
                reply,
            })
            .await?;
        Ok(Some(GotoDefinitionResponse::Array(locations)))
    }

    async fn hover(&self, params: HoverParams) -> jsonrpc::Result<Option<Hover>> {
        let doc = params.text_document_position_params.text_document;
        let pos = params.text_document_position_params.position;
        let Some(path) = Self::path_of(&doc.uri) else {
            return Ok(None);
        };
        let answer = self
            .call(move |reply| Request::Hover {
                path,
                line: pos.line,
                character: pos.character,
                reply,
            })
            .await?;
        Ok(answer.map(|hover| Hover {
            contents: HoverContents::Markup(MarkupContent {
                kind: MarkupKind::Markdown,
                value: hover.contents,
            }),
            range: Some(hover.range),
        }))
    }

    async fn completion(
        &self,
        params: CompletionParams,
    ) -> jsonrpc::Result<Option<CompletionResponse>> {
        let doc = params.text_document_position.text_document;
        let pos = params.text_document_position.position;
        let Some(path) = Self::path_of(&doc.uri) else {
            return Ok(None);
        };
        let items = self
            .call(move |reply| Request::Completion {
                path,
                line: pos.line,
                character: pos.character,
                reply,
            })
            .await?;
        Ok(Some(CompletionResponse::Array(items)))
    }

    async fn references(
        &self,
        params: ReferenceParams,
    ) -> jsonrpc::Result<Option<Vec<lsp::Location>>> {
        let doc = params.text_document_position.text_document;
        let pos = params.text_document_position.position;
        let include_declaration = params.context.include_declaration;
        let Some(path) = Self::path_of(&doc.uri) else {
            return Ok(None);
        };
        let locations = self
            .call(move |reply| Request::References {
                path,
                line: pos.line,
                character: pos.character,
                include_declaration,
                reply,
            })
            .await?;
        Ok(Some(locations))
    }

    async fn prepare_rename(
        &self,
        params: TextDocumentPositionParams,
    ) -> jsonrpc::Result<Option<PrepareRenameResponse>> {
        let doc = params.text_document;
        let pos = params.position;
        let Some(path) = Self::path_of(&doc.uri) else {
            return Ok(None);
        };
        match self
            .call(move |reply| Request::PrepareRename {
                path,
                line: pos.line,
                character: pos.character,
                reply,
            })
            .await?
        {
            RenameAnswer::Prepare(range, placeholder) => {
                Ok(Some(PrepareRenameResponse::RangeWithPlaceholder {
                    range,
                    placeholder,
                }))
            },
            RenameAnswer::Refused(message) => Err(jsonrpc::Error::invalid_params(message)),
            // Rename answers an edit; prepare never does.
            RenameAnswer::Edit(_) => unreachable!("prepare-rename never answers an edit"),
        }
    }

    async fn rename(&self, params: RenameParams) -> jsonrpc::Result<Option<WorkspaceEdit>> {
        let doc = params.text_document_position.text_document;
        let pos = params.text_document_position.position;
        let new_name = params.new_name;
        let Some(path) = Self::path_of(&doc.uri) else {
            return Ok(None);
        };
        match self
            .call(move |reply| Request::Rename {
                path,
                line: pos.line,
                character: pos.character,
                new_name,
                reply,
            })
            .await?
        {
            RenameAnswer::Edit(edit) => Ok(Some(edit)),
            RenameAnswer::Refused(message) => Err(jsonrpc::Error::invalid_params(message)),
            // Prepare answers a range; rename never does.
            RenameAnswer::Prepare(..) => unreachable!("rename never answers a prepare range"),
        }
    }

    async fn document_symbol(
        &self,
        params: DocumentSymbolParams,
    ) -> jsonrpc::Result<Option<DocumentSymbolResponse>> {
        let doc = params.text_document;
        let Some(path) = Self::path_of(&doc.uri) else {
            return Ok(None);
        };
        let symbols = self
            .call(move |reply| Request::Symbols { path, reply })
            .await?;
        Ok((!symbols.is_empty()).then_some(DocumentSymbolResponse::Nested(symbols)))
    }

    async fn formatting(
        &self,
        params: DocumentFormattingParams,
    ) -> jsonrpc::Result<Option<Vec<lsp::TextEdit>>> {
        let doc = params.text_document;
        let Some(path) = Self::path_of(&doc.uri) else {
            return Ok(None);
        };
        self.call(move |reply| Request::Format { path, reply })
            .await
    }

    /// The format options ride through unaccepted: `ds-fmt` is
    /// zero-option, so the client's tab size and insert-spaces never
    /// change the canonical form.
    async fn on_type_formatting(
        &self,
        params: DocumentOnTypeFormattingParams,
    ) -> jsonrpc::Result<Option<Vec<lsp::TextEdit>>> {
        let doc = params.text_document_position.text_document;
        let pos = params.text_document_position.position;
        let ch = params.ch;
        let Some(path) = Self::path_of(&doc.uri) else {
            return Ok(None);
        };
        self.call(move |reply| Request::OnTypeFormat {
            path,
            line: pos.line,
            character: pos.character,
            ch,
            reply,
        })
        .await
    }

    async fn semantic_tokens_full(
        &self,
        params: SemanticTokensParams,
    ) -> jsonrpc::Result<Option<SemanticTokensResult>> {
        let doc = params.text_document;
        let Some(path) = Self::path_of(&doc.uri) else {
            return Ok(None);
        };
        let data = self
            .call(move |reply| Request::Tokens { path, reply })
            .await?;
        Ok(Some(SemanticTokensResult::Tokens(lsp::SemanticTokens {
            result_id: None,
            data,
        })))
    }
}

#[cfg(test)]
mod tests {
    use super::publishable;

    /// The version gate drops only older-tagged publishes; a close's
    /// untagged publish always clears.
    #[test]
    fn the_publish_gate_drops_older_versions_only() {
        assert!(!publishable(Some(3), Some(2)), "an older tag is dropped");
        assert!(publishable(Some(3), Some(3)), "an equal tag goes out");
        assert!(publishable(Some(3), Some(4)), "a newer tag goes out");
        assert!(publishable(Some(3), None), "a close always clears");
        assert!(publishable(None, Some(1)), "a first publish goes out");
    }
}
