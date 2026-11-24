use super::dpb::{LongTermReference, SliceType};
use super::parameter_sets::{ParameterSets, SequenceParameterSet};
use super::rps::ShortTermRps;
use super::{NalHeader, hevc_error};
use crate::VideoResult;
use crate::bit_reader::BitReader;
use crate::rbsp::extract_rbsp;

#[derive(Clone, Debug, Eq, PartialEq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "fields mirror independent HEVC slice-header syntax flags"
)]
pub(crate) struct ReferenceSliceHeader {
    pub(crate) nal: NalHeader,
    pub(crate) first_slice_in_picture: bool,
    pub(crate) dependent_segment: bool,
    pub(crate) slice_segment_address: u32,
    pub(crate) no_output_of_prior_pictures: bool,
    pub(crate) pps_id: u8,
    pub(crate) slice_type: SliceType,
    pub(crate) picture_output: bool,
    pub(crate) poc_lsb: u32,
    pub(crate) short_term_rps_from_sps: bool,
    pub(crate) short_term_rps_bit_size: usize,
    pub(crate) short_term_rps: ShortTermRps,
    pub(crate) long_term_rps: Vec<LongTermReference>,
    pub(crate) temporal_mvp_enabled: bool,
    pub(crate) sao_enabled: [bool; 2],
    pub(crate) active_references: [u8; 2],
    pub(crate) list_modifications: [Vec<u8>; 2],
    pub(crate) mvd_l1_zero: bool,
    pub(crate) cabac_init: bool,
    pub(crate) reference_header_bit_size: usize,
}

impl ReferenceSliceHeader {
    pub(crate) fn parse_nal(
        nal: &[u8],
        sets: &ParameterSets,
        current_poc: i32,
    ) -> VideoResult<Self> {
        Self::parse_nal_with_previous(nal, sets, current_poc, None)
    }

