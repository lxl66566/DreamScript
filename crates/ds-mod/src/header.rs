//! Header value types: target class, flags bitset, dependency entries,
//! section kinds, and the reserved two-hook collector interface.

use core::fmt;

use crate::hash::Hash128;

/// The compilation target a dsmod artifact carries.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum TargetClass {
    /// Tier-1 typed register bytecode.
    Bytecode,
    /// wasm deployment artifact.
    Wasm,
    /// A native target triple, e.g. `x86_64-pc-windows-msvc`.
    Triple(String),
}

impl TargetClass {
    /// Wire tag stored in header byte 5.
    #[must_use]
    pub const fn tag(&self) -> u8 {
        match self {
            Self::Bytecode => 0,
            Self::Wasm => 1,
            Self::Triple(_) => 2,
        }
    }

    /// Parses a wire tag; `None` for unknown tags.
    #[must_use]
    pub const fn from_tag(tag: u8) -> Option<Self> {
        match tag {
            0 => Some(Self::Bytecode),
            1 => Some(Self::Wasm),
            2 => Some(Self::Triple(String::new())),
            _ => None,
        }
    }
}

impl fmt::Display for TargetClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bytecode => f.write_str("bytecode"),
            Self::Wasm => f.write_str("wasm"),
            Self::Triple(triple) => write!(f, "triple `{triple}`"),
        }
    }
}

/// Header flags bitset, byte 6: STRIP, FFI_USED, HAS_JIT_METADATA.
///
/// Named methods keep call sites free of raw boolean noise; unknown bits are
/// rejected at read time because a bit only gains meaning together with a
/// format-version bump.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub struct Flags(u8);

impl Flags {
    /// The module performs dlopen at load time.
    pub const FFI_USED: u8 = 1 << 1;
    /// The module carries JIT metadata (debugger frame materialization).
    pub const HAS_JIT_METADATA: u8 = 1 << 2;
    /// Every bit assigned in format version 1.
    pub const KNOWN_BITS: u8 = Self::STRIP | Self::FFI_USED | Self::HAS_JIT_METADATA;
    /// Omit the strippable debug-spans section at build time.
    pub const STRIP: u8 = 1 << 0;

    /// Flags from raw bits; `Err` carries the unknown bits.
    pub const fn from_bits(bits: u8) -> Result<Self, u8> {
        if bits & !Self::KNOWN_BITS == 0 { Ok(Self(bits)) } else { Err(bits & !Self::KNOWN_BITS) }
    }

    /// Raw wire bits.
    #[must_use]
    pub const fn bits(self) -> u8 {
        self.0
    }

    /// Whether the STRIP flag is set.
    #[must_use]
    pub const fn strips_debug_spans(self) -> bool {
        self.0 & Self::STRIP != 0
    }

    /// Whether the FFI_USED flag is set.
    #[must_use]
    pub const fn uses_ffi(self) -> bool {
        self.0 & Self::FFI_USED != 0
    }

    /// Whether the HAS_JIT_METADATA flag is set.
    #[must_use]
    pub const fn has_jit_metadata(self) -> bool {
        self.0 & Self::HAS_JIT_METADATA != 0
    }

    /// Returns flags with STRIP set or cleared.
    #[must_use]
    pub const fn with_strip(self, on: bool) -> Self {
        if on { Self(self.0 | Self::STRIP) } else { Self(self.0 & !Self::STRIP) }
    }

    /// Returns flags with FFI_USED set or cleared.
    #[must_use]
    pub const fn with_ffi_used(self, on: bool) -> Self {
        if on { Self(self.0 | Self::FFI_USED) } else { Self(self.0 & !Self::FFI_USED) }
    }

    /// Returns flags with HAS_JIT_METADATA set or cleared.
    #[must_use]
    pub const fn with_jit_metadata(self, on: bool) -> Self {
        if on {
            Self(self.0 | Self::HAS_JIT_METADATA)
        } else {
            Self(self.0 & !Self::HAS_JIT_METADATA)
        }
    }
}

/// One dependency entry of the header's dep list: a package name plus its
/// 16-byte content hash.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Dep {
    /// Dependency package name.
    pub name: String,
    /// blake3-128 hash of the dependency package content.
    pub content_hash: Hash128,
}

/// The five section kinds of the section directory.
///
/// Blocks, type metadata, and effect rows are versioned placeholder
/// payloads completed inside the locked framing; constants are locked; debug spans are
/// the only strippable section.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum SectionKind {
    /// Typed register bytecode blocks (placeholder payload).
    Blocks,
    /// Typed constant pool entries (locked payload).
    Constants,
    /// Type metadata tables (placeholder payload).
    TypeMetadata,
    /// Effect row table (placeholder payload).
    EffectRows,
    /// Strippable byte-offset debug spans.
    DebugSpans,
}

impl SectionKind {
    /// Every kind, in wire (canonical writer) order.
    pub const ALL: [Self; 5] = [
        Self::Blocks,
        Self::Constants,
        Self::TypeMetadata,
        Self::EffectRows,
        Self::DebugSpans,
    ];

    /// Wire tag stored in a directory entry's kind byte.
    #[must_use]
    pub const fn tag(self) -> u8 {
        match self {
            Self::Blocks => 0,
            Self::Constants => 1,
            Self::TypeMetadata => 2,
            Self::EffectRows => 3,
            Self::DebugSpans => 4,
        }
    }

