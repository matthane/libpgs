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
        println!(
            "  Track {}: language={}, format={:?}",
            track.track_id, lang, track.container
        );
    }

    Ok(())
}

fn cmd_bench(args: &[String]) -> Result<(), libpgs::error::PgsError> {
    if args.is_empty() {
        eprintln!("Usage: libpgs bench <file>");
        process::exit(1);
    }

    let path = PathBuf::from(&args[0]);
    let start = Instant::now();

    let (track_results, stats) = libpgs::extract_all_display_sets_with_stats(&path)?;
    let elapsed = start.elapsed();

    let total_display_sets: usize = track_results.iter().map(|t| t.display_sets.len()).sum();
    let total_segments: usize = track_results
        .iter()
        .flat_map(|t| &t.display_sets)
        .map(|ds| ds.segments.len())
        .sum();
    let ratio = if stats.file_size > 0 {
        (stats.bytes_read as f64 / stats.file_size as f64) * 100.0
    } else {
        0.0
    };

    println!("File:          {}", path.display());
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
    println!("Tracks:        {}", track_results.len());
    for t in &track_results {
        let lang = t.track.language.as_deref().unwrap_or("unknown");
        let segs: usize = t.display_sets.iter().map(|ds| ds.segments.len()).sum();
        println!(
            "  Track {} ({}): {} display sets, {} segments",
            t.track.track_id,
            lang,
            t.display_sets.len(),
            segs
        );
    }
    println!("Total:         {} display sets, {} segments", total_display_sets, total_segments);
    println!("Time:          {:.3}s", elapsed.as_secs_f64());

    Ok(())
}

fn cmd_extract(args: &[String]) -> Result<(), libpgs::error::PgsError> {
    if args.is_empty() {
        eprintln!("Usage: libpgs extract <file> -o <output.sup> [-t <track_id>]");
        process::exit(1);
    }

    let input = PathBuf::from(&args[0]);
    let mut output: Option<PathBuf> = None;
    let mut track_id: Option<u32> = None;

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
                track_id = Some(args[i].parse().unwrap_or_else(|_| {
                    eprintln!("Invalid track ID: {}", args[i]);
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

    let start = Instant::now();

    if let Some(tid) = track_id {
        // Single-track extraction.
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
        println!("Extracting all PGS tracks from: {}", input.display());

        let track_results = libpgs::extract_all_display_sets(&input)?;
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
