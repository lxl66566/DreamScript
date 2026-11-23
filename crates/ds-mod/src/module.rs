//! The in-memory module value: header data, functions, constants, and
//! debug spans.

use ds_abi::{BoundRet, EncodeError, FrameLayout, Inst, SlotType, Uop};

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
    /// A callee reference for CALL/CALLT pool operands: the module key
    /// selecting the callee's module, plus the callee's function index
    /// within that module's blocks payload. The loader resolves the key
    /// at install time — a dependency position to the linked module, a
    /// native name to its own table.
    FuncRef {
        /// The callee's module key.
        key: FuncRefKey,
        /// The callee's function index within the addressed module's
        /// blocks payload.
        func: u32,
    },
    /// A bound extern's C-ABI callee (H5): the `[artifacts]` name whose
    /// installed library owns the symbol, the symbol's position in that
    /// library's export enumeration, and the return's marshaling word —
    /// a scalar word or the absent word a unit return rides. The
    /// parameters marshal per the staged words' own kinds (the lowering
    /// proved the written signature inside the marshaling slice ahead
    /// of the run), so only the return needs a payload of its own.
    Bound {
        /// The artifact's manifest name: the bound module the run
        /// installs the library's symbol table under.
        module: String,
        /// The symbol's index in the artifact's export enumeration.
        index: u32,
        /// The return's marshaling word: the absent word (`unit`), or
        /// the scalar `int`/`bool`/`float` word.
        ret: BoundRet,
    },
}

/// A FuncRef's module key (the format-v4 selector-led spelling): a uop
/// dependency by position, or a native surface by its reserved name.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum FuncRefKey {
    /// A uop dependency's position in the header's dependency list:
    /// position 0 is the enclosing module itself, position `n` the
    /// `n-1`-th dependency entry.
    Dep(u32),
    /// A native surface's name: the `builtin` core, the platform the
    /// source's declaration names, or the runtime-evidence dispatch
    /// sentinel — modules the loader resolves through their own tables.
    Native(String),
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
            Self::FuncRef { key, func } => {
                4u8.hash(state);
                key.hash(state);
                func.hash(state);
            },
            Self::Bound { module, index, ret } => {
                5u8.hash(state);
                module.hash(state);
                index.hash(state);
                ret.hash(state);
            },
        }
    }
}

/// One evidence-chain object of the runtime-evidence seam `[H3]`: the
/// serving body a staged evidence word names, plus one tail chain index
/// per that body's own evidence demands (in the body's pair order). The
/// staged words carry indices into the module's chain table; a nested
/// runtime dispatch reads its frame's slot word, resolves the chain, and
/// stages the tail objects' indices as the serving body's trailing
/// evidence parameters — a static table read per hop, so depth is
/// table-bounded, never frame-bounded.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct EvidenceChain {
    /// The serving body's function-table index within this module.
    pub func: u32,
    /// One chain-table index per the serving body's evidence pairs.
    pub next: Vec<u32>,
}

/// One row of the module's tag table: a declared union member atom's
/// name and its module-wide tag id. The code words carry the ids (tag
/// tests and staged member words); this table is the name side, so a
/// host can build `Value::Tag` arguments without the front end.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct MemberTag {
    /// The member atom's declared name.
    pub name: String,
    /// The module-wide tag id.
    pub id: u8,
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
            if let Inst::Uop(uop) = inst
                && let Some(target) = jump_target_of(*uop)
                && target >= len
            {
                return Err(FunctionError::JumpEscapesBody { pc, target, len });
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

/// One bound extern's return word as a compact byte (H5): `Unit` rides
/// the tag no [`SlotType`] spells (`slot_tag`'s I32 byte, a kind the
/// bound return never legally held), the scalar words their own
/// [`slot_tag`] bytes — so an artifact the scalar round wrote decodes
/// unchanged over the richer vocabulary.
pub(crate) const fn bound_ret_tag(ret: BoundRet) -> u8 {
    match ret {
        BoundRet::Unit => 0,
        BoundRet::Int => slot_tag(SlotType::I64),
        BoundRet::Bool => slot_tag(SlotType::Bool),
        BoundRet::Float => slot_tag(SlotType::F64),
    }
}

/// Parses a bound return's compact byte; `None` for unknown tags.
pub(crate) const fn bound_ret_from_tag(tag: u8) -> Option<BoundRet> {
    match tag {
        0 => Some(BoundRet::Unit),
        1 => Some(BoundRet::Int),
        2 => Some(BoundRet::Float),
        3 => Some(BoundRet::Bool),
        _ => None,
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
    /// The evidence-chain table `[H3]`: the staged evidence words index it,
    /// the nested runtime dispatches walk it one hop per call.
    pub(crate) chains: Vec<EvidenceChain>,
    /// The tag table (member atom name to tag id), in first-use order.
    pub(crate) tags: Vec<MemberTag>,
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
            chains: Vec::new(),
            tags: Vec::new(),
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

    /// Replaces the record named by `function.name()` in place, keeping
    /// the table's order and every other module section (the park-splice
    /// seam: the REPL rewrites the entry function's body between the
    /// lowerer and the install). Returns false when no record carries the
    /// name.
    pub fn replace_function(&mut self, function: Function) -> bool {
        match self
            .functions
            .iter()
            .position(|f| f.name() == function.name())
        {
            Some(at) => {
                self.functions[at] = function;
                true
            },
            None => false,
        }
    }

    /// The evidence-chain table, in stored order `[H3]`.
    #[must_use]
    pub fn chains(&self) -> &[EvidenceChain] {
        &self.chains
    }

    /// Appends one evidence-chain object; the caller validates the
    /// function and tail indexes against the finished tables (the writer's
    /// producer is the lowerer, the reader re-validates at parse).
    #[must_use]
    pub fn with_chain(mut self, chain: EvidenceChain) -> Self {
        self.chains.push(chain);
        self
    }

    /// The tag table (member atom name to module-wide tag id), in the
    /// lowerer's first-use order.
    #[must_use]
    pub fn tags(&self) -> &[MemberTag] {
        &self.tags
    }

    /// The module-wide tag id of a declared member atom name.
    #[must_use]
    pub fn tag_id(&self, name: &str) -> Option<u8> {
        self.tags
            .iter()
            .find(|tag| tag.name == name)
            .map(|tag| tag.id)
    }

    /// Appends one tag-table row; the caller keeps ids and names unique
    /// (the lowerer's first-use assignment does by construction).
    #[must_use]
    pub fn with_tag(mut self, name: impl Into<String>, id: u8) -> Self {
        self.tags.push(MemberTag {
            name: name.into(),
            id,
        });
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

    use super::{Constant, FuncRefKey};

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
                key: FuncRefKey::Dep(0),
                func: 3
            }),
            hash_of(&Constant::FuncRef {
                key: FuncRefKey::Dep(0),
                func: 3
            })
        );
        assert_eq!(
            hash_of(&Constant::FuncRef {
                key: FuncRefKey::Native("m".to_owned()),
                func: 3
            }),
            hash_of(&Constant::FuncRef {
                key: FuncRefKey::Native("m".to_owned()),
                func: 3
            })
        );
        // a dependency position and a native name never collide as keys
        assert_ne!(
            hash_of(&Constant::FuncRef {
                key: FuncRefKey::Dep(1),
                func: 0
            }),
            hash_of(&Constant::FuncRef {
                key: FuncRefKey::Native("1".to_owned()),
                func: 0
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
