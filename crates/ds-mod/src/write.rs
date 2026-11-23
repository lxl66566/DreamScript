//! Byte-stable writer: the same [`Module`] value always serializes to the
//! same bytes.

use crate::{
    FORMAT_VERSION, MAGIC,
    header::{SectionKind, TargetClass},
    module::{
        Constant, DebugSpan, EvidenceChain, FuncRefKey, Function, MemberTag, Module, bound_ret_tag,
        slot_tag,
    },
    varint::{put_uleb, uleb_len},
};

/// The blocks-section payload version this crate writes and reads.
/// Version 2 appends the module-level evidence-chain table after the
/// function records; version 3 appends the tag table; version 4 retires
/// the entry staging table (the process surface's string argv
/// superseded entry-parameter staging).
pub(crate) const BLOCKS_PAYLOAD_VERSION: u8 = 4;

/// Serializes `module` deterministically: equal modules produce equal
/// bytes, and reading the output back yields a module that serializes to
/// the identical bytes again.
#[must_use]
pub fn write(module: &Module) -> Vec<u8> {
    // Section payload lengths in canonical (directory) order. Measuring runs
    // the same encoders over a counting sink, so the output buffer is
    // allocated once at its exact length and every payload lands in it
    // directly.
    let placeholder_len = measure(placeholder_into);
    let mut entries: Vec<(SectionKind, usize)> = Vec::with_capacity(5);
    entries.push((
        SectionKind::Blocks,
        measure(|out| {
            blocks_into(out, module.functions(), module.chains(), module.tags());
        }),
    ));
    entries.push((
        SectionKind::Constants,
        measure(|out| constants_into(out, module.constants())),
    ));
    entries.push((SectionKind::TypeMetadata, placeholder_len));
    entries.push((SectionKind::EffectRows, placeholder_len));
    if let Some(spans) = module.debug_spans() {
        entries.push((
            SectionKind::DebugSpans,
            measure(|out| spans_into(out, spans)),
        ));
    }

    // The directory size depends on the offsets it assigns, which depend on
    // its own size; both grow monotonically, so iterate to the fixed point.
    let base = measure(|out| header_into(out, module));
    let count = u64::try_from(entries.len()).expect("section count fits u64");
    let mut dir_end = base + uleb_len(count) + entries.len() * 3;
    let mut offsets = Vec::with_capacity(entries.len());
    let mut stable = false;
    for _ in 0..8 {
        offsets.clear();
        let mut offset = dir_end;
        for &(_, len) in &entries {
            offsets.push(offset);
            offset += len;
        }
        let size = uleb_len(count)
            + entries
                .iter()
                .zip(&offsets)
                .map(|(&(_, len), &offset)| 1 + uleb_len(uleb64(offset)) + uleb_len(uleb64(len)))
                .sum::<usize>();
        if base + size == dir_end {
            stable = true;
            break;
        }
        dir_end = base + size;
    }
    assert!(stable, "section directory size did not stabilize");

    let mut out = Vec::with_capacity(dir_end + entries.iter().map(|&(_, len)| len).sum::<usize>());
    header_into(&mut out, module);
    // Section directory.
    out.uleb(count);
    for (&(kind, len), &offset) in entries.iter().zip(&offsets) {
        out.push(kind.tag());
        out.uleb(uleb64(offset));
        out.uleb(uleb64(len));
    }
    debug_assert_eq!(
        out.len(),
        dir_end,
        "directory must end where payloads begin"
    );
    // Payloads, in directory order.
    for (position, &(kind, len)) in entries.iter().enumerate() {
        debug_assert_eq!(
            out.len(),
            offsets[position],
            "{kind} payload must start at its directory offset"
        );
        match kind {
            SectionKind::Blocks => {
                blocks_into(&mut out, module.functions(), module.chains(), module.tags());
            },
            SectionKind::Constants => constants_into(&mut out, module.constants()),
            SectionKind::TypeMetadata | SectionKind::EffectRows => placeholder_into(&mut out),
            SectionKind::DebugSpans => {
                let spans = module
                    .debug_spans()
                    .expect("a listed debug-spans section is present");
                spans_into(&mut out, spans);
            },
        }
        debug_assert_eq!(
            out.len(),
            offsets[position] + len,
            "{kind} payload must fill its directory range"
        );
    }
    out
}

/// Counts the bytes one payload encoder produces.
fn measure(encode: impl FnOnce(&mut Sizing)) -> usize {
    let mut sizing = Sizing::default();
    encode(&mut sizing);
    sizing.0
}

/// The byte sink the header and payload encoders run over: the output
/// buffer itself, or a counter that produces no bytes.
trait Sink {
    /// Appends one byte.
    fn push(&mut self, byte: u8);

    /// Appends a byte run.
    fn put(&mut self, bytes: &[u8]);

    /// Appends the minimal ULEB128 encoding of `value`.
    fn uleb(&mut self, value: u64);
}

impl Sink for Vec<u8> {
    fn push(&mut self, byte: u8) {
        Vec::push(self, byte);
    }

    fn put(&mut self, bytes: &[u8]) {
        self.extend_from_slice(bytes);
    }

    fn uleb(&mut self, value: u64) {
        put_uleb(self, value);
    }
}

