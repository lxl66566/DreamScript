//! The net face's server half through `ds run` (the server surface
//! round): the CLI installs the platform's reactor, so a source's
//! accept parks on the bound listener instead of blocking the drive —
//! the single-thread executor waits on the reactor, the peer's late
//! connection wakes the parked accept, and the accepted connection
//! owns the `net_handle` face in full (send/recv over it, the closing
//! deinit at scope exit — no explicit close anywhere, the exit still
//! clean). Hermetic: a loopback port, a peer thread dialing late, no
//! external network; the port re-picks on an addr-in-use bind the
//! parallel suite can steal.

use std::{
    env, fs,
    io::{Read, Write},
    net::TcpStream,
    path::PathBuf,
    process::Command,
    thread,
    time::Duration,
};

mod support;

fn ds() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ds"));
    command.current_dir(env!("CARGO_MANIFEST_DIR"));
    command.env("DS_CACHE", test_cache());
    command
}

/// A private per-thread script-cache root: spawned runs never read or
/// write a real user cache directory, and the thread's exit removes it.
fn test_cache() -> PathBuf {
    support::shared_dir("ds-cli-net-server-cache-")
}

/// The echo-server source: bind, accept (a park under the reactor), echo
/// the peer's line back, answer the receipt line. No explicit close:
/// the two deinits fire at `main`'s scope exit, a clean exit their pin.
/// The fallible recv's match arms each answer the unit a statement
/// match needs (a bare fallible call cannot stand as a block's own
/// answer (a bare fallible call cannot stand as a block's own answer).
fn source(port: u16) -> String {
    format!(
        r#"fn main() -> int {{
  const l = net_listen("127.0.0.1", {port})
  const c = accept(l)
  const r = recv(c, 64)
  match r {{
    timeout => println("timeout")
    refused => println("refused")
    addr_in_use => println("addr in use")
    reset => println("reset")
    net_other {{ msg }} => println("other")
    text => {{
      send(c, "echo: ${{text}}")
      println("server got: ${{text}}")
    }}
  }}
  0
}}
"#
    )
}

/// A port with nothing bound on it: bind, note, drop.
fn free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("loopback bind");
    listener.local_addr().expect("bound address").port()
}

/// The peer dials late (the program's accept parks first), sends its
/// line, and answers what the server echoed back.
fn late_peer(port: u16) -> thread::JoinHandle<Vec<u8>> {
    thread::spawn(move || {
        thread::sleep(Duration::from_millis(250));
        for _ in 0..200 {
            if let Ok(mut sock) = TcpStream::connect(("127.0.0.1", port)) {
                sock.write_all(b"ping").expect("peer sends");
                let mut echoed = vec![0; 10];
                let mut got = 0;
                while got < echoed.len() {
                    match sock.read(&mut echoed[got..]) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => got += n,
                    }
                }
                echoed.truncate(got);
                return echoed;
            }
            thread::sleep(Duration::from_millis(25));
        }
        Vec::new()
    })
}

/// The failed bind's printing halt: the total spelling pairs with
/// the fallible native on the shared halt path, so a held port answers
/// the net error line and a nonzero status (the `net_connect`
/// precedent, the accepted-side twin pinned by the echo round above).
#[test]
fn a_failed_bind_halts_on_the_total_spellings_error_line() {
    let held = std::net::TcpListener::bind("127.0.0.1:0").expect("the held probe");
    let port = held.local_addr().expect("bound address").port();
    let dir = support::guarded_dir("ds-cli-net-bind-");
    let file = dir.join("bind.ds");
    fs::write(
        &file,
        format!("fn main() -> int {{\n  const l = net_listen(\"127.0.0.1\", {port})\n  0\n}}\n"),
    )
    .expect("source writes");
    let out = ds().arg("run").arg(&file).output().expect("ds run spawns");
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        !out.status.success(),
        "a failed bind never reads as clean success: status {:?}",
        out.status.code()
    );
    // the total spelling's failure halts on the net error line (the
    // halt note riding stderr alone)
    assert!(
        stdout.contains(&format!("net error: net_listen 127.0.0.1:{port}:")),
        "stdout {stdout:?}"
    );
    let _ = fs::remove_dir_all(&dir);
}

