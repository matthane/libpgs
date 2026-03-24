use crate::error::PgsError;
use crate::io::SeekBufReader;
use crate::m2ts::ts_packet::{self, PacketFormat};
use std::io::{Read, Seek};

const PMT_TABLE_ID: u8 = 0x02;
const PGS_STREAM_TYPE: u8 = 0x90;
const LANGUAGE_DESCRIPTOR_TAG: u8 = 0x0A;
const MAX_PACKETS_TO_SCAN: usize = 2000;

/// A stream entry from the PMT.
#[derive(Debug, Clone)]
pub struct PmtStream {
    pub stream_type: u8,
    pub elementary_pid: u16,
    pub language: Option<String>,
}

/// Scan packets for the PMT on the given PID and parse it.
pub fn find_pmt<R: Read + Seek>(
    reader: &mut SeekBufReader<R>,
    format: PacketFormat,
    pmt_pid: u16,
) -> Result<Vec<PmtStream>, PgsError> {
    reader.seek_to(0)?;

    for _ in 0..MAX_PACKETS_TO_SCAN {
        let ts_buf = match ts_packet::read_next_packet(reader, format)? {
            Some(buf) => buf,
            None => break,
        };

        let (header, payload) = ts_packet::extract_payload(&ts_buf)?;

        if header.pid != pmt_pid || !header.pusi || payload.is_empty() {
            continue;
        }

        let pointer = payload[0] as usize;
        let section_start = 1 + pointer;
        if section_start >= payload.len() {
            continue;
        }

        return parse_pmt_section(&payload[section_start..]);
    }

    Err(PgsError::InvalidTs("PMT not found".into()))
}

fn parse_pmt_section(data: &[u8]) -> Result<Vec<PmtStream>, PgsError> {
    if data.len() < 12 {
        return Err(PgsError::InvalidTs("PMT section too short".into()));
    }

    if data[0] != PMT_TABLE_ID {
        return Err(PgsError::InvalidTs(format!(
            "expected PMT table_id 0x02, got 0x{:02X}",
            data[0]
        )));
    }

    let section_length = ((data[1] as usize & 0x0F) << 8) | data[2] as usize;
    // Bytes 3-9: program_number(2) + flags(1) + section_num(1) + last_section_num(1) + PCR_PID(2)
    // Bytes 10-11: program_info_length
    let program_info_length = ((data[10] as usize & 0x0F) << 8) | data[11] as usize;

    let es_start = 12 + program_info_length;
    let section_end = (3 + section_length).saturating_sub(4); // subtract CRC32

    if es_start > data.len() || section_end > data.len() {
        return Err(PgsError::InvalidTs("PMT section length mismatch".into()));
    }

    let mut streams = Vec::new();
    let mut i = es_start;

    while i + 5 <= section_end {
        let stream_type = data[i];
        let elementary_pid = ((data[i + 1] as u16 & 0x1F) << 8) | data[i + 2] as u16;
        let es_info_length = ((data[i + 3] as usize & 0x0F) << 8) | data[i + 4] as usize;
        i += 5;

        let descriptors_end = (i + es_info_length).min(section_end);
        let language = parse_language_from_descriptors(&data[i..descriptors_end]);

        streams.push(PmtStream {
            stream_type,
            elementary_pid,
            language,
        });

        i = descriptors_end;
    }

    Ok(streams)
}

fn parse_language_from_descriptors(data: &[u8]) -> Option<String> {
    let mut i = 0;
    while i + 2 <= data.len() {
        let tag = data[i];
        let length = data[i + 1] as usize;
        i += 2;

        if i + length > data.len() {
            break;
        }

        if tag == LANGUAGE_DESCRIPTOR_TAG && length >= 3 {
            let lang = String::from_utf8_lossy(&data[i..i + 3]).to_string();
            let lang = lang.trim_end_matches('\0').to_string();
            if !lang.is_empty() && lang != "und" {
                return Some(lang);
            }
        }

        i += length;
    }
    None
}

/// Filter PMT streams to find PGS subtitle streams (stream_type 0x90).
pub fn find_pgs_streams(streams: &[PmtStream]) -> Vec<&PmtStream> {
    streams
        .iter()
        .filter(|s| s.stream_type == PGS_STREAM_TYPE)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_pmt_section_with_pgs() {
        let data = [
            0x02, // table_id
            0xB0, 0x18, // section_syntax=1, section_length=24
            0x00, 0x01, // program_number
            0xC1, // reserved, version=0, current=1
            0x00, // section_number
            0x00, // last_section_number
            0xE0, 0x41, // reserved + PCR_PID
            0xF0, 0x00, // reserved + program_info_length=0
            // ES: stream_type=0x90 (PGS), PID=0x1200
            0x90, 0xF2, 0x00, 0xF0, 0x06, // ES_info_length=6
            // Language descriptor: tag=0x0A, length=4, "eng" + audio_type=0
            0x0A, 0x04, 0x65, 0x6E, 0x67, 0x00, // CRC32
            0x00, 0x00, 0x00, 0x00,
        ];

        let streams = parse_pmt_section(&data).unwrap();
        assert_eq!(streams.len(), 1);
        assert_eq!(streams[0].stream_type, 0x90);
        assert_eq!(streams[0].elementary_pid, 0x1200);
        assert_eq!(streams[0].language.as_deref(), Some("eng"));
    }

    #[test]
    fn test_find_pgs_streams() {
        let streams = vec![
            PmtStream {
                stream_type: 0x1B,
                elementary_pid: 0x1011,
                language: None,
            },
            PmtStream {
                stream_type: 0x90,
                elementary_pid: 0x1200,
                language: Some("eng".into()),
            },
            PmtStream {
                stream_type: 0x81,
                elementary_pid: 0x1100,
                language: None,
            },
            PmtStream {
                stream_type: 0x90,
                elementary_pid: 0x1201,
                language: Some("fre".into()),
            },
        ];
        let pgs = find_pgs_streams(&streams);
        assert_eq!(pgs.len(), 2);
        assert_eq!(pgs[0].elementary_pid, 0x1200);
        assert_eq!(pgs[1].elementary_pid, 0x1201);
    }

    #[test]
    fn test_parse_pmt_no_descriptors() {
        let data = [
            0x02, 0xB0, 0x12, // section_length=18
            0x00, 0x01, 0xC1, 0x00, 0x00, 0xE0, 0x41, 0xF0, 0x00, // program_info_length=0
            // ES: stream_type=0x90, PID=0x1200, no descriptors
            0x90, 0xF2, 0x00, 0xF0, 0x00, // ES_info_length=0
            // CRC32
            0x00, 0x00, 0x00, 0x00,
        ];

        let streams = parse_pmt_section(&data).unwrap();
        assert_eq!(streams.len(), 1);
        assert_eq!(streams[0].stream_type, 0x90);
        assert!(streams[0].language.is_none());
    }
}
