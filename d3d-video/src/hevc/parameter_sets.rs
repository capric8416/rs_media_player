use super::rps::ShortTermRps;
use super::{NalHeader, hevc_error, unsupported};
use crate::VideoResult;
use crate::bit_reader::BitReader;
use crate::rbsp::extract_rbsp;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ProfileTierLevel {
    pub(crate) profile_space: u8,
    pub(crate) tier: bool,
    pub(crate) profile_idc: u8,
    pub(crate) level_idc: u8,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct VideoParameterSet {
    pub(crate) id: u8,
    pub(crate) max_layers: u8,
    pub(crate) max_sub_layers: u8,
    pub(crate) temporal_id_nesting: bool,
    pub(crate) profile: ProfileTierLevel,
    pub(crate) max_dec_pic_buffering: Vec<u8>,
    pub(crate) max_num_reorder_pics: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct ConformanceWindow {
    pub(crate) left: u32,
    pub(crate) right: u32,
    pub(crate) top: u32,
    pub(crate) bottom: u32,
}

/// HEVC scaling lists stored in the bitstream's diagonal scan order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ScalingListData {
    pub(crate) matrices: [[[u8; 64]; 6]; 4],
    pub(crate) dc_coefficients: [[u8; 6]; 4],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PcmParameters {
    pub(crate) bit_depth_luma_minus1: u8,
    pub(crate) bit_depth_chroma_minus1: u8,
    pub(crate) log2_min_cb_size_minus3: u8,
    pub(crate) log2_diff_max_min_cb_size: u8,
    pub(crate) loop_filter_disabled: bool,
}

impl Default for ScalingListData {
    fn default() -> Self {
        let mut data = Self {
            matrices: [[[0; 64]; 6]; 4],
            dc_coefficients: [[16; 6]; 4],
        };
        for (size_id, matrices) in data.matrices.iter_mut().enumerate() {
            for (matrix_id, matrix) in matrices.iter_mut().enumerate() {
                if size_id == 0 {
                    matrix[..16].fill(16);
                    continue;
                }
                let raster = if matrix_id < 3 {
                    DEFAULT_INTRA_8X8
                } else {
                    DEFAULT_INTER_8X8
                };
                let mut scan_index = 0;
                for diagonal in 0_usize..15 {
                    let start = diagonal.saturating_sub(7);
                    let end = diagonal.min(7);
                    for x in start..=end {
                        let y = diagonal - x;
                        matrix[scan_index] = raster[y * 8 + x];
                        scan_index += 1;
                    }
                }
            }
        }
        data
    }
}

const DEFAULT_INTRA_8X8: [u8; 64] = [
    16, 16, 16, 16, 17, 18, 21, 24, 16, 16, 16, 16, 17, 19, 22, 25, 16, 16, 17, 18, 20, 22, 25, 29,
    16, 16, 18, 21, 24, 27, 31, 36, 17, 17, 20, 24, 30, 35, 41, 47, 18, 19, 22, 27, 35, 44, 54, 65,
    21, 22, 25, 31, 41, 54, 70, 88, 24, 25, 29, 36, 47, 65, 88, 115,
];
const DEFAULT_INTER_8X8: [u8; 64] = [
    16, 16, 16, 16, 17, 18, 20, 24, 16, 16, 16, 17, 18, 20, 24, 25, 16, 16, 17, 18, 20, 24, 25, 28,
    16, 17, 18, 20, 24, 25, 28, 33, 17, 18, 20, 24, 25, 28, 33, 41, 18, 20, 24, 25, 28, 33, 41, 54,
    20, 24, 25, 28, 33, 41, 54, 71, 24, 25, 28, 33, 41, 54, 71, 91,
];

fn parse_scaling_list_data(bits: &mut BitReader<'_>) -> VideoResult<ScalingListData> {
    let mut lists = ScalingListData::default();
    for size_id in 0..4 {
        let matrix_step = if size_id == 3 { 3 } else { 1 };
        for matrix_id in (0..6).step_by(matrix_step) {
            let matrix_id_u32 = u32::try_from(matrix_id)
                .map_err(|_| hevc_error("parse_scaling_list_data", "matrix id overflow"))?;
            if !bits.read_bit()? {
                let delta = bits.read_ue()?;
                let delta = delta
                    .checked_mul(u32::try_from(matrix_step).map_err(|_| {
                        hevc_error("parse_scaling_list_data", "matrix step overflow")
                    })?)
                    .ok_or_else(|| {
                        hevc_error("parse_scaling_list_data", "matrix delta overflow")
                    })?;
                if delta > matrix_id_u32 {
                    return Err(hevc_error(
                        "parse_scaling_list_data",
                        "scaling-list prediction references a negative matrix id",
                    ));
                }
                if delta != 0 {
                    let source = matrix_id
                        - usize::try_from(delta).map_err(|_| {
                            hevc_error("parse_scaling_list_data", "matrix delta overflow")
                        })?;
                    lists.matrices[size_id][matrix_id] = lists.matrices[size_id][source];
                    lists.dc_coefficients[size_id][matrix_id] =
                        lists.dc_coefficients[size_id][source];
                }
                continue;
            }

            let dc = if size_id > 1 {
                let value = bits.read_se()?.checked_add(8).ok_or_else(|| {
                    hevc_error("parse_scaling_list_data", "scaling-list DC overflow")
                })?;
                u8::try_from(value)
                    .ok()
                    .filter(|value| *value != 0)
                    .ok_or_else(|| {
                        hevc_error(
                            "parse_scaling_list_data",
                            "scaling-list DC coefficient is invalid",
                        )
                    })?
            } else {
                16
            };
            lists.dc_coefficients[size_id][matrix_id] = dc;
            let coefficient_count = 1_usize << (4 + (size_id << 1)).min(6);
            let mut next_coefficient = if size_id > 1 { i64::from(dc) } else { 8 };
            for coefficient in &mut lists.matrices[size_id][matrix_id][..coefficient_count] {
                let delta = bits.read_se()?;
                next_coefficient = (next_coefficient + i64::from(delta)).rem_euclid(256);
                *coefficient = u8::try_from(next_coefficient).map_err(|_| {
                    hevc_error("parse_scaling_list_data", "scaling coefficient exceeds u8")
                })?;
            }
        }
    }
    Ok(lists)
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "fields mirror independent HEVC SPS syntax flags"
)]
pub(crate) struct SequenceParameterSet {
    pub(crate) id: u8,
    pub(crate) vps_id: u8,
    pub(crate) max_sub_layers: u8,
    pub(crate) profile: ProfileTierLevel,
    pub(crate) chroma_format_idc: u8,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) conformance: ConformanceWindow,
    pub(crate) bit_depth_luma: u8,
    pub(crate) bit_depth_chroma: u8,
    pub(crate) log2_max_poc_lsb: u8,
    pub(crate) max_dec_pic_buffering: Vec<u8>,
    pub(crate) max_num_reorder_pics: Vec<u8>,
    pub(crate) log2_min_luma_coding_block_size: u8,
    pub(crate) log2_ctb_size: u8,
    pub(crate) log2_min_transform_block_size: u8,
    pub(crate) log2_diff_max_min_transform_block_size: u8,
    pub(crate) max_transform_hierarchy_depth_inter: u8,
    pub(crate) max_transform_hierarchy_depth_intra: u8,
    pub(crate) scaling_list_enabled: bool,
    pub(crate) scaling_list_data: Option<ScalingListData>,
    pub(crate) pcm_enabled: bool,
    pub(crate) pcm: Option<PcmParameters>,
    pub(crate) amp_enabled: bool,
    pub(crate) sao_enabled: bool,
    pub(crate) temporal_mvp_enabled: bool,
    pub(crate) strong_intra_smoothing_enabled: bool,
    pub(crate) short_term_rps: Vec<ShortTermRps>,
    pub(crate) long_term_lsb: Vec<(u32, bool)>,
}

