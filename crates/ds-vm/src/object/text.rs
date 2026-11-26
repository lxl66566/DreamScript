//! String and bytes payloads: char-boundary metadata, char-indexed queries, content hashing.

use super::*;

impl Str {
    pub(super) fn new(text: Box<str>) -> Self {
        Self {
            text,
            chars: None,
            hash: Cell::new(None),
        }
    }

    /// The content's filter hash, computed on the first ask.
    pub(super) fn content_hash(&self) -> u32 {
        self.hash.get().unwrap_or_else(|| {
            let hash = text_hash(&self.text);
            self.hash.set(Some(hash));
            hash
        })
    }
}

impl CharIndex {
    /// One ASCII scan, then (multi-byte text only) one byte scan for the
    /// char count: a UTF-8 start byte is exactly a byte that is not a
    /// continuation (`b & 0xC0 != 0x80`), so no code point is decoded.
    pub(super) fn new(text: &str) -> Self {
        if text.is_ascii() {
            return Self::Ascii;
        }
        let count = text
            .as_bytes()
            .iter()
            .filter(|byte| **byte & 0xc0 != 0x80)
            .count();
        Self::Utf8 {
            count,
            offsets: None,
        }
    }

    /// The char count.
    pub(super) fn count(&self, text: &str) -> usize {
        match self {
            Self::Ascii => text.len(),
            Self::Utf8 { count, .. } => *count,
        }
    }

    /// Whether the boundary offsets are present (pure ASCII needs none).
    pub(super) fn has_offsets(&self) -> bool {
        match self {
            Self::Ascii => true,
            Self::Utf8 { offsets, .. } => offsets.is_some(),
        }
    }

    /// Records the boundary offsets a range query built.
    pub(super) fn set_offsets(&mut self, built: Box<[u32]>) {
        match self {
            Self::Ascii => unreachable!("ASCII text indexes chars by byte"),
            Self::Utf8 { offsets, .. } => *offsets = Some(built),
        }
    }

    /// The byte offset of char boundary `index`, a valid char position
    /// (`index <= count`); a range query built the table.
    pub(super) fn byte_at(&self, index: usize) -> usize {
        match self {
            Self::Ascii => index,
            Self::Utf8 { offsets, .. } => offsets
                .as_ref()
                .expect("a range query built the boundary offsets")[index]
                as usize,
        }
    }
}

/// The byte offsets of every char boundary of multi-byte `text` through
/// the end. A start byte is exactly a byte that is not a continuation,
/// so the table is one byte scan.
pub(super) fn char_offsets(text: &str) -> Box<[u32]> {
    // byte offsets are u32 words: the length check bounds every entry
    let len = u32::try_from(text.len()).expect("string byte length fits u32");
    let mut offsets = Vec::with_capacity(text.len() / 2 + 1);
    for (at, &byte) in text.as_bytes().iter().enumerate() {
        if byte & 0xc0 != 0x80 {
            offsets.push(u32::try_from(at).expect("char offset below the byte length"));
        }
    }
    // the final entry is the end-of-text boundary a range end reaches
    offsets.push(len);
    offsets.into()
}

impl Objects {
    /// Allocates a fresh string object (no interning), owned by the caller.
    pub fn alloc_str(&mut self, text: String) -> u32 {
        self.alloc(Object::Str(Str::new(text.into())))
    }

    /// Allocates a fresh string object holding `part`, owned by the
    /// caller. The empty answers of `substring` and `char_at` copy once
    /// through here instead of staging a `String` (which would copy
    /// twice).
    pub fn alloc_str_part(&mut self, part: &str) -> u32 {
        self.alloc(Object::Str(Str::new(part.into())))
    }

    /// Allocates a fresh string object holding the byte range
    /// `[begin, stop)` of string object `src` — a range on char
    /// boundaries, which every char-indexed query produces — owned by
    /// the caller. The `substring` and `char_at` results copy once
    /// through here instead of staging a `String` (which would copy
    /// twice).
    ///
    /// # Errors
    ///
    /// [`Trap::UnknownObject`] when the reference names no live string
    /// object.
    pub fn alloc_substr(&mut self, src: u32, begin: usize, stop: usize) -> Result<u32, Trap> {
        let part: Box<str> = self.str_at(src)?[begin..stop].into();
        Ok(self.alloc(Object::Str(Str::new(part))))
    }

    /// Concatenates two strings into a fresh object, owned by the caller.
    ///
    /// # Errors
    ///
    /// [`Trap::UnknownObject`] when either reference names no string.
    pub fn concat(&mut self, lhs: u32, rhs: u32) -> Result<u32, Trap> {
        let (left, right) = (self.str_at(lhs)?, self.str_at(rhs)?);
        let mut text = String::with_capacity(left.len() + right.len());
        text.push_str(left);
        text.push_str(right);
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
        match self.payload_of(obj) {
            Some(Object::Str(s)) => Ok(&s.text),
            _ => Err(Trap::UnknownObject { obj }),
        }
    }

