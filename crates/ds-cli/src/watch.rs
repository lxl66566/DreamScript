//! The dev loop's change signal: the gate deciding whether one tick
//! re-reads the watched bytes. The tick itself (`--poll-ms`) stays the
//! round cadence — the parked entry's drive rate and the merge window
//! that folds a multi-file edit into one round — so the signal moves
//! only the idle tick's cost: a window that saw no event reads
//! nothing, one that saw any reads exactly what the poll always read,
//! and the byte compare stays the change judge whatever platform
//! serves the signal. On Linux the signal is raw inotify over the OS's
//! own symbols — `inotify_init1`/`inotify_add_watch` and the event
//! stream off one plain read, `poll(2)` timing the window — the epoll
//! driver's no-watcher-crate precedent, so no fs-watcher crate enters
//! the dependency policy's queue. Directories carry the watches (an
//! editor's atomic replace swaps the file's inode, so the file itself
//! is never the watch), the watched file's name filters a script
//! target's events, a package root's tree grows its watches as
//! subdirectories appear, and a watch the kernel dropped (its
//! directory deleted) degrades to reading every window until the path
//! returns — never worse than the poll twin, which is also what every
//! other platform runs, and what any watcher refusal falls back to.

use std::{path::Path, thread, time::Duration};

#[cfg(target_os = "linux")]
use {
    std::collections::HashMap, std::io::Read, std::os::fd::AsRawFd, std::os::unix::ffi::OsStrExt,
    std::time::Instant,
};

/// The dev loop's change signal over one watch target: one file (its
/// parent directory watched, the file's name the filter) or a package
/// root's tree (every directory below the root, no filter).
#[derive(Debug)]
pub(crate) enum FsWatch {
    /// The inotify-backed signal (Linux).
    #[cfg(target_os = "linux")]
    Inotify(Inotify),
    /// The poll twin: the window sleeps and every window reads.
    Poll,
}

impl FsWatch {
    /// The signal over one watched file: its parent directory, the
    /// events naming the file alone kept.
    #[must_use]
    pub(crate) fn script(file: &Path) -> Self {
        let dir = file
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .map_or_else(|| std::path::PathBuf::from("."), Path::to_path_buf);
        let only = file.file_name().map(std::ffi::OsString::from);
        Self::signal(&[dir], only)
    }

    /// The signal over several trees: a package root's tree plus every
    /// resolved path dep directory's, one watch set — roots naming one
    /// directory through different spellings deduped by their canonical
    /// forms, so a dep inside the package tree costs no second watch.
    #[must_use]
    pub(crate) fn trees(roots: &[std::path::PathBuf]) -> Self {
        let mut seen = std::collections::BTreeSet::new();
        let dirs = roots
            .iter()
            .filter(|root| seen.insert(root.canonicalize().unwrap_or_else(|_| (*root).clone())))
            .cloned()
            .collect::<Vec<_>>();
        Self::signal(&dirs, None)
    }

    /// The platform's signal over the watched directories: the
    /// inotify driver where it initializes whole (any refusal — the
    /// fd ceiling, an unreadable subtree — keeps the poll twin).
    fn signal(dirs: &[std::path::PathBuf], only: Option<std::ffi::OsString>) -> Self {
        #[cfg(target_os = "linux")]
        {
            if let Some(watch) = Inotify::new(dirs, only) {
                return Self::Inotify(watch);
            }
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (dirs, only);
        }
        Self::Poll
    }

    /// Waits out one full window (the window is the merge cadence: an
    /// early round would split the multi-file edits the tick composes)
    /// and answers whether the window's read should happen.
    pub(crate) fn wait(&mut self, tick: Duration) -> bool {
        match self {
            #[cfg(target_os = "linux")]
            Self::Inotify(watch) => watch.wait(tick),
            Self::Poll => {
                thread::sleep(tick);
                true
            },
        }
    }
}

/// The inotify signal (Linux): one nonblocking descriptor, one watch
/// per directory of the target, the wd map grown as subdirectories
/// appear.
#[cfg(target_os = "linux")]
#[derive(Debug)]
pub(crate) struct Inotify {
    /// The owned descriptor: the inotify fd, closed on drop (which
    /// drops every watch with it).
    fd: std::fs::File,
    /// The watched directories by watch descriptor.
    dirs: HashMap<i32, std::path::PathBuf>,
    /// The directories whose watch the kernel dropped: retried every
    /// window until the path returns, every window reading meanwhile.
    gone: Vec<std::path::PathBuf>,
    /// The one watched file name (a script target): events naming
    /// anything else in the watched directories stay quiet.
    only: Option<std::ffi::OsString>,
}