impl SequenceParameterSet {
    pub(crate) fn display_width(&self) -> u32 {
        self.width - self.conformance.left - self.conformance.right
    }

    pub(crate) fn display_height(&self) -> u32 {
        self.height - self.conformance.top - self.conformance.bottom
    }

    pub(crate) fn ctb_count(&self) -> u32 {
        let size = 1_u32 << self.log2_ctb_size;
        self.width.div_ceil(size) * self.height.div_ceil(size)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "fields mirror independent HEVC PPS syntax flags"
)]
pub(crate) struct PictureParameterSet {
    pub(crate) id: u8,
    pub(crate) sps_id: u8,
    pub(crate) dependent_slice_segments_enabled: bool,
    pub(crate) output_flag_present: bool,
    pub(crate) extra_slice_header_bits: u8,
    pub(crate) sign_data_hiding: bool,
    pub(crate) cabac_init_present: bool,
    pub(crate) default_ref_count: [u8; 2],
    pub(crate) init_qp_minus26: i8,
    pub(crate) constrained_intra_pred: bool,
    pub(crate) transform_skip_enabled: bool,
    pub(crate) cu_qp_delta_enabled: bool,
    pub(crate) cb_qp_offset: i8,
    pub(crate) cr_qp_offset: i8,
    pub(crate) slice_chroma_qp_offsets_present: bool,
    pub(crate) weighted_pred: bool,
    pub(crate) weighted_bipred: bool,
    pub(crate) transquant_bypass_enabled: bool,
    pub(crate) tiles_enabled: bool,
    pub(crate) tile_columns_minus1: u8,
    pub(crate) tile_rows_minus1: u8,
    pub(crate) uniform_spacing: bool,
    pub(crate) column_width_minus1: Vec<u16>,
    pub(crate) row_height_minus1: Vec<u16>,
    pub(crate) loop_filter_across_tiles_enabled: bool,
    pub(crate) entropy_coding_sync_enabled: bool,
    pub(crate) delta_cu_qp_delta_depth: u8,
    pub(crate) loop_filter_across_slices_enabled: bool,
    pub(crate) deblocking_filter_override_enabled: bool,
    pub(crate) disable_deblocking_filter: bool,
    pub(crate) beta_offset: i8,
    pub(crate) tc_offset: i8,
    pub(crate) lists_modification_present: bool,
    pub(crate) parallel_merge_level: u8,
    pub(crate) slice_header_extension_present: bool,
    pub(crate) scaling_list_data: Option<ScalingListData>,
}

