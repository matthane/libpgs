use crate::ebml::{ids, read_element_id, read_element_size};
use crate::error::PgsError;
use crate::io::SeekBufReader;
use crate::mkv::block;
use std::io::{Read, Seek};

/// A raw PGS block extracted from a Cluster, with timing info.
#[derive(Debug)]
pub struct PgsBlock {
    /// MKV track number this block belongs to.
    pub track_number: u64,
    /// Absolute presentation timestamp in Cluster timestamp units.
    /// (cluster_timestamp + block_relative_timestamp)
    pub timestamp: i64,
    /// Raw block payload (PGS segment data, may contain multiple segments).
    pub data: Vec<u8>,
}

/// Scan a single Cluster and extract all PGS blocks for the given tracks.
///
/// The reader should be positioned at the start of the Cluster's data
/// (after the Cluster element ID + size have been consumed).
pub fn scan_cluster_for_pgs<R: Read + Seek>(
    reader: &mut SeekBufReader<R>,
    cluster_data_start: u64,
    cluster_data_size: u64,
    pgs_track_numbers: &[u64],
) -> Result<Vec<PgsBlock>, PgsError> {
    scan_cluster_inner(reader, cluster_data_start, cluster_data_size, pgs_track_numbers, false)
}

/// Scan a single Cluster with fully sequential I/O (no seeks).
///
/// Like `scan_cluster_for_pgs`, but reads through non-PGS data instead of
/// seeking past it. Keeps I/O fully sequential for NAS throughput.
pub fn scan_cluster_for_pgs_sequential<R: Read + Seek>(
    reader: &mut SeekBufReader<R>,
    cluster_data_start: u64,
    cluster_data_size: u64,
    pgs_track_numbers: &[u64],
) -> Result<Vec<PgsBlock>, PgsError> {
    scan_cluster_inner(reader, cluster_data_start, cluster_data_size, pgs_track_numbers, true)
}

fn scan_cluster_inner<R: Read + Seek>(
    reader: &mut SeekBufReader<R>,
    cluster_data_start: u64,
    cluster_data_size: u64,
    pgs_track_numbers: &[u64],
    sequential: bool,
) -> Result<Vec<PgsBlock>, PgsError> {
    if !sequential {
        reader.seek_to(cluster_data_start)?;
    }
    let cluster_end = cluster_data_start + cluster_data_size;

    let mut cluster_timestamp: i64 = 0;
    let mut pgs_blocks = Vec::new();

    while reader.position() < cluster_end {
        let child_id = match read_element_id(reader) {
            Ok(id) => id,
            Err(_) => break,
        };
        let child_size = match read_element_size(reader) {
            Ok(s) => s,
            Err(_) => break,
        };
        let child_data_start = reader.position();

        match child_id.value {
            ids::TIMESTAMP => {
                cluster_timestamp = reader.read_uint_be(child_size.value as usize)? as i64;
            }
            ids::SIMPLE_BLOCK => {
                extract_pgs_from_block(
                    reader,
                    child_size.value,
                    cluster_timestamp,
                    pgs_track_numbers,
                    &mut pgs_blocks,
                    sequential,
                )?;
            }
            ids::BLOCK_GROUP => {
                extract_pgs_from_block_group(
                    reader,
                    child_data_start,
                    child_size.value,
                    cluster_timestamp,
                    pgs_track_numbers,
                    &mut pgs_blocks,
                    sequential,
                )?;
            }
            _ => {
                skip_or_drain(reader, child_size.value, sequential)?;
            }
        }
    }

    Ok(pgs_blocks)
}

/// Skip or drain bytes depending on sequential mode.
#[inline]
fn skip_or_drain<R: Read + Seek>(
    reader: &mut SeekBufReader<R>,
    n: u64,
    sequential: bool,
) -> Result<(), PgsError> {
    if sequential {
        reader.drain(n)?;
    } else {
        reader.skip(n)?;
    }
    Ok(())
}

/// Check a SimpleBlock: read its track number, and if it matches any PGS track, read the payload.
/// Otherwise, skip (or drain) the remaining payload.
fn extract_pgs_from_block<R: Read + Seek>(
    reader: &mut SeekBufReader<R>,
    block_size: u64,
    cluster_timestamp: i64,
    pgs_track_numbers: &[u64],
    pgs_blocks: &mut Vec<PgsBlock>,
    sequential: bool,
) -> Result<(), PgsError> {
    let block_start = reader.position();
    let block_end = block_start + block_size;

    let header = block::read_block_header(reader)?;

    if !pgs_track_numbers.contains(&header.track_number) {
        // Not a PGS track — skip/drain the remaining payload.
        let remaining = block_end - reader.position();
        skip_or_drain(reader, remaining, sequential)?;
        return Ok(());
    }

    // This is a PGS block! Read the payload.
    let payload_size = (block_end - reader.position()) as usize;
    let data = reader.read_bytes(payload_size)?;

    pgs_blocks.push(PgsBlock {
        track_number: header.track_number,
        timestamp: cluster_timestamp + header.relative_timestamp as i64,
        data,
    });

    Ok(())
}

