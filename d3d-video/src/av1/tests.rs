use super::frame::{
    Av1CdefParams, Av1DeltaParams, Av1IntraFrameTail, Av1LoopFilterParams, Av1QuantizationParams,
    Av1RestorationParams, Av1SegmentationParams, FrameReferenceHeader, FrameType,
    IntraFrameTileHeader,
};
use super::refs::{ReferenceSlot, ReferenceSlots, derive_short_refs, relative_distance};
use super::sequence::SequenceHeader;
use super::tile::{TileGroupTracker, TileLayout, collect_tile_groups, parse_tile_group};
use crate::VideoResult;

#[derive(Default)]
struct BitWriter {
    bytes: Vec<u8>,
    bit_len: usize,
}

impl BitWriter {
    fn bit(&mut self, value: bool) {
        self.bits(u64::from(value), 1);
    }

    fn bits(&mut self, value: u64, count: u8) {
        for shift in (0..count).rev() {
            let byte = self.bit_len / 8;
            if byte == self.bytes.len() {
                self.bytes.push(0);
            }
            if value & (1_u64 << shift) != 0 {
                self.bytes[byte] |= 1 << (7 - self.bit_len % 8);
            }
            self.bit_len += 1;
        }
    }

    fn obu(self, kind: u8) -> Vec<u8> {
        assert!(self.bytes.len() < 128);
        let size = u8::try_from(self.bytes.len()).unwrap_or_default();
        let mut obu = vec![(kind << 3) | 2, size];
        obu.extend(self.bytes);
        obu
    }

    fn sequence_obu(self) -> Vec<u8> {
        self.obu(1)
    }

    fn ns(&mut self, value: u32, maximum: u32) {
        let width = maximum.bit_width();
        let marker = (1_u32 << width) - maximum;
        if value < marker {
            self.bits(
                u64::from(value),
                u8::try_from(width - 1).unwrap_or_default(),
            );
        } else {
            let encoded = value + marker;
            self.bits(
                u64::from(encoded >> 1),
                u8::try_from(width - 1).unwrap_or_default(),
            );
            self.bit(encoded & 1 != 0);
        }
    }
}

#[test]
fn wraps_relative_order_hint_distance() {
    assert_eq!(relative_distance(true, 4, 1, 14), 3);
    assert_eq!(relative_distance(true, 4, 14, 1), -3);
    assert_eq!(relative_distance(false, 4, 14, 1), 0);
}

#[test]
fn atomically_refreshes_and_invalidates_slots() -> VideoResult<()> {
    let mut slots = ReferenceSlots::default();
    let frame = ReferenceSlot {
        frame_id: 14,
        order_hint: 7,
        frame_type: 1,
        surface_id: 99,
        width: 64,
        height: 64,
        render_width: 64,
        render_height: 64,
        film_grain: Default::default(),
    };
    slots.commit(0b1000_0001, frame);
    assert_eq!(slots.get(0)?, frame);
    assert_eq!(slots.get(7)?, frame);
    slots.invalidate_stale(1, 4, 3);
    assert_eq!(slots.get(0)?, frame);
    slots.invalidate_stale(7, 4, 3);
    assert!(slots.get(0).is_err());
    Ok(())
}

#[test]
fn derives_short_signalled_reference_order() -> VideoResult<()> {
    let mut slots = ReferenceSlots::default();
    for (index, hint) in [7_u32, 4, 9, 11, 10, 6, 5, 3].into_iter().enumerate() {
        let index = u8::try_from(index).unwrap_or_default();
        slots.commit(
            1 << index,
            ReferenceSlot {
                frame_id: u32::from(index),
                order_hint: hint,
                frame_type: FrameType::Inter as u8,
                surface_id: u64::from(index),
                width: 64,
                height: 64,
                render_width: 64,
                render_height: 64,
                film_grain: Default::default(),
            },
        );
    }
    assert_eq!(
        derive_short_refs(&slots, 8, 4, 0, 1)?,
        [0, 5, 6, 1, 2, 4, 3]
    );
    Ok(())
}

