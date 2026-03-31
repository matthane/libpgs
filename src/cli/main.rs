use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::process;
use std::time::Instant;

fn main() {
    let args: Vec<String> = std::env::args().collect();

    if args.len() < 2 {
        print_usage();
        process::exit(1);
    }

    let result = match args[1].as_str() {
        "tracks" => cmd_tracks(&args[2..]),
        "extract" => cmd_extract(&args[2..]),
        "stream" => cmd_stream(&args[2..]),
        "encode" => cmd_encode(&args[2..]),
        "bench" => cmd_bench(&args[2..]),
        "help" | "--help" | "-h" => {
            print_usage();
            Ok(())
        }
        other => {
            eprintln!("Unknown command: {other}");
            print_usage();
            process::exit(1);
        }
    };

    if let Err(e) = result {
        eprintln!("Error: {e}");
        process::exit(1);
    }
}

fn print_usage() {
    eprintln!("libpgs - Fast PGS subtitle extraction and encoding\n");
    eprintln!("Usage:");
    eprintln!("  libpgs tracks <file>                      List PGS tracks");
    eprintln!("  libpgs extract <file> -o <output.sup>     Extract all PGS tracks");
    eprintln!("  libpgs extract <file> -t <id> -o <out>    Extract specific track");
    eprintln!("  libpgs stream <file> [-t <id>] [--raw-payloads]  Stream PGS data as NDJSON");
    eprintln!("  libpgs encode -o <output.sup>             Encode NDJSON from stdin to .sup");
    eprintln!("  libpgs bench <file>                       Benchmark I/O efficiency");
    eprintln!("  libpgs help                               Show this help");
    eprintln!();
    eprintln!("Time range options (extract and stream commands):");
    eprintln!("  --start TIME    Start extracting from this time (HH:MM:SS, MM:SS, or seconds)");
    eprintln!("  --end TIME      Stop extracting after this time");
    eprintln!();
    eprintln!("When extracting all tracks, output files are named <stem>_track<id>.sup");
}

fn cmd_tracks(args: &[String]) -> Result<(), libpgs::error::PgsError> {
    if args.is_empty() {
        eprintln!("Usage: libpgs tracks <file>");
        process::exit(1);
    }

    let path = PathBuf::from(&args[0]);
    let tracks = libpgs::list_pgs_tracks(&path)?;

    if tracks.is_empty() {
        println!("No PGS tracks found.");
        return Ok(());
    }

    println!("PGS tracks found:");
    for track in &tracks {
        let lang = track.language.as_deref().unwrap_or("unknown");
        let mut extras = Vec::new();
        if let Some(name) = &track.name {
            extras.push(format!("name=\"{name}\""));
        }
        if let Some(true) = track.flag_default {
            extras.push("default".to_string());
        }
        if let Some(true) = track.flag_forced {
            extras.push("forced".to_string());
        }
        if let Some(count) = track.display_set_count {
            extras.push(format!("display_sets={count}"));
        }
        if let Some(cues) = track.has_cues {
            extras.push(format!("has_cues={cues}"));
        }
        let extra_str = if extras.is_empty() {
            String::new()
        } else {
            format!(", {}", extras.join(", "))
        };
        println!(
            "  Track {}: language={}, format={:?}{}",
            track.track_id, lang, track.container, extra_str
        );
    }

    Ok(())
}

fn cmd_bench(args: &[String]) -> Result<(), libpgs::error::PgsError> {
    if args.is_empty() {
        eprintln!("Usage: libpgs bench <file> [--strategy auto|sequential]");
        process::exit(1);
    }

    let path = PathBuf::from(&args[0]);

    let mut strategy = libpgs::MkvStrategy::Auto;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--strategy" | "-s" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("Missing value for --strategy");
                    process::exit(1);
                }
                strategy = match args[i].as_str() {
                    "auto" => libpgs::MkvStrategy::Auto,
                    "sequential" => libpgs::MkvStrategy::Sequential,
                    other => {
                        eprintln!("Unknown strategy: {other} (use auto, sequential)");
                        process::exit(1);
                    }
                };
            }
            other => {
                eprintln!("Unknown option: {other}");
                process::exit(1);
            }
        }
        i += 1;
    }

    let strategy_name = match strategy {
        libpgs::MkvStrategy::Auto => "auto",
        libpgs::MkvStrategy::Sequential => "sequential",
    };

    let start = Instant::now();

    let mut extractor = libpgs::Extractor::open(&path)?;
    if strategy != libpgs::MkvStrategy::Auto {
        extractor = extractor.with_mkv_strategy(strategy);
    }

    let track_info: Vec<_> = extractor.tracks().to_vec();
    let results: Vec<_> = extractor.by_ref().collect::<Result<Vec<_>, _>>()?;
    let stats = extractor.stats().clone();
    let elapsed = start.elapsed();

    // Group results by track for display.
    let mut track_ds_counts: std::collections::HashMap<u32, (usize, usize)> =
        std::collections::HashMap::new();
    for tds in &results {
        let entry = track_ds_counts.entry(tds.track_id).or_insert((0, 0));
        entry.0 += 1;
        entry.1 += tds.display_set.segments.len();
    }

    let total_display_sets = results.len();
    let total_segments: usize = results
        .iter()
        .map(|tds| tds.display_set.segments.len())
        .sum();
    let ratio = if stats.file_size > 0 {
        (stats.bytes_read as f64 / stats.file_size as f64) * 100.0
    } else {
        0.0
    };

    println!("File:          {}", path.display());
    println!("Strategy:      {}", strategy_name);
    println!(
        "File size:     {} ({:.2} MB)",
        stats.file_size,
        stats.file_size as f64 / (1024.0 * 1024.0)
    );
    println!(
        "Bytes read:    {} ({:.2} MB)",
        stats.bytes_read,
        stats.bytes_read as f64 / (1024.0 * 1024.0)
    );
    println!("Read ratio:    {:.2}%", ratio);
    println!("Tracks:        {}", track_info.len());
    for t in &track_info {
        let lang = t.language.as_deref().unwrap_or("unknown");
        if let Some(&(ds_count, seg_count)) = track_ds_counts.get(&t.track_id) {
            println!(
                "  Track {} ({}): {} display sets, {} segments",
                t.track_id, lang, ds_count, seg_count
            );
        }
    }
    println!(
        "Total:         {} display sets, {} segments",
        total_display_sets, total_segments
    );
    println!("Time:          {:.3}s", elapsed.as_secs_f64());

    Ok(())
}

