use super::{MAX_DPB_FRAMES, NalHeader, avc_error, unsupported};
use crate::VideoResult;
use crate::bit_reader::BitReader;
use crate::rbsp::extract_rbsp;

const MAX_SPS_COUNT: usize = 32;
const MAX_PPS_COUNT: usize = 256;

const DEFAULT_SCALING4: [[u8; 16]; 2] = [
    [
        6, 13, 20, 28, 13, 20, 28, 32, 20, 28, 32, 37, 28, 32, 37, 42,
    ],
    [
        10, 14, 20, 24, 14, 20, 24, 27, 20, 24, 27, 30, 24, 27, 30, 34,
    ],
];
const DEFAULT_SCALING8: [[u8; 64]; 2] = [
    [
        6, 10, 13, 16, 18, 23, 25, 27, 10, 11, 16, 18, 23, 25, 27, 29, 13, 16, 18, 23, 25, 27, 29,
        31, 16, 18, 23, 25, 27, 29, 31, 33, 18, 23, 25, 27, 29, 31, 33, 36, 23, 25, 27, 29, 31, 33,
        36, 38, 25, 27, 29, 31, 33, 36, 38, 40, 27, 29, 31, 33, 36, 38, 40, 42,
    ],
    [
        9, 13, 15, 17, 19, 21, 22, 24, 13, 13, 17, 19, 21, 22, 24, 25, 15, 17, 19, 21, 22, 24, 25,
        27, 17, 19, 21, 22, 24, 25, 27, 28, 19, 21, 22, 24, 25, 27, 28, 30, 21, 22, 24, 25, 27, 28,
        30, 32, 22, 24, 25, 27, 28, 30, 32, 33, 24, 25, 27, 28, 30, 32, 33, 35,
    ],
];
const ZIGZAG4: [usize; 16] = [0, 1, 4, 8, 5, 2, 3, 6, 9, 12, 13, 10, 7, 11, 14, 15];
const ZIGZAG8: [usize; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20,
    13, 6, 7, 14, 21, 28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58, 59,
    52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63,
];

#[derive(Clone, Debug, Eq, PartialEq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "fields intentionally mirror independent AVC SPS syntax flags"
)]
pub(crate) struct SequenceParameterSet {
    pub(crate) id: u8,
    pub(crate) profile_idc: u8,
    pub(crate) constraint_set_flags: u8,
    pub(crate) level_idc: u8,
    pub(crate) chroma_format_idc: u8,
    pub(crate) bit_depth_luma: u8,
    pub(crate) bit_depth_chroma: u8,
    pub(crate) transform_bypass: bool,
    pub(crate) log2_max_frame_num: u8,
    pub(crate) poc_type: u8,
    pub(crate) log2_max_poc_lsb: u8,
    pub(crate) delta_pic_order_always_zero: bool,
    pub(crate) offset_for_non_ref_pic: i32,
    pub(crate) offset_for_top_to_bottom_field: i32,
    pub(crate) offset_for_ref_frame: Vec<i32>,
    pub(crate) max_num_ref_frames: u8,
    pub(crate) gaps_in_frame_num_allowed: bool,
    pub(crate) width_in_mbs: u16,
    pub(crate) height_in_mbs: u16,
    pub(crate) frame_mbs_only: bool,
    pub(crate) mb_adaptive_frame_field: bool,
    pub(crate) direct_8x8_inference: bool,
    pub(crate) crop_left: u32,
    pub(crate) crop_right: u32,
    pub(crate) crop_top: u32,
    pub(crate) crop_bottom: u32,
    pub(crate) vui_present: bool,
    pub(crate) scaling_matrix_present: bool,
    pub(crate) scaling_matrix4: [[u8; 16]; 6],
    pub(crate) scaling_matrix8: [[u8; 64]; 6],
}

impl SequenceParameterSet {
    pub(crate) fn coded_width(&self) -> u32 {
        u32::from(self.width_in_mbs) * 16
    }

    pub(crate) fn coded_height(&self) -> u32 {
        u32::from(self.height_in_mbs) * 16
    }

    pub(crate) fn display_width(&self) -> u32 {
        self.coded_width() - self.crop_left - self.crop_right
    }

