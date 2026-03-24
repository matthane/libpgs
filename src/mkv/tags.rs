use crate::ebml::{ids, read_element_id, read_element_size};
use crate::error::PgsError;
use crate::io::SeekBufReader;
use std::collections::HashMap;
use std::io::{Read, Seek};

/// Parse the Tags element and extract NUMBER_OF_FRAMES per TrackUID.
///
/// Returns a map from TrackUID to frame count for tracks in `target_uids`.
/// Skips malformed tags gracefully — missing or unparseable values are ignored.
pub(crate) fn parse_tags_frame_counts<R: Read + Seek>(
    reader: &mut SeekBufReader<R>,
    tags_position: u64,
    target_uids: &[u64],
) -> Result<HashMap<u64, u64>, PgsError> {
    let mut counts = HashMap::new();

    reader.seek_to(tags_position)?;

    let id = read_element_id(reader)?;
    if id.value != ids::TAGS {
        return Ok(counts);
    }
    let size = read_element_size(reader)?;
    let end = reader.position() + size.value;

    while reader.position() < end {
        let child_id = read_element_id(reader)?;
        let child_size = read_element_size(reader)?;

        if child_id.value == ids::TAG {
            parse_tag(
                reader,
                reader.position(),
                child_size.value,
                target_uids,
                &mut counts,
            )?;
        } else {
            reader.skip(child_size.value)?;
        }
    }

    Ok(counts)
}

/// Parse a single Tag element, extracting frame count if it targets one of our tracks.
fn parse_tag<R: Read + Seek>(
    reader: &mut SeekBufReader<R>,
    data_start: u64,
    data_size: u64,
    target_uids: &[u64],
    counts: &mut HashMap<u64, u64>,
) -> Result<(), PgsError> {
    let end = data_start + data_size;

    let mut track_uids: Vec<u64> = Vec::new();
    let mut frame_count: Option<u64> = None;

    while reader.position() < end {
        let child_id = read_element_id(reader)?;
        let child_size = read_element_size(reader)?;

        match child_id.value {
            ids::TARGETS => {
                parse_targets(reader, reader.position(), child_size.value, &mut track_uids)?;
            }
            ids::SIMPLE_TAG => {
                if let Some(count) =
                    parse_simple_tag_frame_count(reader, reader.position(), child_size.value)?
                {
                    frame_count = Some(count);
                }
            }
            _ => {
                reader.skip(child_size.value)?;
            }
        }
    }

    if let Some(count) = frame_count {
        for uid in &track_uids {
            if target_uids.contains(uid) {
                counts.insert(*uid, count);
            }
        }
    }

    Ok(())
}

/// Parse Targets to collect TagTrackUID values.
fn parse_targets<R: Read + Seek>(
    reader: &mut SeekBufReader<R>,
    data_start: u64,
    data_size: u64,
    track_uids: &mut Vec<u64>,
) -> Result<(), PgsError> {
    let end = data_start + data_size;

    while reader.position() < end {
        let child_id = read_element_id(reader)?;
        let child_size = read_element_size(reader)?;

        if child_id.value == ids::TAG_TRACK_UID {
            track_uids.push(reader.read_uint_be(child_size.value as usize)?);
        } else {
            reader.skip(child_size.value)?;
        }
    }

    Ok(())
}

/// Parse a SimpleTag looking for TagName="NUMBER_OF_FRAMES" and return the value.
fn parse_simple_tag_frame_count<R: Read + Seek>(
    reader: &mut SeekBufReader<R>,
    data_start: u64,
    data_size: u64,
) -> Result<Option<u64>, PgsError> {
    let end = data_start + data_size;

    let mut tag_name: Option<String> = None;
    let mut tag_string: Option<String> = None;

    while reader.position() < end {
        let child_id = read_element_id(reader)?;
        let child_size = read_element_size(reader)?;

        match child_id.value {
            ids::TAG_NAME => {
                tag_name = Some(reader.read_string(child_size.value as usize)?);
            }
            ids::TAG_STRING => {
                tag_string = Some(reader.read_string(child_size.value as usize)?);
            }
            _ => {
                reader.skip(child_size.value)?;
            }
        }
    }

    if tag_name.as_deref() == Some("NUMBER_OF_FRAMES")
        && let Some(s) = tag_string
        && let Ok(count) = s.trim().parse::<u64>()
    {
        return Ok(Some(count));
    }

    Ok(None)
}
