//! The package bundle: the canonical byte framing whose blake3-128 digest
//! is a package's content identity. A bundle carries the canonical manifest
//! text as the reserved `ds.pkg` member beside the package's source
//! members; the manifest's dep hashes ride inside, so the digest cascades
//! transitively through the dependency graph.

use std::collections::BTreeMap;

use ds_diag::FileId;
use ds_mod::Hash128;

use crate::manifest::Manifest;

/// The reserved member every bundle carries: the canonical manifest text.
pub const MANIFEST_MEMBER: &str = "ds.pkg";

/// Bundle magic: `DSPK`.
const MAGIC: [u8; 4] = *b"DSPK";

/// The bundle format version this crate reads and writes.
const FORMAT_VERSION: u8 = 1;

/// The digest's domain tag, folded before the bytes so a package bundle
/// can never collide with another artifact kind hashed over the same
/// content.
const HASH_DOMAIN: &[u8] = b"ds-pkg-package-v1";

/// One installed-or-fetched package: the manifest member plus the source
/// members, path-keyed. The map's path order is the canonical framing
/// order, so [`Package::to_bytes`] is a pure function of the model.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Package {
    members: BTreeMap<String, Vec<u8>>,
}

impl Package {
    /// Builds a package from a manifest and its source members, serializing
    /// the manifest canonically into the reserved member. The members are
    /// framed in ascending path order whatever the input order.
    ///
    /// # Errors
    ///
    /// [`BundleError::NoSources`] when no source member arrives,
    /// [`BundleError::ReservedPath`] for a source member claiming `ds.pkg`,
    /// and [`BundleError::BadPath`] for a member path outside the grammar.
    pub fn new(
        manifest: &Manifest,
        sources: impl IntoIterator<Item = (String, Vec<u8>)>,
    ) -> Result<Self, BundleError> {
        let mut members = BTreeMap::new();
        members.insert(MANIFEST_MEMBER.to_owned(), manifest.to_text().into_bytes());
        let mut source_count = 0;
        for (path, bytes) in sources {
            if path == MANIFEST_MEMBER {
                return Err(BundleError::ReservedPath);
            }
            if !valid_member_path(&path) {
                return Err(BundleError::BadPath { path });
            }
            members.insert(path, bytes);
            source_count += 1;
        }
        if source_count == 0 {
            return Err(BundleError::NoSources);
        }
        Ok(Self { members })
    }

    /// The manifest member parsed on demand; a bundle whose reserved member
    /// is missing or does not decode reports one DS4014 diagnostic, and a
    /// parseable member reports its own drift diagnostics unchanged.
    ///
    /// # Errors
    ///
    /// Every diagnostic the manifest read found.
    pub fn manifest(&self, file: FileId) -> Result<Manifest, Vec<ds_diag::Diagnostic>> {
        let Some(bytes) = self.members.get(MANIFEST_MEMBER) else {
            return Err(vec![BundleError::MissingManifest.diagnostic(file)]);
        };
        let text = std::str::from_utf8(bytes).map_err(|_| {
            vec![
                BundleError::Malformed {
                    at: 0,
                    reason: "the ds.pkg member is not UTF-8",
                }
                .diagnostic(file),
            ]
        })?;
        Manifest::parse(text, file)
    }

    /// Every member, path-keyed in canonical order.
    #[must_use]
    pub fn members(&self) -> &BTreeMap<String, Vec<u8>> {
        &self.members
    }

