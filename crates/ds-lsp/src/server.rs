//! The protocol layer: a thin translation between JSON-RPC and commands
//! for the engine's worker thread. The handler holds only the command
//! channel and the client handle (both `Send + Sync`); every engine
//! value stays on the worker.

use std::sync::mpsc;

use tokio::sync::oneshot;
use tower_lsp::{
    Client, LanguageServer, jsonrpc,
    lsp_types::{
        self as lsp, DidChangeTextDocumentParams, DidCloseTextDocumentParams,
        DidOpenTextDocumentParams, DocumentFormattingParams, DocumentSymbolParams,
        DocumentSymbolResponse, GotoDefinitionParams, GotoDefinitionResponse, Hover, HoverContents,
        HoverParams, HoverProviderCapability, InitializeParams, InitializeResult,
        InitializedParams, MarkupContent, MarkupKind, OneOf, ServerCapabilities, ServerInfo,
        TextDocumentSyncCapability, TextDocumentSyncKind,
    },
};

use crate::{Request, spawn_worker};

/// The language server: one command channel into the engine's worker
/// thread plus the client handle.
pub(crate) struct Backend {
    client: Client,
    tx: mpsc::Sender<Request>,
}

impl Backend {
    /// Builds the handler behind a freshly spawned engine worker.
    pub(crate) fn spawn(client: Client) -> Self {
        Self {
            client,
            tx: spawn_worker(),
        }
    }

    /// Sends one command to the worker and awaits its reply. The worker
    /// outlives every single command (only `shutdown` ends it), so a
    /// dropped channel is a server bug, not a client-visible state.
    async fn call<T, F>(&self, make: F) -> jsonrpc::Result<T>
    where
        T: Send + 'static,
        F: FnOnce(oneshot::Sender<Result<T, String>>) -> Request,
    {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(make(tx))
            .map_err(|_| internal("the engine worker stopped"))?;
        match rx.await {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(message)) => Err(internal(&message)),
            Err(_) => Err(internal("the engine worker dropped a reply")),
        }
    }

    /// The decoded on-disk path of one document URI: this server syncs
    /// `file://` documents only, so anything else is logged and ignored.
    fn path_of(uri: &lsp::Url) -> Option<String> {
        uri.to_file_path()
            .ok()
            .map(|path| path.to_string_lossy().into_owned())
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

    /// Collects the document's diagnostics from the worker and publishes
    /// them (publishing an empty set clears the client's old markers,
    /// which is also the close story).
    async fn publish(&self, uri: lsp::Url, version: Option<i32>) {
        let Some(path) = Self::path_of(&uri) else {
            self.reject("publishing", &uri).await;
            return;
        };
        match self
            .call(|reply| Request::Diagnostics { path, reply })
            .await
        {
            Ok(diags) => self.client.publish_diagnostics(uri, diags, version).await,
            Err(error) => {
                self.client
                    .log_message(
                        lsp::MessageType::ERROR,
                        format!("diagnostics failed: {error}"),
                    )
                    .await;
            },
        }
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
                // The client ships the whole document per change; the
                // engine's set_source swap is the incremental story.
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::FULL,
                )),
                hover_provider: Some(HoverProviderCapability::Simple(true)),
                definition_provider: Some(OneOf::Left(true)),
                document_symbol_provider: Some(OneOf::Left(true)),
                document_formatting_provider: Some(OneOf::Left(true)),
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
        let opened = self.call(move |reply| Request::Open {
            path,
            text: doc.text,
            reply,
        });
        if opened.await.is_err() {
            return;
        }
        self.publish(doc.uri, Some(doc.version)).await;
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let doc = params.text_document;
        let Some(path) = Self::path_of(&doc.uri) else {
            self.reject("changing", &doc.uri).await;
            return;
        };
        // Full sync: one whole-document change per notification, the
        // last one winning should a client batch.
        let Some(change) = params.content_changes.into_iter().next_back() else {
            return;
        };
        let changed = self.call(move |reply| Request::Set {
            path,
            text: change.text,
            reply,
        });
        if changed.await.is_err() {
            return;
        }
        self.publish(doc.uri, Some(doc.version)).await;
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
        self.publish(uri, None).await;
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
}
