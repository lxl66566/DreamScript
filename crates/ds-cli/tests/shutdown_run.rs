//! The CLI host's signal shutdown end to end (the 2026-11-24 signal
//! rulings), over the real `ds` binary as a child process — the one
//! process that may own its signal mask. The parked server accepts
//! with no bound (the signal its backstop), the host's signalfd door
//! observes the signal, the drive cuts the parked accept at its
//! resume-shaped point with the frame's deinit hooks fired, and the
//! host answers 128+signum per the die split: SIGINT 130, SIGTERM
//! 143. A bounded accept would have answered the configured timeout
//! span long before the signal — the late peer test pins the
//! unbounded park through the real host's default bound (3s) instead.
//! Hermetic: loopback port-0 binds, no external network.

use std::{
    env, fs,
    io::{BufRead, BufReader},
    net::{TcpListener, TcpStream},
    path::PathBuf,
    process::{Command, Stdio},
    thread,
    time::Duration,
};

mod support;

#[cfg(target_os = "linux")]
use std::process::ExitStatus;

fn ds() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ds"));
    command.current_dir(env!("CARGO_MANIFEST_DIR"));
    command.env("DS_CACHE", test_cache());
    command
}

/// A private per-thread script-cache root: spawned runs never read or
/// write a real user cache directory, and the thread's exit removes it.
fn test_cache() -> PathBuf {
    support::shared_dir("ds-cli-shutdown-cache-")
}

unsafe extern "C" {
    fn kill(pid: i32, sig: i32) -> i32;
}

const SIGINT: i32 = 2;
const SIGTERM: i32 = 15;

/// The serving source: bind on a port-0 pick, announce the bound state,
/// then park the accept (no bound — the signal its backstop) with a
/// deinit-carrying holder live across the park, so the cut's deinit run
/// is observable on stdout.
fn source() -> String {
    "struct holder: ~Copyable {\n\x20 id: int\n\x20 deinit { println(\"freed ${self.id}\") \
     }\n}\nfn main() -> int {\n\x20 const h = holder { id: 7 }\n\x20 const l = \
     net_listen(\"127.0.0.1\", 0)\n\x20 println(\"serving\")\n\x20 const c = accept(l)\n\x20 \
     println(\"accepted\")\n\x20 0\n}\n"
        .to_owned()
}

/// Spawns the serving source, waits for its `serving` line (the parked
/// accept stands behind it), delivers `signal` to the process, and
/// answers (the exit status, the collected stdout).
fn run_then_signal(signal: i32) -> (ExitStatus, String) {
    let dir = support::guarded_dir("ds-cli-shutdown-");
    fs::create_dir_all(&dir).expect("temp dir");
    let file = dir.join("serving.ds");
    fs::write(&file, source()).expect("source writes");
    let mut child = ds()
        .arg("run")
        .arg(&file)
        .stdout(Stdio::piped())
        .spawn()
        .expect("ds run spawns");
    let mut stdout = String::new();
    {
        let reader = child.stdout.take().expect("piped stdout");
        let mut lines = BufReader::new(reader);
        let mut line = String::new();
        lines.read_line(&mut line).expect("the serving line reads");
        stdout.push_str(&line);
        assert!(
            line.trim() == "serving",
            "the server announced its bound state first: {line:?}"
        );
        let pid = child.id();
        // the signal rides the host's signalfd door; the drive cuts the
        // parked accept and the process answers its own exit code
        // SAFETY: the pid belongs to the spawned child alone
        let _ = unsafe { kill(pid.cast_signed(), signal) };
        // the remaining lines (the cut's deinit run) collect at exit
        for line in lines.lines() {
            match line {
                Ok(line) => {
                    stdout.push_str(&line);
                    stdout.push('\n');
                },
                Err(_) => break,
            }
        }
    }
    let status = child.wait().expect("the child exits");
    (status, stdout)
}

/// SIGINT cuts the parked accept: the frame's deinit runs, the host
/// answers 130 (128+signum) — a signal-killed process would carry no
/// code at all, so the status itself separates the cut from the
/// default disposition.
#[test]
#[cfg(target_os = "linux")]
fn a_sigint_cuts_the_parked_accept_and_answers_130() {
    let (status, stdout) = run_then_signal(SIGINT);
    assert_eq!(status.code(), Some(130), "stdout {stdout:?}");
    assert!(
        stdout.contains("freed 7"),
        "the cut fired the kept frame's deinit: {stdout:?}"
    );
    assert!(
        !stdout.contains("accepted"),
        "the cut never ran the parked call's continuation: {stdout:?}"
    );
}

/// SIGTERM rides the same door and answers 143 — the translation is
/// the signal set's, not one signal's own.
#[test]
#[cfg(target_os = "linux")]
fn a_sigterm_cuts_the_parked_accept_and_answers_143() {
    let (status, stdout) = run_then_signal(SIGTERM);
    assert_eq!(status.code(), Some(143), "stdout {stdout:?}");
    assert!(
        stdout.contains("freed 7"),
        "the cut fired the kept frame's deinit: {stdout:?}"
    );
}

/// The quiet accept holds past the host's default bound (3s) for a late
/// peer: under the old bound the accept would have answered its
/// timeout span and halted on the net error line before the peer ever
/// dialed.
#[test]
#[cfg(target_os = "linux")]
fn the_parked_accept_holds_past_the_default_bound_for_a_late_peer() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("the probe's port pick");
    let port = listener.local_addr().expect("bound address").port();
    drop(listener);
    let dir = support::guarded_dir("ds-cli-shutdown-late-");
    fs::create_dir_all(&dir).expect("temp dir");
    let file = dir.join("late_peer.ds");
    fs::write(
        &file,
        format!(
            "fn main() -> int {{\n\x20 const l = net_listen(\"127.0.0.1\", {port})\n\x20 const c \
             = accept(l)\n\x20 const r = recv(c, 64)\n\x20 match r {{\n\x20   timeout => \
             println(\"timeout\")\n\x20   refused => println(\"refused\")\n\x20   addr_in_use => \
             println(\"addr in use\")\n\x20   reset => println(\"reset\")\n\x20   net_other {{ \
             msg }} => println(\"other\")\n\x20   text => println(\"server got: \
             ${{text}}\")\n\x20 }}\n\x20 0\n}}\n"
        ),
    )
    .expect("source writes");
    // 4s past the 3s default: the dial lands only if the park held
    let peer = thread::spawn(move || {
        use std::io::Write;
        thread::sleep(Duration::from_secs(4));
        let mut sock = TcpStream::connect(("127.0.0.1", port)).expect("the late peer dials");
        sock.write_all(b"ping").expect("the late peer writes");
    });
    let out = ds().arg("run").arg(&file).output().expect("ds run spawns");
    peer.join().expect("the late peer lands");
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        out.status.success(),
        "status {:?} stdout {stdout} stderr {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        stdout.contains("server got: ping"),
        "the parked accept held past the default bound: {stdout:?}"
    );
}
