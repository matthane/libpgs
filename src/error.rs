use std::fmt;
use std::io;

/// All errors that can occur during PGS extraction.
#[derive(Debug)]
pub enum PgsError {
    Io(io::Error),
    /// Not a valid EBML file or unsupported DocType.
    InvalidEbml(String),
    /// EBML VINT encoding is malformed.
    InvalidVint,
    /// MKV structural element is malformed or unexpected.
    InvalidMkv(String),
    /// TS/M2TS packet structure is malformed.
    InvalidTs(String),
    /// PGS segment header is malformed.
    InvalidPgs(String),
    /// No PGS tracks found in the container.
    NoPgsTracks,
    /// The requested track ID was not found.
    TrackNotFound(u32),
    /// Container format could not be detected.
    UnknownFormat,
    /// Error during PGS encoding or segment construction.
    EncodingError(String),
}

impl fmt::Display for PgsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PgsError::Io(e) => write!(f, "I/O error: {e}"),
            PgsError::InvalidEbml(msg) => write!(f, "invalid EBML: {msg}"),
            PgsError::InvalidVint => write!(f, "malformed EBML VINT"),
            PgsError::InvalidMkv(msg) => write!(f, "invalid MKV: {msg}"),
            PgsError::InvalidTs(msg) => write!(f, "invalid TS: {msg}"),
            PgsError::InvalidPgs(msg) => write!(f, "invalid PGS: {msg}"),
            PgsError::NoPgsTracks => write!(f, "no PGS tracks found"),
            PgsError::TrackNotFound(id) => write!(f, "track {id} not found"),
            PgsError::UnknownFormat => write!(f, "unknown container format"),
            PgsError::EncodingError(msg) => write!(f, "encoding error: {msg}"),
        }
    }
}

impl std::error::Error for PgsError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            PgsError::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<io::Error> for PgsError {
    fn from(e: io::Error) -> Self {
        PgsError::Io(e)
    }
}
