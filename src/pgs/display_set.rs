use crate::error::PgsError;
use crate::pgs::payload::{OdsData, PcsData, PdsData, SequenceFlag, WdsData};
use crate::pgs::rle::encode_rle;
use crate::pgs::segment::{CompositionState, PgsSegment, SegmentType};

/// A complete PGS Display Set — a group of segments from PCS through END.
#[derive(Debug, Clone)]
pub struct DisplaySet {
    /// Presentation timestamp of the PCS segment (90 kHz ticks).
    pub pts: u64,
    /// Presentation timestamp in milliseconds.
    pub pts_ms: f64,
    /// Composition state from the PCS segment.
    pub composition_state: CompositionState,
    /// All segments in this display set, in order (PCS, WDS, PDS, ODS..., END).
    pub segments: Vec<PgsSegment>,
}

/// State machine that assembles PGS segments into complete Display Sets.
///
/// Feed segments one at a time via `push()`. When an END segment completes
/// a display set, `push()` returns `Some(DisplaySet)`.
pub struct DisplaySetAssembler {
    current_segments: Vec<PgsSegment>,
    current_pts: u64,
    current_state: Option<CompositionState>,
}

impl DisplaySetAssembler {
    pub fn new() -> Self {
        Self {
            current_segments: Vec::new(),
            current_pts: 0,
            current_state: None,
        }
    }

    /// Push a PGS segment into the assembler.
    ///
    /// Returns `Some(DisplaySet)` when an END segment completes a display set.
    /// Returns `None` for intermediate segments.
    pub fn push(&mut self, segment: PgsSegment) -> Option<DisplaySet> {
        match segment.segment_type {
            SegmentType::PresentationComposition => {
                // PCS opens a new display set. If we had a partial one, discard it.
                self.current_segments.clear();
                self.current_pts = segment.pts;
                self.current_state = segment.composition_state();
                self.current_segments.push(segment);
                None
            }
            SegmentType::EndOfDisplaySet => {
                self.current_segments.push(segment);

                // Only emit if we have a PCS.
                if self.current_state.is_some() {
                    let ds = DisplaySet {
                        pts: self.current_pts,
                        pts_ms: self.current_pts as f64 / 90.0,
                        composition_state: self.current_state.unwrap_or(CompositionState::Normal),
                        segments: std::mem::take(&mut self.current_segments),
                    };
                    self.current_state = None;
                    Some(ds)
                } else {
                    // END without a preceding PCS — discard.
                    self.current_segments.clear();
                    None
                }
            }
            _ => {
                // Intermediate segment (WDS, PDS, ODS) — accumulate.
                self.current_segments.push(segment);
                None
            }
        }
    }

    /// Reset the assembler, discarding any partial display set.
    pub fn reset(&mut self) {
        self.current_segments.clear();
        self.current_state = None;
    }
}

impl Default for DisplaySetAssembler {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// ObjectBitmap
// ---------------------------------------------------------------------------

/// A complete object bitmap for encoding into ODS segment(s).
pub struct ObjectBitmap {
    pub id: u16,
    pub version: u8,
    pub width: u16,
    pub height: u16,
    /// Flat pixel buffer — palette indices, row-major, length = width * height.
    pub pixels: Vec<u8>,
}

// ---------------------------------------------------------------------------
// DisplaySetBuilder
// ---------------------------------------------------------------------------

/// Builder for constructing a DisplaySet from structured payload types.
pub struct DisplaySetBuilder {
    pts: u64,
    dts: u64,
    pcs: Option<PcsData>,
    wds: Option<WdsData>,
    palettes: Vec<PdsData>,
    objects: Vec<ObjectBitmap>,
}

/// Maximum payload size for a single PGS segment (u16 max).
const MAX_SEGMENT_PAYLOAD: usize = 65535;

/// ODS header size for Complete/First segments: id(2) + version(1) + flag(1) + data_length(3) + width(2) + height(2) = 11.
const ODS_FIRST_HEADER: usize = 11;

/// ODS header size for Continuation/Last segments: id(2) + version(1) + flag(1) = 4.
const ODS_CONT_HEADER: usize = 4;

impl DisplaySetBuilder {
    pub fn new(pts: u64) -> Self {
        DisplaySetBuilder {
            pts,
            dts: 0,
            pcs: None,
            wds: None,
            palettes: Vec::new(),
            objects: Vec::new(),
        }
    }

    pub fn dts(mut self, dts: u64) -> Self {
        self.dts = dts;
        self
    }

    pub fn pcs(mut self, pcs: PcsData) -> Self {
        self.pcs = Some(pcs);
        self
    }

    pub fn wds(mut self, wds: WdsData) -> Self {
        self.wds = Some(wds);
        self
    }