    pub(crate) fn display_height(&self) -> u32 {
        self.coded_height() - self.crop_top - self.crop_bottom
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "fields intentionally mirror independent AVC PPS syntax flags"
)]
pub(crate) struct PictureParameterSet {
    pub(crate) id: u8,
    pub(crate) sps_id: u8,
    pub(crate) cabac: bool,
    pub(crate) pic_order_present: bool,
    pub(crate) ref_count: [u8; 2],
    pub(crate) weighted_pred: bool,
    pub(crate) weighted_bipred_idc: u8,
    pub(crate) init_qp: i32,
    pub(crate) init_qs: i32,
    pub(crate) chroma_qp_index_offset: [i8; 2],
    pub(crate) deblocking_filter_parameters_present: bool,
    pub(crate) constrained_intra_pred: bool,
    pub(crate) redundant_pic_count_present: bool,
    pub(crate) transform_8x8_mode: bool,
    pub(crate) scaling_matrix4: [[u8; 16]; 6],
    pub(crate) scaling_matrix8: [[u8; 64]; 6],
}

#[derive(Debug)]
pub(crate) struct ParameterSets {
    sequence: [Option<SequenceParameterSet>; MAX_SPS_COUNT],
    picture: [Option<PictureParameterSet>; MAX_PPS_COUNT],
}

impl Default for ParameterSets {
    fn default() -> Self {
        Self {
            sequence: std::array::from_fn(|_| None),
            picture: std::array::from_fn(|_| None),
        }
    }
}

impl ParameterSets {
    pub(crate) fn insert_sps_nal(&mut self, nal: &[u8]) -> VideoResult<&SequenceParameterSet> {
        let payload = nal_payload(nal, 7, "ParameterSets::insert_sps_nal")?;
        let rbsp = extract_rbsp(payload);
        let sps = parse_sps(rbsp.data.as_ref())?;
        let index = usize::from(sps.id);
        self.sequence[index] = Some(sps);
        self.sequence[index]
            .as_ref()
            .ok_or_else(|| avc_error("ParameterSets::insert_sps_nal", "SPS insertion failed"))
    }

    pub(crate) fn insert_pps_nal(&mut self, nal: &[u8]) -> VideoResult<&PictureParameterSet> {
        let payload = nal_payload(nal, 8, "ParameterSets::insert_pps_nal")?;
        let rbsp = extract_rbsp(payload);
        let mut id_reader = BitReader::new(rbsp.data.as_ref());
        let pps_id = id_reader.read_ue()?;
        let sps_id = id_reader.read_ue()?;
        let sps_index = usize::try_from(sps_id)
            .map_err(|_| avc_error("ParameterSets::insert_pps_nal", "SPS id is too large"))?;
        let sps = self
            .sequence
            .get(sps_index)
            .and_then(Option::as_ref)
            .ok_or_else(|| {
                avc_error(
                    "ParameterSets::insert_pps_nal",
                    "PPS references an unknown SPS",
                )
            })?;
        let pps = parse_pps(rbsp.data.as_ref(), sps)?;
        if pps_id != u32::from(pps.id) {
            return Err(avc_error(
                "ParameterSets::insert_pps_nal",
                "PPS id changed while parsing",
            ));
        }
        let index = usize::from(pps.id);
        self.picture[index] = Some(pps);
        self.picture[index]
            .as_ref()
            .ok_or_else(|| avc_error("ParameterSets::insert_pps_nal", "PPS insertion failed"))
    }

    pub(crate) fn sps(&self, id: u8) -> Option<&SequenceParameterSet> {
        self.sequence.get(usize::from(id)).and_then(Option::as_ref)
    }

    pub(crate) fn pps(&self, id: u8) -> Option<&PictureParameterSet> {
        self.picture.get(usize::from(id)).and_then(Option::as_ref)
    }

