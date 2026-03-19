use std::io::{self, Read};

use crate::error::PgsError;

/// Result of reading a VINT: the decoded value and width in bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Vint {
    pub value: u64,
    pub width: u8,
}

/// Read an EBML VINT-encoded element ID from the reader.
///
/// For element IDs, the VINT_WIDTH and VINT_MARKER bits are **included**
/// in the value. So element ID 0xA3 is stored as byte 0xA3 and returned as 0xA3.
#[inline]
pub fn read_element_id<R: Read>(reader: &mut R) -> Result<Vint, PgsError> {
    let first = read_byte(reader)?;
    if first == 0 {
        return Err(PgsError::InvalidVint);
    }

    let width = first.leading_zeros() as u8 + 1;
    if width > 4 {
        // Element IDs are at most 4 bytes in Matroska.
        return Err(PgsError::InvalidVint);
    }

    let mut value = first as u64;
    for _ in 1..width {
        let b = read_byte(reader)?;
        value = (value << 8) | b as u64;
    }

    Ok(Vint { value, width })
}

/// Read an EBML VINT-encoded data size from the reader.
///
/// For data sizes, the leading VINT_MARKER bit is **stripped**. A size where
/// all data bits are 1 means "unknown size" and is returned as `u64::MAX`.
#[inline]
pub fn read_element_size<R: Read>(reader: &mut R) -> Result<Vint, PgsError> {
    let first = read_byte(reader)?;
    if first == 0 {
        return Err(PgsError::InvalidVint);
    }

    let width = first.leading_zeros() as u8 + 1;
    if width > 8 {
        return Err(PgsError::InvalidVint);
    }

    // Strip the leading marker bit.
    // For width=8, the entire first byte is the marker, so mask=0.
    let mask = if width < 8 { 0xFFu8 >> width } else { 0u8 };
    let mut value = (first & mask) as u64;

    for _ in 1..width {
        let b = read_byte(reader)?;
        value = (value << 8) | b as u64;
    }

    // Check for "unknown size" (all data bits set to 1).
    let max_for_width = (1u64 << (7 * width)) - 1;
    if value == max_for_width {
        value = u64::MAX;
    }

    Ok(Vint { value, width })
}

/// Read a VINT-encoded track number from a Block/SimpleBlock header.
///
/// Same encoding as element sizes: marker bit stripped.
#[inline]
pub fn read_track_number<R: Read>(reader: &mut R) -> Result<Vint, PgsError> {
    // Track numbers use the same encoding as element sizes (marker stripped).
    read_element_size(reader)
}

#[inline]
fn read_byte<R: Read>(reader: &mut R) -> Result<u8, PgsError> {
    let mut buf = [0u8; 1];
    reader.read_exact(&mut buf).map_err(|e| {
        if e.kind() == io::ErrorKind::UnexpectedEof {
            PgsError::InvalidVint
        } else {
            PgsError::Io(e)
        }
    })?;
    Ok(buf[0])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn test_element_id_1_byte() {
        // SimpleBlock element ID: 0xA3
        let mut r = Cursor::new([0xA3]);
        let v = read_element_id(&mut r).unwrap();
        assert_eq!(v.value, 0xA3);
        assert_eq!(v.width, 1);
    }

    #[test]
    fn test_element_id_2_byte() {
        // Cluster element ID: 0x1F43B675 — actually 4 bytes
        // TrackNumber element ID: 0xD7 — 1 byte
        // Let's test a 2-byte ID: CodecID = 0x86 is 1 byte.
        // TrackType = 0x83 is 1 byte.
        // SeekID = 0x53AB is 2 bytes.
        let mut r = Cursor::new([0x53, 0xAB]);
        let v = read_element_id(&mut r).unwrap();
        assert_eq!(v.value, 0x53AB);
        assert_eq!(v.width, 2);
    }

    #[test]
    fn test_element_id_4_byte() {
        // Segment element ID: 0x18538067
        let mut r = Cursor::new([0x18, 0x53, 0x80, 0x67]);
        let v = read_element_id(&mut r).unwrap();
        assert_eq!(v.value, 0x18538067);
        assert_eq!(v.width, 4);
    }

    #[test]
    fn test_element_size_1_byte() {
        // Size 5: encoded as 0x85 (1-byte VINT, marker bit 0x80 | 0x05)
        let mut r = Cursor::new([0x85]);
        let v = read_element_size(&mut r).unwrap();
        assert_eq!(v.value, 5);
        assert_eq!(v.width, 1);
    }

    #[test]
    fn test_element_size_2_byte() {
        // Size 200: encoded as 0x40C8 (2-byte VINT, marker 0x4000 | 200)
        let mut r = Cursor::new([0x40, 0xC8]);
        let v = read_element_size(&mut r).unwrap();
        assert_eq!(v.value, 200);
        assert_eq!(v.width, 2);
    }

    #[test]
    fn test_element_size_unknown() {
        // Unknown size for 1-byte VINT: 0xFF (all data bits = 1, value = 127 = 2^7-1)
        let mut r = Cursor::new([0xFF]);
        let v = read_element_size(&mut r).unwrap();
        assert_eq!(v.value, u64::MAX);
        assert_eq!(v.width, 1);
    }

    #[test]
    fn test_track_number() {
        // Track 1: 0x81 (marker 0x80 | 1)
        let mut r = Cursor::new([0x81]);
        let v = read_track_number(&mut r).unwrap();
        assert_eq!(v.value, 1);
        assert_eq!(v.width, 1);

        // Track 3: 0x83
        let mut r = Cursor::new([0x83]);
        let v = read_track_number(&mut r).unwrap();
        assert_eq!(v.value, 3);
        assert_eq!(v.width, 1);
    }

    #[test]
    fn test_vint_zero_is_error() {
        let mut r = Cursor::new([0x00]);
        assert!(read_element_id(&mut r).is_err());
    }
}
