//! Map payloads: key-class hashes, the row index, and probe resolution.

use super::*;

impl MapData {
    /// Records the row just pushed into `keys`, extending the index to
    /// the next power of two when the load factor would pass one half.
    pub(super) fn index_push(&mut self) {
        let rows = self.keys.len();
        let capacity = if self.slots.len() >= rows * 2 {
            self.slots.len()
        } else {
            (rows * 2).next_power_of_two().max(8)
        };
        if capacity != self.slots.len() {
            self.slots.clear();
            self.slots.resize(capacity, ROW_NONE);
            // a rebuilt index re-places every row, the new one included
            for row in 0..self.hashes.len() {
                let hash = self.hashes[row];
                self.place(row, hash);
            }
            return;
        }
        self.place(rows - 1, self.hashes[rows - 1]);
    }

    /// Claims the first free slot at or past the hash of row `row`.
    pub(super) fn place(&mut self, row: usize, hash: u32) {
        let row = u32::try_from(row).expect("a map row fits the index word");
        let mask = self.slots.len() - 1;
        let mut at = hash as usize & mask;
        while self.slots[at] != ROW_NONE {
            at = (at + 1) & mask;
        }
        self.slots[at] = row;
    }

    /// Re-places every row after a removal shifted the row numbers: the
    /// index is positional, so the cleared rebuild is the removal's own
    /// bookkeeping. The capacity rule stays satisfied (rows only
    /// shrank), so the capacity is kept as is.
    pub(super) fn index_rebuild(&mut self) {
        self.slots.fill(ROW_NONE);
        for row in 0..self.keys.len() {
            self.place(row, self.hashes[row]);
        }
    }
}

/// FNV-1a over `bytes`, continuing `state` (one round per byte).
#[inline]
pub(super) const fn fnv_step(mut state: u32, bytes: &[u8]) -> u32 {
    let mut at = 0;
    while at < bytes.len() {
        state ^= bytes[at] as u32;
        state = state.wrapping_mul(0x0100_0193);
        at += 1;
    }
    state
}

/// A key class's FNV seed: the offset basis folded once with the class
/// byte, so the classes stay apart without a round over that byte.
pub(super) const fn class_seed(class: u8) -> u32 {
    (0x811c_9dc5 ^ class as u32).wrapping_mul(0x0100_0193)
}

/// Key equality classes, one seed each: a text hash, an identity hash, and
/// a scalar hash over the same bytes stay distinguishable.
pub(super) const SEED_TEXT: u32 = class_seed(b't');

pub(super) const SEED_REF: u32 = class_seed(b'r');

pub(super) const SEED_SCALAR: u32 = class_seed(b's');

/// The float scalar class: a float key never equals an int key, so the two
/// kinds never share a hash.
pub(super) const SEED_SCALAR_F64: u32 = class_seed(b'd');

/// The hash of a string key: its content, the text equality class.
pub(super) fn text_hash(text: &str) -> u32 {
    fnv_step(SEED_TEXT, text.as_bytes())
}

/// The hash of an identity key: the reference word's own bits, one word.
pub(super) fn ref_hash(word: RefId) -> u32 {
    let (fiber, id) = (word.fiber().0.to_le_bytes(), word.id().to_le_bytes());
    let mut bytes = [0; 8];
    bytes[..4].copy_from_slice(&fiber);
    bytes[4..].copy_from_slice(&id);
    fnv_step(SEED_REF, &bytes)
}

/// The one word-equality judge the structural `==` family rides:
/// floats compare bit-structurally (reflexive, total, `+0.0` and `-0.0`
/// distinct), every other word by its derived equality. Collection
/// content equality and map key matching answer through this judge, the
/// same one the scalar `==`/`in` chain serves.
pub(super) fn word_eq(a: Value, b: Value) -> bool {
    match (a, b) {
        (Value::F64(x), Value::F64(y)) => x.to_bits() == y.to_bits(),
        _ => a == b,
    }
}

