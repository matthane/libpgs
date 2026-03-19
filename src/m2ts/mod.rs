pub mod ts_packet;
pub mod pat;
pub mod pmt;
pub mod pes;
pub mod probe;
pub mod stream;

use crate::error::PgsError;
use crate::io::SeekBufReader;
use std::io::{Read, Seek};
use ts_packet::PacketFormat;

/// A PGS track found in an M2TS/TS file.
#[derive(Debug, Clone)]
pub struct M2tsPgsTrack {
    pub pid: u16,
    pub language: Option<String>,
}

/// Parsed M2TS metadata needed for PGS extraction.
pub(crate) struct M2tsMetadata {
    pub format: PacketFormat,
    pub tracks: Vec<M2tsPgsTrack>,
    pub pgs_pids: Vec<u16>,
    pub file_size: u64,
}

/// Parse all M2TS metadata needed for PGS extraction.
///
/// Discovers packet format, PGS tracks via PAT/PMT, and file size.
pub(crate) fn prepare_m2ts_metadata<R: Read + Seek>(
    reader: &mut SeekBufReader<R>,
) -> Result<M2tsMetadata, PgsError> {
    let format = ts_packet::detect_packet_format(reader)?;
    let tracks = discover_pgs_tracks(reader, format)?;

    if tracks.is_empty() {
        return Err(PgsError::NoPgsTracks);
    }

    let pgs_pids: Vec<u16> = tracks.iter().map(|t| t.pid).collect();
    let file_size = reader.file_size()?;

    Ok(M2tsMetadata {
        format,
        tracks,
        pgs_pids,
        file_size,
    })
}

/// List all PGS tracks in an M2TS/TS file.
pub fn list_pgs_tracks_m2ts<R: Read + Seek>(
    reader: &mut SeekBufReader<R>,
) -> Result<Vec<M2tsPgsTrack>, PgsError> {
    let format = ts_packet::detect_packet_format(reader)?;
    discover_pgs_tracks(reader, format)
}

/// Discover PGS tracks via PAT -> PMT.
pub(crate) fn discover_pgs_tracks<R: Read + Seek>(
    reader: &mut SeekBufReader<R>,
    format: PacketFormat,
) -> Result<Vec<M2tsPgsTrack>, PgsError> {
    let pat_entries = pat::find_pat(reader, format)?;
    let mut pgs_tracks = Vec::new();

    for entry in &pat_entries {
        let streams = pmt::find_pmt(reader, format, entry.pmt_pid)?;
        for stream in pmt::find_pgs_streams(&streams) {
            pgs_tracks.push(M2tsPgsTrack {
                pid: stream.elementary_pid,
                language: stream.language.clone(),
            });
        }
    }

    Ok(pgs_tracks)
}

/// Block size for bulk PID scanning (2 MB).
pub(crate) const SCAN_BLOCK_SIZE: usize = 2 * 1024 * 1024;

/// Maximum bytes to scan when attempting to resync after sync loss.
pub(crate) const MAX_RESYNC_SCAN: u64 = 256 * 1024;

