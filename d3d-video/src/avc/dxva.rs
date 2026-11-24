use super::dpb::{DecodedPictureBuffer, Picture};
use super::parameter_sets::ParameterSets;
use super::slice::SliceHeader;
use super::{PictureStructure, unsupported};
use crate::{VideoError, VideoErrorKind, VideoResult};

const DXVA_H264_PICTURE_PARAMS_SIZE: usize = 1040;
const DXVA_H264_QMATRIX_SIZE: usize = 224;
const DXVA_H264_REFERENCE_COUNT: usize = 16;
const ZIGZAG_4: [usize; 16] = [0, 1, 4, 8, 5, 2, 3, 6, 9, 12, 13, 10, 7, 11, 14, 15];
const ZIGZAG_8: [usize; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20,
    13, 6, 7, 14, 21, 28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58, 59,
    52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63,
];

/// One DPB reference translated to the surface fields used by DXVA H.264.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct DxvaReferenceEntry {
    pub(crate) surface_index: u8,
    pub(crate) frame_num: u16,
    pub(crate) field_order_cnt: [i32; 2],
    pub(crate) reference_mask: u8,
    pub(crate) long_term: bool,
}

/// Converts live AVC DPB pictures to the surface order used by `FFmpeg`'s DXVA2 path.
pub(crate) fn dxva_references(
    dpb: &DecodedPictureBuffer,
    surface_by_picture_id: &[(u64, u8)],
) -> VideoResult<Vec<DxvaReferenceEntry>> {
    let mut references = Vec::new();
    references
        .try_reserve_exact(dpb.short_references().len() + 16)
        .map_err(|_| allocation_error("dxva_references"))?;
    for picture in dpb
        .short_references()
        .iter()
        .chain((0..16).filter_map(|i| dpb.long_reference(i)))
    {
        let surface_index = surface_by_picture_id
            .iter()
            .find_map(|(id, surface)| (*id == picture.id).then_some(*surface))
            .ok_or_else(|| {
                VideoError::new(
                    VideoErrorKind::InvalidState,
                    "dxva_references",
                    "a live DPB picture has no retained decoder surface",
                )
            })?;
        references.push(DxvaReferenceEntry {
            surface_index,
            frame_num: u16::try_from(picture.long_term_index.map_or(picture.frame_num, u32::from))
                .map_err(|_| {
                    VideoError::new(
                        VideoErrorKind::Unsupported,
                        "dxva_references",
                        "AVC reference frame number exceeds the DXVA field width",
                    )
                })?,
            field_order_cnt: picture.field_poc,
            reference_mask: picture.reference_mask,
            long_term: picture.long_term_index.is_some(),
        });
    }
    validate_reference_entries(&references)?;
    Ok(references)
}

/// Builds the DXVA2 H.264 picture-parameter structure for a progressive IDR picture.
///
/// This first mapping boundary covers 8-bit 4:2:0 frame pictures, no FMO, and no references on
/// the IDR picture. The caller owns the surface index and passes the sequence and picture
/// parameter NALs from codec configuration.
pub(crate) fn idr_picture_parameters(
    sps_nal: &[u8],
    pps_nal: &[u8],
    idr_slice_nal: &[u8],
    current_surface: u8,
) -> VideoResult<Vec<u8>> {
    picture_parameters_with_references(sps_nal, pps_nal, idr_slice_nal, current_surface, None, &[])
}