/// The length-only sink: counts what [`Vec<u8>`] would store.
#[derive(Default)]
struct Sizing(usize);

impl Sink for Sizing {
    fn push(&mut self, _byte: u8) {
        self.0 += 1;
    }

    fn put(&mut self, bytes: &[u8]) {
        self.0 += bytes.len();
    }

    fn uleb(&mut self, value: u64) {
        self.0 += uleb_len(value);
    }
}

/// Converts an in-memory length or offset to its uleb value; lengths come
/// from memory, so they always fit u64.
fn uleb64(value: usize) -> u64 {
    u64::try_from(value).expect("length or offset fits u64")
}

/// Encodes the fixed header prefix and the variable header fields
/// (target triple, name, dependency list, source hash).
fn header_into<S: Sink>(out: &mut S, module: &Module) {
    out.put(&MAGIC);
    out.push(FORMAT_VERSION);
    out.push(module.target().tag());
    out.push(module.flags().bits());
    let (slot0, slot1, collector_flags) = module.collector().raw();
    out.push(collector_flags);
    out.put(&slot0.to_le_bytes());
    out.put(&slot1.to_le_bytes());
    out.put(&module.max_call_depth().to_le_bytes());
    out.put(&module.ir_checksum().to_le_bytes());
    if let TargetClass::Triple(triple) = module.target() {
        put_str(out, triple);
    }
    put_str(out, module.name());
    out.uleb(uleb64(module.deps().len()));
    for dep in module.deps() {
        put_str(out, &dep.name);
        out.put(dep.content_hash.as_bytes());
    }
    out.put(module.source_hash().as_bytes());
}

fn put_str<S: Sink>(out: &mut S, s: &str) {
    out.uleb(uleb64(s.len()));
    out.put(s.as_bytes());
}

/// Versioned placeholder payload for the type-metadata and effect-rows
/// sections: payload version 0 plus a reserved zero uleb. Completed at
/// M2/M3 inside the locked framing.
fn placeholder_into<S: Sink>(out: &mut S) {
    out.push(0);
    out.uleb(0);
}

/// The blocks payload, version 4: per-function records over the BL-5
/// uop set and BL-6 frame layouts, then the module-level evidence-chain
/// table and the tag table. See the crate docs for the byte layout.
fn blocks_into<S: Sink>(
    out: &mut S,
    functions: &[Function],
    chains: &[EvidenceChain],
    tags: &[MemberTag],
) {
    out.push(BLOCKS_PAYLOAD_VERSION);
    out.uleb(uleb64(functions.len()));
    for function in functions {
        put_str(out, function.name());
        out.uleb(uleb64(function.layout().params_len()));
        out.uleb(uleb64(function.layout().arg_area_len()));
        let slots = function.layout().slots();
        out.uleb(uleb64(slots.len()));
        for slot in slots {
            out.push(slot_tag(*slot));
        }
        out.uleb(uleb64(function.constants().len()));
        for constant in function.constants() {
            put_constant(out, constant);
        }
        out.uleb(uleb64(function.code().len()));
        for word in function.code() {
            out.put(&word.to_le_bytes());
        }
        // Entry-descriptor placeholder word pair [H1]: the loader fills the
        // tier pointers at install time; the bytes are zero on disk.
        out.put(&[0u8; 8]);
    }
    out.uleb(uleb64(chains.len()));
    for chain in chains {
        out.uleb(u64::from(chain.func));
        out.uleb(uleb64(chain.next.len()));
        for tail in &chain.next {
            out.uleb(u64::from(*tail));
        }
    }
    out.uleb(uleb64(tags.len()));
    for tag in tags {
        put_str(out, &tag.name);
        out.uleb(u64::from(tag.id));
    }
}

fn constants_into<S: Sink>(out: &mut S, constants: &[Constant]) {
    out.uleb(uleb64(constants.len()));
    for constant in constants {
        put_constant(out, constant);
    }
}

/// One constants-pool entry in the locked encoding: a type tag byte then
/// the value.
fn put_constant<S: Sink>(out: &mut S, constant: &Constant) {
    match constant {
        Constant::I64(v) => {
            out.push(0);
            out.put(&v.to_le_bytes());
        },
        Constant::F64(v) => {
            out.push(1);
            out.put(&v.to_bits().to_le_bytes());
        },
        Constant::Bool(v) => {
            out.push(2);
            out.push(u8::from(*v));
        },
        Constant::Str(s) => {
            out.push(3);
            put_str(out, s);
        },
        Constant::FuncRef { key, func } => {
            out.push(4);
            match key {
                FuncRefKey::Dep(position) => {
                    out.push(0);
                    out.uleb(u64::from(*position));
                },
                FuncRefKey::Native(name) => {
                    out.push(1);
                    put_str(out, name);
                },
            }
            out.uleb(u64::from(*func));
        },
        Constant::Bound { module, index, ret } => {
            out.push(5);
            put_str(out, module);
            out.uleb(u64::from(*index));
            out.push(bound_ret_tag(*ret));
        },
    }
}

fn spans_into<S: Sink>(out: &mut S, spans: &[DebugSpan]) {
    out.uleb(uleb64(spans.len()));
    for span in spans {
        out.put(&span.start.to_le_bytes());
        out.put(&span.end.to_le_bytes());
    }
}
