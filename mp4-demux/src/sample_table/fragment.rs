use super::classic::Sample;
use super::reader::Reader;
use crate::{Mp4ErrorKind, Mp4Result};

const TFHD_BASE_DATA_OFFSET: u32 = 0x0000_0001;
const TFHD_SAMPLE_DESCRIPTION_INDEX: u32 = 0x0000_0002;
const TFHD_DEFAULT_SAMPLE_DURATION: u32 = 0x0000_0008;
const TFHD_DEFAULT_SAMPLE_SIZE: u32 = 0x0000_0010;
const TFHD_DEFAULT_SAMPLE_FLAGS: u32 = 0x0000_0020;
const TFHD_DURATION_IS_EMPTY: u32 = 0x0001_0000;
const TFHD_DEFAULT_BASE_IS_MOOF: u32 = 0x0002_0000;
const TFHD_KNOWN_FLAGS: u32 = TFHD_BASE_DATA_OFFSET
    | TFHD_SAMPLE_DESCRIPTION_INDEX
    | TFHD_DEFAULT_SAMPLE_DURATION
    | TFHD_DEFAULT_SAMPLE_SIZE
    | TFHD_DEFAULT_SAMPLE_FLAGS
    | TFHD_DURATION_IS_EMPTY
    | TFHD_DEFAULT_BASE_IS_MOOF;

const TRUN_DATA_OFFSET: u32 = 0x0000_0001;
const TRUN_FIRST_SAMPLE_FLAGS: u32 = 0x0000_0004;
const TRUN_SAMPLE_DURATION: u32 = 0x0000_0100;
const TRUN_SAMPLE_SIZE: u32 = 0x0000_0200;
const TRUN_SAMPLE_FLAGS: u32 = 0x0000_0400;
const TRUN_SAMPLE_CTS: u32 = 0x0000_0800;
const TRUN_KNOWN_FLAGS: u32 = TRUN_DATA_OFFSET
    | TRUN_FIRST_SAMPLE_FLAGS
    | TRUN_SAMPLE_DURATION
    | TRUN_SAMPLE_SIZE
    | TRUN_SAMPLE_FLAGS
    | TRUN_SAMPLE_CTS;
const SAMPLE_IS_NON_SYNC: u32 = 0x0001_0000;
const MAX_FRAGMENT_SAMPLES: usize = 16_777_216;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FragmentDefaults {
    pub track_id: u32,
    pub sample_description_index: u32,
    pub sample_duration: u32,
    pub sample_size: u32,
    pub sample_flags: u32,
}

