//! End-to-end `.dsi` surface serving through the binary: a two-package
//! graph's run caches each linked dep member's checked prefix beside its
//! compiled dsmod, `ds info` serves the root's surface from the cached
//! artifact, and `ds lsp` hovers a linked name's signature off the same
//! cached dep artifact — one key fold, three consumers.

use std::{
    env, fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
};

use ds_diag::FileId;
use ds_mod::Hash128;
use ds_pkg::{Manifest, Package};

mod support;

/// The linked library package's member: one exported fn the root
/// selects.
const LIB_MEMBER: &str = concat!(
    "pub fn pick(n: int) -> int {\n",
    "  n\n",
    "}\n",
    "\n",
    "pub struct span_box {\n",
    "  n: int\n",
    "}\n",
    "\n",
    "fn hidden(n: int) -> int {\n",
    "  n\n",
    "}\n",
);

/// The fixture root's source: imports the library, one exported fn of
/// its own beside the entry.
const ROOT_SOURCE: &str = concat!(
    "import @lib (pick)\n",
    "\n",
    "pub fn twice(n: int) -> int {\n",
    "  pick(n) * 2\n",
    "}\n",
    "\n",
    "fn main() -> int {\n",
    "  twice(21)\n",
    "}\n",
);

fn lib_manifest() -> String {
    "name = \"@lib\"\nversion = \"1.0.0\"\n".to_owned()
}

fn root_manifest(lib: Hash128) -> String {
    format!(
        "name = \"app\"\nversion = \"0.1.0\"\n\n[deps]\n@lib = {{ ver = \"1.0.0\", hash = \
         \"blake3:{lib}\" }}\n"
    )
}

/// The bundle identity a saved package installs under: the same canonical
/// bytes `ds save` frames from the tree.
fn bundle_identity(manifest: &str, sources: &[(&str, &str)]) -> Hash128 {
    let manifest = Manifest::parse(manifest, FileId::new(0)).expect("fixture manifest parses");
    let bundle = Package::new(
        &manifest,
        sources
            .iter()
            .map(|(path, text)| ((*path).to_owned(), text.as_bytes().to_vec())),
    )
    .expect("fixture bundle builds");
    bundle.content_hash()
}

/// Writes one package tree: the manifest plus its sources.
fn write_pkg(dir: &Path, manifest: &str, sources: &[(&str, &str)]) {
    fs::create_dir_all(dir).expect("the package dir creates");
    fs::write(dir.join("ds.pkg"), manifest).expect("the manifest writes");
    for (path, text) in sources {
        fs::write(dir.join(path), text).expect("the member writes");
    }
}

/// A unique guarded temp dir per tag, removed at the test thread's exit.
fn temp_dir(tag: &str) -> PathBuf {
    support::guarded_dir(&format!("ds-cli-dsi-surface-{tag}-"))
}

/// Runs the built binary against one cache directory, flattening the
/// outcome.
fn ds(cache: &Path, args: &[&str]) -> (bool, String, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_ds"))
        .env("DS_CACHE", cache)
        .env_remove("DS_REGISTRY")
        .args(args)
        .output()
        .expect("the ds binary runs");
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

/// Every artifact below one cache segment (say `ir/bytecode`), grouped by
/// extension stem: the `.dsmod` and `.dsi` families' landing shape.
fn artifacts_under(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                found.push(path);
            }
        }
    }
    found
}

/// The fixture tree (the library saved into the store, the root written
/// beside its manifest) and the private cache root, ready to run.
fn fixture(tag: &str) -> (PathBuf, PathBuf, PathBuf) {
    let root = temp_dir(&format!("{tag}-tree"));
    let cache = temp_dir(&format!("{tag}-cache"));
    let lib = root.join("lib");
    let app = root.join("app");
    write_pkg(&lib, &lib_manifest(), &[("lib.ds", LIB_MEMBER)]);
    let identity = bundle_identity(&lib_manifest(), &[("lib.ds", LIB_MEMBER)]);
    write_pkg(&app, &root_manifest(identity), &[("main.ds", ROOT_SOURCE)]);
    let (ok, _, err) = ds(&cache, &["save", lib.to_str().expect("utf-8 path")]);
    assert!(ok, "the library saves into the store (stderr: {err})");
    (app, lib, cache)
}

