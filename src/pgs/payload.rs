//! Parsed PGS segment payloads.
//!
//! Each segment type (PCS, WDS, PDS, ODS) has a corresponding struct with a
//! `parse(&[u8]) -> Option<Self>` constructor that decodes the raw payload bytes.
//! Returns `None` if the payload is truncated or malformed.

use crate::pgs::segment::CompositionState;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn u16_be(data: &[u8], offset: usize) -> u16 {
    u16::from_be_bytes([data[offset], data[offset + 1]])
}

fn u24_be(data: &[u8], offset: usize) -> u32 {
    ((data[offset] as u32) << 16) | ((data[offset + 1] as u32) << 8) | (data[offset + 2] as u32)
}

fn push_u16_be(buf: &mut Vec<u8>, val: u16) {
    buf.extend_from_slice(&val.to_be_bytes());
}

fn push_u24_be(buf: &mut Vec<u8>, val: u32) {
    buf.push((val >> 16) as u8);
    buf.push((val >> 8) as u8);
    buf.push(val as u8);
}

// ---------------------------------------------------------------------------
// PCS — Presentation Composition Segment (0x16)
// ---------------------------------------------------------------------------

/// Parsed Presentation Composition Segment payload.
#[derive(Debug, Clone)]
pub struct PcsData {
    pub video_width: u16,
    pub video_height: u16,
    pub composition_number: u16,
    pub composition_state: CompositionState,
    pub palette_only: bool,
    pub palette_id: u8,
    pub objects: Vec<CompositionObject>,
}

/// A placement instruction within a PCS: draw object X in window Y at (x, y).
#[derive(Debug, Clone)]
pub struct CompositionObject {
    pub object_id: u16,
    pub window_id: u8,
    pub x: u16,
    pub y: u16,
    pub crop: Option<CropInfo>,
}

/// Cropping rectangle for a composition object.
#[derive(Debug, Clone)]
pub struct CropInfo {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

impl PcsData {
    /// Parse a PCS payload. Returns `None` if truncated or malformed.
    pub fn parse(payload: &[u8]) -> Option<Self> {
        if payload.len() < 11 {
            return None;
        }

        let video_width = u16_be(payload, 0);
        let video_height = u16_be(payload, 2);
        // Byte 4: frame rate (always 0x10, ignored)
        let composition_number = u16_be(payload, 5);
        let composition_state = CompositionState::from_byte(payload[7])?;
        let palette_only = payload[8] == 0x80;
        let palette_id = payload[9];
        let num_objects = payload[10] as usize;

        let mut objects = Vec::with_capacity(num_objects);
        let mut offset = 11;

        for _ in 0..num_objects {
            if offset + 8 > payload.len() {
                return None;
            }

            let object_id = u16_be(payload, offset);
            let window_id = payload[offset + 2];
            let cropped = payload[offset + 3] & 0x80 != 0;
            let x = u16_be(payload, offset + 4);
            let y = u16_be(payload, offset + 6);
            offset += 8;

            let crop = if cropped {
                if offset + 8 > payload.len() {
                    return None;
                }
                let crop = CropInfo {
                    x: u16_be(payload, offset),
                    y: u16_be(payload, offset + 2),
                    width: u16_be(payload, offset + 4),
                    height: u16_be(payload, offset + 6),
                };
                offset += 8;
                Some(crop)
            } else {
                None
            };

            objects.push(CompositionObject {
                object_id,
                window_id,
                x,
                y,
                crop,
            });
        }

        Some(PcsData {
            video_width,
            video_height,
            composition_number,
            composition_state,
            palette_only,
            palette_id,
            objects,
        })
    }

    /// Serialize this PCS payload to bytes (reverse of `parse`).
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::new();
        push_u16_be(&mut buf, self.video_width);
        push_u16_be(&mut buf, self.video_height);
        buf.push(0x10); // frame rate (constant)
        push_u16_be(&mut buf, self.composition_number);
        buf.push(self.composition_state.to_byte());
        buf.push(if self.palette_only { 0x80 } else { 0x00 });
        buf.push(self.palette_id);
        buf.push(self.objects.len() as u8);