#[cfg(target_os = "linux")]
mod inotify_imp {
    //! The OS binding: inotify's own symbols plus `poll(2)` for the
    //! timed window, the loader-binding precedent — no watcher crate,
    //! no new dependency.
    pub use std::ffi::{c_char, c_int, c_uint};

    /// The watched changes: a written file's close (the writer's own
    /// close, so no torn mid-write read ever gates), an atomic
    /// replace's rename, the arrivals and departures around them, and
    /// the watched directory's own move or deletion. `IN_MODIFY` stays
    /// out: a modification's writer closes after it, and the close's
    /// bytes are whole.
    pub const WATCH_MASK: u32 = IN_CLOSE_WRITE
        | IN_MOVED_FROM
        | IN_MOVED_TO
        | IN_CREATE
        | IN_DELETE
        | IN_DELETE_SELF
        | IN_MOVE_SELF;

    // the kernel ABI constants, stable since 2.6.19
    pub const IN_CLOSE_WRITE: u32 = 0x008;
    pub const IN_MOVED_FROM: u32 = 0x040;
    pub const IN_MOVED_TO: u32 = 0x080;
    pub const IN_CREATE: u32 = 0x100;
    pub const IN_DELETE: u32 = 0x200;
    pub const IN_DELETE_SELF: u32 = 0x400;
    pub const IN_MOVE_SELF: u32 = 0x800;
    pub const IN_Q_OVERFLOW: u32 = 0x4000;
    pub const IN_IGNORED: u32 = 0x8000;
    pub const IN_ISDIR: u32 = 0x4000_0000;
    pub const IN_NONBLOCK: c_int = 0o4000;
    pub const IN_CLOEXEC: c_int = 0o2_000_000;
    /// `read(2)` answers whole events alone, so a short buffer never
    /// splits one: the fixed header every event prefixes its name with.
    pub const EVENT_HEADER: usize = 16;
    /// The one readiness interest the window waits on.
    pub const POLLIN: i16 = 0x001;

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct PollFd {
        pub fd: c_int,
        pub events: i16,
        pub revents: i16,
    }

    unsafe extern "C" {
        pub fn inotify_init1(flags: c_int) -> c_int;
        pub fn inotify_add_watch(fd: c_int, pathname: *const c_char, mask: u32) -> c_int;
        pub fn poll(fds: *mut PollFd, nfds: c_uint, timeout: c_int) -> c_int;
    }
}

#[cfg(target_os = "linux")]
impl Inotify {
    /// One fresh signal over the target's directories: every watch the
    /// tree needs must land here or the caller keeps the poll twin —
    /// a partially watched target is a silently missed change.
    fn new(dirs: &[std::path::PathBuf], only: Option<std::ffi::OsString>) -> Option<Self> {
        // SAFETY: the inotify ABI's plain call; the returned fd is
        // adopted by the owned handle that closes it on drop.
        let raw = unsafe {
            inotify_imp::inotify_init1(inotify_imp::IN_NONBLOCK | inotify_imp::IN_CLOEXEC)
        };
        if raw < 0 {
            return None;
        }
        let mut watch = Self {
            fd: {
                use std::os::fd::FromRawFd;
                unsafe { std::fs::File::from_raw_fd(raw) }
            },
            dirs: HashMap::new(),
            gone: Vec::new(),
            only,
        };
        for dir in dirs {
            watch.add_tree(dir)?;
        }
        Some(watch)
    }