/// Builds AVC picture parameters using the caller's current picture and mapped DPB surfaces.
#[expect(
    clippy::too_many_lines,
    reason = "writes the DXVA H.264 picture-parameter fields in ABI order"
)]
pub(crate) fn picture_parameters_with_references(
    sps_nal: &[u8],
    pps_nal: &[u8],
    slice_nal: &[u8],
    current_surface: u8,
    current_picture: Option<&Picture>,
    references: &[DxvaReferenceEntry],
) -> VideoResult<Vec<u8>> {
    if current_surface > 0x7f {
        return Err(VideoError::new(
            VideoErrorKind::InvalidInput,
            "picture_parameters_with_references",
            "DXVA H.264 surface index must fit seven bits",
        ));
    }
    let mut sets = ParameterSets::default();
    let sps = sets.insert_sps_nal(sps_nal)?.clone();
    let pps = sets.insert_pps_nal(pps_nal)?.clone();
    let header = SliceHeader::parse_nal(slice_nal, &sets)?;
    sets.resolve_pps(u32::from(header.pps_id))?;
    if current_picture.is_none() && (!header.nal.is_idr() || !header.slice_type.is_intra()) {
        return Err(VideoError::new(
            VideoErrorKind::InvalidInput,
            "picture_parameters_with_references",
            "without caller DPB state only AVC IDR intra pictures are supported",
        ));
    }
    if header.structure != PictureStructure::Frame
        || !sps.frame_mbs_only
        || sps.chroma_format_idc != 1
        || sps.bit_depth_luma != 8
        || sps.bit_depth_chroma != 8
    {
        return Err(unsupported(
            "picture_parameters_with_references",
            "DXVA mapping currently supports progressive 8-bit 4:2:0 frames",
        ));
    }
    if pps.ref_count[0] == 0 || pps.ref_count[1] == 0 {
        return Err(unsupported(
            "picture_parameters_with_references",
            "zero default AVC reference counts are outside the current mapping",
        ));
    }

    let picture = if let Some(picture) = current_picture {
        if picture.frame_num != header.frame_num || picture.structure != header.structure {
            return Err(VideoError::new(
                VideoErrorKind::InvalidInput,
                "picture_parameters_with_references",
                "caller DPB picture does not match the parsed slice header",
            ));
        }
        picture.clone()
    } else {
        DecodedPictureBuffer::default().derive_picture(0, &header, &sps)?
    };
    let mut data = Vec::new();
    data.try_reserve_exact(DXVA_H264_PICTURE_PARAMS_SIZE)
        .map_err(|_| {
            VideoError::new(
                VideoErrorKind::Backend,
                "idr_picture_parameters",
                "unable to allocate DXVA picture parameters",
            )
        })?;
    let width_minus_one = sps.width_in_mbs.checked_sub(1).ok_or_else(|| {
        VideoError::new(
            VideoErrorKind::InvalidInput,
            "idr_picture_parameters",
            "SPS macroblock width is zero",
        )
    })?;
    let height_minus_one = sps.height_in_mbs.checked_sub(1).ok_or_else(|| {
        VideoError::new(
            VideoErrorKind::InvalidInput,
            "idr_picture_parameters",
            "SPS macroblock height is zero",
        )
    })?;
    put_u16(&mut data, width_minus_one);
    put_u16(&mut data, height_minus_one);
    data.push(current_surface);
    data.push(sps.max_num_ref_frames);

    let mut bit_fields = 0_u16;
    bit_fields |= u16::from(sps.mb_adaptive_frame_field) << 1;
    bit_fields |= u16::from(sps.chroma_format_idc & 0x03) << 4;
    bit_fields |= u16::from(header.nal.reference_idc != 0) << 6;
    bit_fields |= u16::from(pps.constrained_intra_pred) << 7;
    bit_fields |= u16::from(pps.weighted_pred) << 8;
    bit_fields |= u16::from(pps.weighted_bipred_idc & 0x03) << 9;
    bit_fields |= 1 << 11; // MbsConsecutiveFlag
    bit_fields |= u16::from(sps.frame_mbs_only) << 12;
    bit_fields |= u16::from(pps.transform_8x8_mode) << 13;
    bit_fields |= u16::from(sps.level_idc >= 31) << 14;
    bit_fields |= u16::from(header.slice_type.is_intra()) << 15;
    put_u16(&mut data, bit_fields);
    data.push(0); // bit_depth_luma_minus8
    data.push(0); // bit_depth_chroma_minus8
    put_u16(&mut data, 3); // Reserved16Bits: FFmpeg's generic DXVA workaround value
    put_u32(&mut data, 1); // StatusReportFeedbackNumber

    append_reference_entries(&mut data, references)?;
    put_i32(&mut data, picture.field_poc[0]);
    put_i32(&mut data, picture.field_poc[1]);
    append_reference_order_counts(&mut data, references)?;
    put_i8(
        &mut data,
        checked_i8(pps.init_qs - 26, "pic_init_qs_minus26")?,
    );
    put_i8(&mut data, pps.chroma_qp_index_offset[0]);
    put_i8(&mut data, pps.chroma_qp_index_offset[1]);
    data.push(1); // ContinuationFlag
    put_i8(
        &mut data,
        checked_i8(pps.init_qp - 26, "pic_init_qp_minus26")?,
    );
    data.push(pps.ref_count[0] - 1);
    data.push(pps.ref_count[1] - 1);
    data.push(0); // Reserved8BitsA
    let used_for_reference_flags = append_frame_numbers(&mut data, references)?;
    put_u32(&mut data, used_for_reference_flags);
    put_u16(&mut data, 0); // NonExistingFrameFlags
    put_u16(
        &mut data,
        u16::try_from(header.frame_num).map_err(|_| {
            VideoError::new(
                VideoErrorKind::Unsupported,
                "idr_picture_parameters",
                "AVC frame_num exceeds the DXVA field width",
            )
        })?,
    );
    data.push(sps.log2_max_frame_num.checked_sub(4).ok_or_else(|| {
        VideoError::new(
            VideoErrorKind::Unsupported,
            "idr_picture_parameters",
            "log2_max_frame_num is below the DXVA minimum",
        )
    })?);
    data.push(sps.poc_type);
    data.push(if sps.poc_type == 0 {
        sps.log2_max_poc_lsb.checked_sub(4).ok_or_else(|| {
            VideoError::new(
                VideoErrorKind::Unsupported,
                "idr_picture_parameters",
                "log2_max_pic_order_cnt_lsb is below the DXVA minimum",
            )
        })?
    } else {
        0
    });
    data.push(u8::from(sps.delta_pic_order_always_zero));
    data.push(u8::from(sps.direct_8x8_inference));
    data.push(u8::from(pps.cabac));
    data.push(u8::from(pps.pic_order_present));
    data.push(0); // num_slice_groups_minus1; FMO is rejected by the parser
    data.push(0); // slice_group_map_type
    data.push(u8::from(pps.deblocking_filter_parameters_present));
    data.push(u8::from(pps.redundant_pic_count_present));
    data.push(0); // Reserved8BitsB
    put_u16(&mut data, 0); // slice_group_change_rate_minus1
    if data.len() > DXVA_H264_PICTURE_PARAMS_SIZE {
        return Err(VideoError::new(
            VideoErrorKind::Backend,
            "idr_picture_parameters",
            "DXVA picture-parameter fields exceed the ABI structure size",
        ));
    }
    data.resize(DXVA_H264_PICTURE_PARAMS_SIZE, 0);

    if data.len() != DXVA_H264_PICTURE_PARAMS_SIZE {
        return Err(VideoError::new(
            VideoErrorKind::Backend,
            "idr_picture_parameters",
            format!("DXVA picture parameter layout is {} bytes", data.len()),
        ));
    }
    Ok(data)
}

