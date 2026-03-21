//! Integration test: verify the CLI `stream` NDJSON output matches
//! the batch extraction API — every field, every payload byte.
//!
//! Runs `libpgs stream` as a subprocess, parses the NDJSON lines, and
//! compares against `extract_all_display_sets`. This covers serialization,
//! base64 encoding, field mapping, and the full CLI pipeline.
//!
//! Fixture files are expected in `tests/fixtures/` but are not tracked in git.
//! Tests are skipped at runtime if the fixtures are not present.

use std::collections::HashMap;
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
    FIXTURES.iter().copied().filter(|p| Path::new(p).exists()).collect()
}

/// Minimal JSON value parser — just enough for our NDJSON schema.
/// Avoids adding serde_json as a dev-dependency.
#[derive(Debug, Clone)]
enum JsonValue {
    Null,
    Bool(bool),
    Str(String),
    Num(f64),
    Array(Vec<JsonValue>),
    Object(Vec<(String, JsonValue)>),
}

impl JsonValue {
    fn as_str(&self) -> Option<&str> {
        match self {
            JsonValue::Str(s) => Some(s),
            _ => None,
        }
    }

    fn as_bool(&self) -> Option<bool> {
        match self {
            JsonValue::Bool(b) => Some(*b),
            _ => None,
        }
    }

    fn as_f64(&self) -> Option<f64> {
        match self {
            JsonValue::Num(n) => Some(*n),
            _ => None,
        }
    }

    fn as_u64(&self) -> Option<u64> {
        self.as_f64().map(|n| n as u64)
    }

    fn as_array(&self) -> Option<&[JsonValue]> {
        match self {
            JsonValue::Array(a) => Some(a),
            _ => None,
        }
    }

    fn is_null(&self) -> bool {
        matches!(self, JsonValue::Null)
    }

    fn get(&self, key: &str) -> Option<&JsonValue> {
        match self {
            JsonValue::Object(pairs) => pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }
}

fn parse_json(s: &str) -> JsonValue {
    let bytes = s.as_bytes();
    let (val, _) = parse_value(bytes, skip_ws(bytes, 0));
    val
}

fn skip_ws(b: &[u8], mut i: usize) -> usize {
    while i < b.len() && matches!(b[i], b' ' | b'\t' | b'\r' | b'\n') {
        i += 1;
    }
    i
}

fn parse_value(b: &[u8], i: usize) -> (JsonValue, usize) {
    match b[i] {
        b'"' => parse_string(b, i),
        b'{' => parse_object(b, i),
        b'[' => parse_array(b, i),
        b'n' => {
            assert_eq!(&b[i..i + 4], b"null");
            (JsonValue::Null, i + 4)
        }
        b't' => {
            assert_eq!(&b[i..i + 4], b"true");
            (JsonValue::Bool(true), i + 4)
        }
        b'f' => {
            assert_eq!(&b[i..i + 5], b"false");
            (JsonValue::Bool(false), i + 5)
        }
        _ => parse_number(b, i),
    }
}

fn parse_string(b: &[u8], i: usize) -> (JsonValue, usize) {
    assert_eq!(b[i], b'"');
    let mut j = i + 1;
    let mut s = String::new();
    while b[j] != b'"' {
        if b[j] == b'\\' {
            j += 1;
            match b[j] {
                b'"' => s.push('"'),
                b'\\' => s.push('\\'),
                b'/' => s.push('/'),
                b'n' => s.push('\n'),
                b'r' => s.push('\r'),
                b't' => s.push('\t'),
                _ => { s.push('\\'); s.push(b[j] as char); }
            }
        } else {
            s.push(b[j] as char);
        }
        j += 1;
    }
    (JsonValue::Str(s), j + 1)
}

fn parse_number(b: &[u8], i: usize) -> (JsonValue, usize) {
    let mut j = i;
    while j < b.len() && (b[j].is_ascii_digit() || b[j] == b'.' || b[j] == b'-' || b[j] == b'e' || b[j] == b'E' || b[j] == b'+') {
        j += 1;
    }
    let s = std::str::from_utf8(&b[i..j]).unwrap();
    (JsonValue::Num(s.parse().unwrap()), j)
}

fn parse_array(b: &[u8], i: usize) -> (JsonValue, usize) {
    assert_eq!(b[i], b'[');
    let mut j = skip_ws(b, i + 1);
    let mut items = Vec::new();
    if b[j] == b']' {
        return (JsonValue::Array(items), j + 1);
    }
    loop {
        let (val, next) = parse_value(b, j);
        items.push(val);
        j = skip_ws(b, next);
        if b[j] == b']' {
            return (JsonValue::Array(items), j + 1);
        }
        assert_eq!(b[j], b',');
        j = skip_ws(b, j + 1);
    }
}

fn parse_object(b: &[u8], i: usize) -> (JsonValue, usize) {
    assert_eq!(b[i], b'{');
    let mut j = skip_ws(b, i + 1);
    let mut pairs = Vec::new();
    if b[j] == b'}' {
        return (JsonValue::Object(pairs), j + 1);
    }
    loop {
        let (key_val, next) = parse_string(b, j);
        let key = match key_val {
            JsonValue::Str(s) => s,
            _ => unreachable!(),
        };
        j = skip_ws(b, next);
        assert_eq!(b[j], b':');
        j = skip_ws(b, j + 1);
        let (val, next) = parse_value(b, j);
        pairs.push((key, val));
        j = skip_ws(b, next);
        if b[j] == b'}' {
            return (JsonValue::Object(pairs), j + 1);
        }
        assert_eq!(b[j], b',');
        j = skip_ws(b, j + 1);
    }
}

fn base64_decode(s: &str) -> Vec<u8> {
    let mut out = Vec::new();
    let lut = |c: u8| -> u8 {
        match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => 0,
        }
    };
    let bytes: Vec<u8> = s.bytes().filter(|&b| b != b'=').collect();
    for chunk in bytes.chunks(4) {
        let len = chunk.len();
        let a = lut(chunk[0]);
        let b = if len > 1 { lut(chunk[1]) } else { 0 };
        let c = if len > 2 { lut(chunk[2]) } else { 0 };
        let d = if len > 3 { lut(chunk[3]) } else { 0 };
        let triple = ((a as u32) << 18) | ((b as u32) << 12) | ((c as u32) << 6) | d as u32;
        out.push((triple >> 16) as u8);
        if len > 2 {
            out.push((triple >> 8) as u8);
        }
        if len > 3 {
            out.push(triple as u8);
        }
    }
    out
}

