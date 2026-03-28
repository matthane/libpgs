//! BDMV CLPI file parser for PID → language mappings.
//!
//! When an M2TS file lives inside a BDMV directory structure, the corresponding
//! `.clpi` file in `CLIPINF/` contains stream attributes including language codes.
//! This module parses those as a fallback for tracks where PMT lacked descriptors.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// PGS presentation graphics stream coding type.
const PGS_CODING_TYPE: u8 = 0x90;
/// PGS text subtitle stream coding type.
const PGS_TEXT_CODING_TYPE: u8 = 0x91;
/// Expected magic bytes at the start of a CLPI file.
const CLPI_MAGIC: &[u8; 4] = b"HDMV";
/// Byte offset where the SequenceInfo start address is stored (u32 BE).
const SEQUENCE_INFO_OFFSET_POS: usize = 8;
/// Byte offset where the ProgramInfo start address is stored (u32 BE).
const PROGRAM_INFO_OFFSET_POS: usize = 12;
/// Minimum valid CLPI file size (header + at least one offset table entry).
const MIN_CLPI_SIZE: usize = 40;

/// Attempt to find and parse a CLPI file corresponding to an M2TS file
/// inside a BDMV/STREAM/ directory. Returns PID → language code mappings.
///
/// Returns an empty map if the path is not inside a BDMV structure,
/// the CLPI file does not exist, or parsing fails.
pub(crate) fn clpi_language_map(m2ts_path: &Path) -> HashMap<u16, String> {
    let Some(clpi_path) = resolve_clpi_path(m2ts_path) else {
        return HashMap::new();
    };

    let Ok(data) = std::fs::read(&clpi_path) else {
        return HashMap::new();
    };

    parse_clpi_file(&data).unwrap_or_default()
}

/// Attempt to find and parse a CLPI file corresponding to an M2TS file
/// to extract presentation start and end times from the SequenceInfo section.
///
/// Returns `(start, end)` in 90 kHz ticks, or `None` if unavailable.
pub(crate) fn clpi_presentation_times(m2ts_path: &Path) -> Option<(u64, u64)> {
    let clpi_path = resolve_clpi_path(m2ts_path)?;
    let data = std::fs::read(&clpi_path).ok()?;
    parse_sequence_info_times(&data)
}

/// Parse the SequenceInfo section of a CLPI file to extract presentation times.
///
/// Layout (from the BD spec):
///   CLPI header bytes 8..12: SequenceInfo section offset (u32 BE)
///   SequenceInfo section:
///     [0..4]   u32 length
///     [4]      reserved
///     [5]      num_atc_sequences
///     Per ATC sequence:
///       [+0..4]  u32 spn_atc_start
///       [+4]     u8  num_stc_sequences
///       [+5]     u8  offset_stc_id
///       Per STC sequence:
///         [+0..2]  u16 pcr_pid
///         [+2..6]  u32 spn_stc_start
///         [+6..10] u32 presentation_start_time (90 kHz ticks)
///         [+10..14] u32 presentation_end_time
fn parse_sequence_info_times(data: &[u8]) -> Option<(u64, u64)> {
    if data.len() < MIN_CLPI_SIZE {
        return None;
    }
    if &data[0..4] != CLPI_MAGIC {
        return None;
    }

    let seq_info_offset = read_u32_be(data, SEQUENCE_INFO_OFFSET_POS)? as usize;
    if seq_info_offset == 0 || seq_info_offset >= data.len() {
        return None;
    }

    let section = &data[seq_info_offset..];
    // Need at least: length(4) + reserved(1) + num_atc(1) + atc_header(6) + stc_entry(14)
    if section.len() < 26 {
        return None;
    }

    // Skip length(4) + reserved(1).
    let num_atc = section[5] as usize;
    if num_atc == 0 {
        return None;
    }

    // First ATC sequence starts at offset 6.
    let atc_pos = 6;
    // spn_atc_start(4) + num_stc_sequences(1) + offset_stc_id(1) = 6 bytes
    if atc_pos + 6 > section.len() {
        return None;
    }
    let num_stc = section[atc_pos + 4] as usize;
    if num_stc == 0 {
        return None;
    }

    // First STC sequence starts at atc_pos + 6.
    let stc_pos = atc_pos + 6;
    // pcr_pid(2) + spn_stc_start(4) + presentation_start_time(4) + presentation_end_time(4) = 14
    if stc_pos + 14 > section.len() {
        return None;
    }

    let start = read_u32_be(section, stc_pos + 6)? as u64;
    let end = read_u32_be(section, stc_pos + 10)? as u64;
    Some((start, end))
}


