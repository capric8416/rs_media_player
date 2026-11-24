use super::av1_error;
use super::refs::{ReferenceSlots, derive_short_refs, relative_distance};
use super::sequence::SequenceHeader;
use super::tile::{TileLayout, read_increment};
use crate::VideoResult;
use crate::bit_reader::BitReader;
use crate::obu::{ObuType, parse_obu};

const DEFAULT_AV1_LOOP_FILTER_REF_DELTAS: [i8; 8] = [1, 0, 0, 0, -1, 0, -1, -1];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum FrameType {
    Key = 0,
    Inter = 1,
    IntraOnly = 2,
    Switch = 3,
}

impl FrameType {
    fn from_bits(value: u8) -> Self {
        match value {
            0 => Self::Key,
            1 => Self::Inter,
            2 => Self::IntraOnly,
            _ => Self::Switch,
        }
    }

    pub(crate) const fn is_intra(self) -> bool {
        matches!(self, Self::Key | Self::IntraOnly)
    }
}

/// The uncompressed-header prefix through AV1's seven inter-reference indices.
///
/// Later picture-tool syntax is intentionally handled by the hardware picture-parameter unit.
#[derive(Clone, Debug, Eq, PartialEq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "fields mirror independent AV1 frame-header syntax flags"
)]
pub(crate) struct FrameReferenceHeader {
    pub(crate) show_existing_frame: bool,
    pub(crate) frame_to_show_map_idx: Option<u8>,
    pub(crate) frame_type: FrameType,
    pub(crate) show_frame: bool,
    pub(crate) showable_frame: bool,
    pub(crate) error_resilient_mode: bool,
    pub(crate) disable_cdf_update: bool,
    pub(crate) allow_screen_content_tools: bool,
    pub(crate) force_integer_mv: bool,
    pub(crate) current_frame_id: u32,
    pub(crate) frame_size_override: bool,
    pub(crate) order_hint: u32,
    pub(crate) primary_ref_frame: u8,
    pub(crate) refresh_frame_flags: u8,
    pub(crate) frame_refs_short_signaling: bool,
    pub(crate) ref_frame_idx: [u8; 7],
    pub(crate) header_bit_position: usize,
    pub(crate) next_slots: super::refs::ReferenceSlots,
}

impl FrameReferenceHeader {
    #[expect(
        clippy::too_many_lines,
        reason = "ordered AV1 uncompressed-header grammar mirrors FFmpeg"
    )]
    pub(crate) fn parse_obu(
        input: &[u8],
        sequence: &SequenceHeader,
        slots: &ReferenceSlots,
    ) -> VideoResult<Self> {
        let obu = parse_obu(input)?;
        if obu.kind != ObuType::FRAME_HEADER
            && obu.kind != ObuType::FRAME
            && obu.kind != ObuType::REDUNDANT_FRAME_HEADER
        {
            return Err(av1_error(
                "FrameReferenceHeader::parse_obu",
                "OBU does not carry a frame header",
            ));
        }
        let mut bits = BitReader::new(obu.payload);
        let mut valid_slots = slots.clone();

        let show_existing_frame = !sequence.reduced_still_picture_header && bits.read_bit()?;
        if show_existing_frame {
            let index = read_u8(&mut bits, 3)?;
            let reference = valid_slots.get(index)?;
            if let Some(model) = &sequence.decoder_model
                && !model.equal_picture_interval
            {
                bits.skip_bits(usize::from(model.frame_presentation_time_bits))?;
            }
            if sequence.frame_id_numbers_present {
                let display_id = read_u32(&mut bits, sequence.frame_id_length)?;
                if display_id != reference.frame_id {
                    return Err(av1_error(
                        "FrameReferenceHeader::parse_obu",
                        "display_frame_id does not match the selected slot",
                    ));
                }
            }
            let frame_type = FrameType::from_bits(reference.frame_type);
            return Ok(Self {
                show_existing_frame: true,
                frame_to_show_map_idx: Some(index),
                frame_type,
                show_frame: true,
                showable_frame: false,
                error_resilient_mode: false,
                disable_cdf_update: false,
                allow_screen_content_tools: false,
                force_integer_mv: false,
                current_frame_id: reference.frame_id,
                frame_size_override: false,
                order_hint: reference.order_hint,
                primary_ref_frame: 7,
                refresh_frame_flags: if frame_type == FrameType::Key {
                    0xff
                } else {
                    0
                },
                frame_refs_short_signaling: false,
                ref_frame_idx: [0; 7],
                header_bit_position: bits.position(),
                next_slots: valid_slots,
            });
        }

        let frame_type = if sequence.reduced_still_picture_header {
            FrameType::Key
        } else {
            FrameType::from_bits(read_u8(&mut bits, 2)?)
        };
        let show_frame = sequence.reduced_still_picture_header || bits.read_bit()?;
        if show_frame
            && let Some(model) = &sequence.decoder_model
            && !model.equal_picture_interval
        {
            bits.skip_bits(usize::from(model.frame_presentation_time_bits))?;
        }
        let showable_frame = if sequence.reduced_still_picture_header {
            false
        } else if show_frame {
            frame_type != FrameType::Key
        } else {
            bits.read_bit()?
        };
        let error_resilient_mode = frame_type == FrameType::Switch
            || (frame_type == FrameType::Key && show_frame)
            || (!sequence.reduced_still_picture_header && bits.read_bit()?);
        let disable_cdf_update = bits.read_bit()?;
        let allow_screen_content_tools = if sequence.screen_content_tools == 2 {
            bits.read_bit()?
        } else {
            sequence.screen_content_tools != 0
        };
        let force_integer_mv = if allow_screen_content_tools {
            if sequence.integer_mv == 2 {
                bits.read_bit()?
            } else {
                sequence.integer_mv != 0
            }
        } else {
            false
        };
        let current_frame_id = if sequence.frame_id_numbers_present {
            let value = read_u32(&mut bits, sequence.frame_id_length)?;
            valid_slots.invalidate_stale(
                value,
                sequence.frame_id_length,
                sequence.delta_frame_id_length,
            );
            value
        } else {
            0
        };
        let frame_size_override = if frame_type == FrameType::Switch {
            true
        } else if sequence.reduced_still_picture_header {
            false
        } else {
            bits.read_bit()?
        };
        let order_hint = read_u32(&mut bits, sequence.order_hint_bits)?;
        let primary_ref_frame = if frame_type.is_intra() || error_resilient_mode {
            7
        } else {
            read_u8(&mut bits, 3)?
        };
        if let Some(model) = &sequence.decoder_model
            && bits.read_bit()?
        {
            for (&operating_point, &present) in model
                .operating_point_idc
                .iter()
                .zip(&model.decoder_model_present)
            {
                let in_temporal_layer = operating_point & (1_u16 << obu.temporal_id) != 0;
                let in_spatial_layer = operating_point & (1_u16 << (obu.spatial_id + 8)) != 0;
                if present && (operating_point == 0 || (in_temporal_layer && in_spatial_layer)) {
                    bits.skip_bits(usize::from(model.buffer_removal_time_bits))?;
                }
            }
        }
        let refresh_frame_flags =
            if frame_type == FrameType::Switch || (frame_type == FrameType::Key && show_frame) {
                0xff
            } else {
                read_u8(&mut bits, 8)?
            };

        if (!frame_type.is_intra() || refresh_frame_flags != 0xff)
            && sequence.enable_order_hint
            && error_resilient_mode
        {
            for index in 0..8_u8 {
                let reference_hint = read_u32(&mut bits, sequence.order_hint_bits)?;
                valid_slots.invalidate_order_mismatch(index, reference_hint);
            }
        }

        let mut frame_refs_short_signaling = false;
        let mut ref_frame_idx = [0_u8; 7];
        if !frame_type.is_intra() {
            frame_refs_short_signaling = sequence.enable_order_hint && bits.read_bit()?;
            if frame_refs_short_signaling {
                let last = read_u8(&mut bits, 3)?;
                let golden = read_u8(&mut bits, 3)?;
                ref_frame_idx = derive_short_refs(
                    &valid_slots,
                    order_hint,
                    sequence.order_hint_bits,
                    last,
                    golden,
                )?;
                if sequence.frame_id_numbers_present {
                    for reference in ref_frame_idx {
                        validate_frame_id(
                            &mut bits,
                            sequence,
                            &valid_slots,
                            current_frame_id,
                            reference,
                        )?;
                    }
                }
            } else {
                for reference in &mut ref_frame_idx {
                    *reference = read_u8(&mut bits, 3)?;
                    valid_slots.get(*reference)?;
                    if sequence.frame_id_numbers_present {
                        validate_frame_id(
                            &mut bits,
                            sequence,
                            &valid_slots,
                            current_frame_id,
                            *reference,
                        )?;
                    }
                }
            }
        }

        Ok(Self {
            show_existing_frame: false,
            frame_to_show_map_idx: None,
            frame_type,
            show_frame,
            showable_frame,
            error_resilient_mode,
            disable_cdf_update,
            allow_screen_content_tools,
            force_integer_mv,
            current_frame_id,
            frame_size_override,
            order_hint,
            primary_ref_frame,
            refresh_frame_flags,
            frame_refs_short_signaling,
            ref_frame_idx,
            header_bit_position: bits.position(),
            next_slots: valid_slots,
        })
    }
}

