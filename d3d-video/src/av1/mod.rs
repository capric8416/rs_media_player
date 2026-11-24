mod dxva;
mod frame;
mod refs;
mod sequence;
mod tile;

use refs::{ReferenceSlot, ReferenceSlots};

#[cfg(test)]
mod tests;

use crate::VideoResult;
use crate::{FrameSize, VideoError, VideoErrorKind};

pub(crate) fn sequence_header_frame_size(sequence_header_obu: &[u8]) -> VideoResult<FrameSize> {
    let sequence = sequence::SequenceHeader::parse_obu(sequence_header_obu)?;
    FrameSize::new(
        u32::from(sequence.max_frame_width),
        u32::from(sequence.max_frame_height),
    )
}

pub(crate) struct Av1FrameDxvaInput {
    pub(crate) picture_parameters: Vec<u8>,
    pub(crate) tile_payload: Vec<u8>,
    pub(crate) tiles: Vec<tile::TileDataRange>,
    pub(crate) width: u16,
    pub(crate) height: u16,
    pub(crate) render_width: u32,
    pub(crate) render_height: u32,
    pub(crate) frame_id: u32,
    pub(crate) order_hint: u32,
    pub(crate) frame_type: u8,
    pub(crate) refresh_frame_flags: u8,
    film_grain: frame::Av1FilmGrainParams,
    next_slots: ReferenceSlots,
}

impl Av1FrameDxvaInput {
    pub(crate) fn reference_surface_bindings(&self) -> VideoResult<Vec<(u64, u8)>> {
        self.next_slots.surface_bindings()
    }
}

/// AV1's eight reference slots, tracking the decoded surfaces used by later frames.
#[derive(Clone, Debug, Default)]
pub struct Av1DecodeState {
    slots: ReferenceSlots,
}

impl Av1DecodeState {
    /// Clears all frame references, such as after a seek or sequence change.
    pub fn reset(&mut self) {
        self.slots = ReferenceSlots::default();
    }

    pub(crate) const fn slots(&self) -> &ReferenceSlots {
        &self.slots
    }

    /// Returns the decoder surface associated with one of AV1's eight reference slots.
    ///
    /// # Errors
    ///
    /// Returns an invalid-input error when the slot index is outside `0..8` or the slot has not
    /// been populated by a decoded frame.
    pub fn surface_for_slot(&self, slot_index: u8) -> VideoResult<u8> {
        let slot = self.slots.get(slot_index)?;
        u8::try_from(slot.surface_id).map_err(|_| {
            av1_error(
                "Av1DecodeState::surface_for_slot",
                "stored surface index exceeds the DXVA AV1 range",
            )
        })
    }

    /// Finds the first decoder surface that can be overwritten by a frame with these refresh flags.
    ///
    /// Surfaces retained only by slots refreshed by the new frame are reusable. The DXVA AV1
    /// surface index is an eight-bit field, so indices above 255 are never returned.
    #[must_use]
    pub fn find_writable_surface(
        &self,
        surface_count: usize,
        refresh_frame_flags: u8,
    ) -> Option<u8> {
        (0..surface_count.min(usize::from(u8::MAX) + 1))
            .filter_map(|index| u8::try_from(index).ok())
            .find(|index| self.can_overwrite_surface(*index, refresh_frame_flags))
    }

    /// Returns the previously decoded surface selected by a show-existing-frame header.
    ///
    /// # Errors
    ///
    /// Returns a parser error if the slot is unavailable or its optional display ID mismatches.
    pub fn show_existing_surface(
        &self,
        sequence_header_obu: &[u8],
        frame_header_obu: &[u8],
    ) -> VideoResult<Option<u8>> {
        let sequence = sequence::SequenceHeader::parse_obu(sequence_header_obu)?;
        let reference =
            frame::FrameReferenceHeader::parse_obu(frame_header_obu, &sequence, &self.slots)?;
        if !reference.show_existing_frame {
            return Ok(None);
        }
        let slot_index = reference.frame_to_show_map_idx.ok_or_else(|| {
            av1_error(
                "Av1DecodeState::show_existing_surface",
                "show-existing-frame header has no reference slot index",
            )
        })?;
        let slot = self.slots.get(slot_index)?;
        let surface = u8::try_from(slot.surface_id).map_err(|_| {
            av1_error(
                "Av1DecodeState::show_existing_surface",
                "stored surface index exceeds the DXVA AV1 range",
            )
        })?;
        Ok(Some(surface))
    }