        for obj in &self.objects {
            push_u16_be(&mut buf, obj.object_id);
            buf.push(obj.window_id);
            buf.push(if obj.crop.is_some() { 0x80 } else { 0x00 });
            push_u16_be(&mut buf, obj.x);
            push_u16_be(&mut buf, obj.y);
            if let Some(crop) = &obj.crop {
                push_u16_be(&mut buf, crop.x);
                push_u16_be(&mut buf, crop.y);
                push_u16_be(&mut buf, crop.width);
                push_u16_be(&mut buf, crop.height);
            }
        }

        buf
    }
}

// ---------------------------------------------------------------------------
// WDS — Window Definition Segment (0x17)
// ---------------------------------------------------------------------------

/// Parsed Window Definition Segment payload.
#[derive(Debug, Clone)]
pub struct WdsData {
    pub windows: Vec<WindowDefinition>,
}

/// A rectangular screen region where objects are drawn.
#[derive(Debug, Clone)]
pub struct WindowDefinition {
    pub id: u8,
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

impl WdsData {
    /// Parse a WDS payload. Returns `None` if truncated.
    pub fn parse(payload: &[u8]) -> Option<Self> {
        if payload.is_empty() {
            return None;
        }

        let num_windows = payload[0] as usize;
        let mut windows = Vec::with_capacity(num_windows);
        let mut offset = 1;

        for _ in 0..num_windows {
            if offset + 9 > payload.len() {
                return None;
            }

            windows.push(WindowDefinition {
                id: payload[offset],
                x: u16_be(payload, offset + 1),
                y: u16_be(payload, offset + 3),
                width: u16_be(payload, offset + 5),
                height: u16_be(payload, offset + 7),
            });
            offset += 9;
        }

        Some(WdsData { windows })
    }

    /// Serialize this WDS payload to bytes (reverse of `parse`).
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.push(self.windows.len() as u8);
        for w in &self.windows {
            buf.push(w.id);
            push_u16_be(&mut buf, w.x);
            push_u16_be(&mut buf, w.y);
            push_u16_be(&mut buf, w.width);
            push_u16_be(&mut buf, w.height);
        }
        buf
    }
}

// ---------------------------------------------------------------------------
// PDS — Palette Definition Segment (0x14)
// ---------------------------------------------------------------------------

/// Parsed Palette Definition Segment payload.
#[derive(Debug, Clone)]
pub struct PdsData {
    pub id: u8,
    pub version: u8,
    pub entries: Vec<PaletteEntry>,
}

/// A single palette color entry (YCrCb + alpha).
#[derive(Debug, Clone)]
pub struct PaletteEntry {
    pub id: u8,
    pub luminance: u8,
    pub cr: u8,
    pub cb: u8,
    pub alpha: u8,
}

impl PdsData {
    /// Parse a PDS payload. Returns `None` if truncated.
    pub fn parse(payload: &[u8]) -> Option<Self> {
        if payload.len() < 2 {
            return None;
        }

        let id = payload[0];
        let version = payload[1];

        let remaining = &payload[2..];
        if !remaining.len().is_multiple_of(5) {
            return None;
        }

        let num_entries = remaining.len() / 5;
        let mut entries = Vec::with_capacity(num_entries);

        for i in 0..num_entries {
            let base = i * 5;
            entries.push(PaletteEntry {
                id: remaining[base],
                luminance: remaining[base + 1],
                cr: remaining[base + 2],
                cb: remaining[base + 3],
                alpha: remaining[base + 4],
            });
        }

        Some(PdsData {
            id,
            version,
            entries,
        })
    }

    /// Serialize this PDS payload to bytes (reverse of `parse`).
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(2 + self.entries.len() * 5);
        buf.push(self.id);
        buf.push(self.version);
        for e in &self.entries {
            buf.push(e.id);
            buf.push(e.luminance);
            buf.push(e.cr);
            buf.push(e.cb);
            buf.push(e.alpha);
        }
        buf
    }
}

