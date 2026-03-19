use crate::error::PgsError;
use crate::io::SeekBufReader;
use crate::m2ts::ts_packet::{PacketFormat, SYNC_BYTE};
use std::collections::BTreeSet;
use std::io::{Read, Seek};

/// Result of the sparse probing pass.
pub struct ProbeResult {
    /// Active regions where PGS data was detected: (start_offset, end_offset).
    pub active_regions: Vec<(u64, u64)>,
}

/// Probe parameters computed from file size.
struct ProbeParams {
    chunk_size: u64,
    num_probes: usize,
    sub_probe_bytes: u64,
}

/// Compute adaptive probe parameters based on file size.
///
/// PGS density in transport streams varies dramatically with video bitrate:
/// - HD (~20 Mbps): PGS is ~0.1–0.2% of packets
/// - UHD (~80 Mbps): PGS is ~0.01–0.03% of packets
///
/// Larger files typically have higher video bitrates, requiring bigger probe
/// windows to reliably detect the sparser PGS packets.
fn probe_params(file_size: u64) -> ProbeParams {
    if file_size > 5_000_000_000 {
        // >5 GB: likely UHD. PGS extremely sparse (~0.025% of packets).
        // 16 MB chunks, 2 sub-probes of 2 MB each (25% per-chunk coverage).
        // At 0.025% density: ~99.6% per-chunk detection.
        ProbeParams {
            chunk_size: 16 * 1024 * 1024,
            num_probes: 2,
            sub_probe_bytes: 2 * 1024 * 1024,
        }
    } else {
        // ≤5 GB: likely HD. PGS moderately sparse (~0.2% of packets).
        // 2 MB chunks, 4 sub-probes of 64 KB each (12.5% per-chunk coverage).
        // At 0.2% density: ~93% per-chunk detection.
        ProbeParams {
            chunk_size: 2 * 1024 * 1024,
            num_probes: 4,
            sub_probe_bytes: 64 * 1024,
        }
    }
}

/// Probe the file to find regions containing PGS data.
///
/// Takes multiple sub-probes at evenly-spaced positions within each chunk,
/// scanning packet headers for PGS PIDs. Uses adaptive parameters based on
/// file size — larger files get bigger probe windows to handle sparser PGS.
/// Returns merged active regions with ±1 chunk expansion to capture PES
/// boundaries.
pub fn probe_for_pgs<R: Read + Seek>(
    reader: &mut SeekBufReader<R>,
    format: PacketFormat,
    pgs_pids: &[u16],
    file_size: u64,
) -> Result<ProbeResult, PgsError> {
    let params = probe_params(file_size);
    let num_chunks = (file_size + params.chunk_size - 1) / params.chunk_size;
    let mut active_chunks = Vec::new();

    for chunk_idx in 0..num_chunks {
        let chunk_start = chunk_idx * params.chunk_size;
        let chunk_end = ((chunk_idx + 1) * params.chunk_size).min(file_size);
        let chunk_len = chunk_end - chunk_start;

        let mut found = false;
        for probe_idx in 0..params.num_probes {
            // Evenly distribute probes at 0/N, 1/N, ... (N-1)/N of the chunk.
            let probe_offset =
                chunk_start + (chunk_len * probe_idx as u64) / params.num_probes as u64;
            let max_probe = chunk_end.saturating_sub(probe_offset);
            let probe_len = params.sub_probe_bytes.min(max_probe) as usize;
            if probe_len == 0 {
                continue;
            }

            reader.seek_to(probe_offset)?;
            let probe_data = reader.read_bytes(probe_len)?;

            if scan_probe_for_pgs(&probe_data, format, pgs_pids) {
                found = true;
                break;
            }
        }

        if found {
            active_chunks.push(chunk_idx);
        }
    }

    let regions = merge_active_chunks(&active_chunks, num_chunks, file_size, params.chunk_size);
    Ok(ProbeResult { active_regions: regions })
}

/// Scan a probe buffer for any packets containing PGS PIDs.
fn scan_probe_for_pgs(data: &[u8], format: PacketFormat, pgs_pids: &[u16]) -> bool {
    let packet_size = format.packet_size();
    let sync_offset = format.sync_offset();

    let Some(start) = find_sync_in_buffer(data, sync_offset, packet_size) else {
        return false;
    };

    let mut offset = start;
    while offset + sync_offset + 4 <= data.len() {
        let ts_pos = offset + sync_offset;
        if data[ts_pos] != SYNC_BYTE {
            // Sync loss in probe buffer — try to re-find sync from here.
            let remaining = &data[offset + 1..];
            let Some(resync) = find_sync_in_buffer(remaining, sync_offset, packet_size) else {
                break;
            };
            offset = offset + 1 + resync;
            continue;
        }

        let pid = ((data[ts_pos + 1] as u16 & 0x1F) << 8) | data[ts_pos + 2] as u16;
        if pgs_pids.contains(&pid) {
            return true;
        }

        offset += packet_size;
    }

    false
}

