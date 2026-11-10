//! The VM-side heap object store [BL-2]: the objects raw `Ref` words name.
//!
//! Strings and lists are counted heap blocks (C4): every object carries a
//! precise non-atomic reference count, RCINC/RCDEC move it, and a count
//! reaching zero frees the object — its slot enters a free list and the
//! next allocation claims it (the dec-to-alloc pairing runs inside this
//! table, so no alloc-at uop exists in the ISA). Free order is
//! deterministic: drops happen in program order and the free list is
//! last-freed-first-reused.
//!
//! A list is one object with its element words inline — the ruled shape;
//! see [`Object::List`]. The element words are plain [`Value`]s, so one
//! kind serves every element type. Ownership of `Ref` elements is exact:
//! storing one into a list (the `list_push` construction append, the
//! `list_concat` shared-element copies, the `list_get` checked read-out)
//! acquires a count, and freeing a list drops one count per `Ref`
//! element it holds. Producer natives that build lists of fresh objects
//! (`split`, `fs_list`) store without acquiring — the fresh object's
//! birth count is the list's. String interning acquires too: a `KSTR`
//! load claims a count of the interned object, and a dying interned
//! string removes its intern entry so the next load of the same content
//! reborns a fresh object.
//!
//! Counts are non-atomic: one `Objects` store belongs to one VM on one
//! thread [C4]. The `diag` feature arms the balance tripwires (rc
//! underflow/overflow asserts) the way `ds-rt`'s diag layer does;
//! without it they stay debug assertions.

use std::{collections::HashMap, time::Duration};

use crate::{net::Connections, value::Value, vm::Trap};

/// The free-list end sentinel: no `Free` word points past it.
const NO_FREE: u32 = u32::MAX;

/// One heap object: live payloads, or a freed slot threading the free
/// list.
#[derive(Debug)]
enum Object {
    Str(Box<str>),
    /// A list: one heap object — len, capacity, and the element words
    /// inline in a single table entry. The one in-place mutation is the
    /// append behind list literals ([`Objects::push_list`], the
    /// compiler-disciplined construction append over a freshly built
    /// list); every other mutation stays reserved for the exclusive
    /// windows, and growth is the backing buffer's amortized doubling.
    List(Vec<Value>),
    /// A freed slot: the next freed slot's index, or [`NO_FREE`]. The
    /// payload died when the slot freed, so its content was released.
    Free(u32),
}

/// The heap: the object table, its per-object reference counts, the free
/// list threading freed slots, and the string intern map.
#[derive(Debug, Default)]
pub struct Objects {
    interned: HashMap<Box<str>, u32>,
    table: Vec<Object>,
    rc: Vec<u32>,
    free_head: Option<u32>,
    live: usize,
    /// The connection table of the net natives ([`crate::net`]): sockets
    /// are VM-lifetime host state like heap objects, and the native
    /// calling convention hands a body exactly two host references (this
    /// store, the output sink), so the table rides the store the VM owns
    /// and drops — closing every open socket when the VM goes away.
    net: Connections,
}

impl Objects {
    /// An empty heap.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// How many objects are live (a freed slot counts only when reused).
    #[must_use]
    pub fn len(&self) -> usize {
        self.live
    }

