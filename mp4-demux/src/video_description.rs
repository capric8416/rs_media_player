//! Playback-oriented ISO BMFF video sample descriptions.

use crate::{
    Av1Config, AvcConfig, FourCc, HeaderParse, HevcConfig, Mp4Error, Mp4ErrorKind, Mp4Result,
    parse_box_header,
};

/// Pixel aspect information from a `pasp` box.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PixelAspectRatio {
    pub horizontal_spacing: u32,
    pub vertical_spacing: u32,
}

impl PixelAspectRatio {
    /// Parses a `pasp` payload. Zero spacing is kept as unspecified by [`Self::reduced`].
    ///
    /// # Errors
    ///
    /// Returns a truncation error when fewer than eight bytes are present.
    pub fn parse(payload: &[u8]) -> Mp4Result<Self> {
        let bytes = payload.get(..8).ok_or_else(|| {
            error(
                Mp4ErrorKind::Truncated,
                "PixelAspectRatio::parse",
                0,
                "pasp payload requires eight bytes",
            )
        })?;
        Ok(Self {
            horizontal_spacing: u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
            vertical_spacing: u32::from_be_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]),
        })
    }

    /// Returns reduced nonzero spacing, or `None` when the box does not specify a valid ratio.
    #[must_use]
    pub fn reduced(self) -> Option<(u32, u32)> {
        let (mut a, mut b) = (self.horizontal_spacing, self.vertical_spacing);
        if a == 0 || b == 0 {
            return None;
        }
        while b != 0 {
            (a, b) = (b, a % b);
        }
        Some((self.horizontal_spacing / a, self.vertical_spacing / a))
    }
}

/// A clean aperture dimension or signed center offset as a rational number.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ApertureRational {
    pub numerator: i32,
    pub denominator: u32,
}

/// Clean aperture (`clap`) values, retained separately from bitstream cropping.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CleanAperture {
    pub width: ApertureRational,
    pub height: ApertureRational,
    pub horizontal_offset: ApertureRational,
    pub vertical_offset: ApertureRational,
}

/// Integer cropping in pixels measured from the four edges of the sample entry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CleanApertureCrop {
    pub left: u32,
    pub right: u32,
    pub top: u32,
    pub bottom: u32,
}

impl CleanAperture {
    /// Parses a `clap` payload, validating positive aperture dimensions and denominators.
    ///
    /// # Errors
    ///
    /// Returns truncation or invalid-data errors for malformed rationals.
    pub fn parse(payload: &[u8]) -> Mp4Result<Self> {
        let bytes = payload.get(..32).ok_or_else(|| {
            error(
                Mp4ErrorKind::Truncated,
                "CleanAperture::parse",
                0,
                "clap payload requires 32 bytes",
            )
        })?;
        let mut values = [0_i32; 8];
        for (value, chunk) in values.iter_mut().zip(bytes.as_chunks::<4>().0) {
            *value = i32::from_be_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        }
        if values[0] <= 0
            || values[2] <= 0
            || [values[1], values[3], values[5], values[7]]
                .iter()
                .any(|value| *value <= 0)
        {
            return Err(error(
                Mp4ErrorKind::InvalidData,
                "CleanAperture::parse",
                0,
                "aperture sizes and all denominators must be positive",
            ));
        }
        let rational = |index: usize| -> Mp4Result<ApertureRational> {
            Ok(ApertureRational {
                numerator: values[index],
                denominator: u32::try_from(values[index + 1]).map_err(|_| {
                    error(
                        Mp4ErrorKind::InvalidData,
                        "CleanAperture::parse",
                        index * 4,
                        "invalid denominator",
                    )
                })?,
            })
        };
        Ok(Self {
            width: rational(0)?,
            height: rational(2)?,
            horizontal_offset: rational(4)?,
            vertical_offset: rational(6)?,
        })
    }

    /// Computes integer crop edges using the sample dimensions and `FFmpeg`'s pixel-center rule.
    ///
    /// # Errors
    ///
    /// Returns invalid-data if the aperture is outside the frame or has invalid rational fields.
    pub fn crop(self, width: u32, height: u32) -> Mp4Result<CleanApertureCrop> {
        let (left, right) = aperture_axis(width, self.width, self.horizontal_offset)?;
        let (top, bottom) = aperture_axis(height, self.height, self.vertical_offset)?;
        Ok(CleanApertureCrop {
            left,
            right,
            top,
            bottom,
        })
    }
}

