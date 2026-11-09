//! Restore-on-every-pop-path guards [C4/H4].

use core::mem;

use crate::{Arena, Mark};

/// Guard that restores an arena mark when dropped, so a frame's temporaries die
/// on every pop path - normal return, early return, and panic unwinding.
///
/// [`Arena::frame`] is the per-entry constructor: mark at entry, restore at
/// exit. [`Arena::scope`] is the closure form. [`RestoreOnDrop::pop`] restores
/// immediately without waiting for the drop.
pub struct RestoreOnDrop<'a> {
    arena: &'a Arena,
    mark: Mark,
}

impl<'a> RestoreOnDrop<'a> {
    /// Takes a mark now and restores it on drop.
    #[must_use]
    pub fn new(arena: &'a Arena) -> Self {
        Self {
            arena,
            mark: arena.mark(),
        }
    }

    /// The mark this guard will restore to.
    #[must_use]
    pub fn mark(&self) -> Mark {
        self.mark
    }

    /// Restores now and disarms the guard.
    pub fn pop(self) {
        let Self { arena, mark } = self;
        mem::forget(self);
        arena.restore(mark);
    }
}

impl Drop for RestoreOnDrop<'_> {
    fn drop(&mut self) {
        self.arena.restore(self.mark);
    }
}

impl Arena {
    /// Per-entry RAII mark: allocates are frame-local until the guard drops
    /// (or [`RestoreOnDrop::pop`]s), on every exit path including unwinding.
    #[must_use]
    pub fn frame(&self) -> RestoreOnDrop<'_> {
        RestoreOnDrop::new(self)
    }

    /// Runs `body` with a mark taken now and restored after it returns or
    /// unwinds.
    pub fn scope<R>(&self, body: impl FnOnce(&Arena) -> R) -> R {
        let _guard = self.frame();
        body(self)
    }
}
