//! Typed frame layouts and the slot-overlay geometry.
//!
//! Physical frame layout, one contiguous span per frame (the drawing grows
//! toward the callee; only the relative order is normative):
//!
//! ```text
//!         higher addresses
//!   +----------------------+
//!   | outgoing-arg slot A-1|   the overlay region: A is the max over this
//!   | ...                  |   function's call sites of callee param count
//!   | outgoing-arg slot 0  |   <- a callee's slot 0 overlays this word
//!   +----------------------+
//!   | call word            |   hosts the callee's metadata word during a
//!   |                      |   call; dead otherwise
//!   +----------------------+
//!   | slots[P + L - 1]     |   locals (L slots)
//!   | ...                  |
//!   | slots[P]             |
//!   +----------------------+
//!   | slots[P - 1]         |   parameters (P slots); slot 0 is the first
//!   | ...                  |   parameter
//!   | slots[0]             |
//!   +----------------------+
//!   | metadata word        |   return pc, frame size, tail chain
//!   +----------------------+   <- frame pointer (fp)
//!         lower addresses
//! ```
//!
//! # Physical word mapping, pinned per frame owner
//!
//! Every word position is defined by the frame that OWNS it. For the frame
//! at `fp` with `B = params_len + locals` (its body, the slots below its
//! call word):
//!
//! ```text
//! | word, owned by the frame at fp       | physical stack index |
//! |--------------------------------------+----------------------|
//! | metadata word                        | fp                   |
//! | body slot i (parameters and locals)  | fp + 1 + i           |
//! | call word                            | fp + 1 + B           |
//! | outgoing-arg slot j (overlay region) | fp + 2 + B + j       |
//! ```
//!
//! The call word is why a frame's body slots and overlay slots disagree by
//! exactly one word: a frame reads its own body at `fp+1+i`, but its
//! outgoing arguments sit above its call word, at `fp+2+B+j`. A mapping is
//! only meaningful together with its owning frame and that frame's fp.
//!
//! - CALL: the callee's metadata word lands in the caller's call word (`callee_fp = fp + 1 +
//!   B_caller`), so callee param slot j coincides with caller outgoing-arg slot j (`callee_fp + 1 +
//!   j == fp + 2 + B_caller + j`) with no argument copying.
//! - CALLT: fp is unchanged, and the two ends of the argument slide use different owners' mappings.
//!   The destination slot j is the callee's own body position `fp + 1 + j` — unconditionally, below
//!   the callee's call word, never mapped through the caller's body, so a wide callee (`params_len
//!   > B_caller`) still lands its parameters contiguously. The source slot j is the caller's
//!   overlay position `fp + 2 + B_caller + j`.
//!
//! [`crate::SlidePlan`] and its reference semantics stay at the logical
//! level: flat frame-relative slot indices into a [`FrameLayout`]'s slot
//! list, with no call-word displacement. Translating a plan's logical
//! indices to physical stack words applies the owner-relative mapping
//! above to each end of the slide (asserted in the
//! `physical_word_mapping_per_owner_matches_the_pinned_table` test).
//!
//! Clarification of the locked contract: this section pins, per frame
//! owner, the physical mapping the frame invariants already implied. It is
//! a documentation change only; no word layout, plan, or semantic change.
//!
//! Invariants:
//!
//! - A normal call writes the callee's metadata word into the caller's call word, so callee slot 0
//!   coincides with caller outgoing-arg slot 0: the callee's argument slots overlay the caller's
//!   outgoing-argument area with no argument copying.
//! - A tail call (CALLT) keeps the same fp: the staged arguments slide down to slots `0..n`
//!   ([`crate::slide_plan`]), the metadata word is updated in place, and control jumps through the
//!   callee's [`crate::EntryDescriptor`]. The stack never grows at any depth.
//! - Every frame reserves the metadata word and the call word uniformly (a zero-argument call still
//!   needs the call word to host the callee's metadata), so [`FrameLayout::frame_size`] is `slots +
//!   2` words for every function and stack arithmetic has no per-function special cases.

