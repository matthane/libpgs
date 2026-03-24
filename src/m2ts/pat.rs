use crate::error::PgsError;
use crate::io::SeekBufReader;
use crate::m2ts::ts_packet::{self, PacketFormat};
use std::io::{Read, Seek};

const PAT_PID: u16 = 0x0000;
const PAT_TABLE_ID: u8 = 0x00;
const MAX_PACKETS_TO_SCAN: usize = 1000;

/// A PAT entry: program_number -> PMT PID.
#[derive(Debug)]
pub struct PatEntry {
    pub program_number: u16,
    pub pmt_pid: u16,
}

/// Scan the first packets of the file for a PAT and parse it.
/// Returns a list of (program_number, PMT_PID) entries (excluding NIT).
pub fn find_pat<R: Read + Seek>(
    reader: &mut SeekBufReader<R>,
    format: PacketFormat,
) -> Result<Vec<PatEntry>, PgsError> {
    reader.seek_to(0)?;

    for _ in 0..MAX_PACKETS_TO_SCAN {
        let ts_buf = match ts_packet::read_next_packet(reader, format)? {
            Some(buf) => buf,
            None => break,
        };

        let (header, payload) = ts_packet::extract_payload(&ts_buf)?;

        if header.pid != PAT_PID || !header.pusi || payload.is_empty() {
            continue;
        }

        // First byte is pointer_field; skip past it.
        let pointer = payload[0] as usize;
        let section_start = 1 + pointer;
        if section_start >= payload.len() {
            continue;
        }

        return parse_pat_section(&payload[section_start..]);
    }

    Err(PgsError::InvalidTs("PAT not found".into()))
}

fn parse_pat_section(data: &[u8]) -> Result<Vec<PatEntry>, PgsError> {
    if data.len() < 8 {
        return Err(PgsError::InvalidTs("PAT section too short".into()));
    }

    if data[0] != PAT_TABLE_ID {
        return Err(PgsError::InvalidTs(format!(
            "expected PAT table_id 0x00, got 0x{:02X}",
            data[0]
        )));
    }

    let section_length = ((data[1] as usize & 0x0F) << 8) | data[2] as usize;

    // Fixed fields after section_length: transport_stream_id(2) + flags(1) + section_num(1) + last_section_num(1) = 5 bytes
    let entries_start = 8; // 3 (table_id + section_length) + 5 (fixed)
    let entries_end = 3 + section_length.saturating_sub(4); // subtract CRC32

    if entries_end > data.len() || entries_start > entries_end {
        return Err(PgsError::InvalidTs("PAT section length mismatch".into()));
    }

    let mut entries = Vec::new();
    let mut i = entries_start;
    while i + 4 <= entries_end {
        let program_number = (data[i] as u16) << 8 | data[i + 1] as u16;
        let pid = ((data[i + 2] as u16 & 0x1F) << 8) | data[i + 3] as u16;

        if program_number != 0 {
            // program_number 0 = NIT, skip it.
            entries.push(PatEntry {
                program_number,
                pmt_pid: pid,
            });
        }
        i += 4;
    }

    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_pat_section() {
        let data = [
            0x00, // table_id
            0xB0, 0x0D, // section_syntax=1, section_length=13
            0x00, 0x01, // transport_stream_id
            0xC1, // reserved, version=0, current=1
            0x00, // section_number
            0x00, // last_section_number
            // Entry: program_number=1, PMT_PID=0x100
            0x00, 0x01, 0xE1, 0x00, // CRC32 (not validated)
            0x00, 0x00, 0x00, 0x00,
        ];

        let entries = parse_pat_section(&data).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].program_number, 1);
        assert_eq!(entries[0].pmt_pid, 0x100);
    }

    #[test]
    fn test_parse_pat_skips_nit() {
        let data = [
            0x00, 0xB0, 0x11, // section_length=17
            0x00, 0x01, 0xC1, 0x00, 0x00, // NIT entry: program_number=0, PID=0x10
            0x00, 0x00, 0xE0, 0x10, // Program entry: program_number=1, PMT_PID=0x100
            0x00, 0x01, 0xE1, 0x00, // CRC32
            0x00, 0x00, 0x00, 0x00,
        ];

        let entries = parse_pat_section(&data).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].program_number, 1);
    }
}
