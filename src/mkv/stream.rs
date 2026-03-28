use super::cluster;
use super::cues::PgsCuePoint;
use super::tracks::ContentCompAlgo;
use super::{
    MAX_PARALLEL_WORKERS, MkvMetadata, PARALLEL_CUE_THRESHOLD, decode_block_data,
    extract_blocks_for_cue_point, extract_via_cues_parallel, mkv_timestamp_to_pts,
    process_pgs_block,
};
use crate::ebml::{ids, read_element_id, read_element_size};
use crate::error::PgsError;
use crate::io::SeekBufReader;
use crate::pgs::DisplaySetAssembler;
use crate::{ContainerFormat, MkvStrategy, PgsTrackInfo, TrackDisplaySet};
use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::File;
use std::path::PathBuf;

/// Seek margin for binary search convergence.
const SEEK_MARGIN: u64 = 2 * 1024 * 1024;

/// Probe scan limit: how far to scan forward looking for a Cluster element (512 KB).
const PROBE_SCAN_LIMIT: u64 = 512 * 1024;

/// Streaming MKV extractor state machine.
///
/// Yields `TrackDisplaySet` one at a time by advancing through the MKV file
/// using whichever strategy (cues or sequential scan) is best.
pub(crate) struct MkvExtractorState {
    reader: SeekBufReader<File>,
    path: PathBuf,
    metadata: MkvMetadata,
    source: MkvBlockSource,
    assemblers: HashMap<u64, DisplaySetAssembler>,
    /// Cached track numbers from assemblers (never changes after construction).
    active_tracks: Vec<u64>,
    compression: HashMap<u64, ContentCompAlgo>,
    track_info: HashMap<u64, PgsTrackInfo>,
    timestamp_scale: u64,
    pending: VecDeque<TrackDisplaySet>,
    /// Whether any display sets have been yielded (for collect_parallel check).
    yielded_count: usize,
    /// Strategy override for benchmarking.
    strategy: MkvStrategy,
    /// Time range for filtering (milliseconds).
    time_range_start_ms: Option<f64>,
    time_range_end_ms: Option<f64>,
}

/// Block sourcing strategy — each variant is a resumable state.
enum MkvBlockSource {
    /// Not yet initialized — `init_source()` will be called on first `next()`.
    Uninitialized,
    /// Cues-based extraction: sequential seek to each cue point.
    Cues {
        cue_points: Vec<PgsCuePoint>,
        index: usize,
        cluster_header_cache: HashMap<u64, u64>,
        visited_clusters: HashSet<u64>,
    },
    /// Single-pass linear scan: read through the Segment, processing Clusters
    /// as they're encountered without building a map first.
    SequentialScan {
        /// Current read position in the file.
        current_position: u64,
        /// End of the Segment data.
        segment_data_end: u64,
    },
    /// Extraction complete.
    Done,
}

impl MkvExtractorState {
    /// Create a new MKV extractor from pre-parsed metadata.
    ///
    /// The `track_filter` restricts extraction to specific track numbers.
    /// If `None`, all PGS tracks are extracted.
    pub(crate) fn new(
        reader: SeekBufReader<File>,
        path: PathBuf,
        metadata: MkvMetadata,
        track_filter: Option<&[u32]>,
        strategy: MkvStrategy,
    ) -> Result<Self, PgsError> {
        // Determine which tracks to extract.
        let active_tracks: Vec<u64> = if let Some(filter) = track_filter {
            metadata
                .pgs_track_numbers
                .iter()
                .filter(|&&tn| filter.contains(&(tn as u32)))
                .copied()
                .collect()
        } else {
            metadata.pgs_track_numbers.clone()
        };

        // Build track info map.
        let mut track_info = HashMap::new();
        for t in &metadata.pgs_tracks {
            if active_tracks.contains(&t.track_number) {
                let has_cues =
                    Some(metadata.cue_points.as_ref().is_some_and(|cues| {
                        cues.iter().any(|cp| cp.track_number == t.track_number)
                    }));
                track_info.insert(
                    t.track_number,
                    PgsTrackInfo {
                        track_id: t.track_number as u32,
                        language: t.language.clone(),
                        container: ContainerFormat::Matroska,
                        name: t.name.clone(),
                        flag_default: t.flag_default,
                        flag_forced: t.flag_forced,
                        display_set_count: t
                            .track_uid
                            .and_then(|uid| metadata.frame_counts.get(&uid).copied()),
                        has_cues,
                    },
                );
            }
        }

        // Build assemblers for active tracks.
        let mut assemblers = HashMap::new();
        for &tn in &active_tracks {
            assemblers.insert(tn, DisplaySetAssembler::new());
        }

        let compression = metadata.compression_map.clone();
        let timestamp_scale = metadata.timestamp_scale;

        Ok(Self {
            reader,
            path,
            metadata,
            source: MkvBlockSource::Uninitialized,
            assemblers,
            active_tracks,
            compression,
            track_info,
            timestamp_scale,
            pending: VecDeque::new(),
            yielded_count: 0,
            strategy,
            time_range_start_ms: None,
            time_range_end_ms: None,
        })
    }