/// A package run caches each linked dep member's checked prefix beside
/// its compiled dsmod — the same key with the `.dsi` extension — and a
/// warm run serves the whole graph from disk unchanged.
#[test]
fn a_package_run_caches_the_dep_dsi_beside_its_dsmod() {
    let (app, _lib, cache) = fixture("beside");
    let entry = app.join("main.ds");
    let (ok, out, err) = ds(&cache, &[
        "run",
        "--print-result",
        entry.to_str().expect("utf-8 path"),
    ]);
    assert!(ok, "the run succeeds (stderr: {err})");
    assert_eq!(out, "42\n", "the fixture's answer");

    let bytecode = cache.join("ir").join("bytecode");
    let artifacts = artifacts_under(&bytecode);
    let of_ext = |ext: &str| -> Vec<&Path> {
        artifacts
            .iter()
            .filter(|path| path.extension().is_some_and(|found| found == ext))
            .map(PathBuf::as_path)
            .collect()
    };
    let dsmods = of_ext("dsmod");
    let dsis = of_ext("dsi");
    assert_eq!(
        dsmods.len(),
        3,
        "the root's, the dep's, and the dep's shared-tier dsmod: {dsmods:?}"
    );
    assert_eq!(dsis.len(), 1, "exactly the dep's .dsi lands: {dsis:?}");
    let dsi = dsis[0];
    assert!(
        dsmods
            .iter()
            .any(|dsmod| dsmod.file_stem() == dsi.file_stem()),
        "the .dsi sits beside the dep's dsmod under one key: {dsi:?} vs {dsmods:?}"
    );
    // the warm run serves the whole graph from disk, answer unchanged
    let (ok, warm, err) = ds(&cache, &[
        "run",
        "--print-result",
        entry.to_str().expect("utf-8 path"),
    ]);
    assert!(ok, "the warm run succeeds (stderr: {err})");
    assert_eq!(warm, out, "the warm answer matches the cold one");
    fs::remove_dir_all(app.parent().expect("the tree root")).expect("the tree removes");
    fs::remove_dir_all(&cache).expect("the cache removes");
}

/// `ds info` serves the root's surface from the cached checked prefix
/// when the run just wrote it, and answers the same document from
/// source when no cache holds the compile — the artifact a pure cache.
#[test]
fn ds_info_serves_the_cached_prefix_and_the_source_alike() {
    let (app, _lib, cache) = fixture("info");
    let entry = app.join("main.ds");
    let (ok, _, err) = ds(&cache, &["run", entry.to_str().expect("utf-8 path")]);
    assert!(ok, "the run succeeds (stderr: {err})");

    let golden = "pub fn twice(n: int) -> int\n";
    let (ok, warm, err) = ds(&cache, &["info", entry.to_str().expect("utf-8 path")]);
    assert!(ok, "the warm info succeeds (stderr: {err})");
    assert_eq!(warm, golden, "the served surface: {warm}");

    // a cache that holds no compile answers the source rendering, the
    // same document over this fixture's carried vocabulary
    let cold_cache = temp_dir("info-cold");
    let (ok, cold, err) = ds(&cold_cache, &["info", entry.to_str().expect("utf-8 path")]);
    assert!(ok, "the cold info succeeds (stderr: {err})");
    assert_eq!(cold, golden, "the source rendering: {cold}");
    fs::remove_dir_all(app.parent().expect("the tree root")).expect("the tree removes");
    fs::remove_dir_all(&cache).expect("the cache removes");
    fs::remove_dir_all(&cold_cache).expect("the cold cache removes");
}

/// One framed JSON-RPC message written to the server's stdin.
fn send(server: &mut Child, body: &str) {
    let stdin = server.stdin.as_mut().expect("the server reads stdin");
    stdin
        .write_all(format!("Content-Length: {}\r\n\r\n{body}", body.len()).as_bytes())
        .expect("the client writes");
    stdin.flush().expect("the client flushes");
}

/// One string JSON-escaped: the document text the didOpen ships.
fn json_string(text: &str) -> String {
    use std::fmt::Write as _;
    let mut out = String::from("\"");
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", u32::from(c));
            },
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// One framed JSON-RPC message read off the server's stdout.
fn recv(server: &mut Child) -> String {
    let stdout = server.stdout.as_mut().expect("the server writes stdout");
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        stdout.read_exact(&mut byte).expect("the server writes");
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
    stdout.read_exact(&mut body).expect("the body follows");
    String::from_utf8(body).expect("the body is UTF-8")
}

