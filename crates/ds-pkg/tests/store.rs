//! The content-addressed store pinned: an install lands atomically under
//! its own identity with no temp file surviving, validation recomputes
//! from the installed file's own bytes, a disagreement refuses naming
//! the file, a refetch repairs disagreeing bytes, and the store's rows
//! self-describe while drifted files serve nothing.

use ds_diag::FileId;
use ds_mod::Hash128;
use ds_pkg::{
    Manifest, Package, Store, StoreError, bundle_content_hash,
    store::{InstalledRow, SEGMENT},
};

/// One fixture package: a manifest plus one source member.
fn package_of(name: &str, version: &str, source: &str) -> Package {
    let manifest = Manifest::parse(
        &format!("name = \"{name}\"\nversion = \"{version}\"\n"),
        FileId::new(0),
    )
    .expect("fixture manifest parses");
    Package::new(&manifest, [(
        format!("{name}.ds"),
        source.as_bytes().to_vec(),
    )])
    .expect("fixture package builds")
}

/// Every file name directly below the store's directory, lossily.
fn entries(store: &Store) -> Vec<String> {
    std::fs::read_dir(store.dir())
        .expect("the store directory exists")
        .map(|entry| {
            entry
                .expect("the directory lists")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect()
}

#[test]
fn an_install_lands_atomically_under_its_own_identity() {
    let root = tempfile::tempdir().expect("temp root");
    let store = Store::open(root.path());
    let package = package_of("app", "1.0.0", "fn main() {}");
    let identity = store.install(&package).expect("the install lands");
    assert_eq!(identity, package.content_hash());
    // the file sits at its identity below the cache root's pkg segment
    let file = store.path_of(identity);
    assert_eq!(
        file,
        root.path().join(SEGMENT).join(format!("{identity}.pkg"))
    );
    // nothing else survives the successful install: no temp file, no
    // side table, exactly the one content-addressed file
    assert_eq!(entries(&store), [format!("{identity}.pkg")]);
    // and it validates by its own bytes alone, round-tripping the model
    assert_eq!(store.load(identity).unwrap(), Some(package.clone()));
    // a repeated install is the same identity, still exactly one file
    assert_eq!(store.install(&package).unwrap(), identity);
    assert_eq!(entries(&store), [format!("{identity}.pkg")]);
}

#[test]
fn an_absent_identity_or_directory_answers_empty() {
    let root = tempfile::tempdir().expect("temp root");
    let store = Store::open(root.path());
    // the missing store directory materializes lazily: reads answer
    // empty without creating it
    assert!(store.load(Hash128::new([7; 16])).unwrap().is_none());
    assert!(store.rows().is_empty());
    assert!(!store.dir().exists());
    // and the first install creates the subtree
    store
        .install(&package_of("app", "1.0.0", "fn main() {}"))
        .expect("the install materializes the directory");
    assert!(store.dir().is_dir());
    assert!(store.load(Hash128::new([7; 16])).unwrap().is_none());
}

#[test]
fn a_disagreement_refuses_naming_the_file() {
    let root = tempfile::tempdir().expect("temp root");
    let store = Store::open(root.path());
    let package = package_of("app", "1.0.0", "fn main() {}");
    let identity = store.install(&package).expect("the install lands");
    // drift the installed bytes: the file's own hash no longer answers
    // the identity its name claims
    let file = store.path_of(identity);
    let corrupt = b"not a bundle".to_vec();
    std::fs::write(&file, &corrupt).expect("the drift lands");
    assert!(matches!(
        store.load(identity),
        Err(StoreError::Disagreement {
            path,
            expected,
            found,
        }) if path == file && expected == identity && found == bundle_content_hash(&corrupt)
    ));
}

#[test]
fn a_refetch_repairs_disagreeing_bytes() {
    let root = tempfile::tempdir().expect("temp root");
    let store = Store::open(root.path());
    let package = package_of("app", "1.0.0", "fn main() {}");
    let identity = store.install(&package).expect("the install lands");
    std::fs::write(store.path_of(identity), b"corrupt bytes").expect("the corruption lands");
    // a refetched bundle installs over the disagreeing file: the write
    // is the repair
    assert_eq!(store.install(&package).unwrap(), identity);
    assert_eq!(store.load(identity).unwrap(), Some(package));
    assert_eq!(entries(&store), [format!("{identity}.pkg")]);
}

#[test]
fn rows_self_describe_while_drifted_files_serve_nothing() {
    let root = tempfile::tempdir().expect("temp root");
    let store = Store::open(root.path());
    let app = package_of("app", "1.0.0", "fn main() {}");
    let lib = package_of("lib", "2.3.0", "fn answer() {}");
    let app_identity = store.install(&app).expect("the app installs");
    let lib_identity = store.install(&lib).expect("the lib installs");
    // three drifted files: corrupt bytes under a real identity, a valid
    // bundle sitting at another identity's path, and a non-pkg stray
    std::fs::write(store.path_of(app_identity), b"corrupt bytes").expect("the corruption lands");
    std::fs::write(store.path_of(Hash128::new([9; 16])), lib.to_bytes())
        .expect("the misplacement lands");
    std::fs::write(store.dir().join("README"), b"stray").expect("the stray lands");
    // only lib's intact file self-describes a row
    let rows: Vec<(String, Hash128)> = store
        .rows()
        .iter()
        .map(|row| (row.manifest.name.clone(), row.identity))
        .collect();
    assert_eq!(rows, [("lib".to_owned(), lib_identity)]);
    // the intact row still loads by its identity
    let row: &InstalledRow = &store.rows()[0];
    assert_eq!(store.load(row.identity).unwrap(), Some(lib));
}
