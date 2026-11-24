use super::dpb::{DecodedPictureBuffer, LongTermReference, SliceType};
use super::parameter_sets::ParameterSets;
use super::rps::ShortTermRps;
use super::slice::ReferenceSliceHeader;
use super::{hevc_error, unsupported};
use crate::VideoResult;

const PICTURE_PARAMETERS_SIZE: usize = 232;
const QUANT_MATRIX_SIZE: usize = 1000;
const HEVC_REFERENCE_COUNT: usize = 15;
const HEVC_RPS_COUNT: usize = 8;

/// One HEVC DPB picture mapped to its decoder surface.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct DxvaReferenceEntry {
    pub(crate) surface_index: u8,
    pub(crate) poc: i32,
    pub(crate) long_term: bool,
}

/// Maps live HEVC DPB pictures to DXVA surface entries in decoder-buffer order.
pub(crate) fn dxva_references(
    dpb: &DecodedPictureBuffer,
    surface_by_picture_id: &[(u64, u8)],
) -> VideoResult<Vec<DxvaReferenceEntry>> {
    let mut references = Vec::new();
    references
        .try_reserve_exact(dpb.pictures().len())
        .map_err(|_| {
            crate::VideoError::new(
                crate::VideoErrorKind::Backend,
                "dxva_references",
                "unable to allocate HEVC DXVA reference table",
            )
        })?;
    for picture in dpb.pictures() {
        let surface_index = surface_by_picture_id
            .iter()
            .find_map(|(picture_id, surface)| (*picture_id == picture.id).then_some(*surface))
            .ok_or_else(|| {
                hevc_error(
                    "dxva_references",
                    "live HEVC DPB picture has no retained decoder surface",
                )
            })?;
        references.push(DxvaReferenceEntry {
            surface_index,
            poc: picture.poc,
            long_term: picture.long_term,
        });
    }
    validate_reference_entries(&references)?;
    Ok(references)
}

/// Converts a parsed HEVC RPS into its three DXVA current-reference surface lists.
pub(crate) fn dxva_reference_sets(
    dpb: &DecodedPictureBuffer,
    surface_by_picture_id: &[(u64, u8)],
    current_poc: i32,
    short_term: &ShortTermRps,
    long_term: &[LongTermReference],
) -> VideoResult<[Vec<u8>; 3]> {
    let negative_count = short_term
        .references
        .iter()
        .filter(|reference| reference.used && reference.delta_poc < 0)
        .count();
    let positive_count = short_term
        .references
        .iter()
        .filter(|reference| reference.used && reference.delta_poc > 0)
        .count();
    let long_count = long_term.iter().filter(|reference| reference.used).count();
    let mut before = Vec::new();
    let mut after = Vec::new();
    let mut long = Vec::new();
    before.try_reserve_exact(negative_count).map_err(|_| {
        crate::VideoError::new(
            crate::VideoErrorKind::Backend,
            "dxva_reference_sets",
            "unable to allocate HEVC negative-reference list",
        )
    })?;
    after.try_reserve_exact(positive_count).map_err(|_| {
        crate::VideoError::new(
            crate::VideoErrorKind::Backend,
            "dxva_reference_sets",
            "unable to allocate HEVC positive-reference list",
        )
    })?;
    long.try_reserve_exact(long_count).map_err(|_| {
        crate::VideoError::new(
            crate::VideoErrorKind::Backend,
            "dxva_reference_sets",
            "unable to allocate HEVC long-term reference list",
        )
    })?;
    for reference in short_term
        .references
        .iter()
        .filter(|reference| reference.used)
    {
        let poc = current_poc
            .checked_add(reference.delta_poc)
            .ok_or_else(|| hevc_error("dxva_reference_sets", "short-term POC overflowed"))?;
        let surface = surface_for_poc(dpb, surface_by_picture_id, poc)?;
        if reference.delta_poc < 0 {
            before.push(surface);
        } else {
            after.push(surface);
        }
    }
    for reference in long_term.iter().filter(|reference| reference.used) {
        long.push(surface_for_poc(dpb, surface_by_picture_id, reference.poc)?);
    }
    if before.len() > 8 || after.len() > 8 || long.len() > 8 {
        return Err(unsupported(
            "dxva_reference_sets",
            "HEVC current RPS set exceeds eight entries",
        ));
    }
    Ok([before, after, long])
}

