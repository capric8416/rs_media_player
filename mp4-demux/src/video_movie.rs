//! Movie-level discovery of classic video tracks.

use std::collections::HashMap;
use std::io::{Read, Seek};

use crate::video_description::child;
use crate::{
    EditList, FourCc, FragmentDefaults, FragmentRun, Mp4Error, Mp4ErrorKind, Mp4Result,
    SeekBoxReader, TrackFragmentHeader, VideoTrack, parse_tfdt, parse_trex,
};

/// Video tracks discovered by scanning a seekable MP4 source.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VideoMovie {
    pub tracks: Vec<VideoTrack>,
    pub movie_timescale: Option<u32>,
    fragment_defaults: HashMap<u32, FragmentDefaults>,
    fragment_decode_times: HashMap<u32, u64>,
}

impl VideoMovie {
    /// Appends one already-parsed `moof`/`traf` run to its matching video track.
    ///
    /// # Errors
    ///
    /// Returns invalid-data when no discovered video track has the run's track ID or when sample
    /// description references are invalid.
    pub fn append_fragment_run(&mut self, run: &FragmentRun) -> Mp4Result<()> {
        let track = self
            .tracks
            .iter_mut()
            .find(|track| track.index.track_id == run.track_id)
            .ok_or_else(|| movie_error(0, "fragment run has no matching video track"))?;
        track.append_fragment_run(run)
    }
}

impl VideoMovie {
    /// Scans top-level boxes and parses the movie's classic video sample tables.
    ///
    /// `max_metadata_bytes` bounds each `moov` and `moof` allocation. Media data is skipped by
    /// seeking, so the entire file is never loaded into memory.
    ///
    /// # Errors
    ///
    /// Returns a typed error for malformed boxes, duplicate or missing `moov`, oversized metadata,
    /// or malformed classic video tracks. Fragmented sample runs are not assembled here.
    pub fn read<R: Read + Seek>(source: R, max_metadata_bytes: usize) -> Mp4Result<Self> {
        let mut reader = SeekBoxReader::new(source)?;
        let mut movie = None;
        while let Some(header) = reader.next_top_level_box()? {
            if header.box_type() == FourCc::new(*b"moov") {
                if movie.is_some() {
                    return Err(movie_error(header.offset(), "duplicate moov box"));
                }
                let payload = reader.read_payload_to_vec(header, max_metadata_bytes)?;
                let tracks = parse_moov_video_tracks(&payload)?;
                let fragment_defaults = parse_fragment_defaults(&payload)?;
                let movie_timescale = parse_movie_timescale(&payload)?;
                movie = Some(VideoMovie {
                    tracks,
                    movie_timescale,
                    fragment_defaults,
                    fragment_decode_times: HashMap::new(),
                });
            } else if header.box_type() == FourCc::new(*b"moof") {
                let payload = reader.read_payload_to_vec(header, max_metadata_bytes)?;
                let current = movie.as_mut().ok_or_else(|| {
                    movie_error(
                        header.offset(),
                        "moof encountered before initialization moov",
                    )
                })?;
                current.append_moof(&payload, header.offset())?;
            } else {
                reader.skip_payload(header)?;
            }
        }
        movie.ok_or_else(|| movie_error(0, "missing moov box"))
    }
}

impl VideoMovie {
    fn append_moof(&mut self, payload: &[u8], moof_offset: u64) -> Mp4Result<()> {
        let mut position = 0;
        let mut implicit_data_offset = moof_offset;
        while position < payload.len() {
            let (kind, data, end) = child(payload, position)?;
            if kind == FourCc::new(*b"traf") {
                implicit_data_offset = self.append_traf(data, moof_offset, implicit_data_offset)?;
            }
            position = end;
        }
        Ok(())
    }

