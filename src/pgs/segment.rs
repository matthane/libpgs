/// PGS segment magic bytes: "PG" (0x50, 0x47).
pub const PGS_MAGIC: [u8; 2] = [0x50, 0x47];

/// PGS segment header size: 13 bytes.
pub const HEADER_SIZE: usize = 13;

/// PGS segment types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SegmentType {
    /// Presentation Composition Segment (0x16)
    PresentationComposition,
    /// Window Definition Segment (0x17)
    WindowDefinition,
    /// Palette Definition Segment (0x14)
    PaletteDefinition,
    /// Object Definition Segment (0x15)
    ObjectDefinition,
    /// End of Display Set (0x80)
    EndOfDisplaySet,
}

impl SegmentType {
    pub fn from_byte(b: u8) -> Option<SegmentType> {
        match b {
            0x16 => Some(SegmentType::PresentationComposition),
            0x17 => Some(SegmentType::WindowDefinition),
            0x14 => Some(SegmentType::PaletteDefinition),
            0x15 => Some(SegmentType::ObjectDefinition),
            0x80 => Some(SegmentType::EndOfDisplaySet),
            _ => None,
        }
    }

    pub fn to_byte(self) -> u8 {
        match self {
            SegmentType::PresentationComposition => 0x16,
            SegmentType::WindowDefinition => 0x17,
            SegmentType::PaletteDefinition => 0x14,
            SegmentType::ObjectDefinition => 0x15,
            SegmentType::EndOfDisplaySet => 0x80,
        }
    }
}

/// Composition state from the PCS segment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompositionState {
    /// Normal update (0x00)
    Normal,
    /// Acquisition point — display refresh (0x40)
    AcquisitionPoint,
    /// Epoch start — new display (0x80)
    EpochStart,
}

impl CompositionState {
    pub fn from_byte(b: u8) -> Option<CompositionState> {
        match b {
            0x00 => Some(CompositionState::Normal),
            0x40 => Some(CompositionState::AcquisitionPoint),
            0x80 => Some(CompositionState::EpochStart),
            _ => None,
        }
    }
}

/// A single PGS segment with its header fields and raw payload.
#[derive(Debug, Clone)]
pub struct PgsSegment {
    /// Presentation timestamp (90 kHz clock).
    pub pts: u64,
    /// Decoding timestamp (90 kHz clock, usually 0).
    pub dts: u64,
    /// Segment type.
    pub segment_type: SegmentType,
    /// Raw payload bytes (after the 13-byte header).
    pub payload: Vec<u8>,
}

impl PgsSegment {
    /// Parse a PGS segment from a byte slice that starts with the 13-byte header.
    /// Returns the segment and the number of bytes consumed.
    pub fn parse(data: &[u8]) -> Result<(PgsSegment, usize), &'static str> {
        if data.len() < HEADER_SIZE {
            return Err("insufficient data for PGS header");
        }

        if data[0] != PGS_MAGIC[0] || data[1] != PGS_MAGIC[1] {
            return Err("invalid PGS magic bytes");
        }

        let pts = u32::from_be_bytes([data[2], data[3], data[4], data[5]]) as u64;
        let dts = u32::from_be_bytes([data[6], data[7], data[8], data[9]]) as u64;

        let segment_type = SegmentType::from_byte(data[10])
            .ok_or("unknown PGS segment type")?;

        let payload_size = u16::from_be_bytes([data[11], data[12]]) as usize;

        let total_size = HEADER_SIZE + payload_size;
        if data.len() < total_size {
            return Err("insufficient data for PGS payload");
        }

        let payload = data[HEADER_SIZE..total_size].to_vec();

        Ok((
            PgsSegment {
                pts,
                dts,
                segment_type,
                payload,
            },
            total_size,
        ))
    }

    /// Serialize this segment back to raw bytes (header + payload).
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(HEADER_SIZE + self.payload.len());
        buf.extend_from_slice(&PGS_MAGIC);
        buf.extend_from_slice(&(self.pts as u32).to_be_bytes());
        buf.extend_from_slice(&(self.dts as u32).to_be_bytes());
        buf.push(self.segment_type.to_byte());
        buf.extend_from_slice(&(self.payload.len() as u16).to_be_bytes());
        buf.extend_from_slice(&self.payload);
        buf
    }

    /// Parse PGS segments from raw format: `segment_type(1) + length(2) + payload`.
    ///
    /// This is the format used inside MKV block payloads and M2TS PES packets,
    /// where PTS/DTS come from the container rather than being embedded in PGS headers.
    pub fn parse_raw_segments(pts: u64, dts: u64, data: &[u8]) -> Vec<PgsSegment> {
        let mut segments = Vec::new();
        let mut offset = 0;

        while offset + 3 <= data.len() {
            let seg_type_byte = data[offset];
            let seg_len = u16::from_be_bytes([data[offset + 1], data[offset + 2]]) as usize;
            offset += 3;

            if offset + seg_len > data.len() {
                break; // Truncated segment.
            }

            if let Some(seg_type) = SegmentType::from_byte(seg_type_byte) {
                segments.push(PgsSegment {
                    pts,
                    dts,
                    segment_type: seg_type,
                    payload: data[offset..offset + seg_len].to_vec(),
                });
            }

            offset += seg_len;
        }

        segments
    }

    /// Get the PTS as milliseconds.
    pub fn pts_ms(&self) -> f64 {
        self.pts as f64 / 90.0
    }

    /// If this is a PCS segment, parse the composition state from the payload.
    pub fn composition_state(&self) -> Option<CompositionState> {
        if self.segment_type != SegmentType::PresentationComposition {
            return None;
        }
        // Composition state is at byte offset 7 in the PCS payload.
        if self.payload.len() < 8 {
            return None;
        }
        CompositionState::from_byte(self.payload[7])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_end_segment() {
        // END segment: PG magic + PTS + DTS + type 0x80 + size 0x0000
        let data = [
            0x50, 0x47, // "PG"
            0x00, 0x04, 0xC1, 0x1C, // PTS = 311580
            0x00, 0x00, 0x00, 0x00, // DTS = 0
            0x80, // END
            0x00, 0x00, // size = 0
        ];

        let (seg, consumed) = PgsSegment::parse(&data).unwrap();
        assert_eq!(consumed, 13);
        assert_eq!(seg.pts, 311580);
        assert_eq!(seg.dts, 0);
        assert_eq!(seg.segment_type, SegmentType::EndOfDisplaySet);
        assert!(seg.payload.is_empty());
        assert!((seg.pts_ms() - 3462.0).abs() < 0.1);
    }

    #[test]
    fn test_roundtrip() {
        let seg = PgsSegment {
            pts: 90000,
            dts: 0,
            segment_type: SegmentType::PaletteDefinition,
            payload: vec![0x00, 0x00, 0x01, 0x10, 0x80, 0x80, 0xFF],
        };
        let bytes = seg.to_bytes();
        let (parsed, consumed) = PgsSegment::parse(&bytes).unwrap();
        assert_eq!(consumed, bytes.len());
        assert_eq!(parsed.pts, seg.pts);
        assert_eq!(parsed.segment_type, seg.segment_type);
        assert_eq!(parsed.payload, seg.payload);
    }

    #[test]
    fn test_invalid_magic() {
        let data = [0x00, 0x00, 0, 0, 0, 0, 0, 0, 0, 0, 0x80, 0, 0];
        assert!(PgsSegment::parse(&data).is_err());
    }
}
