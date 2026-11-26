//! # The string natives (indices 11-18; `len` is generic at 10)
//!
//! The pure string surface, char-indexed: `len` counts unicode scalar
//! values, and `substring`/`char_at` index chars, never bytes, so
//! multi-byte text never cuts a code point. Char positions resolve
//! through the string object's lazily built boundary map (one
//! O(length) pass per string object, never per call), so `substring`
//! and `char_at` answer in O(range) once the bounds are known.
//! Both are checked reads in `list_get`'s span shape (the
//! checked-API convention): `substring` answers the slice between the
//! two positions when `0 <= start <= end` holds inside the string's
//! char count, `char_at` the char at an in-range position, and every
//! out-of-range request answers the [`PLATFORM_NONE`] member with the
//! rejected index as the payload — never clamping, never a trap.
//!
//! `concat` is the callable face of the `STRCAT` uop: interpolation
//! chains emit STRCAT directly, while both call spellings (`a.concat(b)`
//! and string-operand `a + b`) stage the two Ref words and dispatch this
//! native through CALL, so a pool `FuncRef` naming `concat` behaves like
//! any other builtin. `split` answers a list-object `Ref` (the same list kind
//! `fs_list` fills): the separator union's payload word carries a char's
//! code point in the integer slot or the separator text behind a Ref,
//! and an empty separator splits per character.
//!
//! `parse_int` is the pure fallible one: its ambient row is `error` (the
//! caller-handled failure, the `int | parse_err` shape exactly as
//! `read_file`'s row is `string | io_err`), and it
//! answers the same uniform two-word span — [`PLATFORM_IO_OK`] with the
//! parsed `int`, or [`PLATFORM_PARSE_ERR`] (the next id below `net_err`
//! in the platform reservation's countdown) with the position the reader
//! gave up at in the payload word.

use super::{
    scalar::fnv1a,
    sink::{i64_arg, ref_arg},
    *,
};
use crate::value::RefId;

pub(super) fn substring_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let id = ref_arg(args, 0)?;
    let start = i64_arg(args, 1)?;
    let end = i64_arg(args, 2)?;
    let n = i64::try_from(heaps.char_count(id)?).expect("string length fits i64");
    // the checked-API convention (the span shape `parse_int` answers):
    // the slice in range — `0 <= start <= end <= n`, so both offsets sit
    // on char boundaries and the cut cannot split a code point — answers
    // the payload Ref, every out-of-range shape (either side, inverted)
    // the `none` member with the rejected `start`, never clamping
    if !(0 <= start && start <= end && end <= n) {
        return Ok(NativeResult::Tag {
            tag: PLATFORM_NONE,
            payload: Value::I64(start),
        });
    }
    if start == end {
        return Ok(NativeResult::Tag {
            tag: PLATFORM_IO_OK,
            payload: heaps.new_str_part(""),
        });
    }
    let (begin, stop) = heaps.char_byte_range(
        id,
        usize::try_from(start).expect("in-range start fits usize"),
        usize::try_from(end).expect("in-range end fits usize"),
    )?;
    let part = heaps.alloc_substr(id, begin, stop)?;
    Ok(NativeResult::Tag {
        tag: PLATFORM_IO_OK,
        payload: heaps.word(part),
    })
}

pub(super) fn concat_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let joined = heaps.concat(ref_arg(args, 0)?, ref_arg(args, 1)?)?;
    Ok(NativeResult::Word(heaps.word(joined)))
}

pub(super) fn to_upper_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let text = heaps.str_at(ref_arg(args, 0)?)?.to_uppercase();
    Ok(NativeResult::Word(heaps.new_str(text)))
}

pub(super) fn to_lower_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let text = heaps.str_at(ref_arg(args, 0)?)?.to_lowercase();
    Ok(NativeResult::Word(heaps.new_str(text)))
}

