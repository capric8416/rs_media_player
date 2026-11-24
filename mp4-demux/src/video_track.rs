//! Video track sample-table assembly.

use std::io::{ErrorKind, Read, Seek, SeekFrom};

use crate::edit_list::{EditListEntry, map_entry, rescale_near_inf};
use crate::video_description::child;
use crate::{
    ChunkOffsetBox, ClassicTableBoxes, EditList, FragmentRun, Mp4Error, Mp4ErrorKind, Mp4Result,
    Sample, SampleIndex, SampleSizeBox, VideoSampleDescription,
};

const MAX_PRESENTED_SAMPLES: usize = 16_777_216;

/// A sample index and its timestamp after movie edit-list mapping.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PresentedSample {
    pub sample_index: usize,
    pub presentation_time: i64,
}

/// A sample needed in decode order while applying an edit list.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DecodeSample {
    pub sample_index: usize,
    pub edit_index: Option<usize>,
    pub discard_output: bool,
    /// Edit-adjusted DTS in the track's media timescale.
    pub decode_timestamp: i64,
    /// Movie-timescale PTS for visible samples; preroll and trailing decode-only samples use None.
    pub presentation_time: Option<i64>,
}

/// A video track's descriptions and classic, non-fragmented sample index.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VideoTrack {
    pub descriptions: Vec<VideoSampleDescription>,
    pub index: SampleIndex,
    pub edit_list: Option<EditList>,
}

impl VideoTrack {
    /// Parses an `stbl` payload for a video track with known `track_id` and `timescale`.
    ///
    /// The caller obtains the track identity and timescale from `tkhd` and `mdhd`, respectively.
    /// Boxes outside the classic video sample table are skipped. Fragmented MP4 runs and edit
    /// lists are attached by movie-level parsing when an `edts/elst` is present.
    ///
    /// # Errors
    ///
    /// Returns errors for missing, duplicate, truncated or inconsistent tables, unsupported sample
    /// descriptions, or sample indices that do not address an existing description.
    pub fn from_stbl(track_id: u32, timescale: u32, payload: &[u8]) -> Mp4Result<Self> {
        let mut descriptions_box = None;
        let mut timing_box = None;
        let mut composition_box = None;
        let mut chunk_mapping = None;
        let mut sizes = None;
        let mut offsets = None;
        let mut sync_samples = None;
        let mut position = 0;
        while position < payload.len() {
            let (kind, data, end) = child(payload, position)?;
            match &kind.as_bytes() {
                b"stsd" => set_once(&mut descriptions_box, data, position)?,
                b"stts" => set_once(&mut timing_box, data, position)?,
                b"ctts" => set_once(&mut composition_box, data, position)?,
                b"stsc" => set_once(&mut chunk_mapping, data, position)?,
                b"stsz" => set_once(&mut sizes, SampleSizeBox::Stsz(data), position)?,
                b"stz2" => set_once(&mut sizes, SampleSizeBox::Stz2(data), position)?,
                b"stco" => set_once(&mut offsets, ChunkOffsetBox::Stco(data), position)?,
                b"co64" => set_once(&mut offsets, ChunkOffsetBox::Co64(data), position)?,
                b"stss" => set_once(&mut sync_samples, data, position)?,
                _ => {}
            }
            position = end;
        }
        let descriptions =
            VideoSampleDescription::parse_stsd(required(descriptions_box, "missing stsd")?)?;
        let index = SampleIndex::from_classic(
            track_id,
            timescale,
            ClassicTableBoxes {
                stts: required(timing_box, "missing stts")?,
                ctts: composition_box,
                stsc: required(chunk_mapping, "missing stsc")?,
                sample_sizes: required(sizes, "missing stsz/stz2")?,
                chunk_offsets: required(offsets, "missing stco/co64")?,
                stss: sync_samples,
            },
        )?;
        for sample in index.samples() {
            description_for_sample(&descriptions, sample)?;
        }
        Ok(Self {
            descriptions,
            index,
            edit_list: None,
        })
    }

    /// Looks up the sample's one-based sample-description index.
    ///
    /// # Errors
    ///
    /// Returns invalid-data if the index is zero, out of bounds or not representable.
    pub fn description_for(&self, sample: &Sample) -> Mp4Result<&VideoSampleDescription> {
        description_for_sample(&self.descriptions, sample)
    }