fn aperture_axis(
    size: u32,
    aperture: ApertureRational,
    offset: ApertureRational,
) -> Mp4Result<(u32, u32)> {
    let invalid = || {
        error(
            Mp4ErrorKind::InvalidData,
            "CleanAperture::crop",
            0,
            "clean aperture lies outside the sample dimensions",
        )
    };
    if size == 0 || aperture.numerator <= 0 || aperture.denominator == 0 || offset.denominator == 0
    {
        return Err(invalid());
    }
    let size = i128::from(size);
    let num = i128::from(aperture.numerator);
    let den = i128::from(aperture.denominator);
    let off_num = i128::from(offset.numerator);
    let off_den = i128::from(offset.denominator);
    if num > size * den {
        return Err(invalid());
    }
    let denominator = 2 * den * off_den;
    let left = (size * den - num) * off_den + 2 * off_num * den;
    let right = ((size - 2) * den + num) * off_den + 2 * off_num * den;
    if left < 0 || right < left || right > (size - 1) * denominator {
        return Err(invalid());
    }
    let left = left / denominator;
    let right_crop = size - 1 - right / denominator;
    if left + right_crop >= size {
        return Err(invalid());
    }
    Ok((
        u32::try_from(left).map_err(|_| invalid())?,
        u32::try_from(right_crop).map_err(|_| invalid())?,
    ))
}

/// Decoder configuration associated with one video sample entry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum VideoCodecConfig {
    Avc(AvcConfig),
    Hevc(HevcConfig),
    Av1(Av1Config),
}

/// One parsed AVC, HEVC, or AV1 `VisualSampleEntry` from `stsd`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VideoSampleDescription {
    pub sample_entry: FourCc,
    pub data_reference_index: u16,
    pub width: u16,
    pub height: u16,
    pub pixel_aspect_ratio: Option<PixelAspectRatio>,
    pub clean_aperture: Option<CleanAperture>,
    pub codec: VideoCodecConfig,
    /// Original `avcC`, `hvcC`, or `av1C` payload for decoder initialization.
    pub codec_private_data: Vec<u8>,
}

impl VideoSampleDescription {
    /// Parses a complete `stsd` payload containing video sample entries.
    ///
    /// Entries and their child boxes are bounded by the supplied payload. Unknown child boxes are
    /// skipped; unsupported entry types and duplicate decoder configuration boxes are errors.
    /// `pasp` overrides earlier `pasp` boxes in the same entry, matching `FFmpeg` when its vertical
    /// spacing is nonzero. `clap` is validated against sample dimensions. Encrypted entries and
    /// sample-group overrides are not handled.
    ///
    /// # Errors
    ///
    /// Returns typed errors for malformed/truncated entries, unsupported formats, and allocation.
    pub fn parse_stsd(payload: &[u8]) -> Mp4Result<Vec<Self>> {
        let header = payload.get(..8).ok_or_else(|| {
            error(
                Mp4ErrorKind::Truncated,
                "VideoSampleDescription::parse_stsd",
                0,
                "stsd header is truncated",
            )
        })?;
        if header[..4] != [0; 4] {
            return Err(error(
                Mp4ErrorKind::Unsupported,
                "VideoSampleDescription::parse_stsd",
                0,
                "unsupported stsd version or flags",
            ));
        }
        let count = u32::from_be_bytes([header[4], header[5], header[6], header[7]]);
        let count = usize::try_from(count).map_err(|_| {
            error(
                Mp4ErrorKind::Overflow,
                "VideoSampleDescription::parse_stsd",
                4,
                "entry count exceeds addressable size",
            )
        })?;
        if count > (payload.len() - 8) / 8 {
            return Err(error(
                Mp4ErrorKind::Truncated,
                "VideoSampleDescription::parse_stsd",
                4,
                "entry count exceeds available data",
            ));
        }
        let mut entries = Vec::new();
        entries.try_reserve_exact(count).map_err(|_| {
            error(
                Mp4ErrorKind::BufferLimit,
                "VideoSampleDescription::parse_stsd",
                4,
                "cannot allocate sample descriptions",
            )
        })?;
        let mut offset = 8;
        for _ in 0..count {
            let (kind, data, end) = child(payload, offset)?;
            entries.push(parse_visual_entry(kind, data, offset)?);
            offset = end;
        }
        if offset != payload.len() {
            return Err(error(
                Mp4ErrorKind::InvalidData,
                "VideoSampleDescription::parse_stsd",
                offset,
                "unexpected trailing bytes after sample entries",
            ));
        }
        Ok(entries)
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "visual sample-entry validation and child-box precedence form one bounded parse"
)]
fn parse_visual_entry(
    kind: FourCc,
    data: &[u8],
    offset: usize,
) -> Mp4Result<VideoSampleDescription> {
    let expected = match &kind.as_bytes() {
        b"avc1" | b"avc3" => *b"avcC",
        b"hvc1" | b"hev1" => *b"hvcC",
        b"av01" => *b"av1C",
        _ => {
            return Err(error(
                Mp4ErrorKind::Unsupported,
                "parse_visual_entry",
                offset,
                "unsupported video sample entry",
            )
            .with_box_type(kind));
        }
    };
    let fixed = data.get(..78).ok_or_else(|| {
        error(
            Mp4ErrorKind::Truncated,
            "parse_visual_entry",
            offset,
            "visual sample entry is truncated",
        )
        .with_box_type(kind)
    })?;
    let data_reference_index = u16::from_be_bytes([fixed[6], fixed[7]]);
    let width = u16::from_be_bytes([fixed[24], fixed[25]]);
    let height = u16::from_be_bytes([fixed[26], fixed[27]]);
    if data_reference_index == 0 || width == 0 || height == 0 {
        return Err(error(
            Mp4ErrorKind::InvalidData,
            "parse_visual_entry",
            offset,
            "data reference and dimensions must be nonzero",
        )
        .with_box_type(kind));
    }
    let mut position = 78;
    let mut codec = None;
    let mut pixel_aspect_ratio = None;
    let mut clean_aperture = None;
    let mut codec_private_data = None;
    while position < data.len() {
        let (child_kind, payload, end) = child(data, position)?;
        if child_kind.as_bytes() == expected {
            if codec.is_some() {
                return Err(error(
                    Mp4ErrorKind::InvalidData,
                    "parse_visual_entry",
                    offset + position,
                    "duplicate codec configuration",
                )
                .with_box_type(child_kind));
            }
            codec = Some(match &expected {
                b"avcC" => VideoCodecConfig::Avc(AvcConfig::parse(payload)?),
                b"hvcC" => VideoCodecConfig::Hevc(HevcConfig::parse(payload)?),
                _ => VideoCodecConfig::Av1(Av1Config::parse(payload)?),
            });
            codec_private_data = Some(payload.to_vec());
        } else if child_kind.as_bytes() == *b"pasp" {
            let candidate = PixelAspectRatio::parse(payload)?;
            if candidate.vertical_spacing != 0 {
                pixel_aspect_ratio = Some(candidate);
            }
        } else if child_kind.as_bytes() == *b"clap" {
            if clean_aperture.is_some() {
                return Err(error(
                    Mp4ErrorKind::InvalidData,
                    "parse_visual_entry",
                    offset + position,
                    "duplicate clean aperture",
                )
                .with_box_type(child_kind));
            }
            let candidate = CleanAperture::parse(payload)?;
            candidate.crop(u32::from(width), u32::from(height))?;
            clean_aperture = Some(candidate);
        }
        position = end;
    }
    let codec = codec.ok_or_else(|| {
        error(
            Mp4ErrorKind::InvalidData,
            "parse_visual_entry",
            offset,
            "missing codec configuration",
        )
        .with_box_type(kind)
    })?;
    Ok(VideoSampleDescription {
        sample_entry: kind,
        data_reference_index,
        width,
        height,
        pixel_aspect_ratio,
        clean_aperture,
        codec,
        codec_private_data: codec_private_data.ok_or_else(|| {
            error(
                Mp4ErrorKind::InvalidData,
                "parse_visual_entry",
                offset,
                "missing codec-private payload",
            )
        })?,
    })
}