#[derive(Debug)]
pub(crate) struct ParameterSets {
    vps: [Option<VideoParameterSet>; 16],
    sps: [Option<SequenceParameterSet>; 16],
    pps: [Option<PictureParameterSet>; 64],
}

impl Default for ParameterSets {
    fn default() -> Self {
        Self {
            vps: std::array::from_fn(|_| None),
            sps: std::array::from_fn(|_| None),
            pps: std::array::from_fn(|_| None),
        }
    }
}

impl ParameterSets {
    pub(crate) fn insert_vps_nal(&mut self, nal: &[u8]) -> VideoResult<&VideoParameterSet> {
        let rbsp = nal_rbsp(nal, 32, "insert_vps_nal")?;
        let value = parse_vps(rbsp.data.as_ref())?;
        let index = usize::from(value.id);
        self.vps[index] = Some(value);
        self.vps[index]
            .as_ref()
            .ok_or_else(|| hevc_error("insert_vps_nal", "insertion failed"))
    }

    pub(crate) fn insert_sps_nal(&mut self, nal: &[u8]) -> VideoResult<&SequenceParameterSet> {
        let rbsp = nal_rbsp(nal, 33, "insert_sps_nal")?;
        let value = parse_sps(rbsp.data.as_ref(), &self.vps)?;
        let index = usize::from(value.id);
        self.sps[index] = Some(value);
        self.sps[index]
            .as_ref()
            .ok_or_else(|| hevc_error("insert_sps_nal", "insertion failed"))
    }

