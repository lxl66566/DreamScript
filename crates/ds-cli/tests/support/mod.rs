//! Drop-time temp roots for the e2e harnesses: every directory a test
//! creates under the system tmp registers on the test's own thread, so
//! the guard list's drop — a parallel harness's thread exit, or a
//! single-test process's main return — removes the tree, panics
//! included. Nothing accumulates across runs against the tmpfs.

// each harness links the whole module and uses its own subset
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

/// One temp dir per thread per prefix: the first call on a thread
/// creates the root and every later call with the same prefix answers
/// the same path, so a test's cold run warms the same root its warm
/// run reads.
pub fn shared_dir(prefix: &str) -> PathBuf {
    thread_local! {
        static SHARED: RefCell<Option<(String, PathBuf)>> = const { RefCell::new(None) };
    }
    SHARED.with(|shared| {
        if let Some((held, path)) = shared.borrow().as_ref()
            && held == prefix
        {
            return path.clone();
        }
        let path = guarded_dir(prefix);
        *shared.borrow_mut() = Some((prefix.to_owned(), path.clone()));
        path
    })
}
