pub mod header;
pub mod tracks;
pub mod cues;
pub mod cluster;
pub mod block;
pub mod stream;
pub mod tags;

use crate::error::PgsError;
use crate::io::SeekBufReader;
use crate::pgs::{DisplaySet, DisplaySetAssembler, PgsSegment};
use crate::pgs::segment::PGS_MAGIC;
use tracks::ContentCompAlgo;
use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek};
use std::path::Path;

/// Minimum cue points to justify parallel extraction.
pub(crate) const PARALLEL_CUE_THRESHOLD: usize = 32;

/// Maximum parallel workers for file-based extraction.
pub(crate) const MAX_PARALLEL_WORKERS: usize = 8;

/// Parsed MKV metadata needed for PGS extraction.
pub(crate) struct MkvMetadata {
    pub layout: header::SegmentLayout,
    pub pgs_tracks: Vec<tracks::MkvPgsTrack>,
    pub pgs_track_numbers: Vec<u64>,
    pub compression_map: HashMap<u64, ContentCompAlgo>,
    pub timestamp_scale: u64,
    pub cue_points: Option<Vec<cues::PgsCuePoint>>,
    /// TrackUID → NUMBER_OF_FRAMES from Tags element.
    pub frame_counts: HashMap<u64, u64>,
}

/// Parse all MKV metadata needed for PGS extraction in a single pass.
///
/// Discovers PGS tracks, compression settings, timestamp scale, and cue points.
pub(crate) fn prepare_mkv_metadata<R: Read + Seek>(
    reader: &mut SeekBufReader<R>,
) -> Result<MkvMetadata, PgsError> {
    reader.seek_to(0)?;
    let _doc_type = header::parse_ebml_header(reader)?;
    let layout = header::parse_segment(reader)?;

    let tracks_pos = layout.tracks_position
        .ok_or_else(|| PgsError::InvalidMkv("Tracks element not found".into()))?;
    let pgs_tracks = tracks::parse_tracks(reader, tracks_pos)?;

    if pgs_tracks.is_empty() {
        return Err(PgsError::NoPgsTracks);
    }

    let pgs_track_numbers: Vec<u64> = pgs_tracks.iter().map(|t| t.track_number).collect();

    let mut compression_map: HashMap<u64, ContentCompAlgo> = HashMap::new();
    for t in &pgs_tracks {
        if let Some(comp) = &t.compression {
            compression_map.insert(t.track_number, comp.clone());
        }
    }

    let timestamp_scale = if let Some(info_pos) = layout.info_position {
        header::parse_info(reader, info_pos)?
    } else {
        1_000_000
    };

    let cue_points = if let Some(cues_pos) = layout.cues_position {
        let points = cues::parse_cues_for_tracks(
            reader,
            cues_pos,
            layout.segment_data_start,
            &pgs_track_numbers,
        )?;
        if points.is_empty() { None } else { Some(points) }
    } else {
        None
    };

    let frame_counts = if let Some(tags_pos) = layout.tags_position {
        let target_uids: Vec<u64> = pgs_tracks.iter()
            .filter_map(|t| t.track_uid)
            .collect();
        if target_uids.is_empty() {
            HashMap::new()
        } else {
            tags::parse_tags_frame_counts(reader, tags_pos, &target_uids)
                .unwrap_or_default()
        }
    } else {
        HashMap::new()
    };

    Ok(MkvMetadata {
        layout,
        pgs_tracks,
        pgs_track_numbers,
        compression_map,
        timestamp_scale,
        cue_points,
        frame_counts,
    })
}

/// List all PGS tracks in an MKV file.
pub fn list_pgs_tracks_mkv<R: Read + Seek>(
    reader: &mut SeekBufReader<R>,
) -> Result<Vec<tracks::MkvPgsTrack>, PgsError> {
    reader.seek_to(0)?;
    let _doc_type = header::parse_ebml_header(reader)?;
    let layout = header::parse_segment(reader)?;

    let tracks_pos = layout.tracks_position
        .ok_or_else(|| PgsError::InvalidMkv("Tracks element not found".into()))?;
    tracks::parse_tracks(reader, tracks_pos)
}

/// Per-track assembler state used during extraction.
pub(crate) struct TrackAssemblers {
    /// Map from track number to (assembler, collected display sets).
    pub(crate) tracks: HashMap<u64, (DisplaySetAssembler, Vec<DisplaySet>)>,
    /// Per-track content compression settings.
    pub(crate) compression: HashMap<u64, ContentCompAlgo>,
    /// Ordered list of track numbers (preserves discovery order in output).
    pub(crate) order: Vec<u64>,
    /// MKV TimestampScale in nanoseconds per tick (default 1,000,000 = 1ms).
    pub(crate) timestamp_scale: u64,
}

