//! Validating reader: rejects every corruption class with a typed
//! [`ReadError`] that maps to a structured ds-diag [`Diagnostic`].

use ds_abi::{FrameLayout, Inst};
use ds_diag::{Diagnostic, ErrorCode, FileId, Span};

use crate::{
    FORMAT_VERSION, MAGIC,
    hash::Hash128,
    header::{CollectorHooks, Dep, Flags, SectionKind, TargetClass},
    module::{
        Constant, DebugSpan, EvidenceChain, FuncRefKey, Function, MemberTag, Module,
        bound_ret_from_tag, jump_target_of, slot_from_tag,
    },
    write::BLOCKS_PAYLOAD_VERSION,
};

/// Everything that can be wrong with a dsmod byte stream.
///
/// Each variant maps to one stable error code ([`ReadError::code`]) and one
/// structured diagnostic ([`ReadError::diagnostic`]).
#[derive(Clone, PartialEq, Debug, thiserror::Error)]
pub enum ReadError {
    /// The file does not start with the dsmod magic (DS4002).
    #[error("file does not start with the dsmod magic `\\0DSM`")]
    BadMagic,
    /// The format version byte is not the one this reader supports (DS4003).
    #[error("dsmod format version {found} is not supported (supported: {supported})", supported = FORMAT_VERSION)]
    UnsupportedVersion {
        /// The version byte found at offset 4.
        found: u8,
    },
    /// The target-class byte is not a member of the target enum (DS4007).
    #[error("target-class byte {tag} is not a member of the target enum")]
    BadTargetClass {
        /// The unknown tag found at offset 5.
        tag: u8,
    },
    /// The flags byte carries bits no format-version-1 reader knows (DS4004).
    #[error("unknown dsmod flag bits 0x{unknown:02x}")]
    UnknownFlags {
        /// The unknown bits found at offset 6.
        unknown: u8,
    },
    /// Input ended while more bytes were required (DS4004).
    #[error("input ended while more bytes were required (at byte {at})")]
    UnexpectedEof {
        /// Offset at which the stream ran out.
        at: usize,
    },
    /// A ULEB128 integer is over-long, overflowing, or non-canonical (DS4004).
    #[error("malformed or non-canonical ULEB128 integer at byte {at}")]
    MalformedUleb {
        /// Offset of the first byte of the integer.
        at: usize,
    },
    /// A string field is not valid UTF-8 (DS4004).
    #[error("string field is not valid UTF-8 (at byte {at})")]
    InvalidUtf8 {
        /// Offset of the first byte of the string.
        at: usize,
    },
    /// Structural corruption not covered by a more specific variant (DS4004):
    /// unknown constant tags, nonzero reserved fields, span starts past ends,
    /// non-contiguous section layouts, trailing bytes, and similar.
    #[error("malformed dsmod data at byte {at}: {reason}")]
    Malformed {
        /// Offset of the offending byte.
        at: usize,
        /// What is wrong there.
        reason: &'static str,
    },
    /// The stored source hash does not match the actual source (DS4001).
    #[error("stored source hash {stored} does not match the actual source hash {computed}")]
    HashMismatch {
        /// The hash stored in the header.
        stored: Hash128,
        /// The hash computed from the presented source bytes.
        computed: Hash128,
    },
    /// A directory entry names a kind no format-version-1 reader knows (DS4008).
    #[error("unknown section kind {tag} at byte {at}")]
    UnknownSectionKind {
        /// The unknown kind tag.
        tag: u8,
        /// Offset of the kind byte.
        at: usize,
    },
    /// One section kind appears more than once in the directory (DS4008).
    #[error("section {kind} appears more than once")]
    DuplicateSection {
        /// The duplicated kind.
        kind: SectionKind,
    },
    /// A section required in every dsmod file is absent (DS4008).
    #[error("required section {kind} is missing")]
    MissingSection {
        /// The absent kind.
        kind: SectionKind,
    },
    /// A section range escapes the region after the directory, overlaps the
    /// header, or extends past end of file (DS4008).
    #[error("section {kind} at offset {offset} with length {len} escapes the file bounds")]
    SectionOverrun {
        /// The offending kind.
        kind: SectionKind,
        /// Directory-declared payload offset.
        offset: usize,
        /// Directory-declared payload length.
        len: usize,
    },
    /// The directory's kinds are not in the writer's canonical order
    /// (DS4008). Transposed rows with equal-length payloads still tile, but
    /// the writer re-emits canonical order, so accepting them would break
    /// byte stability.
    #[error("section {kind} at byte {at} is out of canonical order (expected {expected})")]
    NonCanonicalSectionOrder {
        /// The kind found at the off-position.
        kind: SectionKind,
        /// The kind the canonical order places at that position.
        expected: SectionKind,
        /// Offset of the offending kind byte.
        at: usize,
    },
}