pub(super) fn trim_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let id = ref_arg(args, 0)?;
    // the trimmed byte range, measured off the borrowed text: the fresh
    // object copies that range once and no intermediate string exists
    let (begin, stop) = {
        let text = heaps.str_at(id)?;
        let begin = text.len() - text.trim_start().len();
        let stop = text.trim_end().len();
        (begin, stop)
    };
    // an all-whitespace subject trims to the empty range
    if begin >= stop {
        return Ok(NativeResult::Word(heaps.new_str_part("")));
    }
    let part = heaps.alloc_substr(id, begin, stop)?;
    Ok(NativeResult::Word(heaps.word(part)))
}

pub(super) fn parse_int_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let id = ref_arg(args, 0)?;
    let text = heaps.str_at(id)?;
    Ok(match text.parse::<i64>() {
        // strict: no implicit whitespace handling (the `trim` native composes)
        Ok(v) => NativeResult::Tag {
            tag: PLATFORM_IO_OK,
            payload: Value::I64(v),
        },
        Err(_) => NativeResult::Tag {
            tag: PLATFORM_PARSE_ERR,
            payload: Value::I64(int_stop(text)),
        },
    })
}

/// The position an integer reader gives up at: the first char the
/// grammar refuses, or the text's end when the shape held but the value
/// did not (an overflow) or the digits are missing past the sign.
pub(super) fn int_stop(text: &str) -> i64 {
    let bytes = text.as_bytes();
    let mut i = 0usize;
    if i < bytes.len() && (bytes[i] == b'-' || bytes[i] == b'+') {
        i += 1;
    }
    let digits = bytes[i..].iter().take_while(|b| b.is_ascii_digit()).count();
    if digits == 0 {
        // no digits at all: the reader stopped where the digits should
        // have started (the empty text answers 0)
        return i64::try_from(i).expect("the index fits i64");
    }
    // past the digits: either the first char the grammar refuses, or the
    // end of a well-shaped text whose value overflowed
    i += digits;
    i64::try_from(i).expect("the index fits i64")
}

/// `parse_float(s: string) -> float | parse_err` — `parse_int`'s twin
/// over the float slot: the correctly-rounded decimal reader, strict
/// about spelling, answering `parse_err` with the position it gave up at
/// as the payload on refusal. The @std json parser's float arm routes
/// through here, so a parsed float carries the same bits any wider
/// ecosystem reader would give it.
pub(super) fn parse_float_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let id = ref_arg(args, 0)?;
    let text = heaps.str_at(id)?;
    Ok(match text.parse::<f64>() {
        // strict: no implicit whitespace handling (the `trim` native composes)
        Ok(v) => NativeResult::Tag {
            tag: PLATFORM_IO_OK,
            payload: Value::F64(v),
        },
        Err(_) => NativeResult::Tag {
            tag: PLATFORM_PARSE_ERR,
            payload: Value::I64(float_stop(text)),
        },
    })
}

/// The position a float reader gives up at: the first char the decimal
/// grammar refuses (sign, mantissa digits with one dot, exponent), or
/// the text's end when the shape held but the value did not. The
/// non-decimal spellings (`inf`, `nan`) scan as their word prefixes.
pub(super) fn float_stop(text: &str) -> i64 {
    let bytes = text.as_bytes();
    let mut i = 0usize;
    if i < bytes.len() && (bytes[i] == b'-' || bytes[i] == b'+') {
        i += 1;
    }
    let lower: Vec<u8> = text.to_ascii_lowercase().as_bytes()[i..].to_vec();
    for word in [b"inf".as_slice(), b"infinity".as_slice(), b"nan".as_slice()] {
        if lower.len() >= word.len() && &lower[..word.len()] == word {
            return i64::try_from(text.len()).expect("the index fits i64");
        }
    }
    // where the mantissa's first digit was required: a text with no
    // digit anywhere stops there (a bare dot is not a number)
    let digits_at = i;
    let mut mantissa = 0usize;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
        mantissa += 1;
    }
    if i < bytes.len() && bytes[i] == b'.' {
        i += 1;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
            mantissa += 1;
        }
    }
    if mantissa == 0 {
        return i64::try_from(digits_at).expect("the index fits i64");
    }
    if i < bytes.len() && (bytes[i] == b'e' || bytes[i] == b'E') {
        i += 1;
        if i < bytes.len() && (bytes[i] == b'-' || bytes[i] == b'+') {
            i += 1;
        }
        let exp_digits = bytes[i..].iter().take_while(|b| b.is_ascii_digit()).count();
        if exp_digits == 0 {
            return i64::try_from(i).expect("the index fits i64");
        }
        i += exp_digits;
    }
    i64::try_from(i).expect("the index fits i64")
}

