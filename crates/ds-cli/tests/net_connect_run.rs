//! The net face's connection half through `ds run` (the parked-connect
//! round): the CLI installs the platform's reactor, so a source's
//! `net_connect` and plain `http_get` ride the parked machines — the
//! raw socket's non-blocking connect and the staged exchange — instead
//! of blocking the drive; the `net_listener` face's `local_addr` op
//! reports a port-0 bind's live address. Hermetic: loopback ports,
//! peers on threads, no external network; a connect or bind racing the
//! parallel suite's port steals re-picks its port.

use std::{
    env, fs,
    io::{BufRead as _, Read, Write},
    net::{TcpListener, TcpStream},
    path::PathBuf,
    process::Command,
    thread,
    time::Duration,
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
            let dir =
                env::temp_dir().join(format!("ds-cli-net-connect-cache-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            dir
        })
        .clone()
}

/// The net_err match arms every fallible row of the sources below
/// spells (a bare fallible call cannot stand as a block's own answer
/// under the M3 contract).
const ERR_ARMS: &str = "    timeout => println(\"timeout\")\n    refused => \
                        println(\"refused\")\n    addr_in_use => println(\"addr in use\")\n    \
                        reset => println(\"reset\")\n    net_other { msg } => println(\"other\")\n";

/// A port with nothing bound on it: bind, note, drop.
fn free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("loopback bind");
    listener.local_addr().expect("bound address").port()
}