impl ReadError {
    /// The stable registry code for this error class.
    #[must_use]
    pub const fn code(&self) -> ErrorCode {
        match self {
            Self::BadMagic => ErrorCode::ModuleBadMagic,
            Self::UnsupportedVersion { .. } => ErrorCode::ModuleVersionUnsupported,
            Self::BadTargetClass { .. } => ErrorCode::ModuleBadTargetClass,
            Self::HashMismatch { .. } => ErrorCode::ModuleHashMismatch,
            Self::UnknownSectionKind { .. }
            | Self::DuplicateSection { .. }
            | Self::MissingSection { .. }
            | Self::SectionOverrun { .. }
            | Self::NonCanonicalSectionOrder { .. } => ErrorCode::ModuleSectionMalformed,
            Self::UnknownFlags { .. }
            | Self::UnexpectedEof { .. }
            | Self::MalformedUleb { .. }
            | Self::InvalidUtf8 { .. }
            | Self::Malformed { .. } => ErrorCode::BytecodeMalformed,
        }
    }

    /// The byte offset the error points at, when the reader knows it.
    #[must_use]
    pub const fn offset(&self) -> Option<usize> {
        match self {
            Self::BadMagic => Some(0),
            Self::UnsupportedVersion { .. } => Some(4),
            Self::BadTargetClass { .. } => Some(5),
            Self::UnknownFlags { .. } => Some(6),
            Self::UnexpectedEof { at }
            | Self::MalformedUleb { at }
            | Self::InvalidUtf8 { at }
            | Self::Malformed { at, .. }
            | Self::UnknownSectionKind { at, .. }
            | Self::NonCanonicalSectionOrder { at, .. } => Some(*at),
            Self::HashMismatch { .. }
            | Self::DuplicateSection { .. }
            | Self::MissingSection { .. } => None,
            Self::SectionOverrun { offset, .. } => Some(*offset),
        }
    }

    /// Builds the structured diagnostic for this error, with spans over the
    /// dsmod file registered under `file`.
    #[must_use]
    pub fn diagnostic(&self, file: FileId) -> Diagnostic {
        let mut diagnostic = Diagnostic::error(self.code(), self.to_string());
        if let Some(at) = self.offset() {
            let at = u32::try_from(at).unwrap_or(u32::MAX);
            diagnostic = diagnostic.with_primary(Span::new(file, at, at), "invalid dsmod data");
        }
        diagnostic
    }
}