/// Parsed intra-frame dimensions, super-resolution state, and tile layout for DXVA setup.
#[derive(Clone, Debug, Eq, PartialEq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "header fields mirror independent AV1 dimensions and tool flags"
)]
pub(crate) struct IntraFrameTileHeader {
    pub(crate) width: u16,
    pub(crate) height: u16,
    pub(crate) upscaled_width: u16,
    pub(crate) render_width: u32,
    pub(crate) render_height: u32,
    pub(crate) superres_denom: u8,
    pub(crate) allow_intrabc: bool,
    pub(crate) disable_frame_end_update_cdf: bool,
    pub(crate) allow_high_precision_mv: bool,
    pub(crate) interpolation_filter: u8,
    pub(crate) is_motion_mode_switchable: bool,
    pub(crate) use_ref_frame_mvs: bool,
    pub(crate) tile_layout: TileLayout,
    pub(crate) header_bit_position: usize,
}

impl IntraFrameTileHeader {
    /// Parses the intra-only frame-header syntax through AV1 tile information.
    #[expect(
        clippy::too_many_lines,
        reason = "AV1 frame sizing and tile-header syntax are parsed in normative order"
    )]
    pub(crate) fn parse_obu(
        input: &[u8],
        sequence: &SequenceHeader,
        reference: &FrameReferenceHeader,
    ) -> VideoResult<Self> {
        if reference.show_existing_frame {
            return Err(av1_error(
                "IntraFrameTileHeader::parse_obu",
                "show-existing frames do not have a tile header",
            ));
        }
        let obu = parse_obu(input)?;
        if !matches!(
            obu.kind,
            ObuType::FRAME_HEADER | ObuType::FRAME | ObuType::REDUNDANT_FRAME_HEADER
        ) {
            return Err(av1_error(
                "IntraFrameTileHeader::parse_obu",
                "OBU does not contain a frame header",
            ));
        }
        let mut bits = BitReader::new(obu.payload);
        bits.skip_bits(reference.header_bit_position)?;
        let (upscaled_width, height, width, render_width, render_height, superres_denom) =
            if !reference.frame_type.is_intra()
                && reference.frame_size_override
                && !reference.error_resilient_mode
            {
                let mut found = None;
                for reference_index in reference.ref_frame_idx {
                    if bits.read_bit()? {
                        let slot = reference.next_slots.get(reference_index)?;
                        found = Some((
                            slot.width,
                            slot.height,
                            slot.render_width,
                            slot.render_height,
                        ));
                        break;
                    }
                }
                if let Some((width, height, render_width, render_height)) = found {
                    let superres_denom = if sequence.enable_superres && bits.read_bit()? {
                        read_u8(&mut bits, 3)? + 9
                    } else {
                        8
                    };
                    let coded_width = if superres_denom == 8 {
                        width
                    } else {
                        u16::try_from(
                            (u32::from(width) * 8 + u32::from(superres_denom / 2))
                                / u32::from(superres_denom),
                        )
                        .map_err(|_| av1_error("frame_size_with_refs", "coded width exceeds u16"))?
                    };
                    (
                        width,
                        height,
                        coded_width,
                        render_width,
                        render_height,
                        superres_denom,
                    )
                } else {
                    parse_frame_size(&mut bits, sequence, reference.frame_size_override)?
                }
            } else {
                parse_frame_size(&mut bits, sequence, reference.frame_size_override)?
            };
        let allow_intrabc = if reference.frame_type.is_intra()
            && reference.allow_screen_content_tools
            && superres_denom == 8
        {
            bits.read_bit()?
        } else {
            false
        };
        let (
            allow_high_precision_mv,
            interpolation_filter,
            is_motion_mode_switchable,
            use_ref_frame_mvs,
        ) = if reference.frame_type.is_intra() {
            (false, 0, false, false)
        } else {
            let high_precision = if reference.force_integer_mv {
                false
            } else {
                bits.read_bit()?
            };
            let filter_switchable = bits.read_bit()?;
            let interpolation = if filter_switchable {
                4
            } else {
                read_u8(&mut bits, 2)?
            };
            let motion_mode_switchable = bits.read_bit()?;
            let use_ref_mvs = if sequence.enable_ref_frame_mvs && !reference.error_resilient_mode {
                bits.read_bit()?
            } else {
                false
            };
            (
                high_precision,
                interpolation,
                motion_mode_switchable,
                use_ref_mvs,
            )
        };
        let disable_frame_end_update_cdf = reference.disable_cdf_update || bits.read_bit()?;
        let tile_layout = TileLayout::parse_bits(&mut bits, sequence, width, height)?;
        let header_bit_position = bits.position();
        Ok(Self {
            width,
            height,
            upscaled_width,
            render_width,
            render_height,
            superres_denom,
            allow_intrabc,
            disable_frame_end_update_cdf,
            allow_high_precision_mv,
            interpolation_filter,
            is_motion_mode_switchable,
            use_ref_frame_mvs,
            tile_layout,
            header_bit_position,
        })
    }
}

