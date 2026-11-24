//! End-to-end MP4 sample submission, hardware decode, scale, and presentation.

use std::collections::HashSet;
use std::io::{Read, Seek};

use mp4_demux::{DecodeSample, Sample, VideoMovie, VideoSampleDescription};

use crate::{
    D3dDecode, D3dRender, D3dScale, EncodedSample, FrameSize, GpuFrame, PresentResult,
    RenderParameters, SampleTiming, ScaleRequest, TimeBase, VideoError, VideoErrorKind,
    VideoResult,
};

/// Result of submitting one MP4 access unit and processing all currently available output.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PlaybackStep {
    pub sample_index: usize,
    pub frames_decoded: usize,
    pub frames_presented: usize,
    pub last_present: Option<PresentResult>,
    pub end_of_stream: bool,
}

/// Options shared by MP4 indexing, output scaling, and presentation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlaybackOptions {
    pub track_number: usize,
    pub scale_request: ScaleRequest,
    pub render_parameters: RenderParameters,
    pub max_metadata_bytes: usize,
    pub max_sample_bytes: usize,
}

/// Seekable MP4 playback chain for a matching decode/scale/render backend.
///
/// The source is read one compressed sample at a time. Movie metadata and the sample index are
/// retained in memory; media payloads are not. Fragmented MP4 `moof` runs are indexed at open.
pub struct Mp4VideoPlayback<R, D, S, P>
where
    R: Read + Seek,
    D: D3dDecode,
    S: D3dScale<Frame = D::Frame>,
    P: D3dRender<Frame = D::Frame>,
{
    source: R,
    samples: Vec<Sample>,
    decode_order: Vec<DecodeSample>,
    visible_presentation_times: HashSet<i64>,
    descriptions: Vec<VideoSampleDescription>,
    media_timescale: u32,
    movie_timescale: u32,
    cursor: usize,
    active_description: Option<u32>,
    max_sample_bytes: usize,
    decoder: D,
    scaler: S,
    renderer: P,
    scale_request: ScaleRequest,
    render_parameters: RenderParameters,
}

