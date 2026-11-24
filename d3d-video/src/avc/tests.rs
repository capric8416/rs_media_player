use super::dpb::{DecodedPictureBuffer, Picture};
use super::parameter_sets::{ParameterSets, SequenceParameterSet};
use super::slice::{MemoryManagementOperation, RefPicListModification, SliceHeader, SliceType};
use super::{NalHeader, PictureStructure};
use crate::{VideoErrorKind, VideoResult};

#[derive(Default)]
struct BitWriter {
    bits: Vec<bool>,
}

impl BitWriter {
    fn bit(&mut self, value: bool) {
        self.bits.push(value);
    }

    fn bits(&mut self, value: u32, count: u8) {
        for shift in (0..count).rev() {
            self.bit(value & (1_u32 << shift) != 0);
        }
    }

    fn ue(&mut self, value: u32) {
        let code = u64::from(value) + 1;
        let width = 64 - code.leading_zeros();
        for _ in 1..width {
            self.bit(false);
        }
        for shift in (0..width).rev() {
            self.bit(code & (1_u64 << shift) != 0);
        }
    }

    fn se(&mut self, value: i32) {
        let code = if value <= 0 {
            value.unsigned_abs() * 2
        } else {
            u32::try_from(value).map_or(u32::MAX, |positive| positive * 2 - 1)
        };
        self.ue(code);
    }

    fn finish_rbsp(mut self) -> Vec<u8> {
        self.bit(true);
        while !self.bits.len().is_multiple_of(8) {
            self.bit(false);
        }
        self.bits
            .as_chunks::<8>()
            .0
            .iter()
            .map(|chunk| {
                chunk
                    .iter()
                    .fold(0_u8, |byte, bit| (byte << 1) | u8::from(*bit))
            })
            .collect()
    }
}

fn baseline_sps_nal(width_mbs: u32, height_map_units: u32, crop_bottom: u32) -> Vec<u8> {
    baseline_sps_with_crop(width_mbs, height_map_units, [0, 0, 0, crop_bottom])
}

fn baseline_sps_with_crop(width_mbs: u32, height_map_units: u32, crop: [u32; 4]) -> Vec<u8> {
    let mut bits = BitWriter::default();
    bits.bits(66, 8);
    bits.bits(0, 8);
    bits.bits(31, 8);
    bits.ue(0);
    bits.ue(0);
    bits.ue(0);
    bits.ue(0);
    bits.ue(2);
    bits.bit(false);
    bits.ue(width_mbs - 1);
    bits.ue(height_map_units - 1);
    bits.bit(true);
    bits.bit(true);
    bits.bit(crop.iter().any(|value| *value != 0));
    if crop.iter().any(|value| *value != 0) {
        for value in crop {
            bits.ue(value);
        }
    }
    bits.bit(false);
    let mut nal = vec![0x67];
    nal.extend(bits.finish_rbsp());
    nal
}

fn high10_sps_nal() -> Vec<u8> {
    let mut bits = BitWriter::default();
    bits.bits(110, 8);
    bits.bits(0, 8);
    bits.bits(40, 8);
    bits.ue(1);
    bits.ue(1);
    bits.ue(2);
    bits.ue(2);
    bits.bit(false);
    bits.bit(false);
    bits.ue(0);
    bits.ue(0);
    bits.ue(0);
    bits.ue(4);
    bits.bit(false);
    bits.ue(119);
    bits.ue(67);
    bits.bit(true);
    bits.bit(true);
    bits.bit(false);
    bits.bit(false);
    let mut nal = vec![0x67];
    nal.extend(bits.finish_rbsp());
    nal
}