fn parse_frame_size(
    bits: &mut BitReader<'_>,
    sequence: &SequenceHeader,
    frame_size_override: bool,
) -> VideoResult<(u16, u16, u16, u32, u32, u8)> {
    let upscaled_width = if frame_size_override {
        read_u16(bits, sequence.frame_width_bits)?
    } else {
        sequence.max_frame_width
    };
    let height = if frame_size_override {
        read_u16(bits, sequence.frame_height_bits)?
    } else {
        sequence.max_frame_height
    };
    let superres_denom = if sequence.enable_superres && bits.read_bit()? {
        read_u8(bits, 3)? + 9
    } else {
        8
    };
    let width = if superres_denom == 8 {
        upscaled_width
    } else {
        let scaled = u32::from(upscaled_width)
            .checked_mul(8)
            .and_then(|value| value.checked_add(u32::from(superres_denom / 2)))
            .ok_or_else(|| av1_error("parse_frame_size", "super-resolution width overflowed"))?
            / u32::from(superres_denom);
        u16::try_from(scaled)
            .map_err(|_| av1_error("parse_frame_size", "coded width exceeds u16"))?
    };
    let (render_width, render_height) = if bits.read_bit()? {
        (
            u32::try_from(bits.read_bits(16)?)
                .map_err(|_| av1_error("parse_frame_size", "render width exceeds u32"))?
                + 1,
            u32::try_from(bits.read_bits(16)?)
                .map_err(|_| av1_error("parse_frame_size", "render height exceeds u32"))?
                + 1,
        )
    } else {
        (u32::from(upscaled_width), u32::from(height))
    };
    Ok((
        upscaled_width,
        height,
        width,
        render_width,
        render_height,
        superres_denom,
    ))
}

/// AV1 quantization fields consumed by the DXVA picture-parameter mapping.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Av1QuantizationParams {
    pub(crate) base_qindex: u8,
    pub(crate) y_dc_delta_q: i8,
    pub(crate) u_dc_delta_q: i8,
    pub(crate) v_dc_delta_q: i8,
    pub(crate) u_ac_delta_q: i8,
    pub(crate) v_ac_delta_q: i8,
    pub(crate) using_qmatrix: bool,
    pub(crate) qm_y: u8,
    pub(crate) qm_u: u8,
    pub(crate) qm_v: u8,
    pub(crate) header_bit_position: usize,
}

impl Av1QuantizationParams {
    /// Parses AV1 quantization parameters immediately after tile information.
    #[expect(
        clippy::similar_names,
        reason = "AV1 names quantizer deltas independently for Y, U, and V planes"
    )]
    pub(crate) fn parse_obu(
        input: &[u8],
        sequence: &SequenceHeader,
        frame: &IntraFrameTileHeader,
    ) -> VideoResult<Self> {
        let obu = parse_obu(input)?;
        let mut bits = BitReader::new(obu.payload);
        bits.skip_bits(frame.header_bit_position)?;
        let base_qindex = read_u8(&mut bits, 8)?;
        let y_dc_delta_q = read_delta_q(&mut bits)?;
        let (u_dc_delta_q, u_ac_delta_q, v_dc_delta_q, v_ac_delta_q) = if sequence.color.monochrome
        {
            (0, 0, 0, 0)
        } else {
            if sequence.color.separate_uv_delta_q {
                bits.read_bit()?;
            }
            let u_dc = read_delta_q(&mut bits)?;
            let u_ac = read_delta_q(&mut bits)?;
            let (v_dc, v_ac) = if sequence.color.separate_uv_delta_q {
                (read_delta_q(&mut bits)?, read_delta_q(&mut bits)?)
            } else {
                (u_dc, u_ac)
            };
            (u_dc, u_ac, v_dc, v_ac)
        };
        let using_qmatrix = bits.read_bit()?;
        let (qm_y, qm_u, qm_v) = if using_qmatrix {
            let y = read_u8(&mut bits, 4)?;
            let u = read_u8(&mut bits, 4)?;
            let v = if sequence.color.separate_uv_delta_q {
                read_u8(&mut bits, 4)?
            } else {
                u
            };
            (y, u, v)
        } else {
            (0xff, 0xff, 0xff)
        };
        let header_bit_position = bits.position();
        Ok(Self {
            base_qindex,
            y_dc_delta_q,
            u_dc_delta_q,
            v_dc_delta_q,
            u_ac_delta_q,
            v_ac_delta_q,
            using_qmatrix,
            qm_y,
            qm_u,
            qm_v,
            header_bit_position,
        })
    }
}

/// AV1 segmentation feature flags and values for DXVA picture parameters.
#[derive(Clone, Debug, Eq, PartialEq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "fields mirror independent AV1 segmentation syntax flags"
)]
pub(crate) struct Av1SegmentationParams {
    pub(crate) enabled: bool,
    pub(crate) update_map: bool,
    pub(crate) temporal_update: bool,
    pub(crate) update_data: bool,
    pub(crate) feature_enabled: [[bool; 8]; 8],
    pub(crate) feature_data: [[i16; 8]; 8],
    pub(crate) header_bit_position: usize,
}