/// The static type of one frame slot; raw unboxed values in slots.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SlotType {
    /// 32-bit integer (explicit-width locals, FFI); one word.
    I32,
    /// 64-bit integer, the `int` default; one word.
    I64,
    /// 64-bit float, the `float` default; one word.
    F64,
    /// Unboxed boolean; one word.
    Bool,
    /// Raw reference to an RC-managed heap object; never null, which leaves
    /// the null niche free for `T | none` packing, a legal later pass.
    Ref,
    /// Tag word of an unboxed tag+payload union: a bare discriminant
    /// in one slot. A payloaded variant's fields occupy the immediately
    /// following slots, typed structurally by the same [`FrameLayout`]; a
    /// unit tag carries no payload slots.
    Tag,
    /// A callable word (the fn slot classification): one word holding a
    /// copyable scalar FuncRef — module id plus function index packed into
    /// the word — naming a non-capturing function; copied by value with zero
    /// RC cost. Capturing lambdas are not scalars: they are `Ref` words
    /// pointing at closure objects (header word the FuncRef, then the
    /// capture block). A value-site closure word lives in a `Ref`-classified
    /// slot; a fn-typed *parameter* slot is callable-defaulted — its
    /// classification stays `Fn` while the arriving word may be either the
    /// scalar or the closure `Ref` (the slide admits that pairing), and the
    /// call readers (CALLS, the hosted per-element call) resolve the callee
    /// word by its runtime kind, the one dynamic-dispatch point.
    Fn,
    /// An untyped raw word: the overlay region's staging slots when one
    /// frame stages more than one call shape, or receives a callee's
    /// result span whose types differ from the staged arguments on the
    /// same slots (both are ordinary — the overlay words are reused). A
    /// `Raw` slot accepts every write and every read checks the value's
    /// concrete type; `slide_plan` accepts `Raw` staging against any
    /// callee parameter shape, and the tier-1 VM re-checks the staged
    /// value words at the call site. This is the one slot type the locked
    /// frame rules needed added to admit heterogeneous call sites within a
    /// single frame: no frame layout can otherwise type one overlay word
    /// that carries a `Tag` argument, then a `Ref` result, then an `I64`
    /// argument over its lifetime.
    Raw,
}

/// One bound extern's return word: the marshaling slice's scalar
/// words, or the absent word a unit return rides — the pool
/// `Constant::Bound` return payload, decoded and re-encoded by the
/// format's compact byte beside the symbol index.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BoundRet {
    /// No return word — the C void, the `unit` return's own encoding.
    Unit,
    /// The i64 general-purpose word, `int`'s.
    Int,
    /// The u8 general-purpose word, `bool`'s.
    Bool,
    /// The f64 floating word, `float`'s.
    Float,
}

/// How a generic dispatch's staged real words map onto the table entry's
/// named parameter words: one word per named parameter; one element span
/// leading the named words (a `cons` head's whole inline span rides the
/// front, the list `Ref` trailing); or one span staged between the leading
/// and trailing named words (a fold's accumulator between the list and the
/// callable). The span's own words are the call site's fact and carry no
/// table-side kind check.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Staging {
    /// The named words are the whole staged span, position by position.
    Fixed,
    /// One element span precedes the trailing named words.
    Leading,
    /// One span sits between the leading named words and the final named
    /// word, so the final named word's position is dynamic per call.
    Interior,
}

/// Invariant violation in a [`FrameLayout`] construction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LayoutError {
    /// `params_len + arg_area_len` exceeds the slot count.
    Overrun {
        params_len: usize,
        arg_area_len: usize,
        slot_count: usize,
    },
}

