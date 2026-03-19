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
    /// Flags byte (keyframe, lacing, etc.).
    pub flags: u8,
    /// Total bytes consumed by the header (VINT track# + 2 timestamp + 1 flags).
    pub header_size: usize,
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

    let mut flags_buf = [0u8; 1];
    reader.read_exact(&mut flags_buf)?;
    let flags = flags_buf[0];

    let header_size = track_vint.width as usize + 2 + 1;

    Ok(BlockHeader {
        track_number: track_vint.value,
        relative_timestamp,
        flags,
        header_size,
    })
}

/// Lacing type encoded in the flags byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lacing {
    None,
    Xiph,
    FixedSize,
    Ebml,
}

impl BlockHeader {
    /// Decode the lacing type from the flags byte.
    pub fn lacing(&self) -> Lacing {
        match (self.flags >> 1) & 0x03 {
            0 => Lacing::None,
            1 => Lacing::Xiph,
            2 => Lacing::FixedSize,
            3 => Lacing::Ebml,
            _ => unreachable!(),
        }
    }

    /// Whether this is a keyframe (SimpleBlock flag, bit 7).
    pub fn is_keyframe(&self) -> bool {
        self.flags & 0x80 != 0
    }
}
