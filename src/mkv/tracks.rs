use crate::ebml::{ids, read_element_id, read_element_size};
use crate::error::PgsError;
use crate::io::SeekBufReader;
use std::io::{Read, Seek};

/// The CodecID for PGS subtitle tracks in MKV.
pub const PGS_CODEC_ID: &str = "S_HDMV/PGS";

/// Track type value for subtitles in MKV.
pub const TRACK_TYPE_SUBTITLE: u64 = 0x11;

/// Content compression algorithm used on block data.
#[derive(Debug, Clone)]
pub enum ContentCompAlgo {
    /// zlib (deflate) compression. Decompress entire block payload.
    Zlib,
    /// Header stripping. Prepend `settings` bytes to each block payload.
    HeaderStripping(Vec<u8>),
}

/// Information about a PGS track found in the MKV container.
#[derive(Debug, Clone)]
pub struct MkvPgsTrack {
    /// Track number used in Block/SimpleBlock headers.
    pub track_number: u64,
    /// TrackUID for Tags matching.
    pub track_uid: Option<u64>,
    /// Language code (ISO 639-2 or BCP 47), if present.
    pub language: Option<String>,
    /// Track name / title, if present.
    pub name: Option<String>,
    /// FlagDefault — whether this track should be active by default.
    pub flag_default: Option<bool>,
    /// FlagForced — whether this track contains forced subtitles.
    pub flag_forced: Option<bool>,
    /// Content compression, if the track uses ContentEncodings.
    pub compression: Option<ContentCompAlgo>,
}

/// Parse the Tracks element and return all PGS tracks found.
pub fn parse_tracks<R: Read + Seek>(
    reader: &mut SeekBufReader<R>,
    tracks_position: u64,
) -> Result<Vec<MkvPgsTrack>, PgsError> {
    reader.seek_to(tracks_position)?;

    let id = read_element_id(reader)?;
    if id.value != ids::TRACKS {
        return Err(PgsError::InvalidMkv("expected Tracks element".into()));
    }
    let size = read_element_size(reader)?;
    let end = reader.position() + size.value;

    let mut pgs_tracks = Vec::new();

    while reader.position() < end {
        let child_id = read_element_id(reader)?;
        let child_size = read_element_size(reader)?;

        if child_id.value == ids::TRACK_ENTRY {
            if let Some(track) = parse_track_entry(reader, reader.position(), child_size.value)? {
                pgs_tracks.push(track);
            }
        } else {
            reader.skip(child_size.value)?;
        }
    }

    Ok(pgs_tracks)
}

