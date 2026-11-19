//! Package versions: numeric `major.minor.patch` triples with optional
//! semver-style pre-release and build metadata (`1.2.3-rc.1+build.42`).

use core::{
    cmp::Ordering,
    fmt,
    hash::{Hash, Hasher},
};

/// One trailing segment's byte capacity: the grammar bounds a
/// `-pre`/`+build` segment the way it bounds the hash spelling, so a
/// version stays one inline copyable value.
const SEGMENT_CAP: usize = 64;

/// One optional trailing segment (`-pre`, `+build`): inline
/// fixed-capacity storage holding its canonical text verbatim.
#[derive(Clone, Copy)]
struct Segment {
    /// The live byte count.
    len: u8,
    /// The segment's bytes, zero beyond `len`.
    bytes: [u8; SEGMENT_CAP],
}

impl Default for Segment {
    /// The empty segment (arrays past 32 bytes derive no Default).
    fn default() -> Self {
        Self::EMPTY
    }
}

impl Segment {
    /// The empty segment (no pre-release, no build metadata).
    const EMPTY: Self = Self {
        len: 0,
        bytes: [0; SEGMENT_CAP],
    };

    /// Stores `text` (its length already validated against the cap).
    fn of(text: &str) -> Self {
        debug_assert!(text.len() <= SEGMENT_CAP, "the caller checked the cap");
        let mut bytes = [0u8; SEGMENT_CAP];
        bytes[..text.len()].copy_from_slice(text.as_bytes());
        Self {
            len: u8::try_from(text.len()).expect("the cap bounds the length"),
            bytes,
        }
    }

    /// The segment's canonical text (the stored bytes are ASCII).
    fn as_str(&self) -> &str {
        std::str::from_utf8(&self.bytes[..usize::from(self.len)])
            .expect("stored segment bytes are ASCII")
    }

    fn is_empty(&self) -> bool {
        self.len == 0
    }
}

/// A package version: three numeric components plus the optional
/// pre-release and build metadata segments. A manifest spells one to
/// three numeric components (`1`, `1.2`, `1.2.3`), missing components
/// reading as zero; the canonical spelling is always three, so `1.2`
/// and `1.2.0` are the same version and serialize identically — the
/// lock's bytes are a pure function of the model — while the
/// pre-release and build segments store their text verbatim and are
/// part of the version's identity.
#[derive(Clone, Copy, Default)]
pub struct Version {
    /// First component.
    pub major: u32,
    /// Second component.
    pub minor: u32,
    /// Third component.
    pub patch: u32,
    /// The `-pre` segment, empty when absent.
    pre: Segment,
    /// The `+build` segment, empty when absent.
    build: Segment,
}

impl Version {
    /// The zero version `0.0.0`.
    pub const ZERO: Self = Self::new(0, 0, 0);

    /// Builds a version from its three numeric components, no
    /// pre-release or build metadata.
    #[must_use]
    pub const fn new(major: u32, minor: u32, patch: u32) -> Self {
        Self {
            major,
            minor,
            patch,
            pre: Segment::EMPTY,
            build: Segment::EMPTY,
        }
    }

    /// The pre-release segment's text (`""` when absent).
    #[must_use]
    pub fn pre(&self) -> &str {
        self.pre.as_str()
    }

    /// The build-metadata segment's text (`""` when absent).
    #[must_use]
    pub fn build(&self) -> &str {
        self.build.as_str()
    }