/// The unprovisioned upgrade's printing halt: the CLI host pins no
/// server identity (a server certificate is the host's own to pin),
/// so `accept_tls` answers its own refusal and the total spelling
/// halts on the net error line exactly the failed bind's way — the
/// raw span's message the fixture round pins.
#[test]
fn an_unprovisioned_accept_tls_halts_on_the_net_error_line() {
    let dir = support::guarded_dir("ds-cli-net-accept-tls-");
    fs::create_dir_all(&dir).expect("temp dir");
    let file = dir.join("accept_tls.ds");
    fs::write(
        &file,
        "fn main() -> int {\n  const l = net_listen(\"127.0.0.1\", 0)\n  const c = \
         accept_tls(l)\n  0\n}\n",
    )
    .expect("source writes");
    let out = ds().arg("run").arg(&file).output().expect("ds run spawns");
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        !out.status.success(),
        "the refused upgrade never reads as clean success: status {:?}",
        out.status.code()
    );
    assert!(
        stdout.contains(
            "net error: net_accept_tls 0: tls refused: the host installed no server config"
        ),
        "the total spelling's failure halts on the net error line: stdout {stdout:?}"
    );
    let _ = fs::remove_dir_all(&dir);
}

/// The failed accept's printing halt: the raw close seam (the
/// deinit's own idempotent twin) closes the bound listener before the
/// accept reads it, so the accept answers the unknown-listener floor
/// and the total spelling halts on the net error line exactly the
/// failed bind's way (a non-halting accept stays the
/// optional-over-linear union seam, no accept-specific patch).
#[test]
fn a_failed_accept_halts_on_the_total_spellings_error_line() {
    let dir = support::guarded_dir("ds-cli-net-accept-");
    fs::create_dir_all(&dir).expect("temp dir");
    let file = dir.join("accept.ds");
    fs::write(
        &file,
        "fn main() -> int {\n  const l = net_listen(\"127.0.0.1\", 0)\n  \
         net_listener_close(l.lid)\n  const c = accept(l)\n  0\n}\n",
    )
    .expect("source writes");
    let out = ds().arg("run").arg(&file).output().expect("ds run spawns");
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        !out.status.success(),
        "the failed accept never reads as clean success: status {:?}",
        out.status.code()
    );
    assert!(
        stdout.contains("net error: net_accept 0: unknown listener 0"),
        "the total spelling's failure halts on the net error line: stdout {stdout:?}"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_scheduled_run_parks_its_accept_and_serves_the_peer() {
    let dir = support::guarded_dir("ds-cli-net-server-");
    fs::create_dir_all(&dir).expect("temp dir");
    let file = dir.join("echo_server.ds");
    for _ in 0..8 {
        let port = free_port();
        let peer = late_peer(port);
        fs::write(&file, source(port)).expect("source writes");
        let out = ds().arg("run").arg(&file).output().expect("ds run spawns");
        let stdout = String::from_utf8_lossy(&out.stdout).to_string();
        // a stolen port halts the total spelling on its own error line
        // before the peer dials: re-pick and retry
        if stdout.contains(&format!("net_listen 127.0.0.1:{port}:")) {
            peer.join().expect("peer lands");
            continue;
        }
        assert!(
            out.status.success(),
            "status {:?} stdout {stdout} stderr {}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            stdout.lines().any(|line| line == "server got: ping"),
            "stdout {stdout:?}"
        );
        assert_eq!(
            peer.join().expect("peer lands"),
            b"echo: ping",
            "the accepted connection's send reaches the peer"
        );
        let _ = fs::remove_dir_all(&dir);
        return;
    }
    panic!("no free loopback port answered the bind");
}