/// `char_of(code: int) -> char | none` — the codepoint→char reader:
/// the one seam a ds-source `\u` decoder needs (codepoint arithmetic
/// stays in ds, the scalar range stays native). A `code` outside the
/// scalar range answers `none`, the payload word carrying the rejected
/// code; a hit answers the code point itself, the char word's own
/// unboxed spelling.
pub(super) fn char_of_fn(
    _heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let code = i64_arg(args, 0)?;
    Ok(match u32::try_from(code).ok().and_then(char::from_u32) {
        Some(_) => NativeResult::Tag {
            tag: PLATFORM_IO_OK,
            payload: Value::I64(code),
        },
        None => NativeResult::Tag {
            tag: PLATFORM_NONE,
            payload: Value::I64(code),
        },
    })
}

/// `code_of(c: char) -> int` — `char_of`'s twin, the char→codepoint
/// reader the @std json serializer's control-char escaping spells its
/// `\u00XX` forms through. The char word IS the code point, so the read
/// hands the integer straight back.
pub(super) fn code_of_fn(
    _heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    Ok(NativeResult::Word(Value::I64(i64_arg(args, 0)?)))
}

/// The char one staged integer spells: the code point validated against
/// the scalar range, a hand-assembled frame's out-of-range word trapping
/// at its own slot.
pub(super) fn char_word_at(code: i64, slot: u8) -> Result<char, Trap> {
    u32::try_from(code)
        .ok()
        .and_then(char::from_u32)
        .ok_or(Trap::SlotTypeMismatch {
            slot,
            expected: SlotType::I64,
            actual: SlotType::I64,
        })
}

/// `bytes_of(xs: list[byte]) -> bytes` — the byte string's construction
/// seam: the byte list folds into one counted block, one payload byte
/// per element word. The list keeps its own counts (a borrow exactly
/// like every native staging), and the bytes word's birth count is the
/// calling frame's. An element word outside `0..=255` traps: the
/// checker types the list, so only a hand-assembled frame reaches it.
pub(super) fn bytes_of_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let id = ref_arg(args, 0)?;
    let elems = {
        let (elems, stride) = heaps.list_at(id)?;
        if stride > 1 {
            return Err(Trap::ElementStride {
                obj: id.id(),
                stride,
                given: 1,
            });
        }
        elems.to_vec()
    };
    let mut payload = Vec::with_capacity(elems.len());
    for word in elems {
        let Value::I64(byte) = word else {
            return Err(Trap::SlotTypeMismatch {
                slot: 0,
                expected: SlotType::I64,
                actual: word.slot_type(),
            });
        };
        payload.push(u8::try_from(byte).map_err(|_| Trap::SlotTypeMismatch {
            slot: 0,
            expected: SlotType::I64,
            actual: SlotType::I64,
        })?);
    }
    Ok(NativeResult::Word(
        heaps.new_bytes(payload.into_boxed_slice()),
    ))
}

/// `byte_list(b: bytes) -> list[byte]` — the bridge back out: one fresh
/// list, one word per payload byte, the bytes keeping their own count.
pub(super) fn byte_list_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let id = ref_arg(args, 0)?;
    let elems: Vec<Value> = heaps
        .bytes_at(id)?
        .iter()
        .map(|byte| Value::I64(i64::from(*byte)))
        .collect();
    Ok(NativeResult::Word(heaps.new_list(elems)))
}