impl TrackAssemblers {
    pub(crate) fn new(track_numbers: &[u64], compression: &HashMap<u64, ContentCompAlgo>, timestamp_scale: u64) -> Self {
        let mut tracks = HashMap::new();
        let mut order = Vec::new();
        for &tn in track_numbers {
            tracks.insert(tn, (DisplaySetAssembler::new(), Vec::new()));
            order.push(tn);
        }
        Self {
            tracks,
            compression: compression.clone(),
            order,
            timestamp_scale,
        }
    }

    /// Convert an MKV timestamp (in clock ticks) to PGS 90kHz PTS.
    pub(crate) fn mkv_timestamp_to_pts(&self, mkv_ts: i64) -> u64 {
        // time_ns = mkv_ts * timestamp_scale
        // pts_90khz = time_ns * 90_000 / 1_000_000_000 = time_ns * 9 / 100_000
        let time_ns = mkv_ts as i128 * self.timestamp_scale as i128;
        let pts = time_ns * 9 / 100_000;
        pts.max(0) as u64
    }

    pub(crate) fn process_pgs_blocks(&mut self, pgs_blocks: Vec<cluster::PgsBlock>) {
        for pgs_block in pgs_blocks {
            let comp = self.compression.get(&pgs_block.track_number);
            let decoded = decode_block_data(&pgs_block.data, comp);
            let pts = self.mkv_timestamp_to_pts(pgs_block.timestamp);
            if let Some((assembler, display_sets)) = self.tracks.get_mut(&pgs_block.track_number) {
                process_pgs_block(&decoded, pts, assembler, display_sets);
            }
        }
    }

    pub(crate) fn into_results(mut self) -> Vec<(u64, Vec<DisplaySet>)> {
        let mut results = Vec::new();
        for tn in &self.order {
            if let Some((_, display_sets)) = self.tracks.remove(tn)
                && !display_sets.is_empty()
            {
                results.push((*tn, display_sets));
            }
        }
        results
    }
}

/// Decode block data according to the track's content encoding.
pub(crate) fn decode_block_data(data: &[u8], compression: Option<&ContentCompAlgo>) -> Vec<u8> {
    match compression {
        Some(ContentCompAlgo::Zlib) => {
            use flate2::read::ZlibDecoder;
            use std::io::Read as _;
            let mut decoder = ZlibDecoder::new(data);
            let mut decoded = Vec::new();
            if decoder.read_to_end(&mut decoded).is_ok() {
                decoded
            } else {
                data.to_vec()
            }
        }
        Some(ContentCompAlgo::HeaderStripping(prefix)) => {
            let mut decoded = Vec::with_capacity(prefix.len() + data.len());
            decoded.extend_from_slice(prefix);
            decoded.extend_from_slice(data);
            decoded
        }
        None => data.to_vec(),
    }
}

/// Extract PGS blocks from a slice of cue points using direct-seek when
/// `relative_position` is available, falling back to cluster scanning.
pub(crate) fn extract_blocks_for_cues<R: Read + Seek>(
    reader: &mut SeekBufReader<R>,
    cue_points: &[cues::PgsCuePoint],
    pgs_track_numbers: &[u64],
) -> Result<Vec<cluster::PgsBlock>, PgsError> {
    let mut blocks = Vec::new();
    let mut visited_clusters = std::collections::HashSet::new();
    let mut cluster_header_cache: HashMap<u64, u64> = HashMap::new();

    for cp in cue_points {
        if let Some(rel_pos) = cp.relative_position {
            // Direct seek: read only the referenced block.
            let cluster_data_start = match cluster_header_cache.get(&cp.cluster_position) {
                Some(&cached) => cached,
                None => {
                    reader.seek_to(cp.cluster_position)?;
                    let id = crate::ebml::read_element_id(reader)?;
                    if id.value != crate::ebml::ids::CLUSTER {
                        continue;
                    }
                    let _size = crate::ebml::read_element_size(reader)?;
                    let ds = reader.position();
                    cluster_header_cache.insert(cp.cluster_position, ds);
                    ds
                }
            };

            let block_pos = cluster_data_start + rel_pos;
            if let Some(pgs_block) = cluster::read_block_at_position(
                reader,
                block_pos,
                cp.time,
                pgs_track_numbers,
            )? {
                blocks.push(pgs_block);
            }
        } else {
            // No relative position — fall back to scanning entire cluster.
            if !visited_clusters.insert(cp.cluster_position) {
                continue;
            }
            reader.seek_to(cp.cluster_position)?;
            let id = crate::ebml::read_element_id(reader)?;
            if id.value != crate::ebml::ids::CLUSTER {
                continue;
            }
            let size = crate::ebml::read_element_size(reader)?;
            let data_start = reader.position();
            if size.value == u64::MAX {
                continue;
            }

            let pgs_blocks = cluster::scan_cluster_for_pgs(
                reader,
                data_start,
                size.value,
                pgs_track_numbers,
            )?;
            blocks.extend(pgs_blocks);
        }
    }

    Ok(blocks)
}

