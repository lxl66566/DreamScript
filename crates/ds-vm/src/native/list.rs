//! # The list natives (indices 20-23)
//!
//! A list is one RC'd heap object — len, capacity, and the element value
//! words inline in a single object-table entry (the header's
//! `{type_id, rc}` words are the precise-RC slot over the same table;
//! see [`crate::object::Objects`]). Elements are plain value words, so
//! the one kind serves every element type, and a multi-word element (a
//! struct) is stored as its whole inline word span at a per-list element
//! stride taken from `list[t]`'s compile-time shape: the first append
//! fixes the stride, every later append repeats it, and the readers
//! (`list_len`, the checked `list_get` span) slice by it — so a
//! wider checked read answers through [`NativeResult::Span`], the span
//! channel, one payload word per element word with a count per `Ref`
//! word. Construction (`list_new`, `list_concat`, and the producers
//! `split`/`fs_list`) and reads (`list_len`, `list_get`) are pure
//! natives. The one in-place mutation is `list_push` (index 27): the
//! construction append list literals lower to, over a freshly built list
//! its frame uniquely owns — its staged span is variadic (the list word
//! plus the element's every word), the one native whose argument count
//! varies by call site; every other mutation stays reserved for the
//! compiler-disciplined exclusive windows, and growth is the backing
//! buffer's amortized doubling (`list_concat` reserves the joined
//! length up front).

use super::{
    sink::{i64_arg, ref_arg},
    *,
};

// total: the Result is the calling convention every native shares, not
// this body's
#[allow(clippy::unnecessary_wraps)]
pub(super) fn list_new_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    _args: &[Value],
) -> Result<NativeResult, Trap> {
    Ok(NativeResult::Word(heaps.new_list(Vec::new())))
}

pub(super) fn list_len_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let (elems, stride) = heaps.list_at(ref_arg(args, 0)?)?;
    // stride 0 iff empty, so the division is exact and the empty list
    // answers 0 whatever elements it would take
    let n = i64::try_from(elems.len() / stride.max(1)).expect("list length fits i64");
    Ok(NativeResult::Word(Value::I64(n)))
}

pub(super) fn list_get_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let (elems, stride) = heaps.list_at(ref_arg(args, 0)?)?;
    let index = i64_arg(args, 1)?;
    // Bounds follow the crate's checked-API convention (the span shape
    // `parse_int` answers), not the string ops' clamping: a generic
    // element type has no benign total answer, and a trap would deny the
    // caller the `t | none` extraction. A negative index fails the
    // usize conversion, so both out-of-range sides meet in `None`. The
    // hit is one element's whole inline span — `stride` words — handed
    // out under one count per `Ref` word (the one-word stride keeps the
    // allocation-free Tag shape).
    let hit = usize::try_from(index)
        .ok()
        .filter(|&at| at < elems.len() / stride.max(1))
        .map(|at| &elems[at * stride..(at + 1) * stride]);
    Ok(match hit {
        Some([only]) => {
            // the one-word stride reads through the `Tag` shape: no span
            // buffer, and the slice dies before the count goes on
            let word = *only;
            native_result_word(heaps, word)?
        },
        Some(words) => {
            // the slice dies with the list borrow before the retains run
            let words = words.to_vec();
            native_result_span(heaps, words)?
        },
        None => NativeResult::Tag {
            tag: PLATFORM_NONE,
            payload: Value::I64(index),
        },
    })
}

/// A checked read's one-word hit: the element word handed out under one
/// count when it is a `Ref` (the list keeps its own) — the
/// allocation-free `Tag` shape.
pub(super) fn native_result_word(heaps: &mut Heaps, word: Value) -> Result<NativeResult, Trap> {
    if let Value::Ref(id) = word {
        heaps.inc(id)?;
    }
    Ok(NativeResult::Tag {
        tag: PLATFORM_IO_OK,
        payload: word,
    })
}

/// A checked read's wider hit (two or more words): the element's whole
/// word span handed out with one count per `Ref` word for the reader (the
/// list keeps its own). The vec moves into the span payload, so the
/// span the caller sliced out of the store is the copy's only one.
pub(super) fn native_result_span(
    heaps: &mut Heaps,
    words: Vec<Value>,
) -> Result<NativeResult, Trap> {
    for value in &words {
        if let Value::Ref(word) = *value {
            heaps.inc(word)?;
        }
    }
    Ok(NativeResult::Span {
        tag: PLATFORM_IO_OK,
        payload: words,
    })
}