// ---------------------------------------------------------------------------
// ODS — Object Definition Segment (0x15)
// ---------------------------------------------------------------------------

/// Fragment position within a (possibly multi-segment) object.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SequenceFlag {
    /// Complete object in a single segment (0xC0).
    Complete,
    /// First fragment (0x80).
    First,
    /// Last fragment (0x40).
    Last,
    /// Middle fragment (0x00).
    Continuation,
}

impl SequenceFlag {
    pub fn from_byte(b: u8) -> Option<Self> {
        match b {
            0xC0 => Some(SequenceFlag::Complete),
            0x80 => Some(SequenceFlag::First),
            0x40 => Some(SequenceFlag::Last),
            0x00 => Some(SequenceFlag::Continuation),
            _ => None,
        }
    }

    pub fn to_byte(self) -> u8 {
        match self {
            SequenceFlag::Complete => 0xC0,
            SequenceFlag::First => 0x80,
            SequenceFlag::Last => 0x40,
            SequenceFlag::Continuation => 0x00,
        }
    }

    /// Returns the JSON string representation.
    pub fn as_str(&self) -> &'static str {
        match self {
            SequenceFlag::Complete => "complete",
            SequenceFlag::First => "first",
            SequenceFlag::Last => "last",
            SequenceFlag::Continuation => "continuation",
        }
    }
}

/// Parsed Object Definition Segment payload.
#[derive(Debug, Clone)]
pub struct OdsData {
    pub id: u16,
    pub version: u8,
    pub sequence: SequenceFlag,
    /// Total object data length (u24), includes 4 bytes for width+height.
    pub data_length: u32,
    /// Image width — present only on `Complete` or `First` fragments.
    pub width: Option<u16>,
    /// Image height — present only on `Complete` or `First` fragments.
    pub height: Option<u16>,
    /// Raw RLE bitmap bytes (after the ODS header).
    pub rle_data: Vec<u8>,
}

impl OdsData {
    /// Parse an ODS payload. Returns `None` if truncated or malformed.
    pub fn parse(payload: &[u8]) -> Option<Self> {
        if payload.len() < 4 {
            return None;
        }

        let id = u16_be(payload, 0);
        let version = payload[2];
        let sequence = SequenceFlag::from_byte(payload[3])?;

        let is_first = matches!(sequence, SequenceFlag::Complete | SequenceFlag::First);

        // Only First/Complete fragments have data_length + width + height after
        // the 4-byte header. Continuation/Last fragments go straight to RLE data.
        let (data_length, width, height, rle_offset) = if is_first {
            if payload.len() < 11 {
                return None;
            }
            (
                u24_be(payload, 4),
                Some(u16_be(payload, 7)),
                Some(u16_be(payload, 9)),
                11,
            )
        } else {
            (0, None, None, 4)
        };

        let rle_data = payload[rle_offset..].to_vec();

        Some(OdsData {
            id,
            version,
            sequence,
            data_length,
            width,
            height,
            rle_data,
        })
    }

    /// Serialize this ODS payload to bytes (reverse of `parse`).
    pub fn to_bytes(&self) -> Vec<u8> {
        let is_first = matches!(self.sequence, SequenceFlag::Complete | SequenceFlag::First);
        let mut buf = Vec::new();
        push_u16_be(&mut buf, self.id);
        buf.push(self.version);
        buf.push(self.sequence.to_byte());

        if is_first {
            let data_length = self.rle_data.len() as u32 + 4; // +4 for width+height
            push_u24_be(&mut buf, data_length);
            push_u16_be(&mut buf, self.width.unwrap_or(0));
            push_u16_be(&mut buf, self.height.unwrap_or(0));
        }

        buf.extend_from_slice(&self.rle_data);
        buf
    }
}

