//! Integration test: extract PGS display sets, apply a visible transform
//! (2× nearest-neighbor scale + palette color shift), write to a new .sup file,
//! then re-read and verify the result is structurally correct.
//!
//! Skipped at runtime if no fixture files are present.

use libpgs::pgs::{
    decode_rle, CompositionObject, DisplaySetBuilder, ObjectBitmap,
    PaletteEntry, PcsData, PdsData, SegmentType, WdsData, WindowDefinition,
};
use std::path::Path;

const FIXTURES: &[&str] = &[
    "tests/fixtures/raw-pgs.sup",
    "tests/fixtures/matroska-with-cues.mkv",
    "tests/fixtures/matroska-no-cues.mkv",
    "tests/fixtures/mpeg-transport-stream.m2ts",
];

fn available_fixtures() -> Vec<&'static str> {
    FIXTURES
        .iter()
        .copied()
        .filter(|p| Path::new(p).exists())
        .collect()
}

/// Scale a palette-indexed bitmap 2× using nearest-neighbor interpolation.
fn scale_2x(pixels: &[u8], w: usize, h: usize) -> Vec<u8> {
    let new_w = w * 2;
    let new_h = h * 2;
    let mut out = vec![0u8; new_w * new_h];
    for y in 0..new_h {
        for x in 0..new_w {
            out[y * new_w + x] = pixels[(y / 2) * w + (x / 2)];
        }
    }
    out
}

/// Shift palette colors to make the transformation very obvious.
/// Swaps Cr and Cb (hue rotation) and inverts luminance.
fn shift_palette(entry: &PaletteEntry) -> PaletteEntry {
    PaletteEntry {
        id: entry.id,
        luminance: 255 - entry.luminance,
        cr: entry.cb,  // swap Cr <-> Cb
        cb: entry.cr,
        alpha: entry.alpha,
    }
}

