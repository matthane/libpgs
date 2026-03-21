# libpgs

A Rust library and CLI for extracting and parsing PGS (Presentation Graphic Stream) subtitles from MKV and M2TS/TS containers.

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

// Or collect everything at once
let by_track = Extractor::open("movie.mkv")?.collect_by_track()?;
for track in &by_track {
    println!("{}: {} display sets", track.track.track_id, track.display_sets.len());
}
```

## CLI

```
libpgs tracks <file>                       # List PGS tracks
libpgs extract <file> -o <out> [-t <id>]   # Extract to .sup
libpgs stream <file> [-t <id>]             # Stream NDJSON to stdout
libpgs bench <file>                        # Benchmark I/O efficiency
```

### Streaming to external scripts

The `stream` command outputs newline-delimited JSON (NDJSON) to stdout, allowing any language to consume PGS data incrementally via a subprocess pipe. Display sets are flushed as soon as they are extracted — no temp files or waiting for the full file to be processed.

The first line is a track discovery message with all available metadata:

```json
{"type":"tracks","tracks":[{"track_id":3,"language":"eng","container":"Matroska","name":"English Subtitles","flag_default":true,"flag_forced":false,"display_set_count":1234}]}
```

Track fields:
- `track_id` — numeric track identifier
- `language` — language code (nullable)
- `container` — `"Matroska"` or `"M2TS"`
- `name` — track name from MKV TrackName (nullable, MKV only)
- `flag_default` — whether the track is flagged as default (nullable, MKV only)
- `flag_forced` — whether the track is flagged as forced (nullable, MKV only)
- `display_set_count` — total number of display sets, from MKV Tags NUMBER_OF_FRAMES (nullable, MKV only)

Each subsequent line is a display set with its per-track index:

```json
{"type":"display_set","track_id":3,"index":0,"pts":311580,"pts_ms":3462.0000,"composition_state":"EpochStart","segments":[...]}
```

The `index` field is a 0-based per-track sequence number. Combined with `display_set_count` from the tracks header, consumers can calculate extraction progress.

Example Python consumer:

```python
import subprocess
import json

proc = subprocess.Popen(
    ["libpgs", "stream", "movie.mkv"],
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
                  f" ({'default' if t.get('flag_default') else 'non-default'})")
    elif msg["type"] == "display_set":
        tid = msg["track_id"]
        total = tracks[tid].get("display_set_count")
        progress = f" ({msg['index']+1}/{total})" if total else ""
        print(f"Track {tid} @ {msg['pts_ms']:.1f}ms — "
              f"{len(msg['segments'])} segments{progress}")
```

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT License](LICENSE-MIT) at your option.
