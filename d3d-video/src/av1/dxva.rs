use super::av1_error;
use super::frame::{
    Av1CdefParams, Av1DeltaParams, Av1FilmGrainParams, Av1IntraFrameTail, Av1LoopFilterParams,
    Av1QuantizationParams, Av1RestorationParams, Av1SegmentationParams, FrameReferenceHeader,
    IntraFrameTileHeader,
};
use super::sequence::SequenceHeader;
use crate::VideoResult;

const AV1_PICTURE_PARAMETERS_SIZE: usize = 912;

/// Parsed AV1 intra-frame state required to build a DXVA picture record.
pub(crate) struct Av1IntraFrame<'a> {
    pub(crate) sequence: &'a SequenceHeader,
    pub(crate) reference: &'a FrameReferenceHeader,
    pub(crate) frame: &'a IntraFrameTileHeader,
    pub(crate) quantization: &'a Av1QuantizationParams,
    pub(crate) segmentation: &'a Av1SegmentationParams,
    pub(crate) deltas: &'a Av1DeltaParams,
    pub(crate) loop_filter: &'a Av1LoopFilterParams,
    pub(crate) cdef: &'a Av1CdefParams,
    pub(crate) restoration: &'a Av1RestorationParams,
    pub(crate) tail: &'a Av1IntraFrameTail,
}