/// The hash of a scalar key: bits that respect word equality — the float
/// word hashes its own bits, so word-equal (bit-structural) floats hash
/// equal and the filter never rejects an equal pair, while `+0.0` and
/// `-0.0` hash apart as the distinct keys they are (every other scalar
/// kind is bits-equal exactly when word-equal; a NaN key matches itself
/// alone, so its payload hashing with the bits is free).
pub(super) fn scalar_hash(word: Value) -> u32 {
    // every scalar word is one 8-byte payload, so the class seed alone
    // carries the kind distinction
    let (payload, seed) = match word {
        Value::I64(n) => (n.to_le_bytes(), SEED_SCALAR),
        Value::F64(x) => (x.to_bits().to_le_bytes(), SEED_SCALAR_F64),
        Value::Bool(b) => (i64::from(b).to_le_bytes(), SEED_SCALAR),
        Value::Tag(t) => (u64::from(t).to_le_bytes(), SEED_SCALAR),
        Value::Arena(h) => (h.raw().to_le_bytes(), SEED_SCALAR),
        Value::Fn(f) => {
            let (module, func) = (f.module().to_le_bytes(), f.func().to_le_bytes());
            let mut bytes = [0; 8];
            bytes[..4].copy_from_slice(&module);
            bytes[4..].copy_from_slice(&func);
            (bytes, SEED_SCALAR)
        },
        // the probe resolution routes a Ref to the text or identity class
        // first; hashing its bits keeps this total for the dead case
        Value::Ref(r) => {
            let (fiber, id) = (r.fiber().0.to_le_bytes(), r.id().to_le_bytes());
            let mut bytes = [0; 8];
            bytes[..4].copy_from_slice(&fiber);
            bytes[4..].copy_from_slice(&id);
            (bytes, SEED_SCALAR)
        },
    };
    fnv_step(seed, &payload)
}

/// A map probe resolved once per lookup or store — the payload-local
/// form the row scan compares against. The probe word's payload is read
/// exactly here (a local word through this store, a foreign word through
/// the router's pre-resolution), so the per-candidate loop never walks
/// the probe's descriptor chain again; the classes mirror the key
/// equality rules exactly.
pub(super) enum ProbeKey<'a> {
    /// A string probe: content equality, the text borrowed from its
    /// object (local) or the router's copy (foreign).
    Text { hash: u32, text: &'a str },
    /// A non-string reference probe: identity equality.
    Identity { hash: u32, word: RefId },
    /// A probe carrying no reference: word equality.
    Scalar { hash: u32, word: Value },
}

impl ProbeKey<'_> {
    /// The row filter hash: equal keys hash equal in every class, so a
    /// mismatch is a certain reject.
    pub(super) fn hash(&self) -> u32 {
        match *self {
            Self::Text { hash, .. } | Self::Identity { hash, .. } | Self::Scalar { hash, .. } => {
                hash
            },
        }
    }
}

impl Objects {
    /// Allocates a fresh empty map object, owned by the caller.
    pub fn alloc_map(&mut self) -> u32 {
        self.alloc(Object::Map(MapData::default()))
    }

