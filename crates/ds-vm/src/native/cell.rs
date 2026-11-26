//! # The owning container's natives (indices 111-113)
//!
//! A `box[t]` value is one RC'd heap object — the held value's whole
//! word span inline in a single object-table entry (the shared payload
//! layout; see [`crate::object::Objects`]). The held words are plain
//! value words under one owned count per `Ref` word, the list edge
//! contract. The constructor adopts the staged span under fresh counts
//! (the staged words' own counts die at the call's window flush), the
//! read hands the span out under fresh counts, and the store releases
//! the held span's counts before adopting the incoming one. All three
//! are pure natives.

use super::{sink::ref_arg, *};

// total: the Result is the calling convention every native shares, not
// this body's
#[allow(clippy::unnecessary_wraps)]
pub(super) fn box_new_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    // the variadic tail is the held value's whole word span
    let held = heaps.alloc_cell(args)?;
    Ok(NativeResult::Word(held))
}

pub(super) fn box_get_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let cell = ref_arg(args, 0)?;
    // the held span hands out under one fresh count per `Ref` word, the
    // container keeping its own — the checked read-out's contract. The
    // answer lands plain: the call's own result type's words with no
    // discriminant (the total-by-result-type row's kind)
    let words = heaps.cell_get(cell)?;
    for value in &words {
        if let Value::Ref(word) = value {
            heaps.inc(*word)?;
        }
    }
    Ok(match words.as_slice() {
        [only] => NativeResult::Word(*only),
        words => NativeResult::Total {
            words: words.to_vec(),
        },
    })
}

pub(super) fn box_set_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let cell = ref_arg(args, 0)?;
    // the variadic tail is the incoming value's whole word span
    heaps.cell_set(cell, &args[1..])?;
    // the inert unit word: the member tier re-answers the receiver's
    // own word after this
    Ok(NativeResult::Word(Value::I64(0)))
}