    /// Parses a wire tag; `None` for unknown tags.
    #[must_use]
    pub const fn from_tag(tag: u8) -> Option<Self> {
        match tag {
            0 => Some(Self::Blocks),
            1 => Some(Self::Constants),
            2 => Some(Self::TypeMetadata),
            3 => Some(Self::EffectRows),
            4 => Some(Self::DebugSpans),
            _ => None,
        }
    }

    /// Position in [`SectionKind::ALL`].
    #[must_use]
    pub(crate) const fn index(self) -> usize {
        match self {
            Self::Blocks => 0,
            Self::Constants => 1,
            Self::TypeMetadata => 2,
            Self::EffectRows => 3,
            Self::DebugSpans => 4,
        }
    }
}

impl fmt::Display for SectionKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Blocks => "blocks",
            Self::Constants => "constants",
            Self::TypeMetadata => "type-metadata",
            Self::EffectRows => "effect-rows",
            Self::DebugSpans => "debug-spans",
        })
    }
}

/// The reserved two-hook collector interface committed in the header from
/// byte one: two u64 hook slots (cycle-candidate registration,
/// weak refs) plus an activity flag.
///
/// The interface is dormant: current writers store zeros, and only
/// [`CollectorHooks::DORMANT`] is constructible in-memory. The reader
/// tolerates and preserves any stored values so later activation does not
/// need a format break.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub struct CollectorHooks {
    /// Reserved slot 0; 0 = dormant.
    slot0: u64,
    /// Reserved slot 1; 0 = dormant.
    slot1: u64,
    /// Reserved flags byte; bit 0 marks the hooks active.
    flags: u8,
}

impl CollectorHooks {
    /// The dormant state every current writer emits.
    pub const DORMANT: Self = Self {
        slot0: 0,
        slot1: 0,
        flags: 0,
    };

    /// The dormant state.
    #[must_use]
    pub const fn dormant() -> Self {
        Self::DORMANT
    }

    /// Restores raw wire values; reader-side only, so round-tripping
    /// preserves reserved bytes.
    #[must_use]
    pub(crate) const fn from_raw(slot0: u64, slot1: u64, flags: u8) -> Self {
        Self {
            slot0,
            slot1,
            flags,
        }
    }

    /// Whether both hooks and the activity flag are unset.
    #[must_use]
    pub const fn is_dormant(self) -> bool {
        self.slot0 == 0 && self.slot1 == 0 && self.flags == 0
    }

    /// The two reserved slot values, in wire order.
    #[must_use]
    pub const fn slots(self) -> [u64; 2] {
        [self.slot0, self.slot1]
    }

    /// Whether the activity flag (bit 0 of the reserved flags byte) is set.
    #[must_use]
    pub const fn hooks_active(self) -> bool {
        self.flags & 1 != 0
    }

    /// Raw wire values, in wire order: slot0, slot1, flags byte.
    #[must_use]
    pub(crate) const fn raw(self) -> (u64, u64, u8) {
        (self.slot0, self.slot1, self.flags)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_round_trip_named_methods() {
        let flags = Flags::default()
            .with_strip(true)
            .with_ffi_used(false)
            .with_jit_metadata(true);
        assert!(flags.strips_debug_spans());
        assert!(!flags.uses_ffi());
        assert!(flags.has_jit_metadata());
        let restored = Flags::from_bits(flags.bits()).expect("known bits");
        assert_eq!(restored, flags);
        // Clearing works both ways.
        assert!(!flags.with_strip(false).strips_debug_spans());
    }

    #[test]
    fn flags_reject_unknown_bits() {
        assert_eq!(
            Flags::from_bits(Flags::KNOWN_BITS),
            Ok(Flags(Flags::KNOWN_BITS))
        );
        for unknown in [Flags::KNOWN_BITS + 1, 0x80, 0xff] {
            let err = Flags::from_bits(unknown).expect_err("unknown bits must be rejected");
            assert_eq!(err, unknown & !Flags::KNOWN_BITS);
        }
    }

    #[test]
    fn section_kinds_tag_round_trip() {
        for kind in SectionKind::ALL {
            assert_eq!(SectionKind::from_tag(kind.tag()), Some(kind));
        }
        assert_eq!(SectionKind::from_tag(5), None);
        assert_eq!(
            SectionKind::ALL[SectionKind::Constants.index()],
            SectionKind::Constants
        );
    }

    #[test]
    fn target_class_tags() {
        assert_eq!(
            TargetClass::from_tag(TargetClass::Bytecode.tag()),
            Some(TargetClass::Bytecode)
        );
        assert_eq!(
            TargetClass::from_tag(TargetClass::Wasm.tag()),
            Some(TargetClass::Wasm)
        );
        assert_eq!(
            TargetClass::from_tag(TargetClass::Triple(String::new()).tag()).expect("triple tag"),
            TargetClass::Triple(String::new())
        );
        assert_eq!(TargetClass::from_tag(3), None);
    }

    #[test]
    fn collector_hooks_only_dormant_constructible() {
        let hooks = CollectorHooks::dormant();
        assert!(hooks.is_dormant());
        assert_eq!(hooks.slots(), [0, 0]);
        assert!(!hooks.hooks_active());
        assert_eq!(hooks.raw(), (0, 0, 0));
    }
}
