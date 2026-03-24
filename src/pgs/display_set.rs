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
}