    /// The canonical bundle bytes: magic, version, member count, then per
    /// member (ascending path order) the framed path and content.
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        let size = 4
            + 1
            + 4
            + self
                .members
                .iter()
                .map(|(path, bytes)| 4 + path.len() + 4 + bytes.len())
                .sum::<usize>();
        let mut out = Vec::with_capacity(size);
        out.extend_from_slice(&MAGIC);
        out.push(FORMAT_VERSION);
        out.extend_from_slice(
            &u32::try_from(self.members.len())
                .expect("member count fits u32")
                .to_le_bytes(),
        );
        for (path, bytes) in &self.members {
            out.extend_from_slice(&length(path.len(), out.len()));
            out.extend_from_slice(path.as_bytes());
            out.extend_from_slice(&length(bytes.len(), out.len()));
            out.extend_from_slice(bytes);
        }
        out
    }

    /// Decodes a bundle, refusing every drift from the canonical framing:
    /// bad magic, a foreign version, truncation, a member path outside the
    /// grammar, and members out of ascending path order (so re-serializing
    /// the decoded model reproduces the input bytes exactly).
    ///
    /// # Errors
    ///
    /// [`BundleError`] naming the first drift and its byte offset.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, BundleError> {
        let bad = |at: usize, reason: &'static str| Err(BundleError::Malformed { at, reason });
        if bytes.len() < 9 || bytes[..4] != MAGIC {
            return bad(0, "not a package bundle (bad magic)");
        }
        if bytes[4] != FORMAT_VERSION {
            return bad(4, "unsupported bundle format version");
        }
        let count = u32::from_le_bytes(bytes[5..9].try_into().expect("four bytes")) as usize;
        // the smallest member is two length words, so a count that cannot
        // fit the remaining bytes is truncation, not a huge allocation
        if count.saturating_mul(8) > bytes.len() - 9 {
            return bad(5, "member count exceeds the bytes present");
        }
        let mut members = BTreeMap::new();
        let mut at = 9;
        let mut last: Option<String> = None;
        for _ in 0..count {
            let path_len = read_length(bytes, &mut at)?;
            let path_at = at;
            let path = read_path(bytes, &mut at, path_len)?;
            if last.as_deref().is_some_and(|last| path.as_str() <= last) {
                return bad(
                    path_at,
                    if last.as_deref() == Some(path.as_str()) {
                        "duplicate member path"
                    } else {
                        "members out of ascending path order"
                    },
                );
            }
            last = Some(path.clone());
            let content_len = read_length(bytes, &mut at)?;
            let end = at
                .checked_add(content_len)
                .filter(|end| *end <= bytes.len())
                .ok_or(BundleError::Malformed {
                    at,
                    reason: "member content exceeds the bytes present",
                })?;
            members.insert(path, bytes[at..end].to_vec());
            at = end;
        }
        if at != bytes.len() {
            return bad(at, "trailing bytes after the last member");
        }
        if !members.contains_key(MANIFEST_MEMBER) {
            return Err(BundleError::MissingManifest);
        }
        Ok(Self { members })
    }

    /// The content identity: blake3-128 over the domain tag and the
    /// canonical bundle bytes.
    #[must_use]
    pub fn content_hash(&self) -> Hash128 {
        bundle_content_hash(&self.to_bytes())
    }
}

/// The content identity of bundle bytes: the domain tag folded before
/// the bytes, so a store validates an installed file by recomputing
/// this over the file's own bytes without decoding first.
#[must_use]
pub fn bundle_content_hash(bytes: &[u8]) -> Hash128 {
    let mut hasher = blake3::Hasher::new();
    hasher.update(HASH_DOMAIN);
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest.as_bytes()[..16]);
    Hash128::new(bytes)
}

/// The member path grammar: relative, `/`-separated, printable ASCII, no
/// `.` or `..` segment, no backslash (a Windows spelling is drift, not a
/// synonym), and never empty.
#[must_use]
pub fn valid_member_path(path: &str) -> bool {
    !path.is_empty()
        && path.bytes().all(|b| matches!(b, 0x20..=0x7e) && b != b'\\')
        && !path.starts_with('/')
        && !path.ends_with('/')
        && path
            .split('/')
            .all(|segment| !segment.is_empty() && segment != "." && segment != "..")
}

/// One length word, refusing a length that runs past the bytes.
fn length(len: usize, at: usize) -> [u8; 4] {
    u32::try_from(len)
        .unwrap_or_else(|_| panic!("bundle member at byte {at} exceeds the u32 length words"))
        .to_le_bytes()
}

fn read_length(bytes: &[u8], at: &mut usize) -> Result<usize, BundleError> {
    let end =
        at.checked_add(4)
            .filter(|end| *end <= bytes.len())
            .ok_or(BundleError::Malformed {
                at: *at,
                reason: "truncated member length word",
            })?;
    let len = u32::from_le_bytes(bytes[*at..end].try_into().expect("four bytes")) as usize;
    *at = end;
    Ok(len)
}

fn read_path(bytes: &[u8], at: &mut usize, len: usize) -> Result<String, BundleError> {
    let end = at
        .checked_add(len)
        .filter(|end| *end <= bytes.len())
        .ok_or(BundleError::Malformed {
            at: *at,
            reason: "member path exceeds the bytes present",
        })?;
    let path = String::from_utf8_lossy(&bytes[*at..end]).into_owned();
    if !valid_member_path(&path) {
        return Err(BundleError::Malformed {
            at: *at,
            reason: "member path is outside the grammar",
        });
    }
    *at = end;
    Ok(path)
}