    /// Maps a sample PTS to movie-timescale units, applying this track's edit list when present.
    ///
    /// # Errors
    ///
    /// Returns invalid-data for zero movie timescale, or overflow if conversion cannot fit in
    /// `i64`. Timebase conversion rounds to nearest, ties away from zero.
    pub fn presentation_time_in_movie_timescale(
        &self,
        sample: &Sample,
        movie_timescale: u32,
    ) -> Mp4Result<Option<i64>> {
        description_for_sample(&self.descriptions, sample)?;
        if let Some(edit_list) = &self.edit_list {
            return edit_list.map_presentation_time(
                sample.pts,
                self.index.timescale,
                movie_timescale,
            );
        }
        if movie_timescale == 0 {
            return Err(track_error(0, "movie timescale must be nonzero"));
        }
        let mapped = rescale_near_inf(
            i128::from(sample.pts) * i128::from(movie_timescale),
            i128::from(self.index.timescale),
        );
        i64::try_from(mapped)
            .map(Some)
            .map_err(|_| track_error(0, "movie presentation timestamp overflowed"))
    }

    /// Builds the movie-timeline presentation sequence, applying edit selection and repetition.
    ///
    /// Samples whose PTS falls outside every nonempty edit are omitted. Repeated edit intervals
    /// yield multiple entries referring to the same sample. The result is stably ordered by movie
    /// presentation timestamp.
    ///
    /// # Errors
    ///
    /// Returns invalid-data for invalid timescales, timestamp overflow, or a buffer-limit error
    /// when the result exceeds the sample safety ceiling. Edit media rates are retained but ignored
    /// to match `FFmpeg`'s MOV index mapping.
    pub fn presented_samples(&self, movie_timescale: u32) -> Mp4Result<Vec<PresentedSample>> {
        if movie_timescale == 0 {
            return Err(track_error(0, "movie timescale must be nonzero"));
        }
        let mut presented = Vec::new();
        for (sample_index, sample) in self.index.samples().iter().enumerate() {
            let mapped_times = if let Some(edit_list) = &self.edit_list {
                edit_list.map_presentation_times(
                    sample.pts,
                    self.index.timescale,
                    movie_timescale,
                )?
            } else {
                let mapped = rescale_near_inf(
                    i128::from(sample.pts) * i128::from(movie_timescale),
                    i128::from(self.index.timescale),
                );
                vec![
                    i64::try_from(mapped)
                        .map_err(|_| track_error(0, "movie presentation timestamp overflowed"))?,
                ]
            };
            let new_length = presented
                .len()
                .checked_add(mapped_times.len())
                .ok_or_else(|| track_error(0, "presented sample count overflowed"))?;
            if new_length > MAX_PRESENTED_SAMPLES {
                return Err(Mp4Error::new(
                    Mp4ErrorKind::BufferLimit,
                    "VideoTrack::presented_samples",
                    0,
                    "presented sample count exceeds safety ceiling",
                ));
            }
            presented.try_reserve(mapped_times.len()).map_err(|_| {
                Mp4Error::new(
                    Mp4ErrorKind::BufferLimit,
                    "VideoTrack::presented_samples",
                    0,
                    "could not reserve presentation sequence",
                )
            })?;
            presented.extend(
                mapped_times
                    .into_iter()
                    .map(|presentation_time| PresentedSample {
                        sample_index,
                        presentation_time,
                    }),
            );
        }
        presented.sort_by_key(|sample| sample.presentation_time);
        Ok(presented)
    }

    /// Finds the nearest sync sample on the movie timeline after applying this track's edits.
    ///
    /// Repeated edit ranges may return the same media sample at different movie timestamps.
    /// `PreviousSync` chooses the latest timestamp at or before the target; `NextSync` chooses
    /// the earliest timestamp at or after it. Equal timestamps retain sample-index order.
    ///
    /// # Errors
    ///
    /// Returns timescale, timestamp-overflow, or allocation errors from presentation mapping.
    pub fn seek_sync_sample(
        &self,
        movie_time: i64,
        movie_timescale: u32,
        mode: crate::sample_table::SeekMode,
    ) -> Mp4Result<Option<PresentedSample>> {
        let samples = self.index.samples();
        let mut best: Option<PresentedSample> = None;
        for presented in self.presented_samples(movie_timescale)? {
            if !samples[presented.sample_index].is_sync {
                continue;
            }
            let is_better = match (mode, best) {
                (_, None) => match mode {
                    crate::sample_table::SeekMode::PreviousSync => {
                        presented.presentation_time <= movie_time
                    }
                    crate::sample_table::SeekMode::NextSync => {
                        presented.presentation_time >= movie_time
                    }
                },
                (crate::sample_table::SeekMode::PreviousSync, Some(current)) => {
                    presented.presentation_time <= movie_time
                        && presented.presentation_time > current.presentation_time
                }
                (crate::sample_table::SeekMode::NextSync, Some(current)) => {
                    presented.presentation_time >= movie_time
                        && presented.presentation_time < current.presentation_time
                }
            };
            if is_better {
                best = Some(presented);
            }
        }
        Ok(best)
    }

