//! The scope-index query: the ds-check scope walk over the file's
//! derive-expanded tree, memoized per file — the substrate completion's
//! position slice reads.

use ds_query::{Db, ScopeIndex};

const SOURCE: &str = concat!(
    "fn add(n: int) -> int {\n",
    "  const m = n\n",
    "  m\n",
    "}\n",
    "##\n",
    "const top = 1\n",
);

#[test]
fn the_index_answers_the_file_s_frames() {
    let mut db = Db::new();
    let file = db.add_file("m.ds", SOURCE);
    let frames = db.get::<ScopeIndex>(file).unwrap();
    // The lambda parameter scope and the body block scope both frame.
    assert!(
        frames.frames.len() >= 2,
        "the parameter and block scopes frame: {:?}",
        frames.frames
    );
    // A file that never registered answers empty frames.
    let missing = db.get::<ScopeIndex>(ds_diag::FileId::new(9)).unwrap();
    assert!(missing.frames.is_empty());
}

#[test]
fn a_broken_parse_answers_empty_frames() {
    let mut db = Db::new();
    let file = db.add_file("m.ds", "fn add(n: int) -> int {\n");
    let frames = db.get::<ScopeIndex>(file).unwrap();
    assert!(frames.frames.is_empty());
    // The file healing re-derives the index: the scopes answer again.
    db.set_file_content(file, SOURCE).unwrap();
    let frames = db.get::<ScopeIndex>(file).unwrap();
    assert!(!frames.frames.is_empty());
}