    pub(crate) fn insert_pps_nal(&mut self, nal: &[u8]) -> VideoResult<&PictureParameterSet> {
        let rbsp = nal_rbsp(nal, 34, "insert_pps_nal")?;
        let value = parse_pps(rbsp.data.as_ref(), &self.sps)?;
        let index = usize::from(value.id);
        self.pps[index] = Some(value);
        self.pps[index]
            .as_ref()
            .ok_or_else(|| hevc_error("insert_pps_nal", "insertion failed"))
    }

    pub(crate) fn resolve_pps(
        &self,
        id: u32,
    ) -> VideoResult<(&PictureParameterSet, &SequenceParameterSet)> {
        let index =
            usize::try_from(id).map_err(|_| hevc_error("resolve_pps", "PPS id is too large"))?;
        let pps = self
            .pps
            .get(index)
            .and_then(Option::as_ref)
            .ok_or_else(|| hevc_error("resolve_pps", "unknown PPS"))?;
        let sps = self.sps[usize::from(pps.sps_id)]
            .as_ref()
            .ok_or_else(|| hevc_error("resolve_pps", "PPS references unknown SPS"))?;
        Ok((pps, sps))
    }
}

fn nal_rbsp<'a>(
    nal: &'a [u8],
    expected: u8,
    operation: &'static str,
) -> VideoResult<crate::rbsp::Rbsp<'a>> {
    if nal.len() < 2 {
        return Err(hevc_error(operation, "NAL header is truncated"));
    }
    let header = NalHeader::parse([nal[0], nal[1]])?;
    if header.unit_type != expected || header.layer_id != 0 {
        return Err(hevc_error(
            operation,
            "unexpected NAL type or nonzero layer",
        ));
    }
    Ok(extract_rbsp(&nal[2..]))
}

fn parse_vps(rbsp: &[u8]) -> VideoResult<VideoParameterSet> {
    let mut bits = BitReader::new(rbsp);
    let id = read_u8(&mut bits, 4)?;
    bits.skip_bits(2)?;
    let max_layers = read_u8(&mut bits, 6)? + 1;
    let max_sub_layers = read_u8(&mut bits, 3)? + 1;
    if max_sub_layers > 7 {
        return Err(hevc_error("parse_vps", "max sub-layers exceeds 7"));
    }
    let temporal_id_nesting = bits.read_bit()?;
    bits.skip_bits(16)?;
    let profile = parse_profile_tier_level(&mut bits, max_sub_layers)?;
    let ordering_all = bits.read_bit()?;
    let (max_dec_pic_buffering, max_num_reorder_pics) =
        parse_ordering(&mut bits, max_sub_layers, ordering_all)?;
    Ok(VideoParameterSet {
        id,
        max_layers,
        max_sub_layers,
        temporal_id_nesting,
        profile,
        max_dec_pic_buffering,
        max_num_reorder_pics,
    })
}