fn surface_for_poc(
    dpb: &DecodedPictureBuffer,
    surface_by_picture_id: &[(u64, u8)],
    poc: i32,
) -> VideoResult<u8> {
    let picture = dpb
        .pictures()
        .iter()
        .find(|picture| picture.poc == poc)
        .ok_or_else(|| {
            hevc_error(
                "surface_for_poc",
                format!("RPS refers to missing POC {poc}"),
            )
        })?;
    surface_by_picture_id
        .iter()
        .find_map(|(picture_id, surface)| (*picture_id == picture.id).then_some(*surface))
        .ok_or_else(|| hevc_error("surface_for_poc", "RPS picture has no decoder surface"))
}

/// Serializes `FFmpeg`'s `DXVA_HEVC` picture parameters for a first IDR/I slice.
pub(crate) fn idr_picture_parameters(
    vps_nal: &[u8],
    sps_nal: &[u8],
    pps_nal: &[u8],
    idr_slice_nal: &[u8],
    surface: u8,
) -> VideoResult<Vec<u8>> {
    serialize_picture_parameters(
        vps_nal,
        sps_nal,
        pps_nal,
        idr_slice_nal,
        surface,
        0,
        &[],
        [&[], &[], &[]],
        true,
    )
}

/// Serializes HEVC picture parameters for a parsed picture and its mapped live DPB references.
#[expect(
    clippy::too_many_arguments,
    reason = "HEVC picture parameters require separate codec NALs, POC, DPB, and RPS views"
)]
pub(crate) fn picture_parameters(
    vps_nal: &[u8],
    sps_nal: &[u8],
    pps_nal: &[u8],
    slice_nal: &[u8],
    surface: u8,
    current_poc: i32,
    references: &[DxvaReferenceEntry],
    reference_sets: [&[u8]; 3],
) -> VideoResult<Vec<u8>> {
    serialize_picture_parameters(
        vps_nal,
        sps_nal,
        pps_nal,
        slice_nal,
        surface,
        current_poc,
        references,
        reference_sets,
        false,
    )
}