// ---------------------------------------------------------------------------
// ODS RLE data extraction
// ---------------------------------------------------------------------------

/// Extract the RLE data bytes from an ODS segment payload.
///
/// For Complete/First segments, RLE starts at byte 11 (after 4-byte header + 3-byte data_length + 4-byte dimensions).
/// For Continuation/Last segments, RLE starts at byte 4 (after 4-byte header only — no data_length or dimensions).
///
/// Returns `None` if the payload is too short.
pub fn ods_rle_data(payload: &[u8], sequence: SequenceFlag) -> Option<&[u8]> {
    let offset = match sequence {
        SequenceFlag::Complete | SequenceFlag::First => 11,
        SequenceFlag::Continuation | SequenceFlag::Last => 4,
    };
    if payload.len() < offset {
        return None;
    }
    Some(&payload[offset..])
}

// ---------------------------------------------------------------------------
// ParsedPayload — dispatch enum
// ---------------------------------------------------------------------------

/// A parsed segment payload, dispatched by segment type.
#[derive(Debug, Clone)]
pub enum ParsedPayload {
    Pcs(PcsData),
    Wds(WdsData),
    Pds(PdsData),
    Ods(OdsData),
    End,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- PCS tests --

    #[test]
    fn test_pcs_no_objects() {
        let payload = vec![
            0x07, 0x80, // width: 1920
            0x04, 0x38, // height: 1080
            0x10, // frame rate
            0x00, 0x01, // composition number: 1
            0x80, // composition state: Epoch Start
            0x00, // palette update: false
            0x00, // palette id: 0
            0x00, // num objects: 0
        ];
        let pcs = PcsData::parse(&payload).unwrap();
        assert_eq!(pcs.video_width, 1920);
        assert_eq!(pcs.video_height, 1080);
        assert_eq!(pcs.composition_number, 1);
        assert_eq!(pcs.composition_state, CompositionState::EpochStart);
        assert!(!pcs.palette_only);
        assert_eq!(pcs.palette_id, 0);
        assert!(pcs.objects.is_empty());
    }

    #[test]
    fn test_pcs_one_uncropped_object() {
        let payload = vec![
            0x07, 0x80, // width: 1920
            0x04, 0x38, // height: 1080
            0x10, // frame rate
            0x01, 0xAE, // composition number: 430
            0x80, // composition state: Epoch Start
            0x00, // palette update: false
            0x00, // palette id
            0x01, // num objects: 1
            // Composition object:
            0x00, 0x00, // object_id: 0
            0x00, // window_id: 0
            0x00, // cropped: false
            0x03, 0x05, // x: 773
            0x00, 0x6C, // y: 108
        ];
        let pcs = PcsData::parse(&payload).unwrap();
        assert_eq!(pcs.objects.len(), 1);
        let obj = &pcs.objects[0];
        assert_eq!(obj.object_id, 0);
        assert_eq!(obj.window_id, 0);
        assert_eq!(obj.x, 773);
        assert_eq!(obj.y, 108);
        assert!(obj.crop.is_none());
    }

    #[test]
    fn test_pcs_cropped_object() {
        let payload = vec![
            0x07, 0x80, 0x04, 0x38, 0x10, // width, height, framerate
            0x00, 0x01, // composition number
            0x00, // Normal
            0x00, 0x00, // palette update, palette id
            0x01, // num objects: 1
            // Composition object:
            0x00, 0x01, // object_id: 1
            0x00, // window_id: 0
            0x80, // cropped: true
            0x00, 0x64, // x: 100
            0x00, 0xC8, // y: 200
            // Crop info:
            0x00, 0x0A, // crop x: 10
            0x00, 0x14, // crop y: 20
            0x00, 0x50, // crop width: 80
            0x00, 0x28, // crop height: 40
        ];
        let pcs = PcsData::parse(&payload).unwrap();
        let obj = &pcs.objects[0];
        assert!(obj.crop.is_some());
        let crop = obj.crop.as_ref().unwrap();
        assert_eq!(crop.x, 10);
        assert_eq!(crop.y, 20);
        assert_eq!(crop.width, 80);
        assert_eq!(crop.height, 40);
    }

