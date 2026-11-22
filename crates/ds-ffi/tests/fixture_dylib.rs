//! The bind round-trip over a real C-ABI shared object: the host's own
//! rustc compiles the fixture source as a cdylib (no C toolchain), the
//! loading layer binds it, and the export reader enumerates its symbols
//! — every enumerated name resolving through the bound handle. Cargo
//! sets `RUSTC` for the test binary, so a fixture build failure is a
//! test failure, never a silent pass; the loading-layer units stay
//! unconditional.

use std::{
    env,
    path::{Path, PathBuf},
    process::Command,
};

use ds_ffi::{Answer, Arg, Library, Param, Ret, Scalar, ScalarKind, ScalarSig, Sig};

mod support;

/// The fixture library's file name per platform: the loader-recognized
/// spelling rustc emits for a cdylib.
fn library_name() -> &'static str {
    if cfg!(windows) {
        "dsffi_fixture.dll"
    } else if cfg!(target_os = "macos") {
        "libdsffi_fixture.dylib"
    } else {
        "libdsffi_fixture.so"
    }
}

/// Compiles the fixture cdylib with the host's rustc into `dir`. Cargo
/// sets `RUSTC` for the test binary, so a build failure is a test
/// failure carrying rustc's own stderr — never a silent pass.
fn build_fixture(dir: &Path) -> PathBuf {
    let rustc = env::var("RUSTC").unwrap_or_else(|_| "rustc".to_owned());
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fixture_cdylib.rs");
    let out = dir.join(library_name());
    let built = Command::new(rustc)
        .arg("--edition=2024")
        .arg("--crate-type=cdylib")
        .arg("--crate-name=dsffi_fixture")
        .arg(&source)
        .arg("-o")
        .arg(&out)
        .output()
        .unwrap_or_else(|error| panic!("the fixture's rustc runs: {error}"));
    assert!(
        built.status.success(),
        "the fixture cdylib builds (rustc stderr: {})",
        String::from_utf8_lossy(&built.stderr)
    );
    out
}

/// One unique guarded temp dir per leg, removed at the test thread's
/// exit.
fn temp_dir(tag: &str) -> PathBuf {
    support::guarded_dir(&format!("ds-ffi-{tag}-"))
}

/// A compiled cdylib binds: the host loader takes it, every exported
/// symbol the reader enumerates resolves through the handle, and a name
/// nothing exports answers `None`.
#[test]
fn a_compiled_cdylib_binds_and_enumerates() {
    let dir = temp_dir("fixture");
    let dylib = build_fixture(&dir);
    let library = Library::open(&dylib).unwrap_or_else(|error| panic!("{error}"));
    for name in ["ds_add", "ds_scale", "ds_answer"] {
        assert!(
            library.symbol(name).is_some(),
            "the fixture exports `{name}`: the bound handle resolves it"
        );
    }
    assert!(
        library.symbol("ds_not_exported").is_none(),
        "a name the fixture does not export answers None"
    );
    let bytes = std::fs::read(&dylib).expect("the fixture reads back");
    let symbols = ds_ffi::enumerate(&bytes).expect("the fixture's export table reads");
    for name in ["ds_add", "ds_answer", "ds_scale"] {
        assert!(
            symbols.iter().any(|symbol| symbol == name),
            "the enumeration reports `{name}` (found: {symbols:?})"
        );
    }
    assert!(
        symbols
            .iter()
            .all(|symbol| library.symbol(symbol).is_some()),
        "every enumerated name resolves through the bound handle ({symbols:?})"
    );
    // the enumeration is a stable report: sorted, deduplicated
    let mut sorted = symbols.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(
        symbols, sorted,
        "the enumeration is sorted and deduplicated"
    );
}