/// The static layout of one function's frame: ordered slot types
/// (parameters, then locals, then the outgoing-argument area), the
/// argument-area slot range (the overlay region), and the frame size.
///
/// `ds-lower` assigns this layout per function; the dsmod per-function
/// frame-slot-type table is its serialized form. The derived
/// constants ride along, so the frame arithmetic every call and every
/// frame entry reads is a field load instead of a re-derivation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FrameLayout {
    slots: Vec<SlotType>,
    params_len: usize,
    arg_area_len: usize,
    /// `slots.len() - arg_area_len`: the parameters and locals, the span
    /// below the call word (see the pinned mapping in the module docs).
    body_len: usize,
    /// `slots.len() + 2`: the metadata word, the value slots, the call
    /// word.
    frame_size: u32,
    /// Whether any outgoing-argument slot is `Raw`: the staging shape
    /// whose call-site check reads the staged words' runtime kinds.
    arg_area_has_raw: bool,
}

impl FrameLayout {
    /// Builds a layout whose slots are `params_len` parameters,
    /// `slots.len() - params_len - arg_area_len` locals, then an
    /// `arg_area_len`-slot outgoing-argument area at the tail.
    ///
    /// # Panics
    ///
    /// Panics if the frame's word span does not fit in a `u32`, which the
    /// uop format's 256-slot cap rules out for any constructed layout.
    pub fn new(
        slots: Vec<SlotType>,
        params_len: usize,
        arg_area_len: usize,
    ) -> Result<Self, LayoutError> {
        if params_len > slots.len() || arg_area_len > slots.len() - params_len {
            return Err(LayoutError::Overrun {
                params_len,
                arg_area_len,
                slot_count: slots.len(),
            });
        }
        let body_len = slots.len() - arg_area_len;
        let frame_size = u32::try_from(slots.len() + 2).expect("frame word span exceeds u32");
        let arg_area_has_raw = slots[body_len..].contains(&SlotType::Raw);
        Ok(Self {
            slots,
            params_len,
            arg_area_len,
            body_len,
            frame_size,
            arg_area_has_raw,
        })
    }

    /// All value slots in frame order: parameters, locals, argument area.
    #[must_use]
    pub fn slots(&self) -> &[SlotType] {
        &self.slots
    }

    /// The parameter slots; the callee-side half of the overlay.
    #[must_use]
    pub fn params(&self) -> &[SlotType] {
        &self.slots[..self.params_len]
    }

    /// How many leading slots are parameters (the callee argument count).
    #[must_use]
    pub fn params_len(&self) -> usize {
        self.params_len
    }

    /// The body slots — parameters then locals — which sit below the
    /// frame's call word (fp+1+i for slot i) and so index the first
    /// outgoing-argument slot.
    #[must_use]
    pub fn body_len(&self) -> usize {
        self.body_len
    }

    /// The non-parameter locals between the parameters and the overlay.
    #[must_use]
    pub fn locals(&self) -> &[SlotType] {
        &self.slots[self.params_len..self.body_len]
    }

    /// The outgoing-argument slot types (the caller-side half of the
    /// overlay).
    #[must_use]
    pub fn arg_types(&self) -> &[SlotType] {
        &self.slots[self.body_len..]
    }

    /// The outgoing-argument slot range within [`FrameLayout::slots`]:
    /// the overlay region, always the frame tail.
    #[must_use]
    pub fn arg_area(&self) -> std::ops::Range<usize> {
        self.body_len..self.slots.len()
    }

    /// Whether any outgoing-argument slot carries [`SlotType::Raw`]: only
    /// then does a call site read the staged words' runtime kinds (a
    /// concretely staged area is fully checked by the slide plan).
    #[must_use]
    pub fn arg_area_has_raw(&self) -> bool {
        self.arg_area_has_raw
    }

    /// Length of the outgoing-argument area: the max over this function's
    /// call sites of the callee parameter count.
    #[must_use]
    pub fn arg_area_len(&self) -> usize {
        self.arg_area_len
    }

    /// Total words in the physical frame span: metadata word + call word +
    /// value slots. This is the value stored in [`crate::FrameMetadata`]'s
    /// `frame_size`; a frame pop restores the stack pointer past exactly
    /// this many words.
    #[must_use]
    pub fn frame_size(&self) -> u32 {
        self.frame_size
    }
}