    #[test]
    fn test_pcs_palette_only_update() {
        let payload = vec![
            0x07, 0x80, 0x04, 0x38, 0x10, 0x00, 0x02, 0x00, // Normal state
            0x80, // palette update: true
            0x03, // palette id: 3
            0x00, // num objects: 0
        ];
        let pcs = PcsData::parse(&payload).unwrap();
        assert!(pcs.palette_only);
        assert_eq!(pcs.palette_id, 3);
    }

    #[test]
    fn test_pcs_truncated() {
        assert!(PcsData::parse(&[0x07, 0x80]).is_none());
        // Truncated: says 1 object but no object data
        let payload = vec![
            0x07, 0x80, 0x04, 0x38, 0x10, 0x00, 0x01, 0x80, 0x00, 0x00, 0x01,
        ];
        assert!(PcsData::parse(&payload).is_none());
    }

    // -- WDS tests --

    #[test]
    fn test_wds_two_windows() {
        let payload = vec![
            0x02, // 2 windows
            // Window 0:
            0x00, // id: 0
            0x03, 0x05, // x: 773
            0x00, 0x6C, // y: 108
            0x01, 0x79, // width: 377
            0x00, 0x2B, // height: 43
            // Window 1:
            0x01, // id: 1
            0x02, 0xE3, // x: 739
            0x03, 0xA0, // y: 928
            0x01, 0xD8, // width: 472
            0x00, 0x2B, // height: 43
        ];
        let wds = WdsData::parse(&payload).unwrap();
        assert_eq!(wds.windows.len(), 2);
        assert_eq!(wds.windows[0].id, 0);
        assert_eq!(wds.windows[0].x, 773);
        assert_eq!(wds.windows[0].width, 377);
        assert_eq!(wds.windows[1].id, 1);
        assert_eq!(wds.windows[1].x, 739);
        assert_eq!(wds.windows[1].width, 472);
    }

    #[test]
    fn test_wds_truncated() {
        assert!(WdsData::parse(&[]).is_none());
        // Says 1 window but only 5 bytes of data
        assert!(WdsData::parse(&[0x01, 0x00, 0x00, 0x00, 0x00, 0x00]).is_none());
    }

    // -- PDS tests --

    #[test]
    fn test_pds_multiple_entries() {
        let payload = vec![
            0x00, // palette id: 0
            0x00, // version: 0
            // Entry 0:
            0x00, 0x10, 0x80, 0x80, 0x00, // id=0, Y=16, Cr=128, Cb=128, A=0
            // Entry 1:
            0x01, 0x10, 0x80, 0x80, 0xFF, // id=1, Y=16, Cr=128, Cb=128, A=255
            // Entry 255:
            0xFF, 0xEB, 0x80, 0x80, 0xFF, // id=255, Y=235, Cr=128, Cb=128, A=255
        ];
        let pds = PdsData::parse(&payload).unwrap();
        assert_eq!(pds.id, 0);
        assert_eq!(pds.version, 0);
        assert_eq!(pds.entries.len(), 3);
        assert_eq!(pds.entries[0].id, 0);
        assert_eq!(pds.entries[0].luminance, 16);
        assert_eq!(pds.entries[0].alpha, 0);
        assert_eq!(pds.entries[2].id, 255);
        assert_eq!(pds.entries[2].luminance, 235);
    }

    #[test]
    fn test_pds_empty_palette() {
        let payload = vec![0x00, 0x00]; // id=0, version=0, no entries
        let pds = PdsData::parse(&payload).unwrap();
        assert!(pds.entries.is_empty());
    }

    #[test]
    fn test_pds_truncated() {
        assert!(PdsData::parse(&[0x00]).is_none());
        // Incomplete entry (3 bytes instead of 5)
        assert!(PdsData::parse(&[0x00, 0x00, 0x01, 0x10, 0x80]).is_none());
    }