    /// Builds decode-order samples for the edit list, including keyframe preroll.
    ///
    /// Samples outside an edit's visible PTS interval are included with `discard_output` set when
    /// they lie between its preceding sync sample and visible samples. For reordered video, the
    /// first sync sample after an edit boundary and one more following sync sample are retained to
    /// cover trailing B frames.
    ///
    /// # Errors
    ///
    /// Returns timestamp, timescale, allocation, or output-count errors.
    pub fn decode_samples(&self, movie_timescale: u32) -> Mp4Result<Vec<DecodeSample>> {
        if movie_timescale == 0 {
            return Err(track_error(0, "movie timescale must be nonzero"));
        }
        let samples = self.index.samples();
        let mut output = Vec::new();
        let Some(edit_list) = &self.edit_list else {
            output.try_reserve(samples.len()).map_err(|_| {
                presentation_buffer_error("could not reserve decode sample sequence")
            })?;
            for (sample_index, sample) in samples.iter().enumerate() {
                let timestamp = rescale_near_inf(
                    i128::from(sample.pts) * i128::from(movie_timescale),
                    i128::from(self.index.timescale),
                );
                output.push(DecodeSample {
                    sample_index,
                    edit_index: None,
                    discard_output: false,
                    decode_timestamp: sample.dts,
                    presentation_time: Some(
                        i64::try_from(timestamp).map_err(|_| {
                            track_error(0, "movie presentation timestamp overflowed")
                        })?,
                    ),
                });
            }
            return Ok(output);
        };

        let movie_scale = i128::from(movie_timescale);
        let media_scale = i128::from(self.index.timescale);
        let has_composition_offsets = self.index.has_composition_offsets();
        let mut movie_start = 0_i128;
        let mut media_start = 0_i128;
        let mut minimum_corrected_pts = None;
        for (edit_index, edit) in edit_list.entries.iter().enumerate() {
            if edit.media_time != -1 {
                EditDecodeContext {
                    samples,
                    edit,
                    edit_index,
                    movie_start,
                    media_start,
                    media_scale,
                    movie_scale,
                    has_composition_offsets,
                }
                .append_to(&mut output, &mut minimum_corrected_pts)?;
            }
            movie_start += i128::from(edit.segment_duration);
            media_start +=
                rescale_near_inf(i128::from(edit.segment_duration) * media_scale, movie_scale);
        }
        if let Some(minimum) = minimum_corrected_pts {
            let empty_duration = edit_list
                .entries
                .iter()
                .take_while(|entry| entry.media_time == -1)
                .map(|entry| {
                    rescale_near_inf(
                        i128::from(entry.segment_duration) * media_scale,
                        movie_scale,
                    )
                })
                .sum::<i128>();
            let minimum = minimum - empty_duration;
            if minimum > 0 {
                for decoded in &mut output {
                    decoded.decode_timestamp =
                        i64::try_from(i128::from(decoded.decode_timestamp) - minimum)
                            .map_err(|_| track_error(0, "edit-adjusted DTS overflowed"))?;
                }
            }
        }
        for decoded in &mut output {
            if !decoded.discard_output {
                let sample = &samples[decoded.sample_index];
                let corrected_pts = i128::from(decoded.decode_timestamp) + i128::from(sample.pts)
                    - i128::from(sample.dts);
                let movie_pts = rescale_near_inf(corrected_pts * movie_scale, media_scale);
                decoded.presentation_time = Some(
                    i64::try_from(movie_pts)
                        .map_err(|_| track_error(0, "edited movie PTS overflowed"))?,
                );
            }
        }
        Ok(output)
    }

