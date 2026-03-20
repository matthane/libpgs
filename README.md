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

The first line is a track discovery message:

```json
{"type":"tracks","tracks":[{"track_id":3,"language":"eng","container":"Matroska"}]}
```

Each subsequent line is a complete display set with full metadata:

```json
{"type":"display_set","track_id":3,"language":"eng","container":"Matroska","pts":311580,"pts_ms":3462.0000,"composition_state":"EpochStart","segments":[...]}
```

Example Python consumer:

```python
import subprocess
import json

proc = subprocess.Popen(
    ["libpgs", "stream", "movie.mkv"],
    stdout=subprocess.PIPE,
    text=True,
)

for line in proc.stdout:
    msg = json.loads(line)
    if msg["type"] == "tracks":
        print(f"Found {len(msg['tracks'])} PGS tracks")
    elif msg["type"] == "display_set":
        print(f"Track {msg['track_id']} @ {msg['pts_ms']:.1f}ms — "
              f"{len(msg['segments'])} segments, state={msg['composition_state']}")
```

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT License](LICENSE-MIT) at your option.