    /// Parses one version spelling: one to three dot-separated numeric
    /// components (each a `u32` with no leading zeros, `0` alone the
    /// one legal zero-prefixed spelling), an optional `-` pre-release,
    /// and an optional `+` build-metadata segment, each dot-separated
    /// identifiers of ASCII alphanumerics and `-` — a pre-release
    /// numeric identifier carrying no leading zero, per semver
    /// precedence. `at` in every error is the byte offset within
    /// `text`, so the caller can lift it into a span.
    ///
    /// # Errors
    ///
    /// [`VersionError::Empty`] for empty text,
    /// [`VersionError::TooManyComponents`] beyond three,
    /// [`VersionError::BadComponent`] for a component that is empty,
    /// holds a non-digit, carries a leading zero, or overflows `u32`,
    /// [`VersionError::BadPre`] and [`VersionError::BadBuild`] for a
    /// malformed segment identifier, and [`VersionError::TooLong`]
    /// when a segment exceeds the grammar's byte cap.
    pub fn parse(text: &str) -> Result<Self, VersionError> {
        if text.is_empty() {
            return Err(VersionError::Empty);
        }
        // build metadata runs to the end of the text after the first
        // `+`; the pre-release follows the first `-` of what precedes
        let (head, build) = match text.split_once('+') {
            Some((head, build)) => (head, Some((build, head.len() + 1))),
            None => (text, None),
        };
        let (core, pre) = match head.split_once('-') {
            Some((core, pre)) => (core, Some((pre, core.len() + 1))),
            None => (head, None),
        };
        // the core is a prefix of the text, so its component offsets
        // are the text's own offsets
        let mut numbers = [0u32; 3];
        let mut at = 0;
        for (count, part) in core.split('.').enumerate() {
            if count == 3 {
                return Err(VersionError::TooManyComponents);
            }
            numbers[count] = parse_component(part, at)?;
            at += part.len() + 1;
        }
        let pre = match pre {
            Some((text, at)) => parse_segment(text, at, true)?,
            None => Segment::EMPTY,
        };
        let build = match build {
            Some((text, at)) => parse_segment(text, at, false)?,
            None => Segment::EMPTY,
        };
        let [major, minor, patch] = numbers;
        Ok(Self {
            major,
            minor,
            patch,
            pre,
            build,
        })
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

/// Parses one trailing segment: dot-separated identifiers of ASCII
/// alphanumerics and `-`, each nonempty — `strict` refusing a numeric
/// identifier's leading zero (semver's pre-release rule, irrelevant to
/// build metadata). Each error names the offending identifier's own
/// offset; a segment past the byte cap names none.
fn parse_segment(text: &str, at: usize, strict: bool) -> Result<Segment, VersionError> {
    if text.len() > SEGMENT_CAP {
        return Err(VersionError::TooLong);
    }
    // the error variant rides the segment: a pre-release identifier and
    // a build identifier refuse with their own class
    let error = |offset: usize| {
        if strict {
            VersionError::BadPre { at: offset }
        } else {
            VersionError::BadBuild { at: offset }
        }
    };
    let mut ident_at = 0;
    for ident in text.split('.') {
        let numeric = ident.bytes().all(|byte| byte.is_ascii_digit());
        if ident.is_empty()
            || ident
                .bytes()
                .any(|byte| !byte.is_ascii_alphanumeric() && byte != b'-')
            || (strict && ident.len() > 1 && numeric && ident.starts_with('0'))
        {
            return Err(error(at + ident_at));
        }
        ident_at += ident.len() + 1;
    }
    Ok(Segment::of(text))
}

impl PartialEq for Version {
    /// Full identity equality: build metadata included, so two
    /// precedence-equal spellings (`1.0.0+a` and `1.0.0+b`) are still
    /// distinct versions, each its own content identity.
    fn eq(&self, other: &Self) -> bool {
        self.major == other.major
            && self.minor == other.minor
            && self.patch == other.patch
            && self.pre.as_str() == other.pre.as_str()
            && self.build.as_str() == other.build.as_str()
    }
}

impl Eq for Version {}

impl Hash for Version {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.major.hash(state);
        self.minor.hash(state);
        self.patch.hash(state);
        self.pre.as_str().hash(state);
        self.build.as_str().hash(state);
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Version {
    /// Semver precedence: numeric components first, then the
    /// pre-release (a release outranks its own pre-releases, the
    /// identifier comparison numeric-below-alphanumeric), and build
    /// metadata outside precedence — folded in only as a byte-order
    /// tiebreak, so distinct identities keep one total order for map
    /// keys.
    fn cmp(&self, other: &Self) -> Ordering {
        self.major
            .cmp(&other.major)
            .then_with(|| self.minor.cmp(&other.minor))
            .then_with(|| self.patch.cmp(&other.patch))
            .then_with(|| pre_cmp(self.pre.as_str(), other.pre.as_str()))
            .then_with(|| self.build.as_str().cmp(other.build.as_str()))
    }
}

/// The pre-release precedence: an absent pre-release outranks any, and
/// two present ones compare identifier by identifier — a numeric
/// identifier below an alphanumeric one, numerics numerically (no
/// leading zeros, so length then bytes), alphanumerics bytewise — a
/// shorter identifier list below its own extension.
fn pre_cmp(left: &str, right: &str) -> Ordering {
    match (left.is_empty(), right.is_empty()) {
        (true, true) => Ordering::Equal,
        (true, false) => Ordering::Greater,
        (false, true) => Ordering::Less,
        (false, false) => {
            let mut left = left.split('.');
            let mut right = right.split('.');
            loop {
                match (left.next(), right.next()) {
                    (Some(one), Some(two)) => {
                        let order = identifier_cmp(one, two);
                        if order != Ordering::Equal {
                            return order;
                        }
                    },
                    (None, Some(_)) => return Ordering::Less,
                    (Some(_), None) => return Ordering::Greater,
                    (None, None) => return Ordering::Equal,
                }
            }
        },
    }
}

/// One pre-release identifier pair: both numeric compare numerically,
/// a numeric identifier sits below an alphanumeric one, and two
/// alphanumerics compare bytewise.
fn identifier_cmp(left: &str, right: &str) -> Ordering {
    let numeric = |text: &str| !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit());
    match (numeric(left), numeric(right)) {
        (true, true) => (left.len(), left).cmp(&(right.len(), right)),
        (true, false) => Ordering::Less,
        (false, true) => Ordering::Greater,
        (false, false) => left.cmp(right),
    }
}

impl fmt::Display for Version {
    /// The canonical spelling: three components, then the pre-release
    /// and build segments verbatim.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)?;
        if !self.pre.is_empty() {
            write!(f, "-{}", self.pre.as_str())?;
        }
        if !self.build.is_empty() {
            write!(f, "+{}", self.build.as_str())?;
        }
        Ok(())
    }
}

impl fmt::Debug for Version {
    /// The canonical spelling, the model having exactly one per version.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

/// Why a version string is not one.
#[derive(Clone, Copy, PartialEq, Eq, Debug, thiserror::Error)]
pub enum VersionError {
    /// The text is empty.
    #[error("version is empty")]
    Empty,
    /// More than three dot-separated numeric components.
    #[error("more than three version components")]
    TooManyComponents,
    /// A component is empty, non-numeric, zero-prefixed, or beyond `u32`;
    /// `at` is its byte offset within the version text.
    #[error("malformed version component at byte {at}")]
    BadComponent {
        /// Byte offset of the offending component.
        at: usize,
    },
    /// A pre-release identifier is empty, malformed, or a numeric
    /// identifier with a leading zero; `at` is its byte offset within
    /// the version text.
    #[error("malformed pre-release identifier at byte {at}")]
    BadPre {
        /// Byte offset of the offending identifier.
        at: usize,
    },
    /// A build-metadata identifier is empty or malformed; `at` is its
    /// byte offset within the version text.
    #[error("malformed build-metadata identifier at byte {at}")]
    BadBuild {
        /// Byte offset of the offending identifier.
        at: usize,
    },
    /// The pre-release or build segment exceeds the grammar's byte cap.
    #[error("the pre-release or build segment exceeds the version grammar's length cap")]
    TooLong,
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
    fn pre_release_and_build_metadata_round_trip_verbatim() {
        let version = Version::parse("1.2.3-rc.1+build.42").unwrap();
        assert_eq!((version.major, version.minor, version.patch), (1, 2, 3));
        assert_eq!(version.pre(), "rc.1");
        assert_eq!(version.build(), "build.42");
        assert_eq!(version.to_string(), "1.2.3-rc.1+build.42");
        assert_eq!(format!("{version:?}"), "1.2.3-rc.1+build.42");
        // a short core canonicalizes; the segments ride unchanged
        let short = Version::parse("1-rc.1+b").unwrap();
        assert_eq!(short.to_string(), "1.0.0-rc.1+b");
        // one segment alone parses; hyphens live inside identifiers
        assert_eq!(Version::parse("2.0.0-alpha-1").unwrap().pre(), "alpha-1");
        assert_eq!(Version::parse("2.0.0+7fd2e").unwrap().build(), "7fd2e");
        // build metadata may keep leading zeros; pre-release numerics may not
        assert_eq!(Version::parse("1.0.0+007").unwrap().build(), "007");
        assert_eq!(
            Version::parse("1.0.3-rc.01").unwrap_err(),
            VersionError::BadPre { at: 9 }
        );
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
    fn pre_release_ordering_follows_semver_precedence() {
        let chain = [
            "1.0.0-alpha",
            "1.0.0-alpha.1",
            "1.0.0-alpha.beta",
            "1.0.0-beta",
            "1.0.0-beta.2",
            "1.0.0-beta.11",
            "1.0.0-rc.1",
            "1.0.0",
        ];
        for pair in chain.windows(2) {
            let low = Version::parse(pair[0]).unwrap();
            let high = Version::parse(pair[1]).unwrap();
            assert!(low < high, "{} must rank below {}", pair[0], pair[1]);
            assert!(high > low);
        }
        // numerics rank below alphanumerics and compare numerically
        assert!(Version::parse("1.0.0-1").unwrap() < Version::parse("1.0.0-alpha").unwrap());
        assert!(Version::parse("1.0.0-2").unwrap() < Version::parse("1.0.0-10").unwrap());
    }

    #[test]
    fn build_metadata_rides_identity_not_precedence() {
        let plain = Version::parse("1.0.0").unwrap();
        let a = Version::parse("1.0.0+a").unwrap();
        let b = Version::parse("1.0.0+b").unwrap();
        // the build segments order bytewise as the total-order tiebreak
        assert!(a < b, "distinct build metadata keeps one total order");
        assert_ne!(a, b, "distinct build metadata is a distinct identity");
        assert_ne!(a, plain, "build metadata is part of the identity");
        assert_ne!(b, plain);
        assert_eq!(a, Version::parse("1.0.0+a").unwrap());
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
        // an empty pre-release identifier, and a malformed build one
        assert_eq!(
            Version::parse("1.0.0-").unwrap_err(),
            VersionError::BadPre { at: 6 }
        );
        assert_eq!(
            Version::parse("1.0.0-rc.").unwrap_err(),
            VersionError::BadPre { at: 9 }
        );
        assert_eq!(
            Version::parse("1.0.0+").unwrap_err(),
            VersionError::BadBuild { at: 6 }
        );
        assert_eq!(
            Version::parse("1.0.0+bui ld").unwrap_err(),
            VersionError::BadBuild { at: 6 }
        );
        // a segment past the byte cap
        let long = format!("1.0.0-{}", "a".repeat(SEGMENT_CAP + 1));
        assert_eq!(Version::parse(&long).unwrap_err(), VersionError::TooLong);
        let long = format!("1.0.0+{}", "b".repeat(SEGMENT_CAP + 1));
        assert_eq!(Version::parse(&long).unwrap_err(), VersionError::TooLong);
    }
}
