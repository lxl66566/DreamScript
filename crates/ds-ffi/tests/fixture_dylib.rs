//! The bind round-trip over a real C-ABI shared object: the host's own
//! rustc compiles the fixture source as a cdylib (no C toolchain), the
//! loading layer binds it, and the export reader enumerates its symbols
//! — every enumerated name resolving through the bound handle. The leg
//! is gated behind the fixture build: a host without a runnable rustc
//! passes with a note, while the loading-layer units stay unconditional.

use std::{
    env,
    path::{Path, PathBuf},
    process::Command,
};

use ds_ffi::Library;

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

/// Compiles the fixture cdylib with the host's rustc into `dir`, `None`
/// when the toolchain cannot run (the gated leg's skip).
fn build_fixture(dir: &Path) -> Option<PathBuf> {
    let rustc = env::var("RUSTC").unwrap_or_else(|_| "rustc".to_owned());
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/scalar_cdylib.rs");
    let out = dir.join(library_name());
    let built = Command::new(rustc)
        .arg("--edition=2024")
        .arg("--crate-type=cdylib")
        .arg("--crate-name=dsffi_fixture")
        .arg(&source)
        .arg("-o")
        .arg(&out)
        .output()
        .ok()?;
    if !built.status.success() {
        return None;
    }
    Some(out)
}

/// One unique temp dir per leg, removed first.
fn temp_dir(tag: &str) -> PathBuf {
    let dir = env::temp_dir().join(format!("ds-ffi-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("the temp dir creates");
    dir
}

/// A compiled cdylib binds: the host loader takes it, every exported
/// symbol the reader enumerates resolves through the handle, and a name
/// nothing exports answers `None`.
#[test]
fn a_compiled_cdylib_binds_and_enumerates() {
    let dir = temp_dir("fixture");
    let Some(dylib) = build_fixture(&dir) else {
        eprintln!("skipping: the host's rustc cannot build the fixture cdylib");
        return;
    };
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