    #[expect(
        clippy::too_many_lines,
        reason = "ordered hardware-relevant HEVC slice grammar mirrors FFmpeg"
    )]
    pub(crate) fn parse_nal_with_previous(
        nal: &[u8],
        sets: &ParameterSets,
        current_poc: i32,
        previous_independent: Option<&Self>,
    ) -> VideoResult<Self> {
        if nal.len() < 2 {
            return Err(hevc_error(
                "ReferenceSliceHeader::parse_nal",
                "NAL is truncated",
            ));
        }
        let header = NalHeader::parse([nal[0], nal[1]])?;
        if header.layer_id != 0 || header.unit_type > 31 {
            return Err(hevc_error(
                "ReferenceSliceHeader::parse_nal",
                "unsupported slice layer or type",
            ));
        }
        let rbsp = extract_rbsp(&nal[2..]);
        let mut bits = BitReader::new(rbsp.data.as_ref());
        let first_slice_in_picture = bits.read_bit()?;
        let no_output_of_prior_pictures = header.is_irap() && bits.read_bit()?;
        let pps_value = bits.read_ue()?;
        let (pps, sps) = sets.resolve_pps(pps_value)?;
        let pps_id = u8::try_from(pps_value)
            .map_err(|_| hevc_error("ReferenceSliceHeader::parse_nal", "PPS id exceeds u8"))?;
        let mut slice_segment_address = 0;
        let mut dependent_segment = false;
        if !first_slice_in_picture {
            if pps.dependent_slice_segments_enabled {
                dependent_segment = bits.read_bit()?;
            }
            let width = ceil_log2(sps.ctb_count() as usize);
            slice_segment_address = u32::try_from(bits.read_bits(width)?).map_err(|_| {
                hevc_error(
                    "ReferenceSliceHeader::parse_nal",
                    "slice segment address exceeds u32",
                )
            })?;
            if slice_segment_address >= sps.ctb_count() {
                return Err(hevc_error(
                    "ReferenceSliceHeader::parse_nal",
                    "slice segment address exceeds picture CTB count",
                ));
            }
        }
        if dependent_segment {
            let previous = previous_independent.ok_or_else(|| {
                super::unsupported(
                    "ReferenceSliceHeader::parse_nal",
                    "dependent HEVC segment has no preceding independent segment",
                )
            })?;
            if previous.pps_id != pps_id
                || previous.nal.unit_type != header.unit_type
                || previous.nal.layer_id != header.layer_id
                || previous.nal.temporal_id != header.temporal_id
            {
                return Err(super::unsupported(
                    "ReferenceSliceHeader::parse_nal",
                    "dependent HEVC segment does not match its preceding picture header",
                ));
            }
            let mut inherited = previous.clone();
            inherited.nal = header;
            inherited.first_slice_in_picture = false;
            inherited.dependent_segment = true;
            inherited.slice_segment_address = slice_segment_address;
            inherited.reference_header_bit_size = bits.position();
            return Ok(inherited);
        }
        bits.skip_bits(usize::from(pps.extra_slice_header_bits))?;
        let slice_type = match bits.read_ue()? {
            0 => SliceType::B,
            1 => SliceType::P,
            2 => SliceType::I,
            _ => {
                return Err(hevc_error(
                    "ReferenceSliceHeader::parse_nal",
                    "invalid slice type",
                ));
            }
        };
        if header.is_irap() && slice_type != SliceType::I {
            return Err(hevc_error(
                "ReferenceSliceHeader::parse_nal",
                "IRAP slice is not intra",
            ));
        }
        let picture_output = !pps.output_flag_present || bits.read_bit()?;
        let (
            poc_lsb,
            short_term_rps,
            short_term_rps_from_sps,
            short_term_rps_bit_size,
            long_term_rps,
            temporal_mvp_enabled,
        ) = if header.is_idr() {
            (0, ShortTermRps::default(), true, 0, Vec::new(), false)
        } else {
            let poc_lsb = u32::try_from(bits.read_bits(sps.log2_max_poc_lsb)?).map_err(|_| {
                hevc_error("ReferenceSliceHeader::parse_nal", "POC LSB exceeds u32")
            })?;
            let from_sps = bits.read_bit()?;
            let short_term_rps_start = bits.position();
            let short = if from_sps {
                let count = sps.short_term_rps.len();
                if count == 0 {
                    return Err(hevc_error(
                        "ReferenceSliceHeader::parse_nal",
                        "SPS has no short-term RPS",
                    ));
                }
                let width = ceil_log2(count);
                let index = if width == 0 {
                    0
                } else {
                    usize::try_from(bits.read_bits(width)?).map_err(|_| {
                        hevc_error("ReferenceSliceHeader::parse_nal", "RPS index exceeds usize")
                    })?
                };
                sps.short_term_rps.get(index).cloned().ok_or_else(|| {
                    hevc_error("ReferenceSliceHeader::parse_nal", "RPS index is invalid")
                })?
            } else {
                ShortTermRps::decode(&mut bits, &sps.short_term_rps, true)?
            };
            let short_term_rps_bit_size = bits.position() - short_term_rps_start;
            let long = parse_long_term_rps(&mut bits, sps, current_poc, poc_lsb)?;
            let temporal = sps.temporal_mvp_enabled && bits.read_bit()?;
            (
                poc_lsb,
                short,
                from_sps,
                short_term_rps_bit_size,
                long,
                temporal,
            )
        };
        let sao_luma = sps.sao_enabled && bits.read_bit()?;
        let sao_chroma = sps.sao_enabled && sps.chroma_format_idc != 0 && bits.read_bit()?;
        let mut active_references = [0, 0];
        let mut list_modifications = [Vec::new(), Vec::new()];
        let mut mvd_l1_zero = false;
        let mut cabac_init = false;
        if slice_type != SliceType::I {
            active_references = pps.default_ref_count;
            if slice_type == SliceType::P {
                active_references[1] = 0;
            }
            if bits.read_bit()? {
                active_references[0] = plus_one_ref(bits.read_ue()?)?;
                if slice_type == SliceType::B {
                    active_references[1] = plus_one_ref(bits.read_ue()?)?;
                }
            }
            let candidate_count = short_term_rps
                .references
                .iter()
                .filter(|entry| entry.used)
                .count()
                + long_term_rps.iter().filter(|entry| entry.used).count();
            if candidate_count == 0 {
                return Err(hevc_error(
                    "ReferenceSliceHeader::parse_nal",
                    "inter slice has zero references",
                ));
            }
            if pps.lists_modification_present && candidate_count > 1 {
                let width = ceil_log2(candidate_count);
                for list in 0..if slice_type == SliceType::B { 2 } else { 1 } {
                    if bits.read_bit()? {
                        for _ in 0..active_references[list] {
                            let entry = u8::try_from(bits.read_bits(width)?).map_err(|_| {
                                hevc_error(
                                    "ReferenceSliceHeader::parse_nal",
                                    "list entry exceeds u8",
                                )
                            })?;
                            if usize::from(entry) >= candidate_count {
                                return Err(hevc_error(
                                    "ReferenceSliceHeader::parse_nal",
                                    "list entry is invalid",
                                ));
                            }
                            list_modifications[list].push(entry);
                        }
                    }
                }
            }
            if slice_type == SliceType::B {
                mvd_l1_zero = bits.read_bit()?;
            }
            if pps.cabac_init_present {
                cabac_init = bits.read_bit()?;
            }
        }
        Ok(Self {
            nal: header,
            first_slice_in_picture,
            dependent_segment: false,
            slice_segment_address,
            no_output_of_prior_pictures,
            pps_id,
            slice_type,
            picture_output,
            poc_lsb,
            short_term_rps_from_sps,
            short_term_rps_bit_size,
            short_term_rps,
            long_term_rps,
            temporal_mvp_enabled,
            sao_enabled: [sao_luma, sao_chroma],
            active_references,
            list_modifications,
            mvd_l1_zero,
            cabac_init,
            reference_header_bit_size: bits.position(),
        })
    }
}