/// Parses and validates a dsmod module from `bytes`.
///
/// Validates the magic, format version, target class, flags, ULEB bounds,
/// section framing (canonical kind order, bounds, tiling), and every
/// payload. The checked source hash is verified
/// separately by [`read_checked`], because the source bytes are not part of
/// the module file.
///
/// # Errors
///
/// Returns the first [`ReadError`] found; the reader never panics on any
/// input.
pub fn read(bytes: &[u8]) -> Result<Module, ReadError> {
    if bytes.first_chunk::<4>() != Some(&MAGIC) {
        return Err(ReadError::BadMagic);
    }
    let mut p = Cursor::new(bytes);
    p.pos = 4;
    let version = p.u8()?;
    if version != FORMAT_VERSION {
        return Err(ReadError::UnsupportedVersion { found: version });
    }
    let tag = p.u8()?;
    let flags = Flags::from_bits(p.u8()?).map_err(|unknown| ReadError::UnknownFlags { unknown })?;
    let collector_flags = p.u8()?;
    let slot0 = p.u64le()?;
    let slot1 = p.u64le()?;
    let collector = CollectorHooks::from_raw(slot0, slot1, collector_flags);
    let max_call_depth = p.u32le()?;
    let ir_checksum = p.u32le()?;
    // The variable header fields follow the fixed prefix; the triple string
    // comes first among them, before the module name.
    let target = match tag {
        0 => TargetClass::Bytecode,
        1 => TargetClass::Wasm,
        2 => TargetClass::Triple(p.utf8()?.to_owned()),
        _ => return Err(ReadError::BadTargetClass { tag }),
    };
    let name = p.utf8()?.to_owned();
    let dep_count = p.count()?;
    let mut deps = Vec::with_capacity(p.capacity_for(dep_count, 17));
    for _ in 0..dep_count {
        let name = p.utf8()?.to_owned();
        let content_hash = p.hash16()?;
        deps.push(Dep { name, content_hash });
    }
    let source_hash = p.hash16()?;

    // Section directory: (kind byte offset, kind, payload offset, length).
    let section_count = p.count()?;
    let mut sections: [Option<(usize, usize)>; 5] = [None; 5];
    let mut entries: Vec<(usize, SectionKind, usize, usize)> =
        Vec::with_capacity(p.capacity_for(section_count, 3));
    for _ in 0..section_count {
        let at = p.abs();
        let tag = p.u8()?;
        let Some(kind) = SectionKind::from_tag(tag) else {
            return Err(ReadError::UnknownSectionKind { tag, at });
        };
        let offset = p.count()?;
        let len = p.count()?;
        if sections[kind.index()].is_some() {
            return Err(ReadError::DuplicateSection { kind });
        }
        sections[kind.index()] = Some((offset, len));
        entries.push((at, kind, offset, len));
    }
    let dir_end = p.abs();

    // Framing: every section lives after the directory and inside the file.
    for &(_, kind, offset, len) in &entries {
        let end = offset.checked_add(len);
        if offset < dir_end || end.is_none_or(|end| end > bytes.len()) {
            return Err(ReadError::SectionOverrun { kind, offset, len });
        }
    }
    for kind in [
        SectionKind::Blocks,
        SectionKind::Constants,
        SectionKind::TypeMetadata,
        SectionKind::EffectRows,
    ] {
        if sections[kind.index()].is_none() {
            return Err(ReadError::MissingSection { kind });
        }
    }
    // Canonical kind order: the writer emits kinds in `SectionKind::ALL`
    // order (DebugSpans last and optional), and a directory holds at most
    // five distinct kinds, so zipping against `ALL` walks every entry.
    // Transposed rows with equal-length payloads still tile below, but the
    // writer re-emits canonical order, so they must not be accepted.
    for (&(at, kind, ..), &expected) in entries.iter().zip(SectionKind::ALL.iter()) {
        if kind != expected {
            return Err(ReadError::NonCanonicalSectionOrder { kind, expected, at });
        }
    }
    // Canonical layout: payloads tile the region after the directory
    // exactly, in directory order, with no gaps, no overlaps, and no
    // trailing bytes. This is the writer's layout, so any accepted file is
    // byte-identical to its re-serialization by construction.
    let mut expected = dir_end;
    for &(_, _, offset, len) in &entries {
        if offset != expected {
            return Err(ReadError::Malformed {
                at: offset,
                reason: "non-contiguous section layout",
            });
        }
        expected += len;
    }
    if expected != bytes.len() {
        return Err(ReadError::Malformed {
            at: expected,
            reason: "trailing bytes after the last section",
        });
    }

    // Payloads.
    let (offset, len) = sections[SectionKind::Constants.index()].expect("checked above");
    let constants = parse_constants(bytes, offset, len, dep_count)?;
    let (offset, len) = sections[SectionKind::Blocks.index()].expect("checked above");
    let (functions, chains, tags) = parse_blocks(bytes, offset, len, dep_count)?;
    for kind in [SectionKind::TypeMetadata, SectionKind::EffectRows] {
        let (offset, len) = sections[kind.index()].expect("checked above");
        parse_placeholder(bytes, offset, len)?;
    }
    let debug_spans = sections[SectionKind::DebugSpans.index()]
        .map(|(offset, len)| parse_debug_spans(bytes, offset, len))
        .transpose()?;

    Ok(Module {
        target,
        flags,
        name,
        deps,
        source_hash,
        collector,
        max_call_depth,
        ir_checksum,
        constants,
        functions,
        chains,
        tags,
        debug_spans,
    })
}