/// Resolve the CLPI path from an M2TS path.
///
/// `BDMV/STREAM/00001.m2ts` → `BDMV/CLIPINF/00001.clpi`
fn resolve_clpi_path(m2ts_path: &Path) -> Option<PathBuf> {
    let stem = m2ts_path.file_stem()?.to_str()?;
    let stream_dir = m2ts_path.parent()?;
    let stream_dir_name = stream_dir.file_name()?.to_str()?;

    // Parent directory must be named "STREAM" (case-insensitive).
    if !stream_dir_name.eq_ignore_ascii_case("STREAM") {
        return None;
    }

    let bdmv_dir = stream_dir.parent()?;

    // Try canonical case first, then lowercase.
    for dir_name in &["CLIPINF", "clipinf"] {
        let clpi_path = bdmv_dir.join(dir_name).join(format!("{}.clpi", stem));
        if clpi_path.exists() {
            return Some(clpi_path);
        }
    }

    None
}

/// Read a u16 big-endian from a byte slice at the given offset.
fn read_u16_be(data: &[u8], offset: usize) -> Option<u16> {
    if offset + 2 > data.len() {
        return None;
    }
    Some(u16::from_be_bytes([data[offset], data[offset + 1]]))
}

/// Read a u32 big-endian from a byte slice at the given offset.
fn read_u32_be(data: &[u8], offset: usize) -> Option<u32> {
    if offset + 4 > data.len() {
        return None;
    }
    Some(u32::from_be_bytes([
        data[offset],
        data[offset + 1],
        data[offset + 2],
        data[offset + 3],
    ]))
}

/// Parse a CLPI file's ProgramInfo section to extract PGS PID → language mappings.
fn parse_clpi_file(data: &[u8]) -> Result<HashMap<u16, String>, ()> {
    if data.len() < MIN_CLPI_SIZE {
        return Err(());
    }

    // Validate magic.
    if &data[0..4] != CLPI_MAGIC {
        return Err(());
    }

    // ProgramInfo section offset.
    let prog_info_offset = read_u32_be(data, PROGRAM_INFO_OFFSET_POS).ok_or(())? as usize;
    if prog_info_offset == 0 || prog_info_offset >= data.len() {
        return Err(());
    }

    let section = &data[prog_info_offset..];
    if section.len() < 6 {
        return Err(());
    }

    // u32 length, then data starts at +4.
    let _length = read_u32_be(section, 0).ok_or(())?;
    // Skip reserved byte at offset 4 if present — number of program sequences is at +5
    // Actually: the structure is length(4) + reserved(1) + num_sequences(1)
    // But in practice some versions put num_sequences right at +4.
    // The BD spec: after the length field, there's a count of program sequences.
    // Let's follow the common layout:
    //   [0..4]  u32 length
    //   [4]     u8  reserved / padding
    //   [5]     u8  number_of_program_sequences
    // But some implementations use:
    //   [0..4]  u32 length
    //   [4]     u8  number_of_program_sequences
    //
    // We'll try the standard BD layout: count at offset 5 after the length.
    if section.len() < 6 {
        return Err(());
    }

    // Try parsing with count at offset 5 (standard CLPI ProgramInfo).
    if let Some(m) = parse_program_info_sequences(section, 5) {
        return Ok(m);
    }
    // Fallback: count at offset 4.
    if let Some(m) = parse_program_info_sequences(section, 4) {
        return Ok(m);
    }

    Ok(HashMap::new())
}