#[cfg(test)]
mod tests {
    use SlotType::{Bool, F64, I64, Raw, Ref};

    use super::*;

    #[test]
    fn arg_area_is_the_frame_tail() {
        let l = FrameLayout::new(vec![I64, Bool, Ref, F64], 1, 2).expect("valid layout");
        assert_eq!(l.slots(), &[I64, Bool, Ref, F64]);
        assert_eq!(l.params(), &[I64]);
        assert_eq!(l.params_len(), 1);
        assert_eq!(l.body_len(), 2);
        assert_eq!(l.locals(), &[Bool]);
        assert_eq!(l.arg_types(), &[Ref, F64]);
        assert_eq!(l.arg_area(), 2..4);
        assert_eq!(l.arg_area_len(), 2);
        assert!(!l.arg_area_has_raw());
    }

    #[test]
    fn a_raw_staging_slot_is_visible_on_the_layout() {
        let l = FrameLayout::new(vec![I64, Raw, Raw], 1, 2).expect("valid layout");
        assert!(l.arg_area_has_raw());
        // a body Raws do not make the call site read staged words
        let body_only = FrameLayout::new(vec![Raw, I64], 0, 1).expect("valid layout");
        assert!(!body_only.arg_area_has_raw());
    }

    #[test]
    fn frame_size_counts_meta_and_call_words() {
        let l = FrameLayout::new(vec![I64], 1, 0).expect("valid layout");
        assert_eq!(l.frame_size(), 3);
        let empty = FrameLayout::new(Vec::new(), 0, 0).expect("valid layout");
        assert_eq!(empty.frame_size(), 2);
        assert_eq!(empty.locals(), &[] as &[SlotType]);
    }

    #[test]
    fn overrun_layout_is_rejected() {
        assert_eq!(
            FrameLayout::new(vec![I64, Bool], 2, 1),
            Err(LayoutError::Overrun {
                params_len: 2,
                arg_area_len: 1,
                slot_count: 2
            })
        );
    }

    // The pinned mapping table in the module docs, asserted for one
    // concrete layout at fp = 100: body = 1 param + 1 local = 2 slots,
    // arg area = 2 slots.
    #[test]
    // caller/callee is this contract's domain vocabulary, not a naming slip.
    #[allow(clippy::similar_names)]
    fn physical_word_mapping_per_owner_matches_the_pinned_table() {
        let caller = FrameLayout::new(vec![I64, Bool, I64, F64], 1, 2).expect("valid layout");
        let body = caller.params_len() + caller.locals().len();
        assert_eq!(body, 2);
        let fp = 100;
        let body_word = |i: usize| fp + 1 + i;
        let call_word = fp + 1 + body;
        let overlay_word = |j: usize| fp + 2 + body + j;
        assert_eq!(body_word(0), 101);
        assert_eq!(call_word, 103);
        // a frame's overlay sits one word above its call word, so body and
        // overlay slots of one frame disagree by exactly the call word
        assert_eq!(overlay_word(0), 104);

        // CALL: the callee's metadata word lands in the caller's call word,
        // so callee param slot j coincides with caller outgoing-arg slot j
        // with zero copy.
        let callee = FrameLayout::new(vec![I64, F64], 2, 0).expect("valid layout");
        let callee_fp = call_word;
        assert_eq!(callee_fp + 1, overlay_word(0));
        assert_eq!(callee_fp + 2, overlay_word(1));

        // CALLT: same fp; the destination is the callee's OWN body mapping
        // — fp+1+j unconditionally, below the callee's call word, never
        // through the caller's body (a wide callee stays contiguous and
        // may cover the caller's dead body slots) — while the source stays
        // caller-owned overlay words.
        let dst_word = |j: usize| fp + 1 + j;
        assert_eq!(dst_word(0), 101); // not overlay_word(0) = 104
        assert_eq!(dst_word(callee.params_len() - 1), 102);
        // physical slide distance = logical delta + the call word
        assert_eq!(overlay_word(0) - dst_word(0), caller.arg_area().start + 1);
    }
}
