//! The scalar width bridges, the float math family, the compare and
//! hash natives, and the scalar-to-text conversions.

use super::{sink::f64_arg, *};

pub(super) fn total_compare_fn(
    _heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    match args {
        [Value::F64(a), Value::F64(b)] => {
            Ok(NativeResult::Word(Value::I64(match a.total_cmp(b) {
                std::cmp::Ordering::Less => -1,
                std::cmp::Ordering::Equal => 0,
                std::cmp::Ordering::Greater => 1,
            })))
        },
        _ => Err(Trap::SlotTypeMismatch {
            slot: 0,
            expected: SlotType::F64,
            actual: args.first().map_or(SlotType::I64, Value::slot_type),
        }),
    }
}

pub(super) fn str_compare_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    // both operands borrow shared, so neither side copies out
    let a = heaps.str_ref(ref_arg(args, 0)?)?;
    let b = heaps.str_ref(ref_arg(args, 1)?)?;
    Ok(NativeResult::Word(Value::I64(
        match a.as_bytes().cmp(b.as_bytes()) {
            std::cmp::Ordering::Less => -1,
            std::cmp::Ordering::Equal => 0,
            std::cmp::Ordering::Greater => 1,
        },
    )))
}

pub(super) fn char_compare_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    // both operands borrow shared, so neither side copies out
    let a = heaps.str_ref(ref_arg(args, 0)?)?;
    let b = heaps.str_ref(ref_arg(args, 1)?)?;
    Ok(NativeResult::Word(Value::I64(
        match a.chars().cmp(b.chars()) {
            std::cmp::Ordering::Less => -1,
            std::cmp::Ordering::Equal => 0,
            std::cmp::Ordering::Greater => 1,
        },
    )))
}

pub(super) fn byte_compare_fn(
    _heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    Ok(NativeResult::Word(Value::I64(
        match i64_arg(args, 0)?.cmp(&i64_arg(args, 1)?) {
            std::cmp::Ordering::Less => -1,
            std::cmp::Ordering::Equal => 0,
            std::cmp::Ordering::Greater => 1,
        },
    )))
}

/// The one content-hash algorithm the hash family uses: FNV-1a over the
/// bytes (offset basis 14695981039346656037, prime 1099511628211, both
/// wrapping in u64) — the same pair the derive's fold mixes through, so
/// every primitive part hashes under one scheme.
// the fold's answer is the full 64-bit pattern reinterpreted as the
// content-hash word
#[allow(clippy::cast_possible_wrap)]
pub(super) fn fnv1a(bytes: &[u8]) -> i64 {
    let mut hash = 14_695_981_039_346_656_037u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(1_099_511_628_211);
    }
    hash as i64
}

pub(super) fn str_hash_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    // the operand borrows shared, so the hash reads without a copy
    let text = heaps.str_ref(ref_arg(args, 0)?)?;
    Ok(NativeResult::Word(Value::I64(fnv1a(text.as_bytes()))))
}

pub(super) fn float_hash_fn(
    _heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    match args {
        [Value::F64(f)] => {
            // the fold: -0.0 shares +0.0's bits and every NaN payload the
            // one canonical NaN's, so bit-structurally equal floats hash
            // equal
            let canonical = if f.is_nan() {
                f64::NAN
            } else if *f == 0.0 {
                0.0
            } else {
                *f
            };
            Ok(NativeResult::Word(Value::I64(fnv1a(
                &canonical.to_bits().to_le_bytes(),
            ))))
        },
        _ => Err(Trap::SlotTypeMismatch {
            slot: 0,
            expected: SlotType::F64,
            actual: args.first().map_or(SlotType::I64, Value::slot_type),
        }),
    }
}

pub(super) fn char_hash_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    // the routed read borrows the store, so the operand copies out
    let text = heaps.str_at(ref_arg(args, 0)?)?.to_owned();
    Ok(NativeResult::Word(Value::I64(fnv1a(text.as_bytes()))))
}

/// `byte_of(n: int) -> byte | none` — the checked byte construction: the
/// number's word under the ok discriminant, none outside 0-255 with the
/// rejected number in the payload (byte_at's span shape).
pub(super) fn byte_of_fn(
    _heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let n = i64_arg(args, 0)?;
    Ok(match u8::try_from(n) {
        Ok(b) => NativeResult::Tag {
            tag: PLATFORM_IO_OK,
            payload: Value::I64(i64::from(b)),
        },
        Err(_) => NativeResult::Tag {
            tag: PLATFORM_NONE,
            payload: Value::I64(n),
        },
    })
}

/// `int_of_byte(b: byte) -> int` — the byte's 0-255 number.
pub(super) fn int_of_byte_fn(
    _heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let b = i64_arg(args, 0)?;
    Ok(NativeResult::Word(Value::I64(b)))
}

/// `u32_of(n: int) -> u32 | none` — the checked u32 construction: the
/// number's word under the ok discriminant, none outside the u32 range.
pub(super) fn u32_of_fn(
    _heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let n = i64_arg(args, 0)?;
    Ok(match u32::try_from(n) {
        Ok(u) => NativeResult::Tag {
            tag: PLATFORM_IO_OK,
            payload: Value::I64(i64::from(u)),
        },
        Err(_) => NativeResult::Tag {
            tag: PLATFORM_NONE,
            payload: Value::I64(n),
        },
    })
}

/// `int_of_u32(v: u32) -> int` — the u32's number.
pub(super) fn int_of_u32_fn(
    _heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let v = i64_arg(args, 0)?;
    Ok(NativeResult::Word(Value::I64(v)))
}