    /// Resolves a probe word once per lookup or store, payload-locally:
    /// a local word resolves through this store, a foreign word through
    /// the router's pre-resolution (content and hash carried off the
    /// probe's own heap), a scalar word needs no payload at all. The scan
    /// then never re-reads the probe's payload per candidate.
    pub(super) fn resolve_probe<'a>(
        &'a self,
        key: Value,
        res: &'a ResolvedKey<'a>,
    ) -> ProbeKey<'a> {
        match res {
            ResolvedKey::Text { text, hash } => ProbeKey::Text { hash: *hash, text },
            ResolvedKey::Opaque => match key {
                // the router builds Opaque only from a reference word
                Value::Ref(word) => ProbeKey::Identity {
                    hash: ref_hash(word),
                    word,
                },
                scalar => ProbeKey::Scalar {
                    hash: scalar_hash(scalar),
                    word: scalar,
                },
            },
            ResolvedKey::Word => match key {
                Value::Ref(word) => match self.payload_of(word.id()) {
                    Some(Object::Str(s)) => ProbeKey::Text {
                        hash: s.content_hash(),
                        text: &s.text,
                    },
                    _ => ProbeKey::Identity {
                        hash: ref_hash(word),
                        word,
                    },
                },
                scalar => ProbeKey::Scalar {
                    hash: scalar_hash(scalar),
                    word: scalar,
                },
            },
        }
    }

    /// Whether one stored key word equals the probe — the exact check a
    /// hash hit runs; the classes mirror the key equality rules with no
    /// probe payload read (the resolution already did it).
    pub(super) fn stored_key_matches(&self, stored: Value, probe: &ProbeKey<'_>) -> bool {
        match probe {
            ProbeKey::Text { text, .. } => match stored {
                // the probe names a string: only a stored string key can
                // equal it (mixed kinds never match), by content
                Value::Ref(a) => matches!(
                    self.payload_of(a.id()),
                    Some(Object::Str(s)) if &*s.text == *text
                ),
                _ => false,
            },
            ProbeKey::Identity { word, .. } => match stored {
                // the probe names no string, and a stored string key is a
                // different object by construction, so the word compare
                // answers without reading the candidate's payload
                Value::Ref(a) => a == *word,
                _ => false,
            },
            ProbeKey::Scalar { word, .. } => word_eq(stored, *word),
        }
    }

    /// The row whose key equals the probe, if present: the index narrows
    /// the candidates to the rows sharing the probe's filter hash, so a miss
    /// reads a few dense index words and the exact check runs on the hash
    /// hits alone.
    pub(super) fn map_row_at(&self, data: &MapData, probe: &ProbeKey<'_>) -> Option<usize> {
        if data.slots.is_empty() {
            return None;
        }
        let hash = probe.hash();
        let mask = data.slots.len() - 1;
        let mut at = hash as usize & mask;
        while data.slots[at] != ROW_NONE {
            let row = data.slots[at] as usize;
            if data.hashes[row] == hash && self.stored_key_matches(data.keys[row], probe) {
                return Some(row);
            }
            at = (at + 1) & mask;
        }
        None
    }

    /// Stores one key and its value span into a map object in place, the
    /// exclusive window's one mutation (the `map_set` native). The value
    /// is the value type's whole inline word span — one word for a
    /// scalar, a tag-plus-region span for a union value — and the first
    /// store onto an empty map fixes the value stride every later store
    /// must repeat. Every stored `Ref` word is acquired for the map
    /// before it enters a row, fresh or overwriting, so the caller's own
    /// count stays the caller's and a later death drops exactly what was
    /// stored. A content-equal key overwrites: the old value span's
    /// counts drop, and the row keeps its original key word (the map
    /// already owns that count; the caller keeps its own probe word), so
    /// the row index keeps its entry too. A fresh row's key must be a
    /// scalar or a word of the map's own heap: the content probe reads a
    /// stored key's payload on this store and ids are per-store, so a
    /// row under a foreign word would answer no content probe from any
    /// heap — the store refuses the insert before any count moves, while
    /// a content-equal foreign probe overwriting an existing row stays
    /// legal (the foreign word never lands).
    ///
    /// # Errors
    ///
    /// [`Trap::UnknownObject`] when the reference names no map object;
    /// [`Trap::ElementStride`] when the staged span's width disagrees
    /// with the non-empty map's value stride; [`Trap::ForeignMapKey`]
    /// when a fresh row's key word is foreign to this store.
    pub fn set_map(
        &mut self,
        obj: u32,
        key: Value,
        res: &ResolvedKey<'_>,
        value: &[Value],
    ) -> Result<(), Trap> {
        let header = match self.table.get(obj as usize) {
            Some(Slot::Live(header))
                if matches!(self.heap.payload::<Object>(*header), Object::Map(_)) =>
            {
                *header
            },
            _ => return Err(Trap::UnknownObject { obj }),
        };
        // one probe resolution serves the scan and the fresh row's hash;
        // the borrow ends before the mutation window opens
        let (found, hash, stride) = match self.payload_of(obj) {
            Some(Object::Map(data)) => {
                let probe = self.resolve_probe(key, res);
                (self.map_row_at(data, &probe), probe.hash(), data.vstride)
            },
            _ => return Err(Trap::UnknownObject { obj }),
        };
        if stride != 0 && stride != value.len() {
            return Err(Trap::ElementStride {
                obj,
                stride,
                given: value.len(),
            });
        }
        if let Some(at) = found {
            // the store acquires for the map first, whatever it replaces:
            // a value word arriving with its caller's count must not ride
            // that count into the row (a dying caller would free it)
            for word in value {
                if let Value::Ref(word) = *word {
                    self.acquire_stored(word);
                }
            }
            let old = match self.heap.payload_mut::<Object>(header) {
                Object::Map(data) => {
                    let at = at * data.vstride;
                    let old = data.values[at..at + value.len()].to_vec();
                    data.values[at..at + value.len()].copy_from_slice(value);
                    old
                },
                _ => return Err(Trap::UnknownObject { obj }),
            };
            for word in old {
                if let Value::Ref(word) = word {
                    self.release_stored(word);
                }
            }
            return Ok(());
        }
        // the fresh row's key must resolve on this store: a foreign word
        // refuses here, before any count moves, so the map is untouched
        // (a content-equal foreign probe found above overwrites instead). The
        // test sits at the store, not the router: only the store knows its own
        // owner, so every direct caller of this seam is covered.
        if let Value::Ref(word) = key
            && word.fiber() != self.owner
        {
            return Err(Trap::ForeignMapKey { obj, key: word });
        }
        // the acquires happen before the store: a bogus reference word
        // leaves the map untouched
        if let Value::Ref(word) = key {
            self.acquire_stored(word);
        }
        for word in value {
            if let Value::Ref(word) = *word {
                self.acquire_stored(word);
            }
        }
        match self.heap.payload_mut::<Object>(header) {
            Object::Map(data) => {
                if data.keys.is_empty() {
                    data.vstride = value.len();
                }
                data.hashes.push(hash);
                data.keys.push(key);
                data.values.extend_from_slice(value);
                data.index_push();
            },
            _ => return Err(Trap::UnknownObject { obj }),
        }
        Ok(())
    }

    /// The value span stored under `key` in map object `obj`, if present.
    /// The read is a lookup, not a hand-out: the map keeps every count it
    /// owns, so the caller that wants the value acquires its own.
    ///
    /// # Errors
    ///
    /// [`Trap::UnknownObject`] when the reference names no map object.
    pub fn map_at(
        &self,
        obj: u32,
        key: Value,
        res: &ResolvedKey<'_>,
    ) -> Result<Option<&[Value]>, Trap> {
        let Some(Object::Map(data)) = self.payload_of(obj) else {
            return Err(Trap::UnknownObject { obj });
        };
        // a rowless map has no candidate, so the probe needs no resolution
        if data.slots.is_empty() {
            return Ok(None);
        }
        let probe = self.resolve_probe(key, res);
        Ok(self.map_row_at(data, &probe).map(|at| {
            let at = at * data.vstride;
            &data.values[at..at + data.vstride]
        }))
    }

    /// The row count of map object `obj`.
    ///
    /// # Errors
    ///
    /// [`Trap::UnknownObject`] when the reference names no map object.
    pub fn map_len(&self, obj: u32) -> Result<usize, Trap> {
        match self.payload_of(obj) {
            Some(Object::Map(data)) => Ok(data.keys.len()),
            _ => Err(Trap::UnknownObject { obj }),
        }
    }

    /// The key words of map object `obj`, in entry order — the ordered
    /// key face (`map_keys`), the value spans read back through
    /// [`Objects::map_at`].
    ///
    /// # Errors
    ///
    /// [`Trap::UnknownObject`] when the reference names no map object.
    pub fn map_keys(&self, obj: u32) -> Result<&[Value], Trap> {
        match self.payload_of(obj) {
            Some(Object::Map(data)) => Ok(&data.keys),
            _ => Err(Trap::UnknownObject { obj }),
        }
    }

    /// Removes the row stored under `key`, when present: the removed
    /// key and value span release one count per `Ref` word (the map's
    /// rows own their counts, the `set` store's contract), and the row
    /// index rebuilds over the shifted row numbers. A missing key is a
    /// no-op — the removal has nothing to release, and the caller that
    /// needs to know composes `contains`/`get` first.
    ///
    /// # Errors
    ///
    /// [`Trap::UnknownObject`] when the reference names no map object.
    pub fn remove_map(&mut self, obj: u32, key: Value, res: &ResolvedKey<'_>) -> Result<(), Trap> {
        let header = self.header_of(obj)?;
        if !matches!(self.heap.payload::<Object>(header), Object::Map(_)) {
            return Err(Trap::UnknownObject { obj });
        }
        let at = match self.payload_of(obj) {
            Some(Object::Map(data)) if !data.slots.is_empty() => {
                let probe = self.resolve_probe(key, res);
                self.map_row_at(data, &probe)
            },
            _ => None,
        };
        let Some(at) = at else {
            return Ok(());
        };
        let (key, value) = match self.heap.payload_mut::<Object>(header) {
            Object::Map(data) => {
                let key = data.keys.remove(at);
                data.hashes.remove(at);
                let start = at * data.vstride;
                let value = data
                    .values
                    .drain(start..start + data.vstride)
                    .collect::<Vec<_>>();
                data.index_rebuild();
                (key, value)
            },
            _ => return Err(Trap::UnknownObject { obj }),
        };
        if let Value::Ref(word) = key {
            self.release_stored(word);
        }
        for value in value {
            if let Value::Ref(word) = value {
                self.release_stored(word);
            }
        }
        Ok(())
    }

    /// Whether a row is stored under `key` — the membership read, no
    /// value handed out.
    ///
    /// # Errors
    ///
    /// [`Trap::UnknownObject`] when the reference names no map object.
    pub fn contains_map(&self, obj: u32, key: Value, res: &ResolvedKey<'_>) -> Result<bool, Trap> {
        let Some(Object::Map(data)) = self.payload_of(obj) else {
            return Err(Trap::UnknownObject { obj });
        };
        if data.slots.is_empty() {
            return Ok(false);
        }
        let probe = self.resolve_probe(key, res);
        Ok(self.map_row_at(data, &probe).is_some())
    }

    /// The entry words of map object `obj`, in entry order — one key
    /// word then its value span per row, flattened — plus the per-entry
    /// stride (one word wider than the value stride; 0 for a rowless
    /// map, keeping the empty list's stride-0 invariant). The read is a
    /// copy, not a hand-out: the map keeps every count, the caller that
    /// adopts the words acquires its own.
    ///
    /// # Errors
    ///
    /// [`Trap::UnknownObject`] when the reference names no map object.
    pub fn map_entries(&self, obj: u32) -> Result<(Vec<Value>, usize), Trap> {
        let Some(Object::Map(data)) = self.payload_of(obj) else {
            return Err(Trap::UnknownObject { obj });
        };
        if data.keys.is_empty() {
            return Ok((Vec::new(), 0));
        }
        let mut words = Vec::with_capacity(data.keys.len() * (1 + data.vstride));
        for (row, key) in data.keys.iter().enumerate() {
            words.push(*key);
            words.extend_from_slice(&data.values[row * data.vstride..(row + 1) * data.vstride]);
        }
        Ok((words, 1 + data.vstride))
    }
}