fn parse_track_entry<R: Read + Seek>(
    reader: &mut SeekBufReader<R>,
    data_start: u64,
    data_size: u64,
) -> Result<Option<MkvPgsTrack>, PgsError> {
    let end = data_start + data_size;

    let mut track_number: Option<u64> = None;
    let mut track_uid: Option<u64> = None;
    let mut track_type: Option<u64> = None;
    let mut codec_id: Option<String> = None;
    let mut language: Option<String> = None;
    let mut name: Option<String> = None;
    let mut flag_default: Option<bool> = None;
    let mut flag_forced: Option<bool> = None;
    let mut compression: Option<ContentCompAlgo> = None;

    while reader.position() < end {
        let child_id = read_element_id(reader)?;
        let child_size = read_element_size(reader)?;

        match child_id.value {
            ids::TRACK_NUMBER => {
                track_number = Some(reader.read_uint_be(child_size.value as usize)?);
            }
            ids::TRACK_UID => {
                track_uid = Some(reader.read_uint_be(child_size.value as usize)?);
            }
            ids::TRACK_TYPE => {
                track_type = Some(reader.read_uint_be(child_size.value as usize)?);
            }
            ids::CODEC_ID => {
                codec_id = Some(reader.read_string(child_size.value as usize)?);
            }
            ids::TRACK_NAME => {
                name = Some(reader.read_string(child_size.value as usize)?);
            }
            ids::FLAG_DEFAULT => {
                flag_default = Some(reader.read_uint_be(child_size.value as usize)? == 1);
            }
            ids::FLAG_FORCED => {
                flag_forced = Some(reader.read_uint_be(child_size.value as usize)? == 1);
            }
            ids::LANGUAGE => {
                language = Some(reader.read_string(child_size.value as usize)?);
            }
            ids::LANGUAGE_BCP47 => {
                // BCP47 takes precedence over the old Language field.
                language = Some(reader.read_string(child_size.value as usize)?);
            }
            ids::CONTENT_ENCODINGS => {
                compression = parse_content_encodings(reader, reader.position(), child_size.value)?;
            }
            _ => {
                reader.skip(child_size.value)?;
            }
        }
    }

    // Check if this is a PGS subtitle track.
    let is_pgs =
        track_type == Some(TRACK_TYPE_SUBTITLE) && codec_id.as_deref() == Some(PGS_CODEC_ID);

    if is_pgs {
        let track_number = track_number
            .ok_or_else(|| PgsError::InvalidMkv("PGS track missing TrackNumber".into()))?;

        // Normalize "und" (undefined) language to None.
        if language.as_deref() == Some("und") {
            language = None;
        }

        // Normalize language codes to BCP 47 (ISO 639-1 where available).
        language = language.map(|l| crate::lang::normalize_language(&l));

        Ok(Some(MkvPgsTrack {
            track_number,
            track_uid,
            language,
            name,
            flag_default,
            flag_forced,
            compression,
        }))
    } else {
        Ok(None)
    }
}

/// Parse ContentEncodings → ContentEncoding → ContentCompression.
fn parse_content_encodings<R: Read + Seek>(
    reader: &mut SeekBufReader<R>,
    data_start: u64,
    data_size: u64,
) -> Result<Option<ContentCompAlgo>, PgsError> {
    let end = data_start + data_size;

    while reader.position() < end {
        let child_id = read_element_id(reader)?;
        let child_size = read_element_size(reader)?;

        if child_id.value == ids::CONTENT_ENCODING {
            if let Some(algo) = parse_content_encoding(reader, reader.position(), child_size.value)?
            {
                return Ok(Some(algo));
            }
        } else {
            reader.skip(child_size.value)?;
        }
    }

    Ok(None)
}

fn parse_content_encoding<R: Read + Seek>(
    reader: &mut SeekBufReader<R>,
    data_start: u64,
    data_size: u64,
) -> Result<Option<ContentCompAlgo>, PgsError> {
    let end = data_start + data_size;

    while reader.position() < end {
        let child_id = read_element_id(reader)?;
        let child_size = read_element_size(reader)?;

        if child_id.value == ids::CONTENT_COMPRESSION {
            return parse_content_compression(reader, reader.position(), child_size.value);
        } else {
            reader.skip(child_size.value)?;
        }
    }

    Ok(None)
}

fn parse_content_compression<R: Read + Seek>(
    reader: &mut SeekBufReader<R>,
    data_start: u64,
    data_size: u64,
) -> Result<Option<ContentCompAlgo>, PgsError> {
    let end = data_start + data_size;

    let mut algo: u64 = 0; // Default: zlib
    let mut settings: Vec<u8> = Vec::new();

    while reader.position() < end {
        let child_id = read_element_id(reader)?;
        let child_size = read_element_size(reader)?;

        match child_id.value {
            ids::CONTENT_COMP_ALGO => {
                algo = reader.read_uint_be(child_size.value as usize)?;
            }
            ids::CONTENT_COMP_SETTINGS => {
                settings = reader.read_bytes(child_size.value as usize)?;
            }
            _ => {
                reader.skip(child_size.value)?;
            }
        }
    }

    match algo {
        0 => Ok(Some(ContentCompAlgo::Zlib)),
        3 => Ok(Some(ContentCompAlgo::HeaderStripping(settings))),
        _ => Ok(None), // Unsupported algo (bzlib, lzo) — treat as uncompressed.
    }
}
