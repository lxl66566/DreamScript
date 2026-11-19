//! Dense-id tables: an id's value is its slot in the table that allocated it.
//!
//! [`crate::Decls`] keys most of its tables by such ids, so a lookup is a
//! bounds check and an index. Insertion grows the table to the key's own
//! slot, an empty slot reads as absent, and the universe keeps minting
//! atoms after the declaration pass, so lookups ask for ids no declaration
//! ever wrote.
//!
//! A table is two-tier: an optional shared base plus per-compile growth.
//! A checked-prefix serve installs its rows as one shared base (an
//! [`Arc`] around a base-only table, parked per process behind a
//! [`Park`](crate::Park) cell by the caller that serves repeatedly), and
//! every later insert — a compiled file's own rows — lands in the growth tier, so a compile's
//! teardown drops the growth and a refcount instead of the served rows. The base is a
//! leading run of the id space (a served prefix precedes the fresh
//! files), and the reserve arms never insert a served id, so iteration
//! stays ascending in id order and no growth row shadows a base row.

use std::sync::Arc;

/// An id whose value indexes the table that allocated it, in declaration
/// order.
pub trait DenseId: Copy {
    /// Slot the id names.
    fn slot(self) -> usize;
}

/// A table over dense [`DenseId`] keys.
///
/// Iteration walks the occupied slots in id order: the base tier's rows
/// first (the leading id run), then the growth tier's. A key that was
/// never inserted reads as absent even when its slot lies inside the
/// table.
pub struct IdTable<K: DenseId, V> {
    slots: Vec<Option<(K, V)>>,
    base: Option<Arc<Self>>,
    occupied: usize,
}

impl<K: DenseId, V> Default for IdTable<K, V> {
    fn default() -> Self {
        Self {
            slots: Vec::new(),
            base: None,
            occupied: 0,
        }
    }
}

impl<K: DenseId, V> IdTable<K, V> {
    /// The value written at `key`'s slot: the growth tier answers first,
    /// the shared base below it.
    #[must_use]
    pub fn get(&self, key: &K) -> Option<&V> {
        if let Some(slot) = self.slots.get(key.slot()).and_then(|s| s.as_ref()) {
            return Some(&slot.1);
        }
        self.base.as_ref()?.get(key)
    }

    /// Whether `key`'s slot holds a value.
    #[must_use]
    pub fn contains_key(&self, key: &K) -> bool {
        self.get(key).is_some()
    }

    /// Writes `key`'s slot, growing the growth tier to reach it. A base
    /// row at the same slot would be shadowed — the declaration arms
    /// never insert a served id, so this is unreachable by construction.
    pub fn insert(&mut self, key: K, value: V) {
        let slot = key.slot();
        if slot >= self.slots.len() {
            self.slots.resize_with(slot + 1, || None);
        }
        if self.slots[slot].is_none() {
            self.occupied += 1;
        }
        self.slots[slot] = Some((key, value));
    }

    /// Installs a served prefix's rows as this table's shared base.
    pub fn install_base(&mut self, base: Arc<Self>) {
        self.base = Some(base);
    }

    /// Number of occupied slots across both tiers.
    #[must_use]
    pub fn len(&self) -> usize {
        self.occupied + self.base.as_ref().map_or(0, |base| base.len())
    }

    /// Whether no slot is occupied.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Walks the occupied slots in id order: the base tier, then the
    /// growth tier.
    pub fn iter(&self) -> std::iter::Chain<Occupied<'_, K, V>, Occupied<'_, K, V>> {
        let empty: &[Option<(K, V)>] = &[];
        let base = self.base.as_deref().map_or(
            Occupied {
                slots: empty.iter(),
            },
            |base| Occupied {
                slots: base.slots.iter(),
            },
        );
        base.chain(Occupied {
            slots: self.slots.iter(),
        })
    }
}

impl<'a, K: DenseId, V> IntoIterator for &'a IdTable<K, V> {
    type IntoIter = std::iter::Chain<Occupied<'a, K, V>, Occupied<'a, K, V>>;
    type Item = (&'a K, &'a V);

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

/// Walks one tier's occupied slots in id order.
pub struct Occupied<'a, K, V> {
    slots: std::slice::Iter<'a, Option<(K, V)>>,
}

impl<'a, K, V> Iterator for Occupied<'a, K, V> {
    type Item = (&'a K, &'a V);

    fn next(&mut self) -> Option<Self::Item> {
        let slot: &'a (K, V) = self.slots.by_ref().flatten().next()?;
        let (key, value) = slot;
        Some((key, value))
    }
}

impl DenseId for crate::atom::TypeId {
    fn slot(self) -> usize {
        self.index() as usize
    }
}

impl DenseId for crate::atom::TraitId {
    fn slot(self) -> usize {
        self.index() as usize
    }
}

impl DenseId for crate::resolve::DefId {
    fn slot(self) -> usize {
        self.index() as usize
    }
}

impl DenseId for crate::resolve::MethodId {
    fn slot(self) -> usize {
        self.index() as usize
    }
}

impl DenseId for crate::instance::InstanceId {
    fn slot(self) -> usize {
        self.index() as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolve::DefId;

    #[test]
    fn a_sparse_insert_leaves_the_slots_below_it_absent() {
        let mut table = IdTable::<DefId, u32>::default();
        table.insert(DefId::from_raw(3), 7);
        assert_eq!(table.get(&DefId::from_raw(0)), None);
        assert_eq!(table.get(&DefId::from_raw(3)), Some(&7));
        assert!(!table.contains_key(&DefId::from_raw(2)));
        assert_eq!(table.len(), 1);
    }

    #[test]
    fn iteration_visits_the_occupied_slots_in_id_order() {
        let mut table = IdTable::<DefId, u32>::default();
        table.insert(DefId::from_raw(2), 20);
        table.insert(DefId::from_raw(0), 10);
        table.insert(DefId::from_raw(0), 11);
        let seen: Vec<(u32, u32)> = (&table)
            .into_iter()
            .map(|(def, value)| (def.index(), *value))
            .collect();
        assert_eq!(seen, vec![(0, 11), (2, 20)]);
        assert_eq!(table.len(), 2);
    }

    /// A shared base answers the served ids, the growth tier answers the
    /// compiled file's own, and iteration walks both tiers in id order.
    #[test]
    fn a_shared_base_answers_reads_below_the_growth_tier() {
        let mut base = IdTable::<DefId, u32>::default();
        base.insert(DefId::from_raw(0), 10);
        base.insert(DefId::from_raw(1), 11);
        let mut table = IdTable::<DefId, u32>::default();
        table.install_base(Arc::new(base));
        table.insert(DefId::from_raw(3), 30);
        assert_eq!(table.get(&DefId::from_raw(0)), Some(&10));
        assert_eq!(table.get(&DefId::from_raw(1)), Some(&11));
        assert_eq!(table.get(&DefId::from_raw(2)), None);
        assert_eq!(table.get(&DefId::from_raw(3)), Some(&30));
        assert_eq!(table.len(), 3);
        let seen: Vec<(u32, u32)> = (&table)
            .into_iter()
            .map(|(def, value)| (def.index(), *value))
            .collect();
        assert_eq!(seen, vec![(0, 10), (1, 11), (3, 30)]);
    }
}