    /// Returns the stored render dimensions for a decoded AV1 surface.
    ///
    /// # Errors
    ///
    /// Returns invalid-state when the surface is not present in a reference slot.
    pub fn surface_render_size(&self, surface: u8) -> VideoResult<crate::FrameSize> {
        let (width, height) = self.slots.render_size_for_surface(surface).ok_or_else(|| {
            av1_error(
                "Av1DecodeState::surface_render_size",
                "surface is not stored in an AV1 reference slot",
            )
        })?;
        crate::FrameSize::new(width, height)
    }

    /// Checks whether decoding to a surface preserves all reference slots not refreshed by a frame.
    #[must_use]
    pub fn can_overwrite_surface(&self, surface_index: u8, refresh_frame_flags: u8) -> bool {
        self.slots
            .can_overwrite_surface(u64::from(surface_index), refresh_frame_flags)
    }

    pub(crate) fn commit_frame(&mut self, input: &Av1FrameDxvaInput, surface_index: u8) {
        self.slots = input.next_slots.clone();
        self.slots.commit(
            input.refresh_frame_flags,
            ReferenceSlot {
                frame_id: input.frame_id,
                order_hint: input.order_hint,
                frame_type: input.frame_type,
                surface_id: u64::from(surface_index),
                width: input.width,
                height: input.height,
                render_width: input.render_width,
                render_height: input.render_height,
                film_grain: input.film_grain,
            },
        );
    }

    /// Records a successfully submitted key/intra frame in the AV1 reference slots.
    ///
    /// # Errors
    ///
    /// Returns parser or unsupported errors when the header is not a decoded key/intra frame.
    pub fn commit_intra_frame(
        &mut self,
        sequence_header_obu: &[u8],
        frame_header_obu: &[u8],
        surface_index: u8,
    ) -> VideoResult<()> {
        let sequence = sequence::SequenceHeader::parse_obu(sequence_header_obu)?;
        let reference =
            frame::FrameReferenceHeader::parse_obu(frame_header_obu, &sequence, &self.slots)?;
        if reference.show_existing_frame || !reference.frame_type.is_intra() {
            return Err(VideoError::new(
                VideoErrorKind::Unsupported,
                "Av1DecodeState::commit_intra_frame",
                "only decoded key/intra frames can update these AV1 reference slots",
            ));
        }
        let tile = frame::IntraFrameTileHeader::parse_obu(frame_header_obu, &sequence, &reference)?;
        let quantization =
            frame::Av1QuantizationParams::parse_obu(frame_header_obu, &sequence, &tile)?;
        let segmentation =
            frame::Av1SegmentationParams::parse_obu(frame_header_obu, &quantization)?;
        let deltas = frame::Av1DeltaParams::parse_obu(
            frame_header_obu,
            &quantization,
            &segmentation,
            tile.allow_intrabc,
        )?;
        let loop_filter = frame::Av1LoopFilterParams::parse_obu(
            frame_header_obu,
            &sequence,
            &tile,
            &quantization,
            &segmentation,
            &deltas,
        )?;
        let cdef = frame::Av1CdefParams::parse_obu(
            frame_header_obu,
            &sequence,
            &tile,
            &quantization,
            &segmentation,
            &loop_filter,
        )?;
        let restoration = frame::Av1RestorationParams::parse_obu(
            frame_header_obu,
            &sequence,
            &tile,
            &quantization,
            &segmentation,
            &cdef,
        )?;
        let tail = frame::Av1IntraFrameTail::parse_obu(
            frame_header_obu,
            &sequence,
            &reference,
            &tile,
            &quantization,
            &segmentation,
            &restoration,
        )?;
        self.slots = reference.next_slots;
        self.slots.commit(
            reference.refresh_frame_flags,
            ReferenceSlot {
                frame_id: reference.current_frame_id,
                order_hint: reference.order_hint,
                frame_type: reference.frame_type as u8,
                surface_id: u64::from(surface_index),
                width: tile.upscaled_width,
                height: tile.height,
                render_width: tile.render_width,
                render_height: tile.render_height,
                film_grain: tail.film_grain,
            },
        );
        Ok(())
    }
}

