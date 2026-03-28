//! Integration test: verify the CLI `stream` NDJSON output matches
//! the batch extraction API.
//!
//! Runs `libpgs stream` as a subprocess, parses the NDJSON lines, and
//! compares against `extract_all_display_sets`. This covers serialization,
//! field mapping, and the full CLI pipeline.
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
    FIXTURES
        .iter()
        .copied()
        .filter(|p| Path::new(p).exists())
        .collect()
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
                _ => {
                    s.push('\\');
                    s.push(b[j] as char);
                }
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
    while j < b.len()
        && (b[j].is_ascii_digit()
            || b[j] == b'.'
            || b[j] == b'-'
            || b[j] == b'e'
            || b[j] == b'E'
            || b[j] == b'+')
    {
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

fn composition_state_name(cs: libpgs::pgs::segment::CompositionState) -> &'static str {
    match cs {
        libpgs::pgs::segment::CompositionState::Normal => "normal",
        libpgs::pgs::segment::CompositionState::AcquisitionPoint => "acquisition_point",
        libpgs::pgs::segment::CompositionState::EpochStart => "epoch_start",
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
    stdout
        .lines()
        .filter(|l| !l.is_empty())
        .map(String::from)
        .collect()
}

/// Run `libpgs stream --raw-payloads` and collect NDJSON output lines.
fn run_stream_raw(fixture: &str) -> Vec<String> {
    let binary = env!("CARGO_BIN_EXE_libpgs");
    let output = Command::new(binary)
        .arg("stream")
        .arg(fixture)
        .arg("--raw-payloads")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to run libpgs stream --raw-payloads");

    assert!(
        output.status.success(),
        "libpgs stream --raw-payloads failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout).expect("invalid UTF-8");
    stdout
        .lines()
        .filter(|l| !l.is_empty())
        .map(String::from)
        .collect()
}

#[test]
fn ndjson_tracks_header_matches_api() {
    let fixtures = available_fixtures();
    if fixtures.is_empty() {
        return;
    }

    for fixture in fixtures {
        let api_tracks =
            libpgs::list_pgs_tracks(Path::new(fixture)).expect("list_pgs_tracks should succeed");

        let lines = run_stream(fixture, None);
        assert!(
            !lines.is_empty(),
            "{fixture}: no output from stream command"
        );

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

            // Renamed field checks.
            let json_name = jt.get("name").unwrap();
            match &at.name {
                Some(name) => assert_eq!(
                    json_name.as_str().unwrap(),
                    name.as_str(),
                    "{fixture}: name mismatch"
                ),
                None => assert!(json_name.is_null(), "{fixture}: expected null name"),
            }

            let json_default = jt.get("is_default").unwrap();
            match at.flag_default {
                Some(v) => assert_eq!(
                    json_default.as_bool().unwrap(),
                    v,
                    "{fixture}: is_default mismatch"
                ),
                None => assert!(
                    json_default.is_null(),
                    "{fixture}: expected null is_default"
                ),
            }

            let json_forced = jt.get("is_forced").unwrap();
            match at.flag_forced {
                Some(v) => assert_eq!(
                    json_forced.as_bool().unwrap(),
                    v,
                    "{fixture}: is_forced mismatch"
                ),
                None => assert!(json_forced.is_null(), "{fixture}: expected null is_forced"),
            }

            let json_count = jt.get("display_set_count").unwrap();
            match at.display_set_count {
                Some(v) => assert_eq!(
                    json_count.as_u64().unwrap(),
                    v,
                    "{fixture}: display_set_count mismatch"
                ),
                None => assert!(
                    json_count.is_null(),
                    "{fixture}: expected null display_set_count"
                ),
            }

            let json_indexed = jt.get("indexed").unwrap();
            match at.has_cues {
                Some(v) => assert_eq!(
                    json_indexed.as_bool().unwrap(),
                    v,
                    "{fixture}: indexed mismatch"
                ),
                None => assert!(json_indexed.is_null(), "{fixture}: expected null indexed"),
            }
        }
    }
}

#[test]
fn ndjson_display_sets_match_batch_extraction() {
    let fixtures = available_fixtures();
    if fixtures.is_empty() {
        return;
    }

    for fixture in fixtures {
        let batch = libpgs::extract_all_display_sets(Path::new(fixture))
            .expect("batch extraction should succeed");

        let lines = run_stream(fixture, None);
        let ds_lines: Vec<&str> = lines[1..].iter().map(|s| s.as_str()).collect();

        let batch_total: usize = batch.iter().map(|t| t.display_sets.len()).sum();
        assert_eq!(
            ds_lines.len(),
            batch_total,
            "{fixture}: display set count mismatch (NDJSON={}, batch={batch_total})",
            ds_lines.len()
        );

        // Build per-track lookup keyed by (track_id, pts).
        let mut batch_by_key: HashMap<
            (u32, u64),
            (&libpgs::PgsTrackInfo, &libpgs::pgs::display_set::DisplaySet),
        > = HashMap::new();
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
            let &(_track, ds) = batch_by_key.get(&key).unwrap_or_else(|| {
                panic!("{fixture} line {i}: no batch match for track={tid} pts={pts}")
            });

            // Verify index is sequential per track.
            let json_index = json.get("index").unwrap().as_u64().unwrap();
            let expected_index = track_index_counters.entry(tid).or_insert(0);
            assert_eq!(
                json_index, *expected_index,
                "{fixture} line {i}: index mismatch for track {tid}"
            );
            *expected_index += 1;

            // pts and pts_ms.
            assert_eq!(pts, ds.pts as u64, "{fixture} line {i}: pts mismatch");

            // Composition state — now in composition.state with snake_case.
            let comp = json.get("composition").unwrap();
            if !comp.is_null() {
                assert_eq!(
                    comp.get("state").unwrap().as_str().unwrap(),
                    composition_state_name(ds.composition_state),
                    "{fixture} line {i}: composition state mismatch"
                );
            }

            // Verify semantic arrays are present.
            assert!(
                json.get("windows").is_some(),
                "{fixture} line {i}: missing windows"
            );
            assert!(
                json.get("palettes").is_some(),
                "{fixture} line {i}: missing palettes"
            );
            assert!(
                json.get("objects").is_some(),
                "{fixture} line {i}: missing objects"
            );

            // Verify segment counts by type match.
            use libpgs::pgs::segment::SegmentType;
            let pcs_count = ds
                .segments
                .iter()
                .filter(|s| s.segment_type == SegmentType::PresentationComposition)
                .count();
            let wds_window_count: usize = ds
                .segments
                .iter()
                .filter(|s| s.segment_type == SegmentType::WindowDefinition)
                .filter_map(|s| s.parse_wds())
                .map(|w| w.windows.len())
                .sum();
            let pds_count = ds
                .segments
                .iter()
                .filter(|s| s.segment_type == SegmentType::PaletteDefinition)
                .count();
            // Count unique object IDs (fragments are grouped).
            let mut ods_ids: Vec<u16> = Vec::new();
            for seg in ds.segments.iter().filter(|s| s.segment_type == SegmentType::ObjectDefinition) {
                if let Some(ods) = seg.parse_ods() {
                    if !ods_ids.contains(&ods.id) {
                        ods_ids.push(ods.id);
                    }
                }
            }
            let ods_count = ods_ids.len();

            if pcs_count > 0 {
                assert!(
                    !comp.is_null(),
                    "{fixture} line {i}: composition should not be null"
                );
            }
            assert_eq!(
                json.get("windows")
                    .unwrap()
                    .as_array()
                    .unwrap_or(&[])
                    .len(),
                wds_window_count,
                "{fixture} line {i}: window count mismatch"
            );
            assert_eq!(
                json.get("palettes")
                    .unwrap()
                    .as_array()
                    .unwrap_or(&[])
                    .len(),
                pds_count,
                "{fixture} line {i}: palette count mismatch"
            );
            assert_eq!(
                json.get("objects")
                    .unwrap()
                    .as_array()
                    .unwrap_or(&[])
                    .len(),
                ods_count,
                "{fixture} line {i}: object count mismatch"
            );

            matched_keys.push(key);
        }

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
    if fixtures.is_empty() {
        return;
    }

    for fixture in fixtures {
        let batch = libpgs::extract_all_display_sets(Path::new(fixture))
            .expect("batch extraction should succeed");
        if batch.is_empty() {
            continue;
        }

        let target = &batch[0];
        let tid = target.track.track_id;

        let lines = run_stream(fixture, Some(tid));
        let header = parse_json(&lines[0]);
        let json_tracks = header.get("tracks").unwrap().as_array().unwrap();

        // Track header should only contain the filtered track.
        assert_eq!(
            json_tracks.len(),
            1,
            "{fixture}: expected 1 track in filtered output"
        );
        assert_eq!(
            json_tracks[0].get("track_id").unwrap().as_u64().unwrap(),
            tid as u64
        );

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

#[test]
fn ndjson_raw_payloads_includes_base64() {
    let fixtures = available_fixtures();
    if fixtures.is_empty() {
        return;
    }

    for fixture in fixtures {
        let lines = run_stream_raw(fixture);
        if lines.len() < 2 {
            continue;
        }

        // Check a display set line has payload fields.
        let json = parse_json(&lines[1]);
        assert_eq!(json.get("type").unwrap().as_str().unwrap(), "display_set");

        // Composition should have a payload field.
        let comp = json.get("composition").unwrap();
        if !comp.is_null() {
            let payload = comp.get("payload");
            assert!(
                payload.is_some(),
                "{fixture}: composition missing payload in --raw-payloads mode"
            );
            assert!(
                payload.unwrap().as_str().is_some(),
                "{fixture}: composition payload should be a string"
            );
        }

        // Objects should have payload fields.
        let objects = json.get("objects").unwrap().as_array().unwrap();
        for obj in objects {
            let payload = obj.get("payload");
            assert!(
                payload.is_some(),
                "{fixture}: object missing payload in --raw-payloads mode"
            );
        }
    }
}

#[test]
fn ndjson_default_mode_no_payload() {
    let fixtures = available_fixtures();
    if fixtures.is_empty() {
        return;
    }

    for fixture in fixtures {
        let lines = run_stream(fixture, None);
        if lines.len() < 2 {
            continue;
        }

        // Check that default mode does NOT include payload fields.
        let json = parse_json(&lines[1]);
        let comp = json.get("composition").unwrap();
        if !comp.is_null() {
            assert!(
                comp.get("payload").is_none(),
                "{fixture}: composition should not have payload in default mode"
            );
        }

        let objects = json.get("objects").unwrap().as_array().unwrap();
        for obj in objects {
            assert!(
                obj.get("payload").is_none(),
                "{fixture}: object should not have payload in default mode"
            );
        }
    }
}

/// Simple base64 decoder for tests.
fn base64_decode(s: &str) -> Vec<u8> {
    const TABLE: [u8; 128] = {
        let mut t = [255u8; 128];
        let mut i = 0u8;
        while i < 26 {
            t[(b'A' + i) as usize] = i;
            t[(b'a' + i) as usize] = i + 26;
            i += 1;
        }
        let mut d = 0u8;
        while d < 10 {
            t[(b'0' + d) as usize] = d + 52;
            d += 1;
        }
        t[b'+' as usize] = 62;
        t[b'/' as usize] = 63;
        t
    };
    let bytes: Vec<u8> = s.bytes().filter(|&b| b != b'=').collect();
    let mut out = Vec::with_capacity(bytes.len() * 3 / 4);
    for chunk in bytes.chunks(4) {
        let mut buf = [0u32; 4];
        for (i, &b) in chunk.iter().enumerate() {
            buf[i] = TABLE[b as usize] as u32;
        }
        let triple = (buf[0] << 18) | (buf[1] << 12) | (buf[2] << 6) | buf[3];
        out.push((triple >> 16) as u8);
        if chunk.len() > 2 {
            out.push((triple >> 8) as u8);
        }
        if chunk.len() > 3 {
            out.push(triple as u8);
        }
    }
    out
}

#[test]
fn ndjson_bitmap_field_present() {
    let fixtures = available_fixtures();
    if fixtures.is_empty() {
        return;
    }

    for fixture in fixtures {
        let lines = run_stream(fixture, None);
        let mut checked = 0;

        for line in &lines[1..] {
            let json = parse_json(line);
            let objects = json.get("objects").unwrap().as_array().unwrap();
            for obj in objects {
                let bitmap = obj.get("bitmap");
                assert!(
                    bitmap.is_some(),
                    "{fixture}: object missing bitmap field"
                );

                let w = obj.get("width");
                let h = obj.get("height");
                if let (Some(w), Some(h)) = (w, h) {
                    let w = w.as_u64().unwrap() as usize;
                    let h = h.as_u64().unwrap() as usize;
                    let bm = bitmap.unwrap();
                    if !bm.is_null() {
                        let decoded = base64_decode(bm.as_str().unwrap());
                        assert_eq!(
                            decoded.len(),
                            w * h,
                            "{fixture}: bitmap size mismatch: got {} expected {}",
                            decoded.len(),
                            w * h,
                        );
                        checked += 1;
                    }
                }
            }
        }

        // Ensure we actually checked some bitmaps (at least for MKV fixtures).
        if fixture.ends_with(".mkv") {
            assert!(
                checked > 0,
                "{fixture}: expected at least one decoded bitmap"
            );
        }
    }
}