    fn append_traf(
        &mut self,
        payload: &[u8],
        moof_offset: u64,
        implicit_data_offset: u64,
    ) -> Mp4Result<u64> {
        let mut tfhd_data = None;
        let mut tfdt_data = None;
        let mut truns = Vec::new();
        let mut position = 0;
        while position < payload.len() {
            let (kind, data, end) = child(payload, position)?;
            match &kind.as_bytes() {
                b"tfhd" => set_unique(&mut tfhd_data, data, position)?,
                b"tfdt" => set_unique(&mut tfdt_data, data, position)?,
                b"trun" => {
                    truns
                        .try_reserve(1)
                        .map_err(|_| movie_error(position as u64, "could not reserve trun list"))?;
                    truns.push(data);
                }
                _ => {}
            }
            position = end;
        }
        let tfhd_data = tfhd_data.ok_or_else(|| movie_error(0, "traf is missing tfhd"))?;
        // Read the track ID first so defaults can be selected before parsing optional tfhd fields.
        let track_id = read_nonzero_u32(tfhd_data, 4, "tfhd track_ID")?;
        let defaults = self
            .fragment_defaults
            .get(&track_id)
            .copied()
            .ok_or_else(|| movie_error(0, "tfhd track has no trex defaults"))?;
        let header = TrackFragmentHeader::parse_tfhd(
            tfhd_data,
            defaults,
            moof_offset,
            implicit_data_offset,
        )?;
        let mut decode_time = if let Some(data) = tfdt_data {
            parse_tfdt(data)?
        } else {
            self.fragment_decode_times
                .get(&track_id)
                .copied()
                .unwrap_or_else(|| {
                    self.tracks
                        .iter()
                        .find(|track| track.index.track_id == track_id)
                        .and_then(|track| track.index.samples().last())
                        .and_then(|sample| sample.dts.checked_add(i64::from(sample.duration)))
                        .and_then(|time| u64::try_from(time).ok())
                        .unwrap_or(0)
                })
        };
        let mut continuation_offset = None;
        let mut next_data_offset = header.base_data_offset;
        for trun in truns {
            let run = FragmentRun::parse_trun(trun, &header, decode_time, continuation_offset)?;
            decode_time = run.next_decode_time;
            next_data_offset = run.next_data_offset;
            continuation_offset = Some(next_data_offset);
            if self
                .tracks
                .iter()
                .any(|track| track.index.track_id == track_id)
            {
                self.append_fragment_run(&run)?;
            }
        }
        self.fragment_decode_times.insert(track_id, decode_time);
        Ok(next_data_offset)
    }
}

/// Parses `trak` children in a `moov` payload, returning only video tracks.
///
/// # Errors
///
/// Returns invalid-data or bounds errors for malformed containers and typed sample-table errors
/// for unsupported or inconsistent video tracks.
pub fn parse_moov_video_tracks(payload: &[u8]) -> Mp4Result<Vec<VideoTrack>> {
    let movie_timescale = parse_movie_timescale(payload)?;
    let mut tracks = Vec::new();
    let mut position = 0;
    while position < payload.len() {
        let (kind, data, end) = child(payload, position)?;
        if kind == FourCc::new(*b"trak")
            && let Some(track) = parse_trak(data, movie_timescale)?
        {
            tracks.try_reserve(1).map_err(|_| {
                Mp4Error::new(
                    Mp4ErrorKind::BufferLimit,
                    "parse_moov_video_tracks",
                    position as u64,
                    "could not reserve video track list",
                )
            })?;
            tracks.push(track);
        }
        position = end;
    }
    Ok(tracks)
}

fn parse_movie_timescale(payload: &[u8]) -> Mp4Result<Option<u32>> {
    let mut timescale = None;
    let mut position = 0;
    while position < payload.len() {
        let (kind, data, end) = child(payload, position)?;
        if kind == FourCc::new(*b"mvhd") {
            let version = full_box_version(data, "mvhd")?;
            if data.get(1..4) != Some(&[0, 0, 0]) {
                return Err(movie_error(position as u64, "mvhd flags must be zero"));
            }
            let offset = match version {
                0 => 12,
                1 => 20,
                _ => return Err(movie_error(position as u64, "unsupported mvhd version")),
            };
            let value = read_nonzero_u32(data, offset, "mvhd timescale must be nonzero")?;
            set_unique(&mut timescale, value, position)?;
        }
        position = end;
    }
    Ok(timescale)
}

