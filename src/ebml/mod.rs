pub mod vint;

pub use vint::{read_element_id, read_element_size, read_track_number, Vint};

// Well-known EBML/Matroska element IDs.
pub mod ids {
    // Top-level
    pub const EBML: u64 = 0x1A45DFA3;
    pub const SEGMENT: u64 = 0x18538067;

    // EBML header children
    pub const EBML_VERSION: u64 = 0x4286;
    pub const EBML_READ_VERSION: u64 = 0x42F7;
    pub const EBML_MAX_ID_LENGTH: u64 = 0x42F2;
    pub const EBML_MAX_SIZE_LENGTH: u64 = 0x42F3;
    pub const DOC_TYPE: u64 = 0x4282;
    pub const DOC_TYPE_VERSION: u64 = 0x4287;
    pub const DOC_TYPE_READ_VERSION: u64 = 0x4285;

    // Segment children
    pub const SEEK_HEAD: u64 = 0x114D9B74;
    pub const INFO: u64 = 0x1549A966;
    pub const TRACKS: u64 = 0x1654AE6B;
    pub const CUES: u64 = 0x1C53BB6B;
    pub const CLUSTER: u64 = 0x1F43B675;
    pub const CHAPTERS: u64 = 0x1043A770;
    pub const ATTACHMENTS: u64 = 0x1941A469;
    pub const TAGS: u64 = 0x1254C367;

    // SeekHead children
    pub const SEEK: u64 = 0x4DBB;
    pub const SEEK_ID: u64 = 0x53AB;
    pub const SEEK_POSITION: u64 = 0x53AC;

    // Info children
    pub const TIMESTAMP_SCALE: u64 = 0x2AD7B1;
    pub const DURATION: u64 = 0x4489;

    // Tracks children
    pub const TRACK_ENTRY: u64 = 0xAE;
    pub const TRACK_NUMBER: u64 = 0xD7;
    pub const TRACK_UID: u64 = 0x73C5;
    pub const TRACK_TYPE: u64 = 0x83;
    pub const FLAG_DEFAULT: u64 = 0x88;
    pub const FLAG_FORCED: u64 = 0x55AA;
    pub const TRACK_NAME: u64 = 0x536E;
    pub const CODEC_ID: u64 = 0x86;
    pub const LANGUAGE: u64 = 0x22B59C;
    pub const LANGUAGE_BCP47: u64 = 0x22B59D;

    // ContentEncodings (track-level compression/encryption)
    pub const CONTENT_ENCODINGS: u64 = 0x6D80;
    pub const CONTENT_ENCODING: u64 = 0x6240;
    pub const CONTENT_COMP_ALGO: u64 = 0x4254;
    pub const CONTENT_COMP_SETTINGS: u64 = 0x4255;
    pub const CONTENT_COMPRESSION: u64 = 0x5034;

    // Cluster children
    pub const TIMESTAMP: u64 = 0xE7;
    pub const SIMPLE_BLOCK: u64 = 0xA3;
    pub const BLOCK_GROUP: u64 = 0xA0;

    // BlockGroup children
    pub const BLOCK: u64 = 0xA1;
    pub const BLOCK_DURATION: u64 = 0x9B;

    // Cues children
    pub const CUE_POINT: u64 = 0xBB;
    pub const CUE_TIME: u64 = 0xB3;
    pub const CUE_TRACK_POSITIONS: u64 = 0xB7;
    pub const CUE_TRACK: u64 = 0xF7;
    pub const CUE_CLUSTER_POSITION: u64 = 0xF1;
    pub const CUE_RELATIVE_POSITION: u64 = 0xF0;

    // Tags children
    pub const TAG: u64 = 0x7373;
    pub const TARGETS: u64 = 0x63C0;
    pub const TAG_TRACK_UID: u64 = 0x63C5;
    pub const SIMPLE_TAG: u64 = 0x67C8;
    pub const TAG_NAME: u64 = 0x4587;
    pub const TAG_STRING: u64 = 0x4487;
}