fn pps_nal(cabac: bool, slice_group_count_minus_one: u32) -> Vec<u8> {
    let mut bits = BitWriter::default();
    bits.ue(0);
    bits.ue(0);
    bits.bit(cabac);
    bits.bit(false);
    bits.ue(slice_group_count_minus_one);
    if slice_group_count_minus_one == 0 {
        bits.ue(0);
        bits.ue(0);
        bits.bit(false);
        bits.bits(0, 2);
        bits.se(0);
        bits.se(0);
        bits.se(0);
        bits.bit(true);
        bits.bit(false);
        bits.bit(false);
    }
    let mut nal = vec![0x68];
    nal.extend(bits.finish_rbsp());
    nal
}

fn high_profile_pps_nal() -> Vec<u8> {
    let mut bits = BitWriter::default();
    bits.ue(2);
    bits.ue(1);
    bits.bit(true);
    bits.bit(true);
    bits.ue(0);
    bits.ue(0);
    bits.ue(1);
    bits.bit(true);
    bits.bits(1, 2);
    bits.se(-2);
    bits.se(0);
    bits.se(-1);
    bits.bit(true);
    bits.bit(false);
    bits.bit(false);
    bits.bit(true);
    bits.bit(false);
    bits.se(2);
    let mut nal = vec![0x68];
    nal.extend(bits.finish_rbsp());
    nal
}

fn baseline_sets(cabac: bool) -> VideoResult<ParameterSets> {
    let mut sets = ParameterSets::default();
    sets.insert_sps_nal(&baseline_sps_nal(80, 45, 0))?;
    sets.insert_pps_nal(&pps_nal(cabac, 0))?;
    Ok(sets)
}

fn slice_nal(
    nal_header: u8,
    slice_type: u32,
    frame_num: u32,
    poc_lsb: u32,
    adaptive: &[MemoryManagementOperation],
) -> Vec<u8> {
    let mut bits = BitWriter::default();
    bits.ue(0);
    bits.ue(slice_type);
    bits.ue(0);
    bits.bits(frame_num, 4);
    if nal_header & 0x1f == 5 {
        bits.ue(0);
    }
    bits.bits(poc_lsb, 4);
    if slice_type % 5 == 1 {
        bits.bit(true);
    }
    if !matches!(slice_type % 5, 2 | 4) {
        bits.bit(false);
        bits.bit(false);
        if slice_type % 5 == 1 {
            bits.bit(false);
        }
    }
    if nal_header >> 5 != 0 {
        if nal_header & 0x1f == 5 {
            bits.bit(false);
            bits.bit(false);
        } else {
            bits.bit(!adaptive.is_empty());
            for operation in adaptive {
                write_mmco(&mut bits, *operation, frame_num);
            }
            if !adaptive.is_empty() {
                bits.ue(0);
            }
        }
    }
    bits.se(0);
    bits.ue(0);
    bits.se(0);
    bits.se(0);
    let mut nal = vec![nal_header];
    nal.extend(bits.finish_rbsp());
    nal
}

fn write_mmco(bits: &mut BitWriter, operation: MemoryManagementOperation, frame_num: u32) {
    match operation {
        MemoryManagementOperation::ForgetShort { picture_number } => {
            bits.ue(1);
            bits.ue(frame_num.wrapping_sub(picture_number).wrapping_sub(1) & 15);
        }
        MemoryManagementOperation::ForgetLong { index } => {
            bits.ue(2);
            bits.ue(u32::from(index));
        }
        MemoryManagementOperation::ShortToLong {
            picture_number,
            index,
        } => {
            bits.ue(3);
            bits.ue(frame_num.wrapping_sub(picture_number).wrapping_sub(1) & 15);
            bits.ue(u32::from(index));
        }
        MemoryManagementOperation::SetMaxLong { max_index } => {
            bits.ue(4);
            bits.ue(max_index.map_or(0, |index| u32::from(index) + 1));
        }
        MemoryManagementOperation::Reset => bits.ue(5),
        MemoryManagementOperation::CurrentToLong { index } => {
            bits.ue(6);
            bits.ue(u32::from(index));
        }
    }
}

