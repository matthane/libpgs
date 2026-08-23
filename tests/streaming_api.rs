//! Integration test: verify the streaming Extractor API against the batch API
//! using real media files across all extraction paths.
//!
//! Fixture files are expected in `tests/fixtures/` but are not tracked in git.
//! Tests are skipped at runtime if the fixtures are not present.

use std::collections::HashMap;
use std::path::Path;

const FIXTURES: &[&str] = &[
    "tests/fixtures/matroska-with-cues.mkv",
    "tests/fixtures/matroska-no-cues.mkv",
    "tests/fixtures/mpeg-transport-stream.m2ts",
    "tests/fixtures/mpeg-transport-stream-descriptors.m2ts",
];

fn available_fixtures() -> Vec<&'static str> {
    FIXTURES
        .iter()
        .copied()
        .filter(|p| Path::new(p).exists())
        .collect()
}

/// Batch extraction baseline, used to compare against streaming results.
fn batch_baseline(path: &str) -> Vec<libpgs::TrackDisplaySets> {
    libpgs::extract_all_display_sets(Path::new(path)).expect("batch extraction should succeed")
}

#[test]
fn streaming_matches_batch_totals() {
    let fixtures = available_fixtures();
    if fixtures.is_empty() {
        return;
    }

    for fixture in fixtures {
        let batch = batch_baseline(fixture);
        let batch_total: usize = batch.iter().map(|t| t.display_sets.len()).sum();

        let extractor = libpgs::Extractor::open(fixture).expect("open");
        let mut count = 0usize;
        let mut track_counts: HashMap<u32, usize> = HashMap::new();
        let mut track_segments: HashMap<u32, usize> = HashMap::new();

        for result in extractor {
            let tds = result.expect("streaming item should be Ok");
            count += 1;
            *track_counts.entry(tds.track_id).or_default() += 1;
            *track_segments.entry(tds.track_id).or_default() += tds.display_set.segments.len();
        }

        assert_eq!(
            count, batch_total,
            "{fixture}: streaming total ({count}) != batch total ({batch_total})"
        );

        for bt in &batch {
            let stream_count = track_counts.get(&bt.track.track_id).copied().unwrap_or(0);
            assert_eq!(
                stream_count,
                bt.display_sets.len(),
                "{fixture}: track {} display set mismatch: streaming={stream_count}, batch={}",
                bt.track.track_id,
                bt.display_sets.len()
            );

            let batch_segs: usize = bt.display_sets.iter().map(|ds| ds.segments.len()).sum();
            let stream_segs = track_segments
                .get(&bt.track.track_id)
                .copied()
                .unwrap_or(0);
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
    if fixtures.is_empty() {
        return;
    }

    for fixture in fixtures {
        let mut extractor = libpgs::Extractor::open(fixture).expect("open");

        let mut total = 0usize;
        while let Some(result) = extractor.next() {
            result.expect("should be Ok");
            total += 1;

            assert_eq!(extractor.history().len(), total);
        }

        let track_ids: Vec<u32> = extractor.tracks().iter().map(|t| t.track_id).collect();
        let mut sum = 0usize;
        for &tid in &track_ids {
            sum += extractor.history_for_track(tid).len();
        }
        assert_eq!(
            sum, total,
            "{fixture}: sum of per-track history != total history"
        );
    }
}

#[test]
fn with_history_false_skips_catalog_but_yields_same_data() {
    let fixtures = available_fixtures();
    if fixtures.is_empty() {
        return;
    }

    for fixture in fixtures {
        // Control: default (history on).
        let control: Vec<(u32, u64)> = libpgs::Extractor::open(fixture)
            .expect("open")
            .map(|r| r.expect("ok"))
            .map(|tds| (tds.track_id, tds.display_set.pts))
            .collect();

        let mut ext = libpgs::Extractor::open(fixture)
            .expect("open")
            .with_history(false);
        let mut seen: Vec<(u32, u64)> = Vec::new();
        while let Some(r) = ext.next() {
            let tds = r.expect("ok");
            seen.push((tds.track_id, tds.display_set.pts));
            assert!(
                ext.history().is_empty(),
                "{fixture}: history must stay empty when disabled"
            );
        }
        assert!(ext.drain_history().is_empty());
        ext.clear_history();
        let mut seen_sorted = seen.clone();
        let mut control_sorted = control.clone();
        seen_sorted.sort();
        control_sorted.sort();
        assert_eq!(
            seen_sorted, control_sorted,
            "{fixture}: yielded data must match regardless of history flag"
        );
    }
}

#[test]
fn drain_history_clears_and_returns() {
    let fixtures = available_fixtures();
    if fixtures.is_empty() {
        return;
    }

    for fixture in fixtures {
        let mut extractor = libpgs::Extractor::open(fixture).expect("open");

        for _ in 0..10 {
            extractor.next().expect("should have items").expect("Ok");
        }
        assert_eq!(extractor.history().len(), 10);

        let drained = extractor.drain_history();
        assert_eq!(drained.len(), 10);
        assert!(
            extractor.history().is_empty(),
            "history should be empty after drain"
        );

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
    if fixtures.is_empty() {
        return;
    }

    for fixture in fixtures {
        let mut extractor = libpgs::Extractor::open(fixture).expect("open");

        let first_five: Vec<_> = extractor
            .by_ref()
            .take(5)
            .collect::<Result<Vec<_>, _>>()
            .expect("Ok");
        assert_eq!(first_five.len(), 5);

        let partial_bytes = extractor.stats().bytes_read;
        assert!(partial_bytes > 0, "should have read some bytes");

        assert_eq!(extractor.history().len(), 5);
    }
}

#[test]
fn track_filter_restricts_output() {
    let fixtures = available_fixtures();
    if fixtures.is_empty() {
        return;
    }

    for fixture in fixtures {
        let batch = batch_baseline(fixture);
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
        assert_eq!(
            count,
            target.display_sets.len(),
            "{fixture}: display set count mismatch for filtered track"
        );
        assert_eq!(
            seg_count, batch_segs,
            "{fixture}: segment count mismatch for filtered track"
        );
    }
}

#[test]
fn stats_update_during_streaming() {
    let fixtures = available_fixtures();
    if fixtures.is_empty() {
        return;
    }

    for fixture in fixtures {
        let mut extractor = libpgs::Extractor::open(fixture).expect("open");

        let initial_bytes = extractor.stats().bytes_read;

        extractor.next().expect("should have items").expect("Ok");
        let after_one = extractor.stats().bytes_read;
        assert!(
            after_one > initial_bytes,
            "bytes_read should increase after yielding a display set"
        );

        for result in extractor.by_ref() {
            result.expect("Ok");
        }
        let final_bytes = extractor.stats().bytes_read;
        assert!(final_bytes >= after_one, "bytes_read should not decrease");
        assert!(extractor.stats().file_size > 0, "file_size should be set");
    }
}

#[test]
fn collect_by_track_matches_batch() {
    let fixtures = available_fixtures();
    if fixtures.is_empty() {
        return;
    }

    for fixture in fixtures {
        let batch = batch_baseline(fixture);
        let collected = libpgs::Extractor::open(fixture)
            .expect("open")
            .collect_by_track()
            .expect("collect_by_track should succeed");

        assert_eq!(
            collected.len(),
            batch.len(),
            "{fixture}: track count mismatch"
        );

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
fn all_fixtures_produce_same_totals() {
    let fixtures = available_fixtures();
    if fixtures.len() < 2 {
        return;
    }

    // Collect totals per fixture: (display sets, segments, track count, per-track ds counts sorted)
    let mut results: Vec<(&str, usize, usize, usize, Vec<usize>)> = Vec::new();

    for fixture in &fixtures {
        let by_track = batch_baseline(fixture);
        let track_count = by_track.len();
        let total_ds: usize = by_track.iter().map(|t| t.display_sets.len()).sum();
        let total_segs: usize = by_track
            .iter()
            .map(|t| {
                t.display_sets
                    .iter()
                    .map(|ds| ds.segments.len())
                    .sum::<usize>()
            })
            .sum();
        let mut per_track: Vec<usize> = by_track.iter().map(|t| t.display_sets.len()).collect();
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

/// Roundtrip test: extract MKV -> .sup via write_sup_file, then re-read via Extractor.
#[test]
fn sup_roundtrip_from_mkv() {
    let mkv = "tests/fixtures/matroska-with-cues.mkv";
    if !Path::new(mkv).exists() {
        return;
    }

    let batch = batch_baseline(mkv);
    assert!(!batch.is_empty(), "should have at least one track");

    let sup_path = std::env::temp_dir().join("libpgs_test_roundtrip.sup");
    let source = &batch[0];
    libpgs::write_sup_file(&source.display_sets, &sup_path).expect("write_sup_file");

    let extractor = libpgs::Extractor::open(&sup_path).expect("open .sup");
    assert_eq!(extractor.tracks().len(), 1);
    assert_eq!(extractor.tracks()[0].track_id, 0);
    assert_eq!(
        extractor.tracks()[0].container,
        libpgs::ContainerFormat::Sup
    );

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

    let extractor2 = libpgs::Extractor::open(&sup_path).expect("reopen .sup");
    for (i, (result, orig_ds)) in extractor2.zip(source.display_sets.iter()).enumerate() {
        let tds = result.expect("Ok");
        assert_eq!(
            tds.display_set.pts, orig_ds.pts,
            "roundtrip PTS mismatch at display set {i}"
        );
        assert_eq!(
            tds.display_set.segments.len(),
            orig_ds.segments.len(),
            "roundtrip segment count mismatch at display set {i}"
        );
    }

    let _ = std::fs::remove_file(&sup_path);
}