/// Like [`read`], then verifies the checked source hash against `source`
/// (PEP 552 style hash validation, never mtime) (G3.5).
///
/// # Errors
///
/// Returns [`ReadError::HashMismatch`] when the stored hash does not match
/// the source, plus every structural error [`read`] can return.
pub fn read_checked(bytes: &[u8], source: &[u8]) -> Result<Module, ReadError> {
    let module = read(bytes)?;
    let computed = Hash128::compute(source);
    if module.source_hash() != computed {
        return Err(ReadError::HashMismatch {
            stored: module.source_hash(),
            computed,
        });
    }
    Ok(module)
}

/// Parses the locked constants payload: typed entries (i64/f64/bool/string).
fn parse_constants(
    bytes: &[u8],
    offset: usize,
    len: usize,
    dep_count: usize,
) -> Result<Vec<Constant>, ReadError> {
    let mut p = Cursor::section(bytes, offset, len);
    let count = p.count()?;
    let mut constants = Vec::with_capacity(p.capacity_for(count, 2));
    for _ in 0..count {
        constants.push(parse_constant(&mut p, dep_count)?);
    }
    p.exhausted("trailing bytes in constants section")?;
    Ok(constants)
}

/// One constants-pool entry in the locked encoding.
fn parse_constant(p: &mut Cursor<'_>, dep_count: usize) -> Result<Constant, ReadError> {
    let at = p.abs();
    let tag = p.u8()?;
    match tag {
        0 => Ok(Constant::I64(i64::from_le_bytes(p.array::<8>()?))),
        1 => Ok(Constant::F64(f64::from_bits(u64::from_le_bytes(
            p.array::<8>()?,
        )))),
        2 => match p.u8()? {
            0 => Ok(Constant::Bool(false)),
            1 => Ok(Constant::Bool(true)),
            _ => Err(malformed(at, "invalid bool constant byte")),
        },
        3 => Ok(Constant::Str(p.utf8()?.to_owned())),
        4 => {
            let selector = p.u8()?;
            let key = match selector {
                0 => {
                    let position_at = p.abs();
                    let position = p.uleb()?;
                    let position = u32::try_from(position)
                        .map_err(|_| malformed(at, "FuncRef dep position exceeds u32"))?;
                    if !usize::try_from(position).is_ok_and(|position| position <= dep_count) {
                        return Err(malformed(
                            position_at,
                            "FuncRef dep position past the dependency list",
                        ));
                    }
                    FuncRefKey::Dep(position)
                },
                1 => FuncRefKey::Native(p.utf8()?.to_owned()),
                _ => return Err(malformed(at, "unknown FuncRef key selector")),
            };
            let func = p.uleb()?;
            let func = u32::try_from(func)
                .map_err(|_| malformed(at, "FuncRef function index exceeds u32"))?;
            Ok(Constant::FuncRef { key, func })
        },
        5 => {
            let module = p.utf8()?.to_owned();
            let index = p.uleb()?;
            let index = u32::try_from(index)
                .map_err(|_| malformed(at, "bound symbol index exceeds u32"))?;
            let ret_at = p.abs();
            let ret = bound_ret_from_tag(p.u8()?)
                .ok_or_else(|| malformed(ret_at, "unknown bound return marshaling word"))?;
            Ok(Constant::Bound { module, index, ret })
        },
        _ => Err(malformed(at, "unknown constant tag")),
    }
}

