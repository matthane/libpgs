use crate::pgs::PgsSegment;

/// PES reassembler for a single PID.
///
/// Accumulates TS packet payloads into complete PES packets,
/// then extracts PGS segments from the PES payload.
pub struct PesReassembler {
    buffer: Vec<u8>,
    has_data: bool,
}

impl PesReassembler {
    pub fn new() -> Self {
        Self {
            buffer: Vec::with_capacity(64 * 1024),
            has_data: false,
        }
    }

    /// Push a TS packet payload into the reassembler.
    ///
    /// When `pusi` is true, the previous PES packet (if any) is complete
    /// and its PGS segments are returned.
    pub fn push(&mut self, pusi: bool, payload: &[u8]) -> Vec<PgsSegment> {
        let mut result = Vec::new();

        if pusi && self.has_data {
            result = self.emit();
        }

        if pusi {
            self.buffer.clear();
            self.has_data = true;
        }

        if self.has_data {
            self.buffer.extend_from_slice(payload);
        }

        result
    }

    /// Flush any remaining PES data. Call at end of scan.
    pub fn flush(&mut self) -> Vec<PgsSegment> {
        if self.has_data {
            self.emit()
        } else {
            Vec::new()
        }
    }

    fn emit(&mut self) -> Vec<PgsSegment> {
        let segments = parse_pes_for_pgs(&self.buffer);
        self.buffer.clear();
        self.has_data = false;
        segments
    }
}

/// Parse a complete PES packet and extract PGS segments from its payload.
fn parse_pes_for_pgs(data: &[u8]) -> Vec<PgsSegment> {
    // PES minimum: start_code(3) + stream_id(1) + length(2) + flags(2) + header_len(1) = 9
    if data.len() < 9 {
        return Vec::new();
    }

    // Verify PES start code: 0x00 0x00 0x01
    if data[0] != 0x00 || data[1] != 0x00 || data[2] != 0x01 {
        return Vec::new();
    }

    // data[3] = stream_id (e.g. 0xBD = private_stream_1)
    // data[4..6] = PES_packet_length
    // data[6] = flags byte 1
    // data[7] = flags byte 2: PTS_DTS_flags in bits 7-6
    // data[8] = PES_header_data_length

    let pts_dts_flags = (data[7] >> 6) & 0x03;
    let header_data_length = data[8] as usize;
    let pes_header_end = 9 + header_data_length;

    if pes_header_end > data.len() {
        return Vec::new();
    }

    // Parse PTS (33-bit MPEG timestamp).
    let pts = if pts_dts_flags >= 2 && header_data_length >= 5 {
        parse_timestamp(&data[9..14])
    } else {
        0
    };

    // Parse DTS if both PTS and DTS present.
    let dts = if pts_dts_flags == 3 && header_data_length >= 10 {
        parse_timestamp(&data[14..19])
    } else {
        0
    };

    // PGS segment data starts after the PES header.
    // In M2TS, the PES payload contains: segment_type(1) + segment_length(2) + data
    // (no "PG" magic — that's a .sup file wrapper).
    PgsSegment::parse_raw_segments(pts, dts, &data[pes_header_end..])
}