#[expect(
    clippy::too_many_arguments,
    reason = "serialization passes explicit parsed fields to keep ABI output auditable"
)]
#[expect(
    clippy::too_many_lines,
    reason = "the DXVA structure is serialized in SDK field order"
)]
fn serialize_picture_parameters(
    vps_nal: &[u8],
    sps_nal: &[u8],
    pps_nal: &[u8],
    slice_nal: &[u8],
    surface: u8,
    current_poc: i32,
    references: &[DxvaReferenceEntry],
    reference_sets: [&[u8]; 3],
    require_idr: bool,
) -> VideoResult<Vec<u8>> {
    if surface > 0x7f {
        return Err(hevc_error(
            "picture_parameters",
            "DXVA HEVC surface index must fit seven bits",
        ));
    }
    let mut sets = ParameterSets::default();
    sets.insert_vps_nal(vps_nal)?;
    sets.insert_sps_nal(sps_nal)?;
    sets.insert_pps_nal(pps_nal)?;
    let slice = ReferenceSliceHeader::parse_nal(slice_nal, &sets, current_poc)?;
    if require_idr && (!slice.nal.is_idr() || slice.slice_type != SliceType::I) {
        return Err(unsupported(
            "idr_picture_parameters",
            "only HEVC IDR I slices are supported",
        ));
    }
    let (pps, sps) = sets.resolve_pps(u32::from(slice.pps_id))?;
    if !slice.nal.is_idr() {
        let poc_range = 1_i32
            .checked_shl(u32::from(sps.log2_max_poc_lsb))
            .ok_or_else(|| hevc_error("picture_parameters", "POC range overflowed"))?;
        if current_poc.rem_euclid(poc_range)
            != i32::try_from(slice.poc_lsb)
                .map_err(|_| hevc_error("picture_parameters", "POC LSB exceeds i32"))?
        {
            return Err(hevc_error(
                "picture_parameters",
                "current POC does not match the parsed slice POC LSB",
            ));
        }
    }
    if sps.chroma_format_idc != 1 {
        return Err(unsupported(
            "idr_picture_parameters",
            "DXVA path supports 4:2:0 pictures",
        ));
    }

    let mut out = Vec::with_capacity(PICTURE_PARAMETERS_SIZE);
    let min_cb = 1_u32 << sps.log2_min_luma_coding_block_size;
    put_u16(
        &mut out,
        u16::try_from(ceil_div(sps.width, min_cb)?).map_err(|_| {
            hevc_error("idr_picture_parameters", "picture width exceeds DXVA field")
        })?,
        "width in min CBs",
    )?;
    put_u16(
        &mut out,
        u16::try_from(ceil_div(sps.height, min_cb)?).map_err(|_| {
            hevc_error(
                "idr_picture_parameters",
                "picture height exceeds DXVA field",
            )
        })?,
        "height in min CBs",
    )?;
    let format_flags = u16::from(sps.chroma_format_idc)
        | (u16::from(sps.bit_depth_luma - 8) << 3)
        | (u16::from(sps.bit_depth_chroma - 8) << 6)
        | (u16::from(sps.log2_max_poc_lsb - 4) << 9);
    put_u16(&mut out, format_flags, "format flags")?;
    out.push(surface);
    out.push(
        sps.max_dec_pic_buffering
            .last()
            .copied()
            .unwrap_or(1)
            .saturating_sub(1),
    );
    out.extend_from_slice(&[
        sps.log2_min_luma_coding_block_size - 3,
        sps.log2_ctb_size - sps.log2_min_luma_coding_block_size,
        sps.log2_min_transform_block_size - 2,
        sps.log2_diff_max_min_transform_block_size,
        sps.max_transform_hierarchy_depth_inter,
        sps.max_transform_hierarchy_depth_intra,
        u8::try_from(sps.short_term_rps.len())
            .map_err(|_| hevc_error("idr_picture_parameters", "too many short-term RPS entries"))?,
        u8::try_from(sps.long_term_lsb.len())
            .map_err(|_| hevc_error("idr_picture_parameters", "too many long-term references"))?,
        pps.default_ref_count[0].saturating_sub(1),
        pps.default_ref_count[1].saturating_sub(1),
        signed_byte(pps.init_qp_minus26),
        if slice.short_term_rps_from_sps {
            0
        } else {
            u8::try_from(slice.short_term_rps.references.len()).map_err(|_| {
                hevc_error(
                    "picture_parameters",
                    "short-term RPS delta count exceeds the DXVA field",
                )
            })?
        },
    ]);
    put_u16(
        &mut out,
        if slice.short_term_rps_from_sps {
            0
        } else {
            u16::try_from(slice.short_term_rps_bit_size).map_err(|_| {
                hevc_error(
                    "picture_parameters",
                    "short-term RPS syntax exceeds the DXVA bit-size field",
                )
            })?
        },
        "short-term RPS bit size",
    )?;
    put_u16(&mut out, 0, "reserved RPS bits")?;
    let pcm = sps.pcm;
    let coding_flags = u32::from(sps.scaling_list_enabled)
        | (u32::from(sps.amp_enabled) << 1)
        | (u32::from(sps.sao_enabled) << 2)
        | (u32::from(sps.pcm_enabled) << 3)
        | (u32::from(pcm.map_or(0, |params| params.bit_depth_luma_minus1)) << 4)
        | (u32::from(pcm.map_or(0, |params| params.bit_depth_chroma_minus1)) << 8)
        | (u32::from(pcm.map_or(0, |params| params.log2_min_cb_size_minus3)) << 12)
        | (u32::from(pcm.map_or(0, |params| params.log2_diff_max_min_cb_size)) << 14)
        | (u32::from(pcm.is_some_and(|params| params.loop_filter_disabled)) << 16)
        | (u32::from(!sps.long_term_lsb.is_empty()) << 7)
        | (u32::from(sps.temporal_mvp_enabled) << 8)
        | (u32::from(sps.strong_intra_smoothing_enabled) << 9);
    out.extend_from_slice(&coding_flags.to_le_bytes());
    let picture_flags = u32::from(pps.constrained_intra_pred)
        | (u32::from(pps.transform_skip_enabled) << 1)
        | (u32::from(pps.cu_qp_delta_enabled) << 2)
        | (u32::from(pps.slice_chroma_qp_offsets_present) << 3)
        | (u32::from(pps.weighted_pred) << 4)
        | (u32::from(pps.weighted_bipred) << 5)
        | (u32::from(pps.transquant_bypass_enabled) << 6)
        | (u32::from(pps.tiles_enabled) << 7)
        | (u32::from(pps.entropy_coding_sync_enabled) << 8)
        | (u32::from(pps.uniform_spacing) << 9)
        | (u32::from(pps.loop_filter_across_tiles_enabled) << 10)
        | (u32::from(pps.loop_filter_across_slices_enabled) << 11)
        | (u32::from(pps.deblocking_filter_override_enabled) << 12)
        | (u32::from(pps.disable_deblocking_filter) << 13)
        | (u32::from(pps.lists_modification_present) << 14)
        | (u32::from(pps.slice_header_extension_present) << 15)
        | (u32::from(slice.nal.is_irap()) << 16)
        | (u32::from(slice.nal.is_idr()) << 17)
        | (u32::from(slice.nal.is_irap()) << 18);
    out.extend_from_slice(&picture_flags.to_le_bytes());
    out.push(signed_byte(pps.cb_qp_offset));
    out.push(signed_byte(pps.cr_qp_offset));
    out.push(pps.tile_columns_minus1);
    out.push(pps.tile_rows_minus1);
    let width_in_ctbs = ceil_div(sps.width, 1_u32 << sps.log2_ctb_size)?;
    let height_in_ctbs = ceil_div(sps.height, 1_u32 << sps.log2_ctb_size)?;
    write_tile_dimensions(
        &mut out,
        &pps.column_width_minus1,
        pps.tiles_enabled,
        pps.uniform_spacing,
        pps.tile_columns_minus1,
        width_in_ctbs,
        19,
    )?;
    write_tile_dimensions(
        &mut out,
        &pps.row_height_minus1,
        pps.tiles_enabled,
        pps.uniform_spacing,
        pps.tile_rows_minus1,
        height_in_ctbs,
        21,
    )?;
    out.push(pps.delta_cu_qp_delta_depth);
    out.push(signed_byte(pps.beta_offset / 2));
    out.push(signed_byte(pps.tc_offset / 2));
    out.push(pps.parallel_merge_level.saturating_sub(2));
    out.extend_from_slice(&current_poc.to_le_bytes());
    append_reference_list(&mut out, references)?;
    append_reference_sets(&mut out, references, reference_sets)?;
    put_u16(&mut out, 0, "reserved")?;
    put_u16(&mut out, 0, "reserved")?;
    out.extend_from_slice(&1_u32.to_le_bytes()); // DXVA status report feedback number
    if out.len() != PICTURE_PARAMETERS_SIZE {
        return Err(hevc_error(
            "idr_picture_parameters",
            format!(
                "serialized size {} differs from DXVA structure size",
                out.len()
            ),
        ));
    }
    Ok(out)
}