fn parse_fragment_defaults(payload: &[u8]) -> Mp4Result<HashMap<u32, FragmentDefaults>> {
    let mut defaults = HashMap::new();
    let mut position = 0;
    while position < payload.len() {
        let (kind, data, end) = child(payload, position)?;
        if kind == FourCc::new(*b"mvex") {
            let mut child_position = 0;
            while child_position < data.len() {
                let (child_kind, child_data, child_end) = child(data, child_position)?;
                if child_kind == FourCc::new(*b"trex") {
                    let entry = parse_trex(child_data)?;
                    if defaults.insert(entry.track_id, entry).is_some() {
                        return Err(movie_error(
                            child_position as u64,
                            "duplicate trex track defaults",
                        ));
                    }
                }
                child_position = child_end;
            }
        }
        position = end;
    }
    Ok(defaults)
}

fn parse_trak(payload: &[u8], movie_timescale: Option<u32>) -> Mp4Result<Option<VideoTrack>> {
    let mut track_header = None;
    let mut mdia = None;
    let mut edts = None;
    let mut position = 0;
    while position < payload.len() {
        let (kind, data, end) = child(payload, position)?;
        if kind == FourCc::new(*b"tkhd") {
            set_unique(&mut track_header, parse_tkhd(data)?, position)?;
        } else if kind == FourCc::new(*b"mdia") {
            set_unique(&mut mdia, data, position)?;
        } else if kind == FourCc::new(*b"edts") {
            set_unique(&mut edts, data, position)?;
        }
        position = end;
    }
    let (track_id, display_rotation) =
        track_header.ok_or_else(|| movie_error(0, "trak is missing tkhd"))?;
    let mdia = mdia.ok_or_else(|| movie_error(0, "trak is missing mdia"))?;
    let mut timescale = None;
    let mut is_video = None;
    let mut minf = None;
    position = 0;
    while position < mdia.len() {
        let (kind, data, end) = child(mdia, position)?;
        match &kind.as_bytes() {
            b"mdhd" => set_unique(&mut timescale, parse_mdhd(data)?, position)?,
            b"hdlr" => set_unique(&mut is_video, parse_hdlr(data)?, position)?,
            b"minf" => set_unique(&mut minf, data, position)?,
            _ => {}
        }
        position = end;
    }
    let is_video = is_video.ok_or_else(|| movie_error(0, "mdia is missing hdlr"))?;
    if !is_video {
        return Ok(None);
    }
    let timescale = timescale.ok_or_else(|| movie_error(0, "video mdia is missing mdhd"))?;
    let minf = minf.ok_or_else(|| movie_error(0, "video mdia is missing minf"))?;
    let mut stbl = None;
    position = 0;
    while position < minf.len() {
        let (kind, data, end) = child(minf, position)?;
        if kind == FourCc::new(*b"stbl") {
            set_unique(&mut stbl, data, position)?;
        }
        position = end;
    }
    let stbl = stbl.ok_or_else(|| movie_error(0, "video minf is missing stbl"))?;
    let mut track = VideoTrack::from_stbl(track_id, timescale, stbl)?;
    track.display_rotation = display_rotation;
    if let Some(edts_payload) = edts {
        let mut elst = None;
        let mut edit_position = 0;
        while edit_position < edts_payload.len() {
            let (kind, data, end) = child(edts_payload, edit_position)?;
            if kind == FourCc::new(*b"elst") {
                set_unique(&mut elst, data, edit_position)?;
            }
            edit_position = end;
        }
        if let Some(elst_payload) = elst {
            if movie_timescale.is_none() {
                return Err(movie_error(0, "elst requires mvhd movie timescale"));
            }
            track.edit_list = Some(EditList::parse(elst_payload)?);
        }
    }
    Ok(Some(track))
}

