use std::num::NonZeroU32;

use crate::{FrameSize, VideoError, VideoErrorKind, VideoResult};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Codec {
    Avc,
    Hevc,
    Av1,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum CodecProfile {
    AvcBaseline,
    AvcMain,
    AvcHigh,
    HevcMain,
    HevcMain10,
    Av1Main,
}

impl CodecProfile {
    #[must_use]
    pub const fn codec(self) -> Codec {
        match self {
            Self::AvcBaseline | Self::AvcMain | Self::AvcHigh => Codec::Avc,
            Self::HevcMain | Self::HevcMain10 => Codec::Hevc,
            Self::Av1Main => Codec::Av1,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum BitDepth {
    Eight,
    Ten,
}

impl BitDepth {
    #[must_use]
    pub const fn bits(self) -> u8 {
        match self {
            Self::Eight => 8,
            Self::Ten => 10,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ChromaSampling {
    Yuv420,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum BitstreamFormat {
    AnnexB,
    LengthPrefixed { length_size: u8 },
    Av1Obu,
    Av1Isobmff,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TimeBase {
    numerator: NonZeroU32,
    denominator: NonZeroU32,
}

impl TimeBase {
    /// Creates a rational time base.
    ///
    /// # Errors
    ///
    /// Returns [`VideoErrorKind::InvalidInput`] when either component is zero.
    pub fn new(numerator: u32, denominator: u32) -> VideoResult<Self> {
        let numerator = NonZeroU32::new(numerator).ok_or_else(|| {
            VideoError::new(
                VideoErrorKind::InvalidInput,
                "TimeBase::new",
                "numerator must be non-zero",
            )
        })?;
        let denominator = NonZeroU32::new(denominator).ok_or_else(|| {
            VideoError::new(
                VideoErrorKind::InvalidInput,
                "TimeBase::new",
                "denominator must be non-zero",
            )
        })?;
        Ok(Self {
            numerator,
            denominator,
        })
    }

    #[must_use]
    pub const fn numerator(self) -> u32 {
        self.numerator.get()
    }

    #[must_use]
    pub const fn denominator(self) -> u32 {
        self.denominator.get()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SampleTiming {
    pub dts: Option<i64>,
    pub pts: Option<i64>,
    pub duration: Option<u64>,
    pub time_base: TimeBase,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DecoderConfig {
    pub profile: CodecProfile,
    pub bit_depth: BitDepth,
    pub chroma_sampling: ChromaSampling,
    pub coded_size: FrameSize,
    pub bitstream_format: BitstreamFormat,
    pub codec_private: Vec<u8>,
}

/// Parameter sets extracted from the codec-private record used by MP4 sample entries.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CodecPrivateData {
    Avc {
        length_size: u8,
        sps: Vec<u8>,
        pps: Vec<u8>,
    },
    Hevc {
        length_size: u8,
        vps: Vec<u8>,
        sps: Vec<u8>,
        pps: Vec<u8>,
    },
    Av1 {
        sequence_header_obu: Vec<u8>,
    },
}

/// Stateful parser/DPB object selected from a decoder configuration's private record.
pub enum CodecDecodeState {
    Avc(crate::AvcDecodeState),
    Hevc(crate::HevcDecodeState),
    Av1(crate::Av1DecodeState),
}

impl CodecDecodeState {
    /// Returns the codec represented by this state.
    #[must_use]
    pub const fn codec(&self) -> Codec {
        match self {
            Self::Avc(_) => Codec::Avc,
            Self::Hevc(_) => Codec::Hevc,
            Self::Av1(_) => Codec::Av1,
        }
    }

    /// Clears reference state after a seek or decoder flush.
    pub fn reset(&mut self) {
        match self {
            Self::Avc(state) => state.reset(),
            Self::Hevc(state) => state.reset(),
            Self::Av1(state) => state.reset(),
        }
    }
}

impl DecoderConfig {
    /// Derives a hardware decoder configuration from an MP4 video sample description.
    ///
    /// # Errors
    ///
    /// Returns unsupported or invalid-input when the sample entry uses a codec profile,
    /// chroma mode, bit depth, or parameter set outside this crate's supported subset.
    #[expect(
        clippy::too_many_lines,
        reason = "one codec-tag dispatch must derive a coherent profile, framing, and coded size"
    )]
    pub fn from_video_description(
        description: &mp4_demux::VideoSampleDescription,
    ) -> VideoResult<Self> {
        use mp4_demux::VideoCodecConfig;

        let (profile, bit_depth, size, bitstream_format) = match &description.codec {
            VideoCodecConfig::Avc(record) => {
                let profile = match record.profile {
                    66 => CodecProfile::AvcBaseline,
                    77 => CodecProfile::AvcMain,
                    100 => CodecProfile::AvcHigh,
                    _ => return Err(codec_private_unsupported("unsupported AVC profile")),
                };
                let sps = record
                    .sequence_parameter_sets
                    .first()
                    .ok_or_else(|| codec_private_unsupported("avcC contains no SPS"))?;
                let pps = record
                    .picture_parameter_sets
                    .first()
                    .ok_or_else(|| codec_private_unsupported("avcC contains no PPS"))?;
                let state = crate::AvcDecodeState::new(sps, pps)?;
                (
                    profile,
                    BitDepth::Eight,
                    state.coded_size()?,
                    BitstreamFormat::LengthPrefixed {
                        length_size: record.length_size,
                    },
                )
            }
            VideoCodecConfig::Hevc(record) => {
                let (profile, bit_depth) = match (
                    record.profile_idc,
                    record.bit_depth_luma,
                    record.bit_depth_chroma,
                ) {
                    (1, 8, 8) => (CodecProfile::HevcMain, BitDepth::Eight),
                    (2, 10, 10) => (CodecProfile::HevcMain10, BitDepth::Ten),
                    _ => {
                        return Err(codec_private_unsupported(
                            "unsupported HEVC profile or bit depth",
                        ));
                    }
                };
                let mut vps = None;
                let mut sps = None;
                let mut pps = None;
                for array in &record.arrays {
                    let target = match array.nal_unit_type {
                        32 => &mut vps,
                        33 => &mut sps,
                        34 => &mut pps,
                        _ => continue,
                    };
                    if target.is_none() {
                        *target = array.nal_units.first();
                    }
                }
                let state = crate::HevcDecodeState::new(
                    vps.ok_or_else(|| codec_private_unsupported("hvcC contains no VPS"))?,
                    sps.ok_or_else(|| codec_private_unsupported("hvcC contains no SPS"))?,
                    pps.ok_or_else(|| codec_private_unsupported("hvcC contains no PPS"))?,
                )?;
                (
                    profile,
                    bit_depth,
                    state.coded_size()?,
                    BitstreamFormat::LengthPrefixed {
                        length_size: record.length_size,
                    },
                )
            }
            VideoCodecConfig::Av1(record) => {
                if record.sequence_profile != 0
                    || record.monochrome
                    || !record.chroma_subsampling_x
                    || !record.chroma_subsampling_y
                {
                    return Err(codec_private_unsupported(
                        "only AV1 Main 4:2:0 is supported",
                    ));
                }
                let mut sequence_header = None;
                for obu in crate::obu::ObuIter::new(&record.config_obus) {
                    let obu = obu?;
                    if obu.kind == crate::obu::ObuType::SEQUENCE_HEADER {
                        sequence_header = Some(obu.raw.to_vec());
                        break;
                    }
                }
                let sequence_header = sequence_header
                    .ok_or_else(|| codec_private_unsupported("av1C contains no sequence header"))?;
                let size = crate::av1::sequence_header_frame_size(&sequence_header)?;
                (
                    CodecProfile::Av1Main,
                    if record.high_bitdepth {
                        BitDepth::Ten
                    } else {
                        BitDepth::Eight
                    },
                    size,
                    BitstreamFormat::Av1Isobmff,
                )
            }
        };
        let config = Self {
            profile,
            bit_depth,
            chroma_sampling: ChromaSampling::Yuv420,
            coded_size: size,
            bitstream_format,
            codec_private: description.codec_private_data.clone(),
        };
        config.validate()?;
        Ok(config)
    }

    /// Builds the codec's parameter-set and reference state from its MP4 codec-private record.
    ///
    /// # Errors
    ///
    /// Returns invalid-input, unsupported syntax, or allocation errors for malformed or
    /// unsupported parameter sets.
    pub fn create_codec_state(&self) -> VideoResult<CodecDecodeState> {
        match self.parse_codec_private()? {
            CodecPrivateData::Avc { sps, pps, .. } => Ok(CodecDecodeState::Avc(
                crate::AvcDecodeState::new(&sps, &pps)?,
            )),
            CodecPrivateData::Hevc { vps, sps, pps, .. } => Ok(CodecDecodeState::Hevc(
                crate::HevcDecodeState::new(&vps, &sps, &pps)?,
            )),
            CodecPrivateData::Av1 { .. } => {
                Ok(CodecDecodeState::Av1(crate::Av1DecodeState::default()))
            }
        }
    }

    /// Parses MP4 `avcC`, `hvcC`, or `av1C` initialization data for this configuration.
    ///
    /// # Errors
    ///
    /// Returns invalid-input for a malformed record or unsupported when required parameter sets
    /// are absent or do not match the selected codec profile.
    pub fn parse_codec_private(&self) -> VideoResult<CodecPrivateData> {
        self.validate()?;
        match self.profile.codec() {
            Codec::Avc => {
                let record = mp4_demux::AvcConfig::parse(&self.codec_private)
                    .map_err(|error| codec_private_error("avcC", &error))?;
                parse_avc_codec_private(record, self.bitstream_format, self.profile)
            }
            Codec::Hevc => {
                let record = mp4_demux::HevcConfig::parse(&self.codec_private)
                    .map_err(|error| codec_private_error("hvcC", &error))?;
                parse_hevc_codec_private(
                    record,
                    self.bitstream_format,
                    self.bit_depth,
                    self.profile,
                )
            }
            Codec::Av1 => {
                let record = mp4_demux::Av1Config::parse(&self.codec_private)
                    .map_err(|error| codec_private_error("av1C", &error))?;
                if record.high_bitdepth != (self.bit_depth == BitDepth::Ten) || record.monochrome {
                    return Err(codec_private_unsupported(
                        "av1C bit depth or chroma sampling does not match the decoder configuration",
                    ));
                }
                let sequence_header_obu = crate::obu::ObuIter::new(&record.config_obus)
                    .find_map(|obu| match obu {
                        Ok(obu) if obu.kind == crate::obu::ObuType::SEQUENCE_HEADER => {
                            Some(Ok(obu.raw.to_vec()))
                        }
                        Ok(_) => None,
                        Err(error) => Some(Err(error)),
                    })
                    .transpose()?
                    .ok_or_else(|| {
                        codec_private_unsupported("av1C contains no sequence header OBU")
                    })?;
                Ok(CodecPrivateData::Av1 {
                    sequence_header_obu,
                })
            }
        }
    }

    /// Validates codec/profile/bit-depth and framing combinations in the public contract.
    ///
    /// # Errors
    ///
    /// Returns [`VideoErrorKind::InvalidInput`] for an unsupported or inconsistent combination.
    pub fn validate(&self) -> VideoResult<()> {
        match self.bitstream_format {
            BitstreamFormat::LengthPrefixed { length_size } if !(1..=4).contains(&length_size) => {
                return Err(VideoError::new(
                    VideoErrorKind::InvalidInput,
                    "DecoderConfig::validate",
                    "NAL length field must contain one through four bytes",
                ));
            }
            BitstreamFormat::AnnexB | BitstreamFormat::LengthPrefixed { .. }
                if self.profile.codec() == Codec::Av1 =>
            {
                return Err(VideoError::new(
                    VideoErrorKind::InvalidInput,
                    "DecoderConfig::validate",
                    "AV1 does not use AVC/HEVC NAL framing",
                ));
            }
            BitstreamFormat::Av1Obu | BitstreamFormat::Av1Isobmff
                if self.profile.codec() != Codec::Av1 =>
            {
                return Err(VideoError::new(
                    VideoErrorKind::InvalidInput,
                    "DecoderConfig::validate",
                    "AV1 OBU framing requires the AV1 profile",
                ));
            }
            _ => {}
        }

        if matches!(
            self.profile,
            CodecProfile::AvcBaseline | CodecProfile::AvcMain | CodecProfile::AvcHigh
        ) && self.bit_depth != BitDepth::Eight
        {
            return Err(VideoError::new(
                VideoErrorKind::InvalidInput,
                "DecoderConfig::validate",
                "first-release AVC profiles require eight-bit samples",
            ));
        }
        if self.profile == CodecProfile::HevcMain10 && self.bit_depth != BitDepth::Ten {
            return Err(VideoError::new(
                VideoErrorKind::InvalidInput,
                "DecoderConfig::validate",
                "HEVC Main10 requires ten-bit samples",
            ));
        }
        if self.profile == CodecProfile::HevcMain && self.bit_depth != BitDepth::Eight {
            return Err(VideoError::new(
                VideoErrorKind::InvalidInput,
                "DecoderConfig::validate",
                "HEVC Main requires eight-bit samples",
            ));
        }
        Ok(())
    }
}

fn parse_avc_codec_private(
    record: mp4_demux::AvcConfig,
    bitstream_format: BitstreamFormat,
    profile: CodecProfile,
) -> VideoResult<CodecPrivateData> {
    validate_nal_length_size(bitstream_format, record.length_size, "avcC")?;
    let mut parameter_sets = crate::avc::parameter_sets::ParameterSets::default();
    let mut sps_by_id = Vec::new();
    for sps in record.sequence_parameter_sets {
        let id = parameter_sets.insert_sps_nal(&sps)?.id;
        sps_by_id.push((id, sps));
    }
    let mut pps_by_id = Vec::new();
    for pps in record.picture_parameter_sets {
        let id = parameter_sets.insert_pps_nal(&pps)?.id;
        pps_by_id.push((id, pps));
    }
    for (pps_id, pps) in pps_by_id {
        let (_, sequence) = parameter_sets.resolve_pps(u32::from(pps_id))?;
        if avc_profile_id(profile) == Some(sequence.profile_idc)
            && let Some((_, sps)) = sps_by_id.iter().rev().find(|(id, _)| *id == sequence.id)
        {
            return Ok(CodecPrivateData::Avc {
                length_size: record.length_size,
                sps: sps.clone(),
                pps,
            });
        }
    }
    Err(codec_private_unsupported(
        "avcC contains no matching SPS and PPS",
    ))
}

fn parse_hevc_codec_private(
    record: mp4_demux::HevcConfig,
    bitstream_format: BitstreamFormat,
    bit_depth: BitDepth,
    profile: CodecProfile,
) -> VideoResult<CodecPrivateData> {
    validate_nal_length_size(bitstream_format, record.length_size, "hvcC")?;
    if record.bit_depth_luma != bit_depth.bits() || record.bit_depth_chroma != bit_depth.bits() {
        return Err(codec_private_unsupported(
            "hvcC bit depth does not match the decoder configuration",
        ));
    }
    let mut vps_nals = Vec::new();
    let mut sps_nals = Vec::new();
    let mut pps_nals = Vec::new();
    for array in record.arrays {
        match array.nal_unit_type {
            32 => vps_nals.extend(array.nal_units),
            33 => sps_nals.extend(array.nal_units),
            34 => pps_nals.extend(array.nal_units),
            _ => {}
        }
    }
    let mut parameter_sets = crate::hevc::parameter_sets::ParameterSets::default();
    let mut vps_by_id = Vec::new();
    for vps in vps_nals {
        let id = parameter_sets.insert_vps_nal(&vps)?.id;
        vps_by_id.push((id, vps));
    }
    let mut sps_by_id = Vec::new();
    for sps in sps_nals {
        let id = parameter_sets.insert_sps_nal(&sps)?.id;
        sps_by_id.push((id, sps));
    }
    let mut pps_by_id = Vec::new();
    for pps in pps_nals {
        let id = parameter_sets.insert_pps_nal(&pps)?.id;
        pps_by_id.push((id, pps));
    }
    for (pps_id, pps_nal) in pps_by_id {
        let (_, sps) = parameter_sets.resolve_pps(u32::from(pps_id))?;
        let vps_id = sps.vps_id;
        let sps_id = sps.id;
        if hevc_profile_id(profile) == Some(sps.profile.profile_idc)
            && let (Some((_, vps)), Some((_, sps_nal))) = (
                vps_by_id.iter().rev().find(|(id, _)| *id == vps_id),
                sps_by_id.iter().rev().find(|(id, _)| *id == sps_id),
            )
        {
            return Ok(CodecPrivateData::Hevc {
                length_size: record.length_size,
                vps: vps.clone(),
                sps: sps_nal.clone(),
                pps: pps_nal,
            });
        }
    }
    Err(codec_private_unsupported(
        "hvcC contains no matching VPS, SPS, and PPS",
    ))
}

const fn avc_profile_id(profile: CodecProfile) -> Option<u8> {
    match profile {
        CodecProfile::AvcBaseline => Some(66),
        CodecProfile::AvcMain => Some(77),
        CodecProfile::AvcHigh => Some(100),
        _ => None,
    }
}

const fn hevc_profile_id(profile: CodecProfile) -> Option<u8> {
    match profile {
        CodecProfile::HevcMain => Some(1),
        CodecProfile::HevcMain10 => Some(2),
        _ => None,
    }
}

fn validate_nal_length_size(
    format: BitstreamFormat,
    record_length_size: u8,
    record_name: &'static str,
) -> VideoResult<()> {
    if let BitstreamFormat::LengthPrefixed { length_size } = format
        && length_size != record_length_size
    {
        return Err(VideoError::new(
            VideoErrorKind::InvalidInput,
            "DecoderConfig::parse_codec_private",
            format!("{record_name} NAL length size does not match the decoder configuration"),
        ));
    }
    Ok(())
}

fn codec_private_error(record: &'static str, error: &mp4_demux::Mp4Error) -> VideoError {
    VideoError::new(
        VideoErrorKind::InvalidInput,
        "DecoderConfig::parse_codec_private",
        format!("invalid {record}: {error}"),
    )
}

fn codec_private_unsupported(detail: &'static str) -> VideoError {
    VideoError::new(
        VideoErrorKind::Unsupported,
        "DecoderConfig::parse_codec_private",
        detail,
    )
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EncodedSample<'data> {
    pub data: &'data [u8],
    pub timing: SampleTiming,
    pub is_random_access: bool,
}

impl EncodedSample<'_> {
    /// Validates fields required before a sample can be submitted.
    ///
    /// # Errors
    ///
    /// Returns [`VideoErrorKind::InvalidInput`] when the encoded payload is empty.
    pub fn validate(self) -> VideoResult<()> {
        if self.data.is_empty() {
            return Err(VideoError::new(
                VideoErrorKind::InvalidInput,
                "EncodedSample::validate",
                "encoded sample must not be empty",
            ));
        }
        Ok(())
    }
}