/// Parse a 33-bit MPEG PTS/DTS timestamp from 5 encoded bytes.
fn parse_timestamp(bytes: &[u8]) -> u64 {
    let a = ((bytes[0] >> 1) & 0x07) as u64;
    let b = bytes[1] as u64;
    let c = ((bytes[2] >> 1) & 0x7F) as u64;
    let d = bytes[3] as u64;
    let e = ((bytes[4] >> 1) & 0x7F) as u64;
    (a << 30) | (b << 22) | (c << 15) | (d << 7) | e
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pgs::SegmentType;

    #[test]
    fn test_parse_timestamp() {
        // PTS = 90000 (1 second at 90 kHz)
        let bytes = [0x21, 0x00, 0x05, 0xBF, 0x21];
        assert_eq!(parse_timestamp(&bytes), 90000);
    }

    #[test]
    fn test_parse_timestamp_large() {
        // PTS = 0x1FFFFFFFF (max 33-bit value = 8589934591)
        let bytes = [0x0F, 0xFF, 0xFF, 0xFF, 0xFF];
        assert_eq!(parse_timestamp(&bytes), 0x1FFFFFFFF);
    }

    #[test]
    fn test_parse_raw_segments_end() {
        // One END segment: type=0x80, length=0
        let data = [0x80, 0x00, 0x00];
        let segments = PgsSegment::parse_raw_segments(90000, 0, &data);
        assert_eq!(segments.len(), 1);
        assert_eq!(segments[0].segment_type, SegmentType::EndOfDisplaySet);
        assert_eq!(segments[0].pts, 90000);
        assert!(segments[0].payload.is_empty());
    }

    #[test]
    fn test_parse_raw_segments_multiple() {
        let mut data = Vec::new();
        // PDS segment: type=0x14, length=3, payload=[0x00, 0x01, 0x02]
        data.extend_from_slice(&[0x14, 0x00, 0x03, 0x00, 0x01, 0x02]);
        // END segment: type=0x80, length=0
        data.extend_from_slice(&[0x80, 0x00, 0x00]);

        let segments = PgsSegment::parse_raw_segments(45000, 0, &data);
        assert_eq!(segments.len(), 2);
        assert_eq!(segments[0].segment_type, SegmentType::PaletteDefinition);
        assert_eq!(segments[0].payload, vec![0x00, 0x01, 0x02]);
        assert_eq!(segments[1].segment_type, SegmentType::EndOfDisplaySet);
    }

    #[test]
    fn test_pes_reassembler() {
        let mut asm = PesReassembler::new();

        // Build a minimal PES packet with a PGS END segment.
        let mut pes = Vec::new();
        pes.extend_from_slice(&[0x00, 0x00, 0x01]); // Start code
        pes.push(0xBD); // Stream ID (private_stream_1)
        pes.extend_from_slice(&[0x00, 0x08]); // PES length
        pes.push(0x80); // flags byte 1
        pes.push(0x80); // flags byte 2: PTS present
        pes.push(0x05); // PES header data length = 5
        // PTS = 90000
        pes.extend_from_slice(&[0x21, 0x00, 0x05, 0xBF, 0x21]);
        // PGS END segment (type=0x80, length=0)
        pes.extend_from_slice(&[0x80, 0x00, 0x00]);

        // Simulate splitting across two TS packets.
        let mid = pes.len() / 2;

        let r1 = asm.push(true, &pes[..mid]);
        assert!(r1.is_empty()); // No previous data to emit.

        let r2 = asm.push(false, &pes[mid..]);
        assert!(r2.is_empty()); // Still accumulating.

        // Flush emits the assembled PES.
        let r3 = asm.flush();
        assert_eq!(r3.len(), 1);
        assert_eq!(r3[0].segment_type, SegmentType::EndOfDisplaySet);
        assert_eq!(r3[0].pts, 90000);
    }

    #[test]
    fn test_pes_reassembler_two_pes_packets() {
        let mut asm = PesReassembler::new();

        // Helper: build a PES packet with a PGS END segment.
        let make_pes = |pts_bytes: &[u8]| {
            let mut pes = Vec::new();
            pes.extend_from_slice(&[0x00, 0x00, 0x01, 0xBD]);
            pes.extend_from_slice(&[0x00, 0x08]);
            pes.push(0x80);
            pes.push(0x80);
            pes.push(0x05);
            pes.extend_from_slice(pts_bytes);
            pes.extend_from_slice(&[0x80, 0x00, 0x00]);
            pes
        };

        let pes1 = make_pes(&[0x21, 0x00, 0x05, 0xBF, 0x21]); // PTS=90000
        let pes2 = make_pes(&[0x21, 0x00, 0x0B, 0x7E, 0x41]); // PTS=180000

        // First PES packet (single TS payload).
        let r1 = asm.push(true, &pes1);
        assert!(r1.is_empty());

        // Second PES packet starts — first is emitted.
        let r2 = asm.push(true, &pes2);
        assert_eq!(r2.len(), 1);
        assert_eq!(r2[0].pts, 90000);

        // Flush emits second.
        let r3 = asm.flush();
        assert_eq!(r3.len(), 1);
        assert_eq!(r3[0].pts, 180000);
    }
}