/// Builds the DXVA2 H.264 inverse-quantization buffer from the active PPS scaling lists.
pub(crate) fn idr_quant_matrix(sps_nal: &[u8], pps_nal: &[u8]) -> VideoResult<Vec<u8>> {
    let mut sets = ParameterSets::default();
    sets.insert_sps_nal(sps_nal)?;
    let pps = sets.insert_pps_nal(pps_nal)?;
    let mut data = Vec::new();
    data.try_reserve_exact(DXVA_H264_QMATRIX_SIZE)
        .map_err(|_| {
            VideoError::new(
                VideoErrorKind::Backend,
                "idr_quant_matrix",
                "unable to allocate DXVA inverse-quantization matrix",
            )
        })?;
    for list in &pps.scaling_matrix4 {
        for index in ZIGZAG_4 {
            data.push(list[index]);
        }
    }
    for list_index in [0, 3] {
        for index in ZIGZAG_8 {
            data.push(pps.scaling_matrix8[list_index][index]);
        }
    }
    Ok(data)
}

fn checked_i8(value: i32, field: &'static str) -> VideoResult<i8> {
    i8::try_from(value).map_err(|_| {
        VideoError::new(
            VideoErrorKind::Unsupported,
            "idr_picture_parameters",
            format!("{field} does not fit the DXVA signed-byte field"),
        )
    })
}