/// Serializes the Windows SDK `DXVA_PicParams_AV1` structure for supported AV1 pictures.
#[expect(
    clippy::too_many_lines,
    reason = "serialization follows the Windows SDK structure's declared field order"
)]
pub(crate) fn intra_picture_parameters(
    frame: &Av1IntraFrame<'_>,
    current_surface: u8,
) -> VideoResult<Vec<u8>> {
    if frame.reference.show_existing_frame {
        return Err(super::av1_error(
            "intra_picture_parameters",
            "show-existing frames do not submit a decode picture",
        ));
    }
    let sequence = frame.sequence;
    let reference = frame.reference;
    let header = frame.frame;
    let layout = &header.tile_layout;
    if layout.column_widths_sb.len() > 64 || layout.row_heights_sb.len() > 64 {
        return Err(av1_error(
            "intra_picture_parameters",
            "tile layout exceeds DXVA's fixed arrays",
        ));
    }
    let mut out = Vec::with_capacity(AV1_PICTURE_PARAMETERS_SIZE);
    push_u32(&mut out, u32::from(header.upscaled_width));
    push_u32(&mut out, u32::from(header.height));
    push_u32(&mut out, u32::from(sequence.max_frame_width));
    push_u32(&mut out, u32::from(sequence.max_frame_height));
    out.extend_from_slice(&[
        current_surface,
        header.superres_denom,
        sequence.color.bit_depth,
        sequence.profile,
    ]);

    out.push(to_u8(layout.column_widths_sb.len(), "tile columns")?);
    out.push(to_u8(layout.row_heights_sb.len(), "tile rows")?);
    push_u16(&mut out, layout.context_update_tile_id);
    write_u16_array(&mut out, &layout.column_widths_sb, 64);
    write_u16_array(&mut out, &layout.row_heights_sb, 64);

    let coding_flags = u32::from(sequence.use_128x128_superblock)
        | (u32::from(sequence.enable_intra_edge_filter) << 1)
        | (u32::from(sequence.enable_interintra_compound) << 2)
        | (u32::from(sequence.enable_masked_compound) << 3)
        | (u32::from(frame.tail.allow_warped_motion) << 4)
        | (u32::from(sequence.enable_dual_filter) << 5)
        | (u32::from(sequence.enable_joint_compound) << 6)
        | (u32::from(reference.allow_screen_content_tools) << 7)
        | (u32::from(reference.force_integer_mv) << 8)
        | (u32::from(sequence.enable_cdef) << 9)
        | (u32::from(sequence.enable_restoration) << 10)
        | (u32::from(sequence.film_grain_params_present) << 11)
        | (u32::from(header.allow_intrabc) << 12)
        | (u32::from(header.allow_high_precision_mv) << 13)
        | (u32::from(header.is_motion_mode_switchable) << 14)
        | (u32::from(sequence.enable_filter_intra) << 15)
        | (u32::from(header.disable_frame_end_update_cdf) << 16)
        | (u32::from(reference.disable_cdf_update) << 17)
        | (u32::from(frame.tail.reference_select) << 18)
        | (u32::from(frame.tail.skip_mode_present) << 19)
        | (u32::from(frame.tail.reduced_tx_set) << 20)
        | (u32::from(header.superres_denom != 8) << 21)
        | (u32::from(frame.tail.tx_mode & 1) << 22)
        | (u32::from(frame.tail.tx_mode & 2) << 22)
        | (u32::from(header.use_ref_frame_mvs) << 24)
        | (u32::from(sequence.enable_ref_frame_mvs) << 25)
        | (1 << 26);
    push_u32(&mut out, coding_flags);

    let subsampled_420 = sequence.profile == 0 && !sequence.color.monochrome;
    let format_flags = reference.frame_type as u8
        | (u8::from(reference.show_frame) << 2)
        | (u8::from(reference.showable_frame) << 3)
        | (u8::from(subsampled_420) << 4)
        | (u8::from(subsampled_420) << 5)
        | (u8::from(sequence.color.monochrome) << 6);
    out.push(format_flags);
    out.push(reference.primary_ref_frame);
    out.push(to_u8(u64::from(reference.order_hint), "order hint")?);
    out.push(sequence.order_hint_bits);
    for logical_reference in 0..7 {
        if reference.frame_type.is_intra() {
            out.extend_from_slice(&[0; 32]); // Width, height, and global-motion matrix.
            out.push(0); // GlobalMotionFlags.
            out.push(0xff); // No reference surface is associated with an intra frame.
            push_u16(&mut out, 0);
        } else {
            let map_index = reference.ref_frame_idx[logical_reference];
            let slot = reference.next_slots.get(map_index)?;
            let motion = frame.tail.global_motion[logical_reference];
            push_u32(&mut out, u32::from(slot.width));
            push_u32(&mut out, u32::from(slot.height));
            for value in motion.params {
                push_u32(&mut out, u32::from_le_bytes(value.to_le_bytes()));
            }
            out.push(u8::from(motion.invalid) | (motion.model << 1));
            out.push(map_index);
            push_u16(&mut out, 0);
        }
    }
    if reference.frame_type.is_intra() {
        out.extend_from_slice(&[0xff; 8]);
    } else {
        for index in 0..8_u8 {
            let texture_index = if reference.next_slots.contains(index) {
                let slot = reference.next_slots.get(index)?;
                u8::try_from(slot.surface_id).map_err(|_| {
                    av1_error(
                        "intra_picture_parameters",
                        "AV1 reference surface index exceeds u8",
                    )
                })?
            } else {
                0xff
            };
            out.push(texture_index);
        }
    }

    out.extend_from_slice(&frame.loop_filter.levels[..2]);
    out.push(frame.loop_filter.levels[2]);
    out.push(frame.loop_filter.levels[3]);
    out.push(frame.loop_filter.sharpness);
    let loop_control = u8::from(frame.loop_filter.delta_enabled)
        | (u8::from(frame.loop_filter.delta_update) << 1)
        | (u8::from(frame.deltas.delta_lf_multi) << 2)
        | (u8::from(frame.deltas.delta_lf_present) << 3);
    out.push(loop_control);
    for delta in frame.loop_filter.ref_deltas {
        out.push(delta.to_le_bytes()[0]);
    }
    for delta in frame.loop_filter.mode_deltas {
        out.push(delta.to_le_bytes()[0]);
    }
    out.push(frame.deltas.delta_lf_res);
    let restoration_types = frame.restoration.types;
    out.extend_from_slice(&restoration_types);
    let uses_restoration = restoration_types.iter().any(|kind| *kind != 0);
    let luma_unit_size = if uses_restoration {
        6 + u16::from(frame.restoration.unit_shift)
    } else {
        8
    };
    let chroma_unit_size = if uses_restoration {
        6 + u16::from(frame.restoration.unit_shift) - u16::from(frame.restoration.uv_shift)
    } else {
        8
    };
    push_u16(&mut out, luma_unit_size);
    push_u16(&mut out, chroma_unit_size);
    push_u16(&mut out, chroma_unit_size);
    push_u16(&mut out, 0);

    let quant_control = u8::from(frame.deltas.delta_q_present) | (frame.deltas.delta_q_res << 1);
    out.push(quant_control);
    out.push(frame.quantization.base_qindex);
    for delta in [
        frame.quantization.y_dc_delta_q,
        frame.quantization.u_dc_delta_q,
        frame.quantization.v_dc_delta_q,
        frame.quantization.u_ac_delta_q,
        frame.quantization.v_ac_delta_q,
    ] {
        out.push(delta.to_le_bytes()[0]);
    }
    out.extend_from_slice(&[
        frame.quantization.qm_y,
        frame.quantization.qm_u,
        frame.quantization.qm_v,
    ]);
    push_u16(&mut out, 0);

    out.push(frame.cdef.damping | (frame.cdef.bits << 2));
    for strengths in frame.cdef.y_strengths {
        out.push(pack_cdef_strength(strengths));
    }
    for strengths in frame.cdef.uv_strengths {
        out.push(pack_cdef_strength(strengths));
    }
    out.push(if reference.frame_type.is_intra() {
        0
    } else {
        header.interpolation_filter
    });

    let segmentation_control = u8::from(frame.segmentation.enabled)
        | (u8::from(frame.segmentation.update_map) << 1)
        | (u8::from(frame.segmentation.update_data) << 2)
        | (u8::from(frame.segmentation.temporal_update) << 3);
    out.push(segmentation_control);
    out.extend_from_slice(&[0; 3]);
    for features in &frame.segmentation.feature_enabled {
        let mask = features
            .iter()
            .enumerate()
            .fold(0_u8, |mask, (feature, enabled)| {
                mask | (u8::from(*enabled) << feature)
            });
        out.push(mask);
    }
    for segment in frame.segmentation.feature_data {
        for value in segment {
            push_i16(&mut out, value);
        }
    }

    write_film_grain(
        &mut out,
        &frame.tail.film_grain,
        sequence.color.matrix_coefficients == 0,
    );
    push_u32(&mut out, 0); // Reserved32Bits.
    push_u32(&mut out, 0); // StatusReportFeedbackNumber: FFmpeg leaves this unset.
    if out.len() != AV1_PICTURE_PARAMETERS_SIZE {
        return Err(av1_error(
            "intra_picture_parameters",
            format!(
                "DXVA AV1 record has {} bytes, expected {AV1_PICTURE_PARAMETERS_SIZE}",
                out.len()
            ),
        ));
    }
    Ok(out)
}