#[test]
fn parses_reduced_main_sequence_header() -> VideoResult<()> {
    let mut bits = BitWriter::default();
    bits.bits(0, 3); // Main profile.
    bits.bit(true); // still_picture
    bits.bit(true); // reduced_still_picture_header
    bits.bits(8, 5); // seq_level_idx[0]
    bits.bits(11, 4); // frame_width_bits_minus_1
    bits.bits(10, 4); // frame_height_bits_minus_1
    bits.bits(1919, 12);
    bits.bits(1079, 11);
    bits.bit(false); // use_128x128_superblock
    bits.bit(true); // enable_filter_intra
    bits.bit(true); // enable_intra_edge_filter
    bits.bit(false); // enable_superres
    bits.bit(true); // enable_cdef
    bits.bit(true); // enable_restoration
    bits.bit(false); // high_bitdepth
    bits.bit(false); // monochrome
    bits.bit(false); // color_description_present
    bits.bit(false); // color_range
    bits.bits(0, 2); // chroma_sample_position
    bits.bit(false); // separate_uv_delta_q
    bits.bit(true); // film_grain_params_present

    let header = SequenceHeader::parse_obu(&bits.sequence_obu())?;
    assert_eq!(header.profile, 0);
    assert!(header.still_picture);
    assert!(header.reduced_still_picture_header);
    assert_eq!(
        (header.max_frame_width, header.max_frame_height),
        (1920, 1080)
    );
    assert_eq!(header.order_hint_bits, 0);
    assert_eq!(header.color.bit_depth, 8);
    assert!(header.film_grain_params_present);
    Ok(())
}

#[test]
fn parses_main10_sequence_header_with_frame_ids() -> VideoResult<()> {
    let mut bits = BitWriter::default();
    bits.bits(0, 3);
    bits.bit(false); // still_picture
    bits.bit(false); // reduced_still_picture_header
    bits.bit(false); // timing_info_present_flag
    bits.bit(false); // initial_display_delay_present_flag
    bits.bits(0, 5); // operating_points_cnt_minus_1
    bits.bits(0, 12); // operating_point_idc[0]
    bits.bits(8, 5); // seq_level_idx[0]
    bits.bit(false); // seq_tier[0]
    bits.bits(7, 4); // width uses 8 bits
    bits.bits(7, 4); // height uses 8 bits
    bits.bits(127, 8);
    bits.bits(71, 8);
    bits.bit(true); // frame_id_numbers_present_flag
    bits.bits(2, 4); // delta_frame_id_length_minus_2
    bits.bits(1, 3); // additional_frame_id_length_minus_1
    bits.bit(true); // use_128x128_superblock
    bits.bit(true); // enable_filter_intra
    bits.bit(true); // enable_intra_edge_filter
    bits.bit(true); // enable_interintra_compound
    bits.bit(false); // enable_masked_compound
    bits.bit(true); // enable_warped_motion
    bits.bit(true); // enable_dual_filter
    bits.bit(true); // enable_order_hint
    bits.bit(true); // enable_joint_compound
    bits.bit(true); // enable_ref_frame_mvs
    bits.bit(true); // seq_choose_screen_content_tools
    bits.bit(true); // seq_choose_integer_mv
    bits.bits(4, 3); // order_hint_bits_minus_1
    bits.bit(true); // enable_superres
    bits.bit(true); // enable_cdef
    bits.bit(true); // enable_restoration
    bits.bit(true); // high_bitdepth
    bits.bit(false); // monochrome
    bits.bit(false); // color_description_present
    bits.bit(false); // color_range
    bits.bits(0, 2); // chroma_sample_position
    bits.bit(true); // separate_uv_delta_q
    bits.bit(false); // film_grain_params_present

    let header = SequenceHeader::parse_obu(&bits.sequence_obu())?;
    assert_eq!((header.max_frame_width, header.max_frame_height), (128, 72));
    assert!(header.frame_id_numbers_present);
    assert_eq!(
        (header.delta_frame_id_length, header.frame_id_length),
        (4, 6)
    );
    assert_eq!(header.order_hint_bits, 5);
    assert_eq!(header.color.bit_depth, 10);
    assert!(header.color.separate_uv_delta_q);
    Ok(())
}

