pub mod error;
pub mod pgs;
pub mod ebml;
pub mod mkv;
pub mod m2ts;
pub mod io;

use error::PgsError;
use io::SeekBufReader;
use mkv::stream::MkvExtractorState;
use m2ts::stream::M2tsExtractorState;
use pgs::DisplaySet;
use std::collections::HashMap;
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};

/// Container format of the source file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContainerFormat {
    Matroska,
    M2ts,
    TransportStream,
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
    Mkv(MkvExtractorState),
    M2ts(M2tsExtractorState),
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
                let tracks: Vec<PgsTrackInfo> = meta.pgs_tracks.iter()
                    .map(|t| PgsTrackInfo {
                        track_id: t.track_number as u32,
                        language: t.language.clone(),
                        container: ContainerFormat::Matroska,
                    })
                    .collect();

                let mut state = MkvExtractorState::new(
                    reader,
                    path.to_path_buf(),
                    meta,
                    None,
                )?;
                state.init_source()?;

                Ok(Extractor {
                    inner: ExtractorInner::Mkv(state),
                    catalog: Vec::new(),
                    tracks,
                    stats: ExtractionStats { file_size, bytes_read: 0 },
                    path: path.to_path_buf(),
                    format: ContainerFormat::Matroska,
                })
            }
            format @ (ContainerFormat::M2ts | ContainerFormat::TransportStream) => {
                // Reopen with large buffer for M2TS throughput.
                let file = File::open(path)?;
                let mut reader = SeekBufReader::with_capacity(M2TS_BUF_SIZE, file);
                detect_format(&mut reader)?;

                let meta = m2ts::prepare_m2ts_metadata(&mut reader, Some(path))?;
                let tracks: Vec<PgsTrackInfo> = meta.tracks.iter()
                    .map(|t| PgsTrackInfo {
                        track_id: t.pid as u32,
                        language: t.language.clone(),
                        container: format,
                    })
                    .collect();

                let state = M2tsExtractorState::new(reader, meta, format, None);

                Ok(Extractor {
                    inner: ExtractorInner::M2ts(state),
                    catalog: Vec::new(),
                    tracks,
                    stats: ExtractionStats { file_size, bytes_read: 0 },
                    path: path.to_path_buf(),
                    format,
                })
            }
        }
    }

    /// Restrict extraction to specific tracks. Chainable.
    ///
    /// Must be called before the first call to `next()`. Configures the
    /// internal state machine to only create assemblers for matching tracks
    /// and skip non-matching blocks at the source level.
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

        // Reconstruct with the filter applied. Reopens the file and
        // re-parses metadata so the state machine is initialized with
        // only the requested tracks from the start.
        match Self::open_filtered(&path, file_size, format, track_ids) {
            Ok(ext) => ext,
            Err(_) => self,
        }
    }

    fn open_filtered(
        path: &Path,
        file_size: u64,
        format: ContainerFormat,
        track_ids: &[u32],
    ) -> Result<Self, PgsError> {
        match format {
            ContainerFormat::Matroska => {
                let file = File::open(path)?;
                let mut reader = SeekBufReader::new(file);
                detect_format(&mut reader)?;

                let meta = mkv::prepare_mkv_metadata(&mut reader)?;
                let tracks: Vec<PgsTrackInfo> = meta.pgs_tracks.iter()
                    .filter(|t| track_ids.contains(&(t.track_number as u32)))
                    .map(|t| PgsTrackInfo {
                        track_id: t.track_number as u32,
                        language: t.language.clone(),
                        container: ContainerFormat::Matroska,
                    })
                    .collect();

                let mut state = MkvExtractorState::new(
                    reader,
                    path.to_path_buf(),
                    meta,
                    Some(track_ids),
                )?;
                state.init_source()?;

                Ok(Extractor {
                    inner: ExtractorInner::Mkv(state),
                    catalog: Vec::new(),
                    tracks,
                    stats: ExtractionStats { file_size, bytes_read: 0 },
                    path: path.to_path_buf(),
                    format: ContainerFormat::Matroska,
                })
            }
            fmt @ (ContainerFormat::M2ts | ContainerFormat::TransportStream) => {
                let file = File::open(path)?;
                let mut reader = SeekBufReader::with_capacity(M2TS_BUF_SIZE, file);
                detect_format(&mut reader)?;

                let meta = m2ts::prepare_m2ts_metadata(&mut reader, Some(path))?;
                let tracks: Vec<PgsTrackInfo> = meta.tracks.iter()
                    .filter(|t| track_ids.contains(&(t.pid as u32)))
                    .map(|t| PgsTrackInfo {
                        track_id: t.pid as u32,
                        language: t.language.clone(),
                        container: fmt,
                    })
                    .collect();

                let state = M2tsExtractorState::new(reader, meta, fmt, Some(track_ids));

                Ok(Extractor {
                    inner: ExtractorInner::M2ts(state),
                    catalog: Vec::new(),
                    tracks,
                    stats: ExtractionStats { file_size, bytes_read: 0 },
                    path: path.to_path_buf(),
                    format: fmt,
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
        self.catalog.iter()
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
        if let ExtractorInner::Mkv(ref state) = self.inner {
            if let Some(result) = state.try_collect_parallel() {
                return result;
            }
        }

        // Sequential drain.
        let mut track_map: HashMap<u32, (PgsTrackInfo, Vec<DisplaySet>)> = HashMap::new();
        let mut track_order: Vec<u32> = Vec::new();

        for result in self.by_ref() {
            let tds = result?;
            let entry = track_map.entry(tds.track_id).or_insert_with(|| {
                track_order.push(tds.track_id);
                (PgsTrackInfo {
                    track_id: tds.track_id,
                    language: tds.language.clone(),
                    container: tds.container,
                }, Vec::new())
            });
            entry.1.push(tds.display_set);
        }

        Ok(track_order.into_iter()
            .filter_map(|id| {
                let (info, display_sets) = track_map.remove(&id)?;
                if display_sets.is_empty() { return None; }
                Some(TrackDisplaySets { track: info, display_sets })
            })
            .collect())
    }

    /// Update stats from the inner reader.
    fn update_stats(&mut self) {
        self.stats.bytes_read = match &self.inner {
            ExtractorInner::Mkv(state) => state.bytes_read(),
            ExtractorInner::M2ts(state) => state.bytes_read(),
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
            Ok(m2ts::ts_packet::PacketFormat::RawTs) => return Ok(ContainerFormat::TransportStream),
            Err(_) => {}
        }
    }

    Err(PgsError::UnknownFormat)
}

/// List all PGS tracks in a container file.
pub fn list_pgs_tracks(path: &Path) -> Result<Vec<PgsTrackInfo>, PgsError> {
    let file = File::open(path)?;
    let mut reader = SeekBufReader::new(file);

    let format = detect_format(&mut reader)?;

    match format {
        ContainerFormat::Matroska => {
            let tracks = mkv::list_pgs_tracks_mkv(&mut reader)?;
            Ok(tracks
                .into_iter()
                .map(|t| PgsTrackInfo {
                    track_id: t.track_number as u32,
                    language: t.language,
                    container: ContainerFormat::Matroska,
                })
                .collect())
        }
        ContainerFormat::M2ts | ContainerFormat::TransportStream => {
            let tracks = m2ts::list_pgs_tracks_m2ts(&mut reader, Some(path))?;
            Ok(tracks
                .into_iter()
                .map(|t| PgsTrackInfo {
                    track_id: t.pid as u32,
                    language: t.language,
                    container: format,
                })
                .collect())
        }
    }
}

/// Extract all PGS Display Sets from all tracks in a container file.
///
/// Returns display sets grouped by track, with track metadata.
pub fn extract_all_display_sets(
    path: &Path,
) -> Result<Vec<TrackDisplaySets>, PgsError> {
    Extractor::open(path)?.collect_by_track()
}

/// Buffer size for M2TS sequential scanning (2 MB).
/// Larger buffers reduce OS-level read calls and improve NAS throughput.
const M2TS_BUF_SIZE: usize = 2 * 1024 * 1024;

/// Extract all PGS Display Sets from all tracks and return I/O statistics.
pub fn extract_all_display_sets_with_stats(
    path: &Path,
) -> Result<(Vec<TrackDisplaySets>, ExtractionStats), PgsError> {
    let mut extractor = Extractor::open(path)?;
    let results = extractor.by_ref().collect::<Result<Vec<_>, _>>()?;
    let stats = extractor.stats().clone();

    // Group by track.
    let mut track_map: HashMap<u32, (PgsTrackInfo, Vec<DisplaySet>)> = HashMap::new();
    let mut track_order: Vec<u32> = Vec::new();

    for tds in results {
        let entry = track_map.entry(tds.track_id).or_insert_with(|| {
            track_order.push(tds.track_id);
            (PgsTrackInfo {
                track_id: tds.track_id,
                language: tds.language.clone(),
                container: tds.container,
            }, Vec::new())
        });
        entry.1.push(tds.display_set);
    }

    let grouped = track_order.into_iter()
        .filter_map(|id| {
            let (info, display_sets) = track_map.remove(&id)?;
            if display_sets.is_empty() { return None; }
            Some(TrackDisplaySets { track: info, display_sets })
        })
        .collect();

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
        if target_id.map_or(true, |id| tds.track_id == id) {
            display_sets.push(tds.display_set);
        }
    }

    let stats = extractor.stats().clone();
    Ok((display_sets, stats))
}

/// Write Display Sets as a raw .sup file (concatenated PGS segments with headers).
pub fn write_sup_file(
    display_sets: &[DisplaySet],
    output: &Path,
) -> Result<(), PgsError> {
    let mut file = File::create(output)?;

    for ds in display_sets {
        for segment in &ds.segments {
            let bytes = segment.to_bytes();
            file.write_all(&bytes)?;
        }
    }

    file.flush()?;
    Ok(())
}
