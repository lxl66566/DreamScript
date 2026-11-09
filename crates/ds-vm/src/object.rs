//! The VM-side heap object store [BL-2]: the objects raw `Ref` words name.
//!
//! M3 carries string objects plus the list objects `fs_list` returns (name
//! lists; the wider M4 heap kinds — boxes, arrays — land with precise RC).
//! The representation is deliberately the simplest thing the tier-1
//! contract needs: one table indexed by the `Value::Ref` word, one
//! content-keyed intern map for string constants. Nothing here uses the
//! ds-rt bump arena: mark/restore arenas reclaim at frame-pop granularity,
//! while interned constants outlive every frame, so an arena would be the
//! wrong lifetime; the store is plain owned storage.
//!
//! Reclamation is the M3 placeholder: objects are never freed. M4 lands
//! precise non-atomic RC with the Perceus dup/drop placement pass
//! (compiler-inserted DUP/DROP uops over exactly this table), so the
//! allocation side of this API is shaped to take an `rc` word without a
//! representation change.

use std::{collections::HashMap, time::Duration};

use crate::{net::Connections, vm::Trap};

/// One heap object. M3: strings, and the `fs_list` name lists.
#[derive(Debug)]
enum Object {
    Str(Box<str>),
    /// A list of element refs. `fs_list` fills it with string refs; M4's
    /// precise RC manages the elements through the same table.
    List(Vec<u32>),
}

/// The heap: the object table plus the string intern map.
#[derive(Debug, Default)]
pub struct Objects {
    interned: HashMap<Box<str>, u32>,
    table: Vec<Object>,
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

    /// How many objects are live (all of them, in the M3 placeholder).
    #[must_use]
    pub fn len(&self) -> usize {
        self.table.len()
    }

    /// Whether the heap holds no object.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.table.is_empty()
    }

    /// Interns `text`: one object per distinct content, so string
    /// constants and host-provided arguments share objects. Concatenation
    /// (`alloc`) deliberately does not intern: its results are transient
    /// by construction and content equality is what EQSTR answers.
    pub fn intern(&mut self, text: &str) -> u32 {
        if let Some(&id) = self.interned.get(text) {
            return id;
        }
        let id = self.alloc_str(text.to_owned());
        self.interned.insert(text.into(), id);
        id
    }

    /// Allocates a fresh string object (no interning).
    pub fn alloc_str(&mut self, text: String) -> u32 {
        let id = u32::try_from(self.table.len()).expect("object table exceeds u32");
        self.table.push(Object::Str(text.into()));
        id
    }

    /// Allocates a fresh list object over `elems`.
    pub fn alloc_list(&mut self, elems: Vec<u32>) -> u32 {
        let id = u32::try_from(self.table.len()).expect("object table exceeds u32");
        self.table.push(Object::List(elems));
        id
    }

    /// Concatenates two strings into a fresh object.
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
    /// [`Trap::UnknownObject`] when the reference names no string object:
    /// hand-assembled code or a host argument carried a bogus `Ref` word.
    pub fn str_at(&self, obj: u32) -> Result<&str, Trap> {
        match self.table.get(obj as usize) {
            Some(Object::Str(text)) => Ok(text),
            _ => Err(Trap::UnknownObject { obj }),
        }
    }

    /// The element refs of list object `obj`.
    ///
    /// # Errors
    ///
    /// [`Trap::UnknownObject`] when the reference names no list object.
    pub fn list_at(&self, obj: u32) -> Result<&[u32], Trap> {
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
            "interning shares one object"
        );
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
    }

    #[test]
    fn lists_hold_element_refs_and_reject_string_reads() {
        let mut objects = Objects::new();
        let a = objects.intern("a");
        let b = objects.intern("b");
        let list = objects.alloc_list(vec![a, b]);
        assert_eq!(objects.list_at(list), Ok(&[a, b][..]));
        // a string read over a list object names the wrong kind
        assert_eq!(objects.str_at(list), Err(Trap::UnknownObject { obj: list }));
        // a list read over a string object does too
        assert_eq!(objects.list_at(a), Err(Trap::UnknownObject { obj: a }));
    }
}