    pub(crate) fn resolve_pps(
        &self,
        id: u32,
    ) -> VideoResult<(&PictureParameterSet, &SequenceParameterSet)> {
        let index = usize::try_from(id)
            .map_err(|_| avc_error("ParameterSets::resolve_pps", "PPS id is too large"))?;
        let pps = self
            .picture
            .get(index)
            .and_then(Option::as_ref)
            .ok_or_else(|| {
                avc_error(
                    "ParameterSets::resolve_pps",
                    "slice references an unknown PPS",
                )
            })?;
        let sps = self.sps(pps.sps_id).ok_or_else(|| {
            avc_error(
                "ParameterSets::resolve_pps",
                "PPS references an unknown SPS",
            )
        })?;
        Ok((pps, sps))
    }
}

fn nal_payload<'data>(
    nal: &'data [u8],
    expected_type: u8,
    operation: &'static str,
) -> VideoResult<&'data [u8]> {
    let (&header, payload) = nal
        .split_first()
        .ok_or_else(|| avc_error(operation, "NAL unit is empty"))?;
    let header = NalHeader::parse(header)?;
    if header.unit_type != expected_type {
        return Err(avc_error(
            operation,
            format!(
                "expected NAL type {expected_type}, got {}",
                header.unit_type
            ),
        ));
    }
    Ok(payload)
}

