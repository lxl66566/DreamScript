//! The in-memory module value: header data, functions, constants, and
//! debug spans.

use ds_abi::{EncodeError, FrameLayout, Inst, SlotType, Uop};

use crate::{
    hash::Hash128,
    header::{CollectorHooks, Dep, Flags, TargetClass},
};

/// One entry of the locked constants payload: an i64, f64, bool, UTF-8
/// string, or function reference (G3.5).
///
/// f64 values are stored and restored by bit pattern, so NaN payloads
/// survive a round trip unchanged.
#[derive(Clone, PartialEq, Debug)]
pub enum Constant {
    /// 64-bit signed integer.
    I64(i64),
    /// 64-bit float, serialized by bit pattern.
    F64(f64),
    /// Boolean.
    Bool(bool),
    /// UTF-8 string bytes.
    Str(String),
    /// A qualified callee reference for CALL/CALLT pool operands: the
    /// callee's module name and its function index within that module.
    /// An empty `module` names the enclosing module itself (a same-module
    /// ref). The loader resolves names to slots at install time.
    FuncRef {
        /// Callee module name; empty for the enclosing module.
        module: String,
        /// Callee function index within `module`'s blocks payload.
        func: u32,
    },
}

/// A constants-pool dedupe key: the variant tag plus the value, with an f64
/// keyed by its bit pattern so a NaN payload survives as its own key.
///
/// `Eq` is the marker [`std::collections::HashMap`] needs; the derived
/// `PartialEq` still compares f64 by value, so `0.0` and `-0.0` are equal yet
/// hash apart and a repeated NaN never matches itself. Both only cost a
/// missed dedupe: no two distinct bit patterns are ever merged onto one pool
/// entry.
impl Eq for Constant {}

impl core::hash::Hash for Constant {
    /// Bit-exact for every variant.
    fn hash<H: core::hash::Hasher>(&self, state: &mut H) {
        match self {
            Self::I64(value) => {
                0u8.hash(state);
                value.hash(state);
            },
            Self::F64(value) => {
                1u8.hash(state);
                value.to_bits().hash(state);
            },
            Self::Bool(value) => {
                2u8.hash(state);
                value.hash(state);
            },
            Self::Str(text) => {
                3u8.hash(state);
                text.hash(state);
            },
            Self::FuncRef { module, func } => {
                4u8.hash(state);
                module.hash(state);
                func.hash(state);
            },
        }
    }
}

/// A half-open byte-offset span `[start, end)` into the module source,
/// used for strippable debug information.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct DebugSpan {
    /// First byte offset, inclusive.
    pub start: u32,
    /// Last byte offset, exclusive.
    pub end: u32,
}

impl DebugSpan {
    /// Creates a span; `start` must not exceed `end`.
    ///
    /// # Panics
    ///
    /// Panics if `start > end`.
    #[must_use]
    pub const fn new(start: u32, end: u32) -> Self {
        assert!(start <= end, "span start must not exceed end");
        Self { start, end }
    }
}

/// Why a [`Function`] cannot be built.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum FunctionError {
    /// A jump target is at or past the end of the function's own code.
    #[error("jump target {target} at pc {pc} escapes the function body ({len} words)")]
    JumpEscapesBody {
        /// Instruction index of the jump.
        pc: u32,
        /// The escaping target.
        target: u32,
        /// The function's code length, in words.
        len: u32,
    },
    /// An instruction cannot be encoded at its pc (bias window overflow).
    #[error("instruction cannot be encoded at its pc: {0:?}")]
    Encode(EncodeError),
}

/// One per-function record of the blocks payload (BL-7): local name, frame
/// layout, its constant pool, and the validated body as canonical 32-bit
/// words — uops interleaved with the 0xE RC pair (C4).
///
/// Construction validates the body once: every word encodes canonically at
/// its pc, and every jump lands inside the function's own code. The record
/// keeps the encoded words, so the writer copies them out verbatim and the
/// loader takes them as-is: one encode and one decode per word on each
/// direction, no re-encoding.
#[derive(Clone, PartialEq, Debug)]
pub struct Function {
    name: String,
    layout: FrameLayout,
    constants: Vec<Constant>,
    code: Vec<u32>,
}