fn append_reference_list(out: &mut Vec<u8>, references: &[DxvaReferenceEntry]) -> VideoResult<()> {
    validate_reference_entries(references)?;
    for index in 0..HEVC_REFERENCE_COUNT {
        if let Some(reference) = references.get(index) {
            out.push(reference.surface_index | (u8::from(reference.long_term) << 7));
        } else {
            out.push(0xff);
        }
    }
    out.push(0); // ReservedBits5
    for index in 0..HEVC_REFERENCE_COUNT {
        out.extend_from_slice(
            &references
                .get(index)
                .map_or(0, |reference| reference.poc)
                .to_le_bytes(),
        );
    }
    Ok(())
}

fn append_reference_sets(
    out: &mut Vec<u8>,
    references: &[DxvaReferenceEntry],
    sets: [&[u8]; 3],
) -> VideoResult<()> {
    validate_reference_entries(references)?;
    for reference_set in sets {
        if reference_set.len() > HEVC_RPS_COUNT {
            return Err(unsupported(
                "append_reference_sets",
                "HEVC reference set exceeds the eight-entry DXVA array",
            ));
        }
        for surface_index in reference_set {
            let position = references
                .iter()
                .position(|reference| reference.surface_index == *surface_index)
                .ok_or_else(|| {
                    hevc_error(
                        "append_reference_sets",
                        "RPS surface is missing from RefPicList",
                    )
                })?;
            out.push(
                u8::try_from(position).map_err(|_| {
                    hevc_error("append_reference_sets", "reference index exceeds u8")
                })?,
            );
        }
        out.resize(out.len() + (HEVC_RPS_COUNT - reference_set.len()), 0xff);
    }
    Ok(())
}