impl Av1SegmentationParams {
    /// Parses segmentation syntax for key/intra frames without a primary reference.
    pub(crate) fn parse_obu(
        input: &[u8],
        quantization: &Av1QuantizationParams,
    ) -> VideoResult<Self> {
        const FEATURE_BITS: [u8; 8] = [8, 6, 6, 6, 6, 3, 0, 0];
        const FEATURE_SIGNED: [bool; 8] = [true, true, true, true, true, false, false, false];
        let obu = parse_obu(input)?;
        let mut bits = BitReader::new(obu.payload);
        bits.skip_bits(quantization.header_bit_position)?;
        let enabled = bits.read_bit()?;
        // With primary_ref_frame == PRIMARY_REF_NONE, update_map and update_data are inferred.
        let (update_map, temporal_update, update_data) = if enabled {
            (true, false, true)
        } else {
            (false, false, false)
        };
        let mut feature_enabled = [[false; 8]; 8];
        let mut feature_data = [[0_i16; 8]; 8];
        if enabled {
            for segment in 0..8 {
                for feature in 0..8 {
                    if bits.read_bit()? {
                        feature_enabled[segment][feature] = true;
                        let width = FEATURE_BITS[feature];
                        if width > 0 {
                            let total_width = width + u8::from(FEATURE_SIGNED[feature]);
                            let value = bits.read_bits(total_width)?;
                            feature_data[segment][feature] = if FEATURE_SIGNED[feature] {
                                read_signed_value(value, total_width)?
                            } else {
                                i16::try_from(value).map_err(|_| {
                                    av1_error(
                                        "Av1SegmentationParams::parse_obu",
                                        "feature value exceeds i16",
                                    )
                                })?
                            };
                        }
                    }
                }
            }
        }
        let header_bit_position = bits.position();
        Ok(Self {
            enabled,
            update_map,
            temporal_update,
            update_data,
            feature_enabled,
            feature_data,
            header_bit_position,
        })
    }
}

/// AV1 frame-level delta-Q and delta-loop-filter controls.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Av1DeltaParams {
    pub(crate) delta_q_present: bool,
    pub(crate) delta_q_res: u8,
    pub(crate) delta_lf_present: bool,
    pub(crate) delta_lf_res: u8,
    pub(crate) delta_lf_multi: bool,
    pub(crate) header_bit_position: usize,
}

impl Av1DeltaParams {
    /// Parses delta-Q and delta-LF syntax following segmentation parameters.
    pub(crate) fn parse_obu(
        input: &[u8],
        quantization: &Av1QuantizationParams,
        segmentation: &Av1SegmentationParams,
        allow_intrabc: bool,
    ) -> VideoResult<Self> {
        let obu = parse_obu(input)?;
        let mut bits = BitReader::new(obu.payload);
        bits.skip_bits(segmentation.header_bit_position)?;
        let delta_q_present = quantization.base_qindex > 0 && bits.read_bit()?;
        let delta_q_res = if delta_q_present {
            read_u8(&mut bits, 2)?
        } else {
            0
        };
        let delta_lf_present = if delta_q_present && !allow_intrabc {
            bits.read_bit()?
        } else {
            false
        };
        let (delta_lf_res, delta_lf_multi) = if delta_lf_present {
            (read_u8(&mut bits, 2)?, bits.read_bit()?)
        } else {
            (0, false)
        };
        let header_bit_position = bits.position();
        Ok(Self {
            delta_q_present,
            delta_q_res,
            delta_lf_present,
            delta_lf_res,
            delta_lf_multi,
            header_bit_position,
        })
    }
}

/// AV1 loop-filter levels, sharpness, and reference/mode deltas.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Av1LoopFilterParams {
    pub(crate) levels: [u8; 4],
    pub(crate) sharpness: u8,
    pub(crate) delta_enabled: bool,
    pub(crate) delta_update: bool,
    pub(crate) ref_deltas: [i8; 8],
    pub(crate) mode_deltas: [i8; 2],
    pub(crate) header_bit_position: usize,
}

impl Av1LoopFilterParams {
    /// Parses loop-filter fields for key/intra frames without reference-derived state.
    pub(crate) fn parse_obu(
        input: &[u8],
        sequence: &SequenceHeader,
        frame: &IntraFrameTileHeader,
        quantization: &Av1QuantizationParams,
        segmentation: &Av1SegmentationParams,
        deltas: &Av1DeltaParams,
    ) -> VideoResult<Self> {
        let obu = parse_obu(input)?;
        let mut bits = BitReader::new(obu.payload);
        bits.skip_bits(deltas.header_bit_position)?;
        if is_coded_lossless(quantization, segmentation) || frame.allow_intrabc {
            return Ok(Self {
                levels: [0; 4],
                sharpness: 0,
                delta_enabled: false,
                delta_update: false,
                ref_deltas: DEFAULT_AV1_LOOP_FILTER_REF_DELTAS,
                mode_deltas: [0; 2],
                header_bit_position: bits.position(),
            });
        }
        let mut levels = [0; 4];
        levels[0] = read_u8(&mut bits, 6)?;
        levels[1] = read_u8(&mut bits, 6)?;
        if !sequence.color.monochrome && (levels[0] != 0 || levels[1] != 0) {
            levels[2] = read_u8(&mut bits, 6)?;
            levels[3] = read_u8(&mut bits, 6)?;
        }
        let sharpness = read_u8(&mut bits, 3)?;
        let delta_enabled = bits.read_bit()?;
        let mut delta_update = false;
        let mut ref_deltas = DEFAULT_AV1_LOOP_FILTER_REF_DELTAS;
        let mut mode_deltas = [0; 2];
        if delta_enabled {
            delta_update = bits.read_bit()?;
            if delta_update {
                for value in &mut ref_deltas {
                    if bits.read_bit()? {
                        *value = read_signed_7(&mut bits, "loop-filter reference delta")?;
                    }
                }
                for value in &mut mode_deltas {
                    if bits.read_bit()? {
                        *value = read_signed_7(&mut bits, "loop-filter mode delta")?;
                    }
                }
            }
        }
        let header_bit_position = bits.position();
        Ok(Self {
            levels,
            sharpness,
            delta_enabled,
            delta_update,
            ref_deltas,
            mode_deltas,
            header_bit_position,
        })
    }
}

/// AV1 CDEF strengths for luma and chroma planes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Av1CdefParams {
    pub(crate) damping: u8,
    pub(crate) bits: u8,
    pub(crate) y_strengths: [[u8; 2]; 8],
    pub(crate) uv_strengths: [[u8; 2]; 8],
    pub(crate) header_bit_position: usize,
}

impl Av1CdefParams {
    /// Parses CDEF syntax after loop-filter parameters.
    pub(crate) fn parse_obu(
        input: &[u8],
        sequence: &SequenceHeader,
        frame: &IntraFrameTileHeader,
        quantization: &Av1QuantizationParams,
        segmentation: &Av1SegmentationParams,
        loop_filter: &Av1LoopFilterParams,
    ) -> VideoResult<Self> {
        let obu = parse_obu(input)?;
        let mut reader = BitReader::new(obu.payload);
        reader.skip_bits(loop_filter.header_bit_position)?;
        let mut result = Self {
            damping: 0,
            bits: 0,
            y_strengths: [[0; 2]; 8],
            uv_strengths: [[0; 2]; 8],
            header_bit_position: reader.position(),
        };
        if is_coded_lossless(quantization, segmentation)
            || frame.allow_intrabc
            || !sequence.enable_cdef
        {
            return Ok(result);
        }
        result.damping = read_u8(&mut reader, 2)?;
        result.bits = read_u8(&mut reader, 2)?;
        for index in 0..(1_usize << result.bits) {
            result.y_strengths[index] = [read_u8(&mut reader, 4)?, read_u8(&mut reader, 2)?];
            if !sequence.color.monochrome {
                result.uv_strengths[index] = [read_u8(&mut reader, 4)?, read_u8(&mut reader, 2)?];
            }
        }
        result.header_bit_position = reader.position();
        Ok(result)
    }
}

