//! dsmod, DreamScript's binary module format: writer, validating reader,
//! mmap loader, and cache path layout.
//!
//! The byte layout implements the LOCKED contract of design decisions G3.5
//! and BL-7 (`docs/en/design-decisions.md`). Any change to the header, the
//! section framing, or the constants payload requires a written
//! justification recorded in that file before the code changes.
//!
//! # Byte layout
//!
//! Integers are little-endian; `uleb` fields are canonical (minimal)
//! ULEB128. The format is fixed-width framed, index-based, and
//! relocation-free: every payload is addressed by an offset/length pair in
//! the section directory, so a module is position-independent and parses
//! directly from a read-only mmap.
//!
//! ```text
//! offset  size  field
//! 0       4     magic "\0DSM"
//! 4       1     format version (currently 3)
//! 5       1     target class: 0 = bytecode, 1 = wasm, 2 = triple
//! 6       1     flags bitset: bit0 STRIP, bit1 FFI_USED, bit2 HAS_JIT_METADATA
//! 7       1     collector flags (reserved; bit 0 = two-hook interface active)
//! 8       8     collector hook slot 0 (reserved; 0 = dormant)
//! 16      8     collector hook slot 1 (reserved; 0 = dormant)
//! 24      4     max call depth (committed BL-7 field; 0 = value pending)
//! 28      4     IR-version checksum (committed BL-7 field; 0 = value pending)
//! 32      var   target triple (uleb length + UTF-8 bytes), only when the
//!               target class is `triple`
//! ...     var   module name (uleb length + UTF-8 bytes)
//! ...     var   dependency list: uleb count, then per dependency a name
//!               (uleb length + UTF-8 bytes) and a 16-byte content hash
//! ...     16    source hash (blake3-128 of the module source)
//! ...     var   section directory: uleb count, then per section a kind
//!               byte, uleb offset from file start, uleb length
//! ...           section payloads, addressed only through the directory
//! ```
//!
//! Constants-pool entries share one locked encoding everywhere they appear
//! (the module-level constants section and every per-function pool): a tag
//! byte then the value — 0 = i64 (8 bytes LE), 1 = f64 (8 bytes LE, bit
//! pattern), 2 = bool (1 byte), 3 = UTF-8 string (uleb length + bytes),
//! 4 = FuncRef (a key selector byte then the key: selector 0 a uleb
//! dependency-list position, 0 the enclosing module itself, `n` the
//! `n-1`-th dependency entry; selector 1 a uleb-length native-surface
//! name), then a uleb function index within the addressed module's
//! blocks payload.
//!
//! The section directory lists kinds in the writer's canonical order
//! (blocks, constants, type metadata, effect rows, then optionally debug
//! spans), and payloads tile the region after the directory exactly, in
//! directory order: no gaps, no overlaps, no trailing bytes. This is the
//! writer's layout, and the reader enforces both, so every accepted file
//! is byte-identical to its re-serialization by construction.
//!
//! # Locked now, completed later
//!
//! The header, the section framing, and the constants payload are locked
//! now. The BL-7 module-level scalars (max call depth, IR-version
//! checksum) are committed fields with pending values: writers emit 0
//! until M3 defines their computation, and the reader accepts and
//! round-trips non-zero values without validating them. The blocks payload
//! is version 3 (below); the type-metadata and effect-rows payloads remain
//! versioned minimal placeholders (payload version 0 plus a reserved zero
//! uleb), completed at M2/M3 inside the locked framing by bumping their
//! per-section payload version. Debug spans are a list of byte-offset
//! spans into the source and the only strippable section: a module built
//! with [`Flags::STRIP`] set carries no debug-spans section.
//!
//! # Blocks payload, version 3 [BL-5, BL-6, BL-7]
//!
//! ```text
//! u8    payload version = 3
//! uleb  function count
//! per function:
//!   uleb len + UTF-8   local function name (the qualified module path is
//!                      the header's module name)
//!   uleb               params_len
//!   uleb               arg_area_len
//!   uleb               slot count, then one compact type byte per slot:
//!                      I32=0, I64=1, F64=2, Bool=3, Ref=4, Tag=5, Raw=6,
//!                      Fn=7 (tags 6 and 7 are additive payload-v1
//!                      extensions; no version bump)
//!   uleb               constant count, then entries in the locked
//!                      constants-pool encoding (the function's own pool;
//!                      uop ConstIdx operands index it)
//!   uleb               code word count, then that many u32 LE canonical
//!                      uop words (ds-abi's BL-5 encoding)
//!   u32 + u32          entry-descriptor placeholder word pair: zero on
//!                      disk, filled with tier pointers by the loader at
//!                      install time
//! uleb  evidence-chain count, then per chain (the chain table the staged
//!      evidence words index; version 2's addition):
//!   uleb               serving body's function-table index
//!   uleb               tail count, then one uleb chain-table index per
//!                      the serving body's evidence pairs
//! uleb  tag-table count (the name side of the code's tag ids; version
//!      3's addition), then per row:
//!   uleb len + UTF-8   member atom's declared name
//!   uleb               the module-wide tag id
//! uleb  entry-table count (which top-level names are run entries and how
//!      their parameters stage), then per entry:
//!   uleb len + UTF-8   entry name
//!   uleb               plan count, then one plan byte per parameter:
//!                      Int=0, Float=1, Bool=2, OptionalInt=3, Foreign=4
//! ```
//!
//! Read-side validation: every code word must decode at its pc — uops and
//! the 0xE RC pair alike (reserved opcode bytes, non-canonical field bits,
//! and undecodable bias fields are DS4004) — and every jump target must
//! land strictly inside the function's own body. FUNCF-vs-layout
//! agreement, slot-range checking,
//! and pool-index resolution are loader/VM concerns, not format concerns.
//!
//! # Two-hook collector interface (C4, H4)
//!
//! The two reserved u64 hook slots plus the activity flag commit the H4(d)
//! two-hook collector interface (cycle-candidate registration, weak refs)
//! in the header from byte one. The slots are dormant (zero) today; the
//! reader tolerates and preserves any stored values so activating the
//! interface later does not need a format break.
//!
//! # Validation and caching
//!
//! Module identity is the checked blake3-128 source hash (PEP 552 style),
//! never mtime: [`read_checked`] verifies a module against its source
//! bytes. [`cache_path`] derives the content-addressed cache location
//! `$DS_CACHE/ir/<triple>/<hash>.dsmod`; resolving `$DS_CACHE` itself
//! belongs to the CLI layer, not this crate.

