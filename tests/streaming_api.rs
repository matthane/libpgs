//! Integration test: verify the streaming Extractor API against the batch API
//! using real media files across all extraction paths.
//!
//! Fixture files are expected in `tests/fixtures/` but are not tracked in git.
//! Tests are skipped at runtime if the fixtures are not present.

use std::collections::HashMap;
use std::path::Path;
use libpgs::pgs::segment::SegmentType;

const FIXTURES: &[&str] = &[
    "tests/fixtures/matroska-with-cues.mkv",
    "tests/fixtures/matroska-no-cues.mkv",
    "tests/fixtures/mpeg-transport-stream.m2ts",
    "tests/fixtures/mpeg-transport-stream-descriptors.m2ts",
    "tests/fixtures/raw-pgs.sup",
];

/// Returns only the fixture paths that exist on disk.
fn available_fixtures() -> Vec<&'static str> {
    FIXTURES.iter().copied().filter(|p| Path::new(p).exists()).collect()
}

/// Batch extraction baseline — used to compare against streaming results.
fn batch_baseline(path: &str) -> Vec<libpgs::TrackDisplaySets> {
    libpgs::extract_all_display_sets(Path::new(path))
        .expect("batch extraction should succeed")
}

#[test]
fn streaming_yields_all_display_sets() {
    let fixtures = available_fixtures();
    if fixtures.is_empty() { return; }

    for fixture in fixtures {
        let batch = batch_baseline(fixture);
        let batch_total: usize = batch.iter().map(|t| t.display_sets.len()).sum();

        // Stream every display set via the iterator.
        let extractor = libpgs::Extractor::open(fixture).expect("open");
        let mut count = 0usize;
        let mut track_counts: HashMap<u32, usize> = HashMap::new();

        for result in extractor {
            let tds = result.expect("streaming item should be Ok");
            count += 1;
            *track_counts.entry(tds.track_id).or_default() += 1;
        }

        // Total display set count must match batch.
        assert_eq!(
            count, batch_total,
            "{fixture}: streaming total ({count}) != batch total ({batch_total})"
        );

        // Per-track counts must match.
        for bt in &batch {
            let stream_count = track_counts.get(&bt.track.track_id).copied().unwrap_or(0);
            assert_eq!(
                stream_count,
                bt.display_sets.len(),
                "{fixture}: track {} mismatch: streaming={stream_count}, batch={}",
                bt.track.track_id,
                bt.display_sets.len()
            );
        }
    }
}

#[test]
fn streaming_segments_match_batch() {
    let fixtures = available_fixtures();
    if fixtures.is_empty() { return; }

    for fixture in fixtures {
        let batch = batch_baseline(fixture);

        let extractor = libpgs::Extractor::open(fixture).expect("open");
        let mut stream_segments: HashMap<u32, usize> = HashMap::new();

        for result in extractor {
            let tds = result.expect("streaming item should be Ok");
            *stream_segments.entry(tds.track_id).or_default() += tds.display_set.segments.len();
        }

        for bt in &batch {
            let batch_segs: usize = bt.display_sets.iter().map(|ds| ds.segments.len()).sum();
            let stream_segs = stream_segments.get(&bt.track.track_id).copied().unwrap_or(0);
            assert_eq!(
                stream_segs, batch_segs,
                "{fixture}: track {} segment mismatch: streaming={stream_segs}, batch={batch_segs}",
                bt.track.track_id
            );
        }
    }
}

#[test]
fn history_accumulates_correctly() {
    let fixtures = available_fixtures();
    if fixtures.is_empty() { return; }

    for fixture in fixtures {
        let mut extractor = libpgs::Extractor::open(fixture).expect("open");

        // Consume all display sets.
        let mut total = 0usize;
        while let Some(result) = extractor.next() {
            result.expect("should be Ok");
            total += 1;

            // History length should always match items yielded so far.
            assert_eq!(extractor.history().len(), total);
        }

        // history_for_track should partition correctly.
        let track_ids: Vec<u32> = extractor.tracks().iter().map(|t| t.track_id).collect();
        let mut sum = 0usize;
        for &tid in &track_ids {
            sum += extractor.history_for_track(tid).len();
        }
        assert_eq!(sum, total, "{fixture}: sum of per-track history != total history");
    }
}

#[test]
fn drain_history_clears_and_returns() {
    let fixtures = available_fixtures();
    if fixtures.is_empty() { return; }

    for fixture in fixtures {
        let mut extractor = libpgs::Extractor::open(fixture).expect("open");

        // Consume 10 items.
        for _ in 0..10 {
            extractor.next().expect("should have items").expect("Ok");
        }
        assert_eq!(extractor.history().len(), 10);

        // Drain and verify.
        let drained = extractor.drain_history();
        assert_eq!(drained.len(), 10);
        assert!(extractor.history().is_empty(), "history should be empty after drain");

        // Continue streaming — new items should accumulate fresh.
        let mut remaining = 0usize;
        for result in extractor.by_ref() {
            result.expect("Ok");
            remaining += 1;
        }
        assert_eq!(extractor.history().len(), remaining);
    }
}

