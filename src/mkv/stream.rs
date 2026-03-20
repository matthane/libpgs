use crate::error::PgsError;
use crate::io::SeekBufReader;
use crate::pgs::DisplaySetAssembler;
use crate::{ContainerFormat, PgsTrackInfo, TrackDisplaySet};
use super::cluster::{self, ClusterEntry};
use super::cues::PgsCuePoint;
use super::tracks::ContentCompAlgo;
use super::{
    MkvMetadata, PARALLEL_CUE_THRESHOLD, MAX_PARALLEL_WORKERS,
    decode_block_data, process_pgs_block, extract_via_cues_parallel,
};
use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::File;
use std::path::PathBuf;

/// Streaming MKV extractor state machine.
///
/// Yields `TrackDisplaySet` one at a time by advancing through the MKV file
/// using whichever strategy (cues or sequential cluster scan) is best.
pub(crate) struct MkvExtractorState {
    reader: SeekBufReader<File>,
    path: PathBuf,
    metadata: MkvMetadata,
    source: MkvBlockSource,
    assemblers: HashMap<u64, DisplaySetAssembler>,
    compression: HashMap<u64, ContentCompAlgo>,
    track_info: HashMap<u64, PgsTrackInfo>,
    timestamp_scale: u64,
    pending: VecDeque<TrackDisplaySet>,
    /// Whether any display sets have been yielded (for collect_parallel check).
    yielded_count: usize,
}