/// Parse a comma-separated list of track IDs (e.g. "3", "3,5,8").
fn parse_track_ids(value: &str) -> Vec<u32> {
    value
        .split(',')
        .map(|s| {
            s.trim().parse().unwrap_or_else(|_| {
                eprintln!("Invalid track ID: {}", s.trim());
                process::exit(1);
            })
        })
        .collect()
}

/// Parse a timestamp string into milliseconds.
///
/// Accepts: `HH:MM:SS.mmm`, `MM:SS.mmm`, `SS.mmm`, or plain seconds (e.g. `123.456`).
fn parse_timestamp(s: &str) -> Option<f64> {
    let parts: Vec<&str> = s.split(':').collect();
    match parts.len() {
        1 => {
            // Plain seconds: "123.456"
            let secs: f64 = parts[0].parse().ok()?;
            Some(secs * 1000.0)
        }
        2 => {
            // MM:SS or MM:SS.mmm
            let mins: f64 = parts[0].parse().ok()?;
            let secs: f64 = parts[1].parse().ok()?;
            Some((mins * 60.0 + secs) * 1000.0)
        }
        3 => {
            // HH:MM:SS or HH:MM:SS.mmm
            let hours: f64 = parts[0].parse().ok()?;
            let mins: f64 = parts[1].parse().ok()?;
            let secs: f64 = parts[2].parse().ok()?;
            Some((hours * 3600.0 + mins * 60.0 + secs) * 1000.0)
        }
        _ => None,
    }
}

fn cmd_stream(args: &[String]) -> Result<(), libpgs::error::PgsError> {
    if args.is_empty() {
        eprintln!("Usage: libpgs stream <file> [-t <id>] [--start TIME] [--end TIME] [--raw-payloads]");
        process::exit(1);
    }

    let input = PathBuf::from(&args[0]);
    let mut track_ids: Vec<u32> = Vec::new();
    let mut raw_payloads = false;
    let mut start_ms: Option<f64> = None;
    let mut end_ms: Option<f64> = None;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "-t" | "--track" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("Missing value for -t");
                    process::exit(1);
                }
                track_ids.extend(parse_track_ids(&args[i]));
            }
            "--raw-payloads" => {
                raw_payloads = true;
            }
            "--start" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("Missing value for --start");
                    process::exit(1);
                }
                start_ms = Some(parse_timestamp(&args[i]).unwrap_or_else(|| {
                    eprintln!("Invalid timestamp for --start: {}", args[i]);
                    process::exit(1);
                }));
            }
            "--end" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("Missing value for --end");
                    process::exit(1);
                }
                end_ms = Some(parse_timestamp(&args[i]).unwrap_or_else(|| {
                    eprintln!("Invalid timestamp for --end: {}", args[i]);
                    process::exit(1);
                }));
            }
            other => {
                eprintln!("Unknown option: {other}");
                process::exit(1);
            }
        }
        i += 1;
    }

    let mut extractor = libpgs::Extractor::open(&input)?;
    if !track_ids.is_empty() {
        extractor = extractor.with_track_filter(&track_ids);
    }
    if start_ms.is_some() || end_ms.is_some() {
        extractor = extractor.with_time_range(start_ms, end_ms);
    }

    let stdout = std::io::stdout();
    let mut out = stdout.lock();

    // Stream tracks header + display sets as NDJSON.
    // If the consumer closes the pipe (BrokenPipe), exit cleanly.
    match stream_ndjson(&mut out, &mut extractor, raw_payloads) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
        Err(e) => Err(e.into()),
    }
}

