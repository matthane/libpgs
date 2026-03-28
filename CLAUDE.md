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
│   ├── payload.rs      # PGS segment payload parsing (PCS, WDS, PDS, ODS)
│   ├── rle.rs          # PGS RLE bitmap decoder (palette indices)
│   └── display_set.rs  # DisplaySet + DisplaySetAssembler (push-based state machine)
├── ebml/
│   ├── mod.rs          # EBML element ID constants
│   └── vint.rs         # Variable-length integer codec
├── mkv/
│   ├── mod.rs          # MKV orchestrator, metadata parsing, parallel extraction
│   ├── header.rs       # EBML header + Segment layout parsing
│   ├── tracks.rs       # PGS track discovery from Tracks element
│   ├── tags.rs         # Tags element parsing (NUMBER_OF_FRAMES per track)
│   ├── cues.rs         # Cues index parsing
│   ├── cluster.rs      # Cluster scanning for PGS blocks
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

**MKV two-tier extraction strategy:**
1. **Cues fast path** — seek directly to clusters via cue point offsets (uses `relative_position` for sub-cluster seeking)
2. **Sequential scan** — single-pass linear read through the Segment with 2 MB I/O buffer, processing Clusters as encountered

**MKV parallel optimization:** For batch collection (`collect_by_track()`), if Cues are available and extraction hasn't started, uses scoped threads (1–8 workers) with independent file handles to pipeline NAS latency.

**M2TS bulk PID scanning:** Reads 2MB blocks, checks PID bytes directly in buffer (~0.025% of packets need full header parsing). 2MB I/O buffer for NAS throughput.

**M2TS BDMV language fallback:** When an M2TS file is inside a `BDMV/STREAM/` directory, the library reads the corresponding `.clpi` file from `BDMV/CLIPINF/` to get PID → language mappings. These are applied as a fallback only — PMT-provided language descriptors always take priority. Fail-silent if the CLPI is missing or unparseable.

**Time-range seeking:** When a time range is specified via `with_time_range()`, each format seeks directly to the estimated byte offset — no data before the start point is read or processed. The iterator also enforces a safety-net filter: display sets with `pts_ms < start_ms` are skipped, and iteration stops when `pts_ms > end_ms`.
- **MKV with Cues** — exact seeking by filtering cue points to the requested time range before iteration begins.
- **M2TS** — binary search refinement: starts with a bitrate estimate, then probes the actual PTS at the estimated position and narrows the range iteratively (up to 20 probes of 512 KB each) to converge on the exact byte offset. BDMV uses CLPI `presentation_start_time`/`presentation_end_time` for the initial estimate; non-BDMV discovers last PTS by scanning the final 2 MB.
- **MKV sequential / SUP** — bitrate estimation: `byte_offset = file_size * (target_pts / duration)`, backed up by a small margin, then scan forward for packet/segment alignment. SUP reads first/last PTS from segment headers.

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
- `Extractor::with_time_range(Option<f64>, Option<f64>)` → restrict to time window in ms (chainable, seeks to estimated offset)
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
libpgs tracks <file>                                                        # List PGS tracks
libpgs extract <file> -o <out> [-t <id>] [--start T] [--end T]             # Extract to .sup
libpgs stream <file> [-t <id>] [--raw-payloads] [--start T] [--end T]      # Stream NDJSON to stdout
libpgs bench <file>                                                         # Benchmark I/O efficiency
```

`extract` and `stream` accept `--start` and `--end` timestamps (`HH:MM:SS.ms`, `MM:SS.ms`, `SS.ms`, or plain seconds) to limit extraction to a time window. Seeks directly to the estimated offset — does not scan data before the start point.

### Stream command (NDJSON protocol)

The `stream` command exposes the `Extractor` streaming API over stdout as newline-delimited JSON, enabling any language to consume PGS data incrementally via a subprocess pipe — no temp files or waiting for full extraction. See `docs/STREAMING.md` for the full consumer reference.

**Line 1 — track discovery:**
```json
{"type":"tracks","tracks":[{"track_id":3,"language":"en","container":"Matroska","name":"English Subtitles","is_default":true,"is_forced":false,"display_set_count":1234,"indexed":true}]}
```

Track fields: `track_id`, `language` (nullable), `container`, `name` (nullable, MKV TrackName), `is_default` (nullable bool), `is_forced` (nullable bool), `display_set_count` (nullable, from MKV Tags NUMBER_OF_FRAMES), `indexed` (nullable bool, MKV only). M2TS tracks have `null` for MKV-specific fields.

**Subsequent lines — one per display set, flushed immediately when yielded:**

Display sets use semantic grouping instead of a flat segment array. PCS data is in `composition`, WDS in `windows[]`, PDS in `palettes[]`, ODS in `objects[]`. END segments are omitted (no data).

```json
{"type":"display_set","track_id":3,"index":0,"pts":311580,"pts_ms":3462.0000,"composition":{"number":1,"state":"epoch_start","video_width":1920,"video_height":1080,"palette_only":false,"palette_id":0,"objects":[{"object_id":0,"window_id":0,"x":773,"y":108,"crop":null}]},"windows":[{"id":0,"x":773,"y":108,"width":377,"height":43}],"palettes":[{"id":0,"version":0,"entries":[{"id":0,"luminance":16,"cr":128,"cb":128,"alpha":0}]}],"objects":[{"id":0,"version":0,"sequence":"complete","data_length":8635,"width":377,"height":43,"bitmap":"<base64>"}]}
```

Key fields: `composition.state` (`normal`/`acquisition_point`/`epoch_start`), `composition.objects[]` (placement instructions cross-referencing `objects[].id` and `windows[].id`), `palettes[].entries[]` (YCrCb+alpha colors), `objects[].sequence` (`complete`/`first`/`last`/`reassembled`), `objects[].bitmap` (base64 palette indices, 1 byte per pixel, row-major). Fragmented objects are automatically reassembled.

**`--raw-payloads` flag:** When passed, each semantic item includes a `"payload"` field with base64-encoded raw segment bytes. Omitted by default.

## Code conventions

- `pub(crate)` for internal APIs shared across modules
- State machines for streaming (MkvBlockSource enum, M2tsExtractorState)
- Tests use production code paths (e.g., M2tsExtractorState with temp files), not test-only helpers
- Constants for tuning: `MKV_PROBE_THRESHOLD`, `CLUSTER_PROBE_SIZE`, `SCAN_BLOCK_SIZE`, `M2TS_BUF_SIZE`
- Error handling via `PgsError` enum with `?` propagation throughout

## Release hygiene

- `Cargo.toml` version must match the latest git release tag (e.g., tag `v0.2.0` → `version = "0.2.0"`). When creating a release or noticing a mismatch, update `Cargo.toml` accordingly.