#[test]
fn parses_reduced_key_frame_reference_header() -> VideoResult<()> {
    let sequence = reduced_sequence()?;
    let mut bits = BitWriter::default();
    bits.bit(false); // disable_cdf_update
    bits.bit(false); // allow_screen_content_tools

    let header =
        FrameReferenceHeader::parse_obu(&bits.obu(3), &sequence, &ReferenceSlots::default())?;
    assert_eq!(header.frame_type, FrameType::Key);
    assert!(header.show_frame);
    assert!(header.error_resilient_mode);
    assert_eq!(header.refresh_frame_flags, 0xff);
    Ok(())
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one synthetic AV1 frame exercises consecutive header parser stages"
)]
fn parses_reduced_key_frame_dimensions_and_tile_layout() -> VideoResult<()> {
    let sequence_obu = reduced_sequence_obu();
    let sequence = SequenceHeader::parse_obu(&sequence_obu)?;
    let mut bits = BitWriter::default();
    bits.bit(false); // disable_cdf_update
    bits.bit(false); // allow_screen_content_tools
    bits.bit(false); // render_and_frame_size_different
    bits.bit(false); // disable_frame_end_update_cdf
    bits.bit(false); // non-uniform tile spacing
    bits.ns(1, 2); // one column spanning the frame
    bits.ns(1, 2); // one row spanning the frame
    bits.bits(93, 8); // base_q_idx
    bits.bit(true); // delta_q_y_dc is coded
    bits.bits(0b111_1101, 7); // -3
    bits.bit(false); // delta_q_u_dc is zero
    bits.bit(true); // delta_q_u_ac is coded
    bits.bits(0b111_1111, 7); // -1
    bits.bit(true); // using_qmatrix
    bits.bits(3, 4); // qm_y
    bits.bits(5, 4); // qm_u; qm_v is inferred
    bits.bit(true); // segmentation_enabled
    bits.bit(true); // segment 0: ALT_Q enabled
    bits.bits(0b1_1111_1011, 9); // ALT_Q = -5
    bits.bit(true); // segment 0: ALT_LF_Y_V enabled
    bits.bits(0b111_1110, 7); // ALT_LF_Y_V = -2
    for _ in 0..6 {
        bits.bit(false);
    }
    for _ in 1..8 {
        for _ in 0..8 {
            bits.bit(false);
        }
    }
    bits.bit(true); // delta_q_present
    bits.bits(2, 2); // delta_q_res
    bits.bit(true); // delta_lf_present
    bits.bits(1, 2); // delta_lf_res
    bits.bit(true); // delta_lf_multi
    bits.bits(10, 6); // loop_filter_level[0]
    bits.bits(12, 6); // loop_filter_level[1]
    bits.bits(4, 6); // loop_filter_level_u
    bits.bits(3, 6); // loop_filter_level_v
    bits.bits(5, 3); // loop_filter_sharpness
    bits.bit(true); // loop_filter_delta_enabled
    bits.bit(true); // loop_filter_delta_update
    bits.bit(true); // update_ref_delta[0]
    bits.bits(0b111_1110, 7); // ref_delta[0] = -2
    for _ in 1..8 {
        bits.bit(false);
    }
    bits.bit(true); // update_mode_delta[0]
    bits.bits(3, 7); // mode_delta[0] = 3
    bits.bit(false); // update_mode_delta[1]
    bits.bits(2, 2); // cdef_damping_minus_3
    bits.bits(1, 2); // cdef_bits
    bits.bits(5, 4); // y primary 0
    bits.bits(2, 2); // y secondary 0
    bits.bits(9, 4); // uv primary 0
    bits.bits(1, 2); // uv secondary 0
    bits.bits(3, 4); // y primary 1
    bits.bits(0, 2); // y secondary 1
    bits.bits(4, 4); // uv primary 1
    bits.bits(2, 2); // uv secondary 1
    bits.bits(2, 2); // luma restoration: Wiener
    bits.bits(1, 2); // chroma U restoration: switchable
    bits.bits(0, 2); // chroma V restoration: none
    bits.bit(true); // restoration unit shift increment
    bits.bit(false); // stop at shift 1
    bits.bit(true); // chroma restoration unit shift
    bits.bit(false); // tx_mode increment stops at TX_MODE_LARGEST
    bits.bit(true); // reduced_tx_set
    bits.bit(true); // apply_grain
    bits.bits(0x1234, 16); // grain_seed
    bits.bits(1, 4); // one luma scaling point
    bits.bits(16, 8);
    bits.bits(128, 8);
    bits.bit(false); // chroma_scaling_from_luma
    bits.bits(1, 4); // one Cb point
    bits.bits(20, 8);
    bits.bits(64, 8);
    bits.bits(0, 4); // no Cr points
    bits.bits(1, 2); // grain_scaling_minus_8
    bits.bits(0, 2); // ar_coeff_lag
    bits.bits(128, 8); // one Cb AR coefficient
    bits.bits(2, 2); // ar_coeff_shift_minus_6
    bits.bits(1, 2); // grain_scale_shift
    bits.bits(7, 8); // cb_mult
    bits.bits(9, 8); // cb_luma_mult
    bits.bits(257, 9); // cb_offset
    bits.bit(true); // overlap_flag
    bits.bit(false); // clip_to_restricted_range
    let obu = bits.obu(3);
    let reference = FrameReferenceHeader::parse_obu(&obu, &sequence, &ReferenceSlots::default())?;
    let frame = IntraFrameTileHeader::parse_obu(&obu, &sequence, &reference)?;
    assert_eq!((frame.width, frame.height), (128, 72));
    assert_eq!((frame.upscaled_width, frame.superres_denom), (128, 8));
    assert_eq!((frame.render_width, frame.render_height), (128, 72));
    assert!(!frame.allow_intrabc);
    assert!(!frame.disable_frame_end_update_cdf);
    assert_eq!(frame.tile_layout.column_widths_sb, [2]);
    assert_eq!(frame.tile_layout.row_heights_sb, [2]);
    let quant = Av1QuantizationParams::parse_obu(&obu, &sequence, &frame)?;
    assert_eq!(quant.base_qindex, 93);
    assert_eq!(quant.y_dc_delta_q, -3);
    assert_eq!(quant.u_dc_delta_q, 0);
    assert_eq!(quant.u_ac_delta_q, -1);
    assert_eq!(quant.v_dc_delta_q, 0);
    assert_eq!(quant.v_ac_delta_q, -1);
    assert_eq!((quant.qm_y, quant.qm_u, quant.qm_v), (3, 5, 5));
    let segmentation = Av1SegmentationParams::parse_obu(&obu, &quant)?;
    assert!(segmentation.enabled);
    assert!(segmentation.update_map);
    assert!(!segmentation.temporal_update);
    assert!(segmentation.update_data);
    assert!(segmentation.feature_enabled[0][0]);
    assert!(segmentation.feature_enabled[0][1]);
    assert_eq!(segmentation.feature_data[0][0], -5);
    assert_eq!(segmentation.feature_data[0][1], -2);
    assert!(!segmentation.feature_enabled[7][7]);
    let deltas = Av1DeltaParams::parse_obu(&obu, &quant, &segmentation, frame.allow_intrabc)?;
    assert!(deltas.delta_q_present);
    assert_eq!(deltas.delta_q_res, 2);
    assert!(deltas.delta_lf_present);
    assert_eq!(deltas.delta_lf_res, 1);
    assert!(deltas.delta_lf_multi);
    let loop_filter =
        Av1LoopFilterParams::parse_obu(&obu, &sequence, &frame, &quant, &segmentation, &deltas)?;
    assert_eq!(loop_filter.levels, [10, 12, 4, 3]);
    assert_eq!(loop_filter.sharpness, 5);
    assert!(loop_filter.delta_enabled);
    assert!(loop_filter.delta_update);
    assert_eq!(loop_filter.ref_deltas[0], -2);
    assert_eq!(loop_filter.mode_deltas, [3, 0]);
    let cdef =
        Av1CdefParams::parse_obu(&obu, &sequence, &frame, &quant, &segmentation, &loop_filter)?;
    assert_eq!(cdef.damping, 2);
    assert_eq!(cdef.bits, 1);
    assert_eq!(cdef.y_strengths[0], [5, 2]);
    assert_eq!(cdef.uv_strengths[0], [9, 1]);
    assert_eq!(cdef.y_strengths[1], [3, 0]);
    assert_eq!(cdef.uv_strengths[1], [4, 2]);
    let restoration =
        Av1RestorationParams::parse_obu(&obu, &sequence, &frame, &quant, &segmentation, &cdef)?;
    assert_eq!(restoration.types, [2, 1, 0]);
    assert_eq!(restoration.unit_shift, 1);
    assert_eq!(restoration.uv_shift, 1);
    let tail = Av1IntraFrameTail::parse_obu(
        &obu,
        &sequence,
        &reference,
        &quant,
        &segmentation,
        &restoration,
    )?;
    assert_eq!(tail.tx_mode, 1);
    assert!(tail.reduced_tx_set);
    assert!(tail.film_grain.apply_grain);
    assert!(tail.film_grain.update_grain);
    assert_eq!(tail.film_grain.grain_seed, 0x1234);
    assert_eq!(tail.film_grain.scaling_points_y[0], [16, 128]);
    assert_eq!(tail.film_grain.scaling_points_cb[0], [20, 64]);
    assert_eq!(tail.film_grain.num_cr_points, 0);
    assert_eq!(tail.film_grain.ar_coeffs_cb[0], 128);
    assert_eq!(tail.film_grain.cb_offset, 257);
    assert!(tail.film_grain.overlap_flag);
    let dxva = super::dxva::intra_picture_parameters(
        &super::dxva::Av1IntraFrame {
            sequence: &sequence,
            reference: &reference,
            frame: &frame,
            quantization: &quant,
            segmentation: &segmentation,
            deltas: &deltas,
            loop_filter: &loop_filter,
            cdef: &cdef,
            restoration: &restoration,
            tail: &tail,
        },
        4,
    )?;
    assert_eq!(dxva.len(), 912);
    assert_eq!(dxva[0..4], [128, 0, 0, 0]);
    assert_eq!(dxva[4..8], [72, 0, 0, 0]);
    assert_eq!(dxva[16], 4);
    assert_eq!(dxva[20..22], [1, 1]);
    assert_eq!(dxva[24..26], [2, 0]);
    assert_eq!(dxva[908..912], [0; 4]);
    let mut superres_frame = frame.clone();
    superres_frame.width = 102;
    superres_frame.superres_denom = 10;
    let superres_dxva = super::dxva::intra_picture_parameters(
        &super::dxva::Av1IntraFrame {
            sequence: &sequence,
            reference: &reference,
            frame: &superres_frame,
            quantization: &quant,
            segmentation: &segmentation,
            deltas: &deltas,
            loop_filter: &loop_filter,
            cdef: &cdef,
            restoration: &restoration,
            tail: &tail,
        },
        4,
    )?;
    assert_eq!(superres_dxva[0..4], [128, 0, 0, 0]);
    assert_eq!(superres_dxva[17], 10);
    let tile_groups = [&[0x22, 3, 0x80, 0xaa, 0xbb][..]];
    let built = super::build_intra_dxva_input(&sequence_obu, &obu, &tile_groups, 5)?;
    assert_eq!(built.picture_parameters.len(), 912);
    assert_eq!((built.width, built.height), (128, 72));
    assert_eq!(built.picture_parameters[16], 5);
    assert_eq!(built.tile_payload, [0xaa, 0xbb]);
    assert_eq!(built.tiles.len(), 1);
    assert_eq!(built.tiles[0].data_offset, 0);
    assert_eq!(built.tiles[0].data_size, 2);
    Ok(())
}