/// The parsed blocks payload's tables: the function records, the
/// evidence chains, and the tag table.
type BlocksTables = (Vec<Function>, Vec<EvidenceChain>, Vec<MemberTag>);

/// Parses the blocks payload, version 4: per-function records, then the
/// module-level evidence-chain table and the tag table. Each body word
/// is decoded once — every word must decode canonically at its pc, and
/// every jump must land inside the function's own body — and the stored
/// words are what the loader takes. Every chain's function index must
/// address the function list and every tail index the chain table
/// itself; the tag table must spell unique names and unique tag ids.
fn parse_blocks(
    bytes: &[u8],
    offset: usize,
    len: usize,
    dep_count: usize,
) -> Result<BlocksTables, ReadError> {
    let mut p = Cursor::section(bytes, offset, len);
    let at = p.abs();
    if p.u8()? != BLOCKS_PAYLOAD_VERSION {
        return Err(malformed(at, "unsupported section payload version"));
    }
    let count = p.count()?;
    let mut functions = Vec::with_capacity(p.capacity_for(count, 14));
    for _ in 0..count {
        let record_at = p.abs();
        let name = p.utf8()?.to_owned();
        let params_len = p.count()?;
        let arg_area_len = p.count()?;
        let slot_count = p.count()?;
        let slots_at = p.abs();
        let slot_bytes = p.run(slot_count, 1)?;
        let mut slots = Vec::with_capacity(slot_count);
        for (index, &tag) in slot_bytes.iter().enumerate() {
            let at = slots_at + index;
            slots.push(slot_from_tag(tag).ok_or_else(|| malformed(at, "unknown frame slot type"))?);
        }
        let layout = FrameLayout::new(slots, params_len, arg_area_len)
            .map_err(|_| malformed(record_at, "frame layout overruns its slots"))?;
        let const_count = p.count()?;
        let mut constants = Vec::with_capacity(p.capacity_for(const_count, 2));
        for _ in 0..const_count {
            constants.push(parse_constant(&mut p, dep_count)?);
        }
        let count_at = p.abs();
        let code_count = p.count()?;
        // pcs are u32; a larger declared count is malformed data, not a
        // mid-loop panic (the reader never panics on any input)
        let code_count = u32::try_from(code_count)
            .map_err(|_| malformed(count_at, "code word count exceeds the u32 pc range"))?;
        let code_len = usize::try_from(code_count)
            .map_err(|_| malformed(count_at, "code word count exceeds usize"))?;
        let code_at = p.abs();
        let code_bytes = p.run(code_len, 4)?;
        let mut code = Vec::with_capacity(code_len);
        let mut at = code_at;
        let (chunks, _) = code_bytes.as_chunks::<4>();
        for (pc, chunk) in (0..code_count).zip(chunks) {
            let word = u32::from_le_bytes(*chunk);
            let inst = Inst::decode(pc, word)
                .map_err(|_| malformed(at, "code word does not decode to an instruction"))?;
            if let Inst::Uop(uop) = inst
                && let Some(target) = jump_target_of(uop)
                && target >= code_count
            {
                return Err(malformed(at, "jump target escapes the function body"));
            }
            code.push(word);
            at += 4;
        }
        // Entry-descriptor placeholder word pair: zero on disk; the loader
        // fills the tier pointers at install time.
        let entry_at = p.abs();
        if p.u32le()? != 0 || p.u32le()? != 0 {
            return Err(malformed(
                entry_at,
                "entry descriptor words must be zero on disk",
            ));
        }
        functions.push(Function::from_words(name, layout, constants, code));
    }
    let chain_count = p.count()?;
    let mut chains = Vec::with_capacity(p.capacity_for(chain_count, 2));
    for _ in 0..chain_count {
        let chain_at = p.abs();
        let func = p.uleb()?;
        let func = u32::try_from(func)
            .map_err(|_| malformed(chain_at, "chain function index exceeds u32"))?;
        if !usize::try_from(func).is_ok_and(|func| func < functions.len()) {
            return Err(malformed(
                chain_at,
                "chain function index escapes the function table",
            ));
        }
        let tail_count = p.count()?;
        let mut next = Vec::with_capacity(tail_count.min(p.remaining()));
        for _ in 0..tail_count {
            let tail_at = p.abs();
            let tail = p.uleb()?;
            let tail = u32::try_from(tail)
                .map_err(|_| malformed(tail_at, "chain tail index exceeds u32"))?;
            if !usize::try_from(tail).is_ok_and(|tail| tail < chain_count) {
                return Err(malformed(
                    tail_at,
                    "chain tail index escapes the chain table",
                ));
            }
            next.push(tail);
        }
        chains.push(EvidenceChain { func, next });
    }
    let tag_count = p.count()?;
    let mut tags = Vec::with_capacity(p.capacity_for(tag_count, 2));
    let mut names = std::collections::HashSet::new();
    let mut ids = std::collections::HashSet::new();
    for _ in 0..tag_count {
        let name_at = p.abs();
        let name = p.utf8()?.to_owned();
        if !names.insert(name.clone()) {
            return Err(malformed(name_at, "duplicate tag-table name"));
        }
        let id_at = p.abs();
        let id = p.uleb()?;
        let id = u8::try_from(id).map_err(|_| malformed(id_at, "tag id exceeds u8"))?;
        if !ids.insert(id) {
            return Err(malformed(id_at, "duplicate tag-table id"));
        }
        tags.push(MemberTag { name, id });
    }
    p.exhausted("trailing bytes in blocks section")?;
    Ok((functions, chains, tags))
}