fn validate_reference_entries(references: &[DxvaReferenceEntry]) -> VideoResult<()> {
    if references.len() > HEVC_REFERENCE_COUNT {
        return Err(unsupported(
            "validate_reference_entries",
            "DXVA HEVC supports at most 15 DPB reference entries",
        ));
    }
    if references
        .iter()
        .any(|reference| reference.surface_index > 0x7f)
    {
        return Err(hevc_error(
            "validate_reference_entries",
            "HEVC reference surface index must fit seven bits",
        ));
    }
    for (index, reference) in references.iter().enumerate() {
        if references[..index]
            .iter()
            .any(|previous| previous.surface_index == reference.surface_index)
        {
            return Err(hevc_error(
                "validate_reference_entries",
                "HEVC references cannot share a decoder surface",
            ));
        }
    }
    Ok(())
}

/// Serializes the DXVA2 HEVC inverse-quantization matrix using HEVC default matrices.
pub(crate) fn idr_quant_matrix(
    vps_nal: &[u8],
    sps_nal: &[u8],
    pps_nal: &[u8],
) -> VideoResult<Vec<u8>> {
    let mut sets = ParameterSets::default();
    sets.insert_vps_nal(vps_nal)?;
    let sps = sets.insert_sps_nal(sps_nal)?.clone();
    let pps = sets.insert_pps_nal(pps_nal)?.clone();
    if sps.chroma_format_idc != 1 {
        return Err(unsupported(
            "idr_quant_matrix",
            "only 4:2:0 scaling matrices are supported",
        ));
    }
    let matrices = pps
        .scaling_list_data
        .or(sps.scaling_list_data)
        .unwrap_or_default();
    let mut out = Vec::with_capacity(QUANT_MATRIX_SIZE);
    for matrix in &matrices.matrices[0] {
        out.extend_from_slice(&matrix[..16]);
    }
    for size_id in [1, 2] {
        for matrix in &matrices.matrices[size_id] {
            out.extend_from_slice(matrix);
        }
    }
    for matrix in [0, 3] {
        out.extend_from_slice(&matrices.matrices[3][matrix]);
    }
    out.extend_from_slice(&matrices.dc_coefficients[2]);
    out.extend_from_slice(&[
        matrices.dc_coefficients[3][0],
        matrices.dc_coefficients[3][3],
    ]);
    if out.len() != QUANT_MATRIX_SIZE {
        return Err(hevc_error(
            "idr_quant_matrix",
            "serialized matrix has an invalid size",
        ));
    }
    Ok(out)
}