fn write_film_grain(out: &mut Vec<u8>, grain: &Av1FilmGrainParams, matrix_is_identity: bool) {
    let flags = u16::from(grain.apply_grain)
        | (u16::from(grain.scaling_shift_minus8) << 1)
        | (u16::from(grain.chroma_scaling_from_luma) << 3)
        | (u16::from(grain.ar_coeff_lag) << 4)
        | (u16::from(grain.ar_coeff_shift_minus6) << 6)
        | (u16::from(grain.grain_scale_shift) << 8)
        | (u16::from(grain.overlap_flag) << 10)
        | (u16::from(grain.clip_to_restricted_range) << 11)
        | (u16::from(matrix_is_identity) << 12);
    push_u16(out, flags);
    push_u16(out, grain.grain_seed);
    write_point_array(out, &grain.scaling_points_y);
    out.push(grain.num_y_points);
    write_point_array(out, &grain.scaling_points_cb);
    out.push(grain.num_cb_points);
    write_point_array(out, &grain.scaling_points_cr);
    out.push(grain.num_cr_points);
    out.extend_from_slice(&grain.ar_coeffs_y);
    out.extend_from_slice(&grain.ar_coeffs_cb);
    out.extend_from_slice(&grain.ar_coeffs_cr);
    out.extend_from_slice(&[
        grain.cb_mult,
        grain.cb_luma_mult,
        grain.cr_mult,
        grain.cr_luma_mult,
        0,
    ]);
    push_u16(out, grain.cb_offset);
    push_u16(out, grain.cr_offset);
}

fn write_point_array<const N: usize>(out: &mut Vec<u8>, points: &[[u8; 2]; N]) {
    for point in points {
        out.extend_from_slice(point);
    }
}

fn write_u16_array(out: &mut Vec<u8>, values: &[u16], capacity: usize) {
    for index in 0..capacity {
        push_u16(out, values.get(index).copied().unwrap_or(0));
    }
}

fn pack_cdef_strength([primary, secondary]: [u8; 2]) -> u8 {
    primary | (secondary << 6)
}

fn push_u16(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn push_i16(out: &mut Vec<u8>, value: i16) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn push_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn to_u8(value: impl TryInto<u8>, field: &'static str) -> VideoResult<u8> {
    value
        .try_into()
        .map_err(|_| av1_error("intra_picture_parameters", format!("{field} exceeds u8")))
}