    /// Set a time range for filtering cue points and early termination.
    pub(crate) fn set_time_range(&mut self, start_ms: Option<f64>, end_ms: Option<f64>) {
        self.time_range_start_ms = start_ms;
        self.time_range_end_ms = end_ms;
    }

    /// Initialize the block source strategy (lazy — called on first iteration).
    ///
    /// Deferred from construction so that track metadata is available immediately
    /// without waiting for potentially expensive cluster map building.
    fn init_source(&mut self) -> Result<(), PgsError> {
        let first_cluster = self
            .metadata
            .layout
            .first_cluster_position
            .ok_or_else(|| PgsError::InvalidMkv("no Clusters found".into()))?;

        // Convert time range from ms to MKV timestamp units for cue point filtering.
        // MKV timestamp units: time_ns / timestamp_scale. ms → ns: ms * 1_000_000.
        let start_mkv = self.time_range_start_ms.map(|ms| {
            (ms * 1_000_000.0 / self.timestamp_scale as f64) as u64
        });
        let end_mkv = self.time_range_end_ms.map(|ms| {
            (ms * 1_000_000.0 / self.timestamp_scale as f64) as u64
        });

        // Auto: use Cues if available, otherwise fall back to Sequential.
        if self.strategy == MkvStrategy::Auto {
            let active_tracks: Vec<u64> = self.assemblers.keys().copied().collect();
            let filtered_cues = self.metadata.cue_points.as_ref().and_then(|cues| {
                let filtered: Vec<_> = cues
                    .iter()
                    .filter(|cp| {
                        if !active_tracks.contains(&cp.track_number) {
                            return false;
                        }
                        if let Some(start) = start_mkv {
                            if cp.time < start {
                                return false;
                            }
                        }
                        if let Some(end) = end_mkv {
                            if cp.time > end {
                                return false;
                            }
                        }
                        true
                    })
                    .cloned()
                    .collect();
                if filtered.is_empty() {
                    None
                } else {
                    Some(filtered)
                }
            });

            if let Some(cue_points) = filtered_cues {
                self.source = MkvBlockSource::Cues {
                    cue_points,
                    index: 0,
                    cluster_header_cache: HashMap::new(),
                    visited_clusters: HashSet::new(),
                };
                return Ok(());
            }
        }

        // Sequential: reopen with a large buffer for linear throughput.
        const SEQ_BUF_SIZE: usize = 2 * 1024 * 1024; // 2 MB
        let file = File::open(&self.path)?;
        self.reader = SeekBufReader::with_capacity(SEQ_BUF_SIZE, file);

        // Estimate start position via binary search refinement.
        let segment_start = first_cluster;
        let segment_end = self.metadata.layout.segment_data_end;
        let scan_start = if let Some(start_mkv) = start_mkv {
            if let Some(duration) = self.metadata.duration {
                let duration_mkv = duration as u64;
                if duration_mkv > 0 {
                    let segment_size = segment_end - segment_start;
                    let ratio = start_mkv as f64 / duration_mkv as f64;
                    let estimated =
                        segment_start + (segment_size as f64 * ratio) as u64;

                    // Binary search: probe Cluster timestamps to converge.
                    let mut lo = segment_start;
                    let mut hi = estimated.min(segment_end);
                    let mut best = segment_start;
                    for _ in 0..20 {
                        if hi.saturating_sub(lo) < SEEK_MARGIN {
                            break;
                        }
                        let mid = lo + (hi - lo) / 2;
                        match self.probe_cluster_timestamp(mid, segment_end) {
                            Some(ts) if ts > start_mkv => {
                                hi = mid;
                            }
                            Some(_) => {
                                best = mid;
                                lo = mid;
                            }
                            None => {
                                hi = mid;
                            }
                        }
                    }
                    best.saturating_sub(SEEK_MARGIN).max(segment_start)
                } else {
                    segment_start
                }
            } else {
                segment_start
            }
        } else {
            segment_start
        };

        self.source = MkvBlockSource::SequentialScan {
            current_position: scan_start,
            segment_data_end: segment_end,
        };

        Ok(())
    }