#[expect(
    clippy::too_many_lines,
    reason = "ordered HEVC SPS grammar mirrors FFmpeg"
)]
fn parse_sps(
    rbsp: &[u8],
    vps_table: &[Option<VideoParameterSet>; 16],
) -> VideoResult<SequenceParameterSet> {
    let mut bits = BitReader::new(rbsp);
    let vps_id = read_u8(&mut bits, 4)?;
    if vps_table[usize::from(vps_id)].is_none() {
        return Err(hevc_error("parse_sps", "SPS references unknown VPS"));
    }
    let max_sub_layers = read_u8(&mut bits, 3)? + 1;
    let _temporal_nesting = bits.read_bit()?;
    let profile = parse_profile_tier_level(&mut bits, max_sub_layers)?;
    let id = bounded_u8(bits.read_ue()?, 15, "SPS id")?;
    let chroma_format_idc = bounded_u8(bits.read_ue()?, 3, "chroma format")?;
    if chroma_format_idc == 3 && bits.read_bit()? {
        return Err(unsupported(
            "parse_sps",
            "separate colour planes are unsupported",
        ));
    }
    let width = bits.read_ue()?;
    let height = bits.read_ue()?;
    if width == 0 || height == 0 || width > 16_888 || height > 16_888 {
        return Err(hevc_error("parse_sps", "invalid picture dimensions"));
    }
    let conformance = if bits.read_bit()? {
        let raw = [
            bits.read_ue()?,
            bits.read_ue()?,
            bits.read_ue()?,
            bits.read_ue()?,
        ];
        let sub_width = if matches!(chroma_format_idc, 1 | 2) {
            2
        } else {
            1
        };
        let sub_height = if chroma_format_idc == 1 { 2 } else { 1 };
        let scale = |value: u32, factor: u32| {
            value
                .checked_mul(factor)
                .ok_or_else(|| hevc_error("parse_sps", "conformance window offset overflow"))
        };
        ConformanceWindow {
            left: scale(raw[0], sub_width)?,
            right: scale(raw[1], sub_width)?,
            top: scale(raw[2], sub_height)?,
            bottom: scale(raw[3], sub_height)?,
        }
    } else {
        ConformanceWindow::default()
    };
    if conformance
        .left
        .checked_add(conformance.right)
        .is_none_or(|value| value >= width)
        || conformance
            .top
            .checked_add(conformance.bottom)
            .is_none_or(|value| value >= height)
    {
        return Err(hevc_error(
            "parse_sps",
            "conformance window removes the picture",
        ));
    }
    let bit_depth_luma = bounded_u8(bits.read_ue()? + 8, 16, "luma bit depth")?;
    let bit_depth_chroma = bounded_u8(bits.read_ue()? + 8, 16, "chroma bit depth")?;
    if bit_depth_luma != bit_depth_chroma || !matches!(bit_depth_luma, 8 | 10) {
        return Err(unsupported(
            "parse_sps",
            "only equal 8-bit or 10-bit components are supported",
        ));
    }
    let log2_max_poc_lsb = bounded_u8(bits.read_ue()? + 4, 16, "log2 max POC LSB")?;
    let ordering_all = bits.read_bit()?;
    let (max_dec_pic_buffering, max_num_reorder_pics) =
        parse_ordering(&mut bits, max_sub_layers, ordering_all)?;
    let log2_min_luma_coding_block_size = bounded_u8(bits.read_ue()? + 3, 6, "min CB size")?;
    let diff_cb = bounded_u8(bits.read_ue()?, 3, "CB size difference")?;
    let log2_ctb_size = log2_min_luma_coding_block_size + diff_cb;
    let log2_min_transform_block_size =
        bounded_u8(bits.read_ue()? + 2, 5, "min transform block size")?;
    let log2_diff_max_min_transform_block_size =
        bounded_u8(bits.read_ue()?, 3, "transform block size difference")?;
    let max_transform_hierarchy_depth_inter =
        bounded_u8(bits.read_ue()?, 7, "inter transform hierarchy depth")?;
    let max_transform_hierarchy_depth_intra =
        bounded_u8(bits.read_ue()?, 7, "intra transform hierarchy depth")?;
    let scaling_list_enabled = bits.read_bit()?;
    let scaling_list_data = if scaling_list_enabled && bits.read_bit()? {
        Some(parse_scaling_list_data(&mut bits)?)
    } else {
        None
    };
    let amp_enabled = bits.read_bit()?;
    let sao_enabled = bits.read_bit()?;
    let pcm_enabled = bits.read_bit()?;
    let pcm = if pcm_enabled {
        let bit_depth_luma_minus1 = u8::try_from(bits.read_bits(4)?)
            .map_err(|_| hevc_error("parse_sps", "PCM luma bit depth exceeds u8"))?;
        let bit_depth_chroma_minus1 = u8::try_from(bits.read_bits(4)?)
            .map_err(|_| hevc_error("parse_sps", "PCM chroma bit depth exceeds u8"))?;
        if bit_depth_luma_minus1.max(bit_depth_chroma_minus1) + 1 > bit_depth_luma {
            return Err(hevc_error(
                "parse_sps",
                "PCM bit depth exceeds the sequence bit depth",
            ));
        }
        let log2_min_cb_size_minus3 =
            bounded_u8(bits.read_ue()?, 3, "minimum PCM coding-block size")?;
        let log2_diff_max_min_cb_size =
            bounded_u8(bits.read_ue()?, 3, "PCM coding-block size difference")?;
        let log2_min_cb_size = log2_min_cb_size_minus3 + 3;
        let log2_max_cb_size = log2_min_cb_size
            .checked_add(log2_diff_max_min_cb_size)
            .ok_or_else(|| hevc_error("parse_sps", "maximum PCM block size overflowed"))?;
        if log2_max_cb_size > log2_ctb_size {
            return Err(hevc_error(
                "parse_sps",
                "maximum PCM block size exceeds the coding-tree block size",
            ));
        }
        Some(PcmParameters {
            bit_depth_luma_minus1,
            bit_depth_chroma_minus1,
            log2_min_cb_size_minus3,
            log2_diff_max_min_cb_size,
            loop_filter_disabled: bits.read_bit()?,
        })
    } else {
        None
    };
    let rps_count = bits.read_ue()?;
    if rps_count > 64 {
        return Err(hevc_error("parse_sps", "too many short-term RPS entries"));
    }
    let mut short_term_rps = Vec::new();
    for _ in 0..rps_count {
        let rps = ShortTermRps::decode(&mut bits, &short_term_rps, false)?;
        short_term_rps.push(rps);
    }
    let mut long_term_lsb = Vec::new();
    if bits.read_bit()? {
        let count = bits.read_ue()?;
        if count > 32 {
            return Err(hevc_error("parse_sps", "too many long-term references"));
        }
        for _ in 0..count {
            let lsb = u32::try_from(bits.read_bits(log2_max_poc_lsb)?)
                .map_err(|_| hevc_error("parse_sps", "long-term POC does not fit u32"))?;
            long_term_lsb.push((lsb, bits.read_bit()?));
        }
    }
    let temporal_mvp_enabled = bits.read_bit()?;
    let strong_intra_smoothing_enabled = bits.read_bit()?;
    Ok(SequenceParameterSet {
        id,
        vps_id,
        max_sub_layers,
        profile,
        chroma_format_idc,
        width,
        height,
        conformance,
        bit_depth_luma,
        bit_depth_chroma,
        log2_max_poc_lsb,
        max_dec_pic_buffering,
        max_num_reorder_pics,
        log2_min_luma_coding_block_size,
        log2_ctb_size,
        log2_min_transform_block_size,
        log2_diff_max_min_transform_block_size,
        max_transform_hierarchy_depth_inter,
        max_transform_hierarchy_depth_intra,
        scaling_list_enabled,
        scaling_list_data,
        pcm_enabled,
        pcm,
        amp_enabled,
        sao_enabled,
        temporal_mvp_enabled,
        strong_intra_smoothing_enabled,
        short_term_rps,
        long_term_lsb,
    })
}

