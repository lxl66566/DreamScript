//! Two-word entry descriptors for cross-tier and cross-module calls (H2).

use std::sync::atomic::{AtomicUsize, Ordering};

/// The tier state of one function entry. The flip is one-way: a compiled
/// entry never returns to interpreted. A recompiled function (hot reload)
/// gets a fresh [`EntryDescriptor`] in the second module slot instead of a
/// backward flip (H6 item 5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tier {
    /// Only word 0 (`tier1_prologue_ptr`) is valid; calls enter tier 1.
    Interpreted,
    /// Word 1 (`tier2_code_ptr`) holds compiled code; new calls enter it.
    Compiled,
}

/// Refusal of an illegal word-1 flip.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TierFlipError {
    /// Word 1 already holds compiled code; the flip is one-way.
    AlreadyCompiled,
    /// 0 is the null sentinel and never a valid code pointer.
    NullCodePointer,
}

/// The two-word entry descriptor of a function (H2):
/// `{tier1_prologue_ptr, tier2_code_ptr}`.
///
/// Flip protocol — who writes which word, and the ordering that keeps a
/// tier-1 frame from ever observing a torn descriptor:
///
/// - The module loader writes word 0 (`tier1_prologue`) once, before the descriptor becomes
///   reachable through the module table. Word 0 is immutable for the descriptor's lifetime, so
///   reads of it need no synchronization beyond the publication of the table itself.
/// - The JIT thread writes word 1 (`tier2_code`) exactly once, via
///   [`try_compile`](Self::try_compile): a compare-and-swap from the null sentinel 0 to the code
///   pointer with [`Ordering::Release`], issued only after the compiled code and its patched holes
///   are fully initialized.
/// - Call sites load word 1 with [`Ordering::Acquire`] ([`resolve`](Self::resolve)): they see
///   either the null sentinel (enter through word 0) or a fully initialized tier-2 entry. A torn
///   descriptor is unobservable because each word is an aligned machine word flipped monotonically
///   (0 to non-null, never back), and the two words are read independently — word 0 never changes,
///   so no read can straddle a state change of both words.
/// - Already-running tier-1 frames keep interpreting the old code; the flip affects subsequent
///   calls only. In-frame tier transitions are in-place J-variant opcode rewrites (H1), which are
///   `ds-jit`'s concern, not this descriptor's.
///
/// Cross-module calls resolve through the two-version module slot table
/// (H6 item 5): a hot reload publishes a fresh descriptor (fresh word 0) in
/// the second slot, and frames still on the old version retire at their
/// next cross-module call — the same invalidation story as the tier flip.
#[derive(Debug)]
pub struct EntryDescriptor {
    tier1_prologue: AtomicUsize,
    tier2_code: AtomicUsize,
}

impl EntryDescriptor {
    /// Creates a descriptor for a callee that has only tier-1 code.
    #[must_use]
    pub fn new_interpreted(tier1_prologue: usize) -> Self {
        Self {
            tier1_prologue: AtomicUsize::new(tier1_prologue),
            tier2_code: AtomicUsize::new(0),
        }
    }

    /// Word 0. Loaded `Relaxed` because the value is immutable after the
    /// loader publishes it (see the flip protocol above).
    #[must_use]
    pub fn tier1_prologue(&self) -> usize {
        self.tier1_prologue.load(Ordering::Relaxed)
    }

    /// Flips word 1 from the null sentinel to `tier2_code` (one-way). The
    /// `Release` ordering publishes the fully initialized compiled code;
    /// readers pair it with `Acquire` in [`resolve`](Self::resolve).
    pub fn try_compile(&self, tier2_code: usize) -> Result<(), TierFlipError> {
        if tier2_code == 0 {
            return Err(TierFlipError::NullCodePointer);
        }
        self.tier2_code
            .compare_exchange(0, tier2_code, Ordering::Release, Ordering::Relaxed)
            .map(|_| ())
            .map_err(|_| TierFlipError::AlreadyCompiled)
    }

    /// Word 1: the tier-2 code pointer, or 0 while interpreted.
    #[must_use]
    pub fn tier2_code(&self) -> usize {
        self.tier2_code.load(Ordering::Acquire)
    }

    /// The current tier of this entry.
    #[must_use]
    pub fn tier(&self) -> Tier {
        if self.tier2_code() == 0 { Tier::Interpreted } else { Tier::Compiled }
    }

    /// The call-site fast path: the tier-2 code pointer once compiled,
    /// otherwise the tier-1 prologue.
    #[must_use]
    pub fn resolve(&self) -> usize {
        let tier2 = self.tier2_code();
        if tier2 != 0 { tier2 } else { self.tier1_prologue() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tier_flips_one_way() {
        let d = EntryDescriptor::new_interpreted(0x1000);
        assert_eq!(d.tier(), Tier::Interpreted);
        assert_eq!(d.resolve(), 0x1000);
        assert_eq!(d.try_compile(0), Err(TierFlipError::NullCodePointer));
        d.try_compile(0x2000).expect("first compile flips");
        assert_eq!(d.tier2_code(), 0x2000);
        assert_eq!(d.tier(), Tier::Compiled);
        assert_eq!(d.resolve(), 0x2000);
        assert_eq!(d.try_compile(0x3000), Err(TierFlipError::AlreadyCompiled));
        assert_eq!(d.resolve(), 0x2000);
    }
}