fn parse_tkhd(data: &[u8]) -> Mp4Result<(u32, crate::DisplayRotation)> {
    let version = full_box_version(data, "tkhd")?;
    let offset = match version {
        0 => 12,
        1 => 20,
        _ => return Err(movie_error(0, "unsupported tkhd version")),
    };
    let track_id = read_nonzero_u32(data, offset, "tkhd track_ID")?;
    let matrix_offset = match version {
        0 => 40,
        1 => 52,
        _ => unreachable!("version was checked above"),
    };
    let matrix = data
        .get(matrix_offset..matrix_offset + 36)
        .ok_or_else(|| movie_error(matrix_offset as u64, "truncated tkhd display matrix"))?;
    let fixed = |index: usize| {
        i32::from_be_bytes([
            matrix[index],
            matrix[index + 1],
            matrix[index + 2],
            matrix[index + 3],
        ])
    };
    // tkhd stores a 16.16 affine matrix as [a, b, u, c, d, v, x, y, w].
    // Matrix signatures follow the QuickTime/FFmpeg convention; the clockwise output
    // rotation is the inverse of atan2(b, a) in its screen-coordinate transform.
    let one = 1 << 16;
    let a = fixed(0);
    let b = fixed(4);
    let c = fixed(12);
    let d = fixed(16);
    if fixed(8) != 0 || fixed(20) != 0 || fixed(32) != 0x4000_0000 {
        return Err(movie_error(
            matrix_offset as u64,
            "unsupported perspective tkhd display matrix",
        ));
    }
    let rotation = if (a, b, c, d) == (one, 0, 0, one) {
        crate::DisplayRotation::Degrees0
    } else if (a, b, c, d) == (0, -one, one, 0) {
        crate::DisplayRotation::Degrees90
    } else if (a, b, c, d) == (-one, 0, 0, -one) {
        crate::DisplayRotation::Degrees180
    } else if (a, b, c, d) == (0, one, -one, 0) {
        crate::DisplayRotation::Degrees270
    } else {
        return Err(movie_error(
            matrix_offset as u64,
            "unsupported non-orthogonal tkhd display matrix",
        ));
    };
    Ok((track_id, rotation))
}

fn parse_mdhd(data: &[u8]) -> Mp4Result<u32> {
    let version = full_box_version(data, "mdhd")?;
    let offset = match version {
        0 => 12,
        1 => 20,
        _ => return Err(movie_error(0, "unsupported mdhd version")),
    };
    read_nonzero_u32(data, offset, "mdhd timescale")
}

fn parse_hdlr(data: &[u8]) -> Mp4Result<bool> {
    full_box_version(data, "hdlr")?;
    let handler = data
        .get(8..12)
        .ok_or_else(|| movie_error(0, "truncated hdlr handler_type"))?;
    Ok(handler == b"vide")
}

fn full_box_version(data: &[u8], name: &'static str) -> Mp4Result<u8> {
    data.first().copied().ok_or_else(|| {
        movie_error(
            0,
            match name {
                "tkhd" => "truncated tkhd full-box header",
                "mdhd" => "truncated mdhd full-box header",
                _ => "truncated hdlr full-box header",
            },
        )
    })
}

fn read_nonzero_u32(data: &[u8], offset: usize, name: &'static str) -> Mp4Result<u32> {
    let bytes = data
        .get(offset..offset + 4)
        .ok_or_else(|| movie_error(offset as u64, "truncated required full-box field"))?;
    let value = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    if value == 0 {
        return Err(movie_error(offset as u64, name));
    }
    Ok(value)
}

fn set_unique<T>(slot: &mut Option<T>, value: T, offset: usize) -> Mp4Result<()> {
    if slot.is_some() {
        return Err(movie_error(
            offset as u64,
            "duplicate box in track hierarchy",
        ));
    }
    *slot = Some(value);
    Ok(())
}

fn movie_error(offset: u64, detail: &'static str) -> Mp4Error {
    Mp4Error::new(
        Mp4ErrorKind::InvalidData,
        "VideoMovie::read",
        offset,
        detail,
    )
}
