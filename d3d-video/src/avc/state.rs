use super::dpb::DecodedPictureBuffer;
use super::dxva::{DxvaReferenceEntry, dxva_references, picture_parameters_with_references};
use super::parameter_sets::{ParameterSets, SequenceParameterSet};
use super::slice::SliceHeader;
use super::{PictureStructure, avc_error, unsupported};
use crate::{FrameSize, VideoError, VideoErrorKind, VideoResult};

/// Stateful AVC parameter-set, DPB, and DXVA surface mapping for progressive 8-bit 4:2:0 video.
pub struct AvcDecodeState {
    sps_nal: Vec<u8>,
    pps_nal: Vec<u8>,
    sps: SequenceParameterSet,
    sets: Box<ParameterSets>,
    dpb: DecodedPictureBuffer,
    surfaces: Vec<(u64, u8)>,
    next_picture_id: u64,
}

/// AVC picture parameters and post-decode DPB state, ready to commit after successful submission.
pub(crate) struct PreparedAvcPicture {
    pub(crate) picture_parameters: Vec<u8>,
    next_dpb: DecodedPictureBuffer,
    next_surfaces: Vec<(u64, u8)>,
    next_picture_id: u64,
}

impl AvcDecodeState {
    /// Creates AVC state from the active sequence and picture parameter NAL units.
    ///
    /// # Errors
    ///
    /// Returns parser or unsupported errors for invalid or unsupported parameter sets.
    pub fn new(sps_nal: &[u8], pps_nal: &[u8]) -> VideoResult<Self> {
        let mut sets = Box::new(ParameterSets::default());
        let sps = sets.insert_sps_nal(sps_nal)?.clone();
        sets.insert_pps_nal(pps_nal)?;
        if !sps.frame_mbs_only
            || sps.chroma_format_idc != 1
            || sps.bit_depth_luma != 8
            || sps.bit_depth_chroma != 8
        {
            return Err(unsupported(
                "AvcDecodeState::new",
                "DXVA state supports progressive 8-bit 4:2:0 pictures",
            ));
        }
        Ok(Self {
            sps_nal: checked_copy(sps_nal, "AvcDecodeState::new")?,
            pps_nal: checked_copy(pps_nal, "AvcDecodeState::new")?,
            sps,
            sets,
            dpb: DecodedPictureBuffer::default(),
            surfaces: Vec::new(),
            next_picture_id: 1,
        })
    }

    /// Returns the coded macroblock dimensions before SPS cropping.
    ///
    /// # Errors
    ///
    /// Returns invalid-input if the SPS dimensions cannot be represented as a frame size.
    pub fn coded_size(&self) -> VideoResult<FrameSize> {
        FrameSize::new(self.sps.coded_width(), self.sps.coded_height())
    }

    /// Returns the currently retained reference surface bindings as `(picture_id, surface_index)`.
    #[must_use]
    pub fn reference_surfaces(&self) -> &[(u64, u8)] {
        &self.surfaces
    }

    /// Finds a decoder surface not retained by the current AVC reference-picture set.
    ///
    /// AVC DXVA picture parameters reserve the high bit for reference flags, so the surface index
    /// must remain below 128. Output frames no longer referenced by the DPB may be selected; the
    /// caller must still ensure they are no longer in use by a renderer or downstream consumer.
    #[must_use]
    pub fn find_writable_surface(&self, surface_count: usize) -> Option<u8> {
        (0..surface_count.min(128))
            .filter_map(|index| u8::try_from(index).ok())
            .find(|candidate| {
                self.surfaces
                    .iter()
                    .all(|(_, surface)| surface != candidate)
            })
    }

    /// Returns the SPS crop rectangle in decoded texture coordinates.
    ///
    /// # Errors
    ///
    /// Returns invalid-input if the parsed visible dimensions are empty.
    pub fn visible_rect(&self) -> VideoResult<crate::Rect> {
        Ok(crate::Rect {
            x: self.sps.crop_left,
            y: self.sps.crop_top,
            size: crate::FrameSize::new(self.sps.display_width(), self.sps.display_height())?,
        })
    }

    pub(crate) fn dimensions_in_mbs(&self) -> (u32, u32) {
        (
            u32::from(self.sps.width_in_mbs),
            u32::from(self.sps.height_in_mbs),
        )
    }

    pub(crate) fn quant_matrix(&self) -> VideoResult<Vec<u8>> {
        super::dxva::idr_quant_matrix(&self.sps_nal, &self.pps_nal)
    }

    /// Drops the current DPB references after a seek or decoder flush.
    pub fn reset(&mut self) {
        self.dpb.clear();
        self.surfaces.clear();
    }

    /// Replaces codec parameter sets and starts a new DPB epoch.
    ///
    /// Recreate the D3D decoder too if the new sequence changes coded dimensions or profile.
    ///
    /// # Errors
    ///
    /// Returns parser or unsupported errors for invalid or unsupported parameter sets.
    pub fn reconfigure(&mut self, sps_nal: &[u8], pps_nal: &[u8]) -> VideoResult<()> {
        *self = Self::new(sps_nal, pps_nal)?;
        Ok(())
    }

    pub(crate) fn validate_parameter_set(&self, nal: &[u8]) -> VideoResult<()> {
        let Some(&header) = nal.first() else {
            return Err(avc_error(
                "AvcDecodeState::validate_parameter_set",
                "parameter-set NAL is empty",
            ));
        };
        let configured = match header & 0x1f {
            7 => Some(self.sps_nal.as_slice()),
            8 => Some(self.pps_nal.as_slice()),
            _ => None,
        };
        if configured.is_some_and(|configured| configured != nal) {
            return Err(unsupported(
                "AvcDecodeState::validate_parameter_set",
                "in-band AVC parameter-set change requires AvcDecodeState::reconfigure",
            ));
        }
        Ok(())
    }