fn parse_long_term_rps(
    bits: &mut BitReader<'_>,
    sps: &SequenceParameterSet,
    current_poc: i32,
    current_lsb: u32,
) -> VideoResult<Vec<LongTermReference>> {
    if sps.long_term_lsb.is_empty() {
        return Ok(Vec::new());
    }
    let from_sps = bits.read_ue()?;
    let from_slice = bits.read_ue()?;
    if usize::try_from(from_sps).map_or(true, |count| count > sps.long_term_lsb.len())
        || from_sps + from_slice > 16
    {
        return Err(hevc_error(
            "parse_long_term_rps",
            "long-term reference count is invalid",
        ));
    }
    let mut output = Vec::new();
    let mut previous_delta = 0_u32;
    for index in 0..from_sps + from_slice {
        let (lsb, used) = if index < from_sps {
            let width = ceil_log2(sps.long_term_lsb.len());
            let selected = if width == 0 {
                0
            } else {
                usize::try_from(bits.read_bits(width)?)
                    .map_err(|_| hevc_error("parse_long_term_rps", "SPS LT index exceeds usize"))?
            };
            *sps.long_term_lsb
                .get(selected)
                .ok_or_else(|| hevc_error("parse_long_term_rps", "SPS LT index is invalid"))?
        } else {
            (
                u32::try_from(bits.read_bits(sps.log2_max_poc_lsb)?)
                    .map_err(|_| hevc_error("parse_long_term_rps", "LT POC exceeds u32"))?,
                bits.read_bit()?,
            )
        };
        let poc = if bits.read_bit()? {
            let mut delta = bits.read_ue()?;
            if index != 0 && index != from_sps {
                delta = delta
                    .checked_add(previous_delta)
                    .ok_or_else(|| hevc_error("parse_long_term_rps", "delta cycle overflowed"))?;
            }
            previous_delta = delta;
            let maximum = 1_i64 << sps.log2_max_poc_lsb;
            let value = i64::from(lsb) + i64::from(current_poc)
                - i64::from(delta) * maximum
                - i64::from(current_lsb);
            i32::try_from(value)
                .map_err(|_| hevc_error("parse_long_term_rps", "LT POC exceeds i32"))?
        } else {
            i32::try_from(lsb)
                .map_err(|_| hevc_error("parse_long_term_rps", "LT POC exceeds i32"))?
        };
        output.push(LongTermReference { poc, used });
    }
    Ok(output)
}

fn ceil_log2(value: usize) -> u8 {
    if value <= 1 {
        0
    } else {
        u8::try_from((value - 1).bit_width()).unwrap_or(64)
    }
}

fn plus_one_ref(value: u32) -> VideoResult<u8> {
    let value = value
        .checked_add(1)
        .ok_or_else(|| hevc_error("slice", "reference count overflowed"))?;
    if value >= 16 {
        return Err(hevc_error("slice", "reference count exceeds 15"));
    }
    u8::try_from(value).map_err(|_| hevc_error("slice", "reference count exceeds u8"))
}