/// AV1 loop-restoration types and unit-size controls.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Av1RestorationParams {
    pub(crate) types: [u8; 3],
    pub(crate) unit_shift: u8,
    pub(crate) uv_shift: u8,
    pub(crate) header_bit_position: usize,
}

/// AV1 film-grain parameters in the layout consumed by DXVA.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "fields mirror independent AV1 film-grain syntax flags"
)]
pub(crate) struct Av1FilmGrainParams {
    pub(crate) apply_grain: bool,
    pub(crate) update_grain: bool,
    pub(crate) grain_seed: u16,
    pub(crate) reference_index: u8,
    pub(crate) chroma_scaling_from_luma: bool,
    pub(crate) scaling_shift_minus8: u8,
    pub(crate) ar_coeff_lag: u8,
    pub(crate) ar_coeffs_y: [u8; 24],
    pub(crate) ar_coeffs_cb: [u8; 25],
    pub(crate) ar_coeffs_cr: [u8; 25],
    pub(crate) ar_coeff_shift_minus6: u8,
    pub(crate) grain_scale_shift: u8,
    pub(crate) scaling_points_y: [[u8; 2]; 14],
    pub(crate) num_y_points: u8,
    pub(crate) scaling_points_cb: [[u8; 2]; 10],
    pub(crate) num_cb_points: u8,
    pub(crate) scaling_points_cr: [[u8; 2]; 10],
    pub(crate) num_cr_points: u8,
    pub(crate) cb_mult: u8,
    pub(crate) cb_luma_mult: u8,
    pub(crate) cb_offset: u16,
    pub(crate) cr_mult: u8,
    pub(crate) cr_luma_mult: u8,
    pub(crate) cr_offset: u16,
    pub(crate) overlap_flag: bool,
    pub(crate) clip_to_restricted_range: bool,
    pub(crate) header_bit_position: usize,
}

/// AV1 intra-frame coding tail and film-grain syntax.
#[derive(Clone, Debug, Eq, PartialEq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "tail fields mirror independent AV1 frame-tool flags"
)]
pub(crate) struct Av1IntraFrameTail {
    pub(crate) tx_mode: u8,
    pub(crate) reference_select: bool,
    pub(crate) skip_mode_present: bool,
    pub(crate) allow_warped_motion: bool,
    pub(crate) reduced_tx_set: bool,
    pub(crate) global_motion: [Av1GlobalMotion; 7],
    pub(crate) film_grain: Av1FilmGrainParams,
    pub(crate) header_bit_position: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Av1GlobalMotion {
    pub(crate) model: u8,
    pub(crate) params: [i32; 6],
    pub(crate) invalid: bool,
}

impl Default for Av1GlobalMotion {
    fn default() -> Self {
        Self {
            model: 0,
            params: [0, 0, 1 << 16, 0, 0, 1 << 16],
            invalid: false,
        }
    }
}

impl Av1IntraFrameTail {
    /// Parses the syntax after loop restoration through the optional film-grain payload.
    #[expect(
        clippy::too_many_lines,
        reason = "frame-tail tool flags, global motion and grain follow AV1 syntax order"
    )]
    pub(crate) fn parse_obu(
        input: &[u8],
        sequence: &SequenceHeader,
        reference: &FrameReferenceHeader,
        frame: &IntraFrameTileHeader,
        quantization: &Av1QuantizationParams,
        segmentation: &Av1SegmentationParams,
        restoration: &Av1RestorationParams,
    ) -> VideoResult<Self> {
        if reference.show_existing_frame {
            return Err(av1_error(
                "Av1IntraFrameTail::parse_obu",
                "show-existing frames do not have frame-tail syntax",
            ));
        }
        let obu = parse_obu(input)?;
        let mut bits = BitReader::new(obu.payload);
        bits.skip_bits(restoration.header_bit_position)?;
        let tx_mode = if is_coded_lossless(quantization, segmentation) {
            0 // TX_MODE_ONLY_4X4 is inferred.
        } else {
            u8::try_from(read_increment(&mut bits, 1, 2)?)
                .map_err(|_| av1_error("Av1IntraFrameTail", "tx_mode exceeds u8"))?
        };
        let reference_select = if reference.frame_type.is_intra() {
            false
        } else {
            bits.read_bit()?
        };
        let skip_mode_present = if reference_select
            && sequence.enable_order_hint
            && skip_mode_allowed(reference, sequence)?
        {
            bits.read_bit()?
        } else {
            false
        };
        let allow_warped_motion = if !reference.frame_type.is_intra()
            && !reference.error_resilient_mode
            && sequence.enable_warped_motion
        {
            bits.read_bit()?
        } else {
            false
        };
        let reduced_tx_set = bits.read_bit()?;
        let mut global_motion = [Av1GlobalMotion::default(); 7];
        if !reference.frame_type.is_intra() {
            for motion in &mut global_motion {
                if !bits.read_bit()? {
                    continue;
                }
                let rot_zoom = bits.read_bit()?;
                motion.model = if rot_zoom {
                    2
                } else if bits.read_bit()? {
                    1
                } else {
                    3
                };
                if motion.model >= 2 {
                    motion.params[2] = read_global_param(
                        &mut bits,
                        motion.model,
                        2,
                        frame.allow_high_precision_mv,
                    )?;
                    motion.params[3] = read_global_param(
                        &mut bits,
                        motion.model,
                        3,
                        frame.allow_high_precision_mv,
                    )?;
                    if motion.model == 3 {
                        motion.params[4] = read_global_param(
                            &mut bits,
                            motion.model,
                            4,
                            frame.allow_high_precision_mv,
                        )?;
                        motion.params[5] = read_global_param(
                            &mut bits,
                            motion.model,
                            5,
                            frame.allow_high_precision_mv,
                        )?;
                    } else {
                        motion.params[4] = -motion.params[3];
                        motion.params[5] = motion.params[2];
                    }
                }
                if motion.model == 1 || motion.model == 3 {
                    motion.params[0] = read_global_param(
                        &mut bits,
                        motion.model,
                        0,
                        frame.allow_high_precision_mv,
                    )?;
                    motion.params[1] = read_global_param(
                        &mut bits,
                        motion.model,
                        1,
                        frame.allow_high_precision_mv,
                    )?;
                }
                motion.invalid = global_motion_invalid(motion.params);
            }
        }
        let film_grain = parse_film_grain(&mut bits, sequence, reference)?;
        Ok(Self {
            tx_mode,
            reference_select,
            skip_mode_present,
            allow_warped_motion,
            reduced_tx_set,
            global_motion,
            film_grain,
            header_bit_position: bits.position(),
        })
    }
}