/// `ds lsp` hovers a linked name's signature off the dep's cached
/// `.dsi`: the provider resolves the document's own graph, reads the
/// artifact the run wrote beside the dep's dsmod, and the linked tier
/// answers the selection's line.
#[test]
fn ds_lsp_hovers_the_linked_name_from_the_cached_dsi() {
    let (app, _lib, cache) = fixture("lsp");
    let entry = app.join("main.ds");
    let (ok, _, err) = ds(&cache, &["run", entry.to_str().expect("utf-8 path")]);
    assert!(ok, "the run succeeds (stderr: {err})");

    let mut server = Command::new(env!("CARGO_BIN_EXE_ds"))
        .env("DS_CACHE", &cache)
        .env_remove("DS_REGISTRY")
        .args(["lsp"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("the ds binary runs");
    let request = |id: u64, method: &str, params: &str| {
        format!("{{\"jsonrpc\":\"2.0\",\"id\":{id},\"method\":\"{method}\",\"params\":{params}}}")
    };
    let notification = |method: &str, params: &str| {
        format!("{{\"jsonrpc\":\"2.0\",\"method\":\"{method}\",\"params\":{params}}}")
    };
    send(
        &mut server,
        &request(
            1,
            "initialize",
            "{\"processId\": null, \"rootUri\": null, \"capabilities\": {}}",
        ),
    );
    loop {
        let reply = recv(&mut server);
        if reply.contains("\"id\":1") {
            break;
        }
    }
    send(&mut server, &notification("initialized", "{}"));
    let uri = format!("file:///{}", entry.to_string_lossy().replace('\\', "/"));
    send(
        &mut server,
        &notification(
            "textDocument/didOpen",
            &format!(
                "{{\"textDocument\": {{\"uri\": \"{uri}\", \"languageId\": \"dreamscript\", \
                 \"version\": 1, \"text\": {}}}}}",
                json_string(ROOT_SOURCE)
            ),
        ),
    );
    // `pick` in `twice`'s body sits at line 3 chars 2..6; the cursor
    // lands on its `i`.
    send(
        &mut server,
        &request(
            2,
            "textDocument/hover",
            &format!(
                "{{\"textDocument\": {{\"uri\": \"{uri}\"}}, \"position\": {{\"line\": 3, \
                 \"character\": 4}}}}"
            ),
        ),
    );
    let mut hover = String::new();
    for _ in 0..8 {
        let reply = recv(&mut server);
        if reply.contains("\"id\":2") {
            hover = reply;
            break;
        }
    }
    assert!(
        hover.contains("pub fn pick(n: int) -> int"),
        "the linked tier answers the cached surface's line: {hover}"
    );
    assert!(
        !hover.contains("span_box"),
        "the private-heavy library surface serves one name's line alone: {hover}"
    );
    let _ = server.kill();
    let _ = server.wait();
    fs::remove_dir_all(app.parent().expect("the tree root")).expect("the tree removes");
    fs::remove_dir_all(&cache).expect("the cache removes");
}

/// The repin fixture's library member, v1: `pick` the root calls plus
/// `bonus`, the import selection the hovers read.
const REPIN_LIB_V1: &str = concat!(
    "pub fn pick(n: int) -> int {\n",
    "  n\n",
    "}\n",
    "\n",
    "pub fn bonus() -> int {\n",
    "  1\n",
    "}\n",
);

/// The repinned library member, v2: `bonus` gains a parameter, so the
/// served `.dsi` line the hover reads moves with the new pin while the
/// root's own call stays valid.
const REPIN_LIB_V2: &str = concat!(
    "pub fn pick(n: int) -> int {\n",
    "  n\n",
    "}\n",
    "\n",
    "pub fn bonus(scale: int) -> int {\n",
    "  scale\n",
    "}\n",
);

/// The repin fixture's root source: `pick` called, `bonus` imported
/// and never called, so both pins of the library type-check.
const REPIN_ROOT_SOURCE: &str = concat!(
    "import @lib (pick, bonus)\n",
    "\n",
    "pub fn twice(n: int) -> int {\n",
    "  pick(n) * 2\n",
    "}\n",
    "\n",
    "fn main() -> int {\n",
    "  twice(21)\n",
    "}\n",
);

/// A dep repin mid-session lands with no restart: the provider's
/// memo guards each root's resolved keys behind the consumed inputs'
/// content fingerprint, so the rewritten manifest's pin re-resolves
/// the root on the next hover and the served `.dsi` line moves with
/// the new pin — the same server session across the repin, and the
/// unchanged manifest after it standing on the memo.
#[test]
fn a_mid_session_repin_serves_the_new_surface_without_restart() {
    let root = temp_dir("repin-tree");
    let cache = temp_dir("repin-cache");
    let lib = root.join("lib");
    let app = root.join("app");
    write_pkg(&lib, &lib_manifest(), &[("lib.ds", REPIN_LIB_V1)]);
    let v1 = bundle_identity(&lib_manifest(), &[("lib.ds", REPIN_LIB_V1)]);
    write_pkg(&app, &root_manifest(v1), &[("main.ds", REPIN_ROOT_SOURCE)]);
    let entry = app.join("main.ds");
    let (ok, _, err) = ds(&cache, &["save", lib.to_str().expect("utf-8 path")]);
    assert!(ok, "the v1 library saves into the store (stderr: {err})");
    let (ok, out, err) = ds(&cache, &[
        "run",
        "--print-result",
        entry.to_str().expect("utf-8 path"),
    ]);
    assert!(ok, "the v1 run caches the dep's .dsi (stderr: {err})");
    assert_eq!(out, "42\n");

    let mut server = Command::new(env!("CARGO_BIN_EXE_ds"))
        .env("DS_CACHE", &cache)
        .env_remove("DS_REGISTRY")
        .args(["lsp"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("the ds binary runs");
    let request = |id: u64, method: &str, params: &str| {
        format!("{{\"jsonrpc\":\"2.0\",\"id\":{id},\"method\":\"{method}\",\"params\":{params}}}")
    };
    let notification = |method: &str, params: &str| {
        format!("{{\"jsonrpc\":\"2.0\",\"method\":\"{method}\",\"params\":{params}}}")
    };
    send(
        &mut server,
        &request(
            1,
            "initialize",
            "{\"processId\": null, \"rootUri\": null, \"capabilities\": {}}",
        ),
    );
    loop {
        let reply = recv(&mut server);
        if reply.contains("\"id\":1") {
            break;
        }
    }
    send(&mut server, &notification("initialized", "{}"));
    let uri = format!("file:///{}", entry.to_string_lossy().replace('\\', "/"));
    send(
        &mut server,
        &notification(
            "textDocument/didOpen",
            &format!(
                "{{\"textDocument\": {{\"uri\": \"{uri}\", \"languageId\": \"dreamscript\", \
                 \"version\": 1, \"text\": {}}}}}",
                json_string(REPIN_ROOT_SOURCE)
            ),
        ),
    );
    // the open publishes the unresolved-import diagnostics; the reads
    // below skip them.
    // `bonus` in the import row sits at line 0 chars 19..24.
    let mut hover = |id: u64| {
        send(
            &mut server,
            &request(
                id,
                "textDocument/hover",
                &format!(
                    "{{\"textDocument\": {{\"uri\": \"{uri}\"}}, \"position\": {{\"line\": 0, \
                     \"character\": 21}}}}"
                ),
            ),
        );
        for _ in 0..8 {
            let reply = recv(&mut server);
            if reply.contains(&format!("\"id\":{id}")) {
                return reply;
            }
        }
        panic!("the hover reply never arrives");
    };
    let v1_hover = hover(2);
    assert!(
        v1_hover.contains("pub fn bonus() -> int"),
        "the pinned v1 surface answers: {v1_hover}"
    );

    // the repin, outside the server: the library's member changes, the
    // new identity replaces the manifest's pin, and a run caches the
    // new dep's .dsi beside its dsmod
    write_pkg(&lib, &lib_manifest(), &[("lib.ds", REPIN_LIB_V2)]);
    let v2 = bundle_identity(&lib_manifest(), &[("lib.ds", REPIN_LIB_V2)]);
    write_pkg(&app, &root_manifest(v2), &[("main.ds", REPIN_ROOT_SOURCE)]);
    let (ok, _, err) = ds(&cache, &["save", lib.to_str().expect("utf-8 path")]);
    assert!(ok, "the v2 library saves into the store (stderr: {err})");
    let (ok, out, err) = ds(&cache, &[
        "run",
        "--print-result",
        entry.to_str().expect("utf-8 path"),
    ]);
    assert!(ok, "the v2 run succeeds (stderr: {err})");
    assert_eq!(out, "42\n");

    // the same session: the changed manifest re-resolves the root, so
    // the hover serves the repinned content's line
    let v2_hover = hover(3);
    assert!(
        v2_hover.contains("pub fn bonus(scale: int) -> int"),
        "the repinned surface answers with no restart: {v2_hover}"
    );
    assert!(
        !v2_hover.contains("pub fn bonus() -> int"),
        "the stale pinned line never serves: {v2_hover}"
    );
    // the unchanged manifest after the repin stands on the memo: the
    // same repinned line answers again
    let again = hover(4);
    assert!(
        again.contains("pub fn bonus(scale: int) -> int"),
        "the memo hit serves the same line: {again}"
    );
    let _ = server.kill();
    let _ = server.wait();
    fs::remove_dir_all(app.parent().expect("the tree root")).expect("the tree removes");
    fs::remove_dir_all(&cache).expect("the cache removes");
}
