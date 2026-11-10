//! A binder map with O(1) snapshot for nested checking scopes.
//!
//! The body checker brackets every block, arm, and lambda boundary with a
//! snapshot/restore of its `locals`/`refined` tables. Cloning the tables
//! at each boundary was the checker's dominant allocation; here a
//! snapshot is the undo log's length (a `usize`), and restoring replays
//! only what the scope changed. The trick that keeps `clear` O(1) too:
//! entries remember the epoch they were written at, and a clear bumps the
//! epoch so pre-clear entries read back as absent — restoring the epoch
//! revives them, while the logged puts/removals of the cleared scope are
//! replayed first, so the table is exactly what it was at the mark.

use std::collections::HashMap;

use ds_diag::Span;

/// One logged mutation: what a put overwrote, or a clear's prior epoch.
enum Undo<V> {
    Put { key: Span, old: Option<(V, u32)> },
    UnClear { epoch: u32 },
}

/// A `Span -> V` map whose changes can be rolled back to a mark.
pub(crate) struct ScopedMap<V> {
    entries: HashMap<Span, (V, u32)>,
    epoch: u32,
    undo: Vec<Undo<V>>,
}

impl<V> Default for ScopedMap<V> {
    fn default() -> Self {
        Self {
            entries: HashMap::new(),
            epoch: 0,
            undo: Vec::new(),
        }
    }
}

impl<V> ScopedMap<V> {
    /// The value at a live key; entries from before the last clear are
    /// logically absent.
    pub(crate) fn get(&self, key: Span) -> Option<&V> {
        self.entries
            .get(&key)
            .filter(|(_, ver)| *ver >= self.epoch)
            .map(|(value, _)| value)
    }

    pub(crate) fn insert(&mut self, key: Span, value: V) {
        let old = self.entries.insert(key, (value, self.epoch));
        self.undo.push(Undo::Put { key, old });
    }

    pub(crate) fn remove(&mut self, key: Span) {
        if let Some(old) = self.entries.remove(&key) {
            self.undo.push(Undo::Put {
                key,
                old: Some(old),
            });
        }
    }

    /// Logically empties the map without touching the entries.
    pub(crate) fn clear(&mut self) {
        self.epoch += 1;
        self.undo.push(Undo::UnClear {
            epoch: self.epoch - 1,
        });
    }

    /// The rollback point: the undo log's current length.
    pub(crate) fn mark(&self) -> usize {
        self.undo.len()
    }

    /// Replays the log back to a mark, in reverse. Marks nest (a scope's
    /// mark is only ever restored before any enclosing mark), which the
    /// checker's lexical snapshot/restore pairing guarantees.
    pub(crate) fn restore_to(&mut self, mark: usize) {
        while self.undo.len() > mark {
            match self.undo.pop().expect("mark longer than the log") {
                Undo::Put { key, old } => match old {
                    Some(pair) => {
                        self.entries.insert(key, pair);
                    },
                    None => {
                        self.entries.remove(&key);
                    },
                },
                Undo::UnClear { epoch } => self.epoch = epoch,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(n: u32) -> Span {
        Span::new(ds_diag::FileId::new(0), n, n)
    }

    #[test]
    fn put_and_restore_nested_marks() {
        let mut map = ScopedMap::default();
        map.insert(span(1), 10);
        let outer = map.mark();
        map.insert(span(2), 20);
        let inner = map.mark();
        map.insert(span(1), 11);
        assert_eq!(map.get(span(1)), Some(&11));
        map.restore_to(inner);
        assert_eq!(map.get(span(1)), Some(&10), "the inner put rolls back");
        assert_eq!(map.get(span(2)), Some(&20));
        map.restore_to(outer);
        assert_eq!(map.get(span(1)), Some(&10));
        assert_eq!(map.get(span(2)), None, "the outer put rolls back too");
    }

    #[test]
    fn remove_rolls_back_to_its_mark() {
        let mut map = ScopedMap::default();
        map.insert(span(1), 10);
        let mark = map.mark();
        map.remove(span(1));
        assert_eq!(map.get(span(1)), None);
        map.restore_to(mark);
        assert_eq!(map.get(span(1)), Some(&10));
    }

    #[test]
    fn clear_hides_then_restore_revives() {
        let mut map = ScopedMap::default();
        map.insert(span(1), 10);
        map.insert(span(2), 20);
        let mark = map.mark();
        map.clear();
        assert_eq!(map.get(span(1)), None);
        assert_eq!(map.get(span(2)), None);
        // writes after the clear are ordinary logged puts
        map.insert(span(1), 11);
        map.insert(span(3), 30);
        assert_eq!(map.get(span(1)), Some(&11));
        assert_eq!(map.get(span(3)), Some(&30));
        map.restore_to(mark);
        assert_eq!(map.get(span(1)), Some(&10), "the pre-clear value returns");
        assert_eq!(map.get(span(2)), Some(&20));
        assert_eq!(map.get(span(3)), None);
    }

    #[test]
    fn clear_inside_an_outer_clear_restores_both_epochs() {
        let mut map = ScopedMap::default();
        map.insert(span(1), 10);
        let mark = map.mark();
        map.clear();
        map.insert(span(2), 20);
        map.clear();
        assert_eq!(map.get(span(1)), None);
        assert_eq!(map.get(span(2)), None);
        map.restore_to(mark);
        assert_eq!(map.get(span(1)), Some(&10));
        assert_eq!(map.get(span(2)), None, "the middle scope's put is undone");
    }
}