fn composition_state_name(cs: libpgs::pgs::segment::CompositionState) -> &'static str {
    match cs {
        libpgs::pgs::segment::CompositionState::Normal => "Normal",
        libpgs::pgs::segment::CompositionState::AcquisitionPoint => "AcquisitionPoint",
        libpgs::pgs::segment::CompositionState::EpochStart => "EpochStart",
    }
}

fn segment_type_name(st: libpgs::pgs::segment::SegmentType) -> &'static str {
    match st {
        libpgs::pgs::segment::SegmentType::PresentationComposition => "PresentationComposition",
        libpgs::pgs::segment::SegmentType::WindowDefinition => "WindowDefinition",
        libpgs::pgs::segment::SegmentType::PaletteDefinition => "PaletteDefinition",
        libpgs::pgs::segment::SegmentType::ObjectDefinition => "ObjectDefinition",
        libpgs::pgs::segment::SegmentType::EndOfDisplaySet => "EndOfDisplaySet",
    }
}

fn container_name(c: libpgs::ContainerFormat) -> &'static str {
    match c {
        libpgs::ContainerFormat::Matroska => "Matroska",
        libpgs::ContainerFormat::M2ts => "M2TS",
        libpgs::ContainerFormat::TransportStream => "TransportStream",
        libpgs::ContainerFormat::Sup => "SUP",
    }
}

