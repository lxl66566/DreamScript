//! Byte-stable writer: the same [`Module`] value always serializes to the
//! same bytes.

use crate::{
    FORMAT_VERSION, MAGIC,
    header::{Dep, SectionKind, TargetClass},
    module::{Constant, DebugSpan, Function, Module, slot_tag},
    varint::{put_uleb, uleb_len},
};

/// The blocks-section payload version this crate writes and reads.
pub(crate) const BLOCKS_PAYLOAD_VERSION: u8 = 1;

/// Serializes `module` deterministically: equal modules produce equal
/// bytes, and reading the output back yields a module that serializes to
/// the identical bytes again.
#[must_use]
pub fn write(module: &Module) -> Vec<u8> {
    // Section payloads in canonical (directory) order.
    let blocks = blocks_payload(module.functions());
    let constants = constants_payload(module.constants());
    let type_metadata = placeholder_payload();
    let effect_rows = placeholder_payload();
    let debug_spans = module.debug_spans().map(spans_payload);
    let mut entries: Vec<(SectionKind, Vec<u8>)> = vec![
        (SectionKind::Blocks, blocks),
        (SectionKind::Constants, constants),
        (SectionKind::TypeMetadata, type_metadata),
        (SectionKind::EffectRows, effect_rows),
    ];
    if let Some(spans) = debug_spans {
        entries.push((SectionKind::DebugSpans, spans));
    }

    // The directory size depends on the offsets it assigns, which depend on
    // its own size; both grow monotonically, so iterate to the fixed point.
    let base = header_len(module);
    let count = u64::try_from(entries.len()).expect("section count fits u64");
    let mut dir_end = base + uleb_len(count) + entries.len() * 3;
    let mut offsets = Vec::with_capacity(entries.len());
    let mut stable = false;
    for _ in 0..8 {
        offsets.clear();
        let mut offset = dir_end;
        for (_, payload) in &entries {
            offsets.push(offset);
            offset += payload.len();
        }
        let size = uleb_len(count)
            + entries
                .iter()
                .zip(&offsets)
                .map(|((_, payload), offset)| {
                    1 + uleb_len(uleb64(*offset)) + uleb_len(uleb64(payload.len()))
                })
                .sum::<usize>();
        if base + size == dir_end {
            stable = true;
            break;
        }
        dir_end = base + size;
    }
    assert!(stable, "section directory size did not stabilize");

    let mut out = Vec::with_capacity(
        offsets.last().copied().unwrap_or(dir_end) + entries.last().map_or(0, |(_, p)| p.len()),
    );
    // Fixed header prefix.
    out.extend_from_slice(&MAGIC);
    out.push(FORMAT_VERSION);
    out.push(module.target().tag());
    out.push(module.flags().bits());
    let (slot0, slot1, collector_flags) = module.collector().raw();
    out.push(collector_flags);
    out.extend_from_slice(&slot0.to_le_bytes());
    out.extend_from_slice(&slot1.to_le_bytes());
    out.extend_from_slice(&module.max_call_depth().to_le_bytes());
    out.extend_from_slice(&module.ir_checksum().to_le_bytes());
    // Variable header fields.
    if let TargetClass::Triple(triple) = module.target() {
        put_str(&mut out, triple);
    }
    put_str(&mut out, module.name());
    put_uleb(
        &mut out,
        u64::try_from(module.deps().len()).expect("dep count fits u64"),
    );
    for dep in module.deps() {
        put_str(&mut out, &dep.name);
        out.extend_from_slice(dep.content_hash.as_bytes());
    }
    out.extend_from_slice(module.source_hash().as_bytes());
    // Section directory.
    put_uleb(&mut out, count);
    for ((kind, payload), offset) in entries.iter().zip(&offsets) {
        out.push(kind.tag());
        put_uleb(&mut out, uleb64(*offset));
        put_uleb(&mut out, uleb64(payload.len()));
    }
    debug_assert_eq!(
        out.len(),
        dir_end,
        "directory must end where payloads begin"
    );
    // Payloads.
    for (kind, payload) in &entries {
        debug_assert_eq!(
            out.len(),
            offsets[kind.index()],
            "{kind} payload must start at its directory offset"
        );
        out.extend_from_slice(payload);
    }
    out
}

/// Converts an in-memory length or offset to its uleb value; lengths come
/// from memory, so they always fit u64.
fn uleb64(value: usize) -> u64 {
    u64::try_from(value).expect("length or offset fits u64")
}

