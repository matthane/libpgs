pub mod ebml;
pub mod error;
pub mod io;
pub mod m2ts;
pub mod mkv;
pub mod pgs;
pub mod sup;

use error::PgsError;
use io::SeekBufReader;
use m2ts::stream::M2tsExtractorState;
use mkv::stream::MkvExtractorState;
use pgs::DisplaySet;
use std::collections::HashMap;
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use sup::stream::SupExtractorState;

/// Container format of the source file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContainerFormat {
    Matroska,
    M2ts,
    TransportStream,
    Sup,
}

/// MKV extraction strategy override.
///
/// Controls how the extractor navigates Clusters in an MKV file.
/// Used for benchmarking and tuning NAS performance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MkvStrategy {
    /// Automatic: use Cues if available, otherwise Sequential.
    #[default]
    Auto,
    /// Single-pass linear scan through the Segment, processing Clusters
    /// as they're encountered without building a map first.
    Sequential,
}

/// Metadata about a PGS track found in the container.
#[derive(Debug, Clone)]
pub struct PgsTrackInfo {
    /// Track number (MKV) or PID (M2TS).
    pub track_id: u32,
    /// Language code, if available.
    pub language: Option<String>,
    /// Container format.
    pub container: ContainerFormat,
    /// Track name / title (MKV TrackName).
    pub name: Option<String>,
    /// Whether this track is flagged as default (MKV FlagDefault).
    pub flag_default: Option<bool>,
    /// Whether this track contains forced subtitles (MKV FlagForced).
    pub flag_forced: Option<bool>,
    /// Total number of display sets / frames, if known from container metadata.
    pub display_set_count: Option<u64>,
    /// Whether the container has cue/index entries for this track (MKV only).
    pub has_cues: Option<bool>,
}

/// Display sets extracted from a single PGS track.
#[derive(Debug, Clone)]
pub struct TrackDisplaySets {
    /// Track metadata.
    pub track: PgsTrackInfo,
    /// All display sets for this track, in presentation order.
    pub display_sets: Vec<DisplaySet>,
}

/// I/O statistics from an extraction operation.
#[derive(Debug, Clone)]
pub struct ExtractionStats {
    /// Total size of the source file in bytes.
    pub file_size: u64,
    /// Total bytes actually read from the file during extraction.
    pub bytes_read: u64,
}

/// A display set annotated with its source track.
#[derive(Debug, Clone)]
pub struct TrackDisplaySet {
    /// Track number (MKV) or PID (M2TS).
    pub track_id: u32,
    /// Language code, if available.
    pub language: Option<String>,
    /// Container format of the source file.
    pub container: ContainerFormat,
    /// The display set itself.
    pub display_set: DisplaySet,
}

/// Internal dispatch to format-specific streaming state machines.
enum ExtractorInner {
    Mkv(Box<MkvExtractorState>),
    M2ts(M2tsExtractorState),
    Sup(SupExtractorState),
    Done,
}

/// Streaming PGS extractor that yields display sets incrementally.
///
/// Created via [`Extractor::open`]. Implements
/// `Iterator<Item = Result<TrackDisplaySet, PgsError>>`.
///
/// # Streaming
///
/// Display sets are yielded one at a time in file order (interleaved across
/// tracks for multi-track files). Only the I/O needed to produce the next
/// display set is performed on each call to `next()`.
///
/// # Early Termination
///
/// Simply drop the `Extractor` to stop extraction. No further I/O occurs.
///
/// # History
///
/// Processed display sets are cataloged internally. Use [`history()`](Extractor::history)
/// to access all display sets yielded so far, or
/// [`history_for_track()`](Extractor::history_for_track) for a specific track.
/// Use [`drain_history()`](Extractor::drain_history) or
/// [`clear_history()`](Extractor::clear_history) to manage memory during long extractions.
///
/// # Example
///
/// ```no_run
/// # use std::path::Path;
/// let mut extractor = libpgs::Extractor::open("movie.mkv").unwrap();
///
/// // Read first 5 display sets from the English track
/// let eng_id = extractor.tracks().iter()
///     .find(|t| t.language.as_deref() == Some("eng"))
///     .unwrap()
///     .track_id;
///
/// let mut extractor = extractor.with_track_filter(&[eng_id]);
///
/// for ds in extractor.by_ref().take(5) {
///     let ds = ds.unwrap();
///     println!("PTS: {}ms", ds.display_set.pts_ms);
/// }
///
/// println!("Bytes read: {}", extractor.stats().bytes_read);
/// ```
pub struct Extractor {
    inner: ExtractorInner,
    catalog: Vec<TrackDisplaySet>,
    tracks: Vec<PgsTrackInfo>,
    stats: ExtractionStats,
    path: PathBuf,
    format: ContainerFormat,
    mkv_strategy: MkvStrategy,
}