/// Extract PGS data using the Cues fast path with parallel workers.
///
/// Partitions cue points across N threads, each with its own file handle.
/// Results are merged in timestamp order before feeding through the assembler.
pub(crate) fn extract_via_cues_parallel(
    path: &Path,
    cue_points: &[cues::PgsCuePoint],
    pgs_track_numbers: &[u64],
    compression_map: &HashMap<u64, ContentCompAlgo>,
    timestamp_scale: u64,
    num_workers: usize,
) -> Result<Vec<(u64, Vec<DisplaySet>)>, PgsError> {
    // Sort cue points by file position for sequential access per worker.
    let mut sorted_cues = cue_points.to_vec();
    sorted_cues.sort_by_key(|cp| (cp.cluster_position, cp.relative_position.unwrap_or(0)));

    // Partition into chunks, respecting cluster boundaries so no cluster
    // is split across workers (avoids duplicate scanning for fallback path).
    let chunks = partition_by_cluster(&sorted_cues, num_workers);

    // Spawn parallel workers with scoped threads.
    let all_blocks: Result<Vec<Vec<cluster::PgsBlock>>, PgsError> = std::thread::scope(|s| {
        let handles: Vec<_> = chunks
            .into_iter()
            .map(|chunk| {
                let track_nums = pgs_track_numbers;
                s.spawn(move || -> Result<Vec<cluster::PgsBlock>, PgsError> {
                    let file = File::open(path)?;
                    let mut worker_reader = SeekBufReader::new(file);
                    extract_blocks_for_cues(&mut worker_reader, &chunk, track_nums)
                })
            })
            .collect();

        handles
            .into_iter()
            .map(|h| h.join().unwrap_or_else(|_| Err(PgsError::InvalidMkv("worker thread panicked".into()))))
            .collect()
    });

    // Merge all blocks, sort by timestamp, feed through assembler.
    let mut merged: Vec<cluster::PgsBlock> = all_blocks?.into_iter().flatten().collect();
    merged.sort_by_key(|b| (b.timestamp, b.track_number));

    let mut ta = TrackAssemblers::new(pgs_track_numbers, compression_map, timestamp_scale);
    ta.process_pgs_blocks(merged);
    Ok(ta.into_results())
}

/// Partition cue points into `num_workers` chunks, keeping all cue points
/// for the same cluster together to avoid duplicate cluster scanning.
pub(crate) fn partition_by_cluster(
    sorted_cues: &[cues::PgsCuePoint],
    num_workers: usize,
) -> Vec<Vec<cues::PgsCuePoint>> {
    // Group consecutive cue points by cluster_position.
    let mut groups: Vec<Vec<cues::PgsCuePoint>> = Vec::new();
    let mut current_cluster = u64::MAX;
    for cp in sorted_cues {
        if cp.cluster_position != current_cluster {
            groups.push(Vec::new());
            current_cluster = cp.cluster_position;
        }
        groups.last_mut().unwrap().push(cp.clone());
    }

    // Distribute groups across workers, balancing by count.
    let target_per_worker = (sorted_cues.len() + num_workers - 1) / num_workers;
    let mut chunks: Vec<Vec<cues::PgsCuePoint>> = vec![Vec::new(); num_workers];
    let mut worker_idx = 0;

    for group in groups {
        chunks[worker_idx].extend(group);
        if chunks[worker_idx].len() >= target_per_worker && worker_idx + 1 < num_workers {
            worker_idx += 1;
        }
    }

    // Remove empty chunks.
    chunks.retain(|c| !c.is_empty());
    chunks
}

/// Parse decoded PGS data into segments and feed them to the assembler.
///
/// Supports two formats:
/// - `.sup` format: 13-byte "PG" header with embedded PTS/DTS per segment.
/// - Raw format: `type(1) + length(2) + payload` (used in MKV compressed blocks),
///   where PTS comes from the MKV block timestamp.
pub(crate) fn process_pgs_block(
    data: &[u8],
    pts: u64,
    assembler: &mut DisplaySetAssembler,
    display_sets: &mut Vec<DisplaySet>,
) {
    if data.len() >= 2 && data[0..2] == PGS_MAGIC {
        // .sup format — each segment has a 13-byte "PG" header with its own PTS.
        let mut offset = 0;
        while offset < data.len() {
            match PgsSegment::parse(&data[offset..]) {
                Ok((segment, consumed)) => {
                    offset += consumed;
                    if let Some(ds) = assembler.push(segment) {
                        display_sets.push(ds);
                    }
                }
                Err(_) => break,
            }
        }
    } else {
        // Raw format — PTS comes from the MKV block timestamp.
        let segments = PgsSegment::parse_raw_segments(pts, 0, data);
        for segment in segments {
            if let Some(ds) = assembler.push(segment) {
                display_sets.push(ds);
            }
        }
    }
}
