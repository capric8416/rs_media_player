//! Bounded primitives for playback-focused MP4 and fragmented MP4 demuxing.

mod box_header;
mod codec_config;
mod edit_list;
mod error;
mod incremental;
mod sample_table;
mod seek_reader;
mod video_description;
mod video_movie;
mod video_track;

pub use box_header::{BoxHeader, FourCc, HeaderParse, parse_box_header};
pub use codec_config::{
    AacConfig, Av1Config, AvcConfig, EsdsConfig, HevcConfig, HevcNalArray, OpusConfig,
};
pub use edit_list::{EditList, EditListEntry};
pub use error::{Mp4Error, Mp4ErrorKind, Mp4Result};
pub use incremental::IncrementalBoxReader;
pub use sample_table::{
    ChunkOffsetBox, ClassicTableBoxes, FragmentDefaults, FragmentRun, Sample, SampleIndex,
    SampleSizeBox, SeekMode, TrackFragmentHeader, parse_tfdt, parse_trex,
};
pub use seek_reader::SeekBoxReader;
pub use video_description::{
    ApertureRational, CleanAperture, CleanApertureCrop, PixelAspectRatio, VideoCodecConfig,
    VideoSampleDescription,
};
pub use video_movie::{VideoMovie, parse_moov_video_tracks};
pub use video_track::{DecodeSample, DisplayRotation, PresentedSample, VideoTrack};