#[expect(
    clippy::too_many_lines,
    reason = "ordered HEVC PPS grammar mirrors the syntax and FFmpeg parser"
)]
fn parse_pps(
    rbsp: &[u8],
    sps_table: &[Option<SequenceParameterSet>; 16],
) -> VideoResult<PictureParameterSet> {
    let mut bits = BitReader::new(rbsp);
    let id = bounded_u8(bits.read_ue()?, 63, "PPS id")?;
    let sps_id = bounded_u8(bits.read_ue()?, 15, "SPS id")?;
    let sps = sps_table[usize::from(sps_id)]
        .as_ref()
        .ok_or_else(|| hevc_error("parse_pps", "PPS references unknown SPS"))?;
    let dependent_slice_segments_enabled = bits.read_bit()?;
    let output_flag_present = bits.read_bit()?;
    let extra_slice_header_bits = read_u8(&mut bits, 3)?;
    let sign_data_hiding = bits.read_bit()?;
    let cabac_init_present = bits.read_bit()?;
    let default_ref_count = [
        plus_one_ref(bits.read_ue()?)?,
        plus_one_ref(bits.read_ue()?)?,
    ];
    let init_qp_minus26 = bounded_i8(bits.read_se()?, -26, 25, "initial QP")?;
    let constrained_intra_pred = bits.read_bit()?;
    let transform_skip_enabled = bits.read_bit()?;
    let cu_qp_delta_enabled = bits.read_bit()?;
    let delta_cu_qp_delta_depth = if cu_qp_delta_enabled {
        bounded_u8(bits.read_ue()?, 6, "CU QP delta depth")?
    } else {
        0
    };
    let cb_qp_offset = bounded_i8(bits.read_se()?, -12, 12, "Cb QP offset")?;
    let cr_qp_offset = bounded_i8(bits.read_se()?, -12, 12, "Cr QP offset")?;
    let slice_chroma_qp_offsets_present = bits.read_bit()?;
    let weighted_pred = bits.read_bit()?;
    let weighted_bipred = bits.read_bit()?;
    let transquant_bypass_enabled = bits.read_bit()?;
    let tiles_enabled = bits.read_bit()?;
    let entropy_coding_sync_enabled = bits.read_bit()?;
    let mut tile_columns_minus1 = 0;
    let mut tile_rows_minus1 = 0;
    let mut uniform_spacing = false;
    let mut column_width_minus1 = Vec::new();
    let mut row_height_minus1 = Vec::new();
    let mut loop_filter_across_tiles_enabled = false;
    if tiles_enabled {
        let columns = bits.read_ue()? + 1;
        let rows = bits.read_ue()? + 1;
        if columns > 20 || rows > 22 {
            return Err(hevc_error("parse_pps", "tile grid exceeds HEVC limits"));
        }
        tile_columns_minus1 = u8::try_from(columns - 1)
            .map_err(|_| hevc_error("parse_pps", "tile columns exceed u8"))?;
        tile_rows_minus1 =
            u8::try_from(rows - 1).map_err(|_| hevc_error("parse_pps", "tile rows exceed u8"))?;
        uniform_spacing = bits.read_bit()?;
        if !uniform_spacing {
            for _ in 1..columns {
                column_width_minus1.push(
                    u16::try_from(bits.read_ue()?)
                        .map_err(|_| hevc_error("parse_pps", "tile column width exceeds u16"))?,
                );
            }
            for _ in 1..rows {
                row_height_minus1.push(
                    u16::try_from(bits.read_ue()?)
                        .map_err(|_| hevc_error("parse_pps", "tile row height exceeds u16"))?,
                );
            }
        }
        loop_filter_across_tiles_enabled = bits.read_bit()?;
    }
    let loop_filter_across_slices_enabled = bits.read_bit()?;
    let (deblocking_filter_override_enabled, disable_deblocking_filter, beta_offset, tc_offset) =
        if bits.read_bit()? {
            let override_enabled = bits.read_bit()?;
            let disabled = bits.read_bit()?;
            if disabled {
                (override_enabled, true, 0, 0)
            } else {
                let beta_offset_div2 = bounded_i8(bits.read_se()?, -6, 6, "beta offset")?;
                let tc_offset_div2 = bounded_i8(bits.read_se()?, -6, 6, "tc offset")?;
                (
                    override_enabled,
                    false,
                    beta_offset_div2
                        .checked_mul(2)
                        .ok_or_else(|| hevc_error("parse_pps", "beta offset overflowed"))?,
                    tc_offset_div2
                        .checked_mul(2)
                        .ok_or_else(|| hevc_error("parse_pps", "tc offset overflowed"))?,
                )
            }
        } else {
            (false, false, 0, 0)
        };
    let scaling_list_data = if bits.read_bit()? {
        Some(parse_scaling_list_data(&mut bits)?)
    } else {
        None
    };
    let lists_modification_present = bits.read_bit()?;
    let parallel_merge_level = bounded_u8(bits.read_ue()? + 2, 6, "parallel merge level")?;
    let slice_header_extension_present = bits.read_bit()?;
    if bits.read_bit()? {
        return Err(unsupported(
            "parse_pps",
            "PPS extensions are not in the Main/Main10 scope",
        ));
    }
    let _ = sps;
    Ok(PictureParameterSet {
        id,
        sps_id,
        dependent_slice_segments_enabled,
        output_flag_present,
        extra_slice_header_bits,
        sign_data_hiding,
        cabac_init_present,
        default_ref_count,
        init_qp_minus26,
        constrained_intra_pred,
        transform_skip_enabled,
        cu_qp_delta_enabled,
        cb_qp_offset,
        cr_qp_offset,
        slice_chroma_qp_offsets_present,
        weighted_pred,
        weighted_bipred,
        transquant_bypass_enabled,
        tiles_enabled,
        tile_columns_minus1,
        tile_rows_minus1,
        uniform_spacing,
        column_width_minus1,
        row_height_minus1,
        loop_filter_across_tiles_enabled,
        entropy_coding_sync_enabled,
        delta_cu_qp_delta_depth,
        loop_filter_across_slices_enabled,
        deblocking_filter_override_enabled,
        disable_deblocking_filter,
        beta_offset,
        tc_offset,
        lists_modification_present,
        parallel_merge_level,
        slice_header_extension_present,
        scaling_list_data,
    })
}

