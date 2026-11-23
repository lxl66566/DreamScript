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
    support::shared_dir("ds-cli-test-cache-")
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
    // the container closes dispatch through CALLG: cons builds, tail
    // drops the first, collect copies, first answers the checked
    // first element (the empty list the none member), and the @std
    // ordering pair answers over its int members
    assert_eq!(
        run_ok(ds().arg("run").arg(example("containers.ds"))),
        "cons 2 1 1 2\nempty -1 -1\nmax 9 3\n0\n"
    );
}

#[test]
fn combinators_run_their_lambdas_reentrantly() {
    // the lambda-taking combinators dispatch through CALLG and reenter
    // the dispatcher per element: map and filter answer fresh lists,
    // reduce folds (an empty list would answer its init), partition
    // splits into its two-list total at the call site, and the
    // block-body println interleaves in element order; the @std list
    // pair (zip, enumerate) answers as the bodied generic library fns
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
zip 3 enum 2 5
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
    // E3's fork-join fold over the @std library spelling: the
    // non-commutative order witness (a string chain through the native
    // concat) and the empty-input init answer; the multi-word struct
    // fold keeps its seam-level pin in the lowerer's e2e (the library
    // spelling's one-word instantiation boundary)
    assert_eq!(
        run_ok(ds().arg("run").arg(example("par_reduce.ds"))),
        "sum 15\njoined alphabetagamma\nempty 12000\n0\n"
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
    let dir = support::guarded_dir("ds_cli_files_example-");
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
        // one write: the example's recv reads once
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

/// The install identity a `ds save` report pins: the report's
/// `blake3:` row is the content hash the next manifest embeds.
fn saved_hash(output: &str) -> String {
    output
        .rsplit("blake3:")
        .next()
        .expect("the save report names the identity it pinned")
        .trim()
        .to_owned()
}

/// The re-export chain over three staged packages: the middle package's
/// `pub import` row joins the origin's callable, const, and struct onto
/// its export surface (the aliased spelling included), and the committed
/// example — copied in as the root package's entry — selects them beside
/// the middle's own member through one import row. A plain `ds run` of
/// the example declares no deps, so the gate stages the chain: both
/// packages saved into the private store, the root's manifest pinning the
/// middle package alone.
#[test]
fn pub_import_selects_the_reexported_surface_through_the_middle_package() {
    let work = support::guarded_dir("ds_cli_pub_import_example-");

    // the origin package: a callable, a literal const, and a flat struct
    let deep = work.join("deep");
    fs::create_dir_all(&deep).expect("the origin dir creates");
    fs::write(
        deep.join("ds.pkg"),
        "name = \"@deep\"\nversion = \"1.0.0\"\n",
    )
    .expect("the origin manifest writes");
    fs::write(
        deep.join("deep.ds"),
        concat!(
            "pub fn base() -> int {\n",
            "  21\n",
            "}\n",
            "\n",
            "pub const step = 2\n",
            "\n",
            "pub struct unit {\n",
            "  n: int,\n",
            "}\n",
        ),
    )
    .expect("the origin source writes");
    let deep_hash = saved_hash(&run_ok(ds().arg("save").arg(&deep)));

    // the middle package: one pub row re-exports the origin's members
    // (the alias binding `origin` included), its own fn beside them
    let lib = work.join("lib");
    fs::create_dir_all(&lib).expect("the middle dir creates");
    fs::write(
        lib.join("ds.pkg"),
        format!(
            "name = \"@lib\"\nversion = \"1.2.0\"\n\n[deps]\n@deep = {{ ver = \"1.0.0\", hash = \
             \"blake3:{deep_hash}\" }}\n"
        ),
    )
    .expect("the middle manifest writes");
    fs::write(
        lib.join("helper.ds"),
        concat!(
            "pub import @deep (base, base as origin, step, unit)\n",
            "\n",
            "pub fn twice(n: int) -> int {\n",
            "  base() + n\n",
            "}\n",
        ),
    )
    .expect("the middle source writes");
    let lib_hash = saved_hash(&run_ok(ds().arg("save").arg(&lib)));

    // the root: the committed example as the entry, pinning the middle
    // package alone — the re-exported names bind through it
    let root = work.join("root");
    fs::create_dir_all(&root).expect("the root dir creates");
    fs::write(
        root.join("ds.pkg"),
        format!(
            "name = \"app\"\nversion = \"0.1.0\"\n\n[deps]\n@lib = {{ ver = \"1.2.0\", hash = \
             \"blake3:{lib_hash}\" }}\n"
        ),
    )
    .expect("the root manifest writes");
    fs::copy(example("pub_import.ds"), root.join("app.ds"))
        .expect("the example copies in as the entry");

    let entry = root.join("app.ds");
    assert_eq!(
        run_ok(ds().arg("run").arg(&entry)),
        "hi\n87\n",
        "the re-exported callable, const, and struct answer beside the middle package's own member"
    );

    fs::remove_dir_all(&work).expect("the temp tree removes");
}

#[test]
fn race_combinators_answer_in_arrival_order() {
    // the @std race pair over the plan-time mint seam: firstof and
    // interleave compose as ambient members — the caller-built thunk
    // list (non-capturing scalar lambdas) feeding one delivery channel
    // bound to the race's size, one spawned child per thunk receiving
    // its callable into a local binding, the join-all boundary
    // draining after every child completed so the channel's FIFO
    // answers in arrival order (the fast thunk wins whatever its list
    // position); sum is the reduce-based fold over the hosted callable
    assert_eq!(
        run_ok(ds().arg("run").arg(example("race.ds"))),
        "first 2\narrival 2 1\nsum 10\n0\n",
        "the race answers by arrival, and the fold folds"
    );
}

#[test]
fn slice_members_clamp_and_flatten_composes_over_map() {
    // the @std slice pair and the flat_map face's composable half: take
    // and drop clamp at the list's own bounds (a negative, zero, or
    // past-the-end n and the empty list answering the empty or whole
    // list the bounds imply), both spellings bare and UFCS; flatten
    // composes the flat spelling over map's hosted lambda — the
    // fn-typed parameter's list-returning signature never solves from a
    // concrete lambda at the call site (B2), so flatten is the half
    // that lands — the nested-empty edge included, and the fluent
    // spelling chains the same pair through a dot-call's receiver
    assert_eq!(
        run_ok(ds().arg("run").arg(example("slices.ds"))),
        "take 2 0 5 0\nedges 0 3\ndrop 3 5 0 5\nedges 0 1\nflat 10 10\nfluent 10\nempty 0 3\n0\n",
        "the slices clamp at their bounds, and flatten composes"
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
         }\ntitle,views,tags\nfields: 3\nschema ======\n0\n"
    );
}

#[test]
fn generic_dispatch_tells_through_the_staged_evidence() {
    // the transitive evidence surface: `impl[t: shows] tells for list[t]`
    // lowers once and the entering concrete site stages the serving
    // body's whole evidence area, so the nested `show` dispatch rides
    // the staged words at runtime — int and string elements plus the
    // two-level list-of-list hop
    assert_eq!(
        run_ok(ds().arg("run").arg(example("generic_dispatch.ds"))),
        "told list:n=1;n=2;\ntold list:a!;b!;\ntold list:list:n=3;;\nlist:n=4;\n0\n"
    );
}

#[test]
fn records_list_reads_stage_whole_word_spans() {
    // the record-element list over the native result span channel:
    // list_push stages whole inline item words, the checked list_get
    // answers `item | none` re-spanned whole, the `.ok`/`.err` chain
    // extracts on both sides (the missing index landing in the fallback
    // record), and the fields project out of the extracted element
    assert_eq!(
        run_ok(ds().arg("run").arg(example("records_lists.ds"))),
        "count 3 first pen mug 15 miss none\ntotal 42\n0\n"
    );
}

#[test]
fn trait_defaults_answer_from_their_single_authorities() {
    // the defaulted-parameter surface across every call spelling: the
    // trait member's declared default and the written argument both
    // stage identically through the dot-call and the `Trait::method`
    // spelling, and the inherent member keeps the impl block's own
    // default
    assert_eq!(
        run_ok(ds().arg("run").arg(example("trait_defaults.ds"))),
        "int 4 4 13 13\nstr 5 5 9\ncounter 4 11\n0\n"
    );
}

#[test]
fn json_round_trips_through_the_derive_families() {
    // the json face plus both derive families: parse_json and the plain
    // writer round trip one open document, the from_json walk answers
    // the declared struct (an absent optional reading none, a missing
    // member or a wrong shape answering parse_err), the to_json walk
    // encodes back with the absent optional's member left out, and the
    // enum target reads its unit cases from their names and its one
    // payloaded case from the member its payload declares
    assert_eq!(
        run_ok(ds().arg("run").arg(example("json.ds"))),
        concat!(
            "{\"b\":[1,2.5,true,null],\"a\":\"x\"}\n",
            "{\"name\":\"ada\",\"age\":45}\n",
            "{\"name\":\"bob\",\"age\":15,\"email\":\"bob@ds.dev\"}\n",
            "parse_err\n",
            "parse_err\n",
            "\"quiet\"\n",
            "{\"volume\":3}\n",
            "parse_err\n",
            "0\n"
        )
    );
}

#[test]
fn json_width_round_trips_the_scalar_field_vocabulary() {
    // the width-scalar fields through both derive families over one
    // declared frame: byte and u32 decode from and encode as json
    // numbers (the range edges 0/255 and 0/2^32-1 included), char as a
    // one-character json string, bytes as a json array of numbers, and
    // an out-of-range u32, a two-character char, an out-of-range byte
    // element, and a missing member each answer parse_err
    assert_eq!(
        run_ok(ds().arg("run").arg(example("json_width.ds"))),
        concat!(
            "{\"tag\":65,\"index\":4000000000,\"mark\":\"q\",\"payload\":[104,105]}\n",
            "{\"tag\":255,\"index\":4294967295,\"mark\":\"é\",\"payload\":[0,255]}\n",
            "{\"tag\":0,\"index\":0,\"mark\":\"q\",\"payload\":[]}\n",
            "parse_err\n",
            "parse_err\n",
            "parse_err\n",
            "parse_err\n",
            "0\n"
        )
    );
}

#[test]
fn bytes_face_shows_orders_and_crosses_the_utf8_bridges() {
    // the bytes representation: show renders the payload as a byte list
    // (empty printing bytes[]), len counts the payload, the checked
    // byte_at reads, == compares content against the encode bridge's
    // answer, decode validates on the way back (the orphan lead byte
    // refusing as utf8_err), and the show derive composes over a bytes
    // part
    assert_eq!(
        run_ok(ds().arg("run").arg(example("bytes.ds"))),
        concat!(
            "bytes[97, 98]\n",
            "bytes[]\n",
            "2 true\n",
            "true\n",
            "6\n",
            "héllo\n",
            "orphan byte refused\n",
            "chunk { data: bytes[97, 98] }\n",
            "0\n"
        )
    );
}

/// Every committed example carries a run gate: each `examples/*.ds`
/// file must appear in an `example(...)` call above, so a new example
/// cannot land unguarded — the plan's "every file in examples/ runs
/// green" held only by listing the files here.
#[test]
fn every_example_has_a_gate() {
    let examples = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples");
    let suite =
        fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/examples.rs"))
            .expect("the suite reads its own source");
    let mut gated = 0;
    for entry in fs::read_dir(&examples).expect("the examples dir reads") {
        let path = entry.expect("the entry reads").path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("ds") {
            continue;
        }
        let name = path.file_name().expect("the entry has a name");
        assert!(
            suite.contains(&format!("example(\"{}\")", name.to_string_lossy())),
            "{} has no gate in tests/examples.rs",
            name.to_string_lossy()
        );
        gated += 1;
    }
    assert!(gated > 0, "the examples dir holds at least one example");
}

#[test]
fn build_then_run_the_module_reproduces_the_source_output() {
    let dir = support::guarded_dir("ds_cli_build_example-");
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