/// The scalar marshaling answers real calls: every register-class shape
/// the slice rules — the i64 GP pair, the f64 FP pair, the empty call,
/// the u8 GP word, and the mixed GP+FP sequence — calls the bound
/// symbol and marshals the answer back.
#[test]
fn scalar_calls_answer_over_every_register_class() {
    let dir = temp_dir("scalar");
    let dylib = build_fixture(&dir);
    let library = Library::open(&dylib).unwrap_or_else(|error| panic!("{error}"));
    let sig = |params: &[ScalarKind], ret: ScalarKind| ScalarSig {
        params: params.to_vec(),
        ret,
    };
    let call = |name: &str, sig: &ScalarSig, args: &[Scalar]| {
        let symbol = library.symbol(name).expect("the fixture exports the name");
        symbol
            .call_scalar(sig, args)
            .unwrap_or_else(|error| panic!("the scalar call answers: {error}"))
    };
    let int = ScalarKind::Int;
    let boolean = ScalarKind::Bool;
    let float = ScalarKind::Float;
    assert_eq!(
        call("ds_add", &sig(&[int, int], int), &[
            Scalar::Int(20),
            Scalar::Int(22)
        ]),
        Scalar::Int(42),
        "the GP pair marshals"
    );
    assert_eq!(
        call("ds_scale", &sig(&[float, float], float), &[
            Scalar::Float(1.5),
            Scalar::Float(4.0)
        ]),
        Scalar::Float(6.0),
        "the FP pair marshals"
    );
    assert_eq!(
        call("ds_answer", &sig(&[], int), &[]),
        Scalar::Int(42),
        "the empty call marshals"
    );
    assert_eq!(
        call("ds_not", &sig(&[boolean], boolean), &[Scalar::Bool(false)]),
        Scalar::Bool(true),
        "the u8 word marshals"
    );
    assert_eq!(
        call("ds_gated", &sig(&[boolean, float], float), &[
            Scalar::Bool(true),
            Scalar::Float(3.5)
        ]),
        Scalar::Float(3.5),
        "the mixed GP+FP sequence marshals"
    );
}

/// The marshaling face answers real calls over its widened encodings:
/// the call-scoped string/bytes pair (alone, mixed with an FP word, and
/// two pairs filling the word envelope) and the unit return (over a
/// pair and over no words), the unit call's effect read back through
/// the scalar face.
#[test]
fn marshaled_calls_carry_pairs_and_unit_returns() {
    let dir = temp_dir("marshal");
    let dylib = build_fixture(&dir);
    let library = Library::open(&dylib).unwrap_or_else(|error| panic!("{error}"));
    let symbol = |name: &str| library.symbol(name).expect("the fixture exports the name");
    let sig = |params: &[Param], ret: Ret| Sig {
        params: params.to_vec(),
        ret,
    };
    // the pair alone: the pointer is dereferenced, so the tally proves
    // the staged borrow reached the callee
    assert_eq!(
        symbol("ds_tally").call(&sig(&[Param::Str], Ret::Int), &[Arg::Str("abc")]),
        Ok(Answer::Int(294)),
        "the string pair marshals its bytes"
    );
    assert_eq!(
        symbol("ds_tally").call(&sig(&[Param::Bytes], Ret::Int), &[Arg::Bytes(&[
            0, 1, 0xff
        ])]),
        Ok(Answer::Int(256)),
        "the bytes pair marshals its buffer, zero and 0xFF included"
    );
    // the pair followed by an FP word: the mixed placement
    assert_eq!(
        symbol("ds_weighed").call(&sig(&[Param::Str, Param::Float], Ret::Float), &[
            Arg::Str("abc"),
            Arg::Float(0.5)
        ],),
        Ok(Answer::Float(147.0)),
        "the pair plus float word marshals"
    );
    // two pairs: the four-GP-word envelope
    assert_eq!(
        symbol("ds_total").call(&sig(&[Param::Str, Param::Bytes], Ret::Int), &[
            Arg::Str("ab"),
            Arg::Bytes(&[1, 2])
        ],),
        Ok(Answer::Int(198)),
        "two pairs fill the word envelope"
    );
    // the unit return over a pair, its effect read back through the
    // scalar face
    assert_eq!(
        symbol("ds_record").call(&sig(&[Param::Str], Ret::Unit), &[Arg::Str("hello")]),
        Ok(Answer::Unit),
        "the unit return answers the absent word"
    );
    assert_eq!(
        symbol("ds_recorded")
            .call_scalar(
                &ScalarSig {
                    params: Vec::new(),
                    ret: ScalarKind::Int,
                },
                &[],
            )
            .ok(),
        Some(Scalar::Int(5)),
        "the unit call ran: the recorded length answers through the scalar face"
    );
    // the unit return over no words at all
    assert_eq!(
        symbol("ds_clear").call(&sig(&[], Ret::Unit), &[]),
        Ok(Answer::Unit),
        "the empty unit call marshals"
    );
    assert_eq!(
        symbol("ds_recorded")
            .call_scalar(
                &ScalarSig {
                    params: Vec::new(),
                    ret: ScalarKind::Int,
                },
                &[],
            )
            .ok(),
        Some(Scalar::Int(0)),
        "the empty unit call ran: the record cleared"
    );
}