#[test]
fn parses_superresolution_and_distinct_render_dimensions() -> VideoResult<()> {
    let mut sequence = reduced_sequence()?;
    sequence.enable_superres = true;
    let mut bits = BitWriter::default();
    bits.bit(false); // disable_cdf_update
    bits.bit(false); // allow_screen_content_tools
    bits.bit(true); // use_superres
    bits.bits(1, 3); // denominator 10: coded width 102, output width 128
    bits.bit(true); // render_and_frame_size_different
    bits.bits(191, 16); // render width 192
    bits.bits(107, 16); // render height 108
    bits.bit(false); // disable_frame_end_update_cdf
    bits.bit(false); // nonuniform tiles
    bits.ns(1, 2); // one column
    bits.ns(1, 2); // one row
    let obu = bits.obu(3);
    let reference = FrameReferenceHeader::parse_obu(&obu, &sequence, &ReferenceSlots::default())?;
    let frame = IntraFrameTileHeader::parse_obu(&obu, &sequence, &reference)?;
    assert_eq!(
        (frame.width, frame.upscaled_width, frame.height),
        (102, 128, 72)
    );
    assert_eq!((frame.render_width, frame.render_height), (192, 108));
    Ok(())
}

#[test]
fn parses_explicit_inter_references_and_frame_ids() -> VideoResult<()> {
    let sequence = main10_sequence()?;
    let mut slots = ReferenceSlots::default();
    for index in 0..8_u8 {
        slots.commit(
            1 << index,
            ReferenceSlot {
                frame_id: 9,
                order_hint: u32::from(index),
                frame_type: FrameType::Inter as u8,
                surface_id: u64::from(index),
                width: 64,
                height: 64,
                render_width: 64,
                render_height: 64,
                film_grain: Default::default(),
            },
        );
    }
    let mut bits = BitWriter::default();
    bits.bit(false); // show_existing_frame
    bits.bits(FrameType::Inter as u64, 2);
    bits.bit(true); // show_frame
    bits.bit(false); // error_resilient_mode
    bits.bit(false); // disable_cdf_update
    bits.bit(false); // allow_screen_content_tools
    bits.bits(10, 6); // current_frame_id
    bits.bit(false); // frame_size_override_flag
    bits.bits(12, 5); // order_hint
    bits.bits(0, 3); // primary_ref_frame
    bits.bits(1, 8); // refresh_frame_flags
    bits.bit(false); // frame_refs_short_signaling
    for index in 0..7_u8 {
        bits.bits(u64::from(index), 3);
        bits.bits(0, 4); // delta_frame_id_minus_1
    }

    let header = FrameReferenceHeader::parse_obu(&bits.obu(3), &sequence, &slots)?;
    assert_eq!(header.ref_frame_idx, [0, 1, 2, 3, 4, 5, 6]);
    assert_eq!(header.current_frame_id, 10);
    assert_eq!(header.order_hint, 12);
    assert_eq!(header.refresh_frame_flags, 1);
    Ok(())
}

