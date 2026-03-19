use crate::error::PgsError;
use crate::io::SeekBufReader;
use std::io::{Read, Seek};

/// TS sync byte.
pub const SYNC_BYTE: u8 = 0x47;

/// Standard TS packet size (bytes).
pub const TS_PACKET_SIZE: usize = 188;

/// M2TS packet size (4-byte timecode prefix + 188-byte TS packet).
pub const M2TS_PACKET_SIZE: usize = 192;

/// Number of consecutive sync bytes to confirm format detection.
const SYNC_CHECK_COUNT: usize = 5;

/// Packet format: M2TS (192-byte) or raw TS (188-byte).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PacketFormat {
    /// 192-byte packets (4-byte timecode + 188-byte TS).
    M2ts,
    /// 188-byte raw TS packets.
    RawTs,
}

impl PacketFormat {
    pub fn packet_size(self) -> usize {
        match self {
            PacketFormat::M2ts => M2TS_PACKET_SIZE,
            PacketFormat::RawTs => TS_PACKET_SIZE,
        }
    }

    pub fn sync_offset(self) -> usize {
        match self {
            PacketFormat::M2ts => 4,
            PacketFormat::RawTs => 0,
        }
    }
}

/// Parsed 4-byte TS packet header.
#[derive(Debug, Clone, Copy)]
pub struct TsHeader {
    /// Payload Unit Start Indicator.
    pub pusi: bool,
    /// Packet Identifier (13 bits).
    pub pid: u16,
    /// Adaptation field control (2 bits).
    pub adaptation_field_control: u8,
    /// Continuity counter (4 bits).
    pub continuity_counter: u8,
}

impl TsHeader {
    /// Parse a 4-byte TS header. First byte must be 0x47.
    pub fn parse(bytes: &[u8; 4]) -> Result<TsHeader, PgsError> {
        if bytes[0] != SYNC_BYTE {
            return Err(PgsError::InvalidTs(format!(
                "expected sync 0x47, got 0x{:02X}",
                bytes[0]
            )));
        }
        Ok(TsHeader {
            pusi: bytes[1] & 0x40 != 0,
            pid: ((bytes[1] as u16 & 0x1F) << 8) | bytes[2] as u16,
            adaptation_field_control: (bytes[3] >> 4) & 0x03,
            continuity_counter: bytes[3] & 0x0F,
        })
    }

    pub fn has_payload(self) -> bool {
        self.adaptation_field_control & 0x01 != 0
    }

    pub fn has_adaptation_field(self) -> bool {
        self.adaptation_field_control & 0x02 != 0
    }
}

/// Detect packet format by checking sync byte patterns at the start of the file.
pub fn detect_packet_format<R: Read + Seek>(
    reader: &mut SeekBufReader<R>,
) -> Result<PacketFormat, PgsError> {
    reader.seek_to(0)?;
    let buf = reader.read_bytes(M2TS_PACKET_SIZE * (SYNC_CHECK_COUNT + 1))?;

    // Check M2TS first (0x47 at offset 4, then every 192 bytes).
    if check_sync_pattern(&buf, 4, M2TS_PACKET_SIZE, SYNC_CHECK_COUNT) {
        return Ok(PacketFormat::M2ts);
    }

    // Check raw TS (0x47 at offset 0, then every 188 bytes).
    if check_sync_pattern(&buf, 0, TS_PACKET_SIZE, SYNC_CHECK_COUNT) {
        return Ok(PacketFormat::RawTs);
    }

    Err(PgsError::InvalidTs("no valid TS sync pattern found".into()))
}

fn check_sync_pattern(buf: &[u8], first_offset: usize, packet_size: usize, count: usize) -> bool {
    for i in 0..count {
        let offset = first_offset + i * packet_size;
        if offset >= buf.len() || buf[offset] != SYNC_BYTE {
            return false;
        }
    }
    true
}

/// Extract the TS header and payload from a 188-byte TS packet.
pub fn extract_payload(ts_data: &[u8; TS_PACKET_SIZE]) -> Result<(TsHeader, &[u8]), PgsError> {
    let header = TsHeader::parse(&[ts_data[0], ts_data[1], ts_data[2], ts_data[3]])?;

    if !header.has_payload() {
        return Ok((header, &[]));
    }

    let mut offset = 4;
    if header.has_adaptation_field() {
        if offset >= TS_PACKET_SIZE {
            return Err(PgsError::InvalidTs(
                "no room for adaptation field length".into(),
            ));
        }
        let adapt_len = ts_data[4] as usize;
        offset = 5 + adapt_len;
        if offset > TS_PACKET_SIZE {
            return Err(PgsError::InvalidTs(
                "adaptation field exceeds packet".into(),
            ));
        }
    }

    Ok((header, &ts_data[offset..]))
}