/// Why a bundle cannot be built or decoded.
#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
pub enum BundleError {
    /// The framing is broken; `at` is the byte offset, `reason` the class.
    #[error("malformed bundle at byte {at}: {reason}")]
    Malformed {
        /// Byte offset of the drift.
        at: usize,
        /// What drifted.
        reason: &'static str,
    },
    /// The reserved `ds.pkg` member is absent.
    #[error("the reserved ds.pkg member is missing")]
    MissingManifest,
    /// A source member claimed the reserved path.
    #[error("a source member claimed the reserved ds.pkg path")]
    ReservedPath,
    /// A member path is outside the grammar.
    #[error("member path `{path}` is outside the grammar")]
    BadPath {
        /// The offending path.
        path: String,
    },
    /// A package with no source member.
    #[error("a package needs at least one source member")]
    NoSources,
}

impl BundleError {
    /// The refusal as a diagnostic (the bundle paths have no manifest text
    /// to point inside, so the span is empty at the bundle's first byte).
    #[must_use]
    pub fn diagnostic(&self, file: FileId) -> ds_diag::Diagnostic {
        use ds_diag::{Diagnostic, ErrorCode, Span};
        Diagnostic::error(ErrorCode::PackageManifestMalformed, self.to_string())
            .with_primary(Span::new(file, 0, 0), "the bundle")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::version::Version;

    fn manifest(name: &str) -> Manifest {
        Manifest::parse(
            &format!("name = \"{name}\"\nversion = \"1.0.0\"\n"),
            FileId::new(0),
        )
        .expect("fixture manifest parses")
    }

    fn package(name: &str, source: &str) -> Package {
        Package::new(&manifest(name), [(
            format!("{name}.ds"),
            source.as_bytes().to_vec(),
        )])
        .expect("fixture package builds")
    }

    #[test]
    fn bytes_round_trip_exactly() {
        let package = package("app", "fn main() {}");
        let bytes = package.to_bytes();
        assert_eq!(Package::from_bytes(&bytes).unwrap(), package);
        // and the second serialization is byte-identical
        assert_eq!(Package::from_bytes(&bytes).unwrap().to_bytes(), bytes);
    }

    #[test]
    fn content_depends_on_every_input() {
        let base = package("app", "fn main() {}");
        let other_source = package("app", "fn main() { }");
        let other_name = package("app2", "fn main() {}");
        assert_ne!(base.content_hash(), other_source.content_hash());
        assert_ne!(base.content_hash(), other_name.content_hash());
        // dep hashes ride inside the manifest member, so a dep change moves
        // the package identity
        let with_dep = Manifest::parse(
            concat!(
                "name = \"app\"\n",
                "version = \"1.0.0\"\n",
                "\n",
                "[deps]\n",
                "@std = { ver = \"1.0.0\", hash = \"blake3:0102030405060708090a0b0c0d0e0f10\" }\n",
            ),
            FileId::new(0),
        )
        .unwrap();
        let with_dep =
            Package::new(&with_dep, [("app.ds".to_owned(), b"fn main() {}".to_vec())]).unwrap();
        assert_ne!(base.content_hash(), with_dep.content_hash());
    }

    #[test]
    fn manifest_member_parses_back() {
        let package = package("app", "fn main() {}");
        let manifest = package.manifest(FileId::new(0)).expect("the member parses");
        assert_eq!(manifest.name, "app");
        assert_eq!(manifest.version, Version::new(1, 0, 0));
    }

    #[test]
    fn construction_and_decode_refuse_drift() {
        let no_sources: [(String, Vec<u8>); 0] = [];
        let errors = [
            (
                Package::new(&manifest("app"), no_sources),
                BundleError::NoSources,
            ),
            (
                Package::new(&manifest("app"), [("ds.pkg".to_owned(), vec![])]),
                BundleError::ReservedPath,
            ),
            (
                Package::new(&manifest("app"), [("../evil".to_owned(), vec![])]),
                BundleError::BadPath {
                    path: "../evil".to_owned(),
                },
            ),
        ];
        for (found, expected) in errors {
            assert_eq!(found.unwrap_err(), expected);
        }
        // decode drift
        assert_eq!(
            Package::from_bytes(b"nope").unwrap_err(),
            BundleError::Malformed {
                at: 0,
                reason: "not a package bundle (bad magic)"
            }
        );
        let mut bytes = package("app", "src").to_bytes();
        bytes[4] = 9;
        assert_eq!(
            Package::from_bytes(&bytes).unwrap_err(),
            BundleError::Malformed {
                at: 4,
                reason: "unsupported bundle format version"
            }
        );
        // truncation inside the member list
        let bytes = package("app", "src").to_bytes();
        assert!(matches!(
            Package::from_bytes(&bytes[..bytes.len() - 1]),
            Err(BundleError::Malformed { .. })
        ));
        // trailing bytes
        let mut bytes = package("app", "src").to_bytes();
        bytes.push(0);
        assert_eq!(
            Package::from_bytes(&bytes).unwrap_err(),
            BundleError::Malformed {
                at: bytes.len() - 1,
                reason: "trailing bytes after the last member"
            }
        );
        // a valid bundle with the manifest member renamed away
        let stripped = Package {
            members: [
                ("app.ds".to_owned(), b"fn main() {}".to_vec()),
                (
                    "zz.pkg".to_owned(),
                    b"name = \"app\"\nversion = \"1.0.0\"\n".to_vec(),
                ),
            ]
            .into_iter()
            .collect(),
        };
        assert_eq!(
            Package::from_bytes(&stripped.to_bytes()).unwrap_err(),
            BundleError::MissingManifest
        );
    }

    #[test]
    fn non_canonical_framing_refuses() {
        let package = package("app", "src");
        // the canonical framing walks members in ascending path order
        // (app.ds before ds.pkg); reframing the same members out of order
        // refuses at the offending path's offset
        let mut reversed = Vec::new();
        reversed.extend_from_slice(&MAGIC);
        reversed.push(FORMAT_VERSION);
        reversed.extend_from_slice(&2u32.to_le_bytes());
        frame_member(
            &mut reversed,
            MANIFEST_MEMBER,
            &package.members()[MANIFEST_MEMBER],
        );
        frame_member(&mut reversed, "app.ds", &package.members()["app.ds"]);
        // the offending member is the second (app.ds after ds.pkg); its
        // path starts after the header, the first member's whole frame,
        // and its own length word
        let ds_pkg = package.members()[MANIFEST_MEMBER].clone();
        let app_at = 9 + 4 + MANIFEST_MEMBER.len() + 4 + ds_pkg.len() + 4;
        assert_eq!(
            Package::from_bytes(&reversed).unwrap_err(),
            BundleError::Malformed {
                at: app_at,
                reason: "members out of ascending path order",
            }
        );
        // and a duplicated member refuses as a duplicate
        let mut doubled = Vec::new();
        doubled.extend_from_slice(&MAGIC);
        doubled.push(FORMAT_VERSION);
        doubled.extend_from_slice(&2u32.to_le_bytes());
        frame_member(&mut doubled, "app.ds", &package.members()["app.ds"]);
        frame_member(&mut doubled, "app.ds", &package.members()["app.ds"]);
        let second_at = 9 + 4 + "app.ds".len() + 4 + package.members()["app.ds"].len() + 4;
        assert_eq!(
            Package::from_bytes(&doubled).unwrap_err(),
            BundleError::Malformed {
                at: second_at,
                reason: "duplicate member path",
            }
        );
    }

    /// Frames one member (path and content) onto the byte stream; the
    /// test's manual framings use it around the fixed nine-byte header.
    fn frame_member(out: &mut Vec<u8>, path: &str, bytes: &[u8]) {
        out.extend_from_slice(
            &u32::try_from(path.len())
                .expect("path fits u32")
                .to_le_bytes(),
        );
        out.extend_from_slice(path.as_bytes());
        out.extend_from_slice(
            &u32::try_from(bytes.len())
                .expect("content fits u32")
                .to_le_bytes(),
        );
        out.extend_from_slice(bytes);
    }

    #[test]
    fn a_stripped_bundle_reports_its_missing_manifest() {
        let stripped = Package {
            members: [("app.ds".to_owned(), b"fn main() {}".to_vec())]
                .into_iter()
                .collect(),
        };
        let diags = stripped.manifest(FileId::new(0)).expect_err("refuses");
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].code, ds_diag::ErrorCode::PackageManifestMalformed);
        assert_eq!(diags[0].message, "the reserved ds.pkg member is missing");
    }
}