fn read_global_param(
    bits: &mut BitReader<'_>,
    model: u8,
    index: usize,
    allow_high_precision_mv: bool,
) -> VideoResult<i32> {
    let (abs_bits, precision_bits) = if index < 2 && model == 1 {
        let adjustment = u32::from(!allow_high_precision_mv);
        (9 - adjustment, 3 - adjustment)
    } else if index < 2 {
        (12, 6)
    } else {
        (12, 15)
    };
    let max = 1_i32 << abs_bits;
    let precision_diff = 16 - precision_bits;
    // PRIMARY_REF_NONE uses setup-past-independence identity parameters.
    let reference = 0;
    let decoded = decode_signed_subexp_with_ref(bits, -max, max + 1, reference)?;
    decoded
        .checked_shl(precision_diff)
        .and_then(|value| value.checked_add(if index % 3 == 2 { 1 << 16 } else { 0 }))
        .ok_or_else(|| {
            av1_error(
                "Av1IntraFrameTail::global_motion",
                "global motion parameter overflowed",
            )
        })
}

fn decode_signed_subexp_with_ref(
    bits: &mut BitReader<'_>,
    low: i32,
    high: i32,
    reference: i32,
) -> VideoResult<i32> {
    let range = u32::try_from(high - low).map_err(|_| {
        av1_error(
            "Av1IntraFrameTail::global_motion",
            "invalid signed subexponential range",
        )
    })?;
    let reference = u32::try_from(reference - low).map_err(|_| {
        av1_error(
            "Av1IntraFrameTail::global_motion",
            "invalid global motion reference",
        )
    })?;
    let value = decode_unsigned_subexp_with_ref(bits, range, reference)?;
    i32::try_from(value)
        .ok()
        .and_then(|value| value.checked_add(low))
        .ok_or_else(|| {
            av1_error(
                "Av1IntraFrameTail::global_motion",
                "decoded parameter exceeds i32",
            )
        })
}

fn decode_unsigned_subexp_with_ref(
    bits: &mut BitReader<'_>,
    symbols: u32,
    reference: u32,
) -> VideoResult<u32> {
    if symbols == 0 || reference >= symbols {
        return Err(av1_error(
            "Av1IntraFrameTail::global_motion",
            "invalid unsigned subexponential range",
        ));
    }
    let value = decode_subexp(bits, symbols)?;
    let recentered = if reference.saturating_mul(2) <= symbols {
        inverse_recenter(reference, value)
    } else {
        symbols - 1 - inverse_recenter(symbols - 1 - reference, value)
    };
    Ok(recentered)
}

fn decode_subexp(bits: &mut BitReader<'_>, symbols: u32) -> VideoResult<u32> {
    let mut index = 0_u32;
    let mut base = 0_u32;
    loop {
        let width = if index == 0 { 2 } else { index + 1 };
        let bucket = 1_u32.checked_shl(width).ok_or_else(|| {
            av1_error(
                "Av1IntraFrameTail::global_motion",
                "subexponential bucket overflowed",
            )
        })?;
        if symbols <= base.saturating_add(bucket.saturating_mul(3)) {
            return read_non_symmetric(bits, symbols - base).map(|value| value + base);
        }
        if !bits.read_bit()? {
            let value = u32::try_from(bits.read_bits(u8::try_from(width).map_err(|_| {
                av1_error(
                    "Av1IntraFrameTail::global_motion",
                    "subexponential width exceeds bit reader",
                )
            })?)?)
            .map_err(|_| {
                av1_error(
                    "Av1IntraFrameTail::global_motion",
                    "subexponential value exceeds u32",
                )
            })?;
            return Ok(base + value);
        }
        base = base.checked_add(bucket).ok_or_else(|| {
            av1_error(
                "Av1IntraFrameTail::global_motion",
                "subexponential base overflowed",
            )
        })?;
        index += 1;
    }
}

fn read_non_symmetric(bits: &mut BitReader<'_>, symbols: u32) -> VideoResult<u32> {
    if symbols <= 1 {
        return Ok(0);
    }
    let width = 32 - (symbols - 1).leading_zeros();
    let cutoff = (1_u32 << width) - symbols;
    let value = u32::try_from(bits.read_bits(u8::try_from(width - 1).map_err(|_| {
        av1_error(
            "Av1IntraFrameTail::global_motion",
            "non-symmetric width exceeds bit reader",
        )
    })?)?)
    .map_err(|_| {
        av1_error(
            "Av1IntraFrameTail::global_motion",
            "non-symmetric value exceeds u32",
        )
    })?;
    if value < cutoff {
        Ok(value)
    } else {
        let extra = u32::from(bits.read_bit()?);
        Ok((value << 1) - cutoff + extra)
    }
}

fn inverse_recenter(reference: u32, value: u32) -> u32 {
    if value > reference.saturating_mul(2) {
        value
    } else if value & 1 != 0 {
        reference - ((value + 1) >> 1)
    } else {
        reference + (value >> 1)
    }
}

fn global_motion_invalid(params: [i32; 6]) -> bool {
    if params[2] <= 0 {
        return true;
    }
    let divisor = u32::try_from(params[2]).unwrap_or(u32::MAX);
    let log = divisor.ilog2();
    let exponent = 1_u32 << log;
    let remainder = divisor - exponent;
    let fraction = if log > 8 {
        round_two(
            u64::from(remainder),
            u16::try_from(log - 8).unwrap_or(u16::MAX),
        )
    } else {
        u64::from(remainder << (8 - log))
    };
    let reciprocal = (1_u64 << 22) / (256 + fraction);
    let shift = log + 14;
    let alpha = clip_i16(params[2] - (1 << 16));
    let beta = clip_i16(params[3]);
    let v = i64::from(params[4]) * (1_i64 << 16);
    let w = i64::from(params[3]) * i64::from(params[4]);
    let reciprocal = i64::try_from(reciprocal).unwrap_or(i64::MAX);
    let gamma = clip_i16(i32::try_from(round_signed(v * reciprocal, shift)).unwrap_or(i32::MAX));
    let delta = clip_i16(
        params[5]
            - i32::try_from(round_signed(w * reciprocal, shift)).unwrap_or(i32::MAX)
            - (1 << 16),
    );
    let alpha = round_signed(i64::from(alpha), 6) << 6;
    let beta = round_signed(i64::from(beta), 6) << 6;
    let gamma = round_signed(i64::from(gamma), 6) << 6;
    let delta = round_signed(i64::from(delta), 6) << 6;
    (4 * alpha.abs() + 7 * beta.abs()) >= (1 << 16)
        || (4 * gamma.abs() + 4 * delta.abs()) >= (1 << 16)
}