pub(super) fn list_concat_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let joined = heaps.concat_lists(ref_arg(args, 0)?, ref_arg(args, 1)?)?;
    Ok(NativeResult::Word(heaps.word(joined)))
}

// total: the Result is the calling convention every native shares, not
// this body's
#[allow(clippy::unnecessary_wraps)]
pub(super) fn list_push_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let list = ref_arg(args, 0)?;
    // the variadic tail is the element's whole inline word span; the
    // first push fixes the list's stride, a disagreeing later push traps
    heaps.push_list(list, &args[1..])?;
    // the inert unit word: no caller reads the overlay result
    Ok(NativeResult::Word(Value::I64(0)))
}

pub(super) fn list_set_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    // the staged span is the list word, the index word, then the
    // element's whole inline word span (`list_push`'s variadic shape at
    // the element position)
    if args.len() < 3 {
        return Err(Trap::ArityMismatch {
            expected: 3,
            given: args.len(),
        });
    }
    heaps.set_list(ref_arg(args, 0)?, i64_arg(args, 1)?, &args[2..])?;
    // the inert unit word: no caller reads the overlay result
    Ok(NativeResult::Word(Value::I64(0)))
}

pub(super) fn list_remove_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    heaps.remove_list(ref_arg(args, 0)?, i64_arg(args, 1)?)?;
    // the inert unit word: no caller reads the overlay result
    Ok(NativeResult::Word(Value::I64(0)))
}

pub(super) fn list_insert_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    // `list_set`'s staging shape: the list word, the index word, then
    // the element's whole inline word span
    if args.len() < 3 {
        return Err(Trap::ArityMismatch {
            expected: 3,
            given: args.len(),
        });
    }
    heaps.insert_list(ref_arg(args, 0)?, i64_arg(args, 1)?, &args[2..])?;
    // the inert unit word: no caller reads the overlay result
    Ok(NativeResult::Word(Value::I64(0)))
}

pub(super) fn list_clear_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    heaps.clear_list(ref_arg(args, 0)?)?;
    // the inert unit word: no caller reads the overlay result
    Ok(NativeResult::Word(Value::I64(0)))
}

/// The copy natives' shared finisher: the fresh list's edge owns one
/// count per `Ref` word of `elems` (taken on the one walk the copy
/// already pays), the sources keep their own, and the vec moves
/// into the adopting list with no second pass. The fresh list carries
/// the source's stride (an empty copy keeps stride 0).
pub(super) fn copy_into_fresh_list(
    heaps: &mut Heaps,
    elems: Vec<Value>,
    stride: usize,
) -> Result<NativeResult, Trap> {
    for value in &elems {
        if let Value::Ref(word) = *value {
            heaps.inc_edge(word)?;
        }
    }
    Ok(NativeResult::Word(heaps.new_list_strided(elems, stride)))
}

pub(super) fn cons_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    // the staged span is the head element's whole word span riding the
    // front (`list_push`'s variadic shape at the other end) then the
    // list `Ref` last — at least one head word plus the list
    let (list_arg, head) = args.split_last().ok_or(Trap::ArityMismatch {
        expected: 2,
        given: args.len(),
    })?;
    if head.is_empty() {
        return Err(Trap::ArityMismatch {
            expected: 2,
            given: args.len(),
        });
    }
    let list = match *list_arg {
        Value::Ref(word) => word,
        other => {
            return Err(Trap::SlotTypeMismatch {
                slot: u8::try_from(args.len() - 1).expect("slot index fits u8"),
                expected: SlotType::Ref,
                actual: other.slot_type(),
            })
        },
    };
    let (rest, stride) = heaps.list_at(list)?;
    // an empty rest list is stride-polymorphic, so the head's width fixes
    // the fresh stride; a non-empty one must repeat it exactly
    let stride = match stride {
        0 => head.len(),
        s if s == head.len() => s,
        s => {
            return Err(Trap::ElementStride {
                obj: list.id(),
                stride: s,
                given: head.len(),
            })
        },
    };
    let mut elems = Vec::with_capacity(rest.len() + head.len());
    elems.extend_from_slice(head);
    elems.extend_from_slice(rest);
    copy_into_fresh_list(heaps, elems, stride)
}

