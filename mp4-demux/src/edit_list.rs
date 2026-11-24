//! ISO BMFF edit-list parsing and presentation-time mapping.

use crate::{Mp4Error, Mp4ErrorKind, Mp4Result};

const MAX_EDIT_ENTRIES: usize = 1_000_000;

/// One `elst` entry. `media_time == -1` denotes an empty edit interval.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EditListEntry {
    pub segment_duration: u64,
    pub media_time: i64,
    pub media_rate_integer: i16,
    pub media_rate_fraction: i16,
}

/// Parsed entries from a track's `edts/elst` boxes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EditList {
    pub entries: Vec<EditListEntry>,
}

impl EditList {
    /// Parses a version 0 or 1 Edit List full-box payload.
    ///
    /// # Errors
    ///
    /// Returns typed errors for unsupported versions, nonzero flags, truncation, excess entry
    /// counts, or allocation failure.
    pub fn parse(payload: &[u8]) -> Mp4Result<Self> {
        if payload.len() < 8 {
            return Err(edit_error(
                Mp4ErrorKind::Truncated,
                0,
                "elst header is truncated",
            ));
        }
        let version = payload[0];
        if !matches!(version, 0 | 1) {
            return Err(edit_error(
                Mp4ErrorKind::Unsupported,
                0,
                "unsupported elst version",
            ));
        }
        if payload[1..4] != [0, 0, 0] {
            return Err(edit_error(
                Mp4ErrorKind::InvalidData,
                1,
                "elst flags must be zero",
            ));
        }
        let count = usize::try_from(u32_at(payload, 4)?).map_err(|_| {
            edit_error(
                Mp4ErrorKind::Overflow,
                4,
                "elst entry count exceeds address space",
            )
        })?;
        if count > MAX_EDIT_ENTRIES {
            return Err(edit_error(
                Mp4ErrorKind::BufferLimit,
                4,
                "elst entry count exceeds safety ceiling",
            ));
        }
        let entry_size = if version == 0 { 12 } else { 20 };
        let required = count
            .checked_mul(entry_size)
            .and_then(|bytes| bytes.checked_add(8))
            .ok_or_else(|| edit_error(Mp4ErrorKind::Overflow, 4, "elst byte size overflowed"))?;
        if payload.len() < required {
            return Err(edit_error(
                Mp4ErrorKind::Truncated,
                payload.len() as u64,
                "elst entries are truncated",
            ));
        }
        if payload.len() != required {
            return Err(edit_error(
                Mp4ErrorKind::InvalidData,
                required as u64,
                "unexpected bytes after elst entries",
            ));
        }
        let mut entries = Vec::new();
        entries.try_reserve_exact(count).map_err(|_| {
            edit_error(
                Mp4ErrorKind::BufferLimit,
                4,
                "could not reserve elst entries",
            )
        })?;
        for index in 0..count {
            let offset = 8 + index * entry_size;
            let (segment_duration, media_time, rate_offset) = if version == 0 {
                (
                    u64::from(u32_at(payload, offset)?),
                    i64::from(i32_at(payload, offset + 4)?),
                    offset + 8,
                )
            } else {
                (
                    u64_at(payload, offset)?,
                    i64_at(payload, offset + 8)?,
                    offset + 16,
                )
            };
            entries.push(EditListEntry {
                segment_duration,
                media_time,
                media_rate_integer: i16::from_be_bytes([
                    payload[rate_offset],
                    payload[rate_offset + 1],
                ]),
                media_rate_fraction: i16::from_be_bytes([
                    payload[rate_offset + 2],
                    payload[rate_offset + 3],
                ]),
            });
        }
        Ok(Self { entries })
    }

    /// Maps a media presentation timestamp to the movie timeline.
    ///
    /// Empty edits advance the movie timeline without mapping a media sample. The first matching
    /// nonempty edit returns a timestamp in movie-timescale ticks. The media-rate fields are
    /// retained but ignored, matching `FFmpeg` 9.0.2's `mov_fix_index` behavior.
    ///
    /// # Errors
    ///
    /// Returns invalid-data for zero timescales, or overflow when the mapped timestamp cannot fit
    /// in `i64`.
    pub fn map_presentation_time(
        &self,
        media_pts: i64,
        media_timescale: u32,
        movie_timescale: u32,
    ) -> Mp4Result<Option<i64>> {
        Ok(self
            .map_presentation_times(media_pts, media_timescale, movie_timescale)?
            .into_iter()
            .next())
    }

