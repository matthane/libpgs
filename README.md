# libpgs

A Rust library and CLI for extracting, encoding, and transforming PGS (Presentation Graphic Stream) subtitles from MKV and M2TS/TS containers.

> **Note:** This project is under active development.

### Cue-based extraction

Most tools (ffmpeg, mkvextract) extract subtitle tracks by reading through the entire MKV file linearly — parsing every cluster and discarding the video/audio blocks they don't need. For a 40 GB Blu-ray remux where the PGS subtitle data is only a few megabytes, this means reading tens of gigabytes just to find the subtitle blocks.

libpgs takes a different approach for MKV files that contain a Cues index. It reads the Cues element to identify exactly which clusters hold subtitle data, then seeks directly to those locations — skipping everything else. It also uses `CueRelativePosition` for sub-cluster seeking when available, jumping straight to the relevant block within a cluster.

The result is that extraction I/O scales with the size of the subtitle data, not the size of the file. On a typical Blu-ray remux, libpgs reads less than 1% of the file — often under 0.1% — compared to the full file read required by conventional tools. This is especially noticeable on network-attached storage where seek latency matters, and for batch workflows that process many large files. You can verify the difference on your own files with `libpgs bench`.

When Cues are not present, libpgs falls back to a single-pass sequential scan.

## Installation

Add to your project:

```bash
cargo add libpgs
```

## Library usage

```rust
use libpgs::Extractor;

// Stream display sets one at a time
let extractor = Extractor::open("movie.mkv")?;
for result in extractor {
    let track_ds = result?;
    println!("Track {}: {} segments", track_ds.track_id, track_ds.display_set.segments.len());
}

// Extract only a specific time range (seeks directly, minimal I/O)
let extractor = Extractor::open("movie.mkv")?
    .with_time_range(Some(300_000.0), Some(600_000.0)); // 5:00 to 10:00
for result in extractor {
    let track_ds = result?;
    println!("Track {} @ {}ms", track_ds.track_id, track_ds.display_set.pts_ms);
}

// Or collect everything at once
let by_track = Extractor::open("movie.mkv")?.collect_by_track()?;
for track in &by_track {
    println!("{}: {} display sets", track.track.track_id, track.display_sets.len());
}
```

### Encoding and round-trip

libpgs supports full round-trip workflows: extract display sets, modify them, and write the result back as a `.sup` file. You can also create PGS display sets from scratch.

```rust
use libpgs::pgs::*;

// Modify an extracted palette and write back
let mut extractor = Extractor::open("movie.mkv")?;
let track_ds = extractor.next().unwrap()?;
let mut ds = track_ds.display_set;

// Parse, modify, and update a palette segment in-place
for seg in &mut ds.segments {
    if let Some(mut pds) = seg.parse_pds() {
        for entry in &mut pds.entries {
            entry.luminance = entry.luminance.saturating_add(10); // brighten
        }
        seg.set_pds_payload(&pds);
    }
}
libpgs::write_sup_file(&[ds], "modified.sup".as_ref())?;

// Or build a display set from scratch
let ds = DisplaySetBuilder::new(90_000) // PTS in 90kHz ticks
    .pcs(PcsData {
        video_width: 1920,
        video_height: 1080,
        composition_number: 0,
        composition_state: CompositionState::EpochStart,
        palette_only: false,
        palette_id: 0,
        objects: vec![CompositionObject {
            object_id: 0, window_id: 0, x: 100, y: 900, crop: None,
        }],
    })
    .wds(WdsData {
        windows: vec![WindowDefinition { id: 0, x: 100, y: 900, width: 200, height: 30 }],
    })
    .palette(PdsData {
        id: 0, version: 0,
        entries: vec![
            PaletteEntry { id: 0, luminance: 16, cr: 128, cb: 128, alpha: 0 },
            PaletteEntry { id: 1, luminance: 235, cr: 128, cb: 128, alpha: 255 },
        ],
    })
    .object(ObjectBitmap {
        id: 0, version: 0, width: 200, height: 30,
        pixels: vec![1u8; 200 * 30], // palette index per pixel, row-major
    })
    .build()?;
libpgs::write_sup_file(&[ds], "output.sup".as_ref())?;
```

The `DisplaySetBuilder` handles RLE encoding automatically and fragments large bitmaps across multiple ODS segments as required by the PGS spec.

## CLI