/// Run `libpgs stream` and collect NDJSON output lines.
fn run_stream(fixture: &str, track_filter: Option<u32>) -> Vec<String> {
    let binary = env!("CARGO_BIN_EXE_libpgs");
    let mut cmd = Command::new(binary);
    cmd.arg("stream").arg(fixture);
    if let Some(tid) = track_filter {
        cmd.arg("-t").arg(tid.to_string());
    }
    let output = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to run libpgs stream");

    assert!(
        output.status.success(),
        "libpgs stream failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout).expect("invalid UTF-8");
    stdout.lines().filter(|l| !l.is_empty()).map(String::from).collect()
}

#[test]
fn ndjson_tracks_header_matches_api() {
    let fixtures = available_fixtures();
    if fixtures.is_empty() { return; }

    for fixture in fixtures {
        let api_tracks = libpgs::list_pgs_tracks(Path::new(fixture))
            .expect("list_pgs_tracks should succeed");

        let lines = run_stream(fixture, None);
        assert!(!lines.is_empty(), "{fixture}: no output from stream command");

        let header = parse_json(&lines[0]);
        assert_eq!(header.get("type").unwrap().as_str().unwrap(), "tracks");

        let json_tracks = header.get("tracks").unwrap().as_array().unwrap();
        assert_eq!(
            json_tracks.len(),
            api_tracks.len(),
            "{fixture}: track count mismatch"
        );

        for (jt, at) in json_tracks.iter().zip(api_tracks.iter()) {
            assert_eq!(
                jt.get("track_id").unwrap().as_u64().unwrap(),
                at.track_id as u64,
                "{fixture}: track_id mismatch"
            );
            let json_lang = jt.get("language").unwrap();
            match &at.language {
                Some(lang) => assert_eq!(json_lang.as_str().unwrap(), lang.as_str()),
                None => assert!(json_lang.is_null()),
            }
            assert_eq!(
                jt.get("container").unwrap().as_str().unwrap(),
                container_name(at.container),
            );

            // Verify new metadata fields are present and match API.
            let json_name = jt.get("name").unwrap();
            match &at.name {
                Some(name) => assert_eq!(json_name.as_str().unwrap(), name.as_str(),
                    "{fixture}: name mismatch"),
                None => assert!(json_name.is_null(), "{fixture}: expected null name"),
            }

            let json_default = jt.get("flag_default").unwrap();
            match at.flag_default {
                Some(v) => assert_eq!(json_default.as_bool().unwrap(), v,
                    "{fixture}: flag_default mismatch"),
                None => assert!(json_default.is_null(), "{fixture}: expected null flag_default"),
            }

            let json_forced = jt.get("flag_forced").unwrap();
            match at.flag_forced {
                Some(v) => assert_eq!(json_forced.as_bool().unwrap(), v,
                    "{fixture}: flag_forced mismatch"),
                None => assert!(json_forced.is_null(), "{fixture}: expected null flag_forced"),
            }

            let json_count = jt.get("display_set_count").unwrap();
            match at.display_set_count {
                Some(v) => assert_eq!(json_count.as_u64().unwrap(), v,
                    "{fixture}: display_set_count mismatch"),
                None => assert!(json_count.is_null(), "{fixture}: expected null display_set_count"),
            }

            let json_has_cues = jt.get("has_cues").unwrap();
            match at.has_cues {
                Some(v) => assert_eq!(json_has_cues.as_bool().unwrap(), v,
                    "{fixture}: has_cues mismatch"),
                None => assert!(json_has_cues.is_null(), "{fixture}: expected null has_cues"),
            }
        }
    }
}

#[test]
fn ndjson_display_sets_match_batch_extraction() {
    let fixtures = available_fixtures();
    if fixtures.is_empty() { return; }

    for fixture in fixtures {
        let batch = libpgs::extract_all_display_sets(Path::new(fixture))
            .expect("batch extraction should succeed");

        let lines = run_stream(fixture, None);
        // First line is tracks header, rest are display sets.
        let ds_lines: Vec<&str> = lines[1..].iter().map(|s| s.as_str()).collect();

        // Flatten batch into ordered display sets for comparison.
        // The streaming API yields in container order, so we need to
        // collect all batch display sets by track and interleave based
        // on streaming order.
        let batch_total: usize = batch.iter().map(|t| t.display_sets.len()).sum();
        assert_eq!(
            ds_lines.len(),
            batch_total,
            "{fixture}: display set count mismatch (NDJSON={}, batch={batch_total})",
            ds_lines.len()
        );

        // Build per-track lookup keyed by (track_id, pts) for order-independent matching.
        // The streaming API may yield display sets in a different order than batch
        // (e.g., interleaved by container position vs. grouped by track).
        let mut batch_by_key: HashMap<(u32, u64), (&libpgs::PgsTrackInfo, &libpgs::pgs::display_set::DisplaySet)> = HashMap::new();
        for t in &batch {
            for ds in &t.display_sets {
                batch_by_key.insert((t.track.track_id, ds.pts as u64), (&t.track, ds));
            }
        }

        let mut matched_keys: Vec<(u32, u64)> = Vec::new();
        let mut track_index_counters: HashMap<u32, u64> = HashMap::new();

        for (i, line) in ds_lines.iter().enumerate() {
            let json = parse_json(line);
            assert_eq!(json.get("type").unwrap().as_str().unwrap(), "display_set");

            let tid = json.get("track_id").unwrap().as_u64().unwrap() as u32;
            let pts = json.get("pts").unwrap().as_u64().unwrap();
            let key = (tid, pts);
            let &(_track, ds) = batch_by_key
                .get(&key)
                .unwrap_or_else(|| panic!("{fixture} line {i}: no batch match for track={tid} pts={pts}"));

            // Verify display_set lines do not carry language/container (slimmed format).
            assert!(json.get("language").is_none(),
                "{fixture} line {i}: display_set should not contain language");
            assert!(json.get("container").is_none(),
                "{fixture} line {i}: display_set should not contain container");

            // Verify index field is present and sequential per track.
            let json_index = json.get("index").unwrap().as_u64().unwrap();
            let expected_index = track_index_counters.entry(tid).or_insert(0);
            assert_eq!(json_index, *expected_index,
                "{fixture} line {i}: index mismatch for track {tid}");
            *expected_index += 1;

            assert_eq!(
                json.get("pts").unwrap().as_u64().unwrap(),
                ds.pts as u64,
                "{fixture} line {i}: pts mismatch"
            );
            assert_eq!(
                json.get("composition_state").unwrap().as_str().unwrap(),
                composition_state_name(ds.composition_state),
                "{fixture} line {i}: composition_state mismatch"
            );

            // Verify segments — count, types, and payload bytes.
            let json_segs = json.get("segments").unwrap().as_array().unwrap();
            assert_eq!(
                json_segs.len(),
                ds.segments.len(),
                "{fixture} line {i}: segment count mismatch"
            );

            for (si, (jseg, seg)) in json_segs.iter().zip(ds.segments.iter()).enumerate() {
                assert_eq!(
                    jseg.get("type").unwrap().as_str().unwrap(),
                    segment_type_name(seg.segment_type),
                    "{fixture} line {i} seg {si}: type mismatch"
                );
                assert_eq!(
                    jseg.get("pts").unwrap().as_u64().unwrap(),
                    seg.pts as u64,
                    "{fixture} line {i} seg {si}: pts mismatch"
                );
                assert_eq!(
                    jseg.get("dts").unwrap().as_u64().unwrap(),
                    seg.dts as u64,
                    "{fixture} line {i} seg {si}: dts mismatch"
                );
                assert_eq!(
                    jseg.get("size").unwrap().as_u64().unwrap(),
                    seg.payload.len() as u64,
                    "{fixture} line {i} seg {si}: size mismatch"
                );

                let payload_b64 = jseg.get("payload").unwrap().as_str().unwrap();
                let decoded = base64_decode(payload_b64);
                assert_eq!(
                    decoded,
                    seg.payload,
                    "{fixture} line {i} seg {si}: payload mismatch ({} decoded bytes vs {} original)",
                    decoded.len(),
                    seg.payload.len()
                );
            }

            matched_keys.push(key);
        }

        // All batch display sets should have been matched.
        assert_eq!(
            matched_keys.len(),
            batch_by_key.len(),
            "{fixture}: matched {} display sets but batch has {}",
            matched_keys.len(),
            batch_by_key.len()
        );
    }
}

#[test]
fn ndjson_track_filter_matches_api() {
    let fixtures = available_fixtures();
    if fixtures.is_empty() { return; }

    for fixture in fixtures {
        let batch = libpgs::extract_all_display_sets(Path::new(fixture))
            .expect("batch extraction should succeed");
        if batch.is_empty() { continue; }

        let target = &batch[0];
        let tid = target.track.track_id;

        let lines = run_stream(fixture, Some(tid));
        let header = parse_json(&lines[0]);
        let json_tracks = header.get("tracks").unwrap().as_array().unwrap();

        // Track header should only contain the filtered track.
        assert_eq!(json_tracks.len(), 1, "{fixture}: expected 1 track in filtered output");
        assert_eq!(json_tracks[0].get("track_id").unwrap().as_u64().unwrap(), tid as u64);

        // Display set count should match.
        let ds_lines = &lines[1..];
        assert_eq!(
            ds_lines.len(),
            target.display_sets.len(),
            "{fixture}: filtered display set count mismatch"
        );

        // All display sets should be from the filtered track.
        for line in ds_lines {
            let json = parse_json(line);
            assert_eq!(json.get("track_id").unwrap().as_u64().unwrap(), tid as u64);
        }
    }
}