/// Parses a versioned placeholder payload (type metadata, effect rows):
/// payload version 0 plus a reserved zero uleb. Completed at M2/M3 inside
/// the locked framing.
fn parse_placeholder(bytes: &[u8], offset: usize, len: usize) -> Result<(), ReadError> {
    let mut p = Cursor::section(bytes, offset, len);
    let at = p.abs();
    if p.u8()? != 0 {
        return Err(malformed(at, "unsupported section payload version"));
    }
    if p.uleb()? != 0 {
        return Err(malformed(at, "nonzero reserved field"));
    }
    p.exhausted("trailing bytes in placeholder section")
}

/// Parses the strippable debug-spans payload: byte-offset spans.
fn parse_debug_spans(bytes: &[u8], offset: usize, len: usize) -> Result<Vec<DebugSpan>, ReadError> {
    let mut p = Cursor::section(bytes, offset, len);
    let count = p.count()?;
    let spans_at = p.abs();
    let span_bytes = p.run(count, 8)?;
    let mut spans = Vec::with_capacity(count);
    for (index, span) in span_bytes.as_chunks::<8>().0.iter().enumerate() {
        let at = spans_at + index * 8;
        let start = u32::from_le_bytes(span[..4].try_into().expect("a half of the span pair"));
        let end = u32::from_le_bytes(span[4..].try_into().expect("a half of the span pair"));
        if start > end {
            return Err(malformed(at, "span start exceeds end"));
        }
        spans.push(DebugSpan { start, end });
    }
    p.exhausted("trailing bytes in debug-spans section")?;
    Ok(spans)
}

fn malformed(at: usize, reason: &'static str) -> ReadError {
    ReadError::Malformed { at, reason }
}

/// Bounds-checked forward cursor over the whole file or one section.
///
/// `pos` is relative to `buf`; `abs()` converts to a whole-file offset so
/// diagnostics point at real bytes.
struct Cursor<'a> {
    buf: &'a [u8],
    pos: usize,
    origin: usize,
}