pub(super) fn tail_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    // an empty list skips nothing and answers the empty list
    let (elems, stride) = heaps.list_at(ref_arg(args, 0)?)?;
    let skipped = elems.get(stride..).unwrap_or(&[]).to_vec();
    copy_into_fresh_list(heaps, skipped, stride)
}

pub(super) fn collect_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let (elems, stride) = heaps.list_at(ref_arg(args, 0)?)?;
    let elems = elems.to_vec();
    copy_into_fresh_list(heaps, elems, stride)
}

/// `first`'s one body: the checked first element in
/// `list_get`'s span shape — the hit is the first element's whole
/// inline span, handed out under one count per `Ref` word, and an
/// empty list answers the `none` span member, never a trap. The miss
/// payload is the inert unit word: there is no rejected index to carry,
/// and callers read the payload only on the hit.
pub(super) fn first_element_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let (elems, stride) = heaps.list_at(ref_arg(args, 0)?)?;
    let hit = (!elems.is_empty()).then(|| &elems[..stride]);
    Ok(match hit {
        Some([only]) => {
            // the one-word element reads through the `Tag` shape: no span
            // buffer, and the slice dies before the count goes on
            let word = *only;
            native_result_word(heaps, word)?
        },
        Some(words) => {
            let words = words.to_vec();
            native_result_span(heaps, words)?
        },
        None => NativeResult::Tag {
            tag: PLATFORM_NONE,
            payload: Value::I64(0),
        },
    })
}

/// The greater of two numeric words: int compares in i64's total order;
/// float follows IEEE and never traps, a NaN operand answering the other
/// side. Any other word kind refuses like a comparison against a
/// mismatched slot — the call site's own trap shape.
pub(super) fn max_fn(
    _heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    match args {
        [Value::I64(a), Value::I64(b)] => Ok(NativeResult::Word(Value::I64((*a).max(*b)))),
        [Value::F64(a), Value::F64(b)] => Ok(NativeResult::Word(Value::F64((*a).max(*b)))),
        _ => {
            // the pair must agree on one numeric kind: name the word that
            // breaks it at its own index — the first non-numeric word, or
            // the second of a mixed int/float pair — citing the other
            // side's kind as expected
            let offender = args
                .iter()
                .position(|a| !matches!(a, Value::I64(_) | Value::F64(_)))
                .unwrap_or(1);
            let expected = args
                .iter()
                .enumerate()
                .find(|(i, a)| *i != offender && matches!(a, Value::I64(_) | Value::F64(_)))
                .map_or(SlotType::I64, |(_, a)| a.slot_type());
            Err(Trap::SlotTypeMismatch {
                slot: u8::try_from(offender).expect("slot index fits u8"),
                expected,
                actual: args.get(offender).map_or(SlotType::I64, Value::slot_type),
            })
        },
    }
}

// The arena natives (the read-side aliasing container): ordinary pure
// natives over the object store — no scheduler step, so no dispatcher
// interception; the handle packing and the slot discipline live in the
// store (`crate::object`).

// total: the Result is the calling convention every native shares, not
// this body's
#[allow(clippy::unnecessary_wraps)]
pub(super) fn arena_new_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    _args: &[Value],
) -> Result<NativeResult, Trap> {
    Ok(NativeResult::Word(heaps.alloc_arena()))
}

pub(super) fn arena_insert_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let arena = ref_arg(args, 0)?;
    // the one element word (the staging's one-word element policy)
    let handle = heaps.arena_insert(arena, args[1])?;
    Ok(NativeResult::Word(Value::I64(handle)))
}

pub(super) fn arena_remove_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let arena = ref_arg(args, 0)?;
    let handle = i64_arg(args, 1)?;
    heaps.arena_remove(arena, handle)?;
    // the inert unit word: no caller reads the overlay result
    Ok(NativeResult::Word(Value::I64(0)))
}

pub(super) fn arena_at_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let arena = ref_arg(args, 0)?;
    let handle = i64_arg(args, 1)?;
    // a `Ref` element answers as one retained count (the arena keeps
    // its own), a scalar word verbatim; a dangling handle traps here
    let word = heaps.arena_at(arena, handle)?;
    Ok(NativeResult::Word(word))
}
