use crate::error::PgsError;
use crate::io::SeekBufReader;
use crate::pgs::DisplaySetAssembler;
use crate::pgs::segment::{HEADER_SIZE, PGS_MAGIC, PgsSegment, SegmentType};
use crate::{ContainerFormat, TrackDisplaySet};
use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::Path;

/// How far back from EOF to scan for the last PGS segment header.
const SUP_TAIL_SCAN: u64 = 64 * 1024;

/// Streaming state machine for reading raw `.sup` files (concatenated PGS segments).
///
/// A `.sup` file contains a single PGS track. Segments are read sequentially
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

    /// Apply a time range for seeking.
    ///
    /// Estimates a byte offset based on first/last PTS and seeks the reader.
    pub(crate) fn set_time_range(&mut self, start_ms: Option<f64>, _end_ms: Option<f64>) {
        if let Some(start) = start_ms {
            let file_size = self.reader.file_size().unwrap_or(0);
            if file_size < HEADER_SIZE as u64 {
                return;
            }

            // Read first PTS (bytes 2-5 of file).
            let first_pts = {
                let _ = self.reader.seek_to(0);
                let mut hdr = [0u8; HEADER_SIZE];
                if self.reader.try_read_exact(&mut hdr).unwrap_or(false)
                    && hdr[0] == PGS_MAGIC[0]
                    && hdr[1] == PGS_MAGIC[1]
                {
                    u32::from_be_bytes([hdr[2], hdr[3], hdr[4], hdr[5]]) as u64
                } else {
                    let _ = self.reader.seek_to(0);
                    return;
                }
            };

            let last_pts = {
                let scan_start = file_size.saturating_sub(SUP_TAIL_SCAN);
                let _ = self.reader.seek_to(scan_start);
                let remaining = (file_size - scan_start) as usize;
                if let Ok(block) = self.reader.read_bytes(remaining) {
                    find_last_sup_pts(&block)
                } else {
                    None
                }
            };

            if let Some(last_pts) = last_pts {
                let duration = last_pts.saturating_sub(first_pts);
                if duration > 0 {
                    let target_pts = (start * 90.0) as u64;
                    let ratio = target_pts as f64 / duration as f64;
                    let estimated = (file_size as f64 * ratio) as u64;
                    let margin = (2 * 1024 * 1024u64).min(file_size / 100);
                    let seek_to = estimated.saturating_sub(margin);
                    let _ = self.reader.seek_to(seek_to);
                    self.scan_to_pg_magic();
                    return;
                }
            }

            let _ = self.reader.seek_to(0);
        }
    }

    /// Scan forward to the next PGS segment header (PG magic bytes).
    fn scan_to_pg_magic(&mut self) {
        let mut buf = [0u8; 1];
        loop {
            match self.reader.try_read_exact(&mut buf) {
                Ok(true) => {
                    if buf[0] == PGS_MAGIC[0] {
                        match self.reader.try_read_exact(&mut buf) {
                            Ok(true) if buf[0] == PGS_MAGIC[1] => {
                                let pos = self.reader.position();
                                let _ = self.reader.seek_to(pos - 2);
                                return;
                            }
                            Ok(true) => continue,
                            _ => return,
                        }
                    }
                }
                _ => return,
            }
        }
    }

    pub(crate) fn next_display_set(&mut self) -> Option<Result<TrackDisplaySet, PgsError>> {
        if self.done {
            return None;
        }

        loop {
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

            // Push into assembler, yields a DisplaySet when END closes a set.
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

/// Counts of display sets in a `.sup` file, produced by a fast pre-scan that
/// reads only segment headers (and PCS payloads) while seeking over other
/// payloads. `content + clear == total`.
#[derive(Debug, Clone, Copy, Default)]
pub struct SupDisplaySetCounts {
    /// Total display sets (count of END segments).
    pub total: u64,
    /// Display sets whose PCS has at least one composition object (a visible subtitle frame).
    pub content: u64,
    /// Display sets whose PCS has zero composition objects (a "clear screen" display set).
    pub clear: u64,
}

/// Fast pre-scan of a `.sup` file that returns `(total, content, clear)` display
/// set counts by walking segment headers only, no RLE decoding, no assembler.
///
/// Touches ~1-2% of file bytes: reads the 13-byte header of every segment plus
/// the payload of every PCS (tiny, usually <64 bytes), and seeks over all other
/// payloads. Completes in well under a second on multi-GB files.
pub fn count_display_sets(path: &Path) -> Result<SupDisplaySetCounts, PgsError> {
    let file = File::open(path)?;
    let mut reader = BufReader::with_capacity(256 * 1024, file);

    let mut counts = SupDisplaySetCounts::default();
    let mut header = [0u8; HEADER_SIZE];

    loop {
        match read_exact_or_eof(&mut reader, &mut header)? {
            false => return Ok(counts),
            true => {}
        }

        if header[0] != PGS_MAGIC[0] || header[1] != PGS_MAGIC[1] {
            return Err(PgsError::InvalidPgs(format!(
                "expected PG magic (0x{:02X}{:02X}), got 0x{:02X}{:02X}",
                PGS_MAGIC[0], PGS_MAGIC[1], header[0], header[1],
            )));
        }

        let segment_type = SegmentType::from_byte(header[10]).ok_or_else(|| {
            PgsError::InvalidPgs(format!("unknown segment type 0x{:02X}", header[10]))
        })?;
        let payload_size = u16::from_be_bytes([header[11], header[12]]) as usize;

        match segment_type {
            SegmentType::PresentationComposition => {
                // PCS payloads are tiny; read to inspect num_composition_objects at byte 10
                if payload_size < 11 {
                    return Err(PgsError::InvalidPgs(format!(
                        "PCS payload too small: {payload_size} bytes"
                    )));
                }
                let mut buf = vec![0u8; payload_size];
                reader.read_exact(&mut buf)?;
                let num_objects = buf[10];
                if num_objects == 0 {
                    counts.clear += 1;
                } else {
                    counts.content += 1;
                }
            }
            SegmentType::EndOfDisplaySet => {
                counts.total += 1;
                // END payloads are zero-length in practice, but honor payload_size.
                if payload_size > 0 {
                    reader.seek(SeekFrom::Current(payload_size as i64))?;
                }
            }
            _ => {
                if payload_size > 0 {
                    reader.seek(SeekFrom::Current(payload_size as i64))?;
                }
            }
        }
    }
}

/// Read exactly `buf.len()` bytes. Returns `Ok(false)` on clean EOF at the start.
fn read_exact_or_eof<R: Read>(reader: &mut R, buf: &mut [u8]) -> std::io::Result<bool> {
    let mut filled = 0;
    while filled < buf.len() {
        match reader.read(&mut buf[filled..])? {
            0 => {
                if filled == 0 {
                    return Ok(false);
                }
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "truncated PGS segment header",
                ));
            }
            n => filled += n,
        }
    }
    Ok(true)
}