/// Parses one AV1 intra frame and constructs its DXVA picture record and packed tile payload.
pub(crate) fn build_intra_dxva_input(
    sequence_obu: &[u8],
    frame_header_obu: &[u8],
    tile_group_obus: &[&[u8]],
    current_surface: u8,
) -> VideoResult<Av1FrameDxvaInput> {
    build_intra_dxva_input_with_slots(
        sequence_obu,
        frame_header_obu,
        tile_group_obus,
        current_surface,
        &ReferenceSlots::default(),
    )
}

pub(crate) fn build_intra_dxva_input_with_slots(
    sequence_obu: &[u8],
    frame_header_obu: &[u8],
    tile_group_obus: &[&[u8]],
    current_surface: u8,
    slots: &ReferenceSlots,
) -> VideoResult<Av1FrameDxvaInput> {
    let sequence = sequence::SequenceHeader::parse_obu(sequence_obu)?;
    let reference = frame::FrameReferenceHeader::parse_obu(frame_header_obu, &sequence, slots)?;
    if !reference.frame_type.is_intra() && reference.primary_ref_frame != 7 {
        return Err(VideoError::new(
            VideoErrorKind::Unsupported,
            "build_intra_dxva_input_with_slots",
            "supported AV1 inter frames must use PRIMARY_REF_NONE",
        ));
    }
    let parsed_frame =
        frame::IntraFrameTileHeader::parse_obu(frame_header_obu, &sequence, &reference)?;
    let quantization =
        frame::Av1QuantizationParams::parse_obu(frame_header_obu, &sequence, &parsed_frame)?;
    let segmentation = frame::Av1SegmentationParams::parse_obu(frame_header_obu, &quantization)?;
    let deltas = frame::Av1DeltaParams::parse_obu(
        frame_header_obu,
        &quantization,
        &segmentation,
        parsed_frame.allow_intrabc,
    )?;
    let loop_filter = frame::Av1LoopFilterParams::parse_obu(
        frame_header_obu,
        &sequence,
        &parsed_frame,
        &quantization,
        &segmentation,
        &deltas,
    )?;
    let cdef = frame::Av1CdefParams::parse_obu(
        frame_header_obu,
        &sequence,
        &parsed_frame,
        &quantization,
        &segmentation,
        &loop_filter,
    )?;
    let restoration = frame::Av1RestorationParams::parse_obu(
        frame_header_obu,
        &sequence,
        &parsed_frame,
        &quantization,
        &segmentation,
        &cdef,
    )?;
    let tail = frame::Av1IntraFrameTail::parse_obu(
        frame_header_obu,
        &sequence,
        &reference,
        &parsed_frame,
        &quantization,
        &segmentation,
        &restoration,
    )?;
    let dxva_frame = dxva::Av1IntraFrame {
        sequence: &sequence,
        reference: &reference,
        frame: &parsed_frame,
        quantization: &quantization,
        segmentation: &segmentation,
        deltas: &deltas,
        loop_filter: &loop_filter,
        cdef: &cdef,
        restoration: &restoration,
        tail: &tail,
    };
    let picture_parameters = dxva::intra_picture_parameters(&dxva_frame, current_surface)?;
    let (tile_payload, tiles) =
        if crate::obu::parse_obu(frame_header_obu)?.kind == crate::obu::ObuType::FRAME {
            if !tile_group_obus.is_empty() {
                return Err(av1_error(
                    "build_intra_dxva_input_with_slots",
                    "OBU_FRAME already contains its tile data and cannot have separate tile groups",
                ));
            }
            tile::collect_embedded_frame_tiles(
                frame_header_obu,
                &parsed_frame.tile_layout,
                tail.header_bit_position,
            )?
        } else {
            tile::collect_tile_groups(tile_group_obus, &parsed_frame.tile_layout)?
        };
    Ok(Av1FrameDxvaInput {
        picture_parameters,
        tile_payload,
        tiles,
        width: parsed_frame.upscaled_width,
        height: parsed_frame.height,
        render_width: parsed_frame.render_width,
        render_height: parsed_frame.render_height,
        frame_id: reference.current_frame_id,
        order_hint: reference.order_hint,
        frame_type: reference.frame_type as u8,
        refresh_frame_flags: reference.refresh_frame_flags,
        film_grain: tail.film_grain,
        next_slots: reference.next_slots.clone(),
    })
}

pub(super) fn av1_error(operation: &'static str, detail: impl Into<String>) -> VideoError {
    VideoError::new(VideoErrorKind::InvalidInput, operation, detail)
}
