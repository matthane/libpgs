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
libpgs bench <file>                        # Benchmark I/O efficiency
```

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT License](LICENSE-MIT) at your option.
