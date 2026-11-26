//! Once-per-process parks of a serve's shared rows.
//!
//! A process that runs one serve repeatedly (a package graph's member
//! compiles, a query session) pays for the serve's shared rows once:
//! the first pass that builds them — the serve conversion, or the
//! first member compile's walk — fills one [`Park`] cell, and every
//! later compile clones the rows by refcount instead of rebuilding
//! them. A park spells the cell plus one install line at the serve (or
//! inside the consuming pass): the installs differ per target (a
//! table's shared base, a package's base tier, a refcounted row set),
//! the cell is the shared shape. A decode or blob cache of immutable
//! input is a plain `OnceLock`, outside this family: a park holds
//! native rows a member compile reads and grows over.
//!
//! Every park answers the same two soundness rules:
//! * the rows may name only identities every serve mints identically (definition and method ids) —
//!   a minted atom shifts with the compiled files' mint landings, so a shared row naming one would
//!   address an id no later compile reproduces; and
//! * the rows live in process memory only, rebuilt by whatever process runs the compile and never
//!   serialized — no checked body fact rides an artifact.

use std::sync::OnceLock;

/// One serve's shared rows, parked once per process: filled either at
/// the serve conversion ([`Park::get_or_init`]) or by the first
/// consumer's walk ([`Park::fill`]), read by every later one.
pub struct Park<T> {
    cell: OnceLock<T>,
}

impl<T> Park<T> {
    /// An empty park: no rows built yet.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            cell: OnceLock::new(),
        }
    }

    /// The parked rows, building them on the first call: the
    /// conversion-time fill, the serve's conversion running once per
    /// process. A failed build parks with the rows, answered to every
    /// later caller the same way (a drifted artifact reports its miss
    /// once, not per serve).
    pub fn get_or_init(&self, build: impl FnOnce() -> T) -> &T {
        self.cell.get_or_init(build)
    }

    /// The parked rows when an earlier caller filled them: `None`
    /// answers an unfilled park, the caller walking the served files
    /// itself this once.
    #[must_use]
    pub fn get(&self) -> Option<&T> {
        self.cell.get()
    }

    /// The fill-later park's write: the first consumer that walks the
    /// served files hands its rows here, every later one installing
    /// them instead. The rows drop when a concurrent fill won the race
    /// (one walk's rows park, whichever set them first).
    pub fn fill(&self, rows: T) -> Result<(), T> {
        self.cell.set(rows)
    }
}

impl<T> Default for Park<T> {
    fn default() -> Self {
        Self::new()
    }
}
