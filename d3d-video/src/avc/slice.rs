use super::parameter_sets::{ParameterSets, PictureParameterSet, SequenceParameterSet};
use super::{MAX_REFS, NalHeader, PictureStructure, avc_error};
use crate::VideoResult;
use crate::bit_reader::BitReader;
use crate::rbsp::extract_rbsp;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SliceType {
    P,
    B,
    I,
    Sp,
    Si,
}

impl SliceType {
    const fn from_code(code: u32) -> Option<Self> {
        match code {
            0 => Some(Self::P),
            1 => Some(Self::B),
            2 => Some(Self::I),
            3 => Some(Self::Sp),
            4 => Some(Self::Si),
            _ => None,
        }
    }

    pub(crate) const fn is_intra(self) -> bool {
        matches!(self, Self::I | Self::Si)
    }

    const fn list_count(self) -> usize {
        match self {
            Self::B => 2,
            Self::P | Self::Sp => 1,
            Self::I | Self::Si => 0,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RefPicListModification {
    Subtract { difference: u32 },
    Add { difference: u32 },
    LongTerm { picture_number: u32 },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MemoryManagementOperation {
    ForgetShort { picture_number: u32 },
    ForgetLong { index: u8 },
    ShortToLong { picture_number: u32, index: u8 },
    SetMaxLong { max_index: Option<u8> },
    Reset,
    CurrentToLong { index: u8 },
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "fields intentionally mirror independent AVC slice-header syntax flags"
)]
pub(crate) struct SliceHeader {
    pub(crate) nal: NalHeader,
    pub(crate) first_mb_address: u32,
    pub(crate) slice_type: SliceType,
    pub(crate) fixed_slice_type: bool,
    pub(crate) pps_id: u8,
    pub(crate) frame_num: u32,
    pub(crate) structure: PictureStructure,
    pub(crate) idr_picture_id: Option<u32>,
    pub(crate) poc_lsb: u32,
    pub(crate) delta_poc_bottom: i32,
    pub(crate) delta_poc: [i32; 2],
    pub(crate) redundant_pic_count: u32,
    pub(crate) direct_spatial_mv_pred: bool,
    pub(crate) ref_count: [u8; 2],
    pub(crate) ref_modifications: [Vec<RefPicListModification>; 2],
    pub(crate) no_output_of_prior_pictures: bool,
    pub(crate) long_term_reference: bool,
    pub(crate) adaptive_ref_pic_marking: bool,
    pub(crate) memory_management: Vec<MemoryManagementOperation>,
    pub(crate) cabac_init_idc: u8,
    pub(crate) qscale: u8,
    pub(crate) deblocking_filter_idc: u8,
    pub(crate) slice_alpha_c0_offset: i8,
    pub(crate) slice_beta_offset: i8,
    pub(crate) header_bit_size: usize,
}

impl SliceHeader {
    #[expect(
        clippy::too_many_lines,
        reason = "keeps the ordered AVC slice-header grammar auditable against FFmpeg"
    )]
    pub(crate) fn parse_nal(nal: &[u8], sets: &ParameterSets) -> VideoResult<Self> {
        let (&header_byte, escaped_payload) = nal
            .split_first()
            .ok_or_else(|| avc_error("SliceHeader::parse_nal", "slice NAL is empty"))?;
        let nal = NalHeader::parse(header_byte)?;
        if !matches!(nal.unit_type, 1 | 5) {
            return Err(avc_error(
                "SliceHeader::parse_nal",
                format!("NAL type {} is not a supported AVC slice", nal.unit_type),
            ));
        }
        let rbsp = extract_rbsp(escaped_payload);
        let mut bits = BitReader::new(rbsp.data.as_ref());
        let first_mb_address = bits.read_ue()?;
        let raw_slice_type = bits.read_ue()?;
        if raw_slice_type > 9 {
            return Err(avc_error("SliceHeader::parse_nal", "slice type exceeds 9"));
        }
        let fixed_slice_type = raw_slice_type > 4;
        let slice_type = SliceType::from_code(raw_slice_type % 5)
            .ok_or_else(|| avc_error("SliceHeader::parse_nal", "invalid slice type"))?;
        if nal.is_idr() && !slice_type.is_intra() {
            return Err(avc_error(
                "SliceHeader::parse_nal",
                "IDR slice is not intra coded",
            ));
        }
        let pps_id_value = bits.read_ue()?;
        let (pps, sps) = sets.resolve_pps(pps_id_value)?;
        let pps_id = u8::try_from(pps_id_value)
            .map_err(|_| avc_error("SliceHeader::parse_nal", "PPS id does not fit u8"))?;
        let frame_num = u32::try_from(bits.read_bits(sps.log2_max_frame_num)?)
            .map_err(|_| avc_error("SliceHeader::parse_nal", "frame_num does not fit u32"))?;
        let structure = if sps.frame_mbs_only {
            PictureStructure::Frame
        } else if bits.read_bit()? {
            if bits.read_bit()? {
                PictureStructure::BottomField
            } else {
                PictureStructure::TopField
            }
        } else {
            PictureStructure::Frame
        };
        let idr_picture_id = if nal.is_idr() {
            let value = bits.read_ue()?;
            if value >= 65_536 {
                return Err(avc_error(
                    "SliceHeader::parse_nal",
                    "idr_pic_id exceeds 65535",
                ));
            }
            Some(value)
        } else {
            None
        };

        let mut poc_lsb = 0;
        let mut delta_poc_bottom = 0;
        let mut delta_poc = [0_i32; 2];
        if sps.poc_type == 0 {
            poc_lsb = u32::try_from(bits.read_bits(sps.log2_max_poc_lsb)?)
                .map_err(|_| avc_error("SliceHeader::parse_nal", "POC LSB does not fit u32"))?;
            if pps.pic_order_present && structure == PictureStructure::Frame {
                delta_poc_bottom = bits.read_se()?;
            }
        } else if sps.poc_type == 1 && !sps.delta_pic_order_always_zero {
            delta_poc[0] = bits.read_se()?;
            if pps.pic_order_present && structure == PictureStructure::Frame {
                delta_poc[1] = bits.read_se()?;
            }
        }
        let redundant_pic_count = if pps.redundant_pic_count_present {
            bits.read_ue()?
        } else {
            0
        };
        let direct_spatial_mv_pred = slice_type == SliceType::B && bits.read_bit()?;
        let ref_count = parse_ref_counts(&mut bits, pps, slice_type)?;
        let ref_modifications = parse_ref_modifications(&mut bits, slice_type, ref_count)?;
        if (pps.weighted_pred && matches!(slice_type, SliceType::P | SliceType::Sp))
            || (pps.weighted_bipred_idc == 1 && slice_type == SliceType::B)
        {
            skip_prediction_weight_table(&mut bits, sps, slice_type, ref_count)?;
        }

        let mut no_output_of_prior_pictures = false;
        let mut long_term_reference = false;
        let mut adaptive_ref_pic_marking = false;
        let mut memory_management = Vec::new();
        if nal.reference_idc != 0 {
            if nal.is_idr() {
                no_output_of_prior_pictures = bits.read_bit()?;
                long_term_reference = bits.read_bit()?;
                if long_term_reference {
                    memory_management.push(MemoryManagementOperation::CurrentToLong { index: 0 });
                }
            } else {
                adaptive_ref_pic_marking = bits.read_bit()?;
                if adaptive_ref_pic_marking {
                    memory_management = parse_memory_management(&mut bits, frame_num, sps)?;
                }
            }
        }

        let cabac_init_idc = if !slice_type.is_intra() && pps.cabac {
            bounded_u8(bits.read_ue()?, 2, "cabac_init_idc")?
        } else {
            0
        };
        let qscale_value = pps
            .init_qp
            .checked_add(bits.read_se()?)
            .ok_or_else(|| avc_error("SliceHeader::parse_nal", "slice QP overflowed"))?;
        let max_qp = 51 + 6 * (i32::from(sps.bit_depth_luma) - 8);
        if !(0..=max_qp).contains(&qscale_value) {
            return Err(avc_error(
                "SliceHeader::parse_nal",
                "slice QP is out of range",
            ));
        }
        if slice_type == SliceType::Sp {
            bits.read_bit()?;
        }
        if matches!(slice_type, SliceType::Sp | SliceType::Si) {
            bits.read_se()?;
        }
        let (deblocking_filter_idc, slice_alpha_c0_offset, slice_beta_offset) =
            if pps.deblocking_filter_parameters_present {
                let idc = bounded_u8(bits.read_ue()?, 2, "deblocking_filter_idc")?;
                if idc == 1 {
                    (idc, 0, 0)
                } else {
                    let alpha = bounded_i8(bits.read_se()?, -6, 6, "slice_alpha_c0_offset_div2")?;
                    let beta = bounded_i8(bits.read_se()?, -6, 6, "slice_beta_offset_div2")?;
                    (idc, alpha * 2, beta * 2)
                }
            } else {
                (0, 0, 0)
            };

        Ok(Self {
            nal,
            first_mb_address,
            slice_type,
            fixed_slice_type,
            pps_id,
            frame_num,
            structure,
            idr_picture_id,
            poc_lsb,
            delta_poc_bottom,
            delta_poc,
            redundant_pic_count,
            direct_spatial_mv_pred,
            ref_count,
            ref_modifications,
            no_output_of_prior_pictures,
            long_term_reference,
            adaptive_ref_pic_marking,
            memory_management,
            cabac_init_idc,
            qscale: u8::try_from(qscale_value)
                .map_err(|_| avc_error("SliceHeader::parse_nal", "slice QP does not fit u8"))?,
            deblocking_filter_idc,
            slice_alpha_c0_offset,
            slice_beta_offset,
            header_bit_size: bits.position(),
        })
    }

    pub(crate) fn current_picture_number(&self) -> u32 {
        if self.structure == PictureStructure::Frame {
            self.frame_num
        } else {
            2 * self.frame_num + 1
        }
    }

    pub(crate) fn maximum_picture_number(&self, sps: &SequenceParameterSet) -> u32 {
        1_u32 << (sps.log2_max_frame_num + u8::from(self.structure != PictureStructure::Frame))
    }
}

fn parse_ref_counts(
    bits: &mut BitReader<'_>,
    pps: &PictureParameterSet,
    slice_type: SliceType,
) -> VideoResult<[u8; 2]> {
    let mut result = pps.ref_count;
    match slice_type {
        SliceType::I | SliceType::Si => return Ok([0, 0]),
        SliceType::P | SliceType::Sp => result[1] = 0,
        SliceType::B => {}
    }
    if bits.read_bit()? {
        result[0] = plus_one_ref_count(bits.read_ue()?)?;
        if slice_type == SliceType::B {
            result[1] = plus_one_ref_count(bits.read_ue()?)?;
        }
    }
    Ok(result)
}

fn parse_ref_modifications(
    bits: &mut BitReader<'_>,
    slice_type: SliceType,
    ref_count: [u8; 2],
) -> VideoResult<[Vec<RefPicListModification>; 2]> {
    let mut lists = [Vec::new(), Vec::new()];
    for list in 0..slice_type.list_count() {
        if !bits.read_bit()? {
            continue;
        }
        loop {
            let operation = bits.read_ue()?;
            if operation == 3 {
                break;
            }
            if operation > 2 || lists[list].len() >= usize::from(ref_count[list]) {
                return Err(avc_error(
                    "parse_ref_modifications",
                    "reference list modification is invalid or exceeds the active count",
                ));
            }
            let value = bits.read_ue()?;
            lists[list].push(match operation {
                0 => RefPicListModification::Subtract {
                    difference: value + 1,
                },
                1 => RefPicListModification::Add {
                    difference: value + 1,
                },
                2 => RefPicListModification::LongTerm {
                    picture_number: value,
                },
                _ => return Err(avc_error("parse_ref_modifications", "invalid operation")),
            });
        }
    }
    Ok(lists)
}

fn skip_prediction_weight_table(
    bits: &mut BitReader<'_>,
    sps: &SequenceParameterSet,
    slice_type: SliceType,
    ref_count: [u8; 2],
) -> VideoResult<()> {
    bounded_u8(bits.read_ue()?, 7, "luma_log2_weight_denom")?;
    let has_chroma = sps.chroma_format_idc != 0;
    if has_chroma {
        bounded_u8(bits.read_ue()?, 7, "chroma_log2_weight_denom")?;
    }
    for &count in ref_count.iter().take(slice_type.list_count()) {
        for _ in 0..count {
            if bits.read_bit()? {
                bits.read_se()?;
                bits.read_se()?;
            }
            if has_chroma && bits.read_bit()? {
                for _ in 0..2 {
                    bits.read_se()?;
                    bits.read_se()?;
                }
            }
        }
    }
    Ok(())
}

fn parse_memory_management(
    bits: &mut BitReader<'_>,
    frame_num: u32,
    sps: &SequenceParameterSet,
) -> VideoResult<Vec<MemoryManagementOperation>> {
    let mut operations = Vec::new();
    let max_picture_number = 1_u32 << sps.log2_max_frame_num;
    for _ in 0..(MAX_REFS * 2 + 3) {
        let opcode = bits.read_ue()?;
        if opcode == 0 {
            return Ok(operations);
        }
        let operation = match opcode {
            1 => {
                let difference = bits.read_ue()?.checked_add(1).ok_or_else(|| {
                    avc_error(
                        "parse_memory_management",
                        "short picture difference overflowed",
                    )
                })?;
                MemoryManagementOperation::ForgetShort {
                    picture_number: frame_num.wrapping_sub(difference) & (max_picture_number - 1),
                }
            }
            2 => MemoryManagementOperation::ForgetLong {
                index: bounded_u8(bits.read_ue()?, 15, "long_term_pic_num")?,
            },
            3 => {
                let difference = bits.read_ue()?.checked_add(1).ok_or_else(|| {
                    avc_error(
                        "parse_memory_management",
                        "short picture difference overflowed",
                    )
                })?;
                let picture_number = frame_num.wrapping_sub(difference) & (max_picture_number - 1);
                let index = bounded_u8(bits.read_ue()?, 15, "long_term_frame_idx")?;
                MemoryManagementOperation::ShortToLong {
                    picture_number,
                    index,
                }
            }
            4 => {
                let plus_one = bounded_u8(bits.read_ue()?, 16, "max_long_term_frame_idx_plus1")?;
                MemoryManagementOperation::SetMaxLong {
                    max_index: plus_one.checked_sub(1),
                }
            }
            5 => MemoryManagementOperation::Reset,
            6 => MemoryManagementOperation::CurrentToLong {
                index: bounded_u8(bits.read_ue()?, 15, "long_term_frame_idx")?,
            },
            _ => {
                return Err(avc_error(
                    "parse_memory_management",
                    "memory management opcode exceeds 6",
                ));
            }
        };
        operations.push(operation);
    }
    Err(avc_error(
        "parse_memory_management",
        "memory management operation list has no terminator",
    ))
}

fn plus_one_ref_count(value: u32) -> VideoResult<u8> {
    let count = value
        .checked_add(1)
        .ok_or_else(|| avc_error("parse_ref_counts", "reference count overflowed"))?;
    bounded_u8(count, 32, "reference count")
}

fn bounded_u8(value: u32, maximum: u32, field: &'static str) -> VideoResult<u8> {
    if value > maximum {
        return Err(avc_error(
            "SliceHeader::parse_nal",
            format!("{field} exceeds {maximum}"),
        ));
    }
    u8::try_from(value)
        .map_err(|_| avc_error("SliceHeader::parse_nal", format!("{field} does not fit u8")))
}

fn bounded_i8(value: i32, minimum: i32, maximum: i32, field: &'static str) -> VideoResult<i8> {
    if !(minimum..=maximum).contains(&value) {
        return Err(avc_error(
            "SliceHeader::parse_nal",
            format!("{field} is out of range"),
        ));
    }
    i8::try_from(value)
        .map_err(|_| avc_error("SliceHeader::parse_nal", format!("{field} does not fit i8")))
}
