//! Drop-time temp roots for the harness: every directory a test creates
//! under the system tmp registers on the test's own thread, so the guard
//! list's drop — a parallel harness's thread exit, or a single-test
//! process's main return — removes the tree, panics included.

#![allow(dead_code)]

use std::{cell::RefCell, path::PathBuf};

thread_local! {
    static GUARDS: RefCell<Vec<tempfile::TempDir>> = const { RefCell::new(Vec::new()) };
}

/// One fresh guarded temp dir per call, named `{prefix}{random}`: the
/// caller keeps the path, the thread's guard list keeps the removal,
/// and the random tail replaces a pid-keyed name that a pid reuse could
/// collide on.
pub fn guarded_dir(prefix: &str) -> PathBuf {
    let dir = tempfile::Builder::new()
        .prefix(prefix)
        .tempdir()
        .expect("the guarded temp dir creates");
    let path = dir.path().to_owned();
    GUARDS.with(|guards| guards.borrow_mut().push(dir));
    path
}