#[test]
fn early_termination_with_take() {
    let fixtures = available_fixtures();
    if fixtures.is_empty() { return; }

    for fixture in fixtures {
        let mut extractor = libpgs::Extractor::open(fixture).expect("open");

        // Take only the first 5 display sets.
        let first_five: Vec<_> = extractor.by_ref().take(5).collect::<Result<Vec<_>, _>>().expect("Ok");
        assert_eq!(first_five.len(), 5);

        // Stats should show partial read — less than a full extraction.
        let partial_bytes = extractor.stats().bytes_read;
        assert!(partial_bytes > 0, "should have read some bytes");

        // History should have exactly 5.
        assert_eq!(extractor.history().len(), 5);
    }
}

#[test]
fn track_filter_restricts_output() {
    let fixtures = available_fixtures();
    if fixtures.is_empty() { return; }

    for fixture in fixtures {
        let batch = batch_baseline(fixture);
        // Pick one track.
        let target = &batch[0];
        let tid = target.track.track_id;

        let extractor = libpgs::Extractor::open(fixture)
            .expect("open")
            .with_track_filter(&[tid]);

        let mut count = 0usize;
        let mut seg_count = 0usize;
        for result in extractor {
            let tds = result.expect("Ok");
            assert_eq!(tds.track_id, tid, "should only yield filtered track");
            count += 1;
            seg_count += tds.display_set.segments.len();
        }

        let batch_segs: usize = target.display_sets.iter().map(|ds| ds.segments.len()).sum();
        assert_eq!(count, target.display_sets.len(), "{fixture}: display set count mismatch for filtered track");
        assert_eq!(seg_count, batch_segs, "{fixture}: segment count mismatch for filtered track");
    }
}

#[test]
fn stats_update_during_streaming() {
    let fixtures = available_fixtures();
    if fixtures.is_empty() { return; }

    for fixture in fixtures {
        let mut extractor = libpgs::Extractor::open(fixture).expect("open");

        let initial_bytes = extractor.stats().bytes_read;

        // Read one item.
        extractor.next().expect("should have items").expect("Ok");
        let after_one = extractor.stats().bytes_read;
        assert!(
            after_one > initial_bytes,
            "bytes_read should increase after yielding a display set"
        );

        // Exhaust remaining.
        for result in extractor.by_ref() {
            result.expect("Ok");
        }
        let final_bytes = extractor.stats().bytes_read;
        assert!(
            final_bytes >= after_one,
            "bytes_read should not decrease"
        );
        assert!(
            extractor.stats().file_size > 0,
            "file_size should be set"
        );
    }
}

#[test]
fn collect_by_track_matches_batch() {
    let fixtures = available_fixtures();
    if fixtures.is_empty() { return; }

    for fixture in fixtures {
        let batch = batch_baseline(fixture);
        let collected = libpgs::Extractor::open(fixture)
            .expect("open")
            .collect_by_track()
            .expect("collect_by_track should succeed");

        assert_eq!(collected.len(), batch.len(), "{fixture}: track count mismatch");

        let batch_map: HashMap<u32, usize> = batch
            .iter()
            .map(|t| (t.track.track_id, t.display_sets.len()))
            .collect();

        for t in &collected {
            let expected = batch_map.get(&t.track.track_id).copied().unwrap_or(0);
            assert_eq!(
                t.display_sets.len(),
                expected,
                "{fixture}: collect_by_track: track {} has {} ds, batch has {}",
                t.track.track_id,
                t.display_sets.len(),
                expected
            );
        }
    }
}

#[test]
fn extraction_summary() {
    let fixtures = available_fixtures();
    if fixtures.is_empty() { return; }

    for fixture in fixtures {
        let (by_track, stats) = libpgs::extract_all_display_sets_with_stats(Path::new(fixture))
            .expect("extraction with stats should succeed");

        println!("\n============================================================");
        println!("  {fixture}");
        println!("  File size: {:.2} MB", stats.file_size as f64 / 1_048_576.0);
        println!("  Bytes read: {:.2} MB ({:.1}%)",
            stats.bytes_read as f64 / 1_048_576.0,
            stats.bytes_read as f64 / stats.file_size as f64 * 100.0,
        );
        println!("  Tracks: {}", by_track.len());

        let mut total_ds = 0usize;
        let mut total_segs = 0usize;

        for track in &by_track {
            let ds_count = track.display_sets.len();
            let seg_count: usize = track.display_sets.iter().map(|ds| ds.segments.len()).sum();
            total_ds += ds_count;
            total_segs += seg_count;

            let first_pts = track.display_sets.first().map(|ds| ds.pts_ms).unwrap_or(0.0);
            let last_pts = track.display_sets.last().map(|ds| ds.pts_ms).unwrap_or(0.0);

            let mut seg_types: HashMap<&str, usize> = HashMap::new();
            for ds in &track.display_sets {
                for seg in &ds.segments {
                    let name = match seg.segment_type {
                        SegmentType::PresentationComposition => "PCS",
                        SegmentType::WindowDefinition => "WDS",
                        SegmentType::PaletteDefinition => "PDS",
                        SegmentType::ObjectDefinition => "ODS",
                        SegmentType::EndOfDisplaySet => "END",
                    };
                    *seg_types.entry(name).or_default() += 1;
                }
            }

            let lang = track.track.language.as_deref().unwrap_or("unknown");
            println!("\n  Track {} ({lang}):", track.track.track_id);
            println!("    Display sets: {ds_count}");
            println!("    Segments: {seg_count}");
            println!("    PTS range: {first_pts:.3}ms - {last_pts:.3}ms");
            println!("    Segment types: PCS={} WDS={} PDS={} ODS={} END={}",
                seg_types.get("PCS").unwrap_or(&0),
                seg_types.get("WDS").unwrap_or(&0),
                seg_types.get("PDS").unwrap_or(&0),
                seg_types.get("ODS").unwrap_or(&0),
                seg_types.get("END").unwrap_or(&0),
            );
        }

        println!("\n  Totals: {total_ds} display sets, {total_segs} segments");
        println!("============================================================\n");
    }
}

