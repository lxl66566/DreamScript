//! Canonical (minimal) ULEB128 encoding, writer side; the reader's decoder
//! lives in [`crate::read`].

/// Appends the minimal ULEB128 encoding of `value`.
pub(crate) fn put_uleb(out: &mut Vec<u8>, value: u64) {
    let mut value = value;
    loop {
        let mut byte = u8::try_from(value & 0x7f).expect("masked to 7 bits");
        value >>= 7;
        if value != 0 {
            byte |= 0x80;
        }
        out.push(byte);
        if value == 0 {
            return;
        }
    }
}

/// Length in bytes of the minimal ULEB128 encoding of `value`.
pub(crate) fn uleb_len(value: u64) -> usize {
    let bits = u64::from(64 - value.leading_zeros()).max(1);
    usize::try_from(bits.div_ceil(7)).expect("uleb length fits usize")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uleb_len_matches_encoding() {
        for &value in &[0u64, 1, 127, 128, 129, 300, u32::MAX.into(), u64::MAX] {
            let mut encoded = Vec::new();
            put_uleb(&mut encoded, value);
            assert_eq!(encoded.len(), uleb_len(value), "value {value}");
        }
    }

    #[test]
    fn uleb_known_bytes() {
        let mut out = Vec::new();
        put_uleb(&mut out, 0);
        put_uleb(&mut out, 624_485);
        put_uleb(&mut out, 128);
        assert_eq!(out, [0x00, 0xe5, 0x8e, 0x26, 0x80, 0x01]);
    }
}
