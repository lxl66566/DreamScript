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
    command.env("DS_CACHE", test_cache());
    command
}

/// A private per-process script-cache root: spawned runs never read or
/// write a real user cache directory.
fn test_cache() -> PathBuf {
    static CACHE: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    CACHE
        .get_or_init(|| {
            let dir = env::temp_dir().join(format!("ds-cli-test-cache-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            dir
        })
        .clone()
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
fn lists_print_the_literal_and_constructor_surface() {
    assert_eq!(
        run_ok(ds().arg("run").arg(example("lists.ds"))),
        "ints 6 0\nwords 3 alpha\n0\n"
    );
}

#[test]
fn containers_compose_the_generic_closes() {
    // the six container closes dispatch through CALLG: cons builds, tail
    // drops the first, collect copies, head/first answer the checked
    // first element (the empty list the none member), and max orders
    // both numeric instances
    assert_eq!(
        run_ok(ds().arg("run").arg(example("containers.ds"))),
        "cons 2 1 1 2\nempty -1 -1\nmax 9 2.5\n0\n"
    );
}

#[test]
fn combinators_run_their_lambdas_reentrantly() {
    // the lambda-taking combinators dispatch through CALLG and reenter
    // the dispatcher per element: map and filter answer fresh lists,
    // reduce folds (an empty list would answer its init), partition
    // splits into its two-list total at the call site, and the
    // block-body println interleaves in element order
    assert_eq!(
        run_ok(ds().arg("run").arg(example("combinators.ds"))),
        "doubled 5 2 10
evens 2 2 4
visiting 1
visiting 2
visiting 3
visiting 4
visiting 5
traced 10 50
sum 15 joined alphabeta
split 3 2
0
"
    );
}

#[test]
fn par_map_forks_one_child_per_element_and_joins_in_input_order() {
    // E3's fork-join slice: each element runs as its own spawned child
    // under a fresh nursery, the boundary join assembles the answers in
    // input order (spawn order is the drive order), and the sequential
    // reduce folds them after
    assert_eq!(
        run_ok(ds().arg("run").arg(example("par_map.ds"))),
        "doubled 5 2 10\nsum 30\n0\n"
    );
}

#[test]
fn par_reduce_chains_one_child_per_element_in_input_order() {
    // E3's fork-join fold: the non-commutative order witness (a string
    // chain through the native concat), the empty-input init answer,
    // and the struct-typed fold chaining its whole word span
    assert_eq!(
        run_ok(ds().arg("run").arg(example("par_reduce.ds"))),
        "sum 15\njoined alphabetagamma\nempty 12000\npair 123 6\n0\n"
    );
}

#[test]
fn callables_pass_named_fns_closures_and_collections() {
    // the callable words end to end: a named fn value and a capturing
    // closure arrive through the fn-typed parameter (the callable-
    // defaulted slot), the trailing block stages its closure like the
    // written lambda, the factory's fn-typed return builds an add-k
    // closure, the fn collection stores the scalar callable words
    // (named fns and non-capturing lambdas — closures and unproven
    // callable parameters stay refused at the element staging), the
    // read-back `.ok` handler calls its element through a fn-typed
    // parameter, and the parse extracts through `?` into arithmetic
    // beside the `.err` fallback
    assert_eq!(
        run_ok(ds().arg("run").arg(example("callables.ds"))),
        "named 5 3\nclosure 40\nblock 42\nfactory 42\nfns 2 42\nparsed port 68\nfallback 8\n0\n"
    );
}

#[test]
fn tuples_destructure_in_bindings_and_match_arms() {
    assert_eq!(
        run_ok(ds().arg("run").arg(example("tuples.ds"))),
        "host:8080\nport 5432\nmatched host 8080\nlabel unit\nseven 7\nequal\nvar db:8081\npick \
         zero-five big zero flat\n0\n"
    );
}

#[test]
fn patterns_test_bind_and_guard_every_match_shape() {
    // the match-pattern surfaces: a struct pattern tests its literal
    // fields in place and binds the rest, an enum case arm tests and
    // binds its payload, a nested struct sub-pattern over a
    // struct-typed payload tests and binds its fields the same way, a
    // guard on a union arm slides its failure to the next arm's
    // dispatch entry (scored(9) lands in the binding arm, scored(2)
    // never reaches the guard), and a tuple member of a union
    // destructures positionally inside its arm's body
    assert_eq!(
        run_ok(ds().arg("run").arg(example("patterns.ds"))),
        "at 2 7\naxis 4\nunit\none\nbig 9\nn 2\nplain\npair 7 0\naxis 5\nat 2 3\nnone\n0\n"
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
fn mutation_runs_the_fallible_parse_extraction() {
    // the ambient's success-typed parse_int answers its failure union at
    // run level (the two-word platform span re-spans into the call's
    // union): the `.err` fallback extracts the port and it flows into
    // arithmetic
    assert_eq!(
        run_ok(ds().arg("run").arg(example("mutation.ds"))),
        "total: 10\nname: DREAMSCRIPT\nfirst: a\nport + total: 8090\njoined: abc\nslot: 7\n0\n"
    );
}

#[test]
fn files_round_trip_runs_through_the_success_typed_read() {
    // the round trip's read leg rides the ambient's string | io_err
    // signature: the `.err` handler extracts at run level, and the
    // written file landing before the read proves the legs ordered
    let dir = env::temp_dir().join("ds_cli_files_example");
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("the temp dir creates");
    let mut command = ds();
    command
        .arg("run")
        .arg(example("files.ds"))
        .current_dir(&dir);
    assert_eq!(
        run_ok(&mut command),
        "read back: files round trip\nsize: 16\nexists: true\nround trip ok\n0\n"
    );
    assert!(
        dir.join("ds_files_example.txt").exists(),
        "the write leg landed the file before the read"
    );
    let _ = fs::remove_dir_all(&dir);
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
fn nursery_joins_cancels_and_unions_child_errors() {
    // the tutorial's concurrency block: fetch_all fans out over `spawn`
    // inside a `nursery`, the clean pass joins in spawn order over the
    // shared list, and the failing pass cancels the in-flight sibling at
    // its next backward branch with the child error unioning out at the
    // boundary into the match
    assert_eq!(
        run_ok(ds().arg("run").arg(example("nursery.ds"))),
        "kept 2 bodies\ncaught refused\n0\n"
    );
}

#[test]
fn script_entry_runs_top_level_statements() {
    // The entry file's top-level statements fold into the synthesized
    // `main` while the declarations beside them stay pure-declaration;
    // the early return ends the run before the final line.
    assert_eq!(
        run_ok(ds().arg("run").arg(example("script_entry.ds"))),
        "total: 3\nsize: small\nbumped: 41\n0\n"
    );
}

#[test]
fn schema_builds_its_table_at_comptime() {
    // The comptime witness (G1, G2): schema(page) and cases(tone)
    // evaluate on the fenced VM at compile time over the declared
    // fields and enum cases, the list of field names embeds through the
    // round-two list surface, and the banner's scalar-literal arguments
    // stage beside the type argument — every printed line is a constant
    // by the time main runs.
    assert_eq!(
        run_ok(ds().arg("run").arg(example("schema.ds"))),
        "{title: string, views: int, tags: list[string], }\n{quiet: none/quiet, loud: loud/loud, \
         }\ntitle, views, tags\nfields: 3\nschema ======\n0\n"
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