fn parse_profile_tier_level(
    bits: &mut BitReader<'_>,
    max_sub_layers: u8,
) -> VideoResult<ProfileTierLevel> {
    let profile_space = read_u8(bits, 2)?;
    let tier = bits.read_bit()?;
    let profile_idc = read_u8(bits, 5)?;
    bits.skip_bits(80)?;
    let level_idc = read_u8(bits, 8)?;
    let count = usize::from(max_sub_layers.saturating_sub(1));
    let mut profile_flags = Vec::with_capacity(count);
    let mut level_flags = Vec::with_capacity(count);
    for _ in 0..count {
        profile_flags.push(bits.read_bit()?);
        level_flags.push(bits.read_bit()?);
    }
    if count > 0 {
        bits.skip_bits((8 - count) * 2)?;
    }
    for (&profile_present, &level_present) in profile_flags.iter().zip(&level_flags) {
        if profile_present {
            bits.skip_bits(88)?;
        }
        if level_present {
            bits.skip_bits(8)?;
        }
    }
    Ok(ProfileTierLevel {
        profile_space,
        tier,
        profile_idc,
        level_idc,
    })
}

fn parse_ordering(
    bits: &mut BitReader<'_>,
    layers: u8,
    all: bool,
) -> VideoResult<(Vec<u8>, Vec<u8>)> {
    let start = if all { 0 } else { layers - 1 };
    let mut buffering = vec![0; usize::from(layers)];
    let mut reorder = vec![0; usize::from(layers)];
    for layer in start..layers {
        buffering[usize::from(layer)] = bounded_u8(bits.read_ue()? + 1, 16, "DPB size")?;
        reorder[usize::from(layer)] = bounded_u8(bits.read_ue()?, 16, "reorder count")?;
        bits.read_ue()?;
    }
    if !all {
        let last = usize::from(layers - 1);
        for index in 0..last {
            buffering[index] = buffering[last];
            reorder[index] = reorder[last];
        }
    }
    Ok((buffering, reorder))
}

fn read_u8(bits: &mut BitReader<'_>, count: u8) -> VideoResult<u8> {
    u8::try_from(bits.read_bits(count)?)
        .map_err(|_| hevc_error("parameter_sets", "field exceeds u8"))
}

fn bounded_u8(value: u32, maximum: u32, field: &'static str) -> VideoResult<u8> {
    if value > maximum {
        return Err(hevc_error(
            "parameter_sets",
            format!("{field} exceeds {maximum}"),
        ));
    }
    u8::try_from(value).map_err(|_| hevc_error("parameter_sets", format!("{field} exceeds u8")))
}

fn bounded_i8(value: i32, minimum: i32, maximum: i32, field: &'static str) -> VideoResult<i8> {
    if !(minimum..=maximum).contains(&value) {
        return Err(hevc_error("parameter_sets", format!("{field} is invalid")));
    }
    i8::try_from(value).map_err(|_| hevc_error("parameter_sets", format!("{field} exceeds i8")))
}

fn plus_one_ref(value: u32) -> VideoResult<u8> {
    bounded_u8(
        value
            .checked_add(1)
            .ok_or_else(|| hevc_error("parse_pps", "reference count overflowed"))?,
        15,
        "reference count",
    )
}
