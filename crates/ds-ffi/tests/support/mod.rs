//! Drop-time temp roots for the harness: every directory a test creates
//! under the system tmp registers on the test's own thread, so the guard
//! list's drop — a parallel harness's thread exit, or a single-test
//! process's main return — removes the tree, panics included. Kept on
//! std alone: a new dev-dependency here re-keys the ambient dsmod's
//! build identity and drags the artifact regen into a test-hygiene
//! round.

#![allow(dead_code)]

use std::{
    cell::RefCell,
    env, fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

/// A temp dir removed when its guard drops; a missing dir at drop (an
/// eager test's own removal) is not an error.
struct Guarded(PathBuf);

impl Drop for Guarded {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

thread_local! {
    static GUARDS: RefCell<Vec<Guarded>> = const { RefCell::new(Vec::new()) };
}

/// One fresh guarded temp dir per call, named `{prefix}{unique}`: the
/// caller keeps the path, the thread's guard list keeps the removal,
/// and the unique tail pairs the pid with a process-global counter and
/// the wall clock, so neither two calls nor a pid reuse collide.
pub fn guarded_dir(prefix: &str) -> PathBuf {
    static COUNT: AtomicU64 = AtomicU64::new(0);
    let unique = format!(
        "{}-{}-{}",
        std::process::id(),
        COUNT.fetch_add(1, Ordering::Relaxed),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| since.as_nanos()),
    );
    let dir = env::temp_dir().join(format!("{prefix}{unique}"));
    fs::create_dir_all(&dir).expect("the guarded temp dir creates");
    GUARDS.with(|guards| guards.borrow_mut().push(Guarded(dir.clone())));
    dir
}
