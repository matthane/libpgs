use crate::ebml::read_track_number;
use crate::error::PgsError;
use crate::io::SeekBufReader;
use std::io::{Read, Seek};

/// Parsed Block/SimpleBlock header fields.
#[derive(Debug)]
pub struct BlockHeader {
    /// Track number this block belongs to.
    pub track_number: u64,
    /// Relative timestamp (signed 16-bit, relative to Cluster timestamp).
    pub relative_timestamp: i16,
}

/// Read the Block/SimpleBlock header: track number, relative timestamp, and flags.
///
/// This reads only the minimal header needed to determine the track number.
/// The caller can then decide to read or skip the payload.
pub fn read_block_header<R: Read + Seek>(
    reader: &mut SeekBufReader<R>,
) -> Result<BlockHeader, PgsError> {
    let track_vint = read_track_number(reader)?;
    let relative_timestamp = reader.read_u16_be()? as i16;

    // Read and discard the flags byte.
    let mut flags_buf = [0u8; 1];
    reader.read_exact(&mut flags_buf)?;

    Ok(BlockHeader {
        track_number: track_vint.value,
        relative_timestamp,
    })
}