impl Extractor {
    /// Open a file and prepare for streaming extraction.
    ///
    /// Performs initial metadata parsing (format detection, track discovery)
    /// but does NOT extract any display sets yet. All PGS tracks are selected
    /// by default; use [`with_track_filter`](Extractor::with_track_filter) to restrict.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, PgsError> {
        let path = path.as_ref();
        let file = File::open(path)?;
        let file_size = file.metadata().map(|m| m.len()).unwrap_or(0);
        let mut reader = SeekBufReader::new(file);

        let format = detect_format(&mut reader)?;

        match format {
            ContainerFormat::Matroska => {
                let meta = mkv::prepare_mkv_metadata(&mut reader)?;
                let tracks: Vec<PgsTrackInfo> = meta
                    .pgs_tracks
                    .iter()
                    .map(|t| mkv_track_to_info(t, &meta.frame_counts, &meta.cue_points))
                    .collect();

                let state = MkvExtractorState::new(
                    reader,
                    path.to_path_buf(),
                    meta,
                    None,
                    MkvStrategy::Auto,
                )?;

                Ok(Extractor {
                    inner: ExtractorInner::Mkv(Box::new(state)),
                    catalog: Vec::new(),
                    tracks,
                    stats: ExtractionStats {
                        file_size,
                        bytes_read: 0,
                    },
                    path: path.to_path_buf(),
                    format: ContainerFormat::Matroska,
                    mkv_strategy: MkvStrategy::Auto,
                })
            }
            format @ (ContainerFormat::M2ts | ContainerFormat::TransportStream) => {
                // Reopen with large buffer for M2TS throughput.
                let file = File::open(path)?;
                let mut reader = SeekBufReader::with_capacity(M2TS_BUF_SIZE, file);
                detect_format(&mut reader)?;

                let meta = m2ts::prepare_m2ts_metadata(&mut reader, Some(path))?;
                let tracks: Vec<PgsTrackInfo> = meta
                    .tracks
                    .iter()
                    .map(|t| m2ts_track_to_info(t, format))
                    .collect();

                let state = M2tsExtractorState::new(reader, meta, format, None);

                Ok(Extractor {
                    inner: ExtractorInner::M2ts(state),
                    catalog: Vec::new(),
                    tracks,
                    stats: ExtractionStats {
                        file_size,
                        bytes_read: 0,
                    },
                    path: path.to_path_buf(),
                    format,
                    mkv_strategy: MkvStrategy::Auto,
                })
            }
            ContainerFormat::Sup => {
                let tracks = vec![sup_track_info()];
                let state = SupExtractorState::new(reader);

                Ok(Extractor {
                    inner: ExtractorInner::Sup(state),
                    catalog: Vec::new(),
                    tracks,
                    stats: ExtractionStats {
                        file_size,
                        bytes_read: 0,
                    },
                    path: path.to_path_buf(),
                    format: ContainerFormat::Sup,
                    mkv_strategy: MkvStrategy::Auto,
                })
            }
        }
    }

    /// Override the MKV extraction strategy. Chainable.
    ///
    /// Must be called before the first call to `next()`. Only affects MKV files.
    /// Useful for benchmarking different strategies on NAS storage.
    #[must_use]
    pub fn with_mkv_strategy(mut self, strategy: MkvStrategy) -> Self {
        if self.format != ContainerFormat::Matroska || strategy == self.mkv_strategy {
            return self;
        }

        let path = self.path.clone();
        let file_size = self.stats.file_size;

        match Self::open_with_strategy(&path, file_size, strategy, None) {
            Ok(ext) => ext,
            Err(_) => {
                self.mkv_strategy = strategy;
                self
            }
        }
    }

    /// Restrict extraction to specific tracks. Chainable.
    ///
    /// Must be called before the first call to `next()`. Configures the
    /// internal state machine to only create assemblers for matching tracks
    /// and skip non-matching blocks at the source level.
    #[must_use]
    ///
    /// # Example
    ///
    /// ```no_run
    /// let mut ext = libpgs::Extractor::open("movie.mkv").unwrap();
    /// let id = ext.tracks()[0].track_id;
    /// let mut ext = ext.with_track_filter(&[id]);
    /// ```
    pub fn with_track_filter(self, track_ids: &[u32]) -> Self {
        if track_ids.is_empty() {
            return self;
        }

        let path = self.path.clone();
        let file_size = self.stats.file_size;
        let format = self.format;
        let mkv_strategy = self.mkv_strategy;

        // Reconstruct with the filter applied. Reopens the file and
        // re-parses metadata so the state machine is initialized with
        // only the requested tracks from the start.
        match Self::open_filtered(&path, file_size, format, track_ids, mkv_strategy) {
            Ok(ext) => ext,
            Err(_) => self,
        }
    }

    /// Open an MKV file with a specific strategy (no track filter).
    fn open_with_strategy(
        path: &Path,
        file_size: u64,
        strategy: MkvStrategy,
        track_ids: Option<&[u32]>,
    ) -> Result<Self, PgsError> {
        let file = File::open(path)?;
        let mut reader = SeekBufReader::new(file);
        detect_format(&mut reader)?;

        let meta = mkv::prepare_mkv_metadata(&mut reader)?;
        let tracks: Vec<PgsTrackInfo> = if let Some(ids) = track_ids {
            meta.pgs_tracks
                .iter()
                .filter(|t| ids.contains(&(t.track_number as u32)))
                .map(|t| mkv_track_to_info(t, &meta.frame_counts, &meta.cue_points))
                .collect()
        } else {
            meta.pgs_tracks
                .iter()
                .map(|t| mkv_track_to_info(t, &meta.frame_counts, &meta.cue_points))
                .collect()
        };

        let state = MkvExtractorState::new(reader, path.to_path_buf(), meta, track_ids, strategy)?;

        Ok(Extractor {
            inner: ExtractorInner::Mkv(Box::new(state)),
            catalog: Vec::new(),
            tracks,
            stats: ExtractionStats {
                file_size,
                bytes_read: 0,
            },
            path: path.to_path_buf(),
            format: ContainerFormat::Matroska,
            mkv_strategy: strategy,
        })
    }

    fn open_filtered(
        path: &Path,
        file_size: u64,
        format: ContainerFormat,
        track_ids: &[u32],
        mkv_strategy: MkvStrategy,
    ) -> Result<Self, PgsError> {
        match format {
            ContainerFormat::Matroska => {
                Self::open_with_strategy(path, file_size, mkv_strategy, Some(track_ids))
            }
            fmt @ (ContainerFormat::M2ts | ContainerFormat::TransportStream) => {
                let file = File::open(path)?;
                let mut reader = SeekBufReader::with_capacity(M2TS_BUF_SIZE, file);
                detect_format(&mut reader)?;

                let meta = m2ts::prepare_m2ts_metadata(&mut reader, Some(path))?;
                let tracks: Vec<PgsTrackInfo> = meta
                    .tracks
                    .iter()
                    .filter(|t| track_ids.contains(&(t.pid as u32)))
                    .map(|t| m2ts_track_to_info(t, fmt))
                    .collect();

                let state = M2tsExtractorState::new(reader, meta, fmt, Some(track_ids));

                Ok(Extractor {
                    inner: ExtractorInner::M2ts(state),
                    catalog: Vec::new(),
                    tracks,
                    stats: ExtractionStats {
                        file_size,
                        bytes_read: 0,
                    },
                    path: path.to_path_buf(),
                    format: fmt,
                    mkv_strategy: MkvStrategy::Auto,
                })
            }
            ContainerFormat::Sup => {
                if !track_ids.contains(&0) {
                    return Ok(Extractor {
                        inner: ExtractorInner::Done,
                        catalog: Vec::new(),
                        tracks: Vec::new(),
                        stats: ExtractionStats {
                            file_size,
                            bytes_read: 0,
                        },
                        path: path.to_path_buf(),
                        format: ContainerFormat::Sup,
                        mkv_strategy: MkvStrategy::Auto,
                    });
                }

                let file = File::open(path)?;
                let mut reader = SeekBufReader::new(file);
                detect_format(&mut reader)?;

                let tracks = vec![sup_track_info()];
                let state = SupExtractorState::new(reader);

                Ok(Extractor {
                    inner: ExtractorInner::Sup(state),
                    catalog: Vec::new(),
                    tracks,
                    stats: ExtractionStats {
                        file_size,
                        bytes_read: 0,
                    },
                    path: path.to_path_buf(),
                    format: ContainerFormat::Sup,
                    mkv_strategy: MkvStrategy::Auto,
                })
            }
        }
    }

    /// PGS tracks discovered in the file.
    pub fn tracks(&self) -> &[PgsTrackInfo] {
        &self.tracks
    }

    /// All display sets yielded so far, in order.
    pub fn history(&self) -> &[TrackDisplaySet] {
        &self.catalog
    }

    /// Display sets yielded so far for a specific track.
    pub fn history_for_track(&self, track_id: u32) -> Vec<&TrackDisplaySet> {
        self.catalog
            .iter()
            .filter(|ds| ds.track_id == track_id)
            .collect()
    }

    /// Take all cataloged display sets, clearing the internal history.
    ///
    /// Useful for periodic memory management during long extractions.
    pub fn drain_history(&mut self) -> Vec<TrackDisplaySet> {
        std::mem::take(&mut self.catalog)
    }

    /// Discard all cataloged display sets to free memory.
    pub fn clear_history(&mut self) {
        self.catalog.clear();
    }

    /// Current I/O statistics. Updates as extraction progresses.
    pub fn stats(&self) -> &ExtractionStats {
        &self.stats
    }

    /// Exhaust the iterator and return all display sets grouped by track.
    ///
    /// For MKV files with Cues and enough cue points that haven't been
    /// partially consumed, this uses parallel extraction with multiple
    /// file handles for maximum throughput.
    pub fn collect_by_track(mut self) -> Result<Vec<TrackDisplaySets>, PgsError> {
        // Try parallel cues optimization for MKV.
        if let ExtractorInner::Mkv(ref state) = self.inner
            && let Some(result) = state.try_collect_parallel()
        {
            return result;
        }

        // Build track info lookup from the pre-parsed metadata.
        let track_info_map: HashMap<u32, PgsTrackInfo> = self
            .tracks
            .iter()
            .map(|t| (t.track_id, t.clone()))
            .collect();

        // Sequential drain and group.
        let results = self.by_ref().collect::<Result<Vec<_>, _>>()?;
        Ok(group_by_track(results, &track_info_map))
    }

    /// Update stats from the inner reader.
    fn update_stats(&mut self) {
        self.stats.bytes_read = match &self.inner {
            ExtractorInner::Mkv(state) => state.bytes_read(),
            ExtractorInner::M2ts(state) => state.bytes_read(),
            ExtractorInner::Sup(state) => state.bytes_read(),
            ExtractorInner::Done => self.stats.bytes_read,
        };
    }
}

