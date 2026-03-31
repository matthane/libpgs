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

    pub fn to_byte(self) -> u8 {
        match self {
            CompositionState::Normal => 0x00,
            CompositionState::AcquisitionPoint => 0x40,
            CompositionState::EpochStart => 0x80,
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
    pub fn parse(data: &[u8]) -> Result<(PgsSegment, usize), crate::error::PgsError> {
        use crate::error::PgsError;

        if data.len() < HEADER_SIZE {
            return Err(PgsError::InvalidPgs("insufficient data for PGS header".into()));
        }

        if data[0] != PGS_MAGIC[0] || data[1] != PGS_MAGIC[1] {
            return Err(PgsError::InvalidPgs("invalid PGS magic bytes".into()));
        }

        let pts = u32::from_be_bytes([data[2], data[3], data[4], data[5]]) as u64;
        let dts = u32::from_be_bytes([data[6], data[7], data[8], data[9]]) as u64;

        let segment_type = SegmentType::from_byte(data[10])
            .ok_or_else(|| PgsError::InvalidPgs("unknown PGS segment type".into()))?;

        let payload_size = u16::from_be_bytes([data[11], data[12]]) as usize;

        let total_size = HEADER_SIZE + payload_size;
        if data.len() < total_size {
            return Err(PgsError::InvalidPgs("insufficient data for PGS payload".into()));
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

    /// Parse this segment's payload into structured data.
    /// Returns `None` if the segment type doesn't match or the payload is malformed.
    pub fn parse_payload(&self) -> Option<super::payload::ParsedPayload> {
        use super::payload::ParsedPayload;
        match self.segment_type {
            SegmentType::PresentationComposition => {
                super::payload::PcsData::parse(&self.payload).map(ParsedPayload::Pcs)
            }
            SegmentType::WindowDefinition => {
                super::payload::WdsData::parse(&self.payload).map(ParsedPayload::Wds)
            }
            SegmentType::PaletteDefinition => {
                super::payload::PdsData::parse(&self.payload).map(ParsedPayload::Pds)
            }
            SegmentType::ObjectDefinition => {
                super::payload::OdsData::parse(&self.payload).map(ParsedPayload::Ods)
            }
            SegmentType::EndOfDisplaySet => Some(ParsedPayload::End),
        }
    }

    /// Parse a PCS payload. Returns `None` if wrong type or malformed.
    pub fn parse_pcs(&self) -> Option<super::payload::PcsData> {
        if self.segment_type != SegmentType::PresentationComposition {
            return None;
        }
        super::payload::PcsData::parse(&self.payload)
    }

    /// Parse a WDS payload. Returns `None` if wrong type or malformed.
    pub fn parse_wds(&self) -> Option<super::payload::WdsData> {
        if self.segment_type != SegmentType::WindowDefinition {
            return None;
        }
        super::payload::WdsData::parse(&self.payload)
    }

    /// Parse a PDS payload. Returns `None` if wrong type or malformed.
    pub fn parse_pds(&self) -> Option<super::payload::PdsData> {
        if self.segment_type != SegmentType::PaletteDefinition {
            return None;
        }
        super::payload::PdsData::parse(&self.payload)
    }

    /// Parse an ODS payload. Returns `None` if wrong type or malformed.
    pub fn parse_ods(&self) -> Option<super::payload::OdsData> {
        if self.segment_type != SegmentType::ObjectDefinition {
            return None;
        }
        super::payload::OdsData::parse(&self.payload)
    }

    // -- Factory methods --

    /// Create a PCS segment from structured payload data.
    pub fn from_pcs(pts: u64, dts: u64, pcs: &super::payload::PcsData) -> Self {
        PgsSegment {
            pts,
            dts,
            segment_type: SegmentType::PresentationComposition,
            payload: pcs.to_bytes(),
        }
    }

    /// Create a WDS segment from structured payload data.
    pub fn from_wds(pts: u64, dts: u64, wds: &super::payload::WdsData) -> Self {
        PgsSegment {
            pts,
            dts,
            segment_type: SegmentType::WindowDefinition,
            payload: wds.to_bytes(),
        }
    }

    /// Create a PDS segment from structured payload data.
    pub fn from_pds(pts: u64, dts: u64, pds: &super::payload::PdsData) -> Self {
        PgsSegment {
            pts,
            dts,
            segment_type: SegmentType::PaletteDefinition,
            payload: pds.to_bytes(),
        }
    }

    /// Create an ODS segment from structured payload data.
    pub fn from_ods(pts: u64, dts: u64, ods: &super::payload::OdsData) -> Self {
        PgsSegment {
            pts,
            dts,
            segment_type: SegmentType::ObjectDefinition,
            payload: ods.to_bytes(),
        }
    }

    /// Create an END segment.
    pub fn end_segment(pts: u64, dts: u64) -> Self {
        PgsSegment {
            pts,
            dts,
            segment_type: SegmentType::EndOfDisplaySet,
            payload: Vec::new(),
        }
    }

    // -- Payload update methods --

    /// Replace this segment's payload with the serialized PCS data.
    pub fn set_pcs_payload(&mut self, pcs: &super::payload::PcsData) {
        debug_assert_eq!(self.segment_type, SegmentType::PresentationComposition);
        self.payload = pcs.to_bytes();
    }

    /// Replace this segment's payload with the serialized WDS data.
    pub fn set_wds_payload(&mut self, wds: &super::payload::WdsData) {
        debug_assert_eq!(self.segment_type, SegmentType::WindowDefinition);
        self.payload = wds.to_bytes();
    }

    /// Replace this segment's payload with the serialized PDS data.
    pub fn set_pds_payload(&mut self, pds: &super::payload::PdsData) {
        debug_assert_eq!(self.segment_type, SegmentType::PaletteDefinition);
        self.payload = pds.to_bytes();
    }

    /// Replace this segment's payload with the serialized ODS data.
    pub fn set_ods_payload(&mut self, ods: &super::payload::OdsData) {
        debug_assert_eq!(self.segment_type, SegmentType::ObjectDefinition);
        self.payload = ods.to_bytes();
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

    #[test]
    fn test_from_pcs_roundtrip() {
        use super::super::payload::{PcsData, CompositionObject};
        let pcs = PcsData {
            video_width: 1920,
            video_height: 1080,
            composition_number: 5,
            composition_state: CompositionState::EpochStart,
            palette_only: false,
            palette_id: 0,
            objects: vec![CompositionObject {
                object_id: 0,
                window_id: 0,
                x: 100,
                y: 200,
                crop: None,
            }],
        };
        let seg = PgsSegment::from_pcs(90000, 0, &pcs);
        assert_eq!(seg.segment_type, SegmentType::PresentationComposition);
        let parsed = seg.parse_pcs().unwrap();
        assert_eq!(parsed.video_width, 1920);
        assert_eq!(parsed.composition_number, 5);
        assert_eq!(parsed.objects.len(), 1);
    }

    #[test]
    fn test_from_wds_roundtrip() {
        use super::super::payload::{WdsData, WindowDefinition};
        let wds = WdsData {
            windows: vec![WindowDefinition {
                id: 0, x: 50, y: 60, width: 300, height: 40,
            }],
        };
        let seg = PgsSegment::from_wds(90000, 0, &wds);
        let parsed = seg.parse_wds().unwrap();
        assert_eq!(parsed.windows[0].width, 300);
    }

    #[test]
    fn test_from_pds_roundtrip() {
        use super::super::payload::{PdsData, PaletteEntry};
        let pds = PdsData {
            id: 0, version: 0,
            entries: vec![PaletteEntry { id: 1, luminance: 200, cr: 128, cb: 128, alpha: 255 }],
        };
        let seg = PgsSegment::from_pds(90000, 0, &pds);
        let parsed = seg.parse_pds().unwrap();
        assert_eq!(parsed.entries[0].luminance, 200);
    }

    #[test]
    fn test_from_ods_roundtrip() {
        use super::super::payload::{OdsData, SequenceFlag};
        let ods = OdsData {
            id: 0, version: 0,
            sequence: SequenceFlag::Complete,
            data_length: 0, // will be recomputed by to_bytes
            width: Some(10), height: Some(5),
            rle_data: vec![0x01, 0x02, 0x03],
        };
        let seg = PgsSegment::from_ods(90000, 0, &ods);
        let parsed = seg.parse_ods().unwrap();
        assert_eq!(parsed.width, Some(10));
        assert_eq!(parsed.rle_data, vec![0x01, 0x02, 0x03]);
    }

    #[test]
    fn test_end_segment() {
        let seg = PgsSegment::end_segment(90000, 0);
        assert_eq!(seg.segment_type, SegmentType::EndOfDisplaySet);
        assert!(seg.payload.is_empty());
    }

    #[test]
    fn test_set_pds_payload() {
        use super::super::payload::{PdsData, PaletteEntry};
        let pds = PdsData {
            id: 0, version: 0,
            entries: vec![PaletteEntry { id: 0, luminance: 16, cr: 128, cb: 128, alpha: 0 }],
        };
        let mut seg = PgsSegment::from_pds(90000, 0, &pds);
        let original_bytes = seg.payload.clone();

        // Modify and update
        let mut new_pds = seg.parse_pds().unwrap();
        new_pds.entries[0].luminance = 235;
        seg.set_pds_payload(&new_pds);

        assert_ne!(seg.payload, original_bytes);
        let reparsed = seg.parse_pds().unwrap();
        assert_eq!(reparsed.entries[0].luminance, 235);
    }
}