    /// Reads one compressed sample from its absolute file offset.
    ///
    /// `max_bytes` limits allocation for an individual sample. The source cursor is left just
    /// after the sample on success.
    ///
    /// # Errors
    ///
    /// Returns a typed bounds, allocation, truncation, or I/O error.
    pub fn read_sample<R: Read + Seek>(
        &self,
        source: &mut R,
        sample: &Sample,
        max_bytes: usize,
    ) -> Mp4Result<Vec<u8>> {
        description_for_sample(&self.descriptions, sample)?;
        let length = usize::try_from(sample.size).map_err(|_| {
            Mp4Error::new(
                Mp4ErrorKind::BufferLimit,
                "VideoTrack::read_sample",
                sample.offset,
                "sample size does not fit the address space",
            )
        })?;
        if length > max_bytes {
            return Err(Mp4Error::new(
                Mp4ErrorKind::BufferLimit,
                "VideoTrack::read_sample",
                sample.offset,
                format!("sample size {length} exceeds caller limit {max_bytes}"),
            ));
        }
        let mut bytes = Vec::new();
        bytes.try_reserve_exact(length).map_err(|_| {
            Mp4Error::new(
                Mp4ErrorKind::BufferLimit,
                "VideoTrack::read_sample",
                sample.offset,
                "could not reserve sample buffer",
            )
        })?;
        bytes.resize(length, 0);
        source
            .seek(SeekFrom::Start(sample.offset))
            .map_err(|error| sample_io_error(sample.offset, "seek to sample", &error))?;
        source
            .read_exact(&mut bytes)
            .map_err(|error| sample_io_error(sample.offset, "read sample bytes", &error))?;
        Ok(bytes)
    }

    /// Appends a parsed fragmented-MP4 run after validating track and description references.
    ///
    /// # Errors
    ///
    /// Returns invalid-data when the run targets another track or references an unknown sample
    /// description, and buffer-limit when the combined index exceeds its safety ceiling.
    pub fn append_fragment_run(&mut self, run: &FragmentRun) -> Mp4Result<()> {
        if run.track_id != self.index.track_id {
            return Err(track_error(0, "fragment run belongs to a different track"));
        }
        for sample in &run.samples {
            description_for_sample(&self.descriptions, sample)?;
        }
        self.index.append_fragment(&run.samples)
    }
}

struct EditDecodeContext<'a> {
    samples: &'a [Sample],
    edit: &'a EditListEntry,
    edit_index: usize,
    movie_start: i128,
    media_start: i128,
    media_scale: i128,
    movie_scale: i128,
    has_composition_offsets: bool,
}

