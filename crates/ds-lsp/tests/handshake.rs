//! In-process smoke test of the whole server: the handshake over one
//! duplex pair, a document open with published diagnostics, hover,
//! definition, symbols, formatting, and the shutdown cycle.

use std::time::Duration;

use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt, DuplexStream},
    time::timeout,
};

/// The document every request addresses; the path need not exist on
/// disk (the server never reads behind a URI).
const URI: &str = "file:///tmp/ds_lsp_smoke.ds";

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
        assert_eq!(reply["result"]["capabilities"]["textDocumentSync"], 1);
        assert_eq!(reply["result"]["capabilities"]["hoverProvider"], true);
        assert_eq!(reply["result"]["capabilities"]["definitionProvider"], true);
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
            "const text: string = trim(msg)\n",
        );
        send(
            &mut client,
            &notification(
                "textDocument/didOpen",
                &json!({"textDocument": {
                    "uri": URI,
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
        assert_eq!(published["params"]["uri"], URI);
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
                &json!({"textDocument": {"uri": URI}, "position": {"line": 0, "character": 22}}),
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
                &json!({"textDocument": {"uri": URI}, "position": {"line": 2, "character": 28}}),
            ),
        )
        .await;
        let reply = expect(&mut client, Wanted::Id(3)).await;
        let locations = reply["result"]
            .as_array()
            .expect("definition answers a location list");
        assert_eq!(locations.len(), 1, "{locations:?}");
        assert_eq!(locations[0]["uri"], URI);
        assert_eq!(locations[0]["range"]["start"]["line"], 0);
        assert_eq!(locations[0]["range"]["start"]["character"], 6);

        // The top-level symbols name the three bindings.
        send(
            &mut client,
            &request(
                4,
                "textDocument/documentSymbol",
                &json!({"textDocument": {"uri": URI}}),
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
                    "textDocument": {"uri": URI},
                    "options": {"tabSize": 4, "insertSpaces": false},
                }),
            ),
        )
        .await;
        let reply = expect(&mut client, Wanted::Id(5)).await;
        assert!(
            reply["result"].is_array() && reply["result"].as_array().unwrap().is_empty(),
            "{}",
            reply["result"]
        );

        // The shutdown cycle answers and ends the server: the request
        // carries no params at all (a params object is a protocol error).
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