/// Find the last PTS value in a block of SUP data by scanning for PG magic headers.
fn find_last_sup_pts(data: &[u8]) -> Option<u64> {
    let mut last_pts = None;
    let mut i = 0;
    while i + HEADER_SIZE <= data.len() {
        if data[i] == PGS_MAGIC[0] && data[i + 1] == PGS_MAGIC[1] {
            let pts = u32::from_be_bytes([data[i + 2], data[i + 3], data[i + 4], data[i + 5]]);
            last_pts = Some(pts as u64);
            // Skip past this header + payload to next segment.
            let payload_len =
                u16::from_be_bytes([data[i + 11], data[i + 12]]) as usize;
            i += HEADER_SIZE + payload_len;
        } else {
            i += 1;
        }
    }
    last_pts
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
    fn count_display_sets_content_and_clear() {
        // Content DS: PCS with 1 composition object, then END.
        let content_pcs = PgsSegment {
            pts: 90000,
            dts: 0,
            segment_type: SegmentType::PresentationComposition,
            payload: vec![
                0x07, 0x80, 0x04, 0x38, 0x10, 0x00, 0x01, 0x80, 0x00, 0x00,
                0x01, // num_composition_objects: 1
                // One composition object (8 bytes, no crop):
                0x00, 0x00, // object_id
                0x00, // window_id
                0x00, // flags (no crop)
                0x00, 0x10, // x
                0x00, 0x20, // y
            ],
        };
        let content_end = PgsSegment {
            pts: 90000,
            dts: 0,
            segment_type: SegmentType::EndOfDisplaySet,
            payload: Vec::new(),
        };
        // Clear DS: PCS with 0 composition objects, then END.
        let clear_pcs = PgsSegment {
            pts: 180000,
            dts: 0,
            segment_type: SegmentType::PresentationComposition,
            payload: vec![
                0x07, 0x80, 0x04, 0x38, 0x10, 0x00, 0x02, 0x00, 0x00, 0x00,
                0x00, // num_composition_objects: 0
            ],
        };
        let clear_end = PgsSegment {
            pts: 180000,
            dts: 0,
            segment_type: SegmentType::EndOfDisplaySet,
            payload: Vec::new(),
        };

        let mut data = content_pcs.to_bytes();
        data.extend_from_slice(&content_end.to_bytes());
        data.extend_from_slice(&clear_pcs.to_bytes());
        data.extend_from_slice(&clear_end.to_bytes());

        let path = temp_path("counts.sup");
        std::fs::write(&path, &data).expect("write");

        let counts = count_display_sets(&path).expect("count");
        assert_eq!(counts.total, 2);
        assert_eq!(counts.content, 1);
        assert_eq!(counts.clear, 1);
        assert_eq!(counts.total, counts.content + counts.clear);

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