impl Iterator for Extractor {
    type Item = Result<TrackDisplaySet, PgsError>;

    fn next(&mut self) -> Option<Self::Item> {
        let result = match &mut self.inner {
            ExtractorInner::Mkv(state) => state.next_display_set(),
            ExtractorInner::M2ts(state) => state.next_display_set(),
            ExtractorInner::Sup(state) => state.next_display_set(),
            ExtractorInner::Done => return None,
        };

        self.update_stats();

        match result {
            Some(Ok(tds)) => {
                self.catalog.push(tds.clone());
                Some(Ok(tds))
            }
            Some(Err(e)) => {
                self.inner = ExtractorInner::Done;
                Some(Err(e))
            }
            None => {
                self.inner = ExtractorInner::Done;
                None
            }
        }
    }
}

/// Detect the container format by reading the first few bytes.
fn detect_format(reader: &mut SeekBufReader<File>) -> Result<ContainerFormat, PgsError> {
    reader.seek_to(0)?;
    let mut magic = [0u8; 5];
    reader.read_exact(&mut magic)?;
    reader.seek_to(0)?;

    // EBML magic: 0x1A45DFA3
    if magic[0..4] == [0x1A, 0x45, 0xDF, 0xA3] {
        return Ok(ContainerFormat::Matroska);
    }

    // TS/M2TS: 0x47 at offset 0 (raw TS) or offset 4 (M2TS).
    if magic[0] == 0x47 || magic[4] == 0x47 {
        match m2ts::ts_packet::detect_packet_format(reader) {
            Ok(m2ts::ts_packet::PacketFormat::M2ts) => return Ok(ContainerFormat::M2ts),
            Ok(m2ts::ts_packet::PacketFormat::RawTs) => {
                return Ok(ContainerFormat::TransportStream);
            }
            Err(_) => {}
        }
    }

    // SUP: raw PGS segments starting with "PG" magic (0x50, 0x47).
    if magic[0] == 0x50 && magic[1] == 0x47 {
        return Ok(ContainerFormat::Sup);
    }

    Err(PgsError::UnknownFormat)
}

