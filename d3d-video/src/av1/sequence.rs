use super::av1_error;
use crate::bit_reader::BitReader;
use crate::obu::{ObuType, parse_obu};
use crate::{VideoError, VideoErrorKind, VideoResult};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ColorConfig {
    pub(crate) bit_depth: u8,
    pub(crate) monochrome: bool,
    pub(crate) color_primaries: u8,
    pub(crate) transfer_characteristics: u8,
    pub(crate) matrix_coefficients: u8,
    pub(crate) full_range: bool,
    pub(crate) chroma_sample_position: u8,
    pub(crate) separate_uv_delta_q: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DecoderModelInfo {
    pub(crate) equal_picture_interval: bool,
    pub(crate) buffer_removal_time_bits: u8,
    pub(crate) frame_presentation_time_bits: u8,
    pub(crate) operating_point_idc: [u16; 32],
    pub(crate) decoder_model_present: [bool; 32],
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "fields mirror independent AV1 sequence-header syntax flags"
)]
pub(crate) struct SequenceHeader {
    pub(crate) profile: u8,
    pub(crate) still_picture: bool,
    pub(crate) reduced_still_picture_header: bool,
    pub(crate) max_frame_width: u16,
    pub(crate) max_frame_height: u16,
    pub(crate) frame_width_bits: u8,
    pub(crate) frame_height_bits: u8,
    pub(crate) frame_id_numbers_present: bool,
    pub(crate) delta_frame_id_length: u8,
    pub(crate) frame_id_length: u8,
    pub(crate) use_128x128_superblock: bool,
    pub(crate) enable_filter_intra: bool,
    pub(crate) enable_intra_edge_filter: bool,
    pub(crate) enable_interintra_compound: bool,
    pub(crate) enable_masked_compound: bool,
    pub(crate) enable_warped_motion: bool,
    pub(crate) enable_dual_filter: bool,
    pub(crate) enable_order_hint: bool,
    pub(crate) enable_joint_compound: bool,
    pub(crate) enable_ref_frame_mvs: bool,
    pub(crate) screen_content_tools: u8,
    pub(crate) integer_mv: u8,
    pub(crate) order_hint_bits: u8,
    pub(crate) enable_superres: bool,
    pub(crate) enable_cdef: bool,
    pub(crate) enable_restoration: bool,
    pub(crate) color: ColorConfig,
    pub(crate) film_grain_params_present: bool,
    pub(crate) decoder_model: Option<DecoderModelInfo>,
}

impl SequenceHeader {
    pub(crate) fn parse_obu(input: &[u8]) -> VideoResult<Self> {
        let obu = parse_obu(input)?;
        if obu.kind != ObuType::SEQUENCE_HEADER {
            return Err(av1_error(
                "SequenceHeader::parse_obu",
                "OBU is not a sequence header",
            ));
        }
        Self::parse_payload(obu.payload)
    }

