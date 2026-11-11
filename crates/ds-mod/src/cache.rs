//! Content-addressed cache path layout (G3.5).

use std::path::{Path, PathBuf};

use crate::hash::Hash128;

/// Derives the cache location of a module artifact:
/// `<cache_root>/ir/<target_triple>/<hex-hash>.dsmod` (G3.5).
///
/// Pure function: resolving the `$DS_CACHE` root is the CLI layer's job,
/// not this crate's. `source_hash` is the caller-computed cache key
/// (blake3 over source and dependency hashes).
#[must_use]
pub fn cache_path(cache_root: &Path, target_triple: &str, source_hash: Hash128) -> PathBuf {
    cache_root
        .join("ir")
        .join(target_triple)
        .join(format!("{source_hash}.dsmod"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_shape_is_ir_triple_hash() {
        let hash = Hash128::new([0xab; 16]);
        let path = cache_path(Path::new("C:/ds-cache"), "x86_64-pc-windows-msvc", hash);
        let expected = Path::new("C:/ds-cache")
            .join("ir")
            .join("x86_64-pc-windows-msvc")
            .join(format!("{hash}.dsmod"));
        assert_eq!(path, expected);
        // The trailing three components are shape-fixed regardless of root.
        let parts: Vec<String> = path
            .iter()
            .map(|s| s.to_string_lossy().into_owned())
            .collect();
        let n = parts.len();
        assert_eq!(parts[n - 3..], [
            "ir".to_owned(),
            "x86_64-pc-windows-msvc".to_owned(),
            format!("{hash}.dsmod")
        ]);
    }

    #[test]
    fn filename_is_hex_hash_with_dsmod_suffix() {
        let hash = Hash128::new([
            0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e,
            0x0f, 0x10,
        ]);
        let path = cache_path(Path::new("root"), "t", hash);
        assert_eq!(
            path.file_name().map(|n| n.to_string_lossy()),
            Some("0102030405060708090a0b0c0d0e0f10.dsmod".into())
        );
        assert!(path.starts_with(Path::new("root").join("ir").join("t")));
    }
}