/// Convert an MKV track to public track info.
fn mkv_track_to_info(
    t: &mkv::tracks::MkvPgsTrack,
    frame_counts: &HashMap<u64, u64>,
    cue_points: &Option<Vec<mkv::cues::PgsCuePoint>>,
) -> PgsTrackInfo {
    let has_cues = Some(
        cue_points
            .as_ref()
            .is_some_and(|cues| cues.iter().any(|cp| cp.track_number == t.track_number)),
    );
    PgsTrackInfo {
        track_id: t.track_number as u32,
        language: t.language.clone(),
        container: ContainerFormat::Matroska,
        name: t.name.clone(),
        flag_default: t.flag_default,
        flag_forced: t.flag_forced,
        display_set_count: t.track_uid.and_then(|uid| frame_counts.get(&uid).copied()),
        has_cues,
    }
}

/// Build synthetic track info for a .sup file (always a single track).
fn sup_track_info() -> PgsTrackInfo {
    PgsTrackInfo {
        track_id: 0,
        language: None,
        container: ContainerFormat::Sup,
        name: None,
        flag_default: None,
        flag_forced: None,
        display_set_count: None,
        has_cues: None,
    }
}

/// Convert an M2TS track to public track info.
fn m2ts_track_to_info(t: &m2ts::M2tsPgsTrack, format: ContainerFormat) -> PgsTrackInfo {
    PgsTrackInfo {
        track_id: t.pid as u32,
        language: t.language.clone(),
        container: format,
        name: None,
        flag_default: None,
        flag_forced: None,
        display_set_count: None,
        has_cues: None,
    }
}