fn clip_i16(value: i32) -> i32 {
    value.clamp(i32::from(i16::MIN), i32::from(i16::MAX))
}

fn round_two(value: u64, bits: u16) -> u64 {
    if bits == 0 {
        value
    } else {
        (value + (1_u64 << (bits - 1))) >> bits
    }
}

fn round_signed(value: i64, bits: u32) -> i64 {
    let magnitude = value.unsigned_abs();
    let rounded = if bits == 0 {
        magnitude
    } else {
        (magnitude + (1_u64 << (bits - 1))) >> bits
    };
    let rounded = i64::try_from(rounded).unwrap_or(i64::MAX);
    if value < 0 { -rounded } else { rounded }
}

fn parse_film_grain(
    bits: &mut BitReader<'_>,
    sequence: &SequenceHeader,
    reference: &FrameReferenceHeader,
) -> VideoResult<Av1FilmGrainParams> {
    let mut result = Av1FilmGrainParams::default();
    if !sequence.film_grain_params_present || (!reference.show_frame && !reference.showable_frame) {
        result.header_bit_position = bits.position();
        return Ok(result);
    }
    result.apply_grain = bits.read_bit()?;
    if !result.apply_grain {
        result.header_bit_position = bits.position();
        return Ok(result);
    }
    result.grain_seed = u16::try_from(bits.read_bits(16)?)
        .map_err(|_| av1_error("parse_film_grain", "grain seed exceeds u16"))?;
    result.update_grain = if reference.frame_type.is_intra() {
        true
    } else {
        bits.read_bit()?
    };
    if !result.update_grain {
        let reference_index = read_u8(bits, 3)?;
        let slot = reference.next_slots.get(reference_index)?;
        if !slot.film_grain.apply_grain {
            return Err(av1_error(
                "parse_film_grain",
                "film grain references a slot without stored grain parameters",
            ));
        }
        let mut inherited = slot.film_grain;
        inherited.apply_grain = true;
        inherited.update_grain = false;
        inherited.grain_seed = result.grain_seed;
        inherited.reference_index = reference_index;
        inherited.header_bit_position = bits.position();
        return Ok(inherited);
    }
    result.num_y_points = read_points(&mut result.scaling_points_y, bits, 14, "luma")?;
    if !sequence.color.monochrome {
        result.chroma_scaling_from_luma = bits.read_bit()?;
    }
    let infer_chroma_points = sequence.color.monochrome
        || result.chroma_scaling_from_luma
        || (sequence.profile == 0 && result.num_y_points == 0);
    if !infer_chroma_points {
        result.num_cb_points = read_points(&mut result.scaling_points_cb, bits, 10, "Cb")?;
        result.num_cr_points = read_points(&mut result.scaling_points_cr, bits, 10, "Cr")?;
    }
    result.scaling_shift_minus8 = read_u8(bits, 2)?;
    result.ar_coeff_lag = read_u8(bits, 2)?;
    let luma_coeff_count = usize::from(2 * result.ar_coeff_lag * (result.ar_coeff_lag + 1));
    let chroma_coeff_count = luma_coeff_count + usize::from(result.num_y_points != 0);
    if result.num_y_points != 0 {
        read_byte_array(&mut result.ar_coeffs_y, bits, luma_coeff_count)?;
    }
    if result.chroma_scaling_from_luma || result.num_cb_points != 0 {
        read_byte_array(&mut result.ar_coeffs_cb, bits, chroma_coeff_count)?;
    }
    if result.chroma_scaling_from_luma || result.num_cr_points != 0 {
        read_byte_array(&mut result.ar_coeffs_cr, bits, chroma_coeff_count)?;
    }
    result.ar_coeff_shift_minus6 = read_u8(bits, 2)?;
    result.grain_scale_shift = read_u8(bits, 2)?;
    if result.num_cb_points != 0 {
        result.cb_mult = read_u8(bits, 8)?;
        result.cb_luma_mult = read_u8(bits, 8)?;
        result.cb_offset = u16::try_from(bits.read_bits(9)?)
            .map_err(|_| av1_error("parse_film_grain", "Cb offset exceeds u16"))?;
    }
    if result.num_cr_points != 0 {
        result.cr_mult = read_u8(bits, 8)?;
        result.cr_luma_mult = read_u8(bits, 8)?;
        result.cr_offset = u16::try_from(bits.read_bits(9)?)
            .map_err(|_| av1_error("parse_film_grain", "Cr offset exceeds u16"))?;
    }
    result.overlap_flag = bits.read_bit()?;
    result.clip_to_restricted_range = bits.read_bit()?;
    result.header_bit_position = bits.position();
    Ok(result)
}

fn read_points<const N: usize>(
    points: &mut [[u8; 2]; N],
    bits: &mut BitReader<'_>,
    maximum: u8,
    plane: &'static str,
) -> VideoResult<u8> {
    let count = read_u8(bits, 4)?;
    if count > maximum {
        return Err(av1_error(
            "parse_film_grain",
            format!("{plane} point count exceeds {maximum}"),
        ));
    }
    let count_usize = usize::from(count);
    let mut previous_value = None;
    for (index, point) in points.iter_mut().take(count_usize).enumerate() {
        let value = read_u8(bits, 8)?;
        let scale = read_u8(bits, 8)?;
        let remaining = count_usize - index - 1;
        let maximum_value = u8::MAX.saturating_sub(u8::try_from(remaining).unwrap_or(u8::MAX));
        if value > maximum_value || previous_value.is_some_and(|previous| value <= previous) {
            return Err(av1_error(
                "parse_film_grain",
                format!("{plane} point values are not strictly increasing or exceed bounds"),
            ));
        }
        *point = [value, scale];
        previous_value = Some(value);
    }
    Ok(count)
}

fn read_byte_array(output: &mut [u8], bits: &mut BitReader<'_>, count: usize) -> VideoResult<()> {
    if count > output.len() {
        return Err(av1_error(
            "parse_film_grain",
            "AR coefficient count exceeds DXVA array",
        ));
    }
    for coefficient in output.iter_mut().take(count) {
        *coefficient = read_u8(bits, 8)?;
    }
    Ok(())
}

