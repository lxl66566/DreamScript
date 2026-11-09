//! Validating reader: rejects every corruption class with a typed
//! [`ReadError`] that maps to a structured ds-diag [`Diagnostic`].

use ds_abi::{FrameLayout, Uop};
use ds_diag::{Diagnostic, ErrorCode, FileId, Span};

use crate::{
    FORMAT_VERSION, MAGIC,
    hash::Hash128,
    header::{CollectorHooks, Dep, Flags, SectionKind, TargetClass},
    module::{Constant, DebugSpan, Function, Module, jump_target_of, slot_from_tag},
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
            | Self::SectionOverrun { .. } => ErrorCode::ModuleSectionMalformed,
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
            | Self::UnknownSectionKind { at, .. } => Some(*at),
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
/// section framing, and every payload. The checked source hash is verified
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
    let mut deps = Vec::new();
    for _ in 0..dep_count {
        let name = p.utf8()?.to_owned();
        let content_hash = p.hash16()?;
        deps.push(Dep { name, content_hash });
    }
    let source_hash = p.hash16()?;

    // Section directory.
    let section_count = p.count()?;
    let mut sections: [Option<(usize, usize)>; 5] = [None; 5];
    let mut entries: Vec<(SectionKind, usize, usize)> = Vec::new();
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
        entries.push((kind, offset, len));
    }
    let dir_end = p.abs();

    // Framing: every section lives after the directory and inside the file.
    for &(kind, offset, len) in &entries {
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
    // Canonical layout: payloads tile the region after the directory
    // exactly, in directory order, with no gaps, no overlaps, and no
    // trailing bytes. This is the writer's layout, so any accepted file is
    // byte-identical to its re-serialization by construction.
    let mut expected = dir_end;
    for &(_, offset, len) in &entries {
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
    let constants = parse_constants(bytes, offset, len)?;
    let (offset, len) = sections[SectionKind::Blocks.index()].expect("checked above");
    let functions = parse_blocks(bytes, offset, len)?;
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
        debug_spans,
    })
}

/// Like [`read`], then verifies the checked source hash against `source`
/// (PEP 552 style hash validation, never mtime) [G3.5].
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
fn parse_constants(bytes: &[u8], offset: usize, len: usize) -> Result<Vec<Constant>, ReadError> {
    let mut p = Cursor::section(bytes, offset, len);
    let count = p.count()?;
    let mut constants = Vec::new();
    for _ in 0..count {
        constants.push(parse_constant(&mut p)?);
    }
    p.exhausted("trailing bytes in constants section")?;
    Ok(constants)
}

/// One constants-pool entry in the locked encoding.
fn parse_constant(p: &mut Cursor<'_>) -> Result<Constant, ReadError> {
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
            let module = p.utf8()?.to_owned();
            let func = p.uleb()?;
            let func = u32::try_from(func)
                .map_err(|_| malformed(at, "FuncRef function index exceeds u32"))?;
            Ok(Constant::FuncRef { module, func })
        },
        _ => Err(malformed(at, "unknown constant tag")),
    }
}

/// Parses the blocks payload, version 1 [BL-7]: per-function records, each
/// validated through [`Function::new`] — every code word must decode to a
/// uop at its pc, and every jump must land inside the function's own body.
fn parse_blocks(bytes: &[u8], offset: usize, len: usize) -> Result<Vec<Function>, ReadError> {
    let mut p = Cursor::section(bytes, offset, len);
    let at = p.abs();
    if p.u8()? != BLOCKS_PAYLOAD_VERSION {
        return Err(malformed(at, "unsupported section payload version"));
    }
    let count = p.count()?;
    let mut functions = Vec::new();
    for _ in 0..count {
        let record_at = p.abs();
        let name = p.utf8()?.to_owned();
        let params_len = p.count()?;
        let arg_area_len = p.count()?;
        let slot_count = p.count()?;
        let mut slots = Vec::new();
        for _ in 0..slot_count {
            let at = p.abs();
            let tag = p.u8()?;
            slots.push(slot_from_tag(tag).ok_or_else(|| malformed(at, "unknown frame slot type"))?);
        }
        let layout = FrameLayout::new(slots, params_len, arg_area_len)
            .map_err(|_| malformed(record_at, "frame layout overruns its slots"))?;
        let const_count = p.count()?;
        let mut constants = Vec::new();
        for _ in 0..const_count {
            constants.push(parse_constant(&mut p)?);
        }
        let code_count = p.count()?;
        let mut code = Vec::new();
        for pc in 0..code_count {
            let at = p.abs();
            let word = p.u32le()?;
            let pc = u32::try_from(pc).expect("pc fits u32");
            let uop = Uop::decode(pc, word)
                .map_err(|_| malformed(at, "code word does not decode to a uop"))?;
            if let Some(target) = jump_target_of(uop) {
                if usize::try_from(target).unwrap_or(usize::MAX) >= code_count {
                    return Err(malformed(at, "jump target escapes the function body"));
                }
            }
            code.push(uop);
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
        let function = Function::new(name, layout, constants, code)
            .map_err(|_| malformed(record_at, "function record fails validation"))?;
        functions.push(function);
    }
    p.exhausted("trailing bytes in blocks section")?;
    Ok(functions)
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
    let mut spans = Vec::new();
    for _ in 0..count {
        let at = p.abs();
        let start = p.u32le()?;
        let end = p.u32le()?;
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

    fn take(&mut self, n: usize) -> Result<&'a [u8], ReadError> {
        let end = self
            .pos
            .checked_add(n)
            .filter(|&end| end <= self.buf.len())
            .ok_or(ReadError::UnexpectedEof { at: self.abs() })?;
        let bytes = &self.buf[self.pos..end];
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