    // -- ODS tests --

    #[test]
    fn test_ods_complete() {
        let payload = vec![
            0x00, 0x00, // object id: 0
            0x00, // version: 0
            0xC0, // sequence: first and last (complete)
            0x00, 0x21, 0xBB, // data_length: 8635
            0x01, 0x79, // width: 377
            0x00, 0x2B, // height: 43
            // RLE data would follow...
            0x00, 0x01, 0x02,
        ];
        let ods = OdsData::parse(&payload).unwrap();
        assert_eq!(ods.id, 0);
        assert_eq!(ods.version, 0);
        assert_eq!(ods.sequence, SequenceFlag::Complete);
        assert_eq!(ods.data_length, 8635);
        assert_eq!(ods.width, Some(377));
        assert_eq!(ods.height, Some(43));
    }

    #[test]
    fn test_ods_continuation_fragment() {
        // Continuation fragments have only the 4-byte header (no data_length/width/height).
        let payload = vec![
            0x00, 0x00, // object id: 0
            0x01, // version: 1
            0x00, // sequence: continuation
            // RLE data follows immediately:
            0xAA, 0xBB,
        ];
        let ods = OdsData::parse(&payload).unwrap();
        assert_eq!(ods.sequence, SequenceFlag::Continuation);
        assert_eq!(ods.data_length, 0);
        assert!(ods.width.is_none());
        assert!(ods.height.is_none());
    }

    #[test]
    fn test_ods_last_fragment() {
        // Last fragments have only the 4-byte header (no data_length/width/height).
        let payload = vec![
            0x00, 0x01, // object id: 1
            0x00, // version: 0
            0x40, // sequence: last
            // RLE data follows immediately:
            0xCC, 0xDD,
        ];
        let ods = OdsData::parse(&payload).unwrap();
        assert_eq!(ods.sequence, SequenceFlag::Last);
        assert_eq!(ods.data_length, 0);
        assert!(ods.width.is_none());
    }

    #[test]
    fn test_ods_first_fragment() {
        let payload = vec![
            0x00, 0x00, // object id: 0
            0x00, // version
            0x80, // sequence: first
            0x00, 0x40, 0x00, // data_length
            0x01, 0x79, // width: 377
            0x00, 0x2B, // height: 43
        ];
        let ods = OdsData::parse(&payload).unwrap();
        assert_eq!(ods.sequence, SequenceFlag::First);
        assert_eq!(ods.width, Some(377));
        assert_eq!(ods.height, Some(43));
    }

    #[test]
    fn test_ods_truncated() {
        // Less than 4-byte header.
        assert!(OdsData::parse(&[0x00, 0x00, 0x00]).is_none());
        // First/complete fragment but missing width/height (only 7 bytes, need 11).
        let payload = vec![0x00, 0x00, 0x00, 0xC0, 0x00, 0x10, 0x00];
        assert!(OdsData::parse(&payload).is_none());
    }

    #[test]
    fn test_ods_invalid_sequence_flag() {
        let payload = vec![
            0x00, 0x00, 0x00, 0x20, // invalid sequence flag
            0x00, 0x10, 0x00,
        ];
        assert!(OdsData::parse(&payload).is_none());
    }

    // -- SequenceFlag tests --

    #[test]
    fn test_sequence_flag_as_str() {
        assert_eq!(SequenceFlag::Complete.as_str(), "complete");
        assert_eq!(SequenceFlag::First.as_str(), "first");
        assert_eq!(SequenceFlag::Last.as_str(), "last");
        assert_eq!(SequenceFlag::Continuation.as_str(), "continuation");
    }

    // -- Round-trip tests (to_bytes -> parse) --

    #[test]
    fn test_pcs_roundtrip_no_objects() {
        let payload = vec![
            0x07, 0x80, 0x04, 0x38, 0x10,
            0x00, 0x01, 0x80, 0x00, 0x00, 0x00,
        ];
        let pcs = PcsData::parse(&payload).unwrap();
        assert_eq!(pcs.to_bytes(), payload);
    }