#[test]
fn validates_show_existing_frame_id() -> VideoResult<()> {
    let sequence = main10_sequence()?;
    let mut slots = ReferenceSlots::default();
    slots.commit(
        1 << 3,
        ReferenceSlot {
            frame_id: 17,
            order_hint: 7,
            frame_type: FrameType::Inter as u8,
            surface_id: 42,
            width: 64,
            height: 64,
            render_width: 64,
            render_height: 64,
            film_grain: Default::default(),
        },
    );
    let mut bits = BitWriter::default();
    bits.bit(true);
    bits.bits(3, 3);
    bits.bits(17, 6);
    let header = FrameReferenceHeader::parse_obu(&bits.obu(3), &sequence, &slots)?;
    assert!(header.show_existing_frame);
    assert_eq!(header.frame_to_show_map_idx, Some(3));
    assert_eq!(header.refresh_frame_flags, 0);
    Ok(())
}

#[test]
fn parses_uniform_and_nonuniform_tile_layouts() -> VideoResult<()> {
    let sequence = reduced_sequence()?;
    let mut uniform = BitWriter::default();
    uniform.bit(true);
    uniform.bit(true); // tile_cols_log2: 0 -> 1
    uniform.bit(false); // stop at one
    uniform.bit(false); // tile_rows_log2 remains zero
    uniform.bits(1, 1); // context_update_tile_id
    uniform.bits(2, 2); // tile_size_bytes_minus_1
    let layout = TileLayout::parse(&uniform.bytes, &sequence, 1920, 1080)?;
    assert!(layout.uniform_spacing);
    assert_eq!(layout.column_widths_sb, [15, 15]);
    assert_eq!(layout.row_heights_sb, [17]);
    assert_eq!(layout.context_update_tile_id, 1);
    assert_eq!(layout.tile_size_bytes, 3);

    let mut nonuniform = BitWriter::default();
    nonuniform.bit(false);
    nonuniform.ns(9, 30); // 10 superblocks
    nonuniform.ns(19, 20); // remaining 20 superblocks
    nonuniform.ns(16, 17); // one row spanning all 17 superblocks
    nonuniform.bits(0, 1);
    nonuniform.bits(0, 2);
    let layout = TileLayout::parse(&nonuniform.bytes, &sequence, 1920, 1080)?;
    assert!(!layout.uniform_spacing);
    assert_eq!(layout.column_starts_sb, [0, 10]);
    assert_eq!(layout.column_widths_sb, [10, 20]);
    assert_eq!(layout.tile_count(), 2);
    Ok(())
}