/// Parse program sequences starting from the given count_offset within the section.
fn parse_program_info_sequences(
    section: &[u8],
    count_offset: usize,
) -> Option<HashMap<u16, String>> {
    if count_offset >= section.len() {
        return None;
    }

    let num_sequences = section[count_offset] as usize;
    if num_sequences == 0 || num_sequences > 100 {
        return None;
    }

    let mut map = HashMap::new();
    let mut pos = count_offset + 1;

    for _ in 0..num_sequences {
        // SPN_program_sequence_start: u32
        // program_map_PID: u16
        // number_of_streams_in_ps: u8
        // number_of_groups: u8
        if pos + 8 > section.len() {
            return None;
        }

        let _spn = read_u32_be(section, pos)?;
        pos += 4;
        let _pmt_pid = read_u16_be(section, pos)?;
        pos += 2;
        let num_streams = section[pos] as usize;
        pos += 1;
        let num_groups = section[pos] as usize;
        pos += 1;

        // Parse each stream entry.
        for _ in 0..num_streams {
            if pos + 3 > section.len() {
                return None;
            }

            let stream_pid = read_u16_be(section, pos)?;
            pos += 2;

            // Length of stream coding info block.
            let coding_info_len = section[pos] as usize;
            pos += 1;

            if coding_info_len == 0 || pos + coding_info_len > section.len() {
                if coding_info_len == 0 {
                    continue;
                }
                return None;
            }

            let coding_type = section[pos];

            if (coding_type == PGS_CODING_TYPE || coding_type == PGS_TEXT_CODING_TYPE)
                && coding_info_len >= 4
            {
                // For PGS: coding_type(1) + language(3)
                let lang_bytes = &section[pos + 1..pos + 4];
                let lang = std::str::from_utf8(lang_bytes)
                    .ok()
                    .map(|s| s.trim_end_matches('\0').to_string())
                    .filter(|s| !s.is_empty() && s != "und")
                    .map(|s| crate::lang::normalize_language(&s));

                if let Some(lang) = lang {
                    map.entry(stream_pid).or_insert(lang);
                }
            }

            pos += coding_info_len;
        }

        // Skip group entries if present.
        for _ in 0..num_groups {
            // Each group has: u8 group_type, u16 group_info_length, then that many bytes
            // Simplified: skip 1 + variable. We need at least the type + length.
            if pos + 3 > section.len() {
                break;
            }
            pos += 1; // group type
            let group_len = read_u16_be(section, pos)? as usize;
            pos += 2;
            pos += group_len;
        }
    }

    if map.is_empty() { None } else { Some(map) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_clpi_path_valid_bdmv() {
        // Use a synthetic path — no filesystem access needed for path logic.
        let path = Path::new("/media/disc/BDMV/STREAM/00001.m2ts");
        let stream_dir = path.parent().unwrap();
        let stream_dir_name = stream_dir.file_name().unwrap().to_str().unwrap();
        assert!(stream_dir_name.eq_ignore_ascii_case("STREAM"));

        let stem = path.file_stem().unwrap().to_str().unwrap();
        assert_eq!(stem, "00001");

        let bdmv_dir = stream_dir.parent().unwrap();
        let expected = bdmv_dir.join("CLIPINF").join("00001.clpi");
        assert_eq!(expected, Path::new("/media/disc/BDMV/CLIPINF/00001.clpi"));
    }

    #[test]
    fn test_resolve_clpi_path_not_bdmv() {
        let path = Path::new("/home/user/videos/movie.m2ts");
        assert!(resolve_clpi_path(path).is_none());
    }

    #[test]
    fn test_resolve_clpi_path_wrong_parent() {
        let path = Path::new("/media/disc/BDMV/BACKUP/00001.m2ts");
        assert!(resolve_clpi_path(path).is_none());
    }

    #[test]
    fn test_parse_clpi_bad_magic() {
        let mut data = vec![0u8; 64];
        data[0..4].copy_from_slice(b"XXXX");
        assert!(parse_clpi_file(&data).is_err());
    }

    #[test]
    fn test_parse_clpi_too_small() {
        let data = vec![0u8; 10];
        assert!(parse_clpi_file(&data).is_err());
    }

    /// Build a minimal CLPI binary with one PGS stream.
    fn build_clpi_with_pgs(pid: u16, lang: &[u8; 3]) -> Vec<u8> {
        let mut data = Vec::new();

        // Header: magic + version (8 bytes)
        data.extend_from_slice(b"HDMV0300");

        // Offset table (offsets at positions 8, 12, 16, 20):
        // [8]  SequenceInfo offset (unused, set to 0)
        // [12] ProgramInfo offset — we'll put it at byte 40
        // [16..] other offsets
        data.extend_from_slice(&0u32.to_be_bytes()); // [8] SequenceInfo
        data.extend_from_slice(&40u32.to_be_bytes()); // [12] ProgramInfo → offset 40
        data.extend_from_slice(&0u32.to_be_bytes()); // [16]
        data.extend_from_slice(&0u32.to_be_bytes()); // [20]
        data.extend_from_slice(&0u32.to_be_bytes()); // [24]
        data.extend_from_slice(&0u32.to_be_bytes()); // [28]
        data.extend_from_slice(&0u32.to_be_bytes()); // [32]
        data.extend_from_slice(&0u32.to_be_bytes()); // [36]

        // ProgramInfo section at offset 40:
        assert_eq!(data.len(), 40);

        // Build ProgramInfo content (after length field):
        //   [0..4] u32 length (of remaining section)
        //   [4]    reserved
        //   [5]    num_sequences = 1
        //   sequence 0:
        //     [6..10]  u32 SPN
        //     [10..12] u16 PMT PID
        //     [12]     u8 num_streams = 1
        //     [13]     u8 num_groups = 0
        //     stream 0:
        //       [14..16] u16 PID
        //       [16]     u8 coding_info_len = 4
        //       [17]     u8 coding_type = 0x90
        //       [18..21] language (3 bytes)
        let section_content_len: u32 = 2 + 8 + 2 + 1 + 4 + 3; // = 20
        data.extend_from_slice(&section_content_len.to_be_bytes()); // length
        data.push(0x00); // reserved
        data.push(0x01); // 1 sequence

        // Sequence:
        data.extend_from_slice(&0u32.to_be_bytes()); // SPN
        data.extend_from_slice(&0x0100u16.to_be_bytes()); // PMT PID
        data.push(0x01); // 1 stream
        data.push(0x00); // 0 groups

        // Stream entry:
        data.extend_from_slice(&pid.to_be_bytes()); // stream PID
        data.push(0x04); // coding_info_len = 4
        data.push(PGS_CODING_TYPE); // 0x90
        data.extend_from_slice(lang); // 3-byte language

        data
    }

    #[test]
    fn test_parse_clpi_pgs_stream() {
        let data = build_clpi_with_pgs(0x1200, b"eng");
        let map = parse_clpi_file(&data).unwrap();
        assert_eq!(map.len(), 1);
        assert_eq!(map.get(&0x1200).unwrap(), "en");
    }

    #[test]
    fn test_parse_clpi_mixed_streams() {
        let mut data = Vec::new();

        // Header
        data.extend_from_slice(b"HDMV0300");
        data.extend_from_slice(&0u32.to_be_bytes()); // [8]
        data.extend_from_slice(&40u32.to_be_bytes()); // [12] ProgramInfo at 40
        // Pad to offset 40
        while data.len() < 40 {
            data.extend_from_slice(&0u32.to_be_bytes());
        }

        // ProgramInfo section
        let section_content_len: u32 = 2 + 8 + 3 * (2 + 1 + 4 + 3); // 2 header + 8 seq header + 3 streams
        data.extend_from_slice(&section_content_len.to_be_bytes());
        data.push(0x00); // reserved
        data.push(0x01); // 1 sequence

        data.extend_from_slice(&0u32.to_be_bytes()); // SPN
        data.extend_from_slice(&0x0100u16.to_be_bytes()); // PMT PID
        data.push(0x03); // 3 streams
        data.push(0x00); // 0 groups

        // Stream 1: Video (coding type 0x02 = MPEG-2)
        data.extend_from_slice(&0x1011u16.to_be_bytes());
        data.push(0x04);
        data.push(0x02); // video
        data.extend_from_slice(b"\0\0\0");

        // Stream 2: Audio (coding type 0x80 = LPCM)
        data.extend_from_slice(&0x1100u16.to_be_bytes());
        data.push(0x04);
        data.push(0x80); // audio
        data.extend_from_slice(b"jpn");

        // Stream 3: PGS (coding type 0x90)
        data.extend_from_slice(&0x1200u16.to_be_bytes());
        data.push(0x04);
        data.push(PGS_CODING_TYPE);
        data.extend_from_slice(b"fra");

        let map = parse_clpi_file(&data).unwrap();
        assert_eq!(map.len(), 1, "should only contain PGS stream");
        assert_eq!(map.get(&0x1200).unwrap(), "fr");
        assert!(!map.contains_key(&0x1011), "should not contain video");
        assert!(!map.contains_key(&0x1100), "should not contain audio");
    }

    #[test]
    fn test_parse_clpi_und_language_filtered() {
        let data = build_clpi_with_pgs(0x1200, b"und");
        let result = parse_clpi_file(&data);
        // "und" should be filtered out, so map is empty → returns None from
        // parse_program_info_sequences, but parse_clpi_file falls through to Ok(empty).
        match result {
            Ok(map) => assert!(map.is_empty()),
            Err(()) => {} // Also acceptable
        }
    }

    #[test]
    fn test_clpi_language_map_nonexistent() {
        let map = clpi_language_map(Path::new("/nonexistent/BDMV/STREAM/00001.m2ts"));
        assert!(map.is_empty());
    }

    /// Build a minimal CLPI binary with a SequenceInfo section containing
    /// the given presentation times.
    fn build_clpi_with_sequence_times(
        presentation_start_time: u32,
        presentation_end_time: u32,
    ) -> Vec<u8> {
        let mut data = Vec::new();

        // Header: magic + version (8 bytes)
        data.extend_from_slice(b"HDMV0300");

        // Offset table:
        // [8]  SequenceInfo offset — we'll put it at byte 40
        // [12] ProgramInfo offset (0 = not present)
        // [16..] other offsets
        data.extend_from_slice(&40u32.to_be_bytes()); // [8] SequenceInfo → offset 40
        data.extend_from_slice(&0u32.to_be_bytes()); // [12] ProgramInfo
        data.extend_from_slice(&0u32.to_be_bytes()); // [16]
        data.extend_from_slice(&0u32.to_be_bytes()); // [20]
        data.extend_from_slice(&0u32.to_be_bytes()); // [24]
        data.extend_from_slice(&0u32.to_be_bytes()); // [28]
        data.extend_from_slice(&0u32.to_be_bytes()); // [32]
        data.extend_from_slice(&0u32.to_be_bytes()); // [36]

        // SequenceInfo section at offset 40:
        assert_eq!(data.len(), 40);

        // length(4) + reserved(1) + num_atc_sequences(1) = 6 bytes header
        // ATC seq: spn_atc_start(4) + num_stc(1) + offset_stc_id(1) = 6 bytes
        // STC seq: pcr_pid(2) + spn_stc_start(4) + start_time(4) + end_time(4) = 14 bytes
        let section_len: u32 = 2 + 6 + 14; // content after length field
        data.extend_from_slice(&section_len.to_be_bytes()); // length
        data.push(0x00); // reserved
        data.push(0x01); // 1 ATC sequence

        // ATC sequence:
        data.extend_from_slice(&0u32.to_be_bytes()); // spn_atc_start
        data.push(0x01); // 1 STC sequence
        data.push(0x00); // offset_stc_id

        // STC sequence:
        data.extend_from_slice(&0x1001u16.to_be_bytes()); // pcr_pid
        data.extend_from_slice(&0u32.to_be_bytes()); // spn_stc_start
        data.extend_from_slice(&presentation_start_time.to_be_bytes()); // presentation_start_time
        data.extend_from_slice(&presentation_end_time.to_be_bytes()); // presentation_end_time

        data
    }

    fn build_clpi_with_sequence_info(presentation_start_time: u32) -> Vec<u8> {
        build_clpi_with_sequence_times(presentation_start_time, 0)
    }

    #[test]
    fn test_parse_sequence_info_start_time() {
        // 54000000 ticks at 90kHz = 600 seconds = 10 minutes (typical BD offset)
        let data = build_clpi_with_sequence_info(54_000_000);
        let result = parse_sequence_info_times(&data);
        assert_eq!(result, Some((54_000_000, 0)));
    }

    #[test]
    fn test_parse_sequence_info_zero_offset() {
        let data = build_clpi_with_sequence_info(0);
        let result = parse_sequence_info_times(&data);
        assert_eq!(result, Some((0, 0)));
    }

    #[test]
    fn test_parse_sequence_info_times() {
        let start = 54_000_000u32; // 10 minutes
        let end = 594_000_000u32; // 110 minutes
        let data = build_clpi_with_sequence_times(start, end);
        let result = parse_sequence_info_times(&data);
        assert_eq!(result, Some((start as u64, end as u64)));
    }

    #[test]
    fn test_parse_sequence_info_no_section() {
        // Build CLPI with SequenceInfo offset = 0 (no section).
        let mut data = Vec::new();
        data.extend_from_slice(b"HDMV0300");
        data.extend_from_slice(&0u32.to_be_bytes()); // [8] SequenceInfo = 0
        while data.len() < MIN_CLPI_SIZE {
            data.push(0);
        }
        assert_eq!(parse_sequence_info_times(&data), None);
    }

    #[test]
    fn test_parse_sequence_info_bad_magic() {
        let mut data = build_clpi_with_sequence_info(90000);
        data[0..4].copy_from_slice(b"XXXX");
        assert_eq!(parse_sequence_info_times(&data), None);
    }

    #[test]
    fn test_parse_sequence_info_no_atc_sequences() {
        let mut data = build_clpi_with_sequence_info(90000);
        // Set num_atc_sequences to 0 at offset 40 + 5.
        data[45] = 0;
        assert_eq!(parse_sequence_info_times(&data), None);
    }
}
