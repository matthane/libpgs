//! Integration test: verify the `stream | encode` round-trip.
//!
//! Runs `libpgs stream` on test fixtures, pipes the NDJSON to `libpgs encode`,
//! then re-reads the resulting .sup file and compares structural properties.
//!
//! Fixture files are expected in `tests/fixtures/` but are not tracked in git.
//! Tests are skipped at runtime if the fixtures are not present.

use std::path::Path;
use std::process::{Command, Stdio};

const FIXTURES: &[&str] = &[
    "tests/fixtures/matroska-with-cues.mkv",
    "tests/fixtures/matroska-no-cues.mkv",
    "tests/fixtures/mpeg-transport-stream.m2ts",
    "tests/fixtures/mpeg-transport-stream-descriptors.m2ts",
    "tests/fixtures/raw-pgs.sup",
];

fn available_fixtures() -> Vec<&'static str> {
    FIXTURES
        .iter()
        .copied()
        .filter(|p| Path::new(p).exists())
        .collect()
}

/// Run `libpgs stream` and return stdout bytes.
fn stream_fixture(fixture: &str) -> Vec<u8> {
    let binary = env!("CARGO_BIN_EXE_libpgs");
    let output = Command::new(binary)
        .arg("stream")
        .arg(fixture)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to run libpgs stream");
    assert!(
        output.status.success(),
        "libpgs stream failed on {fixture}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

/// Run `libpgs encode -o <path>` with given stdin bytes.
fn encode_to_file(ndjson: &[u8], output_path: &Path) {
    let binary = env!("CARGO_BIN_EXE_libpgs");
    let child = Command::new(binary)
        .arg("encode")
        .arg("-o")
        .arg(output_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to start libpgs encode");

    use std::io::Write;
    let mut child = child;
    child.stdin.take().unwrap().write_all(ndjson).unwrap();
    let output = child.wait_with_output().expect("failed to wait on encode");
    assert!(
        output.status.success(),
        "libpgs encode failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn stream_encode_roundtrip_preserves_structure() {
    let fixtures = available_fixtures();
    if fixtures.is_empty() {
        return;
    }

    for fixture in fixtures {
        // Step 1: Extract display sets from original file via API.
        let original_tracks =
            libpgs::Extractor::open(Path::new(fixture))
                .unwrap()
                .collect_by_track()
                .unwrap();

        let original_total: usize = original_tracks.iter().map(|t| t.display_sets.len()).sum();
        if original_total == 0 {
            continue;
        }

        // Step 2: Stream to NDJSON.
        let ndjson = stream_fixture(fixture);

        // Step 3: Encode NDJSON to .sup file.
        let tmp_dir = std::env::temp_dir();
        let fixture_stem = Path::new(fixture)
            .file_stem()
            .unwrap()
            .to_string_lossy();

        // If multi-track, encode produces separate files.
        // Use a single output path — encode will split if needed.
        let sup_path = tmp_dir.join(format!("libpgs_test_{fixture_stem}.sup"));
        encode_to_file(&ndjson, &sup_path);

        // Step 4: Re-read the .sup file(s) and compare.
        // For multi-track sources, encode creates <stem>_track<id>.sup files.
        if original_tracks.len() == 1 {
            // Single track: output is at sup_path directly.
            assert!(
                sup_path.exists(),
                "{fixture}: encoded .sup file not found at {}",
                sup_path.display()
            );
            let re_extracted = libpgs::Extractor::open(&sup_path)
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap();

            let orig_ds = &original_tracks[0].display_sets;
            assert_eq!(
                re_extracted.len(),
                orig_ds.len(),
                "{fixture}: display set count mismatch"
            );

            // Compare each display set.
            for (i, (re, orig)) in re_extracted.iter().zip(orig_ds.iter()).enumerate() {
                assert_eq!(
                    re.display_set.pts, orig.pts,
                    "{fixture} ds[{i}]: PTS mismatch"
                );
                assert_eq!(
                    re.display_set.composition_state, orig.composition_state,
                    "{fixture} ds[{i}]: composition state mismatch"
                );

                // Compare palettes.
                let re_pds: Vec<_> = re.display_set.segments.iter().filter_map(|s| s.parse_pds()).collect();
                let orig_pds: Vec<_> = orig.segments.iter().filter_map(|s| s.parse_pds()).collect();
                assert_eq!(
                    re_pds.len(),
                    orig_pds.len(),
                    "{fixture} ds[{i}]: palette count mismatch"
                );
                for (pi, (rp, op)) in re_pds.iter().zip(orig_pds.iter()).enumerate() {
                    assert_eq!(
                        rp.entries.len(),
                        op.entries.len(),
                        "{fixture} ds[{i}] palette[{pi}]: entry count mismatch"
                    );
                    for (ei, (re_entry, orig_entry)) in rp.entries.iter().zip(op.entries.iter()).enumerate() {
                        assert_eq!(
                            re_entry.id, orig_entry.id,
                            "{fixture} ds[{i}] palette[{pi}] entry[{ei}]: id mismatch"
                        );
                        assert_eq!(
                            re_entry.luminance, orig_entry.luminance,
                            "{fixture} ds[{i}] palette[{pi}] entry[{ei}]: luminance mismatch"
                        );
                        assert_eq!(
                            re_entry.alpha, orig_entry.alpha,
                            "{fixture} ds[{i}] palette[{pi}] entry[{ei}]: alpha mismatch"
                        );
                    }
                }

                // Compare object bitmaps (decode RLE from both and compare pixels).
                let re_ods: Vec<_> = re.display_set.segments.iter().filter_map(|s| s.parse_ods()).collect();
                let orig_ods: Vec<_> = orig.segments.iter().filter_map(|s| s.parse_ods()).collect();

                // Original may have fragmented ODS; re-encoded will have fresh fragmentation.
                // Compare by grouping by object ID and decoding the complete bitmaps.
                let re_bitmaps = collect_bitmaps(&re_ods);
                let orig_bitmaps = collect_bitmaps(&orig_ods);

                assert_eq!(
                    re_bitmaps.len(),
                    orig_bitmaps.len(),
                    "{fixture} ds[{i}]: object count mismatch"
                );
                for (obj_id, re_pixels) in &re_bitmaps {
                    let orig_pixels = orig_bitmaps
                        .iter()
                        .find(|(id, _)| id == obj_id)
                        .map(|(_, p)| p)
                        .unwrap_or_else(|| panic!("{fixture} ds[{i}]: missing object {obj_id}"));
                    assert_eq!(
                        re_pixels.len(),
                        orig_pixels.len(),
                        "{fixture} ds[{i}] object {obj_id}: pixel count mismatch"
                    );
                    assert_eq!(
                        re_pixels, orig_pixels,
                        "{fixture} ds[{i}] object {obj_id}: pixel data mismatch"
                    );
                }
            }

            // Clean up.
            let _ = std::fs::remove_file(&sup_path);
        } else {
            // Multi-track: verify each track's file exists and has correct count.
            let stem = sup_path.file_stem().unwrap().to_string_lossy().to_string();
            let ext = sup_path
                .extension()
                .unwrap()
                .to_string_lossy()
                .to_string();
            let parent = sup_path.parent().unwrap();

            for orig_track in &original_tracks {
                let track_path =
                    parent.join(format!("{}_track{}.{}", stem, orig_track.track.track_id, ext));
                assert!(
                    track_path.exists(),
                    "{fixture}: missing track file {}",
                    track_path.display()
                );
                let re_extracted = libpgs::Extractor::open(&track_path)
                    .unwrap()
                    .collect::<Result<Vec<_>, _>>()
                    .unwrap();
                assert_eq!(
                    re_extracted.len(),
                    orig_track.display_sets.len(),
                    "{fixture} track {}: display set count mismatch",
                    orig_track.track.track_id
                );

                // Sort both by PTS for comparison — collect_by_track() may use
                // parallel extraction which can reorder display sets relative to
                // the sequential stream command.
                let mut re_pts: Vec<u64> = re_extracted.iter().map(|r| r.display_set.pts).collect();
                let mut orig_pts: Vec<u64> = orig_track.display_sets.iter().map(|d| d.pts).collect();
                re_pts.sort();
                orig_pts.sort();

                for (i, (re_pt, orig_pt)) in re_pts.iter().zip(orig_pts.iter()).enumerate() {
                    assert_eq!(
                        re_pt, orig_pt,
                        "{fixture} track {} ds[{i}]: PTS mismatch (sorted)",
                        orig_track.track.track_id
                    );
                }

                let _ = std::fs::remove_file(&track_path);
            }
            // Also try to remove the base path in case it was created.
            let _ = std::fs::remove_file(&sup_path);
        }
    }
}

/// Collect decoded bitmaps grouped by object ID.
/// For fragmented objects, concatenate RLE data from all fragments of the same ID.
fn collect_bitmaps(ods_list: &[libpgs::pgs::OdsData]) -> Vec<(u16, Vec<u8>)> {
    let mut groups: Vec<(u16, Option<u16>, Option<u16>, Vec<u8>)> = Vec::new();
    for ods in ods_list {
        if let Some(group) = groups.iter_mut().find(|(id, _, _, _)| *id == ods.id) {
            group.3.extend_from_slice(&ods.rle_data);
        } else {
            groups.push((ods.id, ods.width, ods.height, ods.rle_data.clone()));
        }
    }

    let mut result = Vec::new();
    for (id, width, height, rle_data) in groups {
        if let (Some(w), Some(h)) = (width, height) {
            if let Some(pixels) = libpgs::pgs::decode_rle(&rle_data, w, h) {
                result.push((id, pixels));
            }
        }
    }
    result
}

#[test]
fn encode_empty_input_produces_no_output() {
    let tmp_dir = std::env::temp_dir();
    let sup_path = tmp_dir.join("libpgs_test_empty_encode.sup");

    let binary = env!("CARGO_BIN_EXE_libpgs");
    let child = Command::new(binary)
        .arg("encode")
        .arg("-o")
        .arg(&sup_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to start libpgs encode");

    // Close stdin immediately (empty input).
    let mut child = child;
    drop(child.stdin.take());
    let output = child.wait_with_output().expect("failed to wait on encode");
    assert!(output.status.success(), "encode should succeed on empty input");

    // Output file should not exist (no display sets).
    let _ = std::fs::remove_file(&sup_path);
}

#[test]
fn encode_tracks_only_input_produces_no_output() {
    let tmp_dir = std::env::temp_dir();
    let sup_path = tmp_dir.join("libpgs_test_tracks_only_encode.sup");

    let binary = env!("CARGO_BIN_EXE_libpgs");
    let ndjson = b"{\"type\":\"tracks\",\"tracks\":[]}\n";
    let child = Command::new(binary)
        .arg("encode")
        .arg("-o")
        .arg(&sup_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to start libpgs encode");

    use std::io::Write;
    let mut child = child;
    child.stdin.take().unwrap().write_all(ndjson).unwrap();
    let output = child.wait_with_output().expect("failed to wait on encode");
    assert!(
        output.status.success(),
        "encode should succeed with tracks-only input"
    );

    let _ = std::fs::remove_file(&sup_path);
}
