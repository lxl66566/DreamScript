//! The example programs through the `ds` binary: every committed example
//! runs end to end in source mode, the build round trip reproduces the
//! source run's output byte for byte, the written module records the
//! source hash [`ds_mod::read_checked`] verifies, and the net example's
//! optional-argument guard skips with instructions and exit 0 when no
//! port stages — with one staged, an in-process `TcpListener` answers
//! the flow.

use std::{
    env, fs,
    io::{Read, Write},
    net::TcpListener,
    path::PathBuf,
    process::Command,
    thread,
};

fn ds() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ds"));
    command.current_dir(env!("CARGO_MANIFEST_DIR"));
    command
}

fn example(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples")
        .join(name)
}

fn run_ok(command: &mut Command) -> String {
    let out = command.output().expect("the binary runs");
    assert!(out.status.success(), "the run failed: {out:?}");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn hello_prints_the_greeting() {
    assert_eq!(
        run_ok(ds().arg("run").arg(example("hello.ds"))),
        "hello, DreamScript\n1 + 2 = 3\n0\n"
    );
}

#[test]
fn fib_computes_and_runs_the_tail_variant_a_million_deep() {
    // the 10^6-deep tail run completes at all — on a non-tail frame
    // per call the stack would grow past the VM's limit
    assert_eq!(
        run_ok(ds().arg("run").arg(example("fib.ds"))),
        "fib(10) = 55\nfib_tail(90, 0, 1) = 2880067194370816120\nfib_tail(1000000, 0, 0) = 0\n0\n"
    );
}

#[test]
fn strings_print_every_conversion() {
    assert_eq!(
        run_ok(ds().arg("run").arg(example("strings.ds"))),
        "int: 42\nfloat: 2.5\nbool: true\ngreeting: hello, DreamScript\n0\n"
    );
}

#[test]
fn files_round_trip_real_bytes() {
    // the example's fixed path is relative to the current directory, so
    // the run lands it in a temp dir and the test reads the real bytes
    let dir = env::temp_dir().join("ds_cli_files_example");
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("the temp dir creates");
    let out = ds()
        .arg("run")
        .arg(example("files.ds"))
        .current_dir(&dir)
        .output()
        .expect("the binary runs");
    assert!(out.status.success(), "the run failed: {out:?}");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "read back: files round trip\nsize: 16\nexists: true\nround trip ok\n0\n"
    );
    assert_eq!(
        fs::read_to_string(dir.join("ds_files_example.txt")).expect("the bytes are there"),
        "files round trip"
    );
}

#[test]
fn net_skips_with_instructions_without_a_staged_port() {
    let out = ds()
        .arg("run")
        .arg(example("net.ds"))
        .output()
        .expect("the binary runs");
    assert!(out.status.success(), "the no-server guard exits 0: {out:?}");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "skipped: no local test server\n  start a TCP listener, then run: ds run examples/net.ds \
         --args <port>\n0\n"
    );
}

#[test]
fn net_round_trips_against_a_local_listener() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("loopback binds");
    let port = listener
        .local_addr()
        .expect("an address")
        .port()
        .to_string();
    let server = thread::spawn(move || {
        let (mut sock, _) = listener.accept().expect("the example connects");
        let mut buf = [0u8; 64];
        let n = sock.read(&mut buf).expect("the ping arrives");
        let mut reply = b"echo: ".to_vec();
        reply.extend_from_slice(&buf[..n]);
        // one write: the example's net_recv reads once
        sock.write_all(&reply).expect("the echo writes");
    });
    let out = ds()
        .arg("run")
        .arg(example("net.ds"))
        .arg("--args")
        .arg(&port)
        .output()
        .expect("the binary runs");
    server.join().expect("the server thread finishes");
    assert!(out.status.success(), "the net flow failed: {out:?}");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "server replied: echo: ping from DreamScript\n0\n"
    );
}

#[test]
fn build_then_run_the_module_reproduces_the_source_output() {
    let dir = env::temp_dir().join("ds_cli_build_example");
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("the temp dir creates");
    let module = dir.join("hello.dsmod");
    let out = ds()
        .arg("build")
        .arg(example("hello.ds"))
        .arg("-o")
        .arg(&module)
        .output()
        .expect("the binary runs");
    assert!(out.status.success(), "the build failed: {out:?}");
    assert!(out.stdout.is_empty(), "build stays quiet on success");
    assert_eq!(
        run_ok(ds().arg("run").arg(&module)),
        "hello, DreamScript\n1 + 2 = 3\n0\n",
        "the module path and the source path print identically"
    );
    // the written header carries the source's blake3: read_checked, the
    // verification the build pipeline records against, passes
    let bytes = fs::read(&module).expect("the module lands");
    let source = fs::read(example("hello.ds")).expect("the source reads back");
    ds_mod::read_checked(&bytes, &source).expect("the recorded hash matches the source");
}