fn synthetic_sps(max_refs: u8) -> VideoResult<SequenceParameterSet> {
    let sets = baseline_sets(false)?;
    let mut sps = sets
        .sps(0)
        .cloned()
        .ok_or_else(|| super::avc_error("synthetic_sps", "test setup did not create SPS"))?;
    sps.max_num_ref_frames = max_refs;
    Ok(sps)
}

fn synthetic_header(slice_type: SliceType, frame_num: u32, ref_idc: u8) -> SliceHeader {
    SliceHeader {
        nal: NalHeader {
            reference_idc: ref_idc,
            unit_type: 1,
        },
        first_mb_address: 0,
        slice_type,
        fixed_slice_type: false,
        pps_id: 0,
        frame_num,
        structure: PictureStructure::Frame,
        idr_picture_id: None,
        poc_lsb: frame_num * 2,
        delta_poc_bottom: 0,
        delta_poc: [0, 0],
        redundant_pic_count: 0,
        direct_spatial_mv_pred: true,
        ref_count: if slice_type == SliceType::B {
            [2, 2]
        } else {
            [1, 0]
        },
        ref_modifications: [Vec::new(), Vec::new()],
        no_output_of_prior_pictures: false,
        long_term_reference: false,
        adaptive_ref_pic_marking: false,
        memory_management: Vec::new(),
        cabac_init_idc: 0,
        qscale: 26,
        deblocking_filter_idc: 0,
        slice_alpha_c0_offset: 0,
        slice_beta_offset: 0,
        header_bit_size: 0,
    }
}

#[test]
fn parses_baseline_dimensions_and_crop_units() -> VideoResult<()> {
    let mut sets = ParameterSets::default();
    let sps = sets.insert_sps_nal(&baseline_sps_nal(120, 68, 4))?;
    assert_eq!(sps.profile_idc, 66);
    assert_eq!(sps.coded_width(), 1920);
    assert_eq!(sps.coded_height(), 1088);
    assert_eq!(sps.display_width(), 1920);
    assert_eq!(sps.display_height(), 1080);
    assert_eq!(sps.max_num_ref_frames, 2);
    Ok(())
}

#[test]
fn state_reports_visible_avc_rectangle() -> VideoResult<()> {
    let state = super::AvcDecodeState::new(
        &baseline_sps_with_crop(120, 68, [2, 4, 3, 1]),
        &pps_nal(false, 0),
    )?;
    let rect = state.visible_rect()?;
    assert_eq!(
        (rect.x, rect.y, rect.size.width(), rect.size.height()),
        (4, 6, 1908, 1080)
    );
    Ok(())
}

#[test]
fn parses_high10_profile_fields() -> VideoResult<()> {
    let mut sets = ParameterSets::default();
    let sps = sets.insert_sps_nal(&high10_sps_nal())?;
    assert_eq!(sps.id, 1);
    assert_eq!(sps.chroma_format_idc, 1);
    assert_eq!(sps.bit_depth_luma, 10);
    assert_eq!(sps.bit_depth_chroma, 10);
    assert_eq!(sps.max_num_ref_frames, 4);
    Ok(())
}

#[test]
fn parses_pps_and_rejects_fmo() -> VideoResult<()> {
    let mut sets = ParameterSets::default();
    sets.insert_sps_nal(&baseline_sps_nal(80, 45, 0))?;
    let pps = sets.insert_pps_nal(&pps_nal(true, 0))?;
    assert!(pps.cabac);
    assert_eq!(pps.ref_count, [1, 1]);
    assert_eq!(pps.init_qp, 26);
    assert!(pps.deblocking_filter_parameters_present);

    let error = sets
        .insert_pps_nal(&pps_nal(false, 1))
        .err()
        .ok_or_else(|| super::avc_error("parses_pps_and_rejects_fmo", "FMO unexpectedly passed"))?;
    assert_eq!(error.kind(), VideoErrorKind::Unsupported);
    Ok(())
}