impl Function {
    /// Builds a validated function record.
    ///
    /// # Errors
    ///
    /// Returns [`FunctionError::JumpEscapesBody`] when a jump target is at
    /// or past the code length, and [`FunctionError::Encode`] when a word
    /// cannot be encoded at its pc. The body accepts any mix of uops and
    /// RC ops (a bare uop stream is the common pure-uop fixture).
    pub fn new(
        name: impl Into<String>,
        layout: FrameLayout,
        constants: Vec<Constant>,
        code: impl IntoIterator<Item = impl Into<Inst>>,
    ) -> Result<Self, FunctionError> {
        let body: Vec<Inst> = code.into_iter().map(Into::into).collect();
        let len = u32::try_from(body.len()).expect("code length fits u32");
        let mut words = Vec::with_capacity(body.len());
        for (pc, inst) in body.iter().enumerate() {
            let pc = u32::try_from(pc).expect("pc fits u32");
            words.push(inst.encode(pc).map_err(FunctionError::Encode)?);
            if let Inst::Uop(uop) = inst {
                if let Some(target) = jump_target_of(*uop) {
                    if target >= len {
                        return Err(FunctionError::JumpEscapesBody { pc, target, len });
                    }
                }
            }
        }
        Ok(Self {
            name: name.into(),
            layout,
            constants,
            code: words,
        })
    }

    /// Builds a record from words the caller has already validated (the
    /// reader decodes and bounds-checks each word exactly once).
    pub(crate) fn from_words(
        name: String,
        layout: FrameLayout,
        constants: Vec<Constant>,
        code: Vec<u32>,
    ) -> Self {
        Self {
            name,
            layout,
            constants,
            code,
        }
    }

    /// The local function name; the qualified module path lives in the
    /// header's module name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The static frame layout (BL-6).
    #[must_use]
    pub fn layout(&self) -> &FrameLayout {
        &self.layout
    }

    /// The function's constant pool, indexed by uop `ConstIdx` operands.
    #[must_use]
    pub fn constants(&self) -> &[Constant] {
        &self.constants
    }

    /// The validated instruction body: one canonical 32-bit word per
    /// instruction, in pc order. This is the on-disk form.
    #[must_use]
    pub fn code(&self) -> &[u32] {
        &self.code
    }

    /// Consumes the record into its parts: local name, frame layout,
    /// constant pool, and the validated body words. The owning install
    /// path takes each record over this seam, so the words reach the
    /// installed table without a copy.
    #[must_use]
    pub fn into_parts(self) -> (String, FrameLayout, Vec<Constant>, Vec<u32>) {
        (self.name, self.layout, self.constants, self.code)
    }
}

/// The absolute jump target of a jump uop, or `None` for non-jumps.
pub(crate) fn jump_target_of(uop: Uop) -> Option<u32> {
    match uop {
        Uop::Jmp { target } | Uop::Jmpif { target, .. } | Uop::JmpifNot { target, .. } => {
            Some(target)
        },
        _ => None,
    }
}

/// The compact wire byte for one [`SlotType`]: I32=0, I64=1, F64=2, Bool=3,
/// Ref=4, Tag=5, Raw=6, Fn=7. Tags 6 and 7 are additive payload-v1
/// extensions: readers that predate them reject the byte as an unknown slot
/// type, so no payload version bump is needed and existing writers stay
/// byte-stable.
pub(crate) const fn slot_tag(slot: SlotType) -> u8 {
    match slot {
        SlotType::I32 => 0,
        SlotType::I64 => 1,
        SlotType::F64 => 2,
        SlotType::Bool => 3,
        SlotType::Ref => 4,
        SlotType::Tag => 5,
        SlotType::Raw => 6,
        SlotType::Fn => 7,
    }
}