#[test]
fn requires_ordered_complete_tile_groups() -> VideoResult<()> {
    let mut groups = TileGroupTracker::new(4)?;
    assert!(!groups.accept(0, 1)?);
    assert!(groups.accept(2, 3)?);

    let mut invalid = TileGroupTracker::new(4)?;
    assert!(invalid.accept(1, 2).is_err());
    Ok(())
}

#[test]
fn extracts_ffmpeg_style_tile_group_ranges() -> VideoResult<()> {
    let sequence = reduced_sequence()?;
    let mut tile_info = BitWriter::default();
    tile_info.bit(false);
    tile_info.ns(9, 30);
    tile_info.ns(19, 20);
    tile_info.ns(16, 17);
    tile_info.bits(0, 1);
    tile_info.bits(0, 2);
    let layout = TileLayout::parse(&tile_info.bytes, &sequence, 1920, 1080)?;
    let tile_group = [0x22, 5, 0xb0, 1, 0xaa, 0xbb, 0xcc];
    let ranges = parse_tile_group(&tile_group, &layout)?;
    assert_eq!(
        ranges,
        [
            super::tile::TileDataRange {
                data_offset: 2,
                data_size: 2,
                row: 0,
                column: 0,
            },
            super::tile::TileDataRange {
                data_offset: 4,
                data_size: 1,
                row: 0,
                column: 1,
            },
        ]
    );
    Ok(())
}

