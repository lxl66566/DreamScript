//! blake3-128 hash values: source hash, dependency content hashes, cache keys.

use core::fmt;

/// A 16-byte blake3 digest (blake3-128): the source hash of a module, the
/// content hash of a dependency, or a cache key [G3.5].
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

impl fmt::Display for Hash128 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for &b in &self.0 {
            write!(f, "{b:02x}")?;
        }
        Ok(())
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