/// A scheduled run's echo client: connect to the pre-bound peer, send a
/// line, answer the reply.
#[test]
fn a_scheduled_run_parks_its_connect_and_speaks_with_the_peer() {
    let dir = env::temp_dir().join(format!("ds-cli-net-connect-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("temp dir");
    let file = dir.join("echo_client.ds");
    for _ in 0..8 {
        let port = free_port();
        let listener = TcpListener::bind(("127.0.0.1", port)).expect("the peer rebinds");
        // the peer serves one connection: read the line it expects,
        // echo it back
        let peer = thread::spawn(move || {
            let (mut sock, _) = listener.accept().expect("the peer accepts");
            sock.set_read_timeout(Some(Duration::from_secs(30)))
                .expect("timeout");
            let mut buf = [0; 4];
            let mut got = 0;
            while got < buf.len() {
                match sock.read(&mut buf[got..]) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => got += n,
                }
            }
            sock.write_all(&buf[..got]).expect("the peer echoes");
            buf[..got].to_vec()
        });
        let arms = ERR_ARMS;
        fs::write(
            &file,
            format!(
                "fn main() -> int {{\n  const c = net_connect(\"127.0.0.1\", {port})\n  send(c, \
                 \"ping\")\n  const r = recv(c, 64)\n  match r {{\n{arms}\n    text => \
                 println(\"got: ${{text}}\")\n  }}\n  0\n}}\n",
            ),
        )
        .expect("source writes");
        let out = ds().arg("run").arg(&file).output().expect("ds run spawns");
        let stdout = String::from_utf8_lossy(&out.stdout).to_string();
        // a stolen port answers the connect's refusal before the peer
        // serves: re-pick and retry
        if stdout.contains(&format!("net_connect 127.0.0.1:{port}:")) {
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
            stdout.lines().any(|line| line == "got: ping"),
            "stdout {stdout:?}"
        );
        assert_eq!(
            peer.join().expect("peer lands"),
            b"ping",
            "the parked connection's send reached the peer"
        );
        let _ = fs::remove_dir_all(&dir);
        return;
    }
    panic!("no free loopback port answered the connect");
}

/// A parked connect against a dead port answers the refused class on
/// the total spelling's own halt path — the machine's wake, not the
/// deadline, drawing the line.
#[test]
fn a_parked_connect_to_a_dead_port_answers_the_refused_class() {
    let dir = env::temp_dir().join(format!("ds-cli-net-refused-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("temp dir");
    let file = dir.join("refused.ds");
    for _ in 0..8 {
        // bind, note, drop: the port answers connects with a reset
        let held = TcpListener::bind("127.0.0.1:0").expect("the held probe");
        let port = held.local_addr().expect("bound address").port();
        drop(held);
        fs::write(
            &file,
            format!(
                "fn main() -> int {{\n  const c = net_connect(\"127.0.0.1\", {port})\n  send(c, \
                 \"late\")\n  0\n}}\n"
            ),
        )
        .expect("source writes");
        let out = ds().arg("run").arg(&file).output().expect("ds run spawns");
        let stdout = String::from_utf8_lossy(&out.stdout).to_string();
        if stdout.contains(&format!("net_connect 127.0.0.1:{port}:"))
            && stdout.contains("net error:")
        {
            assert!(
                !out.status.success(),
                "the refused connect halts nonzero: {stdout:?}"
            );
            let _ = fs::remove_dir_all(&dir);
            return;
        }
        // a parallel suite member re-bound the port mid-flight: the
        // connect landed, so re-pick
    }
    panic!("no dead loopback port answered the refused class");
}

/// A scheduled plain fetch rides the parked exchange machine: the
/// staged connect, the request drain, and the response reads to the
/// local server's close — the body on stdout.
#[test]
fn a_scheduled_run_parks_its_plain_fetch() {
    let dir = env::temp_dir().join(format!("ds-cli-net-fetch-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("temp dir");
    let file = dir.join("fetch.ds");
    for _ in 0..8 {
        let port = free_port();
        let listener = TcpListener::bind(("127.0.0.1", port)).expect("the server rebinds");
        // the one-shot HTTP server: read the request to its blank line,
        // answer close-delimited (the drop the close the machine reads)
        let server = thread::spawn(move || {
            let (mut sock, _) = listener.accept().expect("the server accepts");
            sock.set_read_timeout(Some(Duration::from_secs(30)))
                .expect("timeout");
            let mut seen = vec![0; 1024];
            let mut got = 0;
            while got < seen.len() {
                match sock.read(&mut seen[got..]) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        got += n;
                        if seen[..got].windows(4).any(|w| w == b"\r\n\r\n") {
                            break;
                        }
                    },
                }
            }
            sock.write_all(
                b"HTTP/1.0 200 OK\r\nContent-Type: text/plain\r\n\r\nhello from the parked fetch",
            )
            .expect("the server writes");
        });
        fs::write(
            &file,
            format!(
                "fn main() -> int {{\n  const body = \
                 http_get(\"http://127.0.0.1:{port}/hello\")\n  println(body)\n  0\n}}\n"
            ),
        )
        .expect("source writes");
        let out = ds().arg("run").arg(&file).output().expect("ds run spawns");
        let stdout = String::from_utf8_lossy(&out.stdout).to_string();
        if stdout.contains(&format!("http_get http://127.0.0.1:{port}")) {
            server.join().expect("server lands");
            continue;
        }
        assert!(
            out.status.success(),
            "status {:?} stdout {stdout} stderr {}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            stdout
                .lines()
                .any(|line| line == "hello from the parked fetch"),
            "stdout {stdout:?}"
        );
        server.join().expect("server lands");
        let _ = fs::remove_dir_all(&dir);
        return;
    }
    panic!("no free loopback port served the fetch");
}

/// The port-0 bind's report: `local_addr` answers the bound address on
/// stdout, and the address is live — the test dials the reported port
/// and the program's parked accept serves it.
#[test]
fn a_port_zero_bind_reports_its_live_local_address() {
    let dir = env::temp_dir().join(format!("ds-cli-net-local-addr-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("temp dir");
    let file = dir.join("port_zero.ds");
    let arms = ERR_ARMS;
    fs::write(
        &file,
        format!(
            "fn main() -> int {{\n  const l = net_listen(\"127.0.0.1\", 0)\n  \
             println(local_addr(l))\n  const c = accept(l)\n  const r = recv(c, 64)\n  match r \
             {{\n{arms}\n    text => {{\n      send(c, \"echo: ${{text}}\")\n      \
             println(\"server got: ${{text}}\")\n    }}\n  }}\n  0\n}}\n",
        ),
    )
    .expect("source writes");
    // the run parks on its accept after printing the address, so the
    // address streams off the child's stdout (println's newline the
    // line-buffered flush point) and the dial happens mid-run
    let mut child = ds()
        .arg("run")
        .arg(&file)
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("ds run spawns");
    let mut stdout_reader = std::io::BufReader::new(child.stdout.take().expect("piped stdout"));
    let mut reported = String::new();
    stdout_reader
        .read_line(&mut reported)
        .expect("the address line streams");
    let reported = reported.trim().to_owned();
    let port: u16 = reported
        .rsplit(':')
        .next()
        .unwrap_or_default()
        .parse()
        .unwrap_or_else(|_| panic!("the reported address renders host:port: {reported:?}"));
    assert!(
        port != 0,
        "a port-0 bind reports its bound port: {reported}"
    );
    let mut sock = TcpStream::connect(("127.0.0.1", port)).expect("the reported port dials");
    let mut echoed = vec![0; "echo: hi".len()];
    sock.write_all(b"hi").expect("the dialer sends");
    sock.set_read_timeout(Some(Duration::from_secs(30)))
        .expect("timeout");
    let mut got = 0;
    while got < echoed.len() {
        match sock.read(&mut echoed[got..]) {
            Ok(0) | Err(_) => break,
            Ok(n) => got += n,
        }
    }
    echoed.truncate(got);
    assert_eq!(echoed, b"echo: hi", "the reported address served the dial");
    // the rest of the run's own words (the receipt line) and a clean exit
    let mut rest = String::new();
    stdout_reader
        .read_to_string(&mut rest)
        .expect("the rest streams");
    let out = child.wait_with_output().expect("the run lands");
    assert!(
        out.status.success(),
        "status {:?} rest {rest:?} stderr {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        rest.lines().any(|line| line == "server got: hi"),
        "the rest of stdout {rest:?}"
    );
    let _ = fs::remove_dir_all(&dir);
}