#[test]
fn parses_high_profile_pps_extension() -> VideoResult<()> {
    let mut sets = ParameterSets::default();
    sets.insert_sps_nal(&high10_sps_nal())?;
    let pps = sets.insert_pps_nal(&high_profile_pps_nal())?;
    assert_eq!(pps.id, 2);
    assert_eq!(pps.sps_id, 1);
    assert!(pps.cabac);
    assert!(pps.pic_order_present);
    assert_eq!(pps.ref_count, [1, 2]);
    assert_eq!(pps.init_qp, 36);
    assert_eq!(pps.chroma_qp_index_offset, [-1, 2]);
    assert!(pps.transform_8x8_mode);
    assert_eq!(sets.pps(2).map(|stored| stored.id), Some(2));
    Ok(())
}

#[test]
fn parses_idr_and_fixed_slice_types() -> VideoResult<()> {
    let sets = baseline_sets(false)?;
    let header = SliceHeader::parse_nal(&slice_nal(0x65, 7, 0, 0, &[]), &sets)?;
    assert_eq!(header.slice_type, SliceType::I);
    assert!(header.fixed_slice_type);
    assert!(header.nal.is_idr());
    assert_eq!(header.idr_picture_id, Some(0));
    assert_eq!(header.qscale, 26);

    let b_slice = SliceHeader::parse_nal(&slice_nal(0x01, 1, 2, 4, &[]), &sets)?;
    assert_eq!(b_slice.slice_type, SliceType::B);
    assert!(b_slice.direct_spatial_mv_pred);
    assert_eq!(b_slice.ref_count, [1, 1]);
    Ok(())
}

#[test]
fn builds_dxva2_progressive_idr_picture_parameters() -> VideoResult<()> {
    let data = super::dxva::idr_picture_parameters(
        &baseline_sps_nal(80, 45, 0),
        &pps_nal(false, 0),
        &slice_nal(0x65, 2, 0, 0, &[]),
        7,
    )?;
    assert_eq!(data.len(), 1040);
    assert_eq!(&data[..6], &[79, 0, 44, 0, 7, 2]);
    assert_eq!(data[8], 0);
    assert_eq!(data[9], 0);
    assert_eq!(&data[16..32], &[0xff; 16]);
    assert_eq!(u16::from_le_bytes([data[6], data[7]]) & (1 << 15), 1 << 15);
    assert_eq!(u16::from_le_bytes([data[6], data[7]]) & (1 << 6), 1 << 6);
    assert!(data[230..].iter().all(|byte| *byte == 0));
    Ok(())
}

#[test]
fn builds_non_idr_picture_parameters_from_dpb_surface_bindings() -> VideoResult<()> {
    let sets = baseline_sets(false)?;
    let sps = sets
        .sps(0)
        .ok_or_else(|| super::avc_error("test", "missing SPS"))?;
    let idr_nal = slice_nal(0x65, 2, 0, 0, &[]);
    let idr_header = SliceHeader::parse_nal(&idr_nal, &sets)?;
    let mut dpb = DecodedPictureBuffer::default();
    let reference = dpb.derive_picture(11, &idr_header, sps)?;
    dpb.mark_decoded(reference, &idr_header, sps)?;

    let p_slice = slice_nal(0x41, 0, 1, 2, &[]);
    let p_header = SliceHeader::parse_nal(&p_slice, &sets)?;
    let current = dpb.derive_picture(12, &p_header, sps)?;
    let missing_surface = super::dxva::dxva_references(&dpb, &[])
        .err()
        .ok_or_else(|| super::avc_error("test", "missing DPB surface was accepted"))?;
    assert_eq!(missing_surface.kind(), VideoErrorKind::InvalidState);
    let references = super::dxva::dxva_references(&dpb, &[(11, 3)])?;
    assert_eq!(references.len(), 1);
    assert_eq!(references[0].surface_index, 3);

    let parameters = super::dxva::picture_parameters_with_references(
        &baseline_sps_nal(80, 45, 0),
        &pps_nal(false, 0),
        &p_slice,
        7,
        Some(&current),
        &references,
    )?;
    assert_eq!(parameters[16], 3);
    assert_eq!(parameters[17..32], [0xff; 15]);
    assert_eq!(
        i32::from_le_bytes(
            parameters[40..44]
                .try_into()
                .map_err(|_| { super::avc_error("test", "invalid reference POC field") })?
        ),
        0
    );
    assert_eq!(u16::from_le_bytes([parameters[176], parameters[177]]), 0);
    assert_eq!(
        u32::from_le_bytes(
            parameters[208..212]
                .try_into()
                .map_err(|_| { super::avc_error("test", "invalid reference mask field") })?
        ),
        3
    );
    assert_eq!(
        u16::from_le_bytes([parameters[6], parameters[7]]) & (1 << 15),
        0
    );
    Ok(())
}