```
libpgs tracks <file>                                                        # List PGS tracks
libpgs extract <file> -o <out> [-t <id>] [--start T] [--end T]              # Extract to .sup
libpgs stream <file> [-t <id>] [--raw-payloads] [--start T] [--end T]       # Stream NDJSON to stdout
libpgs encode -o <output.sup>                                               # Encode NDJSON stdin to .sup
libpgs bench <file>                                                         # Benchmark I/O efficiency
```

### Time-range filtering

Both `extract` and `stream` accept `--start` and `--end` timestamps to limit extraction to a specific time window:

```bash
libpgs extract movie.mkv -o out.sup --start 0:05:00          # From 5 minutes to end
libpgs stream movie.mkv --start 0:05:00 --end 0:10:00        # 5-minute window only
```

Timestamps accept `HH:MM:SS.ms`, `MM:SS.ms`, `SS.ms`, or plain seconds. When a time range is specified, libpgs seeks directly to the target byte offset — it does not read and discard data before the start point. For MKV files with a Cues index, seeking is exact. For M2TS files, seeking uses binary search refinement to converge on the correct position despite variable bitrate. SUP files use simple bitrate estimation.

If no display sets fall within the requested range, libpgs reports zero results with no error.

### Streaming to external scripts

The `stream` command outputs newline-delimited JSON (NDJSON) to stdout, allowing any language to consume PGS data incrementally via a subprocess pipe. Display sets are flushed as soon as they are extracted — no temp files or waiting for the full file to be processed.

The first line is a track discovery message with all available metadata:

```json
{"type":"tracks","tracks":[{"track_id":3,"language":"en","container":"Matroska","name":"English Subtitles","is_default":true,"is_forced":false,"display_set_count":1234,"indexed":true}]}
```

Each subsequent line is a display set with fully parsed segment data organized into semantic sections — composition, windows, palettes, and objects:

```json
{"type":"display_set","track_id":3,"index":0,"pts":311580,"pts_ms":3462.0000,"composition":{"number":1,"state":"epoch_start","video_width":1920,"video_height":1080,"palette_only":false,"palette_id":0,"objects":[{"object_id":0,"window_id":0,"x":773,"y":108,"crop":null}]},"windows":[{"id":0,"x":773,"y":108,"width":377,"height":43}],"palettes":[{"id":0,"version":0,"entries":[{"id":0,"luminance":16,"cr":128,"cb":128,"alpha":0}]}],"objects":[{"id":0,"version":0,"sequence":"complete","data_length":8635,"width":377,"height":43,"bitmap":"<base64>"}]}
```

The `index` field is a 0-based per-track sequence number. Combined with `display_set_count` from the tracks header, consumers can calculate extraction progress. Pass `--raw-payloads` to include base64-encoded raw segment bytes alongside the parsed data.

See [docs/STREAMING.md](docs/STREAMING.md) for the complete schema reference, field tables, cross-reference diagram, and usage examples.

Example Python consumer:

```python
import subprocess
import json

proc = subprocess.Popen(
    ["libpgs", "stream", "movie.mkv", "--start", "0:05:00", "--end", "0:10:00"],
    stdout=subprocess.PIPE,
    text=True,
)

tracks = {}
for line in proc.stdout:
    msg = json.loads(line)
    if msg["type"] == "tracks":
        for t in msg["tracks"]:
            tracks[t["track_id"]] = t
            print(f"Track {t['track_id']}: {t.get('name') or t.get('language', '?')}"
                  f" ({'default' if t.get('is_default') else 'non-default'})")
    elif msg["type"] == "display_set":
        tid = msg["track_id"]
        total = tracks[tid].get("display_set_count")
        progress = f" ({msg['index']+1}/{total})" if total else ""
        comp = msg.get("composition") or {}
        n_objects = len(msg.get("objects", []))
        print(f"Track {tid} @ {msg['pts_ms']:.1f}ms — "
              f"{comp.get('state', '?')} {n_objects} objects{progress}")
```

### Encoding from NDJSON

The `encode` command reads the same NDJSON format that `stream` produces, enabling full round-trip workflows:

```bash
# Extract, modify with an external script, and re-encode
libpgs stream movie.mkv | python modify.py | libpgs encode -o modified.sup

# Or pipe stream directly back to encode (identity round-trip)
libpgs stream movie.mkv | libpgs encode -o roundtrip.sup
```

The encode command reads from stdin and writes a `.sup` file. It accepts the `pts` field (90kHz integer ticks) as the primary timestamp; if absent, it falls back to `pts_ms * 90`. Track metadata lines (`{"type":"tracks",...}`) are silently skipped. If the input contains multiple `track_id` values, encode splits the output into separate files (`<stem>_track<id>.sup`).

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT License](LICENSE-MIT) at your option.