    /// Advance the state machine to produce the next display set.
    pub(crate) fn next_display_set(&mut self) -> Option<Result<TrackDisplaySet, PgsError>> {
        // Lazy initialization: deferred from open() so track metadata is
        // available immediately without waiting for I/O strategy setup.
        if matches!(self.source, MkvBlockSource::Uninitialized)
            && let Err(e) = self.init_source()
        {
            self.source = MkvBlockSource::Done;
            return Some(Err(e));
        }

        loop {
            // Drain pending display sets first.
            if let Some(tds) = self.pending.pop_front() {
                self.yielded_count += 1;
                return Some(Ok(tds));
            }

            // Advance the block source.
            match self.advance_source() {
                Ok(true) => continue,     // Blocks were processed, check pending.
                Ok(false) => return None, // Source exhausted.
                Err(e) => {
                    self.source = MkvBlockSource::Done;
                    return Some(Err(e));
                }
            }
        }
    }

    /// Advance the source by one step, processing any resulting blocks.
    /// Returns `Ok(true)` if progress was made, `Ok(false)` if done.
    fn advance_source(&mut self) -> Result<bool, PgsError> {
        let active_tracks = &self.active_tracks;

        match &mut self.source {
            MkvBlockSource::Cues {
                cue_points,
                index,
                cluster_header_cache,
                visited_clusters,
            } => {
                if *index >= cue_points.len() {
                    self.source = MkvBlockSource::Done;
                    return Ok(false);
                }

                let cp = cue_points[*index].clone();
                *index += 1;

                let blocks = extract_blocks_for_cue_point(
                    &mut self.reader,
                    &cp,
                    active_tracks,
                    cluster_header_cache,
                    visited_clusters,
                )?;

                self.process_blocks(blocks);
                Ok(true)
            }

            MkvBlockSource::SequentialScan {
                current_position,
                segment_data_end,
            } => {
                let end = *segment_data_end;

                loop {
                    if *current_position >= end {
                        self.source = MkvBlockSource::Done;
                        return Ok(false);
                    }

                    // Ensure reader is at the right position (first iteration
                    // needs a seek; subsequent ones are already there).
                    if self.reader.position() != *current_position {
                        self.reader.seek_to(*current_position)?;
                    }

                    let id = match read_element_id(&mut self.reader) {
                        Ok(id) => id,
                        Err(_) => {
                            self.source = MkvBlockSource::Done;
                            return Ok(false);
                        }
                    };
                    let size = match read_element_size(&mut self.reader) {
                        Ok(s) => s,
                        Err(_) => {
                            self.source = MkvBlockSource::Done;
                            return Ok(false);
                        }
                    };
                    let data_start = self.reader.position();

                    if size.value == u64::MAX {
                        // Unknown-size element — can't determine length.
                        self.source = MkvBlockSource::Done;
                        return Ok(false);
                    }

                    let element_end = data_start + size.value;

                    if id.value == ids::CLUSTER {
                        // Process this cluster with fully sequential I/O.
                        let blocks = cluster::scan_cluster_for_pgs_sequential(
                            &mut self.reader,
                            data_start,
                            size.value,
                            active_tracks,
                        )?;

                        *current_position = element_end;
                        self.process_blocks(blocks);
                        return Ok(true);
                    }

                    // Not a cluster — drain (read through) to keep I/O sequential.
                    self.reader.drain(size.value)?;
                    *current_position = element_end;
                }
            }

            MkvBlockSource::Uninitialized => unreachable!("init_source not called"),
            MkvBlockSource::Done => Ok(false),
        }
    }