fn append_reference_entries(
    data: &mut Vec<u8>,
    references: &[DxvaReferenceEntry],
) -> VideoResult<()> {
    validate_reference_entries(references)?;
    for index in 0..DXVA_H264_REFERENCE_COUNT {
        let entry = references.get(index);
        let encoded = entry.map_or(0xff, |reference| {
            reference.surface_index | (u8::from(reference.long_term) << 7)
        });
        data.push(encoded);
    }
    Ok(())
}

fn append_reference_order_counts(
    data: &mut Vec<u8>,
    references: &[DxvaReferenceEntry],
) -> VideoResult<()> {
    validate_reference_entries(references)?;
    for index in 0..DXVA_H264_REFERENCE_COUNT {
        if let Some(reference) = references.get(index) {
            for poc in reference.field_order_cnt {
                put_i32(data, if poc == i32::MAX { 0 } else { poc });
            }
        } else {
            put_i32(data, 0);
            put_i32(data, 0);
        }
    }
    Ok(())
}

fn append_frame_numbers(data: &mut Vec<u8>, references: &[DxvaReferenceEntry]) -> VideoResult<u32> {
    validate_reference_entries(references)?;
    let mut used_for_reference_flags = 0_u32;
    for index in 0..DXVA_H264_REFERENCE_COUNT {
        if let Some(reference) = references.get(index) {
            put_u16(data, reference.frame_num);
            let field_flags = u32::from(reference.reference_mask & 0x03);
            used_for_reference_flags |= field_flags << (2 * index);
        } else {
            put_u16(data, 0);
        }
    }
    Ok(used_for_reference_flags)
}

fn validate_reference_entries(references: &[DxvaReferenceEntry]) -> VideoResult<()> {
    if references.len() > DXVA_H264_REFERENCE_COUNT {
        return Err(VideoError::new(
            VideoErrorKind::Unsupported,
            "validate_reference_entries",
            "DXVA H.264 can represent at most 16 reference pictures",
        ));
    }
    if references
        .iter()
        .any(|reference| reference.surface_index > 0x7f || reference.reference_mask > 0x03)
    {
        return Err(VideoError::new(
            VideoErrorKind::InvalidInput,
            "validate_reference_entries",
            "reference surface or field flags exceed DXVA H.264 fields",
        ));
    }
    Ok(())
}

fn allocation_error(operation: &'static str) -> VideoError {
    VideoError::new(
        VideoErrorKind::Backend,
        operation,
        "unable to allocate AVC DXVA reference entries",
    )
}

fn put_u16(data: &mut Vec<u8>, value: u16) {
    data.extend_from_slice(&value.to_le_bytes());
}

fn put_u32(data: &mut Vec<u8>, value: u32) {
    data.extend_from_slice(&value.to_le_bytes());
}

fn put_i32(data: &mut Vec<u8>, value: i32) {
    data.extend_from_slice(&value.to_le_bytes());
}

fn put_i8(data: &mut Vec<u8>, value: i8) {
    data.push(value.cast_unsigned());
}

#[cfg(test)]
mod tests {
    use super::{
        DxvaReferenceEntry, append_frame_numbers, append_reference_entries,
        append_reference_order_counts,
    };

    #[test]
    fn serializes_reference_entries_in_dxva_field_order() -> crate::VideoResult<()> {
        let reference = DxvaReferenceEntry {
            surface_index: 5,
            frame_num: 0x1234,
            field_order_cnt: [7, -3],
            reference_mask: 1,
            long_term: true,
        };
        let mut entries = Vec::new();
        append_reference_entries(&mut entries, &[reference])?;
        assert_eq!(entries.len(), 16);
        assert_eq!(entries[0], 0x85);
        assert_eq!(entries[1..], [0xff; 15]);

        let mut order_counts = Vec::new();
        append_reference_order_counts(&mut order_counts, &[reference])?;
        assert_eq!(order_counts.len(), 16 * 8);
        assert_eq!(&order_counts[..8], &[7, 0, 0, 0, 0xfd, 0xff, 0xff, 0xff]);

        let mut frame_numbers = Vec::new();
        let flags = append_frame_numbers(&mut frame_numbers, &[reference])?;
        assert_eq!(frame_numbers.len(), 16 * 2);
        assert_eq!(&frame_numbers[..2], &[0x34, 0x12]);
        assert_eq!(flags, 1);
        Ok(())
    }
}