/// `byte_at(b: bytes, i: int) -> byte | none` — the checked indexed read,
/// the ok/none span shape `list_get` answers over the one-word byte
/// payload: the in-range byte under the success discriminant, every
/// out-of-range shape the `none` member with the rejected index —
/// never clamping and never a trap.
pub(super) fn byte_at_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let id = ref_arg(args, 0)?;
    let index = i64_arg(args, 1)?;
    let payload = heaps.bytes_at(id)?;
    Ok(
        match usize::try_from(index).ok().filter(|&at| at < payload.len()) {
            Some(at) => NativeResult::Tag {
                tag: PLATFORM_IO_OK,
                payload: Value::I64(i64::from(payload[at])),
            },
            None => NativeResult::Tag {
                tag: PLATFORM_NONE,
                payload: Value::I64(index),
            },
        },
    )
}

/// `contains(haystack, needle: string) -> bool` — the string search
/// family's membership read: the
/// needle occurs in the haystack, the empty needle always occurring.
/// Both operands borrow shared, so neither side copies out.
pub(super) fn contains_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let haystack = heaps.str_ref(ref_arg(args, 0)?)?;
    let needle = heaps.str_ref(ref_arg(args, 1)?)?;
    Ok(NativeResult::Word(Value::Bool(haystack.contains(needle))))
}

/// `index_of(haystack, needle: string) -> int | none` — the checked
/// search: the first occurrence's char index under the ok discriminant
/// (`substring`/`char_at`'s indexing, so the answer composes with the
/// checked slice), a miss the `none` member with the inert unit word as
/// payload. The empty needle sits at index 0.
pub(super) fn index_of_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let haystack = heaps.str_ref(ref_arg(args, 0)?)?;
    let needle = heaps.str_ref(ref_arg(args, 1)?)?;
    Ok(match haystack.find(needle) {
        // find's byte offset sits on a char boundary
        Some(byte) => NativeResult::Tag {
            tag: PLATFORM_IO_OK,
            payload: Value::I64(
                i64::try_from(haystack[..byte].chars().count()).expect("char count fits i64"),
            ),
        },
        None => NativeResult::Tag {
            tag: PLATFORM_NONE,
            payload: Value::I64(0),
        },
    })
}

/// `replace(haystack, needle, with: string) -> string` — every
/// non-overlapping occurrence swapped for `with` in one left-to-right
/// pass, the inserted text never re-scanned; no match answers the
/// input's own text, and the empty needle inserts `with` at every char
/// boundary.
pub(super) fn replace_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let replaced = {
        let haystack = heaps.str_ref(ref_arg(args, 0)?)?;
        let needle = heaps.str_ref(ref_arg(args, 1)?)?;
        let with = heaps.str_ref(ref_arg(args, 2)?)?;
        haystack.replace(needle, with)
    };
    Ok(NativeResult::Word(heaps.new_str(replaced)))
}

/// `bytes_eq(a, b: bytes) -> bool` — the structural content compare:
/// the payloads compare byte for byte, a length mismatch a
/// certain false. Both operands stay borrowed (the native call
/// convention's staging owns no count).
pub(super) fn bytes_eq_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    // both operands borrow shared, so neither side copies out
    let a = heaps.bytes_ref(ref_arg(args, 0)?)?;
    let b = heaps.bytes_ref(ref_arg(args, 1)?)?;
    Ok(NativeResult::Word(Value::Bool(a == b)))
}

/// `bytes_compare(a, b: bytes) -> int` — the bytes sort key: the
/// lexicographic order over the payloads, a shorter prefix ordering
/// first. Both operands stay borrowed.
pub(super) fn bytes_compare_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    // both operands borrow shared, so neither side copies out
    let a = heaps.bytes_ref(ref_arg(args, 0)?)?;
    let b = heaps.bytes_ref(ref_arg(args, 1)?)?;
    Ok(NativeResult::Word(Value::I64(match a.cmp(b) {
        std::cmp::Ordering::Less => -1,
        std::cmp::Ordering::Equal => 0,
        std::cmp::Ordering::Greater => 1,
    })))
}

/// `bytes_hash(b: bytes) -> int` — the bytes content hash, FNV-1a over
/// the payload under the hash family's one scheme.
pub(super) fn bytes_hash_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    // the operand borrows shared, so the hash reads without a copy
    let payload = heaps.bytes_ref(ref_arg(args, 0)?)?;
    Ok(NativeResult::Word(Value::I64(fnv1a(payload))))
}