#[test]
fn advances_avc_dxva_state_only_when_prepared_picture_is_committed() -> VideoResult<()> {
    let sps = baseline_sps_nal(80, 45, 0);
    let pps = pps_nal(false, 0);
    let mut state = super::AvcDecodeState::new(&sps, &pps)?;
    let idr = slice_nal(0x65, 2, 0, 0, &[]);
    let prepared = state.prepare(&[&idr], 7)?;
    assert_eq!(state.reference_surfaces(), []);
    assert_eq!(prepared.picture_parameters.len(), 1040);
    state.commit(prepared);
    assert_eq!(state.reference_surfaces().len(), 1);
    assert_eq!(state.reference_surfaces()[0].1, 7);

    let p_slice = slice_nal(0x41, 0, 1, 2, &[]);
    let prepared = state.prepare(&[&p_slice], 4)?;
    assert_eq!(prepared.picture_parameters[16], 7);
    state.commit(prepared);
    assert_eq!(state.reference_surfaces().len(), 2);
    assert!(
        state
            .reference_surfaces()
            .iter()
            .any(|(_, surface)| *surface == 4)
    );

    let reused = state
        .prepare(&[&p_slice], 7)
        .err()
        .ok_or_else(|| super::avc_error("test", "live AVC reference surface was reused"))?;
    assert_eq!(reused.kind(), VideoErrorKind::InvalidState);
    let changed_pps = pps_nal(true, 0);
    let changed = state
        .validate_parameter_set(&changed_pps)
        .err()
        .ok_or_else(|| super::avc_error("test", "in-band PPS change was accepted"))?;
    assert_eq!(changed.kind(), VideoErrorKind::Unsupported);

    state.reset();
    assert_eq!(state.reference_surfaces(), []);
    state.reconfigure(&sps, &changed_pps)?;
    state.validate_parameter_set(&changed_pps)?;
    Ok(())
}

#[test]
fn builds_dxva2_h264_inverse_quantization_matrix() -> VideoResult<()> {
    let matrix = super::dxva::idr_quant_matrix(&baseline_sps_nal(80, 45, 0), &pps_nal(false, 0))?;
    assert_eq!(matrix.len(), 224);
    assert!(matrix.iter().all(|value| *value != 0));
    Ok(())
}

#[cfg(windows)]
#[test]
fn builds_complete_dxva2_idr_picture_buffers() -> VideoResult<()> {
    let sps = baseline_sps_nal(80, 45, 0);
    let pps = pps_nal(false, 0);
    let slice = slice_nal(0x65, 2, 0, 0, &[]);
    let mut sample_data = vec![0, 0, 1];
    sample_data.extend_from_slice(&slice);
    let sample = crate::EncodedSample {
        data: &sample_data,
        timing: crate::SampleTiming {
            dts: None,
            pts: None,
            duration: None,
            time_base: crate::TimeBase::new(1, 1000)?,
        },
        is_random_access: true,
    };
    let picture = crate::d3d9_decode::Dxva2Picture::from_avc_idr_sample(
        7,
        &sps,
        &pps,
        sample,
        crate::BitstreamFormat::AnnexB,
        crate::FrameSize::new(1280, 720)?,
    )?;
    assert_eq!(picture.target_index, 7);
    assert_eq!(picture.buffers.len(), 4);
    assert_eq!(picture.buffers[0].data.len(), 1040);
    assert_eq!(picture.buffers[1].data.len(), 224);
    assert_eq!(picture.buffers[2].data.len() % 128, 0);
    assert_eq!(picture.buffers[3].data.len(), 10);
    Ok(())
}