fn stream_ndjson(
    out: &mut impl Write,
    extractor: &mut libpgs::Extractor,
    raw_payloads: bool,
) -> std::io::Result<()> {
    // Emit tracks header as first line.
    let tracks = extractor.tracks();
    write!(out, "{{\"type\":\"tracks\",\"tracks\":[")?;
    for (ti, track) in tracks.iter().enumerate() {
        if ti > 0 {
            write!(out, ",")?;
        }
        write!(
            out,
            "{{\"track_id\":{},\"language\":{},\"container\":\"{}\",\
             \"name\":{},\"is_default\":{},\"is_forced\":{},\"display_set_count\":{},\
             \"indexed\":{}}}",
            track.track_id,
            json_string_or_null(track.language.as_deref()),
            container_name(track.container),
            json_string_or_null(track.name.as_deref()),
            json_bool_or_null(track.flag_default),
            json_bool_or_null(track.flag_forced),
            json_u64_or_null(track.display_set_count),
            json_bool_or_null(track.has_cues),
        )?;
    }
    writeln!(out, "]}}")?;
    out.flush()?;

    // Per-track index counter for display set sequence numbers.
    let mut track_indices: std::collections::HashMap<u32, u64> = std::collections::HashMap::new();

    // Stream display sets, one line per display set.
    for result in extractor {
        let tds = result.map_err(std::io::Error::other)?;
        let ds = &tds.display_set;
        let index = track_indices.entry(tds.track_id).or_insert(0);
        let current_index = *index;
        *index += 1;

        write!(
            out,
            "{{\"type\":\"display_set\",\"track_id\":{},\"index\":{},\
             \"pts\":{},\"pts_ms\":{:.4},",
            tds.track_id, current_index, ds.pts, ds.pts_ms,
        )?;

        // -- composition (from PCS) --
        write_composition(out, &ds.segments, raw_payloads)?;
        write!(out, ",")?;

        // -- windows (from WDS) --
        write_windows(out, &ds.segments, raw_payloads)?;
        write!(out, ",")?;

        // -- palettes (from PDS) --
        write_palettes(out, &ds.segments, raw_payloads)?;
        write!(out, ",")?;

        // -- objects (from ODS) --
        write_objects(out, &ds.segments, raw_payloads)?;

        writeln!(out, "}}")?;
        out.flush()?;
    }

    Ok(())
}

fn write_composition(
    out: &mut impl Write,
    segments: &[libpgs::pgs::PgsSegment],
    raw_payloads: bool,
) -> std::io::Result<()> {
    use libpgs::pgs::segment::SegmentType;

    let pcs_seg = segments
        .iter()
        .find(|s| s.segment_type == SegmentType::PresentationComposition);

    let Some(seg) = pcs_seg else {
        write!(out, "\"composition\":null")?;
        return Ok(());
    };

    let Some(pcs) = seg.parse_pcs() else {
        if raw_payloads {
            write!(
                out,
                "\"composition\":{{\"payload\":\"{}\"}}",
                base64_encode(&seg.payload)
            )?;
        } else {
            write!(out, "\"composition\":null")?;
        }
        return Ok(());
    };

    write!(
        out,
        "\"composition\":{{\"number\":{},\"state\":\"{}\",\
         \"video_width\":{},\"video_height\":{},\
         \"palette_only\":{},\"palette_id\":{},\"objects\":[",
        pcs.composition_number,
        composition_state_name(pcs.composition_state),
        pcs.video_width,
        pcs.video_height,
        pcs.palette_only,
        pcs.palette_id,
    )?;

    for (ci, obj) in pcs.objects.iter().enumerate() {
        if ci > 0 {
            write!(out, ",")?;
        }
        write!(
            out,
            "{{\"object_id\":{},\"window_id\":{},\"x\":{},\"y\":{},\"crop\":",
            obj.object_id, obj.window_id, obj.x, obj.y,
        )?;
        match &obj.crop {
            Some(crop) => write!(
                out,
                "{{\"x\":{},\"y\":{},\"width\":{},\"height\":{}}}",
                crop.x, crop.y, crop.width, crop.height,
            )?,
            None => write!(out, "null")?,
        }
        write!(out, "}}")?;
    }

    write!(out, "]")?;
    if raw_payloads {
        write!(out, ",\"payload\":\"{}\"", base64_encode(&seg.payload))?;
    }
    write!(out, "}}")?;

    Ok(())
}

fn write_windows(
    out: &mut impl Write,
    segments: &[libpgs::pgs::PgsSegment],
    raw_payloads: bool,
) -> std::io::Result<()> {
    use libpgs::pgs::segment::SegmentType;

    write!(out, "\"windows\":[")?;
    let mut first = true;
    for seg in segments
        .iter()
        .filter(|s| s.segment_type == SegmentType::WindowDefinition)
    {
        if let Some(wds) = seg.parse_wds() {
            for win in &wds.windows {
                if !first {
                    write!(out, ",")?;
                }
                first = false;
                write!(
                    out,
                    "{{\"id\":{},\"x\":{},\"y\":{},\"width\":{},\"height\":{}",
                    win.id, win.x, win.y, win.width, win.height,
                )?;
                if raw_payloads {
                    write!(out, ",\"payload\":\"{}\"", base64_encode(&seg.payload))?;
                }
                write!(out, "}}")?;
            }
        }
    }
    write!(out, "]")?;

    Ok(())
}

fn write_palettes(
    out: &mut impl Write,
    segments: &[libpgs::pgs::PgsSegment],
    raw_payloads: bool,
) -> std::io::Result<()> {
    use libpgs::pgs::segment::SegmentType;

    write!(out, "\"palettes\":[")?;
    let mut first = true;
    for seg in segments
        .iter()
        .filter(|s| s.segment_type == SegmentType::PaletteDefinition)
    {
        if let Some(pds) = seg.parse_pds() {
            if !first {
                write!(out, ",")?;
            }
            first = false;
            write!(
                out,
                "{{\"id\":{},\"version\":{},\"entries\":[",
                pds.id, pds.version,
            )?;
            for (ei, entry) in pds.entries.iter().enumerate() {
                if ei > 0 {
                    write!(out, ",")?;
                }
                write!(
                    out,
                    "{{\"id\":{},\"luminance\":{},\"cr\":{},\"cb\":{},\"alpha\":{}}}",
                    entry.id, entry.luminance, entry.cr, entry.cb, entry.alpha,
                )?;
            }
            write!(out, "]")?;
            if raw_payloads {
                write!(out, ",\"payload\":\"{}\"", base64_encode(&seg.payload))?;
            }
            write!(out, "}}")?;
        }
    }
    write!(out, "]")?;

    Ok(())
}