impl<R, D, S, P> Mp4VideoPlayback<R, D, S, P>
where
    R: Read + Seek,
    D: D3dDecode,
    S: D3dScale<Frame = D::Frame>,
    P: D3dRender<Frame = D::Frame>,
{
    /// Scans an MP4 source, configures the decoder from its first sample description, then builds
    /// the decode-order playback chain.
    ///
    /// # Errors
    ///
    /// Returns MP4 parse, decoder configuration, or invalid-input errors for a missing track,
    /// empty sample table, invalid output size, or zero sample limit.
    pub fn open(
        mut source: R,
        decoder: D,
        scaler: S,
        renderer: P,
        options: PlaybackOptions,
    ) -> Result<Self, PlaybackError> {
        if options.max_sample_bytes == 0 {
            return Err(playback_input("sample byte limit must be nonzero"));
        }
        let movie = VideoMovie::read(&mut source, options.max_metadata_bytes)?;
        let track = movie
            .tracks
            .get(options.track_number)
            .ok_or_else(|| playback_input("video track index is out of range"))?;
        if track.index.samples().is_empty() {
            return Err(playback_input("selected video track has no samples"));
        }
        let movie_timescale = movie.movie_timescale.unwrap_or(track.index.timescale);
        if movie_timescale == 0 || track.index.timescale == 0 {
            return Err(playback_input("movie and media timescales must be nonzero"));
        }
        let first = &track.index.samples()[0];
        let description = track.description_for(first)?;
        let decoder = decoder_configure(decoder, description)?;
        let decode_order = track.decode_samples(movie_timescale)?;
        let visible_presentation_times = decode_order
            .iter()
            .filter(|entry| !entry.discard_output)
            .filter_map(|entry| entry.presentation_time)
            .collect();
        let samples = track.index.samples().to_vec();
        Ok(Self {
            source,
            samples,
            decode_order,
            visible_presentation_times,
            descriptions: track.descriptions.clone(),
            media_timescale: track.index.timescale,
            movie_timescale,
            cursor: 0,
            active_description: Some(first.sample_description_index),
            max_sample_bytes: options.max_sample_bytes,
            decoder,
            scaler,
            renderer,
            scale_request: options.scale_request,
            render_parameters: options.render_parameters,
        })
    }

    /// Reads, decodes, scales, and presents the next access unit.
    ///
    /// # Errors
    ///
    /// Returns MP4 I/O/index, decoder, scaler, or renderer errors.
    pub fn step(&mut self) -> Result<PlaybackStep, PlaybackError> {
        let Some(decoded) = self.decode_order.get(self.cursor).copied() else {
            return Ok(PlaybackStep {
                sample_index: self.samples.len(),
                frames_decoded: 0,
                frames_presented: 0,
                last_present: None,
                end_of_stream: true,
            });
        };
        let sample = *self
            .samples
            .get(decoded.sample_index)
            .ok_or_else(|| playback_input("decode sequence references a missing sample"))?;
        if self.active_description != Some(sample.sample_description_index) {
            let description = self.description(sample.sample_description_index)?.clone();
            decoder_configure_ref(&mut self.decoder, &description)?;
            self.active_description = Some(sample.sample_description_index);
        }
        let bytes = self.read_sample(sample)?;
        let movie_time_base = TimeBase::new(1, self.movie_timescale)?;
        let timing = SampleTiming {
            dts: Some(rescale_timestamp(
                decoded.decode_timestamp,
                self.movie_timescale,
                self.media_timescale,
            )?),
            pts: decoded.presentation_time,
            duration: Some(rescale_duration(
                u64::from(sample.duration),
                self.movie_timescale,
                self.media_timescale,
            )?),
            time_base: movie_time_base,
        };
        self.decoder.send_sample(EncodedSample {
            data: &bytes,
            timing,
            is_random_access: sample.is_sync,
        })?;
        self.cursor += 1;

        let mut frames_decoded = 0;
        let mut frames_presented = 0;
        let mut last_present = None;
        while let Some(frame) = self.decoder.receive_frame()? {
            frames_decoded += 1;
            if frame
                .info()
                .pts
                .is_some_and(|pts| self.visible_presentation_times.contains(&pts))
            {
                let scaled = self.scaler.scale(&frame, &self.scale_request)?;
                last_present = Some(self.renderer.render(&scaled, &self.render_parameters)?);
                frames_presented += 1;
            }
        }
        Ok(PlaybackStep {
            sample_index: decoded.sample_index,
            frames_decoded,
            frames_presented,
            last_present,
            end_of_stream: false,
        })
    }

    /// Drains decoder output after the last input sample and presents the remaining frames.
    ///
    /// # Errors
    ///
    /// Returns decoder, scaler, or renderer errors.
    pub fn drain(&mut self) -> Result<PlaybackStep, PlaybackError> {
        self.decoder.drain()?;
        let mut decoded = 0;
        let mut presented = 0;
        let mut last_present = None;
        while let Some(frame) = self.decoder.receive_frame()? {
            decoded += 1;
            if frame
                .info()
                .pts
                .is_some_and(|pts| self.visible_presentation_times.contains(&pts))
            {
                let scaled = self.scaler.scale(&frame, &self.scale_request)?;
                last_present = Some(self.renderer.render(&scaled, &self.render_parameters)?);
                presented += 1;
            }
        }
        Ok(PlaybackStep {
            sample_index: self.samples.len(),
            frames_decoded: decoded,
            frames_presented: presented,
            last_present,
            end_of_stream: true,
        })
    }

    /// Flushes decoder references and positions playback at the previous or next sync sample.
    ///
    /// # Errors
    ///
    /// Returns MP4 timestamp/index or decoder flush errors.
    pub fn seek(
        &mut self,
        movie_time: i64,
        mode: mp4_demux::SeekMode,
    ) -> Result<Option<usize>, PlaybackError> {
        let candidates = self
            .decode_order
            .iter()
            .enumerate()
            .filter_map(|(index, entry)| {
                let pts = entry.presentation_time?;
                let sample = self.samples.get(entry.sample_index)?;
                if !sample.is_sync {
                    return None;
                }
                let eligible = match mode {
                    mp4_demux::SeekMode::PreviousSync => pts <= movie_time,
                    mp4_demux::SeekMode::NextSync => pts >= movie_time,
                };
                eligible.then_some((index, pts))
            })
            .collect::<Vec<_>>();
        let target = match mode {
            mp4_demux::SeekMode::PreviousSync => candidates
                .into_iter()
                .max_by_key(|(index, pts)| (*pts, std::cmp::Reverse(*index)))
                .map(|(index, _)| index),
            mp4_demux::SeekMode::NextSync => candidates
                .into_iter()
                .min_by_key(|(index, pts)| (*pts, *index))
                .map(|(index, _)| index),
        };
        let Some(index) = target else { return Ok(None) };
        self.decoder.flush()?;
        self.cursor = index;
        self.active_description = None;
        Ok(Some(self.decode_order[index].sample_index))
    }

    /// Updates the scale operation used for subsequent frames.
    pub fn set_scale_request(&mut self, request: ScaleRequest) {
        self.scale_request = request;
    }

    /// Updates the render operation used for subsequent frames.
    pub fn set_render_parameters(&mut self, parameters: RenderParameters) {
        self.render_parameters = parameters;
    }

    /// Resizes the presentation target.
    ///
    /// # Errors
    ///
    /// Returns the backend's resize error.
    pub fn resize(&mut self, size: FrameSize) -> VideoResult<()> {
        self.renderer.resize(size)
    }

    /// Returns the number of indexed decode-order access units.
    #[must_use]
    pub fn sample_count(&self) -> usize {
        self.decode_order.len()
    }

    /// Returns the next decode-order cursor position.
    #[must_use]
    pub fn position(&self) -> usize {
        self.cursor
    }

    fn description(&self, index: u32) -> Result<&VideoSampleDescription, PlaybackError> {
        let zero_based = index
            .checked_sub(1)
            .and_then(|value| usize::try_from(value).ok())
            .ok_or_else(|| playback_input("sample description index is invalid"))?;
        self.descriptions
            .get(zero_based)
            .ok_or_else(|| playback_input("sample description index is out of range"))
    }

    fn read_sample(&mut self, sample: Sample) -> Result<Vec<u8>, PlaybackError> {
        let length = usize::try_from(sample.size)
            .map_err(|_| playback_input("sample size exceeds address space"))?;
        if length > self.max_sample_bytes {
            return Err(playback_input("sample exceeds configured byte limit"));
        }
        let mut bytes = vec![0; length];
        self.source
            .seek(std::io::SeekFrom::Start(sample.offset))
            .map_err(|error| PlaybackError::Io(error.to_string()))?;
        self.source
            .read_exact(&mut bytes)
            .map_err(|error| PlaybackError::Io(error.to_string()))?;
        Ok(bytes)
    }
}