/// List all PGS tracks in a container file.
pub fn list_pgs_tracks(path: &Path) -> Result<Vec<PgsTrackInfo>, PgsError> {
    let file = File::open(path)?;
    let mut reader = SeekBufReader::new(file);

    let format = detect_format(&mut reader)?;

    match format {
        ContainerFormat::Matroska => {
            let meta = mkv::prepare_mkv_metadata(&mut reader)?;
            Ok(meta
                .pgs_tracks
                .iter()
                .map(|t| mkv_track_to_info(t, &meta.frame_counts, &meta.cue_points))
                .collect())
        }
        ContainerFormat::M2ts | ContainerFormat::TransportStream => {
            let tracks = m2ts::list_pgs_tracks_m2ts(&mut reader, Some(path))?;
            Ok(tracks
                .iter()
                .map(|t| m2ts_track_to_info(t, format))
                .collect())
        }
        ContainerFormat::Sup => Ok(vec![sup_track_info()]),
    }
}

/// Extract all PGS Display Sets from all tracks in a container file.
///
/// Returns display sets grouped by track, with track metadata.
pub fn extract_all_display_sets(path: &Path) -> Result<Vec<TrackDisplaySets>, PgsError> {
    Extractor::open(path)?.collect_by_track()
}

/// Buffer size for M2TS sequential scanning (2 MB).
/// Larger buffers reduce OS-level read calls and improve NAS throughput.
const M2TS_BUF_SIZE: usize = 2 * 1024 * 1024;