    /// One directory's watch plus, recursively, every directory below
    /// it — no symlink follows: a link's own metadata is not a
    /// directory, the same rule the member walk admits.
    fn add_tree(&mut self, dir: &Path) -> Option<i32> {
        let wd = self.add_dir(dir)?;
        for entry in std::fs::read_dir(dir).ok()?.flatten() {
            if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                self.add_tree(&entry.path())?;
            }
        }
        Some(wd)
    }

    /// One directory's watch, recorded by its descriptor.
    fn add_dir(&mut self, dir: &Path) -> Option<i32> {
        let path = std::ffi::CString::new(dir.as_os_str().as_bytes()).ok()?;
        // SAFETY: the plain add call; `path` outlives it and the fd is
        // the owned descriptor's own.
        let wd = unsafe {
            inotify_imp::inotify_add_watch(
                self.fd.as_raw_fd(),
                path.as_ptr(),
                inotify_imp::WATCH_MASK,
            )
        };
        if wd < 0 {
            return None;
        }
        self.dirs.insert(wd, dir.to_path_buf());
        Some(wd)
    }

    /// Retries the dropped watches' paths: a directory back at its
    /// path regains its watch (the tree below it with it), a still
    /// missing one keeps reading.
    fn retry_gone(&mut self) {
        let mut kept = Vec::new();
        for dir in std::mem::take(&mut self.gone) {
            if self.add_tree(&dir).is_none() {
                kept.push(dir);
            }
        }
        self.gone = kept;
    }

    /// Waits out one full window, however many events arrive mid-way
    /// (an early return would split the edits the window composes),
    /// folding whether any watched change signalled.
    fn wait(&mut self, tick: Duration) -> bool {
        // a dropped watch never goes quiet: its window reads until the
        // path returns
        let mut dirty = !self.gone.is_empty();
        if dirty {
            self.retry_gone();
        }
        let deadline = Instant::now() + tick;
        loop {
            let remain = deadline.saturating_duration_since(Instant::now());
            if remain.is_zero() {
                break;
            }
            let millis =
                i32::try_from((remain.as_millis() + 1).min(i32::MAX as u128)).unwrap_or(i32::MAX);
            let mut slot = [inotify_imp::PollFd {
                fd: self.fd.as_raw_fd(),
                events: inotify_imp::POLLIN,
                revents: 0,
            }];
            // SAFETY: the plain poll call; `slot` holds the one
            // interest and answers into itself.
            let ready = unsafe { inotify_imp::poll(slot.as_mut_ptr(), 1, millis) };
            if ready < 0 {
                // EINTR: the remainder keeps its wait; any other
                // refusal degrades this window to the read
                if std::io::Error::last_os_error().raw_os_error() != Some(4) {
                    return true;
                }
            } else if slot[0].revents != 0 {
                dirty |= self.drain();
            }
        }
        // the deadline's own race: one last nonblocking drain
        dirty |= self.drain();
        dirty
    }

    /// Reads and parses every queued event, folding whether one names
    /// a watched change.
    fn drain(&mut self) -> bool {
        let mut dirty = false;
        let mut buf = [0; 8192];
        loop {
            match self.fd.read(&mut buf) {
                Ok(0) => break,
                Ok(read) => dirty |= self.parse(&buf[..read]),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                // a refused read degrades to reading: this window and on
                Err(_) => return true,
            }
        }
        dirty
    }

    /// Parses one read's whole events, folding relevance: a script
    /// target keeps the events naming its own file (and its watched
    /// directory's own departure, which carries no name), a tree
    /// target counts everything below the root.
    fn parse(&mut self, stream: &[u8]) -> bool {
        let mut dirty = false;
        let mut offset = 0;
        while offset + inotify_imp::EVENT_HEADER <= stream.len() {
            let wd = i32::from_ne_bytes(
                stream[offset..offset + 4]
                    .try_into()
                    .expect("four header bytes"),
            );
            let mask = u32::from_ne_bytes(
                stream[offset + 4..offset + 8]
                    .try_into()
                    .expect("four header bytes"),
            );
            let len = u32::from_ne_bytes(
                stream[offset + 12..offset + 16]
                    .try_into()
                    .expect("four header bytes"),
            );
            let name_len = usize::try_from(len).unwrap_or(0);
            let Some(next) = offset
                .checked_add(inotify_imp::EVENT_HEADER)
                .and_then(|start| start.checked_add(name_len))
            else {
                break;
            };
            if next > stream.len() {
                break;
            }
            let name_start = offset + inotify_imp::EVENT_HEADER;
            let name = stream[name_start..next]
                .split(|byte| *byte == 0)
                .next()
                .unwrap_or_default();
            offset = next;
            if mask & inotify_imp::IN_Q_OVERFLOW != 0 {
                // the kernel dropped events: read regardless
                dirty = true;
                continue;
            }
            if mask & inotify_imp::IN_IGNORED != 0 {
                // the kernel dropped this watch (its directory gone):
                // the path retries until it returns
                if let Some(dir) = self.dirs.remove(&wd) {
                    self.gone.push(dir);
                    dirty = true;
                }
                continue;
            }
            let Some(dir) = self.dirs.get(&wd).cloned() else {
                continue;
            };
            // a subdirectory arriving below a watched directory joins
            // the tree's watch, its own subtree with it; one that
            // refuses its watch reads until it lands
            if mask & inotify_imp::IN_ISDIR != 0
                && mask & (inotify_imp::IN_CREATE | inotify_imp::IN_MOVED_TO) != 0
            {
                let path = dir.join(std::ffi::OsStr::from_bytes(name));
                if self.add_tree(&path).is_none() {
                    self.gone.push(path);
                }
            }
            let relevant = match &self.only {
                Some(only) => name.is_empty() || only.as_bytes() == name,
                None => true,
            };
            dirty |= relevant;
        }
        dirty
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::FsWatch;

    /// One window's ceiling: every assertion's events land well inside
    /// it whatever the machine's load.
    const TICK: Duration = Duration::from_millis(60);

    /// The script-shaped signal: a quiet window reads nothing, the
    /// watched file's edit, atomic replace, and deletion signal, and a
    /// neighbor's edits stay quiet.
    #[test]
    #[cfg(target_os = "linux")]
    fn the_script_signal_gates_on_the_file_and_its_replaces() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let file = dir.path().join("prog.ds");
        std::fs::write(&file, "one").expect("the source writes");
        let mut watch = FsWatch::script(&file);
        assert!(!watch.wait(TICK), "a quiet window signals nothing");
        std::fs::write(dir.path().join("other.ds"), "x").expect("the neighbor writes");
        assert!(!watch.wait(TICK), "a neighbor's edit stays quiet");
        std::fs::write(&file, "two").expect("the edit writes");
        assert!(watch.wait(TICK), "the watched file's edit signals");
        // the editor's atomic replace: a sibling renamed over the file
        let swap = dir.path().join("prog.ds~");
        std::fs::write(&swap, "three").expect("the swap source writes");
        std::fs::rename(&swap, &file).expect("the rename lands");
        assert!(watch.wait(TICK), "the atomic replace signals");
        std::fs::remove_file(&file).expect("the delete lands");
        assert!(watch.wait(TICK), "the delete signals");
    }

    /// The tree-shaped signal: every directory below the root watches
    /// from the start (a nested member included), the manifest beside
    /// the root counts, and a subdirectory created after the start
    /// joins with its own members.
    #[test]
    #[cfg(target_os = "linux")]
    fn the_tree_signal_grows_with_new_subdirectories() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let nested = dir.path().join("nested");
        std::fs::create_dir_all(&nested).expect("the nested dir creates");
        std::fs::write(nested.join("deep.ds"), "one").expect("the deep member writes");
        let mut watch = FsWatch::trees(&[dir.path().to_path_buf()]);
        assert!(!watch.wait(TICK), "a quiet window signals nothing");
        std::fs::write(nested.join("deep.ds"), "two").expect("the deep member edits");
        assert!(watch.wait(TICK), "the nested member's edit signals");
        std::fs::write(dir.path().join("ds.pkg"), "name = \"app\"").expect("the manifest writes");
        assert!(watch.wait(TICK), "the manifest's edit signals");
        let fresh = dir.path().join("fresh");
        std::fs::create_dir(&fresh).expect("the fresh dir creates");
        assert!(watch.wait(TICK), "the fresh directory's creation signals");
        std::fs::write(fresh.join("member.ds"), "two").expect("the fresh member writes");
        assert!(watch.wait(TICK), "the grown watch sees the new member");
    }

    /// A watched directory deleted below the root degrades to reading
    /// every window until its path returns, and the returned tree is
    /// watched again — never worse than the poll twin.
    #[test]
    #[cfg(target_os = "linux")]
    fn a_deleted_directory_degrades_and_rewatches_on_return() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let leaf = dir.path().join("leaf");
        std::fs::create_dir(&leaf).expect("the leaf dir creates");
        std::fs::write(leaf.join("member.ds"), "one").expect("the member writes");
        let mut watch = FsWatch::trees(&[dir.path().to_path_buf()]);
        assert!(!watch.wait(TICK), "a quiet window signals nothing");
        std::fs::remove_dir_all(&leaf).expect("the leaf deletes");
        assert!(watch.wait(TICK), "the leaf's deletion signals");
        assert!(watch.wait(TICK), "the gone directory's window reads");
        std::fs::create_dir(&leaf).expect("the leaf returns");
        std::fs::write(leaf.join("member.ds"), "two").expect("the returned member writes");
        assert!(watch.wait(TICK), "the return signals");
        // one settled window proves the rewatch lives without the
        // gone-latch's permanent read
        assert!(!watch.wait(TICK), "the settled window is quiet");
        std::fs::write(leaf.join("member.ds"), "three").expect("the rewatched member edits");
        assert!(watch.wait(TICK), "the rewatched member's edit signals");
    }
}
