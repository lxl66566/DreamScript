//! blake3-128 hash values: source hash, dependency content hashes, cache keys.

use core::fmt;

/// A 16-byte blake3 digest (blake3-128): the source hash of a module, the
/// content hash of a dependency, or a cache key.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct Hash128([u8; 16]);

impl Hash128 {
    /// All-zero digest.
    pub const ZERO: Self = Self([0; 16]);

    /// Wraps raw digest bytes.
    #[must_use]
    pub const fn new(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    /// Digest of `input`, truncated to the first 16 bytes of the full
    /// blake3 output.
    #[must_use]
    pub fn compute(input: &[u8]) -> Self {
        let full = blake3::hash(input);
        let mut bytes = [0; 16];
        bytes.copy_from_slice(&full.as_bytes()[..16]);
        Self(bytes)
    }

    /// Raw digest bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}

/// The lowercase hex digits, indexed by nibble value.
const HEX: [u8; 16] = *b"0123456789abcdef";

impl fmt::Display for Hash128 {
    /// Lowercase hex, written once: the formatter sees a single string
    /// rather than one `fmt::Arguments` per byte.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut out = [0u8; 32];
        for (index, &byte) in self.0.iter().enumerate() {
            out[index * 2] = HEX[usize::from(byte >> 4)];
            out[index * 2 + 1] = HEX[usize::from(byte & 0x0f)];
        }
        f.write_str(core::str::from_utf8(&out).expect("hex digits are ASCII"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_is_32_lowercase_hex_digits() {
        assert_eq!(Hash128::ZERO.to_string(), "0".repeat(32));
        let hash = Hash128::new([0x0f; 16]);
        assert_eq!(hash.to_string(), "0f".repeat(16));
        // every nibble value, high and low, renders through the table
        let mut bytes = [0u8; 16];
        for (index, byte) in bytes.iter_mut().enumerate() {
            let nibble = u8::try_from(index).expect("index fits u8");
            *byte = (nibble << 4) | nibble;
        }
        assert_eq!(
            Hash128::new(bytes).to_string(),
            "00112233445566778899aabbccddeeff"
        );
    }

    #[test]
    fn compute_truncates_full_blake3() {
        let full = blake3::hash(b"fn main() {}");
        let expected = &full.as_bytes()[..16];
        assert_eq!(Hash128::compute(b"fn main() {}").as_bytes(), expected);
        // Different sources give different digests.
        assert_ne!(
            Hash128::compute(b"fn main() {}"),
            Hash128::compute(b"fn main()  {}")
        );
    }
}
