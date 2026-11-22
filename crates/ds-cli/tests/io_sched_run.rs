//! The IO scheduler through `ds run` (the 2026-11-21 scheduler
//! rulings): the CLI installs the platform's reactor, so a source's
//! net face parks on a would-block instead of blocking the drive —
//! the single-thread executor waits on the reactor, the peer's write
//! wakes the parked recv, and the completed answer flows through the
//! program's own match. Hermetic: a loopback listener, a peer thread
//! writing late, no external network.

use std::{
    env, fs, io::Write, net::TcpListener, path::PathBuf, process::Command, thread, time::Duration,
};

fn ds() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ds"));
    command.current_dir(env!("CARGO_MANIFEST_DIR"));
    command.env("DS_CACHE", test_cache());
    command
}

/// A private per-process script-cache root: spawned runs never read or
/// write a real user cache directory.
fn test_cache() -> PathBuf {
    static CACHE: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    CACHE
        .get_or_init(|| {
            let dir = env::temp_dir().join(format!("ds-cli-io-sched-cache-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            dir
        })
        .clone()
}

/// The parked-recv source: connect, recv (a park under the reactor),
/// the answer extracted through `.err`'s fallback — a wrong wake or a
/// lost park prints the fallback, so the assertion names the wake.
fn source(port: u16) -> String {
    format!(
        r#"fn main() -> int {{
  const h = net_connect("127.0.0.1", {port})
  const back = recv(h, 256).err((_: net_err) => "no wake")
  println(back)
  0
}}
"#
    )
}

/// The peer writes after the run's recv has parked (the listener's
/// backlog holds the connection until the accept below), so the only
/// path to the printed answer runs through the reactor's wake.
#[test]
fn a_scheduled_run_parks_its_recv_and_lands_the_answer() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("loopback bind");
    let port = listener.local_addr().expect("local address").port();
    let writer = thread::spawn(move || {
        thread::sleep(Duration::from_millis(200));
        let (mut sock, _) = listener.accept().expect("the run's connection");
        sock.write_all(b"from the peer").expect("peer writes");
        sock.flush().expect("peer flushes");
    });
    let dir = env::temp_dir().join(format!("ds-cli-io-sched-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("temp dir");
    let file = dir.join("parked_recv.ds");
    fs::write(&file, source(port)).expect("source writes");
    let out = ds().arg("run").arg(&file).output().expect("ds run spawns");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "status {:?} stdout {stdout} stderr {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    // the run prints the entry's answer beside the program's own line
    assert!(
        stdout.lines().any(|line| line == "from the peer"),
        "stdout {stdout:?}"
    );
    writer.join().expect("writer lands");
    let _ = fs::remove_dir_all(&dir);
}