/// Parse a BlockGroup, look for the Block child, and extract PGS data if it matches any track.
fn extract_pgs_from_block_group<R: Read + Seek>(
    reader: &mut SeekBufReader<R>,
    data_start: u64,
    data_size: u64,
    cluster_timestamp: i64,
    pgs_track_numbers: &[u64],
    pgs_blocks: &mut Vec<PgsBlock>,
    sequential: bool,
) -> Result<(), PgsError> {
    let end = data_start + data_size;

    while reader.position() < end {
        let child_id = match read_element_id(reader) {
            Ok(id) => id,
            Err(_) => break,
        };
        let child_size = match read_element_size(reader) {
            Ok(s) => s,
            Err(_) => break,
        };

        if child_id.value == ids::BLOCK {
            extract_pgs_from_block(
                reader,
                child_size.value,
                cluster_timestamp,
                pgs_track_numbers,
                pgs_blocks,
                sequential,
            )?;
        } else {
            skip_or_drain(reader, child_size.value, sequential)?;
        }
    }

    Ok(())
}

/// Read a single PGS block at a known position within a Cluster.
///
/// `block_position` is the absolute byte position of the SimpleBlock or BlockGroup element.
/// `cue_time` is the absolute timestamp from the CuePoint (cluster timestamp units).
/// Returns the PGS block if the block belongs to one of the target tracks, or None.
pub fn read_block_at_position<R: Read + Seek>(
    reader: &mut SeekBufReader<R>,
    block_position: u64,
    cue_time: u64,
    pgs_track_numbers: &[u64],
) -> Result<Option<PgsBlock>, PgsError> {
    reader.seek_to(block_position)?;

    let elem_id = match read_element_id(reader) {
        Ok(id) => id,
        Err(_) => return Ok(None),
    };
    let elem_size = match read_element_size(reader) {
        Ok(s) => s,
        Err(_) => return Ok(None),
    };

    match elem_id.value {
        ids::SIMPLE_BLOCK | ids::BLOCK => {
            let block_start = reader.position();
            let block_end = block_start + elem_size.value;

            let header = block::read_block_header(reader)?;
            if !pgs_track_numbers.contains(&header.track_number) {
                return Ok(None);
            }

            let payload_size = (block_end - reader.position()) as usize;
            let data = reader.read_bytes(payload_size)?;

            Ok(Some(PgsBlock {
                track_number: header.track_number,
                timestamp: cue_time as i64 + header.relative_timestamp as i64,
                data,
            }))
        }
        ids::BLOCK_GROUP => {
            // Enter BlockGroup, find the Block child.
            let bg_end = reader.position() + elem_size.value;
            while reader.position() < bg_end {
                let child_id = match read_element_id(reader) {
                    Ok(id) => id,
                    Err(_) => break,
                };
                let child_size = match read_element_size(reader) {
                    Ok(s) => s,
                    Err(_) => break,
                };

                if child_id.value == ids::BLOCK {
                    let block_start = reader.position();
                    let block_end = block_start + child_size.value;

                    let header = block::read_block_header(reader)?;
                    if !pgs_track_numbers.contains(&header.track_number) {
                        return Ok(None);
                    }

                    let payload_size = (block_end - reader.position()) as usize;
                    let data = reader.read_bytes(payload_size)?;

                    return Ok(Some(PgsBlock {
                        track_number: header.track_number,
                        timestamp: cue_time as i64 + header.relative_timestamp as i64,
                        data,
                    }));
                } else {
                    reader.skip(child_size.value)?;
                }
            }
            Ok(None)
        }
        _ => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    // --- Binary builder helpers ---

    /// Write an EBML element: ID (1-4 bytes depending on value) + VINT size + data.
    fn write_ebml_element(buf: &mut Vec<u8>, id: u32, data: &[u8]) {
        // Write element ID.
        if id <= 0xFF {
            buf.push(id as u8);
        } else if id <= 0xFFFF {
            buf.push((id >> 8) as u8);
            buf.push(id as u8);
        } else if id <= 0xFF_FFFF {
            buf.push((id >> 16) as u8);
            buf.push((id >> 8) as u8);
            buf.push(id as u8);
        } else {
            buf.push((id >> 24) as u8);
            buf.push((id >> 16) as u8);
            buf.push((id >> 8) as u8);
            buf.push(id as u8);
        }
        // Write VINT size (1-byte for sizes < 127, 2-byte otherwise).
        if data.len() < 127 {
            buf.push(0x80 | data.len() as u8);
        } else {
            let len = data.len() as u16;
            buf.push(0x40 | (len >> 8) as u8);
            buf.push(len as u8);
        }
        buf.extend_from_slice(data);
    }

    /// Build a SimpleBlock body: VINT track number + relative timestamp + flags + payload.
    fn build_simple_block_body(track: u8, relative_ts: i16, payload: &[u8]) -> Vec<u8> {
        let mut body = Vec::new();
        body.push(0x80 | track); // 1-byte VINT track number
        body.extend_from_slice(&relative_ts.to_be_bytes());
        body.push(0x80); // flags: keyframe
        body.extend_from_slice(payload);
        body
    }

    /// Build a PGS PCS segment in .sup format (13-byte header + 11-byte payload = 24 bytes).
    fn build_pgs_pcs(pts: u32) -> Vec<u8> {
        let mut seg = Vec::new();
        seg.extend_from_slice(&[0x50, 0x47]); // PG magic
        seg.extend_from_slice(&pts.to_be_bytes()); // PTS
        seg.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]); // DTS = 0
        seg.push(0x16); // PCS type
        seg.extend_from_slice(&[0x00, 0x0B]); // payload size = 11
        seg.extend_from_slice(&[
            0x07, 0x80, // width: 1920
            0x04, 0x38, // height: 1080
            0x10,       // frame rate
            0x00, 0x01, // composition number
            0x80,       // composition state: Epoch Start
            0x00,       // palette update: false
            0x00,       // palette id
            0x00,       // num composition objects: 0
        ]);
        seg
    }

    /// Build a PGS END segment in .sup format (13 bytes, no payload).
    fn build_pgs_end(pts: u32) -> Vec<u8> {
        let mut seg = Vec::new();
        seg.extend_from_slice(&[0x50, 0x47]); // PG magic
        seg.extend_from_slice(&pts.to_be_bytes()); // PTS
        seg.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]); // DTS = 0
        seg.push(0x80); // END type
        seg.extend_from_slice(&[0x00, 0x00]); // payload size = 0
        seg
    }

    /// Build a synthetic Cluster's inner data (Timestamp + SimpleBlocks).
    fn build_multi_track_cluster() -> Vec<u8> {
        let mut data = Vec::new();

        // Timestamp element: value = 1000 (2 bytes big-endian)
        write_ebml_element(&mut data, ids::TIMESTAMP as u32, &1000u16.to_be_bytes());

        // Track 3: PGS PCS segment
        let pcs_3 = build_pgs_pcs(90000);
        let block_3_pcs = build_simple_block_body(3, 0, &pcs_3);
        write_ebml_element(&mut data, ids::SIMPLE_BLOCK as u32, &block_3_pcs);

        // Track 1: non-PGS data (video)
        let video = vec![0xAA; 50];
        let block_1 = build_simple_block_body(1, 0, &video);
        write_ebml_element(&mut data, ids::SIMPLE_BLOCK as u32, &block_1);

        // Track 5: PGS PCS segment
        let pcs_5 = build_pgs_pcs(90000);
        let block_5_pcs = build_simple_block_body(5, 0, &pcs_5);
        write_ebml_element(&mut data, ids::SIMPLE_BLOCK as u32, &block_5_pcs);

        // Track 3: PGS END segment
        let end_3 = build_pgs_end(90000);
        let block_3_end = build_simple_block_body(3, 10, &end_3);
        write_ebml_element(&mut data, ids::SIMPLE_BLOCK as u32, &block_3_end);

        // Track 5: PGS END segment
        let end_5 = build_pgs_end(90000);
        let block_5_end = build_simple_block_body(5, 10, &end_5);
        write_ebml_element(&mut data, ids::SIMPLE_BLOCK as u32, &block_5_end);

        data
    }

    #[test]
    fn test_multi_track_scan() {
        let data = build_multi_track_cluster();
        let data_len = data.len() as u64;
        let mut reader = SeekBufReader::new(Cursor::new(data));

        // Scan for tracks 3 and 5 — should get 4 blocks, skipping track 1.
        let blocks = scan_cluster_for_pgs(&mut reader, 0, data_len, &[3, 5]).unwrap();

        assert_eq!(blocks.len(), 4, "expected 4 PGS blocks (2 per track)");
        assert_eq!(blocks[0].track_number, 3);
        assert_eq!(blocks[1].track_number, 5);
        assert_eq!(blocks[2].track_number, 3);
        assert_eq!(blocks[3].track_number, 5);

        // Timestamps: cluster_ts(1000) + relative_ts
        assert_eq!(blocks[0].timestamp, 1000);
        assert_eq!(blocks[1].timestamp, 1000);
        assert_eq!(blocks[2].timestamp, 1010);
        assert_eq!(blocks[3].timestamp, 1010);
    }

    #[test]
    fn test_multi_track_scan_single_track_filter() {
        let data = build_multi_track_cluster();
        let data_len = data.len() as u64;
        let mut reader = SeekBufReader::new(Cursor::new(data));

        // Scan for only track 5 — should get 2 blocks.
        let blocks = scan_cluster_for_pgs(&mut reader, 0, data_len, &[5]).unwrap();

        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].track_number, 5);
        assert_eq!(blocks[1].track_number, 5);
    }

    #[test]
    fn test_multi_track_scan_no_match() {
        let data = build_multi_track_cluster();
        let data_len = data.len() as u64;
        let mut reader = SeekBufReader::new(Cursor::new(data));

        // Scan for track 99 — should get 0 blocks.
        let blocks = scan_cluster_for_pgs(&mut reader, 0, data_len, &[99]).unwrap();
        assert!(blocks.is_empty());
    }

}
