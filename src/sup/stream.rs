use crate::error::PgsError;
use crate::io::SeekBufReader;
use crate::pgs::DisplaySetAssembler;
use crate::pgs::segment::{HEADER_SIZE, PGS_MAGIC, PgsSegment, SegmentType};
use crate::{ContainerFormat, TrackDisplaySet};
use std::fs::File;

/// Streaming state machine for reading raw `.sup` files (concatenated PGS segments).
///
/// A `.sup` file contains a single PGS track — segments are read sequentially
/// and assembled into display sets via [`DisplaySetAssembler`].
pub(crate) struct SupExtractorState {
    reader: SeekBufReader<File>,
    assembler: DisplaySetAssembler,
    done: bool,
}

impl SupExtractorState {
    pub(crate) fn new(reader: SeekBufReader<File>) -> Self {
        Self {
            reader,
            assembler: DisplaySetAssembler::new(),
            done: false,
        }
    }

    pub(crate) fn next_display_set(&mut self) -> Option<Result<TrackDisplaySet, PgsError>> {
        if self.done {
            return None;
        }

        loop {
            // Read the 13-byte PGS segment header.
            let mut header = [0u8; HEADER_SIZE];
            match self.reader.try_read_exact(&mut header) {
                Ok(false) => {
                    self.done = true;
                    return None; // Clean EOF at segment boundary.
                }
                Ok(true) => {}
                Err(e) => {
                    self.done = true;
                    return Some(Err(PgsError::Io(e)));
                }
            }

            // Validate PG magic bytes.
            if header[0] != PGS_MAGIC[0] || header[1] != PGS_MAGIC[1] {
                self.done = true;
                return Some(Err(PgsError::InvalidPgs(format!(
                    "expected PG magic (0x{:02X}{:02X}), got 0x{:02X}{:02X}",
                    PGS_MAGIC[0], PGS_MAGIC[1], header[0], header[1],
                ))));
            }

            // Parse header fields directly (avoids concatenating into a buffer).
            let pts = u32::from_be_bytes([header[2], header[3], header[4], header[5]]) as u64;
            let dts = u32::from_be_bytes([header[6], header[7], header[8], header[9]]) as u64;

            let segment_type = match SegmentType::from_byte(header[10]) {
                Some(t) => t,
                None => {
                    self.done = true;
                    return Some(Err(PgsError::InvalidPgs(format!(
                        "unknown segment type 0x{:02X}",
                        header[10]
                    ))));
                }
            };

            let payload_size = u16::from_be_bytes([header[11], header[12]]) as usize;

            // Read payload.
            let payload = if payload_size > 0 {
                match self.reader.read_bytes(payload_size) {
                    Ok(p) => p,
                    Err(e) => {
                        self.done = true;
                        return Some(Err(PgsError::Io(e)));
                    }
                }
            } else {
                Vec::new()
            };

            let segment = PgsSegment {
                pts,
                dts,
                segment_type,
                payload,
            };

            // Push into assembler — yields a DisplaySet when END closes a set.
            if let Some(ds) = self.assembler.push(segment) {
                return Some(Ok(TrackDisplaySet {
                    track_id: 0,
                    language: None,
                    container: ContainerFormat::Sup,
                    display_set: ds,
                }));
            }
        }
    }

    pub(crate) fn bytes_read(&self) -> u64 {
        self.reader.bytes_read()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pgs::segment::SegmentType;

    fn temp_path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("libpgs_test_{name}"))
    }

    /// Build a minimal .sup file with one complete display set (PCS + END).
    fn build_sup_bytes() -> Vec<u8> {
        let pcs = PgsSegment {
            pts: 90000,
            dts: 0,
            segment_type: SegmentType::PresentationComposition,
            payload: vec![
                0x07, 0x80, // width: 1920
                0x04, 0x38, // height: 1080
                0x10, // frame rate
                0x00, 0x01, // composition number
                0x80, // composition state: Epoch Start
                0x00, // palette update: false
                0x00, // palette id
                0x00, // num composition objects: 0
            ],
        };
        let end = PgsSegment {
            pts: 90000,
            dts: 0,
            segment_type: SegmentType::EndOfDisplaySet,
            payload: Vec::new(),
        };

        let mut data = pcs.to_bytes();
        data.extend_from_slice(&end.to_bytes());
        data
    }

    #[test]
    fn read_single_display_set() {
        let path = temp_path("single_ds.sup");
        std::fs::write(&path, build_sup_bytes()).expect("write temp file");

        let file = File::open(&path).expect("open temp file");
        let reader = SeekBufReader::new(file);
        let mut state = SupExtractorState::new(reader);

        let tds = state
            .next_display_set()
            .expect("should yield a display set")
            .expect("should be Ok");
        assert_eq!(tds.track_id, 0);
        assert_eq!(tds.display_set.pts, 90000);
        assert_eq!(tds.display_set.segments.len(), 2);
        assert_eq!(tds.container, ContainerFormat::Sup);

        assert!(state.next_display_set().is_none());

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn read_multiple_display_sets() {
        let mut data = build_sup_bytes();

        let pcs2 = PgsSegment {
            pts: 180000,
            dts: 0,
            segment_type: SegmentType::PresentationComposition,
            payload: vec![
                0x07, 0x80, 0x04, 0x38, 0x10, 0x00, 0x02, // composition number: 2
                0x00, // composition state: Normal
                0x00, 0x00, 0x00,
            ],
        };
        let end2 = PgsSegment {
            pts: 180000,
            dts: 0,
            segment_type: SegmentType::EndOfDisplaySet,
            payload: Vec::new(),
        };
        data.extend_from_slice(&pcs2.to_bytes());
        data.extend_from_slice(&end2.to_bytes());

        let path = temp_path("multi_ds.sup");
        std::fs::write(&path, &data).expect("write");

        let file = File::open(&path).expect("open");
        let reader = SeekBufReader::new(file);
        let mut state = SupExtractorState::new(reader);

        let ds1 = state.next_display_set().unwrap().unwrap();
        assert_eq!(ds1.display_set.pts, 90000);

        let ds2 = state.next_display_set().unwrap().unwrap();
        assert_eq!(ds2.display_set.pts, 180000);

        assert!(state.next_display_set().is_none());

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn empty_file_yields_none() {
        let path = temp_path("empty.sup");
        std::fs::write(&path, &[]).expect("write empty file");

        let file = File::open(&path).expect("open");
        let reader = SeekBufReader::new(file);
        let mut state = SupExtractorState::new(reader);

        assert!(state.next_display_set().is_none());

        let _ = std::fs::remove_file(&path);
    }
}