/// Parses a Track Extends (`trex`) `FullBox` payload.
///
/// # Errors
///
/// Returns a typed error for invalid identifiers, versions, flags, or truncation.
pub fn parse_trex(data: &[u8]) -> Mp4Result<FragmentDefaults> {
    let mut reader = Reader::new(data, "parse_trex");
    let (_, flags) = reader.full_box(&[0])?;
    if flags != 0 {
        return Err(reader.error(Mp4ErrorKind::InvalidData, "trex flags must be zero"));
    }
    let result = FragmentDefaults {
        track_id: reader.u32()?,
        sample_description_index: reader.u32()?,
        sample_duration: reader.u32()?,
        sample_size: reader.u32()?,
        sample_flags: reader.u32()?,
    };
    reader.finish()?;
    if result.track_id == 0 || result.sample_description_index == 0 {
        return Err(reader.error(
            Mp4ErrorKind::InvalidData,
            "invalid trex track or description id",
        ));
    }
    Ok(result)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TrackFragmentHeader {
    pub track_id: u32,
    pub base_data_offset: u64,
    pub sample_description_index: u32,
    pub default_sample_duration: u32,
    pub default_sample_size: u32,
    pub default_sample_flags: u32,
    pub duration_is_empty: bool,
}

impl TrackFragmentHeader {
    /// Parses a Track Fragment Header (`tfhd`) and applies `trex` defaults.
    ///
    /// # Errors
    ///
    /// Returns a typed error for unknown flags, mismatched tracks, or truncated optional fields.
    pub fn parse_tfhd(
        data: &[u8],
        defaults: FragmentDefaults,
        moof_offset: u64,
        implicit_offset: u64,
    ) -> Mp4Result<Self> {
        let mut reader = Reader::new(data, "TrackFragmentHeader::parse_tfhd");
        let (_, flags) = reader.full_box(&[0])?;
        if flags & !TFHD_KNOWN_FLAGS != 0
            || flags & TFHD_BASE_DATA_OFFSET != 0 && flags & TFHD_DEFAULT_BASE_IS_MOOF != 0
        {
            return Err(reader.error(
                Mp4ErrorKind::InvalidData,
                "invalid or conflicting tfhd flags",
            ));
        }
        let track_id = reader.u32()?;
        if track_id == 0 || track_id != defaults.track_id {
            return Err(reader.error(Mp4ErrorKind::InvalidData, "tfhd track does not match trex"));
        }
        let base_data_offset = if flags & TFHD_BASE_DATA_OFFSET != 0 {
            reader.u64()?
        } else if flags & TFHD_DEFAULT_BASE_IS_MOOF != 0 {
            moof_offset
        } else {
            implicit_offset
        };
        let sample_description_index = if flags & TFHD_SAMPLE_DESCRIPTION_INDEX != 0 {
            reader.u32()?
        } else {
            defaults.sample_description_index
        };
        let default_sample_duration = if flags & TFHD_DEFAULT_SAMPLE_DURATION != 0 {
            reader.u32()?
        } else {
            defaults.sample_duration
        };
        let default_sample_size = if flags & TFHD_DEFAULT_SAMPLE_SIZE != 0 {
            reader.u32()?
        } else {
            defaults.sample_size
        };
        let default_sample_flags = if flags & TFHD_DEFAULT_SAMPLE_FLAGS != 0 {
            reader.u32()?
        } else {
            defaults.sample_flags
        };
        reader.finish()?;
        if sample_description_index == 0 {
            return Err(reader.error(Mp4ErrorKind::InvalidData, "tfhd description id is zero"));
        }
        Ok(Self {
            track_id,
            base_data_offset,
            sample_description_index,
            default_sample_duration,
            default_sample_size,
            default_sample_flags,
            duration_is_empty: flags & TFHD_DURATION_IS_EMPTY != 0,
        })
    }
}

/// Parses a Track Fragment Decode Time (`tfdt`) `FullBox` payload.
///
/// # Errors
///
/// Returns a typed error for unsupported versions, nonzero flags, or truncation.
pub fn parse_tfdt(data: &[u8]) -> Mp4Result<u64> {
    let mut reader = Reader::new(data, "parse_tfdt");
    let (version, flags) = reader.full_box(&[0, 1])?;
    if flags != 0 {
        return Err(reader.error(Mp4ErrorKind::InvalidData, "tfdt flags must be zero"));
    }
    let time = if version == 0 {
        u64::from(reader.u32()?)
    } else {
        reader.u64()?
    };
    reader.finish()?;
    Ok(time)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FragmentRun {
    pub track_id: u32,
    pub samples: Vec<Sample>,
    pub next_decode_time: u64,
    pub next_data_offset: u64,
}

impl FragmentRun {
    /// Parses a Track Run (`trun`) into the common sample representation.
    ///
    /// `continuation_offset` is the preceding run's `next_data_offset` when `trun` omits a data
    /// offset; pass `None` for the first run.
    ///
    /// # Errors
    ///
    /// Returns a typed error for invalid flags/defaults, truncation, counts, or arithmetic overflow.
    pub fn parse_trun(
        data: &[u8],
        header: &TrackFragmentHeader,
        base_decode_time: u64,
        continuation_offset: Option<u64>,
    ) -> Mp4Result<Self> {
        let mut reader = Reader::new(data, "FragmentRun::parse_trun");
        let (version, flags) = reader.full_box(&[0, 1])?;
        if flags & !TRUN_KNOWN_FLAGS != 0
            || flags & TRUN_FIRST_SAMPLE_FLAGS != 0 && flags & TRUN_SAMPLE_FLAGS != 0
        {
            return Err(reader.error(
                Mp4ErrorKind::InvalidData,
                "invalid or conflicting trun flags",
            ));
        }
        let count = usize::try_from(reader.u32()?)
            .map_err(|_| reader.error(Mp4ErrorKind::Overflow, "trun count exceeds usize"))?;
        if count > MAX_FRAGMENT_SAMPLES {
            return Err(reader.error(
                Mp4ErrorKind::BufferLimit,
                "trun count exceeds safety ceiling",
            ));
        }
        if header.duration_is_empty && count != 0 {
            return Err(reader.error(Mp4ErrorKind::InvalidData, "empty fragment contains samples"));
        }
        let data_offset = if flags & TRUN_DATA_OFFSET != 0 {
            Some(reader.i32()?)
        } else {
            None
        };
        let first_sample_flags = if flags & TRUN_FIRST_SAMPLE_FLAGS != 0 {
            Some(reader.u32()?)
        } else {
            None
        };
        let mut offset = if let Some(relative) = data_offset {
            add_signed(header.base_data_offset, relative, &reader)?
        } else {
            continuation_offset.unwrap_or(header.base_data_offset)
        };
        let mut dts = i64::try_from(base_decode_time)
            .map_err(|_| reader.error(Mp4ErrorKind::Overflow, "tfdt exceeds i64"))?;
        let mut samples = Vec::with_capacity(count);
        for index in 0..count {
            let duration = if flags & TRUN_SAMPLE_DURATION != 0 {
                reader.u32()?
            } else {
                header.default_sample_duration
            };
            let size = if flags & TRUN_SAMPLE_SIZE != 0 {
                reader.u32()?
            } else {
                header.default_sample_size
            };
            let sample_flags = if flags & TRUN_SAMPLE_FLAGS != 0 {
                reader.u32()?
            } else if index == 0 {
                first_sample_flags.unwrap_or(header.default_sample_flags)
            } else {
                header.default_sample_flags
            };
            let composition_offset = if flags & TRUN_SAMPLE_CTS != 0 {
                if version == 0 {
                    i64::from(reader.u32()?)
                } else {
                    i64::from(reader.i32()?)
                }
            } else {
                0
            };
            if duration == 0 {
                return Err(reader.error(Mp4ErrorKind::InvalidData, "fragment duration is zero"));
            }
            let pts = dts
                .checked_add(composition_offset)
                .ok_or_else(|| reader.error(Mp4ErrorKind::Overflow, "fragment PTS overflowed"))?;
            samples.push(Sample {
                offset,
                size,
                dts,
                pts,
                duration,
                sample_description_index: header.sample_description_index,
                is_sync: sample_flags & SAMPLE_IS_NON_SYNC == 0,
            });
            offset = offset.checked_add(u64::from(size)).ok_or_else(|| {
                reader.error(Mp4ErrorKind::Overflow, "fragment offset overflowed")
            })?;
            dts = dts
                .checked_add(i64::from(duration))
                .ok_or_else(|| reader.error(Mp4ErrorKind::Overflow, "fragment DTS overflowed"))?;
        }
        reader.finish()?;
        Ok(Self {
            track_id: header.track_id,
            samples,
            next_decode_time: u64::try_from(dts)
                .map_err(|_| reader.error(Mp4ErrorKind::Overflow, "negative next decode time"))?,
            next_data_offset: offset,
        })
    }
}

fn add_signed(base: u64, relative: i32, reader: &Reader<'_>) -> Mp4Result<u64> {
    if relative >= 0 {
        base.checked_add(u64::try_from(relative).unwrap_or(u64::MAX))
    } else {
        base.checked_sub(u64::from(relative.unsigned_abs()))
    }
    .ok_or_else(|| reader.error(Mp4ErrorKind::Overflow, "fragment data offset overflowed"))
}