/// Group a flat list of `TrackDisplaySet` into per-track `TrackDisplaySets`,
/// preserving insertion order of tracks.
fn group_by_track(
    results: Vec<TrackDisplaySet>,
    track_info_map: &HashMap<u32, PgsTrackInfo>,
) -> Vec<TrackDisplaySets> {
    let mut track_map: HashMap<u32, Vec<DisplaySet>> = HashMap::new();
    let mut track_order: Vec<u32> = Vec::new();

    for tds in results {
        let entry = track_map.entry(tds.track_id).or_insert_with(|| {
            track_order.push(tds.track_id);
            Vec::new()
        });
        entry.push(tds.display_set);
    }

    track_order
        .into_iter()
        .filter_map(|id| {
            let display_sets = track_map.remove(&id)?;
            if display_sets.is_empty() {
                return None;
            }
            let track = track_info_map.get(&id)?.clone();
            Some(TrackDisplaySets {
                track,
                display_sets,
            })
        })
        .collect()
}

/// Extract all PGS Display Sets from all tracks and return I/O statistics.
pub fn extract_all_display_sets_with_stats(
    path: &Path,
) -> Result<(Vec<TrackDisplaySets>, ExtractionStats), PgsError> {
    let mut extractor = Extractor::open(path)?;
    let track_info_map: HashMap<u32, PgsTrackInfo> = extractor
        .tracks()
        .iter()
        .map(|t| (t.track_id, t.clone()))
        .collect();

    let results = extractor.by_ref().collect::<Result<Vec<_>, _>>()?;
    let stats = extractor.stats().clone();
    let grouped = group_by_track(results, &track_info_map);

    Ok((grouped, stats))
}

/// Extract PGS Display Sets from a container file for a single track.
///
/// If `track_id` is `None`, extracts from the first PGS track found.
pub fn extract_display_sets(
    path: &Path,
    track_id: Option<u32>,
) -> Result<Vec<DisplaySet>, PgsError> {
    let (display_sets, _) = extract_display_sets_with_stats(path, track_id)?;
    Ok(display_sets)
}

/// Extract PGS Display Sets for a single track and return I/O statistics.
///
/// Same as `extract_display_sets`, but also returns `ExtractionStats`
/// with file size and bytes actually read — useful for benchmarking
/// and verifying the library's I/O efficiency.
pub fn extract_display_sets_with_stats(
    path: &Path,
    track_id: Option<u32>,
) -> Result<(Vec<DisplaySet>, ExtractionStats), PgsError> {
    let extractor = Extractor::open(path)?;
    let mut extractor = if let Some(id) = track_id {
        extractor.with_track_filter(&[id])
    } else {
        extractor
    };

    let target_id = track_id.or_else(|| extractor.tracks().first().map(|t| t.track_id));

    let mut display_sets = Vec::new();
    for result in extractor.by_ref() {
        let tds = result?;
        if target_id.is_none_or(|id| tds.track_id == id) {
            display_sets.push(tds.display_set);
        }
    }

    let stats = extractor.stats().clone();
    Ok((display_sets, stats))
}

/// Write Display Sets as a raw .sup file (concatenated PGS segments with headers).
pub fn write_sup_file(display_sets: &[DisplaySet], output: &Path) -> Result<(), PgsError> {
    let file = File::create(output)?;
    let mut writer = std::io::BufWriter::new(file);

    for ds in display_sets {
        for segment in &ds.segments {
            let bytes = segment.to_bytes();
            writer.write_all(&bytes)?;
        }
    }

    writer.flush()?;
    Ok(())
}
