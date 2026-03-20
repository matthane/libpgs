# CLAUDE.md — libpgs

## What is this project?

libpgs is a Rust library + CLI for extracting PGS (Presentation Graphic Stream) subtitles from MKV and M2TS/TS containers. It is designed to extract only PGS data with minimal I/O — skipping video/audio entirely.

## Build & test

```bash
cargo build          # Build library + CLI
cargo test           # Run all unit + integration tests
cargo check          # Type-check without building
cargo run -- <args>  # Run the CLI
```

- Rust edition: 2024
- Single external dependency: `flate2` (zlib decompression for MKV ContentEncoding)
- No dev-dependencies

## Architecture

### Module layout

```
src/
├── lib.rs              # Public API: Extractor, batch functions, format detection
├── error.rs            # PgsError enum
├── io/
│   └── reader.rs       # SeekBufReader — buffered I/O with position tracking
├── pgs/
│   ├── segment.rs      # PgsSegment: parse/serialize PGS segments
│   └── display_set.rs  # DisplaySet + DisplaySetAssembler (push-based state machine)
├── ebml/
│   ├── mod.rs          # EBML element ID constants
│   └── vint.rs         # Variable-length integer codec
├── mkv/
│   ├── mod.rs          # MKV orchestrator, metadata parsing, parallel extraction
│   ├── header.rs       # EBML header + Segment layout parsing
│   ├── tracks.rs       # PGS track discovery from Tracks element
│   ├── cues.rs         # Cues index parsing
│   ├── cluster.rs      # Cluster map building, scanning, probing
│   ├── block.rs        # Block header parsing
│   └── stream.rs       # MkvExtractorState — streaming state machine
├── m2ts/
│   ├── mod.rs          # M2TS orchestrator, metadata parsing
│   ├── ts_packet.rs    # TS packet format detection + parsing
│   ├── pat.rs          # PAT parsing (program association)
│   ├── pmt.rs          # PMT parsing (stream discovery)
│   ├── pes.rs          # PES reassembly state machine
│   ├── clpi.rs         # BDMV CLPI parser for PID → language fallback
│   └── stream.rs       # M2tsExtractorState — streaming state machine
└── cli/
    └── main.rs         # CLI binary: tracks, extract, stream, bench subcommands
```

### Core design

**Single code path:** The `Extractor` (iterator-based streaming API) is THE extraction implementation. Batch functions (`extract_all_display_sets`, `extract_display_sets`, etc.) are thin wrappers around it.

**Streaming pattern:** `Extractor` implements `Iterator<Item = Result<TrackDisplaySet, PgsError>>`. Display sets are yielded one at a time. Callers can stop early, filter, or take N items without extracting the entire file.

**History catalog:** Every yielded display set is cloned into an internal `Vec`. Access via `history()` / `history_for_track()`. Manage memory via `drain_history()` / `clear_history()`.

**MKV three-tier extraction strategy:**
1. **Cues fast path** — seek directly to clusters via cue point offsets (uses `relative_position` for sub-cluster seeking)
2. **Cluster probe** — build cluster map, probe each with 16KB window, full-scan only active clusters
3. **Sequential scan** — linear fallback

**MKV parallel optimization:** For batch collection (`collect_by_track()`), if Cues are available and extraction hasn't started, uses scoped threads (1–8 workers) with independent file handles to pipeline NAS latency.

**M2TS bulk PID scanning:** Reads 2MB blocks, checks PID bytes directly in buffer (~0.025% of packets need full header parsing). 2MB I/O buffer for NAS throughput.

**M2TS BDMV language fallback:** When an M2TS file is inside a `BDMV/STREAM/` directory, the library reads the corresponding `.clpi` file from `BDMV/CLIPINF/` to get PID → language mappings. These are applied as a fallback only — PMT-provided language descriptors always take priority. Fail-silent if the CLPI is missing or unparseable.

### Key types

- `Extractor` — streaming iterator, the central API
- `TrackDisplaySet` — a display set annotated with track_id, language, container
- `DisplaySet` — PTS + composition state + ordered segments
- `PgsSegment` — type + PTS/DTS + payload, with serialize support
- `DisplaySetAssembler` — push-based state machine: PCS opens, END closes
- `PesReassembler` — M2TS PES packet reassembly per PID
- `MkvExtractorState` / `M2tsExtractorState` — format-specific streaming state machines
- `SeekBufReader<R>` — buffered reader with absolute position tracking and I/O accounting

### Public API (src/lib.rs)

**Streaming:**
- `Extractor::open(path)` → create extractor
- `Extractor::with_track_filter(&[u32])` → restrict tracks (chainable, reopens file)
- `Extractor::tracks()` → discovered PGS tracks
- `Extractor::history()` / `history_for_track(id)` → catalog of yielded display sets
- `Extractor::drain_history()` / `clear_history()` → memory management
- `Extractor::stats()` → I/O statistics
- `Extractor::collect_by_track()` → exhaust + group by track (parallel optimization)
- `Iterator::next()` → yields `Result<TrackDisplaySet, PgsError>`

**Batch convenience:**
- `extract_all_display_sets(path)` → all tracks grouped
- `extract_all_display_sets_with_stats(path)` → all tracks + I/O stats
- `extract_display_sets(path, track_id)` → single track
- `extract_display_sets_with_stats(path, track_id)` → single track + I/O stats

**Utilities:**
- `list_pgs_tracks(path)` → discover tracks without extraction
- `write_sup_file(display_sets, output)` → write .sup file

## CLI

```
libpgs tracks <file>                       # List PGS tracks
libpgs extract <file> -o <out> [-t <id>]   # Extract to .sup
libpgs stream <file> [-t <id>]             # Stream NDJSON to stdout
libpgs bench <file>                        # Benchmark I/O efficiency
```

### Stream command (NDJSON protocol)

The `stream` command exposes the `Extractor` streaming API over stdout as newline-delimited JSON, enabling any language to consume PGS data incrementally via a subprocess pipe — no temp files or waiting for full extraction.

**Line 1 — track discovery:**
```json
{"type":"tracks","tracks":[{"track_id":3,"language":"eng","container":"Matroska"}]}
```

**Subsequent lines — one per display set, flushed immediately when yielded:**
```json
{"type":"display_set","track_id":3,"language":"eng","container":"Matroska","pts":311580,"pts_ms":3462.0000,"composition_state":"EpochStart","segments":[{"type":"PresentationComposition","pts":311580,"dts":0,"size":19,"payload":"<base64>"},{"type":"EndOfDisplaySet","pts":311580,"dts":0,"size":0,"payload":""}]}
```

Fields per display set: `track_id`, `language` (nullable), `container`, `pts` (90 kHz ticks), `pts_ms`, `composition_state` (Normal/AcquisitionPoint/EpochStart). Each segment includes `type`, `pts`, `dts`, `size`, and base64-encoded `payload`.

## Code conventions

- `pub(crate)` for internal APIs shared across modules
- State machines for streaming (MkvBlockSource enum, M2tsExtractorState)
- Tests use production code paths (e.g., M2tsExtractorState with temp files), not test-only helpers
- Constants for tuning: `MKV_PROBE_THRESHOLD`, `CLUSTER_PROBE_SIZE`, `SCAN_BLOCK_SIZE`, `M2TS_BUF_SIZE`
- Error handling via `PgsError` enum with `?` propagation throughout