    #[test]
    fn test_pcs_roundtrip_with_object() {
        let payload = vec![
            0x07, 0x80, 0x04, 0x38, 0x10,
            0x01, 0xAE, 0x80, 0x00, 0x00, 0x01,
            0x00, 0x00, 0x00, 0x00, 0x03, 0x05, 0x00, 0x6C,
        ];
        let pcs = PcsData::parse(&payload).unwrap();
        assert_eq!(pcs.to_bytes(), payload);
    }

    #[test]
    fn test_pcs_roundtrip_cropped() {
        let payload = vec![
            0x07, 0x80, 0x04, 0x38, 0x10,
            0x00, 0x01, 0x00, 0x00, 0x00, 0x01,
            0x00, 0x01, 0x00, 0x80, 0x00, 0x64, 0x00, 0xC8,
            0x00, 0x0A, 0x00, 0x14, 0x00, 0x50, 0x00, 0x28,
        ];
        let pcs = PcsData::parse(&payload).unwrap();
        assert_eq!(pcs.to_bytes(), payload);
    }

    #[test]
    fn test_wds_roundtrip() {
        let payload = vec![
            0x02,
            0x00, 0x03, 0x05, 0x00, 0x6C, 0x01, 0x79, 0x00, 0x2B,
            0x01, 0x02, 0xE3, 0x03, 0xA0, 0x01, 0xD8, 0x00, 0x2B,
        ];
        let wds = WdsData::parse(&payload).unwrap();
        assert_eq!(wds.to_bytes(), payload);
    }

    #[test]
    fn test_pds_roundtrip() {
        let payload = vec![
            0x00, 0x00,
            0x00, 0x10, 0x80, 0x80, 0x00,
            0x01, 0x10, 0x80, 0x80, 0xFF,
            0xFF, 0xEB, 0x80, 0x80, 0xFF,
        ];
        let pds = PdsData::parse(&payload).unwrap();
        assert_eq!(pds.to_bytes(), payload);
    }

    #[test]
    fn test_ods_roundtrip_complete() {
        let payload = vec![
            0x00, 0x00, 0x00, 0xC0,
            0x00, 0x00, 0x07, // data_length: 3 + 4 = 7
            0x01, 0x79, 0x00, 0x2B,
            0x00, 0x01, 0x02,
        ];
        let ods = OdsData::parse(&payload).unwrap();
        assert_eq!(ods.rle_data, vec![0x00, 0x01, 0x02]);
        assert_eq!(ods.to_bytes(), payload);
    }

    #[test]
    fn test_ods_roundtrip_continuation() {
        let payload = vec![
            0x00, 0x00, 0x01, 0x00,
            0xAA, 0xBB,
        ];
        let ods = OdsData::parse(&payload).unwrap();
        assert_eq!(ods.rle_data, vec![0xAA, 0xBB]);
        assert_eq!(ods.to_bytes(), payload);
    }

    #[test]
    fn test_ods_roundtrip_last() {
        let payload = vec![
            0x00, 0x01, 0x00, 0x40,
            0xCC, 0xDD,
        ];
        let ods = OdsData::parse(&payload).unwrap();
        assert_eq!(ods.to_bytes(), payload);
    }

    #[test]
    fn test_sequence_flag_roundtrip() {
        for flag in [SequenceFlag::Complete, SequenceFlag::First, SequenceFlag::Last, SequenceFlag::Continuation] {
            assert_eq!(SequenceFlag::from_byte(flag.to_byte()), Some(flag));
        }
    }

    #[test]
    fn test_composition_state_roundtrip() {
        for state in [CompositionState::Normal, CompositionState::AcquisitionPoint, CompositionState::EpochStart] {
            assert_eq!(CompositionState::from_byte(state.to_byte()), Some(state));
        }
    }
}