    /// The char count of string object `obj`, building its char-boundary
    /// map on the first char-indexed query (see `[Str]`).
    ///
    /// # Errors
    ///
    /// [`Trap::UnknownObject`] when the reference names no live string
    /// object.
    pub fn char_count(&mut self, obj: u32) -> Result<usize, Trap> {
        self.ensure_char_count(obj)?;
        let (text, chars) = self.str_with_chars(obj)?;
        Ok(chars.count(text))
    }

    /// The byte offsets of the char range `[start, end)` of string object
    /// `obj`, building its char-boundary map on the first char-indexed
    /// query. Both bounds are valid char positions with
    /// `start <= end <= char count` — the contract the clamping in
    /// `substring` establishes — so both offsets sit on char boundaries
    /// and the slice between them cannot cut a code point.
    ///
    /// # Errors
    ///
    /// [`Trap::UnknownObject`] when the reference names no live string
    /// object.
    pub fn char_byte_range(
        &mut self,
        obj: u32,
        start: usize,
        end: usize,
    ) -> Result<(usize, usize), Trap> {
        self.ensure_char_offsets(obj)?;
        let (_, chars) = self.str_with_chars(obj)?;
        Ok((chars.byte_at(start), chars.byte_at(end)))
    }

    /// Builds the char count of string object `obj` unless it is already
    /// known: a count query leaves the boundary offsets unbuilt.
    pub(super) fn ensure_char_count(&mut self, obj: u32) -> Result<(), Trap> {
        let header = self.header_of(obj)?;
        if matches!(self.heap.payload::<Object>(header), Object::Str(s) if s.chars.is_some()) {
            return Ok(());
        }
        let built = CharIndex::new(self.str_at(obj)?);
        match self.heap.payload_mut::<Object>(header) {
            Object::Str(s) => s.chars = Some(built),
            _ => return Err(Trap::UnknownObject { obj }),
        }
        Ok(())
    }

    /// Builds the char-boundary offsets of string object `obj` unless they
    /// are already known: one table serves every later range query.
    pub(super) fn ensure_char_offsets(&mut self, obj: u32) -> Result<(), Trap> {
        self.ensure_char_count(obj)?;
        let header = self.header_of(obj)?;
        if matches!(
            self.heap.payload::<Object>(header),
            Object::Str(s) if s.chars.as_ref().is_some_and(CharIndex::has_offsets)
        ) {
            return Ok(());
        }
        let built = char_offsets(self.str_at(obj)?);
        match self.heap.payload_mut::<Object>(header) {
            Object::Str(s) => s
                .chars
                .as_mut()
                .expect("the char count was just built")
                .set_offsets(built),
            _ => return Err(Trap::UnknownObject { obj }),
        }
        Ok(())
    }

    /// The text and its char-boundary map of string object `obj`; the
    /// caller built the map first (a char-indexed query's private
    /// invariant, not a promise to embedders).
    pub(super) fn str_with_chars(&self, obj: u32) -> Result<(&str, &CharIndex), Trap> {
        match self.payload_of(obj) {
            Some(Object::Str(s)) => Ok((
                &s.text,
                s.chars
                    .as_ref()
                    .expect("a char-indexed query built the map"),
            )),
            _ => Err(Trap::UnknownObject { obj }),
        }
    }

    /// Allocates a fresh bytes object holding `payload`, owned by the
    /// caller — one counted block, `string`'s shape over raw bytes.
    pub fn alloc_bytes(&mut self, payload: Box<[u8]>) -> u32 {
        self.alloc(Object::Bytes(payload))
    }

    /// The byte payload of bytes object `obj`.
    ///
    /// # Errors
    ///
    /// [`Trap::UnknownObject`] when the reference names no live bytes
    /// object: hand-assembled code or a host argument carried a bogus
    /// `Ref` word.
    pub fn bytes_at(&self, obj: u32) -> Result<&[u8], Trap> {
        match self.payload_of(obj) {
            Some(Object::Bytes(data)) => Ok(data),
            _ => Err(Trap::UnknownObject { obj }),
        }
    }

    /// Whether `obj` names a live string object holding exactly `text`:
    /// the KSTR pool-const cache's hit validation — a cached id stays
    /// servable only while its content still matches, so a dead or
    /// recycled id never serves.
    pub(crate) fn is_live_str_with(&self, obj: u32, text: &str) -> bool {
        matches!(self.payload_of(obj), Some(Object::Str(s)) if &*s.text == text)
    }

    /// The string content of object `obj` with its cached filter hash, when
    /// `obj` names a live string — the map probe's foreign-resolution read,
    /// so the probe's own heap hashes and copies the text once.
    pub(crate) fn str_with_hash(&self, obj: u32) -> Option<(&str, u32)> {
        match self.payload_of(obj) {
            Some(Object::Str(s)) => Some((&s.text, s.content_hash())),
            _ => None,
        }
    }
}