fn decoder_configure<D: D3dDecode>(
    mut decoder: D,
    description: &VideoSampleDescription,
) -> Result<D, PlaybackError> {
    let config = crate::DecoderConfig::from_video_description(description)?;
    decoder.configure(&config)?;
    Ok(decoder)
}

fn decoder_configure_ref<D: D3dDecode>(
    decoder: &mut D,
    description: &VideoSampleDescription,
) -> Result<(), PlaybackError> {
    let config = crate::DecoderConfig::from_video_description(description)?;
    decoder.configure(&config)?;
    Ok(())
}

fn rescale_timestamp(value: i64, numerator: u32, denominator: u32) -> Result<i64, PlaybackError> {
    let scaled = i128::from(value) * i128::from(numerator);
    let divisor = i128::from(denominator);
    let rounded = if scaled < 0 {
        -((-scaled + divisor / 2) / divisor)
    } else {
        (scaled + divisor / 2) / divisor
    };
    i64::try_from(rounded).map_err(|_| playback_input("timestamp conversion overflowed"))
}

fn rescale_duration(value: u64, numerator: u32, denominator: u32) -> Result<u64, PlaybackError> {
    let scaled = u128::from(value) * u128::from(numerator);
    let divisor = u128::from(denominator);
    u64::try_from((scaled + divisor / 2) / divisor)
        .map_err(|_| playback_input("duration conversion overflowed"))
}

fn playback_input(message: &'static str) -> PlaybackError {
    PlaybackError::Video(VideoError::new(
        VideoErrorKind::InvalidInput,
        "Mp4VideoPlayback",
        message,
    ))
}

/// Error returned by the MP4-to-D3D playback chain.
#[derive(Debug)]
pub enum PlaybackError {
    Mp4(mp4_demux::Mp4Error),
    Video(VideoError),
    Io(String),
}

impl From<mp4_demux::Mp4Error> for PlaybackError {
    fn from(error: mp4_demux::Mp4Error) -> Self {
        Self::Mp4(error)
    }
}
impl From<VideoError> for PlaybackError {
    fn from(error: VideoError) -> Self {
        Self::Video(error)
    }
}
impl std::fmt::Display for PlaybackError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Mp4(error) => error.fmt(formatter),
            Self::Video(error) => error.fmt(formatter),
            Self::Io(error) => write!(formatter, "MP4 source I/O: {error}"),
        }
    }
}
impl std::error::Error for PlaybackError {}