/// The float math family's one-argument shape: IEEE over the f64 word, total by construction —
/// no operand shape traps (a negative `sqrt` answers NaN).
macro_rules! float_unary {
    ($name:ident, $op:expr) => {
        pub(super) fn $name(
            _heaps: &mut Heaps,
            _out: &mut OutputSink,
            args: &[Value],
        ) -> Result<NativeResult, Trap> {
            Ok(NativeResult::Word(Value::F64($op(f64_arg(args, 0)?))))
        }
    };
}

float_unary!(sqrt_fn, f64::sqrt);
float_unary!(sin_fn, f64::sin);
float_unary!(cos_fn, f64::cos);
float_unary!(floor_fn, f64::floor);
float_unary!(ceil_fn, f64::ceil);
float_unary!(abs_fn, f64::abs);
float_unary!(round_fn, f64::round);
float_unary!(trunc_fn, f64::trunc);
float_unary!(log_fn, f64::ln);
float_unary!(exp_fn, f64::exp);
float_unary!(tan_fn, f64::tan);

/// `atan2(y: float, x: float) -> float` — the two-argument arctangent:
/// the quadrant reads off both operands' signs, the signed zeros
/// included (`atan2(0, -1)` answers pi, `atan2(-0, -1)` negative pi).
pub(super) fn atan2_fn(
    _heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let y = f64_arg(args, 0)?;
    let x = f64_arg(args, 1)?;
    Ok(NativeResult::Word(Value::F64(y.atan2(x))))
}

/// `pow(base: float, exp: float) -> float` — the exponentiation,
/// `powf`'s IEEE edge answers (a negative base to a non-integer
/// exponent answers NaN, never a trap).
pub(super) fn pow_fn(
    _heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let base = f64_arg(args, 0)?;
    let exp = f64_arg(args, 1)?;
    Ok(NativeResult::Word(Value::F64(base.powf(exp))))
}

/// A scalar's rendered text, in a fixed stack buffer: `str_int` and
/// `str_float` hand the bytes to the fresh object as one exact-size
/// allocation, where `to_string` grows a `String` whose move into a
/// `Box<str>` then shrinks that buffer again.
pub(super) struct ScalarText {
    bytes: [u8; ScalarText::CAP],
    len: usize,
}

impl ScalarText {
    /// `f64`'s Display spells every digit out, so the smallest subnormal
    /// (5e-324, 326 bytes) and its negative form are the widest.
    const CAP: usize = 384;

    pub(super) fn new() -> Self {
        Self {
            bytes: [0; Self::CAP],
            len: 0,
        }
    }

    /// The rendered text; the sink only ever stores whole `&str` chunks.
    fn as_str(&self) -> &str {
        core::str::from_utf8(&self.bytes[..self.len]).expect("the sink stores &str chunks")
    }
}

impl core::fmt::Write for ScalarText {
    fn write_str(&mut self, text: &str) -> core::fmt::Result {
        let end = self.len + text.len();
        let room = self.bytes.get_mut(self.len..end).ok_or(core::fmt::Error)?;
        room.copy_from_slice(text.as_bytes());
        self.len = end;
        Ok(())
    }
}

/// One scalar's fresh string object: the stack render, falling back to the
/// growing path for a rendering past the buffer.
pub(super) fn scalar_str(heaps: &mut Heaps, scalar: impl core::fmt::Display) -> Value {
    let mut text = ScalarText::new();
    match core::fmt::write(&mut text, format_args!("{scalar}")) {
        Ok(()) => heaps.new_str_part(text.as_str()),
        Err(core::fmt::Error) => heaps.new_str(scalar.to_string()),
    }
}

pub(super) fn str_int_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    match args {
        [Value::I64(v)] => Ok(NativeResult::Word(scalar_str(heaps, *v))),
        _ => Err(Trap::SlotTypeMismatch {
            slot: 0,
            expected: SlotType::I64,
            actual: args.first().map_or(SlotType::I64, Value::slot_type),
        }),
    }
}

pub(super) fn str_float_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    match args {
        [Value::F64(v)] => Ok(NativeResult::Word(scalar_str(heaps, *v))),
        _ => Err(Trap::SlotTypeMismatch {
            slot: 0,
            expected: SlotType::F64,
            actual: args.first().map_or(SlotType::I64, Value::slot_type),
        }),
    }
}

pub(super) fn str_fixed_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    match args {
        [Value::F64(v), Value::I64(digits)] => {
            // the widest exact f64 fraction bounds the width; more
            // digits past it only answer trailing zeros the value
            // cannot distinguish
            // the clamp pins the width non-negative, so the usize cast
            // cannot lose the sign
            let digits = (*digits).clamp(0, 767);
            let digits = usize::try_from(digits).expect("the clamped width fits usize");
            Ok(NativeResult::Word(heaps.new_str(format!("{v:.digits$}"))))
        },
        _ => Err(Trap::SlotTypeMismatch {
            slot: 0,
            expected: SlotType::F64,
            actual: args.first().map_or(SlotType::I64, Value::slot_type),
        }),
    }
}

pub(super) fn str_bool_fn(
    heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    match args {
        [Value::Bool(v)] => Ok(NativeResult::Word(heaps.new_intern(if *v {
            "true"
        } else {
            "false"
        }))),
        _ => Err(Trap::SlotTypeMismatch {
            slot: 0,
            expected: SlotType::Bool,
            actual: args.first().map_or(SlotType::I64, Value::slot_type),
        }),
    }
}
