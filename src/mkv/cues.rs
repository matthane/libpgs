use crate::ebml::{ids, read_element_id, read_element_size};
use crate::error::PgsError;
use crate::io::SeekBufReader;
use std::io::{Read, Seek};

/// A Cue entry pointing to a Cluster containing data for our PGS track.
#[derive(Debug, Clone)]
pub struct PgsCuePoint {
    /// Timestamp of the cue (in Cluster timestamp units).
    pub time: u64,
    /// Absolute byte position of the Cluster.
    pub cluster_position: u64,
    /// Relative byte position of the Block within the Cluster, if available.
    pub relative_position: Option<u64>,
}

/// Parse the Cues element and return CuePoints that reference any of the given PGS tracks.
///
/// Returns an empty Vec if the Cues element has no entries for these tracks
/// (common when only video keyframes are indexed).
pub fn parse_cues_for_tracks<R: Read + Seek>(
    reader: &mut SeekBufReader<R>,
    cues_position: u64,
    segment_data_start: u64,
    pgs_track_numbers: &[u64],
) -> Result<Vec<PgsCuePoint>, PgsError> {
    reader.seek_to(cues_position)?;

    let id = read_element_id(reader)?;
    if id.value != ids::CUES {
        return Err(PgsError::InvalidMkv("expected Cues element".into()));
    }
    let size = read_element_size(reader)?;
    let end = reader.position() + size.value;

    let mut cue_points = Vec::new();

    while reader.position() < end {
        let child_id = read_element_id(reader)?;
        let child_size = read_element_size(reader)?;

        if child_id.value == ids::CUE_POINT {
            if let Some(cp) = parse_cue_point(
                reader,
                reader.position(),
                child_size.value,
                segment_data_start,
                pgs_track_numbers,
            )? {
                cue_points.push(cp);
            }
        } else {
            reader.skip(child_size.value)?;
        }
    }

    Ok(cue_points)
}

fn parse_cue_point<R: Read + Seek>(
    reader: &mut SeekBufReader<R>,
    data_start: u64,
    data_size: u64,
    segment_data_start: u64,
    pgs_track_numbers: &[u64],
) -> Result<Option<PgsCuePoint>, PgsError> {
    let end = data_start + data_size;

    let mut cue_time: u64 = 0;
    let mut matched_cluster_pos: Option<u64> = None;
    let mut matched_relative_pos: Option<u64> = None;

    while reader.position() < end {
        let child_id = read_element_id(reader)?;
        let child_size = read_element_size(reader)?;

        match child_id.value {
            ids::CUE_TIME => {
                cue_time = reader.read_uint_be(child_size.value as usize)?;
            }
            ids::CUE_TRACK_POSITIONS => {
                let tp_end = reader.position() + child_size.value;
                let mut track: u64 = 0;
                let mut cluster_pos: u64 = 0;
                let mut relative_pos: Option<u64> = None;

                while reader.position() < tp_end {
                    let tp_id = read_element_id(reader)?;
                    let tp_size = read_element_size(reader)?;

                    match tp_id.value {
                        ids::CUE_TRACK => {
                            track = reader.read_uint_be(tp_size.value as usize)?;
                        }
                        ids::CUE_CLUSTER_POSITION => {
                            cluster_pos = reader.read_uint_be(tp_size.value as usize)?;
                        }
                        ids::CUE_RELATIVE_POSITION => {
                            relative_pos = Some(reader.read_uint_be(tp_size.value as usize)?);
                        }
                        _ => {
                            reader.skip(tp_size.value)?;
                        }
                    }
                }

                if pgs_track_numbers.contains(&track) {
                    matched_cluster_pos = Some(segment_data_start + cluster_pos);
                    matched_relative_pos = relative_pos;
                }
            }
            _ => {
                reader.skip(child_size.value)?;
            }
        }
    }

    if let Some(cluster_pos) = matched_cluster_pos {
        Ok(Some(PgsCuePoint {
            time: cue_time,
            cluster_position: cluster_pos,
            relative_position: matched_relative_pos,
        }))
    } else {
        Ok(None)
    }
}