#[test]
fn all_fixtures_produce_same_totals() {
    let fixtures = available_fixtures();
    if fixtures.len() < 2 { return; }

    // Collect totals per fixture: (display sets, segments, track count, per-track ds counts sorted)
    let mut results: Vec<(&str, usize, usize, usize, Vec<usize>)> = Vec::new();

    for fixture in &fixtures {
        let by_track = batch_baseline(fixture);
        let track_count = by_track.len();
        let total_ds: usize = by_track.iter().map(|t| t.display_sets.len()).sum();
        let total_segs: usize = by_track.iter()
            .map(|t| t.display_sets.iter().map(|ds| ds.segments.len()).sum::<usize>())
            .sum();
        let mut per_track: Vec<usize> = by_track.iter()
            .map(|t| t.display_sets.len())
            .collect();
        per_track.sort();

        results.push((fixture, total_ds, total_segs, track_count, per_track));
    }

    let (ref_fixture, ref_ds, ref_segs, ref_tracks, ref_per_track) = &results[0];

    for (fixture, total_ds, total_segs, track_count, per_track) in &results[1..] {
        assert_eq!(
            track_count, ref_tracks,
            "{fixture} has {track_count} tracks, but {ref_fixture} has {ref_tracks}"
        );
        assert_eq!(
            total_ds, ref_ds,
            "{fixture} has {total_ds} display sets, but {ref_fixture} has {ref_ds}"
        );
        assert_eq!(
            total_segs, ref_segs,
            "{fixture} has {total_segs} segments, but {ref_fixture} has {ref_segs}"
        );
        assert_eq!(
            per_track, ref_per_track,
            "{fixture} per-track display set distribution differs from {ref_fixture}"
        );
    }
}

/// Roundtrip test: extract MKV → .sup via write_sup_file, then re-read via Extractor.
#[test]
fn sup_roundtrip_from_mkv() {
    let mkv = "tests/fixtures/matroska-with-cues.mkv";
    if !Path::new(mkv).exists() { return; }

    let batch = batch_baseline(mkv);
    assert!(!batch.is_empty(), "should have at least one track");

    // Write first track to a temp .sup file.
    let sup_path = std::env::temp_dir().join("libpgs_test_roundtrip.sup");
    let source = &batch[0];
    libpgs::write_sup_file(&source.display_sets, &sup_path).expect("write_sup_file");

    // Re-read via Extractor.
    let extractor = libpgs::Extractor::open(&sup_path).expect("open .sup");
    assert_eq!(extractor.tracks().len(), 1);
    assert_eq!(extractor.tracks()[0].track_id, 0);
    assert_eq!(extractor.tracks()[0].container, libpgs::ContainerFormat::Sup);

    let mut ds_count = 0usize;
    let mut seg_count = 0usize;
    for result in extractor {
        let tds = result.expect("streaming item should be Ok");
        assert_eq!(tds.track_id, 0);
        ds_count += 1;
        seg_count += tds.display_set.segments.len();
    }

    assert_eq!(
        ds_count,
        source.display_sets.len(),
        "roundtrip display set count mismatch"
    );

    let source_segs: usize = source.display_sets.iter().map(|ds| ds.segments.len()).sum();
    assert_eq!(seg_count, source_segs, "roundtrip segment count mismatch");

    // Verify PTS values match.
    let extractor2 = libpgs::Extractor::open(&sup_path).expect("reopen .sup");
    for (i, (result, orig_ds)) in extractor2.zip(source.display_sets.iter()).enumerate() {
        let tds = result.expect("Ok");
        assert_eq!(
            tds.display_set.pts, orig_ds.pts,
            "roundtrip PTS mismatch at display set {i}"
        );
        assert_eq!(
            tds.display_set.segments.len(), orig_ds.segments.len(),
            "roundtrip segment count mismatch at display set {i}"
        );
    }

    let _ = std::fs::remove_file(&sup_path);
}