#[expect(
    clippy::too_many_lines,
    reason = "keeps the ordered AVC SPS grammar auditable against FFmpeg"
)]
fn parse_sps(rbsp: &[u8]) -> VideoResult<SequenceParameterSet> {
    let mut bits = BitReader::new(rbsp);
    let profile_idc = read_u8(&mut bits, 8, "parse_sps")?;
    let mut constraint_set_flags = 0_u8;
    for index in 0..6 {
        if bits.read_bit()? {
            constraint_set_flags |= 1_u8 << index;
        }
    }
    bits.skip_bits(2)?;
    let level_idc = read_u8(&mut bits, 8, "parse_sps")?;
    let id = bounded_u8(bits.read_ue()?, 31, "parse_sps", "SPS id")?;

    let mut scaling_matrix4 = [[16_u8; 16]; 6];
    let mut scaling_matrix8 = [[16_u8; 64]; 6];
    let mut scaling_matrix_present = false;
    let (chroma_format_idc, bit_depth_luma, bit_depth_chroma, transform_bypass) =
        if is_high_profile(profile_idc) {
            let chroma = bounded_u8(bits.read_ue()?, 3, "parse_sps", "chroma_format_idc")?;
            if chroma == 3 && bits.read_bit()? {
                return Err(unsupported(
                    "parse_sps",
                    "separate colour planes are unsupported",
                ));
            }
            let luma = bits
                .read_ue()?
                .checked_add(8)
                .ok_or_else(|| avc_error("parse_sps", "luma bit depth overflowed"))?;
            let chroma_depth = bits
                .read_ue()?
                .checked_add(8)
                .ok_or_else(|| avc_error("parse_sps", "chroma bit depth overflowed"))?;
            if luma != chroma_depth || !(8..=14).contains(&luma) {
                return Err(avc_error(
                    "parse_sps",
                    "invalid or differing component bit depths",
                ));
            }
            let bypass = bits.read_bit()?;
            if bits.read_bit()? {
                decode_scaling_matrices(
                    &mut bits,
                    true,
                    false,
                    chroma,
                    None,
                    &mut scaling_matrix4,
                    &mut scaling_matrix8,
                )?;
                scaling_matrix_present = true;
            }
            (
                chroma,
                u8::try_from(luma).map_err(|_| avc_error("parse_sps", "invalid luma depth"))?,
                u8::try_from(chroma_depth)
                    .map_err(|_| avc_error("parse_sps", "invalid chroma depth"))?,
                bypass,
            )
        } else {
            (1, 8, 8, false)
        };

    let log2_max_frame_num = bounded_u8(
        bits.read_ue()?,
        12,
        "parse_sps",
        "log2_max_frame_num_minus4",
    )? + 4;
    let poc_type = bounded_u8(bits.read_ue()?, 2, "parse_sps", "pic_order_cnt_type")?;
    let mut log2_max_poc_lsb = 0;
    let mut delta_pic_order_always_zero = false;
    let mut offset_for_non_ref_pic = 0;
    let mut offset_for_top_to_bottom_field = 0;
    let mut offset_for_ref_frame = Vec::new();
    if poc_type == 0 {
        log2_max_poc_lsb = bounded_u8(
            bits.read_ue()?,
            12,
            "parse_sps",
            "log2_max_pic_order_cnt_lsb_minus4",
        )? + 4;
    } else if poc_type == 1 {
        delta_pic_order_always_zero = bits.read_bit()?;
        offset_for_non_ref_pic = bits.read_se()?;
        offset_for_top_to_bottom_field = bits.read_se()?;
        let cycle = bits.read_ue()?;
        if cycle > 255 {
            return Err(avc_error("parse_sps", "POC cycle length exceeds 255"));
        }
        for _ in 0..cycle {
            offset_for_ref_frame.push(bits.read_se()?);
        }
    }

    let max_num_ref_frames = bounded_u8(
        bits.read_ue()?,
        u8::try_from(MAX_DPB_FRAMES).map_or(16, u32::from),
        "parse_sps",
        "max_num_ref_frames",
    )?;
    let gaps_in_frame_num_allowed = bits.read_bit()?;
    let width_in_mbs = plus_one_u16(bits.read_ue()?, "parse_sps", "picture width")?;
    let map_height = plus_one_u16(bits.read_ue()?, "parse_sps", "picture height")?;
    let frame_mbs_only = bits.read_bit()?;
    let height_in_mbs = u32::from(map_height)
        .checked_mul(if frame_mbs_only { 1 } else { 2 })
        .filter(|height| *height <= 1055)
        .ok_or_else(|| avc_error("parse_sps", "picture height exceeds the AVC limit"))?;
    if width_in_mbs > 1055 {
        return Err(avc_error(
            "parse_sps",
            "picture width exceeds the AVC limit",
        ));
    }
    let mb_adaptive_frame_field = if frame_mbs_only {
        false
    } else {
        bits.read_bit()?
    };
    let direct_8x8_inference = bits.read_bit()?;
    let crop = bits.read_bit()?;
    let (crop_left, crop_right, crop_top, crop_bottom) = if crop {
        let raw = [
            bits.read_ue()?,
            bits.read_ue()?,
            bits.read_ue()?,
            bits.read_ue()?,
        ];
        let horizontal_subsample = u32::from(chroma_format_idc == 1 || chroma_format_idc == 2);
        let vertical_subsample = u32::from(chroma_format_idc == 1);
        let step_x = 1_u32 << horizontal_subsample;
        let step_y = (if frame_mbs_only { 1 } else { 2 }) << vertical_subsample;
        let scaled = [
            raw[0].checked_mul(step_x),
            raw[1].checked_mul(step_x),
            raw[2].checked_mul(step_y),
            raw[3].checked_mul(step_y),
        ];
        let values =
            scaled.map(|value| value.ok_or_else(|| avc_error("parse_sps", "crop overflowed")));
        let [left, right, top, bottom] = values;
        let (left, right, top, bottom) = (left?, right?, top?, bottom?);
        let width = u32::from(width_in_mbs) * 16;
        let height = height_in_mbs * 16;
        if left + right >= width || top + bottom >= height {
            return Err(avc_error("parse_sps", "crop removes the entire picture"));
        }
        (left, right, top, bottom)
    } else {
        (0, 0, 0, 0)
    };
    let vui_present = bits.read_bit()?;

    Ok(SequenceParameterSet {
        id,
        profile_idc,
        constraint_set_flags,
        level_idc,
        chroma_format_idc,
        bit_depth_luma,
        bit_depth_chroma,
        transform_bypass,
        log2_max_frame_num,
        poc_type,
        log2_max_poc_lsb,
        delta_pic_order_always_zero,
        offset_for_non_ref_pic,
        offset_for_top_to_bottom_field,
        offset_for_ref_frame,
        max_num_ref_frames,
        gaps_in_frame_num_allowed,
        width_in_mbs,
        height_in_mbs: u16::try_from(height_in_mbs)
            .map_err(|_| avc_error("parse_sps", "picture height does not fit u16"))?,
        frame_mbs_only,
        mb_adaptive_frame_field,
        direct_8x8_inference,
        crop_left,
        crop_right,
        crop_top,
        crop_bottom,
        vui_present,
        scaling_matrix_present,
        scaling_matrix4,
        scaling_matrix8,
    })
}

