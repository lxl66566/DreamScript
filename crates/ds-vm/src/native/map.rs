//! # The map natives (indices 28-31)
//!
//! The map surface over the builder-call interim: a map is one RC'd
//! heap object behind one `Ref` word — an association vector of key-value
//! pairs, string keys matching by content and every other word by
//! identity (see [`crate::object::Object::Map`]; no ordering or iteration
//! promise). `map_new` constructs the empty map — the `map[k, v]()`
//! constructor's runtime half — `map_len` counts entries, and `map_get`
//! is the checked read in exactly `list_get`'s span shape: in range is
//! [`PLATFORM_IO_OK`] with the value, a miss is [`PLATFORM_NONE`] with
//! the probe key as the payload (no new platform tag id: the map reuses
//! the checked-read reservation). `map_set` is the one mutation, through
//! the checker's exclusive `mut` window; it stores fresh pairs by
//! acquiring one count per `Ref` word and drops the overwritten value's
//! count, so a dying map frees with one dec per stored `Ref` word —
//! the same retain contract the list carries.
//!
//! `list_get` is the checked read, and its bounds behavior follows the
//! crate's checked-API convention — the same two-word span `parse_int`
//! answers: in range is [`PLATFORM_IO_OK`] with the element, out of
//! range (either side) is [`PLATFORM_NONE`] with the rejected index as
//! the payload. The string ops' clamping fits no generic element type
//! (there is no empty value of every `t`), and a trap would deny the
//! caller the `t | none` extraction, so the span is the one shape both
//! the caller and the checker can see.

use super::{list::native_result_span, sink::ref_arg, *};

/// `list_eq(a, b: list) -> bool` — the list collection's structural
/// content compare: the content walk over both objects' element
/// words, every nested reference pair compared the same way. Both
/// operands stay borrowed (the native call convention's staging owns no
/// count).
pub(super) fn list_eq_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    Ok(NativeResult::Word(Value::Bool(
        heaps.content_eq(ref_arg(args, 0)?, ref_arg(args, 1)?)?,
    )))
}

/// `map_eq(a, b: map) -> bool` — the map collection's structural content
/// compare: the content walk over both objects' rows, key lookup the
/// face `map_get` serves and value spans compared word for word. Both
/// operands stay borrowed.
pub(super) fn map_eq_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    Ok(NativeResult::Word(Value::Bool(
        heaps.content_eq(ref_arg(args, 0)?, ref_arg(args, 1)?)?,
    )))
}

// total: the Result is the calling convention every native shares, not
// this body's
#[allow(clippy::unnecessary_wraps)]
pub(super) fn map_new_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    _args: &[Value],
) -> Result<NativeResult, Trap> {
    Ok(NativeResult::Word(heaps.new_map()))
}

pub(super) fn map_len_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let n = i64::try_from(heaps.map_len(ref_arg(args, 0)?)?).expect("map length fits i64");
    Ok(NativeResult::Word(Value::I64(n)))
}

/// `map_keys[k, v](map) -> list[k]`: the ordered key face — the map's
/// entry order (the order rows were first stored), one word per key,
/// the fresh list's edge owning one count per `Ref` key (the sources
/// keep their own).
pub(super) fn map_keys_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let keys = heaps.map_keys(ref_arg(args, 0)?)?;
    for key in &keys {
        if let Value::Ref(word) = *key {
            heaps.inc_edge(word)?;
        }
    }
    Ok(NativeResult::Word(heaps.new_list_strided(keys, 1)))
}

pub(super) fn map_get_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let map = ref_arg(args, 0)?;
    let hit = heaps.map_at(map, args[1])?;
    Ok(match hit.as_deref() {
        Some([only]) => {
            // the one-word stride reads through the `Tag` shape: no span
            // buffer, and the slice dies before the count goes on
            if let Value::Ref(word) = *only {
                heaps.inc(word)?;
            }
            NativeResult::Tag {
                tag: PLATFORM_IO_OK,
                payload: *only,
            }
        },
        Some(words) => {
            // the read hands the value span out: a Ref word gains a count
            // for the reader per word, the map keeps its own — the
            // list checked read's own span shape
            native_result_span(heaps, words.to_vec())?
        },
        // a miss answers the `none` span member with the probe key, the
        // same checked-read shape `list_get` answers with the index
        None => NativeResult::Tag {
            tag: PLATFORM_NONE,
            payload: args[1],
        },
    })
}

pub(super) fn map_set_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    // the variadic tail is the value's whole inline word span, the
    // `list_push` staging shape at the value position
    if args.len() < 3 {
        return Err(Trap::ArityMismatch {
            expected: 3,
            given: args.len(),
        });
    }
    heaps.set_map(ref_arg(args, 0)?, args[1], &args[2..])?;
    // the inert unit word: no caller reads the overlay result
    Ok(NativeResult::Word(Value::I64(0)))
}

pub(super) fn map_remove_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    heaps.remove_map(ref_arg(args, 0)?, args[1])?;
    // the inert unit word: no caller reads the overlay result
    Ok(NativeResult::Word(Value::I64(0)))
}

pub(super) fn map_contains_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let hit = heaps.contains_map(ref_arg(args, 0)?, args[1])?;
    Ok(NativeResult::Word(Value::Bool(hit)))
}

pub(super) fn map_entries_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    // the entry-order pair list: the fresh list's edge owns one count
    // per `Ref` word of key and value alike (the map keeps its own),
    // the `map_keys` adoption discipline over the value words too
    let (words, stride) = heaps.map_entries(ref_arg(args, 0)?)?;
    for word in &words {
        if let Value::Ref(word) = *word {
            heaps.inc_edge(word)?;
        }
    }
    Ok(NativeResult::Word(heaps.new_list_strided(words, stride)))
}