    /// Prepares one AVC frame picture without mutating the live DPB.
    ///
    /// The caller commits the returned state only after the hardware submission succeeds.
    ///
    /// # Errors
    ///
    /// Returns parser, missing-reference, surface-reuse, unsupported-syntax, or allocation errors.
    #[expect(
        clippy::too_many_lines,
        reason = "prepares one AVC picture atomically across slice validation, DPB, and DXVA mapping"
    )]
    pub(crate) fn prepare(
        &self,
        slice_nals: &[&[u8]],
        surface_index: u8,
    ) -> VideoResult<PreparedAvcPicture> {
        let first_slice = slice_nals.first().ok_or_else(|| {
            avc_error(
                "AvcDecodeState::prepare",
                "access unit contains no AVC VCL slice",
            )
        })?;
        if surface_index > 0x7f {
            return Err(avc_error(
                "AvcDecodeState::prepare",
                "DXVA H.264 surface index must fit seven bits",
            ));
        }
        let header = SliceHeader::parse_nal(first_slice, &self.sets)?;
        if !matches!(
            header.slice_type,
            super::slice::SliceType::I | super::slice::SliceType::P | super::slice::SliceType::B
        ) {
            return Err(unsupported(
                "AvcDecodeState::prepare",
                "SP and SI slices are not supported by the current DXVA mapping",
            ));
        }
        for slice_nal in &slice_nals[1..] {
            let slice = SliceHeader::parse_nal(slice_nal, &self.sets)?;
            if slice.pps_id != header.pps_id
                || slice.frame_num != header.frame_num
                || slice.structure != header.structure
                || slice.nal.is_idr() != header.nal.is_idr()
                || slice.nal.reference_idc != header.nal.reference_idc
                || slice.slice_type != header.slice_type
                || slice.memory_management != header.memory_management
            {
                return Err(avc_error(
                    "AvcDecodeState::prepare",
                    "VCL NAL units do not belong to the same AVC picture",
                ));
            }
        }
        if header.structure != PictureStructure::Frame {
            return Err(unsupported(
                "AvcDecodeState::prepare",
                "field pictures are not supported by the current DXVA mapping",
            ));
        }
        if !header.nal.is_idr()
            && self
                .surfaces
                .iter()
                .any(|(_, bound_surface)| *bound_surface == surface_index)
        {
            return Err(VideoError::new(
                VideoErrorKind::InvalidState,
                "AvcDecodeState::prepare",
                "target surface is still retained by an AVC reference picture",
            ));
        }

        let mut next_dpb = self.dpb.clone();
        let current_picture = next_dpb.derive_picture(self.next_picture_id, &header, &self.sps)?;
        for slice_nal in slice_nals {
            let slice = SliceHeader::parse_nal(slice_nal, &self.sets)?;
            if !slice.slice_type.is_intra() {
                self.dpb
                    .build_reference_lists(&slice, &self.sps, current_picture.poc)?;
            }
        }
        let references: Vec<DxvaReferenceEntry> = if header.nal.is_idr() {
            Vec::new()
        } else {
            dxva_references(&self.dpb, &self.surfaces)?
        };
        let picture_parameters = picture_parameters_with_references(
            &self.sps_nal,
            &self.pps_nal,
            first_slice,
            surface_index,
            Some(&current_picture),
            &references,
        )?;
        next_dpb.mark_decoded(current_picture.clone(), &header, &self.sps)?;
        let current_is_reference = header.nal.reference_idc != 0;
        let next_picture_id = self.next_picture_id.checked_add(1).ok_or_else(|| {
            VideoError::new(
                VideoErrorKind::InvalidState,
                "AvcDecodeState::prepare",
                "AVC picture identifier overflowed",
            )
        })?;
        let next_reference_count = next_dpb.short_references().len()
            + (0..16)
                .filter(|index| next_dpb.long_reference(*index).is_some())
                .count();
        let mut next_surfaces = Vec::new();
        next_surfaces
            .try_reserve_exact(next_reference_count)
            .map_err(|_| allocation_error("AvcDecodeState::prepare"))?;
        for &(picture_id, reference_surface) in &self.surfaces {
            if dpb_contains(&next_dpb, picture_id) {
                next_surfaces.push((picture_id, reference_surface));
            }
        }
        if current_is_reference {
            next_surfaces.push((current_picture.id, surface_index));
        }
        Ok(PreparedAvcPicture {
            picture_parameters,
            next_dpb,
            next_surfaces,
            next_picture_id,
        })
    }

    pub(crate) fn commit(&mut self, prepared: PreparedAvcPicture) {
        self.dpb = prepared.next_dpb;
        self.next_picture_id = prepared.next_picture_id;
        self.surfaces = prepared.next_surfaces;
    }
}

fn dpb_contains(dpb: &DecodedPictureBuffer, picture_id: u64) -> bool {
    dpb.short_references()
        .iter()
        .any(|picture| picture.id == picture_id)
        || (0..16).any(|index| {
            dpb.long_reference(index)
                .is_some_and(|picture| picture.id == picture_id)
        })
}

fn checked_copy(bytes: &[u8], operation: &'static str) -> VideoResult<Vec<u8>> {
    let mut copy = Vec::new();
    copy.try_reserve_exact(bytes.len())
        .map_err(|_| allocation_error(operation))?;
    copy.extend_from_slice(bytes);
    Ok(copy)
}

fn allocation_error(operation: &'static str) -> VideoError {
    VideoError::new(
        VideoErrorKind::Backend,
        operation,
        "unable to allocate AVC decoder state",
    )
}
