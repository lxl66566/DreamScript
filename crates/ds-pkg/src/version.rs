//! Package versions: numeric `major.minor.patch` triples.

use core::fmt;

/// A package version, three numeric components ordered lexicographically.
///
/// A manifest spells one to three components (`1`, `1.2`, `1.2.3`), missing
/// components reading as zero; the canonical spelling is always three, so
/// `1.2` and `1.2.0` are the same version and serialize identically — the
/// lock's bytes are a pure function of the model. Pre-release and build
/// metadata wait on the remote registry round.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct Version {
    /// First component.
    pub major: u32,
    /// Second component.
    pub minor: u32,
    /// Third component.
    pub patch: u32,
}

impl Version {
    /// The zero version `0.0.0`.
    pub const ZERO: Self = Self::new(0, 0, 0);

    /// Builds a version from its three components.
    #[must_use]
    pub const fn new(major: u32, minor: u32, patch: u32) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }

    /// Parses one to three dot-separated numeric components, each a `u32`
    /// with no leading zeros (`0` alone is the one legal zero-prefixed
    /// spelling). `at` in the error is the byte offset of the offending
    /// component within `text`, so the caller can lift it into a span.
    ///
    /// # Errors
    ///
    /// [`VersionError::Empty`] for empty text,
    /// [`VersionError::TooManyComponents`] beyond three, and
    /// [`VersionError::BadComponent`] for a component that is empty, holds a
    /// non-digit, carries a leading zero, or overflows `u32`.
    pub fn parse(text: &str) -> Result<Self, VersionError> {
        if text.is_empty() {
            return Err(VersionError::Empty);
        }
        let mut numbers = [0u32; 3];
        let mut at = 0;
        for (count, part) in text.split('.').enumerate() {
            if count == 3 {
                return Err(VersionError::TooManyComponents);
            }
            numbers[count] = parse_component(part, at)?;
            at += part.len() + 1;
        }
        let [major, minor, patch] = numbers;
        Ok(Self::new(major, minor, patch))
    }
}

/// Parses one component: nonempty, ASCII digits only, no leading zero
/// unless the component is exactly `0`, and within `u32`.
fn parse_component(part: &str, at: usize) -> Result<u32, VersionError> {
    if part.is_empty() {
        return Err(VersionError::BadComponent { at });
    }
    let bytes = part.as_bytes();
    if bytes[0] == b'0' && bytes.len() > 1 {
        return Err(VersionError::BadComponent { at });
    }
    let mut value = 0u32;
    for &byte in bytes {
        let Some(digit) = byte.checked_sub(b'0').filter(|digit| *digit < 10) else {
            return Err(VersionError::BadComponent { at });
        };
        value = value
            .checked_mul(10)
            .and_then(|value| value.checked_add(u32::from(digit)))
            .ok_or(VersionError::BadComponent { at })?;
    }
    Ok(value)
}

impl fmt::Display for Version {
    /// The canonical three-component spelling.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// Why a version string is not one.
#[derive(Clone, Copy, PartialEq, Eq, Debug, thiserror::Error)]
pub enum VersionError {
    /// The text is empty.
    #[error("version is empty")]
    Empty,
    /// More than three dot-separated components.
    #[error("more than three version components")]
    TooManyComponents,
    /// A component is empty, non-numeric, zero-prefixed, or beyond `u32`;
    /// `at` is its byte offset within the version text.
    #[error("malformed version component at byte {at}")]
    BadComponent {
        /// Byte offset of the offending component.
        at: usize,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_components_read_as_zero() {
        assert_eq!(Version::parse("1").unwrap(), Version::new(1, 0, 0));
        assert_eq!(Version::parse("1.2").unwrap(), Version::new(1, 2, 0));
        assert_eq!(Version::parse("1.2.3").unwrap(), Version::new(1, 2, 3));
        assert_eq!(Version::parse("0").unwrap(), Version::ZERO);
    }

    #[test]
    fn canonical_display_is_three_components() {
        assert_eq!(Version::parse("1.2").unwrap().to_string(), "1.2.0");
        assert_eq!(Version::ZERO.to_string(), "0.0.0");
    }

    #[test]
    fn ordering_is_lexicographic_by_component() {
        let one = Version::parse("1.2.0").unwrap();
        let two = Version::parse("1.10.0").unwrap();
        assert!(one < two);
        assert!(Version::parse("1.9.9").unwrap() < Version::parse("2.0.0").unwrap());
        assert!(Version::parse("1.2.3").unwrap() > Version::parse("1.2.2").unwrap());
    }

    #[test]
    fn malformed_versions_name_their_offset() {
        assert_eq!(Version::parse(""), Err(VersionError::Empty));
        assert_eq!(
            Version::parse("1.2.3.4"),
            Err(VersionError::TooManyComponents)
        );
        // the second component starts at byte 2
        assert_eq!(
            Version::parse("1.x.3"),
            Err(VersionError::BadComponent { at: 2 })
        );
        assert_eq!(
            Version::parse("1.02"),
            Err(VersionError::BadComponent { at: 2 })
        );
        assert_eq!(
            Version::parse("1."),
            Err(VersionError::BadComponent { at: 2 })
        );
        assert_eq!(
            Version::parse("4294967296"),
            Err(VersionError::BadComponent { at: 0 })
        );
    }
}