#[expect(
    clippy::similar_names,
    reason = "init_qp and init_qs are distinct normative AVC syntax fields"
)]
fn parse_pps(rbsp: &[u8], sps: &SequenceParameterSet) -> VideoResult<PictureParameterSet> {
    let mut bits = BitReader::new(rbsp);
    let id = bounded_u8(bits.read_ue()?, 255, "parse_pps", "PPS id")?;
    let sps_id = bounded_u8(bits.read_ue()?, 31, "parse_pps", "SPS id")?;
    if sps_id != sps.id {
        return Err(avc_error(
            "parse_pps",
            "PPS was parsed against the wrong SPS",
        ));
    }
    let cabac = bits.read_bit()?;
    let pic_order_present = bits.read_bit()?;
    let slice_group_count = bits
        .read_ue()?
        .checked_add(1)
        .ok_or_else(|| avc_error("parse_pps", "slice group count overflowed"))?;
    if slice_group_count > 1 {
        return Err(unsupported(
            "parse_pps",
            "flexible macroblock ordering is unsupported",
        ));
    }
    let ref_count = [
        plus_one_ref_count(bits.read_ue()?, "parse_pps")?,
        plus_one_ref_count(bits.read_ue()?, "parse_pps")?,
    ];
    let weighted_pred = bits.read_bit()?;
    let weighted_bipred_idc = read_u8(&mut bits, 2, "parse_pps")?;
    let qp_offset = 6 * (i32::from(sps.bit_depth_luma) - 8);
    let init_qp = bits
        .read_se()?
        .checked_add(26 + qp_offset)
        .ok_or_else(|| avc_error("parse_pps", "initial QP overflowed"))?;
    let init_qs = bits
        .read_se()?
        .checked_add(26 + qp_offset)
        .ok_or_else(|| avc_error("parse_pps", "initial QS overflowed"))?;
    let first_chroma_offset =
        bounded_i8(bits.read_se()?, -12, 12, "parse_pps", "chroma QP offset")?;
    let deblocking_filter_parameters_present = bits.read_bit()?;
    let constrained_intra_pred = bits.read_bit()?;
    let redundant_pic_count_present = bits.read_bit()?;

    let mut transform_8x8_mode = false;
    let mut scaling_matrix4 = sps.scaling_matrix4;
    let mut scaling_matrix8 = sps.scaling_matrix8;
    let mut second_chroma_offset = first_chroma_offset;
    if bits.has_more_rbsp_data()? && pps_may_have_extension(sps) {
        transform_8x8_mode = bits.read_bit()?;
        if bits.read_bit()? {
            decode_scaling_matrices(
                &mut bits,
                false,
                transform_8x8_mode,
                sps.chroma_format_idc,
                sps.scaling_matrix_present.then_some(sps),
                &mut scaling_matrix4,
                &mut scaling_matrix8,
            )?;
        }
        second_chroma_offset = bounded_i8(
            bits.read_se()?,
            -12,
            12,
            "parse_pps",
            "second chroma QP offset",
        )?;
    }

    Ok(PictureParameterSet {
        id,
        sps_id,
        cabac,
        pic_order_present,
        ref_count,
        weighted_pred,
        weighted_bipred_idc,
        init_qp,
        init_qs,
        chroma_qp_index_offset: [first_chroma_offset, second_chroma_offset],
        deblocking_filter_parameters_present,
        constrained_intra_pred,
        redundant_pic_count_present,
        transform_8x8_mode,
        scaling_matrix4,
        scaling_matrix8,
    })
}