/// Byte length of the header (everything before the section directory).
fn header_len(module: &Module) -> usize {
    // 8 fixed bytes (magic, version, target tag, flags, collector flags)
    // + 16 bytes of reserved hook slots + 8 bytes of committed BL-7 scalars
    // (max call depth, IR checksum) + name + deps + source hash.
    let mut len = 8 + 16 + 8 + str_len(module.name()) + 16 + deps_len(module.deps());
    if let TargetClass::Triple(triple) = module.target() {
        len += str_len(triple);
    }
    len
}

fn deps_len(deps: &[Dep]) -> usize {
    let count = u64::try_from(deps.len()).expect("dep count fits u64");
    uleb_len(count)
        + deps
            .iter()
            .map(|dep| str_len(&dep.name) + 16)
            .sum::<usize>()
}

fn str_len(s: &str) -> usize {
    let len = u64::try_from(s.len()).expect("string length fits u64");
    uleb_len(len) + s.len()
}

fn put_str(out: &mut Vec<u8>, s: &str) {
    put_uleb(out, u64::try_from(s.len()).expect("string length fits u64"));
    out.extend_from_slice(s.as_bytes());
}

/// Versioned placeholder payload for the type-metadata and effect-rows
/// sections: payload version 0 plus a reserved zero uleb. Completed at
/// M2/M3 inside the locked framing.
fn placeholder_payload() -> Vec<u8> {
    let mut out = Vec::with_capacity(2);
    out.push(0);
    put_uleb(&mut out, 0);
    out
}

/// The blocks payload, version 1 (BL-7): per-function records over the
/// BL-5 uop set and BL-6 frame layouts. See the crate docs for the byte
/// layout.
fn blocks_payload(functions: &[Function]) -> Vec<u8> {
    let mut out = Vec::new();
    out.push(BLOCKS_PAYLOAD_VERSION);
    put_uleb(
        &mut out,
        u64::try_from(functions.len()).expect("function count fits u64"),
    );
    for function in functions {
        put_str(&mut out, function.name());
        put_uleb(
            &mut out,
            u64::try_from(function.layout().params_len()).expect("params_len fits u64"),
        );
        put_uleb(
            &mut out,
            u64::try_from(function.layout().arg_area_len()).expect("arg_area_len fits u64"),
        );
        let slots = function.layout().slots();
        put_uleb(
            &mut out,
            u64::try_from(slots.len()).expect("slot count fits u64"),
        );
        for slot in slots {
            out.push(slot_tag(*slot));
        }
        put_uleb(
            &mut out,
            u64::try_from(function.constants().len()).expect("constant count fits u64"),
        );
        for constant in function.constants() {
            put_constant(&mut out, constant);
        }
        put_uleb(
            &mut out,
            u64::try_from(function.code().len()).expect("code count fits u64"),
        );
        for (pc, inst) in function.code().iter().enumerate() {
            let pc = u32::try_from(pc).expect("pc fits u32");
            out.extend_from_slice(
                &inst
                    .encode(pc)
                    .expect("Function::new validated encodability")
                    .to_le_bytes(),
            );
        }
        // Entry-descriptor placeholder word pair [H1]: the loader fills the
        // tier pointers at install time; the bytes are zero on disk.
        out.extend_from_slice(&[0u8; 8]);
    }
    out
}

fn constants_payload(constants: &[Constant]) -> Vec<u8> {
    let mut out = Vec::new();
    put_uleb(
        &mut out,
        u64::try_from(constants.len()).expect("constant count fits u64"),
    );
    for constant in constants {
        put_constant(&mut out, constant);
    }
    out
}

/// One constants-pool entry in the locked encoding: a type tag byte then
/// the value.
fn put_constant(out: &mut Vec<u8>, constant: &Constant) {
    match constant {
        Constant::I64(v) => {
            out.push(0);
            out.extend_from_slice(&v.to_le_bytes());
        },
        Constant::F64(v) => {
            out.push(1);
            out.extend_from_slice(&v.to_bits().to_le_bytes());
        },
        Constant::Bool(v) => {
            out.push(2);
            out.push(u8::from(*v));
        },
        Constant::Str(s) => {
            out.push(3);
            put_str(out, s);
        },
        Constant::FuncRef { module, func } => {
            out.push(4);
            put_str(out, module);
            put_uleb(out, u64::from(*func));
        },
    }
}

fn spans_payload(spans: &[DebugSpan]) -> Vec<u8> {
    let mut out = Vec::new();
    put_uleb(
        &mut out,
        u64::try_from(spans.len()).expect("span count fits u64"),
    );
    for span in spans {
        out.extend_from_slice(&span.start.to_le_bytes());
        out.extend_from_slice(&span.end.to_le_bytes());
    }
    out
}