/// Find the first sync-byte-aligned offset within a raw buffer.
fn find_sync_in_buffer(data: &[u8], sync_offset: usize, packet_size: usize) -> Option<usize> {
    if data.len() < sync_offset + packet_size + 1 {
        return None;
    }

    for start in 0..packet_size {
        let first = start + sync_offset;
        let second = first + packet_size;
        if second < data.len()
            && data[first] == SYNC_BYTE
            && data[second] == SYNC_BYTE
        {
            return Some(start);
        }
    }
    None
}

/// Expand active chunks by ±1 and merge into contiguous regions.
fn merge_active_chunks(
    active: &[u64],
    num_chunks: u64,
    file_size: u64,
    chunk_size: u64,
) -> Vec<(u64, u64)> {
    if active.is_empty() {
        return Vec::new();
    }

    let mut expanded = BTreeSet::new();
    for &idx in active {
        if idx > 0 {
            expanded.insert(idx - 1);
        }
        expanded.insert(idx);
        if idx + 1 < num_chunks {
            expanded.insert(idx + 1);
        }
    }

    let indices: Vec<u64> = expanded.into_iter().collect();
    let mut regions = Vec::new();
    let mut start = indices[0] * chunk_size;
    let mut end = ((indices[0] + 1) * chunk_size).min(file_size);

    for &idx in &indices[1..] {
        let chunk_start = idx * chunk_size;
        let chunk_end = ((idx + 1) * chunk_size).min(file_size);

        if chunk_start <= end {
            end = chunk_end;
        } else {
            regions.push((start, end));
            start = chunk_start;
            end = chunk_end;
        }
    }
    regions.push((start, end));

    regions
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_CHUNK: u64 = 512 * 1024;

    #[test]
    fn test_merge_active_chunks() {
        // Chunks 2 and 8,9 are active (far enough apart to stay separate after ±1 expansion).
        let active = vec![2, 8, 9];
        let regions = merge_active_chunks(&active, 15, 15 * TEST_CHUNK, TEST_CHUNK);
        // Chunk 2 expands to [1,3], chunks 8,9 expand to [7,10]
        assert_eq!(regions.len(), 2);
        assert_eq!(regions[0], (1 * TEST_CHUNK, 4 * TEST_CHUNK));
        assert_eq!(regions[1], (7 * TEST_CHUNK, 11 * TEST_CHUNK));
    }

    #[test]
    fn test_merge_empty() {
        let regions = merge_active_chunks(&[], 10, 10 * TEST_CHUNK, TEST_CHUNK);
        assert!(regions.is_empty());
    }

    #[test]
    fn test_merge_single_chunk() {
        let active = vec![5];
        let regions = merge_active_chunks(&active, 10, 10 * TEST_CHUNK, TEST_CHUNK);
        assert_eq!(regions.len(), 1);
        assert_eq!(regions[0], (4 * TEST_CHUNK, 7 * TEST_CHUNK));
    }

    #[test]
    fn test_merge_at_boundaries() {
        // First chunk.
        let active = vec![0];
        let regions = merge_active_chunks(&active, 5, 5 * TEST_CHUNK, TEST_CHUNK);
        assert_eq!(regions.len(), 1);
        assert_eq!(regions[0], (0, 2 * TEST_CHUNK));

        // Last chunk.
        let active = vec![4];
        let file_size = 5 * TEST_CHUNK;
        let regions = merge_active_chunks(&active, 5, file_size, TEST_CHUNK);
        assert_eq!(regions.len(), 1);
        assert_eq!(regions[0], (3 * TEST_CHUNK, file_size));
    }

    #[test]
    fn test_merge_adjacent_collapse() {
        // Chunks 3 and 5: after expansion [2,4] and [4,6] → merge into [2,6]
        let active = vec![3, 5];
        let regions = merge_active_chunks(&active, 10, 10 * TEST_CHUNK, TEST_CHUNK);
        assert_eq!(regions.len(), 1);
        assert_eq!(regions[0], (2 * TEST_CHUNK, 7 * TEST_CHUNK));
    }
}