fn write_objects(
    out: &mut impl Write,
    segments: &[libpgs::pgs::PgsSegment],
    raw_payloads: bool,
) -> std::io::Result<()> {
    use libpgs::pgs::payload::ods_rle_data;
    use libpgs::pgs::rle::decode_rle;
    use libpgs::pgs::segment::SegmentType;

    // Collect ODS segments with parsed data.
    let ods_segments: Vec<_> = segments
        .iter()
        .filter(|s| s.segment_type == SegmentType::ObjectDefinition)
        .filter_map(|seg| seg.parse_ods().map(|ods| (seg, ods)))
        .collect();

    // Group by object ID, preserving first-appearance order.
    let mut groups: Vec<(u16, Vec<(&libpgs::pgs::PgsSegment, libpgs::pgs::OdsData)>)> = Vec::new();
    for (seg, ods) in ods_segments {
        if let Some(group) = groups.iter_mut().find(|(id, _)| *id == ods.id) {
            group.1.push((seg, ods));
        } else {
            groups.push((ods.id, vec![(seg, ods)]));
        }
    }

    write!(out, "\"objects\":[")?;
    for (gi, (_obj_id, fragments)) in groups.iter().enumerate() {
        if gi > 0 {
            write!(out, ",")?;
        }

        // Get metadata from the first/complete fragment.
        let first_ods = &fragments[0].1;
        let is_reassembled = fragments.len() > 1;
        let sequence_str = if is_reassembled {
            "reassembled"
        } else {
            first_ods.sequence.as_str()
        };

        write!(
            out,
            "{{\"id\":{},\"version\":{},\"sequence\":\"{}\",\"data_length\":{}",
            first_ods.id, first_ods.version, sequence_str, first_ods.data_length,
        )?;

        // Width/height from Complete or First fragment.
        let width = first_ods.width;
        let height = first_ods.height;
        if let Some(w) = width {
            write!(out, ",\"width\":{}", w)?;
        }
        if let Some(h) = height {
            write!(out, ",\"height\":{}", h)?;
        }

        // Decode bitmap: concatenate RLE data from all fragments, then decode.
        if let (Some(w), Some(h)) = (width, height) {
            let mut rle_data = Vec::new();
            let mut rle_ok = true;
            for (seg, ods) in fragments {
                if let Some(data) = ods_rle_data(&seg.payload, ods.sequence) {
                    rle_data.extend_from_slice(data);
                } else {
                    rle_ok = false;
                    break;
                }
            }
            if rle_ok {
                if let Some(pixels) = decode_rle(&rle_data, w, h) {
                    write!(out, ",\"bitmap\":\"{}\"", base64_encode(&pixels))?;
                } else {
                    write!(out, ",\"bitmap\":null")?;
                }
            } else {
                write!(out, ",\"bitmap\":null")?;
            }
        } else {
            write!(out, ",\"bitmap\":null")?;
        }

        if raw_payloads {
            // For reassembled objects, concatenate all raw payloads.
            if fragments.len() == 1 {
                write!(
                    out,
                    ",\"payload\":\"{}\"",
                    base64_encode(&fragments[0].0.payload)
                )?;
            } else {
                let mut combined = Vec::new();
                for (seg, _) in fragments {
                    combined.extend_from_slice(&seg.payload);
                }
                write!(out, ",\"payload\":\"{}\"", base64_encode(&combined))?;
            }
        }
        write!(out, "}}")?;
    }
    write!(out, "]")?;

    Ok(())
}

fn json_string_or_null(s: Option<&str>) -> String {
    match s {
        Some(v) => {
            let escaped = v
                .replace('\\', "\\\\")
                .replace('"', "\\\"")
                .replace('\n', "\\n")
                .replace('\r', "\\r")
                .replace('\t', "\\t");
            format!("\"{escaped}\"")
        }
        None => "null".to_string(),
    }
}

fn json_bool_or_null(b: Option<bool>) -> &'static str {
    match b {
        Some(true) => "true",
        Some(false) => "false",
        None => "null",
    }
}

fn json_u64_or_null(n: Option<u64>) -> String {
    match n {
        Some(v) => v.to_string(),
        None => "null".to_string(),
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

fn composition_state_name(cs: libpgs::pgs::segment::CompositionState) -> &'static str {
    match cs {
        libpgs::pgs::segment::CompositionState::Normal => "normal",
        libpgs::pgs::segment::CompositionState::AcquisitionPoint => "acquisition_point",
        libpgs::pgs::segment::CompositionState::EpochStart => "epoch_start",
    }
}

const BASE64_CHARS: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn base64_encode(data: &[u8]) -> String {
    let mut encoded = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let triple = (b0 << 16) | (b1 << 8) | b2;

        encoded.push(BASE64_CHARS[((triple >> 18) & 0x3F) as usize] as char);
        encoded.push(BASE64_CHARS[((triple >> 12) & 0x3F) as usize] as char);

        if chunk.len() > 1 {
            encoded.push(BASE64_CHARS[((triple >> 6) & 0x3F) as usize] as char);
        } else {
            encoded.push('=');
        }
        if chunk.len() > 2 {
            encoded.push(BASE64_CHARS[(triple & 0x3F) as usize] as char);
        } else {
            encoded.push('=');
        }
    }
    encoded
}

