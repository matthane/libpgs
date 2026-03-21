use std::io::Write;
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
    eprintln!("libpgs - Fast PGS subtitle extraction\n");
    eprintln!("Usage:");
    eprintln!("  libpgs tracks <file>                      List PGS tracks");
    eprintln!("  libpgs extract <file> -o <output.sup>     Extract all PGS tracks");
    eprintln!("  libpgs extract <file> -t <id> -o <out>    Extract specific track");
    eprintln!("  libpgs stream <file> [-t <id>]            Stream PGS segments to stdout");
    eprintln!("  libpgs bench <file>                       Benchmark I/O efficiency");
    eprintln!("  libpgs help                               Show this help");
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
    let mut track_ds_counts: std::collections::HashMap<u32, (usize, usize)> = std::collections::HashMap::new();
    for tds in &results {
        let entry = track_ds_counts.entry(tds.track_id).or_insert((0, 0));
        entry.0 += 1;
        entry.1 += tds.display_set.segments.len();
    }

    let total_display_sets = results.len();
    let total_segments: usize = results.iter().map(|tds| tds.display_set.segments.len()).sum();
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
    println!("Total:         {} display sets, {} segments", total_display_sets, total_segments);
    println!("Time:          {:.3}s", elapsed.as_secs_f64());

    Ok(())
}

/// Parse a comma-separated list of track IDs (e.g. "3", "3,5,8").
fn parse_track_ids(value: &str) -> Vec<u32> {
    value.split(',')
        .map(|s| s.trim().parse().unwrap_or_else(|_| {
            eprintln!("Invalid track ID: {}", s.trim());
            process::exit(1);
        }))
        .collect()
}

fn cmd_stream(args: &[String]) -> Result<(), libpgs::error::PgsError> {
    if args.is_empty() {
        eprintln!("Usage: libpgs stream <file> [-t <track_id>[,<track_id>...]]");
        process::exit(1);
    }

    let input = PathBuf::from(&args[0]);
    let mut track_ids: Vec<u32> = Vec::new();

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

    let stdout = std::io::stdout();
    let mut out = stdout.lock();

    // Stream tracks header + display sets as NDJSON.
    // If the consumer closes the pipe (BrokenPipe), exit cleanly.
    match stream_ndjson(&mut out, &mut extractor) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
        Err(e) => Err(e.into()),
    }
}

fn stream_ndjson(
    out: &mut impl Write,
    extractor: &mut libpgs::Extractor,
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
             \"name\":{},\"flag_default\":{},\"flag_forced\":{},\"display_set_count\":{},\
             \"has_cues\":{}}}",
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
        let tds = result.map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
        let ds = &tds.display_set;
        let index = track_indices.entry(tds.track_id).or_insert(0);
        let current_index = *index;
        *index += 1;

        write!(
            out,
            "{{\"type\":\"display_set\",\"track_id\":{},\"index\":{},\
             \"pts\":{},\"pts_ms\":{:.4},\"composition_state\":\"{}\",\"segments\":[",
            tds.track_id,
            current_index,
            ds.pts,
            ds.pts_ms,
            composition_state_name(ds.composition_state),
        )?;

        for (si, seg) in ds.segments.iter().enumerate() {
            if si > 0 {
                write!(out, ",")?;
            }
            write!(
                out,
                "{{\"type\":\"{}\",\"pts\":{},\"dts\":{},\"size\":{},\"payload\":\"{}\"}}",
                segment_type_name(seg.segment_type),
                seg.pts,
                seg.dts,
                seg.payload.len(),
                base64_encode(&seg.payload),
            )?;
        }

        writeln!(out, "]}}")?;
        out.flush()?;
    }

    Ok(())
}

fn json_string_or_null(s: Option<&str>) -> String {
    match s {
        Some(v) => format!("\"{}\"", v),
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

const BASE64_CHARS: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn base64_encode(data: &[u8]) -> String {
    let mut encoded = String::with_capacity((data.len() + 2) / 3 * 4);
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

fn cmd_extract(args: &[String]) -> Result<(), libpgs::error::PgsError> {
    if args.is_empty() {
        eprintln!("Usage: libpgs extract <file> -o <output.sup> [-t <track_id>[,<track_id>...]]");
        process::exit(1);
    }

    let input = PathBuf::from(&args[0]);
    let mut output: Option<PathBuf> = None;
    let mut track_ids: Vec<u32> = Vec::new();

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

    let start = Instant::now();

    if track_ids.len() == 1 {
        // Single-track extraction.
        let tid = track_ids[0];
        println!("Extracting PGS track {} from: {}", tid, input.display());

        let display_sets = libpgs::extract_display_sets(&input, Some(tid))?;
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
            println!("Extracting PGS tracks {:?} from: {}", track_ids, input.display());
        }

        let track_results = if track_ids.is_empty() {
            libpgs::extract_all_display_sets(&input)?
        } else {
            libpgs::Extractor::open(&input)?
                .with_track_filter(&track_ids)
                .collect_by_track()?
        };
        let elapsed_extract = start.elapsed();

        if track_results.is_empty() {
            println!("No PGS display sets found.");
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
                let track_output = parent.join(format!(
                    "{}_track{}.{}",
                    stem, t.track.track_id, ext
                ));
                libpgs::write_sup_file(&t.display_sets, &track_output)?;
                let file_size = std::fs::metadata(&track_output).map(|m| m.len()).unwrap_or(0);
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