#[test]
fn rejects_non_intra_idr_and_truncated_slice() -> VideoResult<()> {
    let sets = baseline_sets(false)?;
    let invalid = SliceHeader::parse_nal(&slice_nal(0x65, 0, 0, 0, &[]), &sets)
        .err()
        .ok_or_else(|| super::avc_error("rejects_non_intra_idr", "invalid IDR passed"))?;
    assert_eq!(invalid.kind(), VideoErrorKind::InvalidInput);
    let truncated = SliceHeader::parse_nal(&[0x41], &sets)
        .err()
        .ok_or_else(|| super::avc_error("rejects_truncated_slice", "truncated slice passed"))?;
    assert_eq!(truncated.kind(), VideoErrorKind::InvalidInput);
    Ok(())
}

#[test]
fn parses_adaptive_mmco() -> VideoResult<()> {
    let sets = baseline_sets(false)?;
    let requested = [
        MemoryManagementOperation::ShortToLong {
            picture_number: 1,
            index: 2,
        },
        MemoryManagementOperation::SetMaxLong { max_index: Some(2) },
        MemoryManagementOperation::CurrentToLong { index: 1 },
    ];
    let header = SliceHeader::parse_nal(&slice_nal(0x41, 0, 3, 6, &requested), &sets)?;
    assert!(header.adaptive_ref_pic_marking);
    assert_eq!(header.memory_management, requested);
    Ok(())
}

#[test]
fn poc_type_zero_wraps_msb() -> VideoResult<()> {
    let sps = synthetic_sps(4)?;
    let mut dpb = DecodedPictureBuffer::default();
    let mut first = synthetic_header(SliceType::P, 0, 1);
    first.poc_lsb = 14;
    let picture = dpb.derive_picture(1, &first, &sps)?;
    assert_eq!(picture.poc, 14);
    let mut second = synthetic_header(SliceType::P, 1, 1);
    second.poc_lsb = 1;
    let picture = dpb.derive_picture(2, &second, &sps)?;
    assert_eq!(picture.poc, 17);
    Ok(())
}

#[test]
fn derives_type_one_and_type_two_poc() -> VideoResult<()> {
    let mut type_one = synthetic_sps(4)?;
    type_one.poc_type = 1;
    type_one.offset_for_ref_frame = vec![2];
    type_one.offset_for_top_to_bottom_field = 1;
    let mut dpb = DecodedPictureBuffer::default();
    let header = synthetic_header(SliceType::P, 1, 1);
    let picture = dpb.derive_picture(1, &header, &type_one)?;
    assert_eq!(picture.field_poc, [2, 3]);

    let mut type_two = synthetic_sps(4)?;
    type_two.poc_type = 2;
    let mut dpb = DecodedPictureBuffer::default();
    let reference = dpb.derive_picture(2, &synthetic_header(SliceType::P, 3, 1), &type_two)?;
    assert_eq!(reference.poc, 6);
    let non_reference = dpb.derive_picture(3, &synthetic_header(SliceType::P, 4, 0), &type_two)?;
    assert_eq!(non_reference.poc, 7);
    Ok(())
}

#[test]
fn sliding_window_keeps_newest_references() -> VideoResult<()> {
    let sps = synthetic_sps(2)?;
    let mut dpb = DecodedPictureBuffer::default();
    for frame in 0..3_u32 {
        let header = synthetic_header(SliceType::P, frame, 1);
        let picture = Picture::new(
            u64::from(frame),
            frame,
            [i32::try_from(frame * 2).map_err(|_| super::avc_error("test", "POC overflow"))?; 2],
            PictureStructure::Frame,
        );
        dpb.mark_decoded(picture, &header, &sps)?;
    }
    let frames: Vec<_> = dpb
        .short_references()
        .iter()
        .map(|picture| picture.frame_num)
        .collect();
    assert_eq!(frames, [2, 1]);
    Ok(())
}

