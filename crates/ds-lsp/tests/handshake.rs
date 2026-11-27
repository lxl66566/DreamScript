//! In-process smoke test of the whole server: the handshake over one
//! duplex pair, a document open with published diagnostics, hover,
//! definition, references, prepare-rename, rename, symbols, formatting,
//! and the shutdown cycle.

use std::time::Duration;

use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt, DuplexStream},
    time::timeout,
};
use tower_lsp::lsp_types::Url;

/// The document every request addresses, spelled from a real temp-dir
/// path: the server syncs `file://` documents only, and `Url::to_file_path`
/// refuses a Unix path spelling on Windows, so a hardcoded `file:///tmp/...`
/// never opens there and the session hangs waiting for its diagnostics.
/// The path need not exist on disk (the server never reads behind a URI),
/// and concurrent lane gates share the temp dir, so the probe stem takes
/// the same pid suffix ds-cli's fixtures took.
fn document_uri(stem: &str) -> String {
    let path = std::env::temp_dir().join(format!("{stem}-{}.ds", std::process::id()));
    Url::from_file_path(path)
        .expect("the temp path carries a file URI")
        .to_string()
}

fn request(id: u64, method: &str, params: &Value) -> String {
    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}).to_string()
}

fn notification(method: &str, params: &Value) -> String {
    json!({"jsonrpc": "2.0", "method": method, "params": params}).to_string()
}

async fn send(stream: &mut DuplexStream, body: &str) {
    let framed = format!("Content-Length: {}\r\n\r\n{body}", body.len());
    stream
        .write_all(framed.as_bytes())
        .await
        .expect("the client writes");
}

/// Reads one framed message off the stream.
async fn recv(stream: &mut DuplexStream) -> String {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        stream
            .read_exact(&mut byte)
            .await
            .expect("the server writes");
        head.push(byte[0]);
        if head.ends_with(b"\r\n\r\n") {
            break;
        }
    }
    let head = String::from_utf8_lossy(&head);
    let length: usize = head
        .lines()
        .find_map(|line| line.strip_prefix("Content-Length:"))
        .and_then(|value| value.trim().parse().ok())
        .expect("the server frames its messages");
    let mut body = vec![0u8; length];
    stream
        .read_exact(&mut body)
        .await
        .expect("the body follows the headers");
    String::from_utf8(body).expect("the body is UTF-8")
}