// ---------------------------------------------------------------------------
// encode command — JSON parser, base64 decoder, field extraction
// ---------------------------------------------------------------------------

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

    fn as_u16(&self) -> Option<u16> {
        self.as_f64().map(|n| n as u16)
    }

    fn as_u8(&self) -> Option<u8> {
        self.as_f64().map(|n| n as u8)
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

fn json_parse(s: &str) -> Result<JsonValue, String> {
    let bytes = s.as_bytes();
    let i = json_skip_ws(bytes, 0);
    if i >= bytes.len() {
        return Err("empty JSON input".into());
    }
    let (val, _) = json_parse_value(bytes, i)?;
    Ok(val)
}

fn json_skip_ws(b: &[u8], mut i: usize) -> usize {
    while i < b.len() && matches!(b[i], b' ' | b'\t' | b'\r' | b'\n') {
        i += 1;
    }
    i
}

fn json_parse_value(b: &[u8], i: usize) -> Result<(JsonValue, usize), String> {
    if i >= b.len() {
        return Err("unexpected end of input".into());
    }
    match b[i] {
        b'"' => json_parse_string(b, i),
        b'{' => json_parse_object(b, i),
        b'[' => json_parse_array(b, i),
        b'n' => {
            if b.len() < i + 4 || &b[i..i + 4] != b"null" {
                return Err(format!("expected 'null' at offset {i}"));
            }
            Ok((JsonValue::Null, i + 4))
        }
        b't' => {
            if b.len() < i + 4 || &b[i..i + 4] != b"true" {
                return Err(format!("expected 'true' at offset {i}"));
            }
            Ok((JsonValue::Bool(true), i + 4))
        }
        b'f' => {
            if b.len() < i + 5 || &b[i..i + 5] != b"false" {
                return Err(format!("expected 'false' at offset {i}"));
            }
            Ok((JsonValue::Bool(false), i + 5))
        }
        c if c.is_ascii_digit() || c == b'-' => json_parse_number(b, i),
        c => Err(format!("unexpected character '{}' at offset {i}", c as char)),
    }
}

fn json_parse_string(b: &[u8], i: usize) -> Result<(JsonValue, usize), String> {
    if b[i] != b'"' {
        return Err(format!("expected '\"' at offset {i}"));
    }
    let mut j = i + 1;
    let mut s = String::new();
    while j < b.len() && b[j] != b'"' {
        if b[j] == b'\\' {
            j += 1;
            if j >= b.len() {
                return Err("unterminated string escape".into());
            }
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
    if j >= b.len() {
        return Err("unterminated string".into());
    }
    Ok((JsonValue::Str(s), j + 1))
}

fn json_parse_number(b: &[u8], i: usize) -> Result<(JsonValue, usize), String> {
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
    let s = std::str::from_utf8(&b[i..j]).map_err(|_| format!("invalid number at offset {i}"))?;
    let n: f64 = s
        .parse()
        .map_err(|_| format!("invalid number '{s}' at offset {i}"))?;
    Ok((JsonValue::Num(n), j))
}

fn json_parse_array(b: &[u8], i: usize) -> Result<(JsonValue, usize), String> {
    let mut j = json_skip_ws(b, i + 1);
    let mut items = Vec::new();
    if j < b.len() && b[j] == b']' {
        return Ok((JsonValue::Array(items), j + 1));
    }
    loop {
        let (val, next) = json_parse_value(b, j)?;
        items.push(val);
        j = json_skip_ws(b, next);
        if j >= b.len() {
            return Err("unterminated array".into());
        }
        if b[j] == b']' {
            return Ok((JsonValue::Array(items), j + 1));
        }
        if b[j] != b',' {
            return Err(format!("expected ',' or ']' at offset {j}"));
        }
        j = json_skip_ws(b, j + 1);
    }
}

fn json_parse_object(b: &[u8], i: usize) -> Result<(JsonValue, usize), String> {
    let mut j = json_skip_ws(b, i + 1);
    let mut pairs = Vec::new();
    if j < b.len() && b[j] == b'}' {
        return Ok((JsonValue::Object(pairs), j + 1));
    }
    loop {
        let (key_val, next) = json_parse_string(b, j)?;
        let key = match key_val {
            JsonValue::Str(s) => s,
            _ => unreachable!(),
        };
        j = json_skip_ws(b, next);
        if j >= b.len() || b[j] != b':' {
            return Err(format!("expected ':' after key '{key}' at offset {j}"));
        }
        j = json_skip_ws(b, j + 1);
        let (val, next) = json_parse_value(b, j)?;
        pairs.push((key, val));
        j = json_skip_ws(b, next);
        if j >= b.len() {
            return Err("unterminated object".into());
        }
        if b[j] == b'}' {
            return Ok((JsonValue::Object(pairs), j + 1));
        }
        if b[j] != b',' {
            return Err(format!("expected ',' or '}}' at offset {j}"));
        }
        j = json_skip_ws(b, j + 1);
    }
}

fn base64_decode(s: &str) -> Result<Vec<u8>, String> {
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
            if b >= 128 || TABLE[b as usize] == 255 {
                return Err(format!("invalid base64 character: '{}'", b as char));
            }
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
    Ok(out)
}

fn require_field<'a>(obj: &'a JsonValue, key: &str, line: usize) -> Result<&'a JsonValue, String> {
    obj.get(key)
        .ok_or_else(|| format!("line {line}: missing field '{key}'"))
}

fn require_u16(obj: &JsonValue, key: &str, line: usize) -> Result<u16, String> {
    require_field(obj, key, line)?
        .as_u16()
        .ok_or_else(|| format!("line {line}: field '{key}' is not a number"))
}

fn require_u8(obj: &JsonValue, key: &str, line: usize) -> Result<u8, String> {
    require_field(obj, key, line)?
        .as_u8()
        .ok_or_else(|| format!("line {line}: field '{key}' is not a number"))
}

fn parse_composition_state_str(s: &str) -> Option<libpgs::pgs::segment::CompositionState> {
    match s {
        "normal" => Some(libpgs::pgs::segment::CompositionState::Normal),
        "acquisition_point" => Some(libpgs::pgs::segment::CompositionState::AcquisitionPoint),
        "epoch_start" => Some(libpgs::pgs::segment::CompositionState::EpochStart),
        _ => None,
    }
}

fn parse_pcs_json(
    val: &JsonValue,
    line: usize,
) -> Result<libpgs::pgs::PcsData, String> {
    let state_str = require_field(val, "state", line)?
        .as_str()
        .ok_or_else(|| format!("line {line}: 'state' is not a string"))?;
    let composition_state = parse_composition_state_str(state_str)
        .ok_or_else(|| format!("line {line}: unknown composition state '{state_str}'"))?;

    let mut objects = Vec::new();
    if let Some(arr) = val.get("objects").and_then(|v| v.as_array()) {
        for obj in arr {
            let crop = if let Some(c) = obj.get("crop") {
                if c.is_null() {
                    None
                } else {
                    Some(libpgs::pgs::CropInfo {
                        x: require_u16(c, "x", line)?,
                        y: require_u16(c, "y", line)?,
                        width: require_u16(c, "width", line)?,
                        height: require_u16(c, "height", line)?,
                    })
                }
            } else {
                None
            };
            objects.push(libpgs::pgs::CompositionObject {
                object_id: require_u16(obj, "object_id", line)?,
                window_id: require_u8(obj, "window_id", line)?,
                x: require_u16(obj, "x", line)?,
                y: require_u16(obj, "y", line)?,
                crop,
            });
        }
    }

    Ok(libpgs::pgs::PcsData {
        video_width: require_u16(val, "video_width", line)?,
        video_height: require_u16(val, "video_height", line)?,
        composition_number: require_u16(val, "number", line)?,
        composition_state,
        palette_only: require_field(val, "palette_only", line)?
            .as_bool()
            .ok_or_else(|| format!("line {line}: 'palette_only' is not a boolean"))?,
        palette_id: require_u8(val, "palette_id", line)?,
        objects,
    })
}

fn parse_wds_json(
    arr: &[JsonValue],
    line: usize,
) -> Result<libpgs::pgs::WdsData, String> {
    let mut windows = Vec::with_capacity(arr.len());
    for w in arr {
        windows.push(libpgs::pgs::WindowDefinition {
            id: require_u8(w, "id", line)?,
            x: require_u16(w, "x", line)?,
            y: require_u16(w, "y", line)?,
            width: require_u16(w, "width", line)?,
            height: require_u16(w, "height", line)?,
        });
    }
    Ok(libpgs::pgs::WdsData { windows })
}

fn parse_pds_json(
    arr: &[JsonValue],
    line: usize,
) -> Result<Vec<libpgs::pgs::PdsData>, String> {
    let mut palettes = Vec::with_capacity(arr.len());
    for p in arr {
        let entries_arr = require_field(p, "entries", line)?
            .as_array()
            .ok_or_else(|| format!("line {line}: 'entries' is not an array"))?;
        let mut entries = Vec::with_capacity(entries_arr.len());
        for e in entries_arr {
            entries.push(libpgs::pgs::PaletteEntry {
                id: require_u8(e, "id", line)?,
                luminance: require_u8(e, "luminance", line)?,
                cr: require_u8(e, "cr", line)?,
                cb: require_u8(e, "cb", line)?,
                alpha: require_u8(e, "alpha", line)?,
            });
        }
        palettes.push(libpgs::pgs::PdsData {
            id: require_u8(p, "id", line)?,
            version: require_u8(p, "version", line)?,
            entries,
        });
    }
    Ok(palettes)
}

fn parse_object_json(
    val: &JsonValue,
    line: usize,
) -> Result<libpgs::pgs::ObjectBitmap, String> {
    let id = require_u16(val, "id", line)?;
    let version = require_u8(val, "version", line)?;
    let width = require_u16(val, "width", line)?;
    let height = require_u16(val, "height", line)?;

    let bitmap_val = require_field(val, "bitmap", line)?;
    if bitmap_val.is_null() {
        return Err(format!("line {line}: object {id} has null bitmap, cannot encode"));
    }
    let bitmap_b64 = bitmap_val
        .as_str()
        .ok_or_else(|| format!("line {line}: 'bitmap' is not a string"))?;
    let pixels = base64_decode(bitmap_b64)
        .map_err(|e| format!("line {line}: object {id} bitmap decode failed: {e}"))?;

    let expected = width as usize * height as usize;
    if pixels.len() != expected {
        return Err(format!(
            "line {line}: object {id} bitmap size mismatch: got {} bytes, expected {} ({}x{})",
            pixels.len(),
            expected,
            width,
            height
        ));
    }

    Ok(libpgs::pgs::ObjectBitmap {
        id,
        version,
        width,
        height,
        pixels,
    })
}

fn cmd_encode(args: &[String]) -> Result<(), libpgs::error::PgsError> {
    use libpgs::error::PgsError;

    if args.is_empty() {
        eprintln!("Usage: libpgs encode -o <output.sup>");
        eprintln!("  Reads NDJSON from stdin (same format as 'libpgs stream' output)");
        process::exit(1);
    }

    let mut output: Option<PathBuf> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-o" | "--output" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("Missing value for -o");
                    process::exit(1);
                }
                output = Some(PathBuf::from(&args[i]));
            }
            other => {
                eprintln!("Unknown option: {other}");
                process::exit(1);
            }
        }
        i += 1;
    }

    let output = output.unwrap_or_else(|| {
        eprintln!("Missing -o <output.sup>");
        process::exit(1);
    });

    let stdin = std::io::stdin();
    let reader = std::io::BufReader::new(stdin.lock());

    // Collect display sets grouped by track_id.
    let mut track_display_sets: Vec<(u64, Vec<libpgs::pgs::DisplaySet>)> = Vec::new();
    let mut skipped = 0u64;

    for (line_idx, line_result) in reader.lines().enumerate() {
        let line = line_result.map_err(PgsError::Io)?;
        let line_num = line_idx + 1;

        if line.trim().is_empty() {
            continue;
        }

        let json = json_parse(&line)
            .map_err(|e| PgsError::EncodingError(format!("line {line_num}: {e}")))?;

        let type_str = json.get("type").and_then(|v| v.as_str());
        match type_str {
            Some("tracks") => continue,
            Some("display_set") => {}
            Some(other) => {
                eprintln!("line {line_num}: skipping unknown type '{other}'");
                continue;
            }
            None => {
                return Err(PgsError::EncodingError(format!(
                    "line {line_num}: missing 'type' field"
                )));
            }
        }

        // Extract PTS: prefer integer 'pts', fall back to 'pts_ms' * 90.
        let pts = if let Some(pts_val) = json.get("pts") {
            pts_val
                .as_u64()
                .ok_or_else(|| PgsError::EncodingError(format!(
                    "line {line_num}: 'pts' is not a number"
                )))?
        } else if let Some(pts_ms_val) = json.get("pts_ms") {
            let ms = pts_ms_val
                .as_f64()
                .ok_or_else(|| PgsError::EncodingError(format!(
                    "line {line_num}: 'pts_ms' is not a number"
                )))?;
            (ms * 90.0).round() as u64
        } else {
            return Err(PgsError::EncodingError(format!(
                "line {line_num}: missing 'pts' or 'pts_ms'"
            )));
        };

        let track_id = json.get("track_id").and_then(|v| v.as_u64()).unwrap_or(0);

        // Parse composition (required).
        let comp_val = json.get("composition");
        if comp_val.is_none() || comp_val.unwrap().is_null() {
            eprintln!("line {line_num}: skipping display set with null composition");
            skipped += 1;
            continue;
        }
        let pcs = parse_pcs_json(comp_val.unwrap(), line_num)
            .map_err(|e| PgsError::EncodingError(e))?;

        let mut builder = libpgs::pgs::DisplaySetBuilder::new(pts).pcs(pcs);

        // Parse windows.
        if let Some(arr) = json.get("windows").and_then(|v| v.as_array()) {
            if !arr.is_empty() {
                let wds = parse_wds_json(arr, line_num)
                    .map_err(|e| PgsError::EncodingError(e))?;
                builder = builder.wds(wds);
            }
        }

        // Parse palettes.
        if let Some(arr) = json.get("palettes").and_then(|v| v.as_array()) {
            if !arr.is_empty() {
                let palettes = parse_pds_json(arr, line_num)
                    .map_err(|e| PgsError::EncodingError(e))?;
                for pds in palettes {
                    builder = builder.palette(pds);
                }
            }
        }

        // Parse objects.
        if let Some(arr) = json.get("objects").and_then(|v| v.as_array()) {
            for obj_val in arr {
                let bitmap = parse_object_json(obj_val, line_num)
                    .map_err(|e| PgsError::EncodingError(e))?;
                builder = builder.object(bitmap);
            }
        }

        let ds = builder.build()?;

        // Group by track_id.
        if let Some(group) = track_display_sets.iter_mut().find(|(tid, _)| *tid == track_id) {
            group.1.push(ds);
        } else {
            track_display_sets.push((track_id, vec![ds]));
        }
    }

    if track_display_sets.is_empty() {
        eprintln!("No display sets found in input.");
        return Ok(());
    }

    // Write output.
    let total_ds: usize = track_display_sets.iter().map(|(_, dss)| dss.len()).sum();

    if track_display_sets.len() == 1 {
        // Single track: write directly to the output path.
        let (track_id, display_sets) = &track_display_sets[0];
        libpgs::write_sup_file(display_sets, &output)?;
        let file_size = std::fs::metadata(&output).map(|m| m.len()).unwrap_or(0);
        eprintln!(
            "Encoded {} display sets (track {}) to {} ({} bytes)",
            display_sets.len(),
            track_id,
            output.display(),
            file_size,
        );
    } else {
        // Multiple tracks: write <stem>_track<id>.<ext> per track.
        let stem = output
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "output".to_string());
        let ext = output
            .extension()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "sup".to_string());
        let parent = output.parent().unwrap_or_else(|| std::path::Path::new("."));

        for (track_id, display_sets) in &track_display_sets {
            let track_output = parent.join(format!("{}_track{}.{}", stem, track_id, ext));
            libpgs::write_sup_file(display_sets, &track_output)?;
            let file_size = std::fs::metadata(&track_output)
                .map(|m| m.len())
                .unwrap_or(0);
            eprintln!(
                "  Track {} -> {} ({} display sets, {} bytes)",
                track_id,
                track_output.display(),
                display_sets.len(),
                file_size,
            );
        }
        eprintln!(
            "Encoded {} display sets across {} tracks",
            total_ds,
            track_display_sets.len()
        );
    }

    if skipped > 0 {
        eprintln!("Skipped {} display sets with null composition", skipped);
    }

    Ok(())
}