fn write_tile_dimensions(
    out: &mut Vec<u8>,
    explicit: &[u16],
    enabled: bool,
    uniform: bool,
    minus1: u8,
    total: u32,
    slots: usize,
) -> VideoResult<()> {
    let count = if enabled { usize::from(minus1) + 1 } else { 1 };
    if count > slots {
        return Err(unsupported(
            "idr_picture_parameters",
            "tile count exceeds DXVA array capacity",
        ));
    }
    let mut widths = Vec::with_capacity(count);
    if !enabled || uniform {
        widths.push(total);
    } else {
        let mut sum = 0_u32;
        for width in explicit {
            let value = u32::from(*width) + 1;
            widths.push(value);
            sum = sum
                .checked_add(value)
                .ok_or_else(|| hevc_error("idr_picture_parameters", "tile dimensions overflow"))?;
        }
        let last = total.checked_sub(sum).filter(|v| *v != 0).ok_or_else(|| {
            hevc_error(
                "idr_picture_parameters",
                "explicit tile dimensions exceed picture",
            )
        })?;
        widths.push(last);
    }
    for index in 0..slots {
        let value = widths.get(index).copied().unwrap_or(0);
        put_u16(
            out,
            u16::try_from(value.saturating_sub(u32::from(value != 0)))
                .map_err(|_| hevc_error("idr_picture_parameters", "tile dimension exceeds u16"))?,
            "tile dimension",
        )?;
    }
    Ok(())
}

fn ceil_div(value: u32, divisor: u32) -> VideoResult<u32> {
    value
        .checked_add(divisor - 1)
        .map(|v| v / divisor)
        .ok_or_else(|| hevc_error("idr_picture_parameters", "coded dimensions overflow"))
}

#[expect(
    clippy::unnecessary_wraps,
    reason = "keeps field writing call sites uniform with checked field conversions"
)]
fn put_u16(out: &mut Vec<u8>, value: u16, field: &'static str) -> VideoResult<()> {
    out.extend_from_slice(&value.to_le_bytes());
    let _ = field;
    Ok(())
}

fn signed_byte(value: i8) -> u8 {
    value.to_le_bytes()[0]
}

#[cfg(test)]
mod tests {
    use super::{DxvaReferenceEntry, append_reference_list, append_reference_sets};

    #[test]
    fn serializes_hevc_dpb_and_current_rps_indices() -> crate::VideoResult<()> {
        let references = [
            DxvaReferenceEntry {
                surface_index: 3,
                poc: -2,
                long_term: true,
            },
            DxvaReferenceEntry {
                surface_index: 8,
                poc: 4,
                long_term: false,
            },
        ];
        let mut fields = Vec::new();
        append_reference_list(&mut fields, &references)?;
        assert_eq!(fields.len(), 76);
        assert_eq!(&fields[..3], &[0x83, 8, 0xff]);
        assert_eq!(fields[15], 0);
        assert_eq!(&fields[16..20], &(-2_i32).to_le_bytes());
        assert_eq!(&fields[20..24], &4_i32.to_le_bytes());

        append_reference_sets(&mut fields, &references, [&[3], &[8], &[3]])?;
        assert_eq!(
            &fields[76..84],
            &[0, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff]
        );
        assert_eq!(
            &fields[84..92],
            &[1, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff]
        );
        assert_eq!(
            &fields[92..100],
            &[0, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff]
        );
        Ok(())
    }
}