    #[expect(
        clippy::too_many_lines,
        reason = "ordered AV1 sequence-header grammar mirrors FFmpeg"
    )]
    fn parse_payload(payload: &[u8]) -> VideoResult<Self> {
        let mut bits = BitReader::new(payload);
        let profile = read_u8(&mut bits, 3)?;
        if profile != 0 {
            return Err(VideoError::new(
                VideoErrorKind::Unsupported,
                "SequenceHeader::parse_payload",
                "first release supports AV1 Main profile only",
            ));
        }
        let still_picture = bits.read_bit()?;
        let reduced = bits.read_bit()?;
        if reduced && !still_picture {
            return Err(av1_error(
                "SequenceHeader::parse_payload",
                "reduced header requires still_picture",
            ));
        }
        let mut decoder_model = None;
        if reduced {
            bits.read_bits(5)?;
        } else {
            let timing_present = bits.read_bit()?;
            let mut equal_picture_interval = false;
            let mut decoder_model_present_in_sequence = false;
            let mut buffer_delay_bits = 0_u8;
            let mut buffer_removal_time_bits = 0_u8;
            let mut frame_presentation_time_bits = 0_u8;
            if timing_present {
                bits.read_bits(32)?;
                bits.read_bits(32)?;
                equal_picture_interval = bits.read_bit()?;
                if equal_picture_interval {
                    bits.read_ue()?;
                }
                decoder_model_present_in_sequence = bits.read_bit()?;
                if decoder_model_present_in_sequence {
                    buffer_delay_bits = read_u8(&mut bits, 5)? + 1;
                    bits.read_bits(32)?;
                    buffer_removal_time_bits = read_u8(&mut bits, 5)? + 1;
                    frame_presentation_time_bits = read_u8(&mut bits, 5)? + 1;
                }
            }
            let initial_delay = bits.read_bit()?;
            let operating_points = read_u8(&mut bits, 5)? + 1;
            let mut operating_point_idc = [0_u16; 32];
            let mut decoder_model_present = [false; 32];
            for index in 0..usize::from(operating_points) {
                operating_point_idc[index] = u16::try_from(bits.read_bits(12)?).map_err(|_| {
                    av1_error(
                        "SequenceHeader::parse_payload",
                        "operating point exceeds u16",
                    )
                })?;
                let level = bits.read_bits(5)?;
                if level > 7 {
                    bits.read_bit()?;
                }
                if decoder_model_present_in_sequence {
                    decoder_model_present[index] = bits.read_bit()?;
                    if decoder_model_present[index] {
                        bits.skip_bits(usize::from(buffer_delay_bits) * 2)?;
                        bits.read_bit()?; // low_delay_mode_flag
                    }
                }
                if initial_delay && bits.read_bit()? {
                    bits.read_bits(4)?;
                }
            }
            if decoder_model_present_in_sequence {
                decoder_model = Some(DecoderModelInfo {
                    equal_picture_interval,
                    buffer_removal_time_bits,
                    frame_presentation_time_bits,
                    operating_point_idc,
                    decoder_model_present,
                });
            }
        }
        let width_bits = read_u8(&mut bits, 4)? + 1;
        let height_bits = read_u8(&mut bits, 4)? + 1;
        let max_frame_width = read_dimension(&mut bits, width_bits)?;
        let max_frame_height = read_dimension(&mut bits, height_bits)?;
        let frame_id_numbers_present = !reduced && bits.read_bit()?;
        let (delta_frame_id_length, frame_id_length) = if frame_id_numbers_present {
            let delta = read_u8(&mut bits, 4)? + 2;
            let additional = read_u8(&mut bits, 3)? + 1;
            (delta, delta + additional)
        } else {
            (0, 0)
        };
        let use_128x128_superblock = bits.read_bit()?;
        let enable_filter_intra = bits.read_bit()?;
        let enable_intra_edge_filter = bits.read_bit()?;
        let (
            enable_interintra_compound,
            enable_masked_compound,
            enable_warped_motion,
            enable_dual_filter,
            enable_order_hint,
            enable_joint_compound,
            enable_ref_frame_mvs,
            screen_content_tools,
            integer_mv,
            order_hint_bits,
        ) = if reduced {
            (false, false, false, false, false, false, false, 2, 2, 0)
        } else {
            let interintra = bits.read_bit()?;
            let masked = bits.read_bit()?;
            let warped = bits.read_bit()?;
            let dual = bits.read_bit()?;
            let order = bits.read_bit()?;
            let (joint, ref_mvs) = if order {
                (bits.read_bit()?, bits.read_bit()?)
            } else {
                (false, false)
            };
            let choose_screen = bits.read_bit()?;
            let screen = if choose_screen {
                2
            } else {
                u8::from(bits.read_bit()?)
            };
            let integer = if screen > 0 {
                let choose_integer = bits.read_bit()?;
                if choose_integer {
                    2
                } else {
                    u8::from(bits.read_bit()?)
                }
            } else {
                2
            };
            let hint_bits = if order { read_u8(&mut bits, 3)? + 1 } else { 0 };
            (
                interintra, masked, warped, dual, order, joint, ref_mvs, screen, integer, hint_bits,
            )
        };
        let enable_superres = bits.read_bit()?;
        let enable_cdef = bits.read_bit()?;
        let enable_restoration = bits.read_bit()?;
        let color = parse_color_config(&mut bits, profile)?;
        let film_grain_params_present = bits.read_bit()?;
        Ok(Self {
            profile,
            still_picture,
            reduced_still_picture_header: reduced,
            max_frame_width,
            max_frame_height,
            frame_width_bits: width_bits,
            frame_height_bits: height_bits,
            frame_id_numbers_present,
            delta_frame_id_length,
            frame_id_length,
            use_128x128_superblock,
            enable_filter_intra,
            enable_intra_edge_filter,
            enable_interintra_compound,
            enable_masked_compound,
            enable_warped_motion,
            enable_dual_filter,
            enable_order_hint,
            enable_joint_compound,
            enable_ref_frame_mvs,
            screen_content_tools,
            integer_mv,
            order_hint_bits,
            enable_superres,
            enable_cdef,
            enable_restoration,
            color,
            film_grain_params_present,
            decoder_model,
        })
    }
}

fn parse_color_config(bits: &mut BitReader<'_>, profile: u8) -> VideoResult<ColorConfig> {
    let bit_depth = if bits.read_bit()? { 10 } else { 8 };
    let monochrome = bits.read_bit()?;
    let description = bits.read_bit()?;
    let (primaries, transfer, matrix) = if description {
        (read_u8(bits, 8)?, read_u8(bits, 8)?, read_u8(bits, 8)?)
    } else {
        (2, 2, 2)
    };
    let rgb_identity = !monochrome && primaries == 1 && transfer == 13 && matrix == 0;
    let (full_range, chroma_sample_position, separate_uv_delta_q) = if monochrome {
        (bits.read_bit()?, 0, false)
    } else if rgb_identity {
        (true, 0, bits.read_bit()?)
    } else {
        let range = bits.read_bit()?;
        let position = if profile == 0 { read_u8(bits, 2)? } else { 0 };
        (range, position, bits.read_bit()?)
    };
    Ok(ColorConfig {
        bit_depth,
        monochrome,
        color_primaries: primaries,
        transfer_characteristics: transfer,
        matrix_coefficients: matrix,
        full_range,
        chroma_sample_position,
        separate_uv_delta_q,
    })
}

fn read_u8(bits: &mut BitReader<'_>, count: u8) -> VideoResult<u8> {
    u8::try_from(bits.read_bits(count)?)
        .map_err(|_| av1_error("SequenceHeader", "field exceeds u8"))
}

fn read_dimension(bits: &mut BitReader<'_>, count: u8) -> VideoResult<u16> {
    let value = bits
        .read_bits(count)?
        .checked_add(1)
        .ok_or_else(|| av1_error("SequenceHeader", "dimension overflowed"))?;
    u16::try_from(value).map_err(|_| av1_error("SequenceHeader", "dimension exceeds u16"))
}
