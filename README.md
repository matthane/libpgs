# libpgs

A Rust library and CLI for extracting and parsing PGS (Presentation Graphic Stream) subtitles from MKV and M2TS/TS containers.

> **Note:** This project is under active development.

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