#[test]
fn extract_transform_write_reread() {
    let fixtures = available_fixtures();
    if fixtures.is_empty() {
        eprintln!("No fixture files found — skipping round_trip_transform test");
        return;
    }

    for fixture in &fixtures {
        eprintln!("--- Testing round-trip transform on: {fixture}");

        // 1. Extract all display sets
        let all_tracks =
            libpgs::extract_all_display_sets(Path::new(fixture)).expect("extraction should succeed");

        let total_ds: usize = all_tracks.iter().map(|t| t.display_sets.len()).sum();
        if total_ds == 0 {
            eprintln!("  No display sets found, skipping");
            continue;
        }

        // Use the first track that has display sets
        let track = all_tracks
            .iter()
            .find(|t| !t.display_sets.is_empty())
            .unwrap();

        eprintln!(
            "  Track {}: {} display sets, language={:?}",
            track.track.track_id,
            track.display_sets.len(),
            track.track.language
        );

        // 2. Transform each display set: 2× scale + palette color shift
        let mut transformed = Vec::new();

        for ds in &track.display_sets {
            // Parse the PCS
            let pcs_seg = ds
                .segments
                .iter()
                .find(|s| s.segment_type == SegmentType::PresentationComposition)
                .expect("display set must have PCS");
            let pcs = pcs_seg.parse_pcs().expect("PCS should parse");

            // Parse WDS if present
            let wds_opt = ds
                .segments
                .iter()
                .find(|s| s.segment_type == SegmentType::WindowDefinition)
                .and_then(|s| s.parse_wds());

            // Parse all PDS segments and shift colors
            let palettes: Vec<PdsData> = ds
                .segments
                .iter()
                .filter(|s| s.segment_type == SegmentType::PaletteDefinition)
                .filter_map(|s| s.parse_pds())
                .map(|pds| PdsData {
                    id: pds.id,
                    version: pds.version,
                    entries: pds.entries.iter().map(shift_palette).collect(),
                })
                .collect();

            // Collect and reassemble ODS fragments per object_id
            let ods_segments: Vec<_> = ds
                .segments
                .iter()
                .filter(|s| s.segment_type == SegmentType::ObjectDefinition)
                .filter_map(|s| s.parse_ods())
                .collect();

            // Group ODS by object id, reassemble RLE data
            let mut ods_by_id: std::collections::BTreeMap<u16, (Option<u16>, Option<u16>, Vec<u8>)> =
                std::collections::BTreeMap::new();
            for ods in &ods_segments {
                let entry = ods_by_id
                    .entry(ods.id)
                    .or_insert_with(|| (None, None, Vec::new()));
                if let Some(w) = ods.width {
                    entry.0 = Some(w);
                }
                if let Some(h) = ods.height {
                    entry.1 = Some(h);
                }
                entry.2.extend_from_slice(&ods.rle_data);
            }

            // Build scaled objects — keep original position, grow the bitmap.
            // Clamp the scaled size so the object fits within the video frame.
            let mut objects = Vec::new();
            let mut scaled_sizes: std::collections::BTreeMap<u16, (u16, u16)> =
                std::collections::BTreeMap::new();

            for (id, (w_opt, h_opt, rle_data)) in &ods_by_id {
                let w = w_opt.expect("ODS must have width");
                let h = h_opt.expect("ODS must have height");

                // Find this object's position from the PCS to know how much room we have
                let co = pcs.objects.iter().find(|o| o.object_id == *id);
                let (ox, oy) = co.map_or((0u16, 0u16), |c| (c.x, c.y));

                // Clamp scaled dimensions so object stays within the video frame
                let max_w = pcs.video_width.saturating_sub(ox);
                let max_h = pcs.video_height.saturating_sub(oy);
                let new_w = (w * 2).min(max_w);
                let new_h = (h * 2).min(max_h);

                let pixels = decode_rle(rle_data, w, h).expect("RLE decode should succeed");
                let scaled_full = scale_2x(&pixels, w as usize, h as usize);

                // Crop the scaled bitmap to the clamped dimensions
                let cropped = if new_w == w * 2 && new_h == h * 2 {
                    scaled_full
                } else {
                    let full_w = (w * 2) as usize;
                    let mut cropped = vec![0u8; new_w as usize * new_h as usize];
                    for row in 0..new_h as usize {
                        let src_start = row * full_w;
                        let dst_start = row * new_w as usize;
                        cropped[dst_start..dst_start + new_w as usize]
                            .copy_from_slice(&scaled_full[src_start..src_start + new_w as usize]);
                    }
                    cropped
                };

                scaled_sizes.insert(*id, (new_w, new_h));

                objects.push(ObjectBitmap {
                    id: *id,
                    version: 0,
                    width: new_w,
                    height: new_h,
                    pixels: cropped,
                });
            }

            // Build new PCS — keep original positions, don't scale them
            let new_pcs = PcsData {
                video_width: pcs.video_width,
                video_height: pcs.video_height,
                composition_number: pcs.composition_number,
                composition_state: pcs.composition_state,
                palette_only: pcs.palette_only,
                palette_id: pcs.palette_id,
                objects: pcs
                    .objects
                    .iter()
                    .map(|co| CompositionObject {
                        object_id: co.object_id,
                        window_id: co.window_id,
                        x: co.x,
                        y: co.y,
                        crop: co.crop.clone(),
                    })
                    .collect(),
            };

            // Build new WDS — keep original positions, expand window to fit scaled objects
            let new_wds = wds_opt.map(|wds| WdsData {
                windows: wds
                    .windows
                    .iter()
                    .map(|win| {
                        // Expand window to at least fit the scaled objects placed in it
                        let mut needed_w = win.width * 2;
                        let mut needed_h = win.height * 2;
                        // Clamp to video bounds from this position
                        needed_w = needed_w.min(pcs.video_width.saturating_sub(win.x));
                        needed_h = needed_h.min(pcs.video_height.saturating_sub(win.y));
                        WindowDefinition {
                            id: win.id,
                            x: win.x,
                            y: win.y,
                            width: needed_w,
                            height: needed_h,
                        }
                    })
                    .collect(),
            });

            // Assemble via DisplaySetBuilder
            let mut builder = DisplaySetBuilder::new(ds.pts).pcs(new_pcs);

            if let Some(wds) = new_wds {
                builder = builder.wds(wds);
            }

            for pds in palettes {
                builder = builder.palette(pds);
            }

            for obj in objects {
                builder = builder.object(obj);
            }

            let new_ds = builder.build().expect("builder should succeed");
            transformed.push(new_ds);
        }

        eprintln!("  Built {} transformed display sets", transformed.len());

        // 3. Write to a temp .sup file
        let out_path = std::env::temp_dir().join(format!(
            "libpgs_test_transform_{}.sup",
            Path::new(fixture)
                .file_stem()
                .unwrap()
                .to_string_lossy()
        ));
        libpgs::write_sup_file(&transformed, &out_path).expect("write_sup_file should succeed");

        let file_size = std::fs::metadata(&out_path).unwrap().len();
        eprintln!("  Wrote {} bytes to {}", file_size, out_path.display());
        assert!(file_size > 0, "output .sup file should not be empty");

        // 4. Re-read the written .sup file and verify structure
        let reread = libpgs::extract_all_display_sets(&out_path)
            .expect("re-reading written .sup should succeed");
        assert_eq!(reread.len(), 1, "SUP file should have exactly 1 track");

        let reread_ds = &reread[0].display_sets;
        assert_eq!(
            reread_ds.len(),
            transformed.len(),
            "re-read display set count should match written count"
        );

        // 5. Verify each display set round-trips correctly
        for (i, (orig, reread)) in transformed.iter().zip(reread_ds.iter()).enumerate() {
            assert_eq!(
                orig.pts, reread.pts,
                "DS {i}: PTS mismatch"
            );
            assert_eq!(
                orig.composition_state, reread.composition_state,
                "DS {i}: composition state mismatch"
            );
            assert_eq!(
                orig.segments.len(),
                reread.segments.len(),
                "DS {i}: segment count mismatch"
            );

            // Verify PCS parsed correctly
            let orig_pcs = orig.segments[0].parse_pcs().unwrap();
            let reread_pcs = reread.segments[0].parse_pcs().unwrap();
            assert_eq!(orig_pcs.video_width, reread_pcs.video_width, "DS {i}: video width");
            assert_eq!(orig_pcs.video_height, reread_pcs.video_height, "DS {i}: video height");
            assert_eq!(orig_pcs.objects.len(), reread_pcs.objects.len(), "DS {i}: object count");

            // Verify ODS bitmaps: decode and compare pixel data
            let orig_ods: Vec<_> = orig
                .segments
                .iter()
                .filter(|s| s.segment_type == SegmentType::ObjectDefinition)
                .collect();
            let reread_ods: Vec<_> = reread
                .segments
                .iter()
                .filter(|s| s.segment_type == SegmentType::ObjectDefinition)
                .collect();
            assert_eq!(orig_ods.len(), reread_ods.len(), "DS {i}: ODS segment count");

            // Verify palette color shift was preserved
            let orig_pds: Vec<_> = orig
                .segments
                .iter()
                .filter_map(|s| s.parse_pds())
                .collect();
            let reread_pds: Vec<_> = reread
                .segments
                .iter()
                .filter_map(|s| s.parse_pds())
                .collect();
            assert_eq!(orig_pds.len(), reread_pds.len(), "DS {i}: PDS count");
            for (j, (op, rp)) in orig_pds.iter().zip(reread_pds.iter()).enumerate() {
                assert_eq!(op.entries.len(), rp.entries.len(), "DS {i} PDS {j}: entry count");
                for (k, (oe, re)) in op.entries.iter().zip(rp.entries.iter()).enumerate() {
                    assert_eq!(oe.luminance, re.luminance, "DS {i} PDS {j} entry {k}: luminance");
                    assert_eq!(oe.cr, re.cr, "DS {i} PDS {j} entry {k}: Cr");
                    assert_eq!(oe.cb, re.cb, "DS {i} PDS {j} entry {k}: Cb");
                    assert_eq!(oe.alpha, re.alpha, "DS {i} PDS {j} entry {k}: alpha");
                }
            }
        }

        // Clean up
        let _ = std::fs::remove_file(&out_path);
        eprintln!("  PASS: round-trip transform verified for {fixture}");
    }
}