    pub fn palette(mut self, pds: PdsData) -> Self {
        self.palettes.push(pds);
        self
    }

    pub fn object(mut self, obj: ObjectBitmap) -> Self {
        self.objects.push(obj);
        self
    }

    pub fn build(self) -> Result<DisplaySet, PgsError> {
        let pcs = self.pcs.ok_or_else(|| {
            PgsError::EncodingError("DisplaySet requires a PCS".into())
        })?;

        let composition_state = pcs.composition_state;
        let mut segments = Vec::new();

        // PCS segment
        segments.push(PgsSegment::from_pcs(self.pts, self.dts, &pcs));

        // WDS segment
        if let Some(wds) = &self.wds {
            segments.push(PgsSegment::from_wds(self.pts, self.dts, wds));
        }

        // PDS segments
        for pds in &self.palettes {
            segments.push(PgsSegment::from_pds(self.pts, self.dts, pds));
        }

        // ODS segments (with fragmentation if needed)
        for obj in &self.objects {
            let rle = encode_rle(&obj.pixels, obj.width, obj.height).ok_or_else(|| {
                PgsError::EncodingError(format!(
                    "RLE encoding failed for object {} ({}x{}, {} pixels)",
                    obj.id, obj.width, obj.height, obj.pixels.len()
                ))
            })?;

            let total_rle = rle.len();
            let first_max_rle = MAX_SEGMENT_PAYLOAD - ODS_FIRST_HEADER;

            if ODS_FIRST_HEADER + total_rle <= MAX_SEGMENT_PAYLOAD {
                // Single Complete segment
                let ods = OdsData {
                    id: obj.id,
                    version: obj.version,
                    sequence: SequenceFlag::Complete,
                    data_length: total_rle as u32 + 4,
                    width: Some(obj.width),
                    height: Some(obj.height),
                    rle_data: rle,
                };
                segments.push(PgsSegment::from_ods(self.pts, self.dts, &ods));
            } else {
                // Fragment into First + Continuation(s) + Last
                let mut offset = 0;

                // First segment
                let first_chunk = first_max_rle;
                let ods_first = OdsData {
                    id: obj.id,
                    version: obj.version,
                    sequence: SequenceFlag::First,
                    data_length: total_rle as u32 + 4,
                    width: Some(obj.width),
                    height: Some(obj.height),
                    rle_data: rle[..first_chunk].to_vec(),
                };
                segments.push(PgsSegment::from_ods(self.pts, self.dts, &ods_first));
                offset += first_chunk;

                let cont_max_rle = MAX_SEGMENT_PAYLOAD - ODS_CONT_HEADER;

                // Continuation + Last segments
                while offset < total_rle {
                    let remaining = total_rle - offset;
                    let is_last = remaining <= cont_max_rle;
                    let chunk = remaining.min(cont_max_rle);

                    let ods_frag = OdsData {
                        id: obj.id,
                        version: obj.version,
                        sequence: if is_last { SequenceFlag::Last } else { SequenceFlag::Continuation },
                        data_length: 0,
                        width: None,
                        height: None,
                        rle_data: rle[offset..offset + chunk].to_vec(),
                    };
                    segments.push(PgsSegment::from_ods(self.pts, self.dts, &ods_frag));
                    offset += chunk;
                }
            }
        }

        // END segment
        segments.push(PgsSegment::end_segment(self.pts, self.dts));

        Ok(DisplaySet {
            pts: self.pts,
            pts_ms: self.pts as f64 / 90.0,
            composition_state,
            segments,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_segment(seg_type: SegmentType, pts: u64) -> PgsSegment {
        let mut payload = Vec::new();
        if seg_type == SegmentType::PresentationComposition {
            // Minimal PCS payload: 11 bytes (width, height, framerate,
            // comp_number, comp_state, palette_update, palette_id, num_objects)
            payload = vec![
                0x07, 0x80, // width: 1920
                0x04, 0x38, // height: 1080
                0x10, // frame rate
                0x00, 0x01, // composition number
                0x80, // composition state: Epoch Start
                0x00, // palette update: false
                0x00, // palette id
                0x00, // num composition objects: 0
            ];
        }
        PgsSegment {
            pts,
            dts: 0,
            segment_type: seg_type,
            payload,
        }
    }

    #[test]
    fn test_assemble_complete_display_set() {
        let mut asm = DisplaySetAssembler::new();

        assert!(
            asm.push(make_segment(SegmentType::PresentationComposition, 90000))
                .is_none()
        );
        assert!(
            asm.push(make_segment(SegmentType::WindowDefinition, 90000))
                .is_none()
        );
        assert!(
            asm.push(make_segment(SegmentType::PaletteDefinition, 90000))
                .is_none()
        );
        assert!(
            asm.push(make_segment(SegmentType::ObjectDefinition, 90000))
                .is_none()
        );

        let ds = asm.push(make_segment(SegmentType::EndOfDisplaySet, 90000));
        assert!(ds.is_some());

        let ds = ds.unwrap();
        assert_eq!(ds.pts, 90000);
        assert_eq!(ds.segments.len(), 5);
        assert_eq!(ds.composition_state, CompositionState::EpochStart);
    }

    #[test]
    fn test_end_without_pcs_is_discarded() {
        let mut asm = DisplaySetAssembler::new();
        assert!(
            asm.push(make_segment(SegmentType::EndOfDisplaySet, 90000))
                .is_none()
        );
    }

    #[test]
    fn test_pcs_resets_partial() {
        let mut asm = DisplaySetAssembler::new();

        // Start a display set but don't finish it.
        asm.push(make_segment(SegmentType::PresentationComposition, 90000));
        asm.push(make_segment(SegmentType::WindowDefinition, 90000));

        // New PCS resets.
        asm.push(make_segment(SegmentType::PresentationComposition, 180000));
        let ds = asm.push(make_segment(SegmentType::EndOfDisplaySet, 180000));
        assert!(ds.is_some());
        let ds = ds.unwrap();
        assert_eq!(ds.pts, 180000);
        // Only PCS + END from the second set.
        assert_eq!(ds.segments.len(), 2);
    }

    // -- DisplaySetBuilder tests --

    use crate::pgs::payload::{
        CompositionObject, PaletteEntry, WindowDefinition,
    };

    fn test_pcs() -> PcsData {
        PcsData {
            video_width: 1920,
            video_height: 1080,
            composition_number: 1,
            composition_state: CompositionState::EpochStart,
            palette_only: false,
            palette_id: 0,
            objects: vec![CompositionObject {
                object_id: 0,
                window_id: 0,
                x: 100,
                y: 900,
                crop: None,
            }],
        }
    }

    #[test]
    fn test_builder_minimal_pcs_only() {
        let ds = DisplaySetBuilder::new(90000)
            .pcs(PcsData {
                video_width: 1920,
                video_height: 1080,
                composition_number: 0,
                composition_state: CompositionState::Normal,
                palette_only: false,
                palette_id: 0,
                objects: vec![],
            })
            .build()
            .unwrap();

        assert_eq!(ds.pts, 90000);
        assert_eq!(ds.composition_state, CompositionState::Normal);
        // PCS + END = 2 segments
        assert_eq!(ds.segments.len(), 2);
        assert_eq!(ds.segments[0].segment_type, SegmentType::PresentationComposition);
        assert_eq!(ds.segments[1].segment_type, SegmentType::EndOfDisplaySet);
    }

    #[test]
    fn test_builder_no_pcs_error() {
        let result = DisplaySetBuilder::new(90000).build();
        assert!(result.is_err());
    }

    #[test]
    fn test_builder_full_display_set() {
        let pcs = test_pcs();
        let wds = WdsData {
            windows: vec![WindowDefinition {
                id: 0, x: 100, y: 900, width: 200, height: 30,
            }],
        };
        let pds = PdsData {
            id: 0, version: 0,
            entries: vec![
                PaletteEntry { id: 0, luminance: 16, cr: 128, cb: 128, alpha: 0 },
                PaletteEntry { id: 1, luminance: 235, cr: 128, cb: 128, alpha: 255 },
            ],
        };
        // 200x30 image, all pixel index 1
        let pixels = vec![1u8; 200 * 30];
        let obj = ObjectBitmap {
            id: 0, version: 0,
            width: 200, height: 30,
            pixels,
        };

        let ds = DisplaySetBuilder::new(90000)
            .pcs(pcs)
            .wds(wds)
            .palette(pds)
            .object(obj)
            .build()
            .unwrap();

        // PCS + WDS + PDS + ODS + END = 5
        assert_eq!(ds.segments.len(), 5);
        assert_eq!(ds.segments[0].segment_type, SegmentType::PresentationComposition);
        assert_eq!(ds.segments[1].segment_type, SegmentType::WindowDefinition);
        assert_eq!(ds.segments[2].segment_type, SegmentType::PaletteDefinition);
        assert_eq!(ds.segments[3].segment_type, SegmentType::ObjectDefinition);
        assert_eq!(ds.segments[4].segment_type, SegmentType::EndOfDisplaySet);

        // Verify ODS is Complete
        let ods = ds.segments[3].parse_ods().unwrap();
        assert_eq!(ods.sequence, crate::pgs::payload::SequenceFlag::Complete);
        assert_eq!(ods.width, Some(200));
        assert_eq!(ods.height, Some(30));
    }

    #[test]
    fn test_builder_write_and_reparse() {
        let pcs = test_pcs();
        let pds = PdsData {
            id: 0, version: 0,
            entries: vec![
                PaletteEntry { id: 0, luminance: 16, cr: 128, cb: 128, alpha: 0 },
                PaletteEntry { id: 1, luminance: 235, cr: 128, cb: 128, alpha: 255 },
            ],
        };
        let pixels = vec![1u8; 50 * 10];
        let obj = ObjectBitmap {
            id: 0, version: 0,
            width: 50, height: 10,
            pixels,
        };

        let ds = DisplaySetBuilder::new(90000)
            .pcs(pcs)
            .palette(pds)
            .object(obj)
            .build()
            .unwrap();

        // Serialize to a buffer, then reparse
        let mut buf = Vec::new();
        for seg in &ds.segments {
            buf.extend_from_slice(&seg.to_bytes());
        }

        // Parse back
        let mut asm = DisplaySetAssembler::new();
        let mut offset = 0;
        let mut reparsed = Vec::new();
        while offset < buf.len() {
            let (seg, consumed) = PgsSegment::parse(&buf[offset..]).unwrap();
            offset += consumed;
            if let Some(ds) = asm.push(seg) {
                reparsed.push(ds);
            }
        }

        assert_eq!(reparsed.len(), 1);
        let ds2 = &reparsed[0];
        assert_eq!(ds2.pts, 90000);
        assert_eq!(ds2.composition_state, CompositionState::EpochStart);

        // Verify the ODS round-trips
        let ods = ds2.segments.iter()
            .find(|s| s.segment_type == SegmentType::ObjectDefinition)
            .unwrap()
            .parse_ods()
            .unwrap();
        assert_eq!(ods.width, Some(50));
        assert_eq!(ods.height, Some(10));

        // Decode the RLE data and verify pixels
        let decoded = crate::pgs::rle::decode_rle(&ods.rle_data, 50, 10).unwrap();
        assert_eq!(decoded, vec![1u8; 500]);
    }

    #[test]
    fn test_builder_large_ods_fragmentation() {
        let pcs = PcsData {
            video_width: 1920,
            video_height: 1080,
            composition_number: 0,
            composition_state: CompositionState::EpochStart,
            palette_only: false,
            palette_id: 0,
            objects: vec![CompositionObject {
                object_id: 0, window_id: 0, x: 0, y: 0, crop: None,
            }],
        };

        // Create a large bitmap that will need fragmentation.
        // Alternating pixel values to prevent RLE from compressing too well.
        let w: u16 = 1920;
        let h: u16 = 200;
        let total = w as usize * h as usize;
        let pixels: Vec<u8> = (0..total).map(|i| ((i % 254) + 1) as u8).collect();

        let obj = ObjectBitmap {
            id: 0, version: 0,
            width: w, height: h,
            pixels: pixels.clone(),
        };

        let ds = DisplaySetBuilder::new(90000)
            .pcs(pcs)
            .object(obj)
            .build()
            .unwrap();

        // Should have more than 3 segments (PCS + multiple ODS + END)
        assert!(ds.segments.len() > 3, "expected fragmented ODS, got {} segments", ds.segments.len());

        // Verify segment types: PCS, then ODS (First, Continuation..., Last), then END
        assert_eq!(ds.segments[0].segment_type, SegmentType::PresentationComposition);
        assert_eq!(ds.segments.last().unwrap().segment_type, SegmentType::EndOfDisplaySet);

        let ods_segments: Vec<_> = ds.segments.iter()
            .filter(|s| s.segment_type == SegmentType::ObjectDefinition)
            .collect();
        assert!(ods_segments.len() >= 2);

        // First ODS should be First, last ODS should be Last
        let first_ods = ods_segments[0].parse_ods().unwrap();
        assert_eq!(first_ods.sequence, crate::pgs::payload::SequenceFlag::First);
        assert_eq!(first_ods.width, Some(w));
        assert_eq!(first_ods.height, Some(h));

        let last_ods = ods_segments.last().unwrap().parse_ods().unwrap();
        assert_eq!(last_ods.sequence, crate::pgs::payload::SequenceFlag::Last);

        // Reassemble RLE data from all ODS fragments
        let mut all_rle = Vec::new();
        for seg in &ods_segments {
            let ods = seg.parse_ods().unwrap();
            all_rle.extend_from_slice(&ods.rle_data);
        }

        // Decode and verify pixels
        let decoded = crate::pgs::rle::decode_rle(&all_rle, w, h).unwrap();
        assert_eq!(decoded, pixels);

        // Verify no segment payload exceeds 65535 bytes
        for seg in &ds.segments {
            assert!(seg.payload.len() <= 65535, "payload too large: {}", seg.payload.len());
        }
    }
}