#[test]
fn mmco_moves_short_reference_and_assigns_current_long() -> VideoResult<()> {
    let sps = synthetic_sps(4)?;
    let mut dpb = DecodedPictureBuffer::default();
    let first_header = synthetic_header(SliceType::P, 1, 1);
    dpb.mark_decoded(
        Picture::new(1, 1, [2, 2], PictureStructure::Frame),
        &first_header,
        &sps,
    )?;
    let mut second_header = synthetic_header(SliceType::P, 2, 1);
    second_header.adaptive_ref_pic_marking = true;
    second_header.memory_management = vec![
        MemoryManagementOperation::ShortToLong {
            picture_number: 1,
            index: 2,
        },
        MemoryManagementOperation::CurrentToLong { index: 0 },
    ];
    dpb.mark_decoded(
        Picture::new(2, 2, [4, 4], PictureStructure::Frame),
        &second_header,
        &sps,
    )?;
    assert_eq!(dpb.long_reference(0).map(|picture| picture.id), Some(2));
    assert_eq!(dpb.long_reference(2).map(|picture| picture.id), Some(1));
    assert_eq!(dpb.short_references(), []);
    Ok(())
}

#[test]
fn builds_p_and_b_default_reference_lists() -> VideoResult<()> {
    let sps = synthetic_sps(4)?;
    let mut dpb = DecodedPictureBuffer::default();
    for (id, frame, poc) in [(1, 1, 2), (2, 2, 6), (3, 3, 10)] {
        dpb.mark_decoded(
            Picture::new(id, frame, [poc, poc], PictureStructure::Frame),
            &synthetic_header(SliceType::P, frame, 1),
            &sps,
        )?;
    }
    let p = dpb.build_reference_lists(&synthetic_header(SliceType::P, 4, 0), &sps, 12)?;
    assert_eq!(
        p.list0.iter().map(|entry| entry.id).collect::<Vec<_>>(),
        [3]
    );

    let b = dpb.build_reference_lists(&synthetic_header(SliceType::B, 4, 0), &sps, 8)?;
    assert_eq!(
        b.list0.iter().map(|entry| entry.id).collect::<Vec<_>>(),
        [2, 1]
    );
    assert_eq!(
        b.list1.iter().map(|entry| entry.id).collect::<Vec<_>>(),
        [3, 2]
    );
    Ok(())
}

#[test]
fn applies_short_term_reference_list_modification() -> VideoResult<()> {
    let sps = synthetic_sps(4)?;
    let mut dpb = DecodedPictureBuffer::default();
    for frame in 1..=3_u32 {
        let poc =
            i32::try_from(frame * 2).map_err(|_| super::avc_error("test", "POC exceeds i32"))?;
        dpb.mark_decoded(
            Picture::new(u64::from(frame), frame, [poc, poc], PictureStructure::Frame),
            &synthetic_header(SliceType::P, frame, 1),
            &sps,
        )?;
    }
    let mut header = synthetic_header(SliceType::P, 4, 0);
    header.ref_modifications[0].push(RefPicListModification::Subtract { difference: 3 });
    let lists = dpb.build_reference_lists(&header, &sps, 8)?;
    assert_eq!(
        lists.list0.iter().map(|entry| entry.id).collect::<Vec<_>>(),
        [1]
    );
    Ok(())
}

#[test]
fn rejects_forbidden_nal_header() -> VideoResult<()> {
    let error = NalHeader::parse(0x80)
        .err()
        .ok_or_else(|| super::avc_error("rejects_forbidden_nal_header", "forbidden bit passed"))?;
    assert_eq!(error.kind(), VideoErrorKind::InvalidInput);
    Ok(())
}