/// Read the next packet and return the 188-byte TS portion. Returns None at EOF.
pub fn read_next_packet<R: Read + Seek>(
    reader: &mut SeekBufReader<R>,
    format: PacketFormat,
) -> Result<Option<[u8; TS_PACKET_SIZE]>, PgsError> {
    let packet_size = format.packet_size();
    let sync_offset = format.sync_offset();

    let mut buf = [0u8; M2TS_PACKET_SIZE];
    if !reader.try_read_exact(&mut buf[..packet_size])? {
        return Ok(None);
    }

    let mut ts_buf = [0u8; TS_PACKET_SIZE];
    ts_buf.copy_from_slice(&buf[sync_offset..sync_offset + TS_PACKET_SIZE]);
    Ok(Some(ts_buf))
}

/// Attempt to resync to the next valid packet boundary after sync loss.
///
/// Scans forward from the reader's current position up to `scan_limit` bytes,
/// looking for two consecutive sync bytes at the correct packet stride.
/// On success, positions the reader at the start of the found packet and returns
/// the byte position. Returns `None` if no valid sync is found within the limit.
pub fn resync<R: Read + Seek>(
    reader: &mut SeekBufReader<R>,
    format: PacketFormat,
    scan_limit: u64,
) -> Result<Option<u64>, PgsError> {
    let packet_size = format.packet_size();
    let sync_offset = format.sync_offset();
    let start = reader.position();
    let end = start + scan_limit;
    const SCAN_CHUNK: usize = 64 * 1024;

    while reader.position() + (packet_size * 2) as u64 <= end {
        let chunk_start = reader.position();
        let to_read = SCAN_CHUNK.min((end - chunk_start) as usize);
        if to_read < packet_size * 2 {
            break;
        }

        let buf = match reader.read_bytes(to_read) {
            Ok(buf) => buf,
            Err(_) => break,
        };

        // Look for two consecutive sync bytes at the correct stride.
        let search_end = buf.len().saturating_sub(sync_offset + packet_size);
        for i in 0..search_end {
            let first = i + sync_offset;
            let second = first + packet_size;
            if second < buf.len() && buf[first] == SYNC_BYTE && buf[second] == SYNC_BYTE {
                let found = chunk_start + i as u64;
                reader.seek_to(found)?;
                return Ok(Some(found));
            }
        }

        // Overlap by one packet to handle patterns at chunk boundaries.
        let overlap = packet_size as u64;
        let new_pos = chunk_start + to_read as u64 - overlap;
        if new_pos > chunk_start {
            reader.seek_to(new_pos)?;
        } else {
            break;
        }
    }

    Ok(None)
}

/// Align a byte position up to the next packet boundary.
pub fn align_to_packet(pos: u64, packet_size: u64) -> u64 {
    let rem = pos % packet_size;
    if rem == 0 {
        pos
    } else {
        pos + packet_size - rem
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ts_header_parse() {
        // Sync=0x47, PUSI=1, PID=0x100, AFC=01 (payload only), CC=0
        let bytes = [0x47, 0x41, 0x00, 0x10];
        let h = TsHeader::parse(&bytes).unwrap();
        assert!(h.pusi);
        assert_eq!(h.pid, 256);
        assert!(h.has_payload());
        assert!(!h.has_adaptation_field());
    }

    #[test]
    fn test_ts_header_bad_sync() {
        let bytes = [0x00, 0x41, 0x00, 0x10];
        assert!(TsHeader::parse(&bytes).is_err());
    }

    #[test]
    fn test_packet_format_sizes() {
        assert_eq!(PacketFormat::M2ts.packet_size(), 192);
        assert_eq!(PacketFormat::RawTs.packet_size(), 188);
        assert_eq!(PacketFormat::M2ts.sync_offset(), 4);
        assert_eq!(PacketFormat::RawTs.sync_offset(), 0);
    }

    #[test]
    fn test_extract_payload_no_adaptation() {
        let mut ts = [0u8; TS_PACKET_SIZE];
        ts[0] = 0x47;
        ts[1] = 0x40; // PUSI=1
        ts[2] = 0x00; // PID=0
        ts[3] = 0x10; // AFC=01, CC=0
        ts[4] = 0xFF; // First payload byte

        let (header, payload) = extract_payload(&ts).unwrap();
        assert!(header.pusi);
        assert_eq!(header.pid, 0);
        assert_eq!(payload.len(), 184);
        assert_eq!(payload[0], 0xFF);
    }

    #[test]
    fn test_extract_payload_with_adaptation() {
        let mut ts = [0u8; TS_PACKET_SIZE];
        ts[0] = 0x47;
        ts[1] = 0x00;
        ts[2] = 0x41; // PID=0x41
        ts[3] = 0x30; // AFC=11 (adaptation + payload), CC=0
        ts[4] = 0x07; // Adaptation field length = 7
        // ts[5..12] = adaptation field data
        ts[12] = 0xAA; // First payload byte

        let (header, payload) = extract_payload(&ts).unwrap();
        assert_eq!(header.pid, 0x41);
        assert!(header.has_payload());
        assert!(header.has_adaptation_field());
        assert_eq!(payload.len(), 188 - 12);
        assert_eq!(payload[0], 0xAA);
    }

    #[test]
    fn test_align_to_packet() {
        assert_eq!(align_to_packet(0, 192), 0);
        assert_eq!(align_to_packet(1, 192), 192);
        assert_eq!(align_to_packet(192, 192), 192);
        assert_eq!(align_to_packet(193, 192), 384);
    }
}