fn cmd_extract(args: &[String]) -> Result<(), libpgs::error::PgsError> {
    if args.is_empty() {
        eprintln!("Usage: libpgs extract <file> -o <out> [-t <id>] [--start TIME] [--end TIME]");
        process::exit(1);
    }

    let input = PathBuf::from(&args[0]);
    let mut output: Option<PathBuf> = None;
    let mut track_ids: Vec<u32> = Vec::new();
    let mut start_ms: Option<f64> = None;
    let mut end_ms: Option<f64> = None;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "-o" | "--output" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("Missing value for -o");
                    process::exit(1);
                }
                output = Some(PathBuf::from(&args[i]));
            }
            "-t" | "--track" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("Missing value for -t");
                    process::exit(1);
                }
                track_ids.extend(parse_track_ids(&args[i]));
            }
            "--start" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("Missing value for --start");
                    process::exit(1);
                }
                start_ms = Some(parse_timestamp(&args[i]).unwrap_or_else(|| {
                    eprintln!("Invalid timestamp for --start: {}", args[i]);
                    process::exit(1);
                }));
            }
            "--end" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("Missing value for --end");
                    process::exit(1);
                }
                end_ms = Some(parse_timestamp(&args[i]).unwrap_or_else(|| {
                    eprintln!("Invalid timestamp for --end: {}", args[i]);
                    process::exit(1);
                }));
            }
            other => {
                eprintln!("Unknown option: {other}");
                process::exit(1);
            }
        }
        i += 1;
    }

    let output = output.unwrap_or_else(|| {
        eprintln!("Missing -o <output.sup>");
        process::exit(1);
    });

    let has_time_range = start_ms.is_some() || end_ms.is_some();

    let start = Instant::now();

    if track_ids.len() == 1 {
        // Single-track extraction.
        let tid = track_ids[0];
        println!("Extracting PGS track {} from: {}", tid, input.display());

        // Use Extractor API when time range is set (batch helpers don't support it).
        let display_sets = if has_time_range {
            let extractor = libpgs::Extractor::open(&input)?
                .with_track_filter(&[tid])
                .with_time_range(start_ms, end_ms);
            extractor
                .collect::<Result<Vec<_>, _>>()?
                .into_iter()
                .map(|tds| tds.display_set)
                .collect::<Vec<_>>()
        } else {
            libpgs::extract_display_sets(&input, Some(tid))?
        };
        let elapsed_extract = start.elapsed();

        let total_segments: usize = display_sets.iter().map(|ds| ds.segments.len()).sum();
        println!(
            "Found {} display sets ({} segments) in {:.2}s",
            display_sets.len(),
            total_segments,
            elapsed_extract.as_secs_f64()
        );

        libpgs::write_sup_file(&display_sets, &output)?;
        let elapsed_total = start.elapsed();

        let file_size = std::fs::metadata(&output).map(|m| m.len()).unwrap_or(0);
        println!(
            "Written to: {} ({} bytes) in {:.2}s total",
            output.display(),
            file_size,
            elapsed_total.as_secs_f64()
        );
    } else {
        // Multi-track extraction: write <stem>_track<id>.sup per track.
        if track_ids.is_empty() {
            println!("Extracting all PGS tracks from: {}", input.display());
        } else {
            println!(
                "Extracting PGS tracks {:?} from: {}",
                track_ids,
                input.display()
            );
        }

        let track_results = if track_ids.is_empty() && !has_time_range {
            libpgs::extract_all_display_sets(&input)?
        } else {
            let mut ext = libpgs::Extractor::open(&input)?;
            if !track_ids.is_empty() {
                ext = ext.with_track_filter(&track_ids);
            }
            if has_time_range {
                ext = ext.with_time_range(start_ms, end_ms);
            }
            ext.collect_by_track()?
        };
        let elapsed_extract = start.elapsed();

        if track_results.is_empty() {
            if has_time_range {
                println!("No PGS display sets found in the requested time range.");
            } else {
                println!("No PGS display sets found.");
            }
            return Ok(());
        }

        let total_ds: usize = track_results.iter().map(|t| t.display_sets.len()).sum();
        println!(
            "Found {} tracks, {} display sets in {:.2}s",
            track_results.len(),
            total_ds,
            elapsed_extract.as_secs_f64()
        );

        // Derive output stem and extension from -o path.
        let stem = output
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "output".to_string());
        let ext = output
            .extension()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "sup".to_string());
        let parent = output.parent().unwrap_or_else(|| std::path::Path::new("."));

        if track_results.len() == 1 {
            // Only one track — write directly to the specified output path.
            let t = &track_results[0];
            libpgs::write_sup_file(&t.display_sets, &output)?;
            let file_size = std::fs::metadata(&output).map(|m| m.len()).unwrap_or(0);
            println!(
                "  Track {} -> {} ({} bytes)",
                t.track.track_id,
                output.display(),
                file_size
            );
        } else {
            for t in &track_results {
                let track_output =
                    parent.join(format!("{}_track{}.{}", stem, t.track.track_id, ext));
                libpgs::write_sup_file(&t.display_sets, &track_output)?;
                let file_size = std::fs::metadata(&track_output)
                    .map(|m| m.len())
                    .unwrap_or(0);
                println!(
                    "  Track {} -> {} ({} bytes)",
                    t.track.track_id,
                    track_output.display(),
                    file_size
                );
            }
        }

        let elapsed_total = start.elapsed();
        println!("Done in {:.2}s total", elapsed_total.as_secs_f64());
    }

    Ok(())
}