    /// Maps one media timestamp through every matching edit entry.
    ///
    /// Repeated media intervals produce repeated movie timestamps, as required by edit lists that
    /// replay a segment. Timebase conversions use nearest, ties-away-from-zero rescaling.
    ///
    /// # Errors
    ///
    /// Returns invalid-data for zero timescales, or overflow when a mapped timestamp cannot fit in
    /// `i64`.
    pub fn map_presentation_times(
        &self,
        media_pts: i64,
        media_timescale: u32,
        movie_timescale: u32,
    ) -> Mp4Result<Vec<i64>> {
        if media_timescale == 0 || movie_timescale == 0 {
            return Err(edit_error(
                Mp4ErrorKind::InvalidData,
                0,
                "edit-list timescales must be nonzero",
            ));
        }
        let media_scale = i128::from(media_timescale);
        let movie_scale = i128::from(movie_timescale);
        let mut movie_start = 0_i128;
        let mut mapped_times = Vec::new();
        for entry in &self.entries {
            if let Some(timestamp) =
                map_entry(media_pts, entry, movie_start, media_scale, movie_scale)?
            {
                mapped_times.try_reserve(1).map_err(|_| {
                    edit_error(
                        Mp4ErrorKind::BufferLimit,
                        0,
                        "could not reserve mapped edit timestamps",
                    )
                })?;
                mapped_times.push(timestamp);
            }
            movie_start += i128::from(entry.segment_duration);
        }
        Ok(mapped_times)
    }
}

pub(crate) fn map_entry(
    media_pts: i64,
    entry: &EditListEntry,
    movie_start: i128,
    media_timescale: i128,
    movie_timescale: i128,
) -> Mp4Result<Option<i64>> {
    if media_timescale <= 0 || movie_timescale <= 0 {
        return Err(edit_error(
            Mp4ErrorKind::InvalidData,
            0,
            "edit-list timescales must be nonzero",
        ));
    }
    if entry.media_time == -1 {
        return Ok(None);
    }
    let delta = i128::from(media_pts) - i128::from(entry.media_time);
    let media_duration = rescale_near_inf(
        i128::from(entry.segment_duration) * media_timescale,
        movie_timescale,
    );
    if delta < 0 || delta >= media_duration {
        return Ok(None);
    }
    let mapped = movie_start + rescale_near_inf(delta * movie_timescale, media_timescale);
    i64::try_from(mapped).map(Some).map_err(|_| {
        edit_error(
            Mp4ErrorKind::Overflow,
            0,
            "mapped presentation time exceeds i64",
        )
    })
}

pub(crate) fn rescale_near_inf(value: i128, denominator: i128) -> i128 {
    let magnitude = value.unsigned_abs();
    let divisor = u128::try_from(denominator).unwrap_or(u128::MAX);
    let quotient = magnitude / divisor;
    let remainder = magnitude % divisor;
    let rounded = quotient + u128::from(remainder * 2 >= divisor);
    let signed = i128::try_from(rounded).unwrap_or(i128::MAX);
    if value < 0 { -signed } else { signed }
}

fn u32_at(bytes: &[u8], offset: usize) -> Mp4Result<u32> {
    let value = bytes.get(offset..offset + 4).ok_or_else(|| {
        edit_error(
            Mp4ErrorKind::Truncated,
            offset as u64,
            "elst integer is truncated",
        )
    })?;
    Ok(u32::from_be_bytes([value[0], value[1], value[2], value[3]]))
}

fn i32_at(bytes: &[u8], offset: usize) -> Mp4Result<i32> {
    Ok(i32::from_be_bytes(u32_at(bytes, offset)?.to_be_bytes()))
}

fn u64_at(bytes: &[u8], offset: usize) -> Mp4Result<u64> {
    let value = bytes.get(offset..offset + 8).ok_or_else(|| {
        edit_error(
            Mp4ErrorKind::Truncated,
            offset as u64,
            "elst integer is truncated",
        )
    })?;
    Ok(u64::from_be_bytes([
        value[0], value[1], value[2], value[3], value[4], value[5], value[6], value[7],
    ]))
}

fn i64_at(bytes: &[u8], offset: usize) -> Mp4Result<i64> {
    Ok(i64::from_be_bytes(u64_at(bytes, offset)?.to_be_bytes()))
}

fn edit_error(kind: Mp4ErrorKind, offset: u64, detail: &'static str) -> Mp4Error {
    Mp4Error::new(kind, "EditList", offset, detail)
}