#[test]
fn joins_ordered_av1_tile_groups_and_relocates_offsets() -> VideoResult<()> {
    let sequence = reduced_sequence()?;
    let mut tile_info = BitWriter::default();
    tile_info.bit(false);
    tile_info.ns(9, 30);
    tile_info.ns(19, 20);
    tile_info.ns(16, 17);
    tile_info.bits(0, 1);
    tile_info.bits(0, 2);
    let layout = TileLayout::parse(&tile_info.bytes, &sequence, 1920, 1080)?;
    let groups = [&[0x22, 2, 0x90, 0xaa][..], &[0x22, 2, 0xf0, 0xbb][..]];
    let (payload, ranges) = collect_tile_groups(&groups, &layout)?;
    assert_eq!(payload, [0xaa, 0xbb]);
    assert_eq!(ranges[0].data_offset, 0);
    assert_eq!(ranges[1].data_offset, 1);
    assert_eq!(
        ranges.iter().map(|r| (r.row, r.column)).collect::<Vec<_>>(),
        [(0, 0), (0, 1)]
    );
    assert!(collect_tile_groups(&groups[..1], &layout).is_err());
    assert!(collect_tile_groups(&[groups[1], groups[0]], &layout).is_err());
    Ok(())
}

fn reduced_sequence() -> VideoResult<SequenceHeader> {
    SequenceHeader::parse_obu(&reduced_sequence_obu())
}

fn reduced_sequence_obu() -> Vec<u8> {
    let mut bits = BitWriter::default();
    bits.bits(0, 3);
    bits.bit(true);
    bits.bit(true);
    bits.bits(8, 5);
    bits.bits(7, 4);
    bits.bits(7, 4);
    bits.bits(127, 8);
    bits.bits(71, 8);
    bits.bit(false); // use_128x128_superblock
    bits.bit(true);
    bits.bit(true);
    bits.bit(false); // enable_superres
    bits.bit(true); // enable_cdef
    bits.bit(true); // enable_restoration
    bits.bit(true);
    bits.bit(false);
    bits.bit(false);
    bits.bit(false);
    bits.bits(0, 2);
    bits.bit(false);
    bits.bit(true); // film_grain_params_present
    bits.sequence_obu()
}

fn main10_sequence() -> VideoResult<SequenceHeader> {
    let mut bits = BitWriter::default();
    bits.bits(0, 3);
    bits.bit(false);
    bits.bit(false);
    bits.bit(false);
    bits.bit(false);
    bits.bits(0, 5);
    bits.bits(0, 12);
    bits.bits(8, 5);
    bits.bit(false);
    bits.bits(7, 4);
    bits.bits(7, 4);
    bits.bits(127, 8);
    bits.bits(71, 8);
    bits.bit(true);
    bits.bits(2, 4);
    bits.bits(1, 3);
    bits.bit(true);
    bits.bit(true);
    bits.bit(true);
    bits.bit(true);
    bits.bit(false);
    bits.bit(true);
    bits.bit(true);
    bits.bit(true);
    bits.bit(true);
    bits.bit(true);
    bits.bit(true);
    bits.bit(true);
    bits.bits(4, 3);
    bits.bit(true);
    bits.bit(true);
    bits.bit(true);
    bits.bit(true);
    bits.bit(false);
    bits.bit(false);
    bits.bit(false);
    bits.bits(0, 2);
    bits.bit(true);
    bits.bit(false);
    SequenceHeader::parse_obu(&bits.sequence_obu())
}