/// Parses a compact slot-type byte; `None` for unknown tags.
pub(crate) const fn slot_from_tag(tag: u8) -> Option<SlotType> {
    match tag {
        0 => Some(SlotType::I32),
        1 => Some(SlotType::I64),
        2 => Some(SlotType::F64),
        3 => Some(SlotType::Bool),
        4 => Some(SlotType::Ref),
        5 => Some(SlotType::Tag),
        6 => Some(SlotType::Raw),
        7 => Some(SlotType::Fn),
        _ => None,
    }
}

/// A dsmod module: the full header plus the sections this milestone
/// carries in memory.
///
/// The type-metadata and effect-rows sections are versioned placeholders
/// with no in-memory data yet; the writer always emits their current
/// placeholder payload.
#[derive(Clone, PartialEq, Debug)]
pub struct Module {
    pub(crate) target: TargetClass,
    pub(crate) flags: Flags,
    pub(crate) name: String,
    pub(crate) deps: Vec<Dep>,
    pub(crate) source_hash: Hash128,
    pub(crate) collector: CollectorHooks,
    /// Committed BL-7 field, value pending: 0 until M3 defines the
    /// computation. Non-zero values read from a file are round-tripped,
    /// not validated.
    pub(crate) max_call_depth: u32,
    /// Committed BL-7 field (IR-version checksum), value pending: 0 until
    /// M3 defines the computation. Non-zero values read from a file are
    /// round-tripped, not validated.
    pub(crate) ir_checksum: u32,
    pub(crate) constants: Vec<Constant>,
    pub(crate) functions: Vec<Function>,
    /// `None` means the module carries no debug-spans section.
    pub(crate) debug_spans: Option<Vec<DebugSpan>>,
}

impl Module {
    /// Creates a module with default flags, the dormant collector, pending
    /// BL-7 fields (zero), no dependencies, no constants, no functions,
    /// and no debug spans.
    #[must_use]
    pub fn new(name: impl Into<String>, target: TargetClass, source_hash: Hash128) -> Self {
        Self {
            target,
            flags: Flags::default(),
            name: name.into(),
            deps: Vec::new(),
            source_hash,
            collector: CollectorHooks::dormant(),
            max_call_depth: 0,
            ir_checksum: 0,
            constants: Vec::new(),
            functions: Vec::new(),
            debug_spans: None,
        }
    }

    /// The module name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The compilation target.
    #[must_use]
    pub fn target(&self) -> &TargetClass {
        &self.target
    }

    /// The header flags.
    #[must_use]
    pub fn flags(&self) -> Flags {
        self.flags
    }

    /// Replaces the header flags.
    #[must_use]
    pub fn with_flags(mut self, flags: Flags) -> Self {
        self.flags = flags;
        self
    }

    /// Marks the module stripped: sets [`Flags::STRIP`] and drops the
    /// debug spans so the writer emits no debug-spans section.
    #[must_use]
    pub fn stripped(mut self) -> Self {
        self.flags = self.flags.with_strip(true);
        self.debug_spans = None;
        self
    }

    /// The dependency list, in stored order.
    #[must_use]
    pub fn deps(&self) -> &[Dep] {
        &self.deps
    }

    /// Appends a dependency entry.
    #[must_use]
    pub fn with_dep(mut self, name: impl Into<String>, content_hash: Hash128) -> Self {
        self.deps.push(Dep {
            name: name.into(),
            content_hash,
        });
        self
    }

    /// The checked blake3-128 source hash (G3.5).
    #[must_use]
    pub fn source_hash(&self) -> Hash128 {
        self.source_hash
    }

    /// The reserved two-hook collector interface state (C4, H4).
    #[must_use]
    pub fn collector(&self) -> CollectorHooks {
        self.collector
    }

    /// The committed BL-7 max-call-depth field; 0 means "value pending"
    /// until M3 defines the computation.
    #[must_use]
    pub const fn max_call_depth(&self) -> u32 {
        self.max_call_depth
    }

