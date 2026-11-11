//! Per-fiber claim ledgers: the multiset of heap counts a fiber's frames
//! own at each moment — the substrate the teardown drain walk releases
//! (the cross-heap accounting channel of the counts-stay ruling).
//!
//! Every count a frame acquires enters its fiber's ledger; every release
//! (a frame's drop, a delivery to another fiber or the host) leaves it.
//! The ledger tracks counts, not frame slots: the placement pass moves
//! words between slots without touching counts (a dup pairs its `RCINC`
//! with a later `RCDEC`, a forward rides the callee's own exit drops),
//! so no dispatch site needs to know which staged copy anchors a claim.

use std::{
    collections::HashMap,
    hash::{BuildHasherDefault, Hasher},
};

use crate::value::RefId;

/// A one-multiply hasher: claim keys are single 64-bit word identities,
/// so ledger traffic stays a handful of naniseconds on the count
/// funnels.
#[derive(Default)]
struct WordHasher(u64);

impl Hasher for WordHasher {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.write_u64(u64::from(byte));
        }
    }

    fn write_u64(&mut self, n: u64) {
        self.0 = self.0.rotate_left(13) ^ n;
        self.0 = self.0.wrapping_mul(0x9e37_79b9_7f4a_7c15);
    }
}

type WordBuild = BuildHasherDefault<WordHasher>;

/// One fiber's claim ledger: each word with the multiplicity of counts
/// the fiber's frames own on it. Releasing a never-ledgered word (a
/// host-borrowed word a drive decs, a hand-assembled release with no
/// matching acquire) is a no-op: the ledger never goes negative, and the
/// store count is the mutation sites' business either way.
#[derive(Debug, Default)]
pub(crate) struct ClaimTable {
    counts: HashMap<RefId, u32, WordBuild>,
}

impl ClaimTable {
    /// One count the fiber's frames now own on `word`.
    pub(crate) fn acquire(&mut self, word: RefId) {
        *self.counts.entry(word).or_insert(0) += 1;
    }

    /// One count left the fiber's frames: a frame's drop, or a delivery
    /// to another fiber or the host.
    pub(crate) fn release(&mut self, word: RefId) {
        if let Some(n) = self.counts.get_mut(&word) {
            *n -= 1;
            if *n == 0 {
                self.counts.remove(&word);
            }
        }
    }

    /// The drain walk's input: every claim with its multiplicity, taken
    /// out of the ledger. `held` — a word about to ride its recorded
    /// outcome to a delivery — keeps its claim and stays.
    pub(crate) fn take_all_except(&mut self, held: Option<RefId>) -> Vec<(RefId, u32)> {
        let all = std::mem::take(&mut self.counts);
        let mut out = Vec::with_capacity(all.len());
        let mut keep = None;
        for (word, n) in all {
            if Some(word) == held {
                keep = Some((word, n));
            } else {
                out.push((word, n));
            }
        }
        if let Some((word, n)) = keep {
            self.counts.insert(word, n);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::ClaimTable;
    use crate::{fiber::FiberId, value::RefId};

    fn word(id: u32) -> RefId {
        RefId::new(FiberId(0), id)
    }

    #[test]
    fn acquire_release_balances_to_empty() {
        let mut table = ClaimTable::default();
        let w = word(3);
        table.acquire(w);
        table.acquire(w);
        table.acquire(word(7));
        table.release(w);
        let drained = table.take_all_except(None);
        assert_eq!(drained, vec![(w, 1), (word(7), 1)]);
        assert!(
            table.take_all_except(None).is_empty(),
            "a taken ledger drains to nothing"
        );
    }

    #[test]
    fn release_of_a_never_ledgered_word_is_a_no_op() {
        let mut table = ClaimTable::default();
        table.release(word(9));
        assert!(
            table.take_all_except(None).is_empty(),
            "a never-ledgered release leaves nothing"
        );
    }

    #[test]
    fn take_all_keeps_the_held_word() {
        let mut table = ClaimTable::default();
        let held = word(1);
        let gone = word(2);
        table.acquire(held);
        table.acquire(gone);
        let drained = table.take_all_except(Some(held));
        assert_eq!(drained, vec![(gone, 1)]);
        // the held claim survives a take
        table.release(held);
        assert!(
            table.take_all_except(None).is_empty(),
            "the held claim left with its release"
        );
    }
}