/// Block sourcing strategy — each variant is a resumable state.
enum MkvBlockSource {
    /// Cues-based extraction: sequential seek to each cue point.
    Cues {
        cue_points: Vec<PgsCuePoint>,
        index: usize,
        cluster_header_cache: HashMap<u64, u64>,
        visited_clusters: HashSet<u64>,
    },
    /// Sequential cluster scan: full-scan every cluster.
    ClusterScan {
        cluster_map: Vec<ClusterEntry>,
        cluster_index: usize,
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
    ) -> Result<Self, PgsError> {
        // Determine which tracks to extract.
        let active_tracks: Vec<u64> = if let Some(filter) = track_filter {
            metadata.pgs_track_numbers.iter()
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
                track_info.insert(t.track_number, PgsTrackInfo {
                    track_id: t.track_number as u32,
                    language: t.language.clone(),
                    container: ContainerFormat::Matroska,
                    name: t.name.clone(),
                    flag_default: t.flag_default,
                    flag_forced: t.flag_forced,
                    display_set_count: t.track_uid
                        .and_then(|uid| metadata.frame_counts.get(&uid).copied()),
                });
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
            source: MkvBlockSource::Done, // placeholder — initialized below
            assemblers,
            compression,
            track_info,
            timestamp_scale,
            pending: VecDeque::new(),
            yielded_count: 0,
        })
    }

    /// Initialize the block source strategy. Must be called after construction.
    ///
    /// Separate from `new()` because it needs to borrow `self.reader` mutably.
    pub(crate) fn init_source(&mut self) -> Result<(), PgsError> {
        // Filter cue points to only reference active tracks.
        let filtered_cues = self.metadata.cue_points.as_ref().and_then(|cues| {
            // Cue points reference clusters that may contain blocks for any track.
            // We keep all cue points since a cluster may hold blocks for multiple tracks
            // and we can't know which tracks a cue point covers without scanning.
            if cues.is_empty() { None } else { Some(cues.clone()) }
        });

        if let Some(cue_points) = filtered_cues {
            self.source = MkvBlockSource::Cues {
                cue_points,
                index: 0,
                cluster_header_cache: HashMap::new(),
                visited_clusters: HashSet::new(),
            };
        } else {
            let first_cluster = self.metadata.layout.first_cluster_position
                .ok_or_else(|| PgsError::InvalidMkv("no Clusters found".into()))?;

            let cluster_map = cluster::build_cluster_map(
                &mut self.reader,
                first_cluster,
                self.metadata.layout.segment_data_end,
            )?;

            self.source = MkvBlockSource::ClusterScan {
                cluster_map,
                cluster_index: 0,
            };
        }

        Ok(())
    }

    /// Advance the state machine to produce the next display set.
    pub(crate) fn next_display_set(&mut self) -> Option<Result<TrackDisplaySet, PgsError>> {
        loop {
            // Drain pending display sets first.
            if let Some(tds) = self.pending.pop_front() {
                self.yielded_count += 1;
                return Some(Ok(tds));
            }

            // Advance the block source.
            match self.advance_source() {
                Ok(true) => continue,  // Blocks were processed, check pending.
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
        let active_tracks: Vec<u64> = self.assemblers.keys().copied().collect();

        match &mut self.source {
            MkvBlockSource::Cues { cue_points, index, cluster_header_cache, visited_clusters } => {
                if *index >= cue_points.len() {
                    self.source = MkvBlockSource::Done;
                    return Ok(false);
                }

                let cp = cue_points[*index].clone();
                *index += 1;

                let blocks = Self::extract_cue_point_blocks(
                    &mut self.reader,
                    &cp,
                    &active_tracks,
                    cluster_header_cache,
                    visited_clusters,
                )?;

                self.process_blocks(blocks);
                Ok(true)
            }

            MkvBlockSource::ClusterScan { cluster_map, cluster_index } => {
                if *cluster_index >= cluster_map.len() {
                    self.source = MkvBlockSource::Done;
                    return Ok(false);
                }

                let entry = cluster_map[*cluster_index].clone();
                *cluster_index += 1;

                let blocks = cluster::scan_cluster_for_pgs(
                    &mut self.reader,
                    entry.data_start,
                    entry.data_size,
                    &active_tracks,
                )?;

                self.process_blocks(blocks);
                Ok(true)
            }

            MkvBlockSource::Done => Ok(false),
        }
    }

    /// Extract PGS blocks from a single cue point.
    fn extract_cue_point_blocks(
        reader: &mut SeekBufReader<File>,
        cp: &PgsCuePoint,
        active_tracks: &[u64],
        cluster_header_cache: &mut HashMap<u64, u64>,
        visited_clusters: &mut HashSet<u64>,
    ) -> Result<Vec<cluster::PgsBlock>, PgsError> {
        if let Some(rel_pos) = cp.relative_position {
            // Direct seek to the referenced block.
            let cluster_data_start = match cluster_header_cache.get(&cp.cluster_position) {
                Some(&cached) => cached,
                None => {
                    reader.seek_to(cp.cluster_position)?;
                    let id = crate::ebml::read_element_id(reader)?;
                    if id.value != crate::ebml::ids::CLUSTER {
                        return Ok(Vec::new());
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
                active_tracks,
            )? {
                Ok(vec![pgs_block])
            } else {
                Ok(Vec::new())
            }
        } else {
            // No relative position — scan entire cluster.
            if !visited_clusters.insert(cp.cluster_position) {
                return Ok(Vec::new());
            }

            reader.seek_to(cp.cluster_position)?;
            let id = crate::ebml::read_element_id(reader)?;
            if id.value != crate::ebml::ids::CLUSTER {
                return Ok(Vec::new());
            }
            let size = crate::ebml::read_element_size(reader)?;
            let data_start = reader.position();
            if size.value == u64::MAX {
                return Ok(Vec::new());
            }

            cluster::scan_cluster_for_pgs(
                reader,
                data_start,
                size.value,
                active_tracks,
            )
        }
    }

    /// Convert an MKV timestamp to PGS 90kHz PTS.
    fn mkv_timestamp_to_pts(&self, mkv_ts: i64) -> u64 {
        let time_ns = mkv_ts as i128 * self.timestamp_scale as i128;
        let pts = time_ns * 9 / 100_000;
        pts.max(0) as u64
    }

    /// Decode blocks and push segments through assemblers, collecting display sets into pending.
    fn process_blocks(&mut self, blocks: Vec<cluster::PgsBlock>) {
        for block in blocks {
            let comp = self.compression.get(&block.track_number);
            let decoded = decode_block_data(&block.data, comp);
            let pts = self.mkv_timestamp_to_pts(block.timestamp);

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
    pub(crate) fn try_collect_parallel(&self) -> Option<Result<Vec<crate::TrackDisplaySets>, PgsError>> {
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
            track_results.into_iter()
                .filter_map(|(track_num, display_sets)| {
                    let info = self.track_info.get(&track_num)?.clone();
                    Some(crate::TrackDisplaySets { track: info, display_sets })
                })
                .collect()
        }))
    }

    /// Get current bytes read from the underlying reader.
    pub(crate) fn bytes_read(&self) -> u64 {
        self.reader.bytes_read()
    }
}
