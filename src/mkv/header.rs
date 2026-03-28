use crate::ebml::{ids, read_element_id, read_element_size};
use crate::error::PgsError;
use crate::io::SeekBufReader;
use std::io::{Read, Seek};

/// Positions of key elements within the Segment, discovered from SeekHead.
#[derive(Debug, Default)]
pub struct SegmentLayout {
    /// Absolute byte position of the Segment's data start (after ID + size).
    pub segment_data_start: u64,
    /// Absolute byte position of the Tracks element, if found.
    pub tracks_position: Option<u64>,
    /// Absolute byte position of the Cues element, if found.
    pub cues_position: Option<u64>,
    /// Absolute byte position of the Info element, if found.
    pub info_position: Option<u64>,
    /// Absolute byte position of the Tags element, if found.
    pub tags_position: Option<u64>,
    /// Absolute byte position of the first Cluster, if found.
    pub first_cluster_position: Option<u64>,
    /// Absolute byte position of the end of the Segment's data.
    pub segment_data_end: u64,
}

/// Validate EBML header and return the DocType.
pub fn parse_ebml_header<R: Read + Seek>(
    reader: &mut SeekBufReader<R>,
) -> Result<String, PgsError> {
    let id = read_element_id(reader)?;
    if id.value != ids::EBML {
        return Err(PgsError::InvalidEbml(format!(
            "expected EBML header (0x{:X}), got 0x{:X}",
            ids::EBML,
            id.value
        )));
    }

    let size = read_element_size(reader)?;
    let header_end = reader.position() + size.value;

    let mut doc_type = String::new();

    while reader.position() < header_end {
        let child_id = read_element_id(reader)?;
        let child_size = read_element_size(reader)?;

        if child_id.value == ids::DOC_TYPE {
            doc_type = reader.read_string(child_size.value as usize)?;
        } else {
            reader.skip(child_size.value)?;
        }
    }

    if doc_type.is_empty() {
        return Err(PgsError::InvalidEbml("missing DocType".into()));
    }

    if doc_type != "matroska" && doc_type != "webm" {
        return Err(PgsError::InvalidEbml(format!(
            "unsupported DocType: {doc_type}"
        )));
    }

    Ok(doc_type)
}

/// Parse the Segment element header and discover the layout via SeekHead.
///
/// After this call, the reader is positioned somewhere within the Segment.
/// Use the returned `SegmentLayout` to seek to specific elements.
pub fn parse_segment<R: Read + Seek>(
    reader: &mut SeekBufReader<R>,
) -> Result<SegmentLayout, PgsError> {
    let id = read_element_id(reader)?;
    if id.value != ids::SEGMENT {
        return Err(PgsError::InvalidMkv(format!(
            "expected Segment (0x{:X}), got 0x{:X}",
            ids::SEGMENT,
            id.value
        )));
    }

    let size = read_element_size(reader)?;
    let segment_data_start = reader.position();
    let segment_data_end = if size.value == u64::MAX {
        u64::MAX // Unknown size — read until EOF.
    } else {
        segment_data_start + size.value
    };

    let mut layout = SegmentLayout {
        segment_data_start,
        segment_data_end,
        ..Default::default()
    };

    // Scan top-level Segment children to find SeekHead (and possibly other elements).
    // We scan until we find a Cluster (which means we've passed the metadata section)
    // or until we've found a SeekHead.
    while reader.position() < segment_data_end {
        let elem_pos = reader.position();
        let child_id = match read_element_id(reader) {
            Ok(id) => id,
            Err(_) => break, // EOF or corrupted — stop scanning.
        };
        let child_size = match read_element_size(reader) {
            Ok(s) => s,
            Err(_) => break,
        };
        let child_data_start = reader.position();

        match child_id.value {
            ids::SEEK_HEAD => {
                parse_seekhead(reader, child_data_start, child_size.value, &mut layout)?;
            }
            ids::INFO => {
                if layout.info_position.is_none() {
                    layout.info_position = Some(elem_pos);
                }
                reader.skip(child_size.value)?;
            }
            ids::TRACKS => {
                if layout.tracks_position.is_none() {
                    layout.tracks_position = Some(elem_pos);
                }
                reader.skip(child_size.value)?;
            }
            ids::CUES => {
                if layout.cues_position.is_none() {
                    layout.cues_position = Some(elem_pos);
                }
                reader.skip(child_size.value)?;
            }
            ids::TAGS => {
                if layout.tags_position.is_none() {
                    layout.tags_position = Some(elem_pos);
                }
                reader.skip(child_size.value)?;
            }
            ids::CLUSTER => {
                if layout.first_cluster_position.is_none() {
                    layout.first_cluster_position = Some(elem_pos);
                }
                // Don't scan into clusters during header parsing.
                break;
            }
            _ => {
                // Skip unknown elements.
                if child_size.value != u64::MAX {
                    reader.skip(child_size.value)?;
                } else {
                    break;
                }
            }
        }
    }

    // If SeekHead gave us positions, convert them to absolute positions.
    // SeekHead positions are relative to segment_data_start.
    // (Already handled in parse_seekhead)

    Ok(layout)
}