impl EditDecodeContext<'_> {
    fn append_to(
        &self,
        output: &mut Vec<DecodeSample>,
        minimum_corrected_pts: &mut Option<i128>,
    ) -> Mp4Result<()> {
        let Self {
            samples,
            edit,
            edit_index,
            movie_start,
            media_start,
            media_scale,
            movie_scale,
            has_composition_offsets: _,
        } = *self;
        let visible = self.visible_samples()?;
        let (Some(first_visible), Some(last_visible)) =
            (visible.first().copied(), visible.last().copied())
        else {
            return Ok(());
        };
        let (decode_start, decode_end) = self.decode_bounds(first_visible, last_visible)?;
        let decode_count = decode_end - decode_start + 1;
        let new_count = output
            .len()
            .checked_add(decode_count)
            .ok_or_else(|| track_error(0, "decode sample count overflowed"))?;
        if new_count > MAX_PRESENTED_SAMPLES {
            return Err(presentation_buffer_error(
                "decode sample count exceeds safety ceiling",
            ));
        }
        output
            .try_reserve(decode_count)
            .map_err(|_| presentation_buffer_error("could not reserve decode samples"))?;
        let frame_durations = self.frame_durations(decode_start, decode_end)?;
        let mut dts_counter = media_start;
        for (sample_index, sample) in samples
            .iter()
            .enumerate()
            .take(decode_end + 1)
            .skip(decode_start)
        {
            let is_visible =
                map_entry(sample.pts, edit, movie_start, media_scale, movie_scale)?.is_some();
            let frame_position = sample_index - decode_start;
            let decode_timestamp = if sample_index < first_visible {
                let duration_to_visible = frame_durations[frame_position..]
                    .iter()
                    .take(first_visible - sample_index)
                    .sum::<i128>();
                dts_counter - duration_to_visible
            } else {
                let timestamp = dts_counter;
                dts_counter += frame_durations[frame_position];
                timestamp
            };
            if is_visible {
                let corrected_pts =
                    decode_timestamp + i128::from(sample.pts) - i128::from(sample.dts);
                *minimum_corrected_pts =
                    Some(minimum_corrected_pts.map_or(corrected_pts, |min| min.min(corrected_pts)));
            }
            output.push(DecodeSample {
                sample_index,
                edit_index: Some(edit_index),
                discard_output: !is_visible,
                decode_timestamp: i64::try_from(decode_timestamp)
                    .map_err(|_| track_error(0, "edit-adjusted DTS overflowed"))?,
                presentation_time: None,
            });
        }
        Ok(())
    }

    fn visible_samples(&self) -> Mp4Result<Vec<usize>> {
        let mut visible = Vec::new();
        for (sample_index, sample) in self.samples.iter().enumerate() {
            if map_entry(
                sample.pts,
                self.edit,
                self.movie_start,
                self.media_scale,
                self.movie_scale,
            )?
            .is_some()
            {
                visible.try_reserve(1).map_err(|_| {
                    presentation_buffer_error("could not reserve visible edit samples")
                })?;
                visible.push(sample_index);
            }
        }
        Ok(visible)
    }

    fn decode_bounds(
        &self,
        first_visible: usize,
        last_visible: usize,
    ) -> Mp4Result<(usize, usize)> {
        let samples = self.samples;
        let decode_start = samples[..=first_visible]
            .iter()
            .rposition(|sample| sample.is_sync)
            .unwrap_or(0);
        let media_duration = rescale_near_inf(
            i128::from(self.edit.segment_duration) * self.media_scale,
            self.movie_scale,
        );
        let media_end = i128::from(self.edit.media_time)
            .checked_add(media_duration)
            .ok_or_else(|| track_error(0, "edit media end overflowed"))?;
        let mut decode_end = last_visible;
        if let Some((sync_index, _)) =
            samples
                .iter()
                .enumerate()
                .skip(last_visible)
                .find(|(_, sample)| {
                    sample.is_sync
                        && i128::from(sample.pts) + i128::from(sample.duration) >= media_end
                })
        {
            decode_end = sync_index;
            if self.has_composition_offsets {
                decode_end = samples
                    .iter()
                    .enumerate()
                    .skip(sync_index + 1)
                    .find(|(_, sample)| sample.is_sync)
                    .map_or(samples.len() - 1, |(next_sync, _)| next_sync);
            }
        }
        Ok((decode_start, decode_end))
    }

    fn frame_durations(&self, decode_start: usize, decode_end: usize) -> Mp4Result<Vec<i128>> {
        let decode_count = decode_end - decode_start + 1;
        let mut frame_durations = Vec::new();
        frame_durations
            .try_reserve(decode_count)
            .map_err(|_| presentation_buffer_error("could not reserve edit frame durations"))?;
        for sample_index in decode_start..=decode_end {
            let duration = self.samples.get(sample_index + 1).map_or_else(
                || {
                    rescale_near_inf(
                        i128::from(self.edit.segment_duration) * self.media_scale,
                        self.movie_scale,
                    )
                },
                |next| i128::from(next.dts) - i128::from(self.samples[sample_index].dts),
            );
            frame_durations.push(duration);
        }
        Ok(frame_durations)
    }
}

fn sample_io_error(offset: u64, action: &'static str, error: &std::io::Error) -> Mp4Error {
    let kind = if error.kind() == ErrorKind::UnexpectedEof {
        Mp4ErrorKind::Truncated
    } else {
        Mp4ErrorKind::Io
    };
    Mp4Error::new(
        kind,
        "VideoTrack::read_sample",
        offset,
        format!("{action}: {error}"),
    )
}

fn description_for_sample<'a>(
    descriptions: &'a [VideoSampleDescription],
    sample: &Sample,
) -> Mp4Result<&'a VideoSampleDescription> {
    let index = sample
        .sample_description_index
        .checked_sub(1)
        .and_then(|index| usize::try_from(index).ok())
        .ok_or_else(|| track_error(0, "invalid sample description index"))?;
    descriptions
        .get(index)
        .ok_or_else(|| track_error(0, "sample description index exceeds stsd entry count"))
}

fn set_once<T>(slot: &mut Option<T>, value: T, offset: usize) -> Mp4Result<()> {
    if slot.is_some() {
        return Err(track_error(offset, "duplicate or conflicting sample table"));
    }
    *slot = Some(value);
    Ok(())
}

fn required<T>(value: Option<T>, message: &'static str) -> Mp4Result<T> {
    value.ok_or_else(|| track_error(0, message))
}

fn track_error(offset: usize, detail: &'static str) -> Mp4Error {
    Mp4Error::new(
        Mp4ErrorKind::InvalidData,
        "VideoTrack::from_stbl",
        u64::try_from(offset).unwrap_or(u64::MAX),
        detail,
    )
}

fn presentation_buffer_error(detail: &'static str) -> Mp4Error {
    Mp4Error::new(
        Mp4ErrorKind::BufferLimit,
        "VideoTrack::decode_samples",
        0,
        detail,
    )
}