/// Which reply to wait for: the answer to one request id, or the next
/// notification of one method.
enum Wanted {
    Id(u64),
    Method(&'static str),
}

/// Reads messages until the wanted one arrives, discarding the others
/// (a notification may interleave with a pending request's answer).
async fn expect(stream: &mut DuplexStream, wanted: Wanted) -> Value {
    loop {
        let message: Value =
            serde_json::from_str(&recv(stream).await).expect("the message is JSON");
        let matched = match wanted {
            Wanted::Id(id) => message["id"] == id,
            Wanted::Method(method) => message["method"] == method,
        };
        if matched {
            return message;
        }
    }
}

#[tokio::test]
async fn the_handshake_diagnoses_and_answers() {
    timeout(Duration::from_secs(120), async {
        let uri = document_uri("ds_lsp_smoke");
        let uri = uri.as_str();
        let (server, mut client) = tokio::io::duplex(16 << 10);
        // The server's streams are one duplex pair split into its halves,
        // like stdin/stdout are two ends of the same session.
        let (stdin, stdout) = tokio::io::split(server);
        tokio::spawn(async move {
            ds_lsp::serve(stdin, stdout)
                .await
                .expect("the server serves");
        });

        // The handshake: full-text sync, hover, definition, symbols,
        // formatting.
        send(
            &mut client,
            &request(
                1,
                "initialize",
                &json!({"processId": null, "rootUri": null, "capabilities": {}}),
            ),
        )
        .await;
        let reply = expect(&mut client, Wanted::Id(1)).await;
        // The incremental test below pins the full sync options; here
        // the kind alone answers the "which sync" question.
        assert_eq!(
            reply["result"]["capabilities"]["textDocumentSync"]["change"],
            2
        );
        assert_eq!(reply["result"]["capabilities"]["hoverProvider"], true);
        assert_eq!(reply["result"]["capabilities"]["definitionProvider"], true);
        assert_eq!(reply["result"]["capabilities"]["referencesProvider"], true);
        assert_eq!(
            reply["result"]["capabilities"]["renameProvider"]["prepareProvider"],
            true
        );
        assert!(reply["result"]["capabilities"]["completionProvider"].is_object());
        assert_eq!(
            reply["result"]["capabilities"]["documentSymbolProvider"],
            true
        );
        assert_eq!(
            reply["result"]["capabilities"]["documentFormattingProvider"],
            true
        );
        send(&mut client, &notification("initialized", &json!({}))).await;

        // Opening a document publishes its diagnostics: the second line's
        // int initializer does not contain in the string annotation.
        let source = concat!(
            "const msg: string = \"hi\"\n",
            "const broken: string = 1\n",
            "const text: string = join([msg], \"\")\n",
        );
        send(
            &mut client,
            &notification(
                "textDocument/didOpen",
                &json!({"textDocument": {
                    "uri": uri,
                    "languageId": "dreamscript",
                    "version": 1,
                    "text": source,
                }}),
            ),
        )
        .await;
        let published = expect(
            &mut client,
            Wanted::Method("textDocument/publishDiagnostics"),
        )
        .await;
        assert_eq!(published["params"]["uri"], uri);
        let diags = &published["params"]["diagnostics"];
        assert!(diags.is_array(), "{diags}");
        let mismatch = diags
            .as_array()
            .expect("the diagnostics are a list")
            .iter()
            .find(|diag| diag["code"] == "DS2001")
            .expect("the type mismatch publishes");
        assert_eq!(mismatch["range"]["start"]["line"], 1);

        // Hover answers the checked type at the position.
        send(
            &mut client,
            &request(
                2,
                "textDocument/hover",
                &json!({"textDocument": {"uri": uri}, "position": {"line": 0, "character": 22}}),
            ),
        )
        .await;
        let reply = expect(&mut client, Wanted::Id(2)).await;
        let value = reply["result"]["contents"]["value"]
            .as_str()
            .expect("the hover carries markdown contents");
        assert!(value.contains("string"), "{value}");

        // Definition follows the resolution to the defining name.
        send(
            &mut client,
            &request(
                3,
                "textDocument/definition",
                &json!({"textDocument": {"uri": uri}, "position": {"line": 2, "character": 28}}),
            ),
        )
        .await;
        let reply = expect(&mut client, Wanted::Id(3)).await;
        let locations = reply["result"]
            .as_array()
            .expect("definition answers a location list");
        assert_eq!(locations.len(), 1, "{locations:?}");
        assert_eq!(locations[0]["uri"], uri);
        assert_eq!(locations[0]["range"]["start"]["line"], 0);
        assert_eq!(locations[0]["range"]["start"]["character"], 6);

        // The top-level symbols name the three bindings.
        send(
            &mut client,
            &request(
                4,
                "textDocument/documentSymbol",
                &json!({"textDocument": {"uri": uri}}),
            ),
        )
        .await;
        let reply = expect(&mut client, Wanted::Id(4)).await;
        let names: Vec<&str> = reply["result"]
            .as_array()
            .expect("symbols answer a list")
            .iter()
            .filter_map(|symbol| symbol["name"].as_str())
            .collect();
        assert_eq!(names, ["msg", "broken", "text"]);

        // The canonical form of the source is itself: no edits.
        send(
            &mut client,
            &request(
                5,
                "textDocument/formatting",
                &json!({
                    "textDocument": {"uri": uri},
                    "options": {"tabSize": 4, "insertSpaces": false},
                }),
            ),
        )
        .await;
        let reply = expect(&mut client, Wanted::Id(5)).await;
        assert!(
            reply["result"].as_array().is_some_and(Vec::is_empty),
            "{}",
            reply["result"]
        );

        // Completion answers the names visible at the position: the
        // document's declarations and the ambient's, the ambient's
        // first.
        send(
            &mut client,
            &request(
                6,
                "textDocument/completion",
                &json!({"textDocument": {"uri": uri}, "position": {"line": 0, "character": 22}}),
            ),
        )
        .await;
        let reply = expect(&mut client, Wanted::Id(6)).await;
        let items = reply["result"]
            .as_array()
            .expect("completion answers a list");
        let labels: Vec<&str> = items
            .iter()
            .filter_map(|item| item["label"].as_str())
            .collect();
        for name in ["msg", "broken", "text", "join"] {
            assert!(labels.contains(&name), "{labels:?}");
        }
        let idx = |name: &str| {
            labels
                .iter()
                .position(|got| *got == name)
                .unwrap_or_else(|| panic!("`{name}` answers: {labels:?}"))
        };
        assert!(idx("join") < idx("msg"), "{labels:?}");

        // References answer the declaration plus every use of the name at
        // the position; uses inside the ambient carry no URI and drop.
        send(
            &mut client,
            &request(
                7,
                "textDocument/references",
                &json!({
                    "textDocument": {"uri": uri},
                    "position": {"line": 2, "character": 28},
                    "context": {"includeDeclaration": true},
                }),
            ),
        )
        .await;
        let reply = expect(&mut client, Wanted::Id(7)).await;
        let locations = reply["result"]
            .as_array()
            .expect("references answer a location list");
        assert_eq!(locations.len(), 2, "{locations:?}");
        assert_eq!(locations[0]["range"]["start"]["line"], 0);
        assert_eq!(locations[0]["range"]["start"]["character"], 6);
        assert_eq!(locations[1]["range"]["start"]["line"], 2);
        assert_eq!(locations[1]["range"]["start"]["character"], 27);

        // Prepare-rename answers the name's range and spelling.
        send(
            &mut client,
            &request(
                8,
                "textDocument/prepareRename",
                &json!({"textDocument": {"uri": uri}, "position": {"line": 2, "character": 28}}),
            ),
        )
        .await;
        let reply = expect(&mut client, Wanted::Id(8)).await;
        assert_eq!(reply["result"]["range"]["start"]["line"], 2);
        assert_eq!(reply["result"]["range"]["start"]["character"], 27);
        assert_eq!(reply["result"]["range"]["end"]["character"], 30);
        assert_eq!(reply["result"]["placeholder"], "msg");

        // Rename answers the edit set over the declaration and the use.
        send(
            &mut client,
            &request(
                9,
                "textDocument/rename",
                &json!({
                    "textDocument": {"uri": uri},
                    "position": {"line": 2, "character": 28},
                    "newName": "greeting",
                }),
            ),
        )
        .await;
        let reply = expect(&mut client, Wanted::Id(9)).await;
        let edits = reply["result"]["changes"][uri]
            .as_array()
            .expect("rename answers document changes");
        assert_eq!(edits.len(), 2, "{edits:?}");
        assert_eq!(edits[0]["range"]["start"]["line"], 0);
        assert_eq!(edits[0]["range"]["start"]["character"], 6);
        assert_eq!(edits[1]["range"]["start"]["line"], 2);
        assert_eq!(edits[1]["range"]["start"]["character"], 27);
        assert_eq!(edits[1]["newText"], "greeting");

        // A rename of an ambient name answers an error, honestly worded:
        // `join` lives in the stdlib ambient, which no document edits.
        send(
            &mut client,
            &request(
                10,
                "textDocument/rename",
                &json!({
                    "textDocument": {"uri": uri},
                    "position": {"line": 2, "character": 23},
                    "newName": "greeting",
                }),
            ),
        )
        .await;
        let reply = expect(&mut client, Wanted::Id(10)).await;
        let message = reply["error"]["message"]
            .as_str()
            .expect("the refusal carries a message");
        assert!(message.contains("ambient"), "{message}");

        // The shutdown cycle answers and ends the server: the request
        // carries no params at all (a params object is a protocol error).
        send(
            &mut client,
            r#"{"jsonrpc":"2.0","id":11,"method":"shutdown"}"#,
        )
        .await;
        let reply = expect(&mut client, Wanted::Id(11)).await;
        assert!(reply.get("error").is_none(), "{reply}");
        send(&mut client, &notification("exit", &json!({}))).await;
    })
    .await
    .expect("the cycle finishes");
}

/// The on-type formatting wire: the capability registers its trigger
/// characters, a closing `}` repairs the declaration it closed through
/// one trimmed edit, a newline inside the body answers the same region,
/// and an unregistered trigger character answers no edits.
#[tokio::test]
async fn on_type_formatting_answers_over_the_wire() {
    timeout(Duration::from_secs(120), async {
        let uri = document_uri("ds_lsp_ontype");
        let uri = uri.as_str();
        let (server, mut client) = tokio::io::duplex(16 << 10);
        let (stdin, stdout) = tokio::io::split(server);
        tokio::spawn(async move {
            ds_lsp::serve(stdin, stdout)
                .await
                .expect("the server serves");
        });

        send(
            &mut client,
            &request(
                1,
                "initialize",
                &json!({"processId": null, "rootUri": null, "capabilities": {}}),
            ),
        )
        .await;
        let reply = expect(&mut client, Wanted::Id(1)).await;
        let provider = &reply["result"]["capabilities"]["documentOnTypeFormattingProvider"];
        assert_eq!(provider["firstTriggerCharacter"], "\n", "{provider}");
        assert_eq!(provider["moreTriggerCharacter"], json!(["}"]), "{provider}");
        send(&mut client, &notification("initialized", &json!({}))).await;

        let source = "fn  greet( name:string ) ->string{\n\"hi\"\n}";
        send(
            &mut client,
            &notification(
                "textDocument/didOpen",
                &json!({"textDocument": {
                    "uri": uri,
                    "languageId": "dreamscript",
                    "version": 1,
                    "text": source,
                }}),
            ),
        )
        .await;
        // The open publishes the clean document's (empty) diagnostics;
        // the request loop below skips them.
        let on_type = |id: u64, line: u32, character: u32, ch: &str| {
            request(
                id,
                "textDocument/onTypeFormatting",
                &json!({
                    "textDocument": {"uri": uri},
                    "position": {"line": line, "character": character},
                    "ch": ch,
                    "options": {"tabSize": 2, "insertSpaces": true},
                }),
            )
        };

        // The `}` closing the fn repairs it: one edit over the changed
        // middle of the declaration alone — the shared `fn ` header and
        // the `"hi"}` tail stay out of the range, and the edit ends
        // where the re-indented body begins diverging.
        send(&mut client, &on_type(2, 2, 1, "}")).await;
        let reply = expect(&mut client, Wanted::Id(2)).await;
        let edits = reply["result"]
            .as_array()
            .expect("on-type formatting answers an edit list");
        assert_eq!(edits.len(), 1, "{edits:?}");
        assert_eq!(edits[0]["range"]["start"]["line"], 0, "{edits:?}");
        assert_eq!(edits[0]["range"]["start"]["character"], 3, "{edits:?}");
        assert_eq!(edits[0]["range"]["end"]["line"], 1, "{edits:?}");
        assert_eq!(edits[0]["range"]["end"]["character"], 0, "{edits:?}");
        assert_eq!(edits[0]["newText"], "greet(name: string) -> string {\n  ");

        // A newline typed inside the body answers the same region.
        send(&mut client, &on_type(3, 1, 0, "\n")).await;
        let reply = expect(&mut client, Wanted::Id(3)).await;
        let edits = reply["result"]
            .as_array()
            .expect("on-type formatting answers an edit list");
        assert_eq!(edits.len(), 1, "{edits:?}");
        assert_eq!(edits[0]["newText"], "greet(name: string) -> string {\n  ");

        // A trigger the server never registered answers no edits.
        send(&mut client, &on_type(4, 1, 0, ";")).await;
        let reply = expect(&mut client, Wanted::Id(4)).await;
        assert!(
            reply["result"].as_array().is_some_and(Vec::is_empty),
            "{}",
            reply["result"]
        );

        send(
            &mut client,
            r#"{"jsonrpc":"2.0","id":5,"method":"shutdown"}"#,
        )
        .await;
        let reply = expect(&mut client, Wanted::Id(5)).await;
        assert!(reply.get("error").is_none(), "{reply}");
        send(&mut client, &notification("exit", &json!({}))).await;
    })
    .await
    .expect("the cycle finishes");
}

/// The completion slices through the wire: a struct receiver's members,
/// a self receiver inside an inherent impl, UFCS candidates, a generic
/// body's type position, prefix ranking, the fixed skeletons behind a
/// typed keyword prefix, and the honest fallback to scope names when no
/// checked receiver answers.
#[tokio::test]
async fn completion_answers_members_types_and_ranking() {
    timeout(Duration::from_secs(120), async {
        let uri = document_uri("ds_lsp_smoke");
        let uri = uri.as_str();
        let (server, mut client) = tokio::io::duplex(16 << 10);
        let (stdin, stdout) = tokio::io::split(server);
        tokio::spawn(async move {
            ds_lsp::serve(stdin, stdout)
                .await
                .expect("the server serves");
        });

        send(
            &mut client,
            &request(
                1,
                "initialize",
                &json!({"processId": null, "rootUri": null, "capabilities": {}}),
            ),
        )
        .await;
        expect(&mut client, Wanted::Id(1)).await;
        send(&mut client, &notification("initialized", &json!({}))).await;

        let source = concat!(
            "struct point { x: int, y: int }\n",
            "impl point {\n",
            "  fn norm(self) -> int {\n",
            "    self.x\n",
            "  }\n",
            "}\n",
            "fn pick(p: point) -> int {\n",
            "  p.x\n",
            "}\n",
            "fn first[t](v: t) -> t? {\n",
            "  none\n",
            "}\n",
            "fn probe() -> int {\n",
            "  print\n",
            "}\n",
            "const sprint = 1\n",
            "const fnord = 1\n",
        );
        send(
            &mut client,
            &notification(
                "textDocument/didOpen",
                &json!({"textDocument": {
                    "uri": uri,
                    "languageId": "dreamscript",
                    "version": 1,
                    "text": source,
                }}),
            ),
        )
        .await;
        // The open publishes diagnostics (the `print` line resolves the
        // ambient names);
        // the request loop below skips them.
        let completion = |id: u64, line: u32, character: u32| {
            request(
                id,
                "textDocument/completion",
                &json!({"textDocument": {"uri": uri}, "position": {"line": line, "character": character}}),
            )
        };

        // A struct receiver's dot: fields, the inherent member, and the
        // UFCS free fn taking the receiver.
        send(&mut client, &completion(2, 7, 4)).await;
        let reply = expect(&mut client, Wanted::Id(2)).await;
        let items = reply["result"].as_array().expect("completion answers a list");
        let labels: Vec<&str> = items.iter().filter_map(|i| i["label"].as_str()).collect();
        assert_eq!(labels, ["x", "y", "norm", "pick"], "{labels:?}");
        assert_eq!(items[0]["kind"], 5, "a field item: {items:?}");

        // A self receiver inside the inherent impl answers the same
        // target type's members (the UFCS candidate included: `self.pick()`
        // feeds `pick`'s first parameter like any point receiver).
        send(&mut client, &completion(3, 3, 9)).await;
        let reply = expect(&mut client, Wanted::Id(3)).await;
        let items = reply["result"].as_array().expect("completion answers a list");
        let labels: Vec<&str> = items.iter().filter_map(|i| i["label"].as_str()).collect();
        assert_eq!(labels, ["x", "y", "norm", "pick"], "{labels:?}");

        // A generic body's type position answers the type variable ahead
        // of the type names, and no value names.
        send(&mut client, &completion(4, 9, 22)).await;
        let reply = expect(&mut client, Wanted::Id(4)).await;
        let items = reply["result"].as_array().expect("completion answers a list");
        let labels: Vec<&str> = items.iter().filter_map(|i| i["label"].as_str()).collect();
        assert_eq!(labels[0], "t", "{labels:?}");
        for name in ["string", "point"] {
            assert!(labels.contains(&name), "`{name}` answers: {labels:?}");
        }
        assert!(!labels.contains(&"join"), "value names stay out: {labels:?}");

        // The typed prefix ranks the answer: the exact prefix first, the
        // word-boundary match next, the substring after, the rest last.
        // `print` and `println` share the exact `print` prefix — their
        // relative order follows the ambient's declaration order, which
        // a composed-tail migration moves.
        send(&mut client, &completion(5, 13, 7)).await;
        let reply = expect(&mut client, Wanted::Id(5)).await;
        let items = reply["result"].as_array().expect("completion answers a list");
        let labels: Vec<&str> = items.iter().filter_map(|i| i["label"].as_str()).collect();
        let idx = |name: &str| {
            labels
                .iter()
                .position(|got| *got == name)
                .unwrap_or_else(|| panic!("`{name}` answers: {labels:?}"))
        };
        assert!(idx("print") < idx("sprint"), "prefix ahead of substring: {labels:?}");
        assert!(idx("println") < idx("sprint"), "prefix ahead of substring: {labels:?}");
        assert!(idx("sprint") < idx("pick"), "substring ahead of the rest: {labels:?}");

        // A dot over an unresolved expression answers the ordinary scope
        // names honestly, never a guessed member.
        let broken = document_uri("ds_lsp_broken");
        let broken = broken.as_str();
        send(
            &mut client,
            &notification(
                "textDocument/didOpen",
                &json!({"textDocument": {
                    "uri": broken,
                    "languageId": "dreamscript",
                    "version": 1,
                    "text": "const broken = missing.x\n",
                }}),
            ),
        )
        .await;
        send(
            &mut client,
            &request(
                6,
                "textDocument/completion",
                &json!({"textDocument": {"uri": broken}, "position": {"line": 0, "character": 22}}),
            ),
        )
        .await;
        let reply = expect(&mut client, Wanted::Id(6)).await;
        let items = reply["result"].as_array().expect("completion answers a list");
        let labels: Vec<&str> = items.iter().filter_map(|i| i["label"].as_str()).collect();
        assert!(labels.contains(&"println"), "scope names answer: {labels:?}");
        assert!(labels.contains(&"broken"), "{labels:?}");

        // The fixed skeletons answer behind a typed keyword prefix: `fn`
        // at item level carries the declaration skeleton in snippet
        // format, ranked behind every same-prefix name match (the parseable
        // document's `fnord`, whose own document stays the skeleton's).
        let skeleton = document_uri("ds_lsp_skeleton");
        let skeleton = skeleton.as_str();
        send(
            &mut client,
            &notification(
                "textDocument/didOpen",
                &json!({"textDocument": {
                    "uri": skeleton,
                    "languageId": "dreamscript",
                    "version": 1,
                    "text": "fn\n",
                }}),
            ),
        )
        .await;
        send(
            &mut client,
            &request(
                7,
                "textDocument/completion",
                &json!({"textDocument": {"uri": skeleton}, "position": {"line": 0, "character": 2}}),
            ),
        )
        .await;
        let reply = expect(&mut client, Wanted::Id(7)).await;
        let items = reply["result"].as_array().expect("completion answers a list");
        let fn_item = items
            .iter()
            .find(|item| item["label"] == "fn" && item["kind"] == 15)
            .expect("the fn skeleton answers")
            .clone();
        assert!(
            fn_item["insertText"]
                .as_str()
                .is_some_and(|text| text.starts_with("fn ${1:name}")),
            "{}",
            fn_item["insertText"]
        );
        assert_eq!(fn_item["insertTextFormat"], 2, "{fn_item}");
        let labels: Vec<&str> = items.iter().filter_map(|i| i["label"].as_str()).collect();
        let idx = |name: &str| {
            labels
                .iter()
                .position(|got| *got == name)
                .unwrap_or_else(|| panic!("`{name}` answers: {labels:?}"))
        };
        assert!(idx("fnord") < idx("fn"), "the name match outranks the skeleton: {labels:?}");

        send(&mut client, r#"{"jsonrpc":"2.0","id":8,"method":"shutdown"}"#).await;
        let reply = expect(&mut client, Wanted::Id(8)).await;
        assert!(reply.get("error").is_none(), "{reply}");
        send(&mut client, &notification("exit", &json!({}))).await;
    })
    .await
    .expect("the cycle finishes");
}

/// Which import edit one document's answer owes over the wire.
#[derive(Clone, Copy)]
enum ImportCase {
    /// A fresh row opens the document at its first item.
    Fresh,
    /// A merge appends the name after the row's last selection.
    Merge,
    /// The row already selects the name: no edit at all.
    Picked,
}

impl ImportCase {
    /// The case's document (its own URI spelling and source).
    fn document(self) -> (String, String) {
        let (name, source) = match self {
            Self::Fresh => ("fresh", "const text: string = he\n"),
            Self::Merge => (
                "merge",
                "import @std/io (println)\nconst text: string = he\n",
            ),
            Self::Picked => ("picked", "import @std (hex)\nconst text: string = he\n"),
        };
        let uri = document_uri(&format!("ds_lsp_{name}"));
        (uri, source.to_owned())
    }
}

/// The auto-import wire: an unresolved typed prefix attaches the import
/// edit to the exported ambient member's item — a fresh row opening the
/// document, a merge into the file's existing `@std` row, and no edit
/// when the row already selects the name.
#[tokio::test]
async fn completion_offers_auto_imports_over_the_wire() {
    timeout(Duration::from_secs(120), async {
        let (server, mut client) = tokio::io::duplex(16 << 10);
        let (stdin, stdout) = tokio::io::split(server);
        tokio::spawn(async move {
            ds_lsp::serve(stdin, stdout)
                .await
                .expect("the server serves");
        });

        send(
            &mut client,
            &request(
                1,
                "initialize",
                &json!({"processId": null, "rootUri": null, "capabilities": {}}),
            ),
        )
        .await;
        expect(&mut client, Wanted::Id(1)).await;
        send(&mut client, &notification("initialized", &json!({}))).await;

        // The edit of the `hex` item in each document's answer.
        for (id, case) in [ImportCase::Fresh, ImportCase::Merge, ImportCase::Picked]
            .into_iter()
            .enumerate()
        {
            let id = id as u64 + 2;
            let (uri, source) = case.document();
            send(
                &mut client,
                &notification(
                    "textDocument/didOpen",
                    &json!({"textDocument": {
                        "uri": uri,
                        "languageId": "dreamscript",
                        "version": 1,
                        "text": source,
                    }}),
                ),
            )
            .await;
            send(
                &mut client,
                &request(
                    id,
                    "textDocument/completion",
                    &json!({
                        "textDocument": {"uri": uri},
                        // The typed prefix ends the document's last line
                        // in every case.
                        "position": {"line": source.lines().count() - 1, "character": 26},
                    }),
                ),
            )
            .await;
            let reply = expect(&mut client, Wanted::Id(id)).await;
            let items = reply["result"]
                .as_array()
                .expect("completion answers a list");
            let item = items
                .iter()
                .find(|item| item["label"] == "hex")
                .expect("the exported ambient member answers");
            let edit = item["additionalTextEdits"]
                .as_array()
                .and_then(|edits| edits.first());
            match case {
                ImportCase::Fresh => {
                    let edit = edit.expect("the offer imports");
                    assert_eq!(
                        edit["range"],
                        json!({"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 0}}),
                        "{edit}"
                    );
                    assert_eq!(edit["newText"], "import @std (hex)\n\n", "{edit}");
                },
                ImportCase::Merge => {
                    let edit = edit.expect("the offer imports");
                    assert_eq!(
                        edit["range"],
                        json!({"start": {"line": 0, "character": 23}, "end": {"line": 0, "character": 23}}),
                        "{edit}"
                    );
                    assert_eq!(edit["newText"], ", hex", "{edit}");
                },
                ImportCase::Picked => assert!(
                    edit.is_none(),
                    "an already-imported name carries no edit: {item}"
                ),
            }
        }

        send(&mut client, r#"{"jsonrpc":"2.0","id":5,"method":"shutdown"}"#).await;
        let reply = expect(&mut client, Wanted::Id(5)).await;
        assert!(reply.get("error").is_none(), "{reply}");
        send(&mut client, &notification("exit", &json!({}))).await;
    })
    .await
    .expect("the cycle finishes");
}

/// The std-member wire: the composed candidate set's member half answers
/// over the wire — the `@std` package's own member (`urls`, the members'
/// table's name riding inside the merged ambient's own item) behind an
/// unresolved typed prefix, carrying the fresh `@std` row.
#[tokio::test]
async fn completion_offers_the_std_members_over_the_wire() {
    timeout(Duration::from_secs(120), async {
        let uri = document_uri("ds_lsp_std_member");
        let source = "fn probe() -> list[string] {\n  url\n}\n";
        let (server, mut client) = tokio::io::duplex(16 << 10);
        let (stdin, stdout) = tokio::io::split(server);
        tokio::spawn(async move {
            ds_lsp::serve(stdin, stdout)
                .await
                .expect("the server serves");
        });

        send(
            &mut client,
            &request(
                1,
                "initialize",
                &json!({"processId": null, "rootUri": null, "capabilities": {}}),
            ),
        )
        .await;
        expect(&mut client, Wanted::Id(1)).await;
        send(&mut client, &notification("initialized", &json!({}))).await;

        send(
            &mut client,
            &notification(
                "textDocument/didOpen",
                &json!({"textDocument": {
                    "uri": uri,
                    "languageId": "dreamscript",
                    "version": 1,
                    "text": source,
                }}),
            ),
        )
        .await;
        send(
            &mut client,
            &request(
                2,
                "textDocument/completion",
                &json!({
                    "textDocument": {"uri": uri},
                    // The cursor sits right after the typed prefix `url`.
                    "position": {"line": 1, "character": 5},
                }),
            ),
        )
        .await;
        let reply = expect(&mut client, Wanted::Id(2)).await;
        let items = reply["result"]
            .as_array()
            .expect("completion answers a list");
        let item = items
            .iter()
            .find(|item| item["label"] == "urls")
            .expect("the std member answers");
        let edit = item["additionalTextEdits"]
            .as_array()
            .and_then(|edits| edits.first())
            .expect("the offer imports");
        assert_eq!(
            edit["range"],
            json!({"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 0}}),
            "{edit}"
        );
        assert_eq!(edit["newText"], "import @std (urls)\n\n", "{edit}");

        send(
            &mut client,
            r#"{"jsonrpc":"2.0","id":3,"method":"shutdown"}"#,
        )
        .await;
        let reply = expect(&mut client, Wanted::Id(3)).await;
        assert!(reply.get("error").is_none(), "{reply}");
        send(&mut client, &notification("exit", &json!({}))).await;
    })
    .await
    .expect("the cycle finishes");
}

/// Hover's effect row reads the tightest covering expression, not the
/// enclosing definition: the world-effecting call inside a function whose
/// definition row also traps answers `world`, the trapping arithmetic
/// answers `trap`.
#[tokio::test]
async fn hover_answers_the_tightest_expression_row() {
    timeout(Duration::from_secs(120), async {
        let uri = document_uri("ds_lsp_rows");
        let uri = uri.as_str();
        let (server, mut client) = tokio::io::duplex(16 << 10);
        let (stdin, stdout) = tokio::io::split(server);
        tokio::spawn(async move {
            ds_lsp::serve(stdin, stdout)
                .await
                .expect("the server serves");
        });

        send(
            &mut client,
            &request(
                1,
                "initialize",
                &json!({"processId": null, "rootUri": null, "capabilities": {}}),
            ),
        )
        .await;
        expect(&mut client, Wanted::Id(1)).await;
        send(&mut client, &notification("initialized", &json!({}))).await;

        let source = concat!(
            "fn noisy() -> int {\n",
            "  println(\"side\")\n",
            "  1 + 2\n",
            "}\n",
        );
        send(
            &mut client,
            &notification(
                "textDocument/didOpen",
                &json!({"textDocument": {
                    "uri": uri,
                    "languageId": "dreamscript",
                    "version": 1,
                    "text": source,
                }}),
            ),
        )
        .await;
        // The open publishes nothing (the document is clean); the request
        // loop below would skip it anyway.
        let hover = |id: u64, line: u32, character: u32| {
            request(
                id,
                "textDocument/hover",
                &json!({"textDocument": {"uri": uri}, "position": {"line": line, "character": character}}),
            )
        };

        // On the world-effecting call: the call's own row, not the
        // definition's `trap | world`.
        send(&mut client, &hover(2, 1, 4)).await;
        let reply = expect(&mut client, Wanted::Id(2)).await;
        let value = reply["result"]["contents"]["value"]
            .as_str()
            .expect("the hover carries markdown contents");
        assert!(value.contains("~ world"), "{value}");
        assert!(!value.contains("trap"), "{value}");

        // In the arithmetic's operator gap: the sum's own trap.
        send(&mut client, &hover(3, 2, 5)).await;
        let reply = expect(&mut client, Wanted::Id(3)).await;
        let value = reply["result"]["contents"]["value"]
            .as_str()
            .expect("the hover carries markdown contents");
        assert!(value.contains("~ trap"), "{value}");

        send(&mut client, r#"{"jsonrpc":"2.0","id":4,"method":"shutdown"}"#).await;
        let reply = expect(&mut client, Wanted::Id(4)).await;
        assert!(reply.get("error").is_none(), "{reply}");
        send(&mut client, &notification("exit", &json!({}))).await;
    })
    .await
    .expect("the cycle finishes");
}

/// The semantic-tokens wire: the capability registers with its legend and
/// full serving, and `textDocument/semanticTokens/full` answers the flat
/// delta array — the leading comment first, the keywords, literals, and
/// resolved names where the session's decode agrees.
#[tokio::test]
async fn semantic_tokens_answer_over_the_wire() {
    timeout(Duration::from_secs(120), async {
        let uri = document_uri("ds_lsp_tokens");
        let uri = uri.as_str();
        let (server, mut client) = tokio::io::duplex(16 << 10);
        let (stdin, stdout) = tokio::io::split(server);
        tokio::spawn(async move {
            ds_lsp::serve(stdin, stdout)
                .await
                .expect("the server serves");
        });

        send(
            &mut client,
            &request(
                1,
                "initialize",
                &json!({"processId": null, "rootUri": null, "capabilities": {}}),
            ),
        )
        .await;
        let reply = expect(&mut client, Wanted::Id(1)).await;
        let provider = &reply["result"]["capabilities"]["semanticTokensProvider"];
        assert!(provider.is_object(), "{provider}");
        let legend = &provider["legend"];
        assert!(
            legend["tokenTypes"]
                .as_array()
                .is_some_and(|t| !t.is_empty()),
            "the legend carries the token types: {legend}"
        );
        assert_eq!(provider["full"], true, "full serving registers: {provider}");
        assert_eq!(provider["range"], false, "no range serving: {provider}");
        let token_types: Vec<&str> = legend["tokenTypes"]
            .as_array()
            .expect("the legend is a list")
            .iter()
            .filter_map(|t| t.as_str())
            .collect();
        assert_eq!(token_types[0], "keyword");
        assert!(token_types.contains(&"comment"), "{token_types:?}");
        assert!(token_types.contains(&"enumMember"), "{token_types:?}");
        let modifiers: Vec<&str> = legend["tokenModifiers"]
            .as_array()
            .expect("the modifiers are a list")
            .iter()
            .filter_map(|m| m.as_str())
            .collect();
        assert_eq!(modifiers, ["declaration", "readonly"], "{modifiers:?}");
        send(&mut client, &notification("initialized", &json!({}))).await;

        let source = concat!(
            "// a leading comment\n",
            "const msg: string = \"hi\"\n",
            "fn clean() -> string {\n",
            "  join(msg)\n",
            "}\n",
        );
        send(
            &mut client,
            &notification(
                "textDocument/didOpen",
                &json!({"textDocument": {
                    "uri": uri,
                    "languageId": "dreamscript",
                    "version": 1,
                    "text": source,
                }}),
            ),
        )
        .await;
        // The open publishes nothing (the document is clean); the request
        // loop below would skip it anyway.
        send(
            &mut client,
            &request(
                2,
                "textDocument/semanticTokens/full",
                &json!({"textDocument": {"uri": uri}}),
            ),
        )
        .await;
        let reply = expect(&mut client, Wanted::Id(2)).await;
        let data = reply["result"]["data"]
            .as_array()
            .expect("tokens answer a flat array")
            .iter()
            .filter_map(Value::as_u64)
            .collect::<Vec<_>>();
        assert_eq!(data.len() % 5, 0, "the array is five u32s per token");
        // The first token is the leading comment: line 0, character 0,
        // twenty units, the legend's comment index, no modifiers.
        assert_eq!(&data[..5], &[0, 0, 20, 2, 0], "{data:?}");
        // The decoded stream names the document's pieces in order: the
        // `const` keyword on line 1, the `msg` declaration with
        // declaration and readonly bits, and the free call's callee.
        let keyword = token_types.iter().position(|t| *t == "keyword").unwrap();
        let decoded: Vec<[u64; 5]> = data
            .chunks(5)
            .map(|c| [c[0], c[1], c[2], c[3], c[4]])
            .collect();
        assert_eq!(
            decoded[1],
            [1, 0, 5, keyword as u64, 0],
            "`const` opens line 1: {decoded:?}"
        );
        // `msg` (three units) answers variable with declaration (bit 0)
        // and readonly (bit 1).
        let variable = token_types.iter().position(|t| *t == "variable").unwrap();
        assert_eq!(decoded[2], [0, 6, 3, variable as u64, 3], "{decoded:?}");
        // `join` answers a function at its use.
        let function = token_types.iter().position(|t| *t == "function").unwrap();
        assert!(
            decoded.iter().any(|t| t[2] == 4 && t[3] == function as u64),
            "the dot-call's free function answers: {decoded:?}"
        );

        send(
            &mut client,
            r#"{"jsonrpc":"2.0","id":3,"method":"shutdown"}"#,
        )
        .await;
        let reply = expect(&mut client, Wanted::Id(3)).await;
        assert!(reply.get("error").is_none(), "{reply}");
        send(&mut client, &notification("exit", &json!({}))).await;
    })
    .await
    .expect("the cycle finishes");
}

/// The incremental-sync wire: ranged edits splice into the held
/// document through the advertised sync kind, and the debounced
/// publish coalesces a burst of change notifications into one
/// publish (the last one's version tag) followed by silence; a
/// rangeless change still replaces the whole text, and a save rides
/// the same debounce.
#[tokio::test]
async fn incremental_sync_debounces_the_publish() {
    timeout(Duration::from_secs(120), async {
        let uri = document_uri("ds_lsp_incremental");
        let uri = uri.as_str();
        let (server, mut client) = tokio::io::duplex(16 << 10);
        let (stdin, stdout) = tokio::io::split(server);
        tokio::spawn(async move {
            ds_lsp::serve(stdin, stdout)
                .await
                .expect("the server serves");
        });

        send(
            &mut client,
            &request(
                1,
                "initialize",
                &json!({"processId": null, "rootUri": null, "capabilities": {}}),
            ),
        )
        .await;
        let reply = expect(&mut client, Wanted::Id(1)).await;
        let sync = &reply["result"]["capabilities"]["textDocumentSync"];
        assert_eq!(sync["openClose"], true);
        assert_eq!(sync["change"], 2);
        assert_eq!(sync["save"], true);
        send(&mut client, &notification("initialized", &json!({}))).await;

        // The document opens clean.
        send(
            &mut client,
            &notification(
                "textDocument/didOpen",
                &json!({"textDocument": {
                    "uri": uri,
                    "languageId": "dreamscript",
                    "version": 1,
                    "text": "const msg: string = \"hi\"\n",
                }}),
            ),
        )
        .await;
        let published = expect(
            &mut client,
            Wanted::Method("textDocument/publishDiagnostics"),
        )
        .await;
        assert_eq!(published["params"]["version"], 1);
        assert!(
            published["params"]["diagnostics"]
                .as_array()
                .expect("the diagnostics are a list")
                .is_empty(),
            "{}",
            published["params"]["diagnostics"]
        );

        // A burst of three ranged edits inside one debounce window: the
        // annotation breaks, then a leading comment line shifts the
        // mismatch down, then the annotation on the moved line breaks a
        // second binding.
        let change = |version, range, text| {
            json!({
                "textDocument": {"uri": uri, "version": version},
                "contentChanges": [{"range": range, "text": text}],
            })
        };
        send(
            &mut client,
            &notification(
                "textDocument/didChange",
                &change(
                    2,
                    json!({"start": {"line": 0, "character": 11}, "end": {"line": 0, "character": 17}}),
                    "int",
                ),
            ),
        )
        .await;
        send(
            &mut client,
            &notification(
                "textDocument/didChange",
                &change(
                    3,
                    json!({"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 0}}),
                    "// lead\n",
                ),
            ),
        )
        .await;
        send(
            &mut client,
            &notification(
                "textDocument/didChange",
                &change(
                    4,
                    json!({"start": {"line": 2, "character": 0}, "end": {"line": 2, "character": 0}}),
                    "const broken: string = 1\n",
                ),
            ),
        )
        .await;

        // One publish lands after the window: the last version's tag,
        // carrying both mismatches on their shifted lines.
        let published = expect(
            &mut client,
            Wanted::Method("textDocument/publishDiagnostics"),
        )
        .await;
        assert_eq!(published["params"]["uri"], uri);
        assert_eq!(published["params"]["version"], 4);
        let diags = published["params"]["diagnostics"]
            .as_array()
            .expect("the diagnostics are a list");
        let lines: Vec<u64> = diags
            .iter()
            .map(|diag| diag["range"]["start"]["line"].as_u64().expect("a line"))
            .collect();
        assert_eq!(lines, [1, 2], "{diags:?}");

        // No further publish follows inside another window: the burst
        // coalesced into exactly one.
        assert!(
            timeout(Duration::from_millis(600), recv(&mut client))
                .await
                .is_err(),
            "the burst published once"
        );

        // A rangeless change (the full-text client's path) replaces the
        // whole document and publishes clean through the window.
        send(
            &mut client,
            &notification(
                "textDocument/didChange",
                &json!({
                    "textDocument": {"uri": uri, "version": 5},
                    "contentChanges": [{
                        "text": "const msg: string = \"hi\"\n",
                    }],
                }),
            ),
        )
        .await;
        let published = expect(
            &mut client,
            Wanted::Method("textDocument/publishDiagnostics"),
        )
        .await;
        assert_eq!(published["params"]["version"], 5);
        assert!(
            published["params"]["diagnostics"]
                .as_array()
                .expect("the diagnostics are a list")
                .is_empty(),
            "{}",
            published["params"]["diagnostics"]
        );

        // A save asks for one fresh publish through the same window,
        // untagged (a save carries no version).
        send(
            &mut client,
            &notification(
                "textDocument/didSave",
                &json!({"textDocument": {"uri": uri}}),
            ),
        )
        .await;
        let published = expect(
            &mut client,
            Wanted::Method("textDocument/publishDiagnostics"),
        )
        .await;
        assert!(published["params"]["version"].is_null());
        assert!(
            published["params"]["diagnostics"]
                .as_array()
                .expect("the diagnostics are a list")
                .is_empty(),
            "{}",
            published["params"]["diagnostics"]
        );

        send(
            &mut client,
            r#"{"jsonrpc":"2.0","id":2,"method":"shutdown"}"#,
        )
        .await;
        let reply = expect(&mut client, Wanted::Id(2)).await;
        assert!(reply.get("error").is_none(), "{reply}");
        send(&mut client, &notification("exit", &json!({}))).await;
    })
    .await
    .expect("the cycle finishes");
}

/// Rename of registered members over the wire: an inherent member with its
/// dot-calls, and a trait member covering its declaration, its impl, its
/// qualified escape-hatch use, and every call site that selects it —
/// prepare-rename, rename, and references answering from both the
/// declaration and the dot-call.
#[tokio::test]
async fn rename_answers_members_over_the_wire() {
    timeout(Duration::from_secs(120), async {
        let uri = document_uri("ds_lsp_member");
        let uri = uri.as_str();
        let (server, mut client) = tokio::io::duplex(16 << 10);
        let (stdin, stdout) = tokio::io::split(server);
        tokio::spawn(async move {
            ds_lsp::serve(stdin, stdout)
                .await
                .expect("the server serves");
        });

        send(
            &mut client,
            &request(
                1,
                "initialize",
                &json!({"processId": null, "rootUri": null, "capabilities": {}}),
            ),
        )
        .await;
        expect(&mut client, Wanted::Id(1)).await;
        send(&mut client, &notification("initialized", &json!({}))).await;

        let source = concat!(
            "trait pretty {\n",
            "  fn show(self) -> string {\n",
            "    \"pretty\"\n",
            "  }\n",
            "}\n",
            "struct point { x: int, y: int }\n",
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
            "fn render(p: point) -> string {\n",
            "  pretty::show(p)\n",
            "  p.show()\n",
            "}\n",
            "fn total(p: point) -> int {\n",
            "  p.norm()\n",
            "}\n",
        );
        send(
            &mut client,
            &notification(
                "textDocument/didOpen",
                &json!({"textDocument": {
                    "uri": uri,
                    "languageId": "dreamscript",
                    "version": 1,
                    "text": source,
                }}),
            ),
        )
        .await;
        // The open publishes diagnostics; the request loop below skips
        // them.
        let rename = |id: u64, line: u32, character: u32, new_name: &str| {
            request(
                id,
                "textDocument/rename",
                &json!({
                    "textDocument": {"uri": uri},
                    "position": {"line": line, "character": character},
                    "newName": new_name,
                }),
            )
        };

        // Rename of the trait member from its declaration: the trait's
        // own member, the impl's member, the qualified use, and the
        // dot-call that selects it all edit.
        send(&mut client, &rename(2, 1, 8, "display")).await;
        let reply = expect(&mut client, Wanted::Id(2)).await;
        let edits = reply["result"]["changes"][uri]
            .as_array()
            .expect("rename answers document changes");
        assert_eq!(edits.len(), 4, "{edits:?}");
        let starts: Vec<(u64, u64)> = edits
            .iter()
            .map(|edit| {
                (
                    edit["range"]["start"]["line"].as_u64().expect("a line"),
                    edit["range"]["start"]["character"]
                        .as_u64()
                        .expect("a character"),
                )
            })
            .collect();
        assert_eq!(starts, [(1, 5), (7, 5), (17, 10), (18, 4)], "{edits:?}");
        assert_eq!(edits[3]["newText"], "display");

        // Prepare-rename at the dot-call answers the callee's range and
        // spelling, and references answer the whole member set: the two
        // declarations, the qualified use, and the selecting call site.
        send(
            &mut client,
            &request(
                3,
                "textDocument/prepareRename",
                &json!({"textDocument": {"uri": uri}, "position": {"line": 18, "character": 6}}),
            ),
        )
        .await;
        let reply = expect(&mut client, Wanted::Id(3)).await;
        assert_eq!(reply["result"]["range"]["start"]["line"], 18);
        assert_eq!(reply["result"]["range"]["start"]["character"], 4);
        assert_eq!(reply["result"]["range"]["end"]["character"], 8);
        assert_eq!(reply["result"]["placeholder"], "show");
        send(
            &mut client,
            &request(
                4,
                "textDocument/references",
                &json!({
                    "textDocument": {"uri": uri},
                    "position": {"line": 18, "character": 6},
                    "context": {"includeDeclaration": true},
                }),
            ),
        )
        .await;
        let reply = expect(&mut client, Wanted::Id(4)).await;
        let locations = reply["result"]
            .as_array()
            .expect("references answer a location list");
        let starts: Vec<(u64, u64)> = locations
            .iter()
            .map(|location| {
                (
                    location["range"]["start"]["line"].as_u64().expect("a line"),
                    location["range"]["start"]["character"]
                        .as_u64()
                        .expect("a character"),
                )
            })
            .collect();
        assert_eq!(starts, [(1, 5), (7, 5), (17, 10), (18, 4)], "{locations:?}");

        // Rename of the inherent member from its dot-call edits the
        // declaration and the call site alike.
        send(&mut client, &rename(5, 21, 6, "length")).await;
        let reply = expect(&mut client, Wanted::Id(5)).await;
        let edits = reply["result"]["changes"][uri]
            .as_array()
            .expect("rename answers document changes");
        assert_eq!(edits.len(), 2, "{edits:?}");
        assert_eq!(edits[0]["range"]["start"]["line"], 12);
        assert_eq!(edits[0]["range"]["start"]["character"], 5);
        assert_eq!(edits[1]["range"]["start"]["line"], 21);
        assert_eq!(edits[1]["range"]["start"]["character"], 4);
        assert_eq!(edits[1]["newText"], "length");

        send(
            &mut client,
            r#"{"jsonrpc":"2.0","id":6,"method":"shutdown"}"#,
        )
        .await;
        let reply = expect(&mut client, Wanted::Id(6)).await;
        assert!(reply.get("error").is_none(), "{reply}");
        send(&mut client, &notification("exit", &json!({}))).await;
    })
    .await
    .expect("the cycle finishes");
}

/// Rename refuses where no single member is named even under the
/// checker's type-directed selection: a dot-call whose candidate set is
/// truly ambiguous (two traits claiming one member name for one
/// receiver) failed its own check and answers an error, as does a member
/// the stdlib ambient declares (no open document can edit it).
#[tokio::test]
async fn rename_refuses_where_no_member_is_selected() {
    timeout(Duration::from_secs(120), async {
        let uri = document_uri("ds_lsp_refuse");
        let uri = uri.as_str();
        let (server, mut client) = tokio::io::duplex(16 << 10);
        let (stdin, stdout) = tokio::io::split(server);
        tokio::spawn(async move {
            ds_lsp::serve(stdin, stdout)
                .await
                .expect("the server serves");
        });

        send(
            &mut client,
            &request(
                1,
                "initialize",
                &json!({"processId": null, "rootUri": null, "capabilities": {}}),
            ),
        )
        .await;
        expect(&mut client, Wanted::Id(1)).await;
        send(&mut client, &notification("initialized", &json!({}))).await;

        let source = concat!(
            "trait a { fn f(self) -> int { 1 } }\n",
            "trait b { fn f(self) -> int { 2 } }\n",
            "tag unit\n",
            "impl a for unit { fn f(self) -> int { 1 } }\n",
            "impl b for unit { fn f(self) -> int { 2 } }\n",
            "const v = unit\n",
            "const n = v.f()\n",
            "const text: string = \"x\"\n",
            "const shown = text.show()\n",
        );
        send(
            &mut client,
            &notification(
                "textDocument/didOpen",
                &json!({"textDocument": {
                    "uri": uri,
                    "languageId": "dreamscript",
                    "version": 1,
                    "text": source,
                }}),
            ),
        )
        .await;
        // The open publishes diagnostics (the ambiguous call fails its
        // own check); the request loop below skips them.

        // The ambiguous dot-call names no member: rename refuses.
        send(
            &mut client,
            &request(
                2,
                "textDocument/rename",
                &json!({
                    "textDocument": {"uri": uri},
                    "position": {"line": 6, "character": 12},
                    "newName": "g",
                }),
            ),
        )
        .await;
        let reply = expect(&mut client, Wanted::Id(2)).await;
        let message = reply["error"]["message"]
            .as_str()
            .expect("the refusal carries a message");
        assert!(message.contains("did not settle"), "{message}");

        // The ambient's own trait member (selected at the dot-call
        // through the checker's instance solve) refuses the edit: no
        // open document carries the stdlib source.
        send(
            &mut client,
            &request(
                3,
                "textDocument/rename",
                &json!({
                    "textDocument": {"uri": uri},
                    "position": {"line": 8, "character": 20},
                    "newName": "render",
                }),
            ),
        )
        .await;
        let reply = expect(&mut client, Wanted::Id(3)).await;
        let message = reply["error"]["message"]
            .as_str()
            .expect("the refusal carries a message");
        assert!(message.contains("ambient"), "{message}");

        send(
            &mut client,
            r#"{"jsonrpc":"2.0","id":4,"method":"shutdown"}"#,
        )
        .await;
        let reply = expect(&mut client, Wanted::Id(4)).await;
        assert!(reply.get("error").is_none(), "{reply}");
        send(&mut client, &notification("exit", &json!({}))).await;
    })
    .await
    .expect("the cycle finishes");
}

/// The linked-name hover wire: a document importing `@lib` and using the
/// selection hovers the served surface's signature line — the provider
/// the embedder installs over the duplex pair serving what a run of the
/// same package graph cached beside the dep's compiled module.
#[tokio::test]
async fn hover_answers_linked_names_over_the_wire() {
    timeout(Duration::from_secs(120), async {
        let uri = document_uri("ds_lsp_linked");
        let uri = uri.as_str();
        let (server, mut client) = tokio::io::duplex(16 << 10);
        let (stdin, stdout) = tokio::io::split(server);
        let surface = std::sync::Arc::new(ds_lsp::LinkedSurface {
            signatures: std::collections::HashMap::from([(
                "pick".to_owned(),
                "pub fn pick(n: int) -> int".to_owned(),
            )]),
            docs: std::collections::HashMap::from([("pick".to_owned(), vec![
                "The identity: the number back, unchanged.".to_owned(),
            ])]),
        });
        let provider = std::sync::Arc::new(move |_doc: &std::path::Path, link: &str| {
            (link == "@lib").then(|| surface.clone())
        });
        tokio::spawn(async move {
            ds_lsp::serve_with(stdin, stdout, provider)
                .await
                .expect("the server serves");
        });

        send(
            &mut client,
            &request(
                1,
                "initialize",
                &json!({"processId": null, "rootUri": null, "capabilities": {}}),
            ),
        )
        .await;
        expect(&mut client, Wanted::Id(1)).await;
        send(&mut client, &notification("initialized", &json!({}))).await;

        let source = concat!(
            "import @lib (pick)\n",
            "\n",
            "fn main() -> int {\n",
            "  pick(3)\n",
            "}\n",
        );
        send(
            &mut client,
            &notification(
                "textDocument/didOpen",
                &json!({"textDocument": {
                    "uri": uri,
                    "languageId": "dreamscript",
                    "version": 1,
                    "text": source,
                }}),
            ),
        )
        .await;
        // The open publishes the unresolved-use diagnostics (the session
        // carries no package graph); the request loop below skips them.

        // `pick` in the body sits at line 3 chars 2..6.
        send(
            &mut client,
            &request(
                2,
                "textDocument/hover",
                &json!({"textDocument": {"uri": uri}, "position": {"line": 3, "character": 4}}),
            ),
        )
        .await;
        let reply = expect(&mut client, Wanted::Id(2)).await;
        let value = reply["result"]["contents"]["value"]
            .as_str()
            .expect("the hover carries markdown contents");
        assert!(
            value.contains("pub fn pick(n: int) -> int"),
            "the linked tier's line answers: {value}"
        );

        send(
            &mut client,
            r#"{"jsonrpc":"2.0","id":3,"method":"shutdown"}"#,
        )
        .await;
        let reply = expect(&mut client, Wanted::Id(3)).await;
        assert!(reply.get("error").is_none(), "{reply}");
        send(&mut client, &notification("exit", &json!({}))).await;
    })
    .await
    .expect("the cycle finishes");
}