fn parse_seekhead<R: Read + Seek>(
    reader: &mut SeekBufReader<R>,
    data_start: u64,
    data_size: u64,
    layout: &mut SegmentLayout,
) -> Result<(), PgsError> {
    let end = data_start + data_size;

    while reader.position() < end {
        let child_id = read_element_id(reader)?;
        let child_size = read_element_size(reader)?;

        if child_id.value == ids::SEEK {
            parse_seek_entry(reader, reader.position(), child_size.value, layout)?;
        } else {
            reader.skip(child_size.value)?;
        }
    }

    Ok(())
}

fn parse_seek_entry<R: Read + Seek>(
    reader: &mut SeekBufReader<R>,
    data_start: u64,
    data_size: u64,
    layout: &mut SegmentLayout,
) -> Result<(), PgsError> {
    let end = data_start + data_size;
    let mut seek_id: Option<u64> = None;
    let mut seek_position: Option<u64> = None;

    while reader.position() < end {
        let child_id = read_element_id(reader)?;
        let child_size = read_element_size(reader)?;

        match child_id.value {
            ids::SEEK_ID => {
                // SeekID is an EBML element ID stored as binary data.
                // Read the raw bytes as a big-endian integer.
                seek_id = Some(reader.read_uint_be(child_size.value as usize)?);
            }
            ids::SEEK_POSITION => {
                seek_position = Some(reader.read_uint_be(child_size.value as usize)?);
            }
            _ => {
                reader.skip(child_size.value)?;
            }
        }
    }

    if let (Some(id), Some(pos)) = (seek_id, seek_position) {
        // SeekPosition is relative to the Segment data start.
        let abs_pos = layout.segment_data_start + pos;
        match id {
            ids::TRACKS => {
                if layout.tracks_position.is_none() {
                    layout.tracks_position = Some(abs_pos);
                }
            }
            ids::CUES => {
                if layout.cues_position.is_none() {
                    layout.cues_position = Some(abs_pos);
                }
            }
            ids::INFO => {
                if layout.info_position.is_none() {
                    layout.info_position = Some(abs_pos);
                }
            }
            ids::CLUSTER => {
                if layout.first_cluster_position.is_none() {
                    layout.first_cluster_position = Some(abs_pos);
                }
            }
            ids::TAGS => {
                if layout.tags_position.is_none() {
                    layout.tags_position = Some(abs_pos);
                }
            }
            _ => {} // We don't need Chapters or Attachments positions.
        }
    }

    Ok(())
}

/// Parse the Info element to get TimestampScale.
/// Returns the timestamp scale in nanoseconds (default 1,000,000 = 1ms).
/// Parsed fields from the Segment/Info element.
pub struct SegmentInfo {
    /// Timestamp scale in nanoseconds per MKV clock tick (default: 1,000,000 = 1ms).
    pub timestamp_scale: u64,
    /// Duration of the segment in timestamp-scale units, if present.
    pub duration: Option<f64>,
}

pub fn parse_info<R: Read + Seek>(
    reader: &mut SeekBufReader<R>,
    info_position: u64,
) -> Result<SegmentInfo, PgsError> {
    reader.seek_to(info_position)?;

    let id = read_element_id(reader)?;
    if id.value != ids::INFO {
        return Err(PgsError::InvalidMkv("expected Info element".into()));
    }
    let size = read_element_size(reader)?;
    let end = reader.position() + size.value;

    let mut timestamp_scale: u64 = 1_000_000; // Default: 1ms
    let mut duration: Option<f64> = None;

    while reader.position() < end {
        let child_id = read_element_id(reader)?;
        let child_size = read_element_size(reader)?;

        match child_id.value {
            ids::TIMESTAMP_SCALE => {
                timestamp_scale = reader.read_uint_be(child_size.value as usize)?;
            }
            ids::DURATION => {
                // Duration is an EBML float (4 or 8 bytes).
                if child_size.value == 4 {
                    let bytes = reader.read_bytes(4)?;
                    let val = f32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
                    duration = Some(val as f64);
                } else if child_size.value == 8 {
                    let bytes = reader.read_bytes(8)?;
                    let val = f64::from_be_bytes([
                        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6],
                        bytes[7],
                    ]);
                    duration = Some(val);
                } else {
                    reader.skip(child_size.value)?;
                }
            }
            _ => {
                reader.skip(child_size.value)?;
            }
        }
    }

    Ok(SegmentInfo {
        timestamp_scale,
        duration,
    })
}