fn decode_scaling_matrices(
    bits: &mut BitReader<'_>,
    is_sps: bool,
    transform_8x8_mode: bool,
    chroma_format_idc: u8,
    fallback_sps: Option<&SequenceParameterSet>,
    matrix4: &mut [[u8; 16]; 6],
    matrix8: &mut [[u8; 64]; 6],
) -> VideoResult<()> {
    for index in 0..6 {
        let fallback = if index == 0 || index == 3 {
            fallback_sps.map_or(DEFAULT_SCALING4[usize::from(index == 3)], |sps| {
                sps.scaling_matrix4[index]
            })
        } else {
            matrix4[index - 1]
        };
        let default = DEFAULT_SCALING4[usize::from(index >= 3)];
        decode_scaling_list(bits, &mut matrix4[index], &ZIGZAG4, &default, &fallback)?;
    }
    if is_sps || transform_8x8_mode {
        let count = if chroma_format_idc == 3 { 6 } else { 2 };
        let order = [0_usize, 3, 1, 4, 2, 5];
        for &index in &order[..count] {
            let intra = index < 3;
            let fallback = if index == 0 || index == 3 {
                fallback_sps.map_or(DEFAULT_SCALING8[usize::from(!intra)], |sps| {
                    sps.scaling_matrix8[index]
                })
            } else {
                matrix8[index - 1]
            };
            let default = DEFAULT_SCALING8[usize::from(!intra)];
            decode_scaling_list(bits, &mut matrix8[index], &ZIGZAG8, &default, &fallback)?;
        }
    }
    Ok(())
}

fn decode_scaling_list<const N: usize>(
    bits: &mut BitReader<'_>,
    factors: &mut [u8; N],
    scan: &[usize; N],
    jvt_default: &[u8; N],
    fallback: &[u8; N],
) -> VideoResult<()> {
    if !bits.read_bit()? {
        *factors = *fallback;
        return Ok(());
    }
    let mut last = 8_i32;
    let mut next = 8_i32;
    for (position, &destination) in scan.iter().enumerate() {
        if next != 0 {
            let delta = bits.read_se()?;
            if !(-128..=127).contains(&delta) {
                return Err(avc_error(
                    "decode_scaling_list",
                    "delta scale is outside i8",
                ));
            }
            next = (last + delta) & 0xff;
        }
        if position == 0 && next == 0 {
            *factors = *jvt_default;
            break;
        }
        let value = if next == 0 { last } else { next };
        factors[destination] = u8::try_from(value)
            .map_err(|_| avc_error("decode_scaling_list", "scale is outside u8"))?;
        last = value;
    }
    Ok(())
}

const fn is_high_profile(profile: u8) -> bool {
    matches!(
        profile,
        44 | 83 | 86 | 100 | 110 | 118 | 122 | 128 | 138 | 144 | 244
    )
}

fn pps_may_have_extension(sps: &SequenceParameterSet) -> bool {
    !matches!(sps.profile_idc, 66 | 77 | 88) || sps.constraint_set_flags.trailing_zeros() >= 3
}

fn read_u8(bits: &mut BitReader<'_>, count: u8, operation: &'static str) -> VideoResult<u8> {
    u8::try_from(bits.read_bits(count)?)
        .map_err(|_| avc_error(operation, "bit field does not fit u8"))
}

fn bounded_u8(
    value: u32,
    maximum: u32,
    operation: &'static str,
    field: &'static str,
) -> VideoResult<u8> {
    if value > maximum {
        return Err(avc_error(operation, format!("{field} exceeds {maximum}")));
    }
    u8::try_from(value).map_err(|_| avc_error(operation, format!("{field} does not fit u8")))
}

fn bounded_i8(
    value: i32,
    minimum: i32,
    maximum: i32,
    operation: &'static str,
    field: &'static str,
) -> VideoResult<i8> {
    if !(minimum..=maximum).contains(&value) {
        return Err(avc_error(operation, format!("{field} is out of range")));
    }
    i8::try_from(value).map_err(|_| avc_error(operation, format!("{field} does not fit i8")))
}

fn plus_one_u16(value: u32, operation: &'static str, field: &'static str) -> VideoResult<u16> {
    let value = value
        .checked_add(1)
        .ok_or_else(|| avc_error(operation, format!("{field} overflowed")))?;
    u16::try_from(value).map_err(|_| avc_error(operation, format!("{field} does not fit u16")))
}

fn plus_one_ref_count(value: u32, operation: &'static str) -> VideoResult<u8> {
    let count = value
        .checked_add(1)
        .ok_or_else(|| avc_error(operation, "reference count overflowed"))?;
    bounded_u8(count, 32, operation, "reference count")
}