    /// The committed BL-7 IR-version checksum field; 0 means "value
    /// pending" until M3 defines the computation.
    #[must_use]
    pub const fn ir_checksum(&self) -> u32 {
        self.ir_checksum
    }

    /// The constant pool, in stored order.
    #[must_use]
    pub fn constants(&self) -> &[Constant] {
        &self.constants
    }

    /// Appends a constant.
    #[must_use]
    pub fn with_constant(mut self, constant: Constant) -> Self {
        self.constants.push(constant);
        self
    }

    /// The function records of the blocks payload, in stored order.
    #[must_use]
    pub fn functions(&self) -> &[Function] {
        &self.functions
    }

    /// The function records of the blocks payload, consumed: the owning
    /// install path takes every body over without copying it.
    #[must_use]
    pub fn into_functions(self) -> Vec<Function> {
        self.functions
    }

    /// Appends a validated function record to the blocks payload.
    #[must_use]
    pub fn with_function(mut self, function: Function) -> Self {
        self.functions.push(function);
        self
    }

    /// The debug spans, or `None` when the section is absent (stripped or
    /// never recorded).
    #[must_use]
    pub fn debug_spans(&self) -> Option<&[DebugSpan]> {
        self.debug_spans.as_deref()
    }

    /// Appends a debug span covering `[start, end)`.
    #[must_use]
    pub fn with_debug_span(mut self, start: u32, end: u32) -> Self {
        self.debug_spans
            .get_or_insert_with(Vec::new)
            .push(DebugSpan::new(start, end));
        self
    }

    /// Whether the checked source hash matches `source` (G3.5); identity is
    /// hash-based, never mtime.
    #[must_use]
    pub fn matches_source(&self, source: &[u8]) -> bool {
        self.source_hash == Hash128::compute(source)
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashMap,
        hash::{DefaultHasher, Hash, Hasher},
    };

    use super::Constant;

    fn hash_of(constant: &Constant) -> u64 {
        let mut hasher = DefaultHasher::new();
        constant.hash(&mut hasher);
        hasher.finish()
    }

    #[test]
    fn constant_hash_keys_the_value_and_variant() {
        assert_eq!(hash_of(&Constant::I64(-42)), hash_of(&Constant::I64(-42)));
        assert_ne!(hash_of(&Constant::I64(1)), hash_of(&Constant::Bool(true)));
        assert_ne!(hash_of(&Constant::I64(1)), hash_of(&Constant::I64(0)));
        assert_eq!(
            hash_of(&Constant::Str("pool".to_owned())),
            hash_of(&Constant::Str("pool".to_owned()))
        );
        assert_eq!(
            hash_of(&Constant::FuncRef {
                module: "m".to_owned(),
                func: 3
            }),
            hash_of(&Constant::FuncRef {
                module: "m".to_owned(),
                func: 3
            })
        );
    }

    #[test]
    fn constant_hash_keys_the_f64_bit_pattern() {
        assert_eq!(hash_of(&Constant::F64(3.25)), hash_of(&Constant::F64(3.25)));
        // distinct bit patterns stay distinct keys, so no dedupe merges a
        // signed zero or a NaN payload onto another entry
        assert_ne!(hash_of(&Constant::F64(0.0)), hash_of(&Constant::F64(-0.0)));
        assert_ne!(
            hash_of(&Constant::F64(f64::from_bits(0x7ff8_0000_dead_beef))),
            hash_of(&Constant::F64(f64::from_bits(0x7ff8_0000_beef_dead)))
        );
    }

    #[test]
    fn constant_is_a_pool_index_key() {
        let mut index = HashMap::new();
        index.insert(Constant::Str("pool".to_owned()), 0u16);
        index.insert(Constant::I64(7), 1u16);
        assert_eq!(index.get(&Constant::Str("pool".to_owned())), Some(&0));
        assert_eq!(index.get(&Constant::I64(7)), Some(&1));
        assert_eq!(index.get(&Constant::I64(8)), None);
    }
}