impl<'a> Cursor<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self {
            buf,
            pos: 0,
            origin: 0,
        }
    }

    fn section(buf: &'a [u8], offset: usize, len: usize) -> Self {
        Self {
            buf: &buf[offset..offset + len],
            pos: 0,
            origin: offset,
        }
    }

    fn abs(&self) -> usize {
        self.origin + self.pos
    }

    /// Bytes left in this cursor's buffer.
    fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }

    /// A presized capacity for a declared count, clamped to what the
    /// remaining bytes can hold at `min_elem` bytes per element: a corrupt
    /// count cannot allocate ahead of the loop that rejects it.
    fn capacity_for(&self, count: usize, min_elem: usize) -> usize {
        count.min(self.remaining() / min_elem)
    }

    /// The next `n` bytes, or `UnexpectedEof` when the run escapes the
    /// buffer.
    fn take(&mut self, n: usize) -> Result<&'a [u8], ReadError> {
        let at = self.abs();
        // A `wrapping_add` lands below `pos` only on overflow, and `get`
        // rejects that reversed range like any other over-long one.
        let end = self.pos.wrapping_add(n);
        let bytes = self
            .buf
            .get(self.pos..end)
            .ok_or(ReadError::UnexpectedEof { at })?;
        self.pos = end;
        Ok(bytes)
    }

    fn u8(&mut self) -> Result<u8, ReadError> {
        Ok(self.take(1)?[0])
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], ReadError> {
        self.take(N).map(|bytes| {
            let mut out = [0; N];
            out.copy_from_slice(bytes);
            out
        })
    }

    /// A run of `count` fixed-size elements of `elem` bytes each, taken in
    /// one bounds check and walked from the returned slice.
    fn run(&mut self, count: usize, elem: usize) -> Result<&'a [u8], ReadError> {
        let at = self.abs();
        let len = count
            .checked_mul(elem)
            .ok_or(ReadError::UnexpectedEof { at })?;
        self.take(len)
    }

    fn u32le(&mut self) -> Result<u32, ReadError> {
        Ok(u32::from_le_bytes(self.array::<4>()?))
    }

    fn u64le(&mut self) -> Result<u64, ReadError> {
        Ok(u64::from_le_bytes(self.array::<8>()?))
    }

    fn hash16(&mut self) -> Result<Hash128, ReadError> {
        self.array::<16>().map(Hash128::new)
    }

    /// Decodes a canonical ULEB128 integer: at most ten bytes, no u64
    /// overflow, no redundant trailing zero byte.
    fn uleb(&mut self) -> Result<u64, ReadError> {
        let at = self.abs();
        let mut value = 0u64;
        for i in 0..10 {
            let byte = self.u8()?;
            if i == 9 {
                // Tenth byte: a single value bit remains.
                if byte & 0x7f > 1 || byte & 0x80 != 0 || byte == 0 {
                    return Err(ReadError::MalformedUleb { at });
                }
                return Ok(value | (u64::from(byte & 0x7f) << 63));
            }
            value |= u64::from(byte & 0x7f) << (i * 7);
            if byte & 0x80 == 0 {
                if i > 0 && byte == 0 {
                    return Err(ReadError::MalformedUleb { at });
                }
                return Ok(value);
            }
        }
        unreachable!("loop returns within ten bytes")
    }

    /// A count or offset field: a uleb small enough to address memory.
    fn count(&mut self) -> Result<usize, ReadError> {
        usize::try_from(self.uleb()?).map_err(|_| ReadError::MalformedUleb { at: self.abs() - 1 })
    }

    fn utf8(&mut self) -> Result<&'a str, ReadError> {
        let len = self.count()?;
        let at = self.abs();
        let bytes = self.take(len)?;
        core::str::from_utf8(bytes).map_err(|_| ReadError::InvalidUtf8 { at })
    }

    fn exhausted(&self, reason: &'static str) -> Result<(), ReadError> {
        if self.pos == self.buf.len() { Ok(()) } else { Err(malformed(self.abs(), reason)) }
    }
}