#![warn(missing_docs)]

mod cache;
mod hash;
mod header;
mod mmap;
mod module;
mod read;
mod varint;
mod write;

pub use cache::cache_path;
pub use hash::Hash128;
pub use header::{CollectorHooks, Dep, Flags, SectionKind, TargetClass};
pub use mmap::{LoadError, MappedModule, read_mmap};
pub use module::{
    Constant, DebugSpan, EntryArg, EntryPlan, EvidenceChain, FuncRefKey, Function, FunctionError,
    MemberTag, Module,
};
pub use read::{ReadError, read, read_checked};
pub use write::write;

/// Magic bytes every dsmod file starts with: NUL + `DSM`, four bytes (G3.5).
pub const MAGIC: [u8; 4] = *b"\0DSM";

/// The dsmod format version this crate reads and writes.
///
/// Version 2 commits the BL-7 module-level scalars (max call depth at
/// offset 24, IR-version checksum at offset 28) in the fixed header
/// prefix. Version 3 adds the FuncRef constants-pool kind (tag 4) for
/// qualified callees. Version 4 makes that FuncRef selector-led: a
/// dependency-position key (0 the enclosing module) addresses uop
/// dependencies, a native-by-name key covers the native surfaces; the
/// constants payload is locked and carries no payload version of its
/// own, so any change visible in it bumps this version. Files of older
/// versions are rejected.
pub const FORMAT_VERSION: u8 = 4;