impl Av1RestorationParams {
    /// Parses restoration syntax following CDEF for key/intra frames.
    pub(crate) fn parse_obu(
        input: &[u8],
        sequence: &SequenceHeader,
        frame: &IntraFrameTileHeader,
        quantization: &Av1QuantizationParams,
        segmentation: &Av1SegmentationParams,
        cdef: &Av1CdefParams,
    ) -> VideoResult<Self> {
        let obu = parse_obu(input)?;
        let mut bits = BitReader::new(obu.payload);
        bits.skip_bits(cdef.header_bit_position)?;
        let mut result = Self {
            types: [0; 3],
            unit_shift: 0,
            uv_shift: 0,
            header_bit_position: bits.position(),
        };
        let all_lossless =
            is_coded_lossless(quantization, segmentation) && frame.width == frame.upscaled_width;
        if all_lossless || frame.allow_intrabc || !sequence.enable_restoration {
            return Ok(result);
        }
        let plane_count = if sequence.color.monochrome { 1 } else { 3 };
        for value in result.types.iter_mut().take(plane_count) {
            *value = read_u8(&mut bits, 2)?;
        }
        let uses_restoration = result.types[..plane_count].iter().any(|kind| *kind != 0);
        if uses_restoration {
            let minimum = u32::from(sequence.use_128x128_superblock);
            result.unit_shift = u8::try_from(read_increment(&mut bits, minimum, 2)?)
                .map_err(|_| av1_error("Av1RestorationParams", "unit shift exceeds u8"))?;
            let uses_chroma = result.types[1..plane_count].iter().any(|kind| *kind != 0);
            let is_subsampled_420 = sequence.profile == 0 && !sequence.color.monochrome;
            if is_subsampled_420 && uses_chroma {
                result.uv_shift = u8::from(bits.read_bit()?);
            }
        }
        result.header_bit_position = bits.position();
        Ok(result)
    }
}

fn is_coded_lossless(
    quantization: &Av1QuantizationParams,
    segmentation: &Av1SegmentationParams,
) -> bool {
    (0..8).all(|segment| {
        let segment_q = i16::from(quantization.base_qindex)
            + if segmentation.feature_enabled[segment][0] {
                segmentation.feature_data[segment][0]
            } else {
                0
            };
        segment_q.clamp(0, 255) == 0
            && quantization.y_dc_delta_q == 0
            && quantization.u_dc_delta_q == 0
            && quantization.v_dc_delta_q == 0
            && quantization.u_ac_delta_q == 0
            && quantization.v_ac_delta_q == 0
    })
}

fn validate_frame_id(
    bits: &mut BitReader<'_>,
    sequence: &SequenceHeader,
    slots: &ReferenceSlots,
    current_frame_id: u32,
    reference: u8,
) -> VideoResult<()> {
    let modulus = 1_u32 << sequence.frame_id_length;
    let delta = read_u32(bits, sequence.delta_frame_id_length)? + 1;
    let expected = current_frame_id.wrapping_sub(delta) & (modulus - 1);
    if slots.get(reference)?.frame_id != expected {
        return Err(av1_error(
            "FrameReferenceHeader::parse_obu",
            format!(
                "delta_frame_id does not match reference {reference}: expected {expected}, stored {}",
                slots.get(reference)?.frame_id
            ),
        ));
    }
    Ok(())
}

fn skip_mode_allowed(
    reference: &FrameReferenceHeader,
    sequence: &SequenceHeader,
) -> VideoResult<bool> {
    let mut nearest_forward = None;
    let mut nearest_backward = None;
    for slot_index in reference.ref_frame_idx {
        let slot = reference.next_slots.get(slot_index)?;
        let distance = relative_distance(
            sequence.enable_order_hint,
            sequence.order_hint_bits,
            slot.order_hint,
            reference.order_hint,
        );
        if distance < 0 {
            if nearest_forward.is_none_or(|(_, current)| {
                relative_distance(true, sequence.order_hint_bits, slot.order_hint, current) > 0
            }) {
                nearest_forward = Some((slot_index, slot.order_hint));
            }
        } else if distance > 0
            && nearest_backward.is_none_or(|(_, current)| {
                relative_distance(true, sequence.order_hint_bits, slot.order_hint, current) < 0
            })
        {
            nearest_backward = Some((slot_index, slot.order_hint));
        }
    }
    let Some((_, forward_hint)) = nearest_forward else {
        return Ok(false);
    };
    if nearest_backward.is_some() {
        return Ok(true);
    }
    for slot_index in reference.ref_frame_idx {
        let slot = reference.next_slots.get(slot_index)?;
        if relative_distance(
            true,
            sequence.order_hint_bits,
            slot.order_hint,
            forward_hint,
        ) < 0
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn read_u8(bits: &mut BitReader<'_>, count: u8) -> VideoResult<u8> {
    u8::try_from(bits.read_bits(count)?)
        .map_err(|_| av1_error("FrameReferenceHeader", "field exceeds u8"))
}

fn read_u32(bits: &mut BitReader<'_>, count: u8) -> VideoResult<u32> {
    u32::try_from(bits.read_bits(count)?)
        .map_err(|_| av1_error("FrameReferenceHeader", "field exceeds u32"))
}

fn read_u16(bits: &mut BitReader<'_>, count: u8) -> VideoResult<u16> {
    let value = u16::try_from(bits.read_bits(count)?)
        .map_err(|_| av1_error("IntraFrameTileHeader", "dimension exceeds u16"))?;
    value
        .checked_add(1)
        .ok_or_else(|| av1_error("IntraFrameTileHeader", "dimension overflows u16"))
}

fn read_delta_q(bits: &mut BitReader<'_>) -> VideoResult<i8> {
    let coded = bits.read_bit()?;
    if !coded {
        return Ok(0);
    }
    let value = i16::try_from(bits.read_bits(7)?)
        .map_err(|_| av1_error("read_delta_q", "delta_q exceeds i16"))?;
    let signed = if value & 0x40 != 0 {
        value - 0x80
    } else {
        value
    };
    i8::try_from(signed).map_err(|_| av1_error("read_delta_q", "delta_q exceeds i8"))
}

fn read_signed_value(value: u64, width: u8) -> VideoResult<i16> {
    let value = i32::try_from(value)
        .map_err(|_| av1_error("read_signed_value", "signed value exceeds i32"))?;
    let sign = 1_i32 << (width - 1);
    let signed = if value & sign == 0 {
        value
    } else {
        value - (1_i32 << width)
    };
    i16::try_from(signed).map_err(|_| av1_error("read_signed_value", "signed value exceeds i16"))
}

fn read_signed_7(bits: &mut BitReader<'_>, field: &'static str) -> VideoResult<i8> {
    let value = bits.read_bits(7)?;
    i8::try_from(read_signed_value(value, 7)?)
        .map_err(|_| av1_error("read_signed_7", format!("{field} exceeds i8")))
}