/// `utf8_encode(s: string) -> bytes` — the string→bytes bridge: the
/// string's own UTF-8 bytes become one fresh counted block, the string
/// keeping its own count.
pub(super) fn utf8_encode_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let payload = heaps.str_at(ref_arg(args, 0)?)?.as_bytes().to_vec();
    Ok(NativeResult::Word(
        heaps.new_bytes(payload.into_boxed_slice()),
    ))
}

/// `utf8_decode(b: bytes) -> string | utf8_err` — the bytes→string
/// bridge: the payload validates as UTF-8 on the way back, an invalid
/// sequence answering the taxonomy's `utf8_err` tag with the rejection
/// message in the payload word.
pub(super) fn utf8_decode_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    // the routed read borrows the store, so the operand copies out
    let payload = heaps.bytes_at(ref_arg(args, 0)?)?.to_vec();
    match String::from_utf8(payload) {
        Ok(text) => Ok(NativeResult::Tag {
            tag: PLATFORM_IO_OK,
            payload: heaps.new_str(text),
        }),
        Err(_) => Ok(NativeResult::Tag {
            tag: PLATFORM_UTF8_ERR,
            payload: heaps.new_str_part("invalid utf-8 payload"),
        }),
    }
}

/// How `split` cuts the subject and `join` renders around it. The
/// surface separator is a `char` (one scalar value), and an empty string
/// one splits per character rather than trap.
pub(super) enum SplitSep {
    /// An empty separator: one part per character. The empty spelling
    /// rides the string member (a char word is never empty), so lowered
    /// code stages it through the string member; the arm answers those
    /// frames and hand-assembled ones alike.
    PerChar,
    /// One character: the split scans the subject's chars once.
    Byte(char),
    /// Any other separator text, owned because the store's routed string
    /// read borrows the whole store mutably.
    Text(String),
}

/// The separator union's staged span read back: the region's words by
/// kind (the text Ref beside the char's code point integer), the staged
/// tag word matched against the char member's tag id the call stages
/// beside the span — tag ids being per-compile facts, the lowering hands
/// the body its own. A hand-assembled frame staging no shape the span
/// admits traps at its slot.
pub(super) fn separator(heaps: &mut Heaps, args: &[Value]) -> Result<SplitSep, Trap> {
    // exactly one region word holds each kind: the text member's Ref and
    // the char member's I64, whichever region order the solved set
    // sorted the members into
    let (text_at, code) = match (args[2], args[3]) {
        (Value::Ref(_), Value::I64(code)) => (2, code),
        (Value::I64(code), Value::Ref(_)) => (3, code),
        (a, b) => {
            return Err(Trap::SlotTypeMismatch {
                slot: 2,
                expected: SlotType::Raw,
                actual: if matches!(a, Value::Ref(_) | Value::I64(_)) {
                    b.slot_type()
                } else {
                    a.slot_type()
                },
            })
        },
    };
    let char_tag = i64_arg(args, 4)?;
    let is_char = matches!(args[1], Value::Tag(tag) if i64::from(tag) == char_tag);
    if is_char {
        return Ok(SplitSep::Byte(char_word_at(code, 2)?));
    }
    let sep = heaps.str_at(ref_arg(args, text_at)?)?;
    Ok(match sep {
        "" => SplitSep::PerChar,
        sep if sep.chars().count() == 1 => {
            SplitSep::Byte(sep.chars().next().expect("one char is one char"))
        },
        sep => SplitSep::Text(sep.to_owned()),
    })
}