pub(crate) fn child(data: &[u8], offset: usize) -> Mp4Result<(FourCc, &[u8], usize)> {
    let tail = data.get(offset..).ok_or_else(|| {
        error(
            Mp4ErrorKind::Bounds,
            "video sample box",
            offset,
            "offset exceeds parent",
        )
    })?;
    let parent_end = u64::try_from(data.len()).map_err(|_| {
        error(
            Mp4ErrorKind::Overflow,
            "video sample box",
            offset,
            "parent length overflow",
        )
    })?;
    let start = u64::try_from(offset).map_err(|_| {
        error(
            Mp4ErrorKind::Overflow,
            "video sample box",
            offset,
            "offset overflow",
        )
    })?;
    let header = match parse_box_header(tail, start, Some(parent_end))? {
        HeaderParse::Complete(header) => header,
        HeaderParse::NeedMoreData { .. } | HeaderParse::NeedsContainerEnd => {
            return Err(error(
                Mp4ErrorKind::Truncated,
                "video sample box",
                offset,
                "child header is truncated",
            ));
        }
    };
    if header.was_clamped() {
        return Err(error(
            Mp4ErrorKind::Truncated,
            "video sample box",
            offset,
            "child box exceeds its parent",
        )
        .with_box_type(header.box_type()));
    }
    let begin = usize::try_from(header.payload_offset()).map_err(|_| {
        error(
            Mp4ErrorKind::Overflow,
            "video sample box",
            offset,
            "payload offset overflow",
        )
    })?;
    let end = usize::try_from(header.end()).map_err(|_| {
        error(
            Mp4ErrorKind::Overflow,
            "video sample box",
            offset,
            "box end overflow",
        )
    })?;
    Ok((header.box_type(), &data[begin..end], end))
}

fn error(
    kind: Mp4ErrorKind,
    operation: &'static str,
    offset: usize,
    detail: &'static str,
) -> Mp4Error {
    Mp4Error::new(
        kind,
        operation,
        u64::try_from(offset).unwrap_or(u64::MAX),
        detail,
    )
}
