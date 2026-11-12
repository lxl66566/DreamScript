//! Dense-id tables: an id's value is its slot in the table that allocated it.
//!
//! [`crate::Decls`] keys most of its tables by such ids, so a lookup is a
//! bounds check and an index. Insertion grows the table to the key's own
//! slot, an empty slot reads as absent, and the universe keeps minting
//! atoms after the declaration pass, so lookups ask for ids no declaration
//! ever wrote.

/// An id whose value indexes the table that allocated it, in declaration
/// order.
pub trait DenseId: Copy {
    /// Slot the id names.
    fn slot(self) -> usize;
}

/// A table over dense [`DenseId`] keys.
///
/// Iteration walks the occupied slots in id order. A key that was never
/// inserted reads as absent even when its slot lies inside the table.
pub struct IdTable<K: DenseId, V> {
    slots: Vec<Option<(K, V)>>,
    occupied: usize,
}

impl<K: DenseId, V> Default for IdTable<K, V> {
    fn default() -> Self {
        Self {
            slots: Vec::new(),
            occupied: 0,
        }
    }
}

impl<K: DenseId, V> IdTable<K, V> {
    /// The value written at `key`'s slot.
    #[must_use]
    pub fn get(&self, key: &K) -> Option<&V> {
        let slot = self.slots.get(key.slot())?.as_ref()?;
        Some(&slot.1)
    }

    /// Whether `key`'s slot holds a value.
    #[must_use]
    pub fn contains_key(&self, key: &K) -> bool {
        self.get(key).is_some()
    }

    /// Writes `key`'s slot, growing the table to reach it.
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

    /// Number of occupied slots.
    #[must_use]
    pub fn len(&self) -> usize {
        self.occupied
    }

    /// Whether no slot is occupied.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.occupied == 0
    }

    /// Walks the occupied slots in id order.
    #[must_use]
    pub fn iter(&self) -> Occupied<'_, K, V> {
        Occupied {
            slots: self.slots.iter(),
        }
    }
}

impl<'a, K: DenseId, V> IntoIterator for &'a IdTable<K, V> {
    type IntoIter = Occupied<'a, K, V>;
    type Item = (&'a K, &'a V);

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

/// Walks one table's occupied slots in id order.
pub struct Occupied<'a, K, V> {
    slots: std::slice::Iter<'a, Option<(K, V)>>,
}

impl<'a, K: DenseId, V> Iterator for Occupied<'a, K, V> {
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
}