pub(super) fn split_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let text_id = ref_arg(args, 0)?;
    let sep = separator(heaps, args)?;
    // the part boundaries come off one borrow of the subject; the parts
    // materialize after it ends
    let mut bounds: Vec<(usize, usize)> = Vec::new();
    {
        let text = heaps.str_at(text_id)?;
        let mut begin = 0;
        match &sep {
            SplitSep::PerChar => {
                for part in text.chars() {
                    bounds.push((begin, begin + part.len_utf8()));
                    begin += part.len_utf8();
                }
            },
            SplitSep::Byte(sep) => {
                for part in text.split(*sep) {
                    bounds.push((begin, begin + part.len()));
                    begin += part.len() + sep.len_utf8();
                }
            },
            SplitSep::Text(sep) => {
                for part in text.split(sep.as_str()) {
                    bounds.push((begin, begin + part.len()));
                    begin += part.len() + sep.len();
                }
            },
        }
    }
    // every boundary sits on a char boundary, so each part copies its own
    // byte range once
    let mut elems = Vec::with_capacity(bounds.len());
    for (begin, stop) in bounds {
        let id = heaps.alloc_substr(text_id, begin, stop)?;
        elems.push(heaps.word(id));
    }
    // each fresh part's birth count becomes the list's edge, so the
    // frame claim its alloc entered leaves with the adoption
    for elem in &elems {
        if let Value::Ref(word) = *elem {
            heaps.release_claim(word);
        }
    }
    Ok(NativeResult::Word(heaps.new_list(elems)))
}

pub(super) fn char_at_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let id = ref_arg(args, 0)?;
    let index = i64_arg(args, 1)?;
    // the checked char read in `list_get`'s span shape: an in-range index
    // answers the one-scalar char word (the hit payload a one-char
    // string Ref rides, exactly the word `char_of` mints), either side
    // out of range the `none` member with the rejected index — never a
    // trap
    let n = heaps.char_count(id)?;
    if index < 0 || usize::try_from(index).unwrap_or(usize::MAX) >= n {
        return Ok(NativeResult::Tag {
            tag: PLATFORM_NONE,
            payload: Value::I64(index),
        });
    }
    let i = usize::try_from(index).expect("in-range index fits usize");
    let (begin, stop) = heaps.char_byte_range(id, i, i + 1)?;
    let code = heaps.str_at(id)?[begin..stop]
        .chars()
        .next()
        .map_or(0, |c| i64::from(u32::from(c)));
    Ok(NativeResult::Tag {
        tag: PLATFORM_IO_OK,
        payload: Value::I64(code),
    })
}

pub(super) fn join_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    // the separator union's staged span: the live region word carries
    // the text Ref or the code point integer (the reader's own rules)
    let sep = match separator(heaps, args)? {
        SplitSep::PerChar => String::new(),
        SplitSep::Byte(c) => c.to_string(),
        SplitSep::Text(text) => text,
    };
    let (elems, stride) = heaps.list_at(ref_arg(args, 0)?)?;
    // one string Ref per element is the surface's contract (list[string]);
    // a wider stride reaches here only hand-assembled
    if stride > 1 {
        return Err(Trap::ElementStride {
            obj: ref_arg(args, 0)?.id(),
            stride,
            given: 1,
        });
    }
    // the element words copy out of the list's borrow: the routed string
    // reads below borrow the whole store mutably
    let elems: Vec<Value> = elems.to_vec();
    // the joined text's exact byte length first, so its buffer is
    // allocated once at the final size: growth would copy the text again,
    // and the fresh object's exact-size conversion would shrink it
    let mut capacity = sep.len() * elems.len().saturating_sub(1);
    for (at, part) in elems.iter().enumerate() {
        capacity += heaps.str_at(join_element(*part, at)?)?.len();
    }
    let mut joined = String::with_capacity(capacity);
    for (at, part) in elems.iter().enumerate() {
        if at > 0 {
            joined.push_str(&sep);
        }
        joined.push_str(heaps.str_at(join_element(*part, at)?)?);
    }
    Ok(NativeResult::Word(heaps.new_str(joined)))
}

/// A join element's string word, refusing at the element's own position:
/// the checked surface guarantees `list[string]`, so only a hand-assembled
/// caller reaches the miss.
pub(super) fn join_element(part: Value, at: usize) -> Result<RefId, Trap> {
    match part {
        Value::Ref(word) => Ok(word),
        other => Err(Trap::SlotTypeMismatch {
            slot: u8::try_from(at).expect("element index fits u8"),
            expected: SlotType::Ref,
            actual: other.slot_type(),
        }),
    }
}