    /// Decode blocks and push segments through assemblers, collecting display sets into pending.
    fn process_blocks(&mut self, blocks: Vec<cluster::PgsBlock>) {
        for block in blocks {
            let comp = self.compression.get(&block.track_number);
            let Some(decoded) = decode_block_data(&block.data, comp) else {
                continue; // Skip blocks that fail decompression
            };
            let pts = mkv_timestamp_to_pts(block.timestamp, self.timestamp_scale);

            if let Some(assembler) = self.assemblers.get_mut(&block.track_number) {
                let mut collected = Vec::new();
                process_pgs_block(&decoded, pts, assembler, &mut collected);

                if let Some(info) = self.track_info.get(&block.track_number) {
                    for ds in collected {
                        self.pending.push_back(TrackDisplaySet {
                            track_id: info.track_id,
                            language: info.language.clone(),
                            container: info.container,
                            display_set: ds,
                        });
                    }
                }
            }
        }
    }

    /// Attempt parallel cues extraction for batch drain.
    ///
    /// Returns `Some(results)` if parallel extraction was used, `None` if not applicable.
    /// Only used when the iterator hasn't been partially consumed.
    pub(crate) fn try_collect_parallel(
        &self,
    ) -> Option<Result<Vec<crate::TrackDisplaySets>, PgsError>> {
        // Can only use parallel extraction if nothing has been yielded yet.
        if self.yielded_count > 0 {
            return None;
        }

        let cue_points = self.metadata.cue_points.as_ref()?;

        let num_workers = std::thread::available_parallelism()
            .map(|n| n.get().min(MAX_PARALLEL_WORKERS))
            .unwrap_or(1);

        if cue_points.len() < PARALLEL_CUE_THRESHOLD || num_workers <= 1 {
            return None;
        }

        let active_tracks: Vec<u64> = self.assemblers.keys().copied().collect();

        let result = extract_via_cues_parallel(
            &self.path,
            cue_points,
            &active_tracks,
            &self.compression,
            self.timestamp_scale,
            num_workers,
        );

        Some(result.map(|track_results| {
            track_results
                .into_iter()
                .filter_map(|(track_num, display_sets)| {
                    let info = self.track_info.get(&track_num)?.clone();
                    Some(crate::TrackDisplaySets {
                        track: info,
                        display_sets,
                    })
                })
                .collect()
        }))
    }

    /// Probe the Cluster timestamp at or after the given byte position.
    ///
    /// Seeks to `position`, scans forward for a Cluster element, reads its
    /// Timestamp child, and returns the timestamp in MKV units. Used for
    /// binary search refinement during sequential scan seeking.
    fn probe_cluster_timestamp(&mut self, position: u64, segment_end: u64) -> Option<u64> {
        self.reader.seek_to(position).ok()?;
        let scan_end = (position + PROBE_SCAN_LIMIT).min(segment_end);

        while self.reader.position() < scan_end {
            let id = read_element_id(&mut self.reader).ok()?;
            let size = read_element_size(&mut self.reader).ok()?;

            if id.value == ids::CLUSTER {
                // Read children looking for Timestamp.
                let cluster_end = self.reader.position() + size.value;
                while self.reader.position() < cluster_end {
                    let child_id = read_element_id(&mut self.reader).ok()?;
                    let child_size = read_element_size(&mut self.reader).ok()?;
                    if child_id.value == ids::TIMESTAMP {
                        return self
                            .reader
                            .read_uint_be(child_size.value as usize)
                            .ok();
                    }
                    // Skip non-Timestamp children.
                    self.reader
                        .seek_to(self.reader.position() + child_size.value)
                        .ok()?;
                }
                return None; // Cluster found but no Timestamp child.
            }

            // Skip non-Cluster elements.
            self.reader
                .seek_to(self.reader.position() + size.value)
                .ok()?;
        }
        None
    }

    /// Get current bytes read from the underlying reader.
    pub(crate) fn bytes_read(&self) -> u64 {
        self.reader.bytes_read()
    }
}