/// Find the first sync-byte-aligned offset within a block.
pub(crate) fn find_sync_start(data: &[u8], sync_offset: usize, packet_size: usize) -> Option<usize> {
    if data.len() < sync_offset + packet_size + 1 {
        return None;
    }
    for start in 0..packet_size {
        let first = start + sync_offset;
        let second = first + packet_size;
        if second < data.len()
            && data[first] == ts_packet::SYNC_BYTE
            && data[second] == ts_packet::SYNC_BYTE
        {
            return Some(start);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ContainerFormat;
    use std::io::Write;

    // --- TS packet builder helpers ---

    /// Build a 188-byte raw TS packet with the given PID, PUSI flag, and PES payload.
    fn build_ts_packet(pid: u16, pusi: bool, cc: u8, payload: &[u8]) -> [u8; 188] {
        let mut pkt = [0u8; 188];
        pkt[0] = 0x47; // sync
        pkt[1] = if pusi { 0x40 } else { 0x00 } | ((pid >> 8) as u8 & 0x1F);
        pkt[2] = (pid & 0xFF) as u8;
        pkt[3] = 0x10 | (cc & 0x0F); // adaptation=01 (payload only)
        let copy_len = payload.len().min(184);
        pkt[4..4 + copy_len].copy_from_slice(&payload[..copy_len]);
        pkt
    }

    /// Build a minimal PES packet containing a PGS PCS segment.
    fn build_pes_pcs(pts_bytes: &[u8; 5]) -> Vec<u8> {
        let mut pes = Vec::new();
        pes.extend_from_slice(&[0x00, 0x00, 0x01]); // PES start code
        pes.push(0xBD); // stream ID: private_stream_1
        pes.extend_from_slice(&[0x00, 0x16]); // PES packet length = 22
        pes.push(0x80); // flags byte 1
        pes.push(0x80); // flags byte 2: PTS present
        pes.push(0x05); // PES header data length = 5
        pes.extend_from_slice(pts_bytes);
        pes.push(0x16); // PCS type
        pes.extend_from_slice(&[0x00, 0x0B]); // PCS payload length = 11
        pes.extend_from_slice(&[
            0x07, 0x80, 0x04, 0x38, 0x10, 0x00, 0x01, 0x80, 0x00, 0x00, 0x00,
        ]);
        pes
    }

    /// Build a minimal PES packet containing a PGS END segment.
    fn build_pes_end(pts_bytes: &[u8; 5]) -> Vec<u8> {
        let mut pes = Vec::new();
        pes.extend_from_slice(&[0x00, 0x00, 0x01]); // PES start code
        pes.push(0xBD);
        pes.extend_from_slice(&[0x00, 0x0B]); // PES packet length = 11
        pes.push(0x80);
        pes.push(0x80);
        pes.push(0x05);
        pes.extend_from_slice(pts_bytes);
        pes.extend_from_slice(&[0x80, 0x00, 0x00]); // END segment
        pes
    }

    /// Build a stream of raw TS packets containing PGS display sets for two PIDs.
    fn build_multi_pid_stream() -> Vec<u8> {
        let pts_90k: [u8; 5] = [0x21, 0x00, 0x05, 0xBF, 0x21]; // PTS=90000
        let pts_180k: [u8; 5] = [0x21, 0x00, 0x0B, 0x7E, 0x41]; // PTS=180000
        let pid_a: u16 = 0x1100;
        let pid_b: u16 = 0x1101;

        let mut data = Vec::new();
        data.extend_from_slice(&build_ts_packet(pid_a, true, 0, &build_pes_pcs(&pts_90k)));
        data.extend_from_slice(&build_ts_packet(pid_b, true, 0, &build_pes_pcs(&pts_180k)));
        data.extend_from_slice(&build_ts_packet(pid_a, true, 1, &build_pes_end(&pts_90k)));
        data.extend_from_slice(&build_ts_packet(pid_b, true, 1, &build_pes_end(&pts_180k)));
        data
    }

    /// Write stream data to a temp file and create an M2tsExtractorState.
    fn make_extractor(
        data: &[u8],
        pids: &[u16],
        track_filter: Option<&[u32]>,
    ) -> stream::M2tsExtractorState {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("libpgs_test_{}.ts", std::process::id()));
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(data).unwrap();
        f.flush().unwrap();
        drop(f);

        let file = std::fs::File::open(&path).unwrap();
        let reader = SeekBufReader::new(file);

        let meta = M2tsMetadata {
            format: PacketFormat::RawTs,
            tracks: pids.iter().map(|&pid| M2tsPgsTrack { pid, language: None }).collect(),
            pgs_pids: pids.to_vec(),
            file_size: data.len() as u64,
        };

        let ext = stream::M2tsExtractorState::new(
            reader, meta, ContainerFormat::TransportStream, track_filter,
        );

        // Clean up temp file (already opened by the reader).
        let _ = std::fs::remove_file(&path);

        ext
    }

    /// Drain all display sets from an extractor.
    fn drain(ext: &mut stream::M2tsExtractorState) -> Vec<crate::TrackDisplaySet> {
        let mut results = Vec::new();
        while let Some(Ok(tds)) = ext.next_display_set() {
            results.push(tds);
        }
        results
    }

    #[test]
    fn test_streaming_multi_pid_extraction() {
        let data = build_multi_pid_stream();
        let mut ext = make_extractor(&data, &[0x1100, 0x1101], None);
        let results = drain(&mut ext);

        assert_eq!(results.len(), 2, "expected display sets from 2 PIDs");

        let ds_a = results.iter().find(|r| r.track_id == 0x1100).unwrap();
        assert_eq!(ds_a.display_set.pts, 90000);

        let ds_b = results.iter().find(|r| r.track_id == 0x1101).unwrap();
        assert_eq!(ds_b.display_set.pts, 180000);
    }

    #[test]
    fn test_streaming_single_pid_filter() {
        let data = build_multi_pid_stream();
        let mut ext = make_extractor(&data, &[0x1100, 0x1101], Some(&[0x1101]));
        let results = drain(&mut ext);

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].track_id, 0x1101);
        assert_eq!(results[0].display_set.pts, 180000);
    }

    #[test]
    fn test_streaming_no_matching_pid() {
        let data = build_multi_pid_stream();
        let mut ext = make_extractor(&data, &[0x9999], None);
        let results = drain(&mut ext);

        assert!(results.is_empty());
    }
}