    /// Whether the heap holds no live object.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.live == 0
    }

    /// The live reference count of object `obj`.
    ///
    /// # Errors
    ///
    /// [`Trap::UnknownObject`] when the reference names no live object.
    #[must_use = "the count is the witness"]
    pub fn rc_at(&self, obj: u32) -> Result<u32, Trap> {
        match self.table.get(obj as usize) {
            Some(Object::Str(_) | Object::List(_)) => Ok(self.rc[obj as usize]),
            _ => Err(Trap::UnknownObject { obj }),
        }
    }

    /// Claims one slot for `object`: the free list first (the
    /// dec-to-alloc pairing), the table tail otherwise. The birth count
    /// is 1 — the caller owns the object.
    fn alloc(&mut self, object: Object) -> u32 {
        let id = if let Some(id) = self.free_head {
            let Object::Free(next) = self.table[id as usize] else {
                unreachable!("the free list head is a freed slot");
            };
            self.free_head = (next != NO_FREE).then_some(next);
            self.table[id as usize] = object;
            id
        } else {
            let id = u32::try_from(self.table.len()).expect("object table exceeds u32");
            self.table.push(object);
            self.rc.push(1);
            id
        };
        self.rc[id as usize] = 1;
        self.live += 1;
        id
    }

    /// Interns `text`: one live object per distinct content, so string
    /// constants and host-provided arguments share objects — and the
    /// claim acquires a count (each `KSTR` load owns its own reference).
    /// Concatenation (`alloc_str`) deliberately does not intern: its
    /// results are transient by construction and content equality is
    /// what EQSTR answers.
    pub fn intern(&mut self, text: &str) -> u32 {
        if let Some(&id) = self.interned.get(text) {
            self.inc(id).expect("an interned object is live");
            return id;
        }
        let id = self.alloc_str(text.to_owned());
        self.interned.insert(text.into(), id);
        id
    }

    /// Allocates a fresh string object (no interning), owned by the caller.
    pub fn alloc_str(&mut self, text: String) -> u32 {
        self.alloc(Object::Str(text.into()))
    }

    /// Allocates a fresh list object over `elems`, owned by the caller.
    /// The element words are adopted as-is: a caller handing `Ref`
    /// elements must have acquired their counts itself (the producer
    /// natives over fresh objects).
    pub fn alloc_list(&mut self, elems: Vec<Value>) -> u32 {
        self.alloc(Object::List(elems))
    }

    /// `rc_inc` (C4): the reference in the slot naming `obj` gains one
    /// owner.
    ///
    /// # Errors
    ///
    /// [`Trap::UnknownObject`] when the reference names no live object:
    /// hand-assembled code or a host argument carried a bogus `Ref` word.
    pub fn inc(&mut self, obj: u32) -> Result<(), Trap> {
        match self.table.get(obj as usize) {
            Some(Object::Str(_) | Object::List(_)) => {},
            _ => return Err(Trap::UnknownObject { obj }),
        }
        let rc = &mut self.rc[obj as usize];
        // u32::MAX doubles as the diag overflow tripwire; wrapping keeps
        // the release build branch-free for a state only a balance bug
        // reaches.
        #[cfg(feature = "diag")]
        assert!(*rc != u32::MAX, "ds-vm diag: rc overflow (unbalanced inc)");
        #[cfg(not(feature = "diag"))]
        debug_assert!(*rc != u32::MAX);
        *rc = rc.wrapping_add(1);
        Ok(())
    }

    /// `rc_dec` (C4): one owner drops. A unique death frees the object —
    /// a string releases its content and its intern entry, a list drops
    /// one count per `Ref` element it holds — and threads the slot into
    /// the free list (the next compatible allocation claims it).
    ///
    /// # Errors
    ///
    /// [`Trap::UnknownObject`] when the reference names no live object
    /// (a double drop of a freed object lands here: the slot is `Free`).
    pub fn dec(&mut self, obj: u32) -> Result<(), Trap> {
        match self.table.get(obj as usize) {
            Some(Object::Str(_) | Object::List(_)) => {},
            _ => return Err(Trap::UnknownObject { obj }),
        }
        let rc = &mut self.rc[obj as usize];
        #[cfg(feature = "diag")]
        assert!(*rc > 0, "ds-vm diag: rc underflow (double drop)");
        #[cfg(not(feature = "diag"))]
        debug_assert!(*rc > 0);
        *rc -= 1;
        if *rc == 0 {
            self.free(obj);
        }
        Ok(())
    }

    /// The unique death: releases the payload, unbinds the intern entry,
    /// and threads the slot onto the free list.
    fn free(&mut self, obj: u32) {
        // an interned entry pointing here must not outlive the object:
        // the next intern of the same content allocates a fresh one
        if let Object::Str(text) = &self.table[obj as usize] {
            if self.interned.get(&**text) == Some(&obj) {
                self.interned.remove(&**text);
            }
        }
        let next = self.free_head.unwrap_or(NO_FREE);
        self.free_head = Some(obj);
        let taken = std::mem::replace(&mut self.table[obj as usize], Object::Free(next));
        self.live -= 1;
        if let Object::List(elems) = taken {
            // the list owned one count per Ref element (C4): the death
            // drops them, freed in element order
            for elem in elems {
                if let Value::Ref(id) = elem {
                    self.dec(id).expect("a stored element reference is live");
                }
            }
        }
    }

    /// Appends one element word onto a list object in place: the
    /// construction append behind list literals, where the list is
    /// freshly created and uniquely owned by the constructing frame.
    /// Storing a `Ref` element acquires a count for the list — the
    /// staged word stays owned by its own frame too (the placement pass's
    /// native-borrow discipline). Every other in-place mutation stays the
    /// exclusive-window story.
    ///
    /// # Errors
    ///
    /// [`Trap::UnknownObject`] when the reference names no list object.
    pub fn push_list(&mut self, obj: u32, value: Value) -> Result<(), Trap> {
        if !matches!(self.table.get(obj as usize), Some(Object::List(_))) {
            return Err(Trap::UnknownObject { obj });
        }
        // the acquire happens before the store: a bogus element reference
        // leaves the list untouched
        if let Value::Ref(id) = value {
            self.inc(id)?;
        }
        let Some(Object::List(elems)) = self.table.get_mut(obj as usize) else {
            return Err(Trap::UnknownObject { obj });
        };
        elems.push(value);
        Ok(())
    }

    /// Concatenates two lists into a fresh object: `lhs`'s elements then
    /// `rhs`'s, the joined length reserved up front (growth is amortized
    /// doubling; a join never re-grows mid-copy). The joined list owns
    /// one count per `Ref` element word it copies; the operand lists keep
    /// theirs.
    ///
    /// # Errors
    ///
    /// [`Trap::UnknownObject`] when either reference names no list.
    pub fn concat_lists(&mut self, lhs: u32, rhs: u32) -> Result<u32, Trap> {
        let (front, back) = (self.list_at(lhs)?, self.list_at(rhs)?);
        let mut elems = Vec::with_capacity(front.len() + back.len());
        let words: Vec<Value> = front.iter().chain(back.iter()).copied().collect();
        for value in words {
            if let Value::Ref(id) = value {
                self.inc(id)?;
            }
            elems.push(value);
        }
        Ok(self.alloc_list(elems))
    }

    /// Concatenates two strings into a fresh object, owned by the caller.
    ///
    /// # Errors
    ///
    /// [`Trap::UnknownObject`] when either reference names no string.
    pub fn concat(&mut self, lhs: u32, rhs: u32) -> Result<u32, Trap> {
        let mut text = String::with_capacity(self.str_at(lhs)?.len() + self.str_at(rhs)?.len());
        text.push_str(self.str_at(lhs)?);
        text.push_str(self.str_at(rhs)?);
        Ok(self.alloc_str(text))
    }

    /// The string content of object `obj`.
    ///
    /// # Errors
    ///
    /// [`Trap::UnknownObject`] when the reference names no live string
    /// object: hand-assembled code or a host argument carried a bogus
    /// `Ref` word.
    pub fn str_at(&self, obj: u32) -> Result<&str, Trap> {
        match self.table.get(obj as usize) {
            Some(Object::Str(text)) => Ok(text),
            _ => Err(Trap::UnknownObject { obj }),
        }
    }

    /// The element words of list object `obj`.
    ///
    /// # Errors
    ///
    /// [`Trap::UnknownObject`] when the reference names no live list object.
    pub fn list_at(&self, obj: u32) -> Result<&[Value], Trap> {
        match self.table.get(obj as usize) {
            Some(Object::List(elems)) => Ok(elems),
            _ => Err(Trap::UnknownObject { obj }),
        }
    }

    /// The connection table of the net natives.
    pub(crate) fn net(&mut self) -> &mut Connections {
        &mut self.net
    }

    /// Bounds every blocking net wait ([`crate::net`]).
    pub(crate) fn set_net_timeout(&mut self, timeout: Duration) {
        self.net.set_timeout(timeout);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intern_shares_by_content_and_alloc_does_not() {
        let mut objects = Objects::new();
        let a = objects.intern("localhost");
        let b = objects.intern("local");
        let c = objects.alloc_str("host".to_owned());
        let d = objects.concat(b, c).expect("both strings live");
        assert_eq!(
            objects.intern("localhost"),
            a,
            "interning shares one object and acquires a count"
        );
        assert_eq!(objects.rc_at(a), Ok(2));
        assert_ne!(
            d, a,
            "concatenation never interns: identity differs, content matches"
        );
        assert_eq!(objects.str_at(d), Ok("localhost"));
        assert_eq!(objects.len(), 4);
    }

    #[test]
    fn bogus_references_name_no_string() {
        let mut objects = Objects::new();
        let id = objects.intern("x");
        assert_eq!(
            objects.str_at(id + 1),
            Err(Trap::UnknownObject { obj: id + 1 })
        );
        assert_eq!(
            objects.concat(id, id + 1),
            Err(Trap::UnknownObject { obj: id + 1 })
        );
        assert_eq!(
            objects.inc(id + 1),
            Err(Trap::UnknownObject { obj: id + 1 })
        );
    }

    #[test]
    fn lists_hold_any_value_words_and_concat_shares_elements() {
        let mut objects = Objects::new();
        let a = objects.intern("a");
        let scalars = objects.alloc_list(vec![Value::I64(1), Value::Bool(false)]);
        let strings = objects.alloc_list(vec![Value::Ref(a)]);
        let joined = objects
            .concat_lists(scalars, strings)
            .expect("both lists live");
        assert_eq!(
            objects.list_at(joined),
            Ok(&[Value::I64(1), Value::Bool(false), Value::Ref(a)][..]),
            "element words copied in order, any kind"
        );
        // the joined list owns one acquired count of its shared element
        assert_eq!(objects.rc_at(a), Ok(2));
        // a string read over a list object names the wrong kind
        assert_eq!(
            objects.str_at(joined),
            Err(Trap::UnknownObject { obj: joined })
        );
        // a list read over a string object does too
        assert_eq!(objects.list_at(a), Err(Trap::UnknownObject { obj: a }));
        // concat over a non-list reports the bogus side
        assert_eq!(
            objects.concat_lists(scalars, a),
            Err(Trap::UnknownObject { obj: a })
        );
    }

    /// A count reaching zero frees the object: the slot reuses on the
    /// next allocation, a freed reference names no object, and a dying
    /// interned string unbinds its intern entry (the next load of the
    /// same content reborns fresh).
    #[test]
    fn a_unique_death_frees_and_the_slot_reuses() {
        let mut objects = Objects::new();
        let a = objects.intern("gone");
        assert_eq!(objects.rc_at(a), Ok(1));
        objects.dec(a).expect("the one owner drops");
        assert_eq!(objects.str_at(a), Err(Trap::UnknownObject { obj: a }));
        assert!(objects.is_empty(), "the heap drains");
        assert_eq!(objects.dec(a), Err(Trap::UnknownObject { obj: a }));
        // the reborn literal is a fresh object — but the freed slot
        // reuses for it (the dec-to-alloc pairing)
        let b = objects.intern("gone");
        assert_eq!(a, b, "the freed slot is claimed by the next allocation");
        assert_eq!(objects.str_at(b), Ok("gone"));
    }

    /// A shared list decs to zero without freeing; the unique death frees
    /// the list and drops its acquired element counts (a list of lists
    /// dies recursively).
    #[test]
    fn a_list_death_drops_its_element_counts() {
        let mut objects = Objects::new();
        // a fresh element whose birth count is the adopting list's (the
        // producer-native construction rule)
        let s = objects.alloc_str("elem".to_owned());
        let inner = objects.alloc_list(vec![Value::Ref(s)]);
        assert_eq!(objects.rc_at(s), Ok(1), "the adoption keeps the one count");
        let outer = objects.concat_lists(inner, inner).expect("both lists live");
        // the outer list acquired one count per copied element word
        assert_eq!(objects.rc_at(s), Ok(3));
        objects.dec(inner).expect("one owner of inner drops");
        assert_eq!(objects.rc_at(s), Ok(2), "inner still lives in outer");
        objects.dec(outer).expect("the outer's owner drops");
        assert!(objects.is_empty(), "the recursive death drains the heap");
    }
}
