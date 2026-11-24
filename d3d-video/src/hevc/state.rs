use super::dpb::{DecodedPictureBuffer, Picture, SliceType};
use super::dxva::{DxvaReferenceEntry, dxva_reference_sets, dxva_references, picture_parameters};
use super::parameter_sets::{ParameterSets, SequenceParameterSet};
use super::slice::ReferenceSliceHeader;
use super::{hevc_error, unsupported};
use crate::{FrameSize, VideoError, VideoErrorKind, VideoResult};

/// Stateful HEVC parameter-set, POC, DPB, and DXVA surface mapping.
pub struct HevcDecodeState {
    vps_nal: Vec<u8>,
    sps_nal: Vec<u8>,
    pps_nal: Vec<u8>,
    sps: SequenceParameterSet,
    sets: Box<ParameterSets>,
    dpb: DecodedPictureBuffer,
    surfaces: Vec<(u64, u8)>,
    next_picture_id: u64,
}

pub(crate) struct PreparedHevcPicture {
    pub(crate) picture_parameters: Vec<u8>,
    next_dpb: DecodedPictureBuffer,
    next_surfaces: Vec<(u64, u8)>,
    next_picture_id: u64,
}

impl HevcDecodeState {
    /// Creates state from the active HEVC VPS/SPS/PPS NAL units.
    ///
    /// # Errors
    ///
    /// Returns parser or unsupported errors for invalid or unsupported parameter sets.
    pub fn new(vps_nal: &[u8], sps_nal: &[u8], pps_nal: &[u8]) -> VideoResult<Self> {
        let mut sets = Box::new(ParameterSets::default());
        sets.insert_vps_nal(vps_nal)?;
        let sps = sets.insert_sps_nal(sps_nal)?.clone();
        sets.insert_pps_nal(pps_nal)?;
        if sps.chroma_format_idc != 1 {
            return Err(unsupported(
                "HevcDecodeState::new",
                "DXVA state supports 4:2:0 pictures",
            ));
        }
        Ok(Self {
            vps_nal: checked_copy(vps_nal, "HevcDecodeState::new")?,
            sps_nal: checked_copy(sps_nal, "HevcDecodeState::new")?,
            pps_nal: checked_copy(pps_nal, "HevcDecodeState::new")?,
            sps,
            sets,
            dpb: DecodedPictureBuffer::default(),
            surfaces: Vec::new(),
            next_picture_id: 1,
        })
    }

    /// Returns the coded picture dimensions before the conformance window is applied.
    ///
    /// # Errors
    ///
    /// Returns invalid-input if the SPS dimensions cannot be represented as a frame size.
    pub fn coded_size(&self) -> VideoResult<FrameSize> {
        FrameSize::new(self.sps.width, self.sps.height)
    }

    /// Returns current DPB surface bindings as `(picture_id, surface_index)` pairs.
    #[must_use]
    pub fn reference_surfaces(&self) -> &[(u64, u8)] {
        &self.surfaces
    }

    /// Finds a decoder surface not retained by the current HEVC decoded-picture buffer.
    ///
    /// HEVC DXVA picture entries use seven surface-index bits. The caller must also ensure that
    /// the returned surface is no longer in use by a renderer or downstream consumer.
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

    /// Returns the SPS conformance window in decoded texture coordinates.
    ///
    /// # Errors
    ///
    /// Returns invalid-input if the parsed visible dimensions are empty.
    pub fn visible_rect(&self) -> VideoResult<crate::Rect> {
        Ok(crate::Rect {
            x: self.sps.conformance.left,
            y: self.sps.conformance.top,
            size: crate::FrameSize::new(self.sps.display_width(), self.sps.display_height())?,
        })
    }

    pub(crate) fn dimensions(&self) -> (u32, u32) {
        (self.sps.width, self.sps.height)
    }

    pub(crate) fn quant_matrix(&self) -> VideoResult<Vec<u8>> {
        super::dxva::idr_quant_matrix(&self.vps_nal, &self.sps_nal, &self.pps_nal)
    }

    /// Clears DPB references after a seek or flush.
    pub fn reset(&mut self) {
        self.dpb = DecodedPictureBuffer::default();
        self.surfaces.clear();
    }

    /// Replaces parameter sets and starts a new DPB epoch.
    ///
    /// Recreate the D3D decoder too when dimensions or profile change.
    ///
    /// # Errors
    ///
    /// Returns parser or unsupported errors for invalid or unsupported parameter sets.
    pub fn reconfigure(
        &mut self,
        vps_nal: &[u8],
        sps_nal: &[u8],
        pps_nal: &[u8],
    ) -> VideoResult<()> {
        *self = Self::new(vps_nal, sps_nal, pps_nal)?;
        Ok(())
    }

    pub(crate) fn validate_parameter_set(&self, nal: &[u8]) -> VideoResult<()> {
        let Some(&header) = nal.first() else {
            return Err(hevc_error(
                "HevcDecodeState::validate_parameter_set",
                "parameter-set NAL is empty",
            ));
        };
        let configured = match (header >> 1) & 0x3f {
            32 => Some(self.vps_nal.as_slice()),
            33 => Some(self.sps_nal.as_slice()),
            34 => Some(self.pps_nal.as_slice()),
            _ => None,
        };
        if configured.is_some_and(|configured| configured != nal) {
            return Err(unsupported(
                "HevcDecodeState::validate_parameter_set",
                "in-band HEVC parameter-set change requires HevcDecodeState::reconfigure",
            ));
        }
        Ok(())
    }

    /// Prepares one first-slice HEVC picture without mutating live DPB state.
    ///
    /// The returned state is committed only after hardware submission succeeds.
    ///
    /// # Errors
    ///
    /// Returns parser, missing-reference, surface-reuse, unsupported-syntax, or allocation errors.
    #[expect(
        clippy::too_many_lines,
        reason = "prepares one HEVC picture atomically across POC, RPS, DPB and DXVA mapping"
    )]
    pub(crate) fn prepare(
        &self,
        slice_nals: &[&[u8]],
        surface_index: u8,
    ) -> VideoResult<PreparedHevcPicture> {
        let slice_nal = *slice_nals.first().ok_or_else(|| {
            hevc_error(
                "HevcDecodeState::prepare",
                "access unit contains no HEVC slices",
            )
        })?;
        if surface_index > 0x7f {
            return Err(hevc_error(
                "HevcDecodeState::prepare",
                "DXVA HEVC surface index must fit seven bits",
            ));
        }
        // The first parse discovers POC LSB and syntax. Reparse with the derived full POC so
        // long-term references with delta_msb_present resolve against the correct POC epoch.
        let initial = ReferenceSliceHeader::parse_nal(slice_nal, &self.sets, 0)?;
        let mut next_dpb = self.dpb.clone();
        let current_poc =
            next_dpb.compute_poc(initial.nal, self.sps.log2_max_poc_lsb, initial.poc_lsb)?;
        let slice = ReferenceSliceHeader::parse_nal(slice_nal, &self.sets, current_poc)?;
        if !slice.first_slice_in_picture || slice.slice_segment_address != 0 {
            return Err(hevc_error(
                "HevcDecodeState::prepare",
                "first HEVC VCL NAL is not the first slice segment",
            ));
        }
        let mut previous_address = 0;
        let mut previous_independent = slice.clone();
        for nal in &slice_nals[1..] {
            let following = ReferenceSliceHeader::parse_nal_with_previous(
                nal,
                &self.sets,
                current_poc,
                Some(&previous_independent),
            )?;
            if following.first_slice_in_picture
                || following.slice_segment_address <= previous_address
                || !same_picture_header(&slice, &following)
            {
                return Err(unsupported(
                    "HevcDecodeState::prepare",
                    "HEVC access unit has a duplicate picture, unordered segment, or mismatched slice parameters",
                ));
            }
            previous_address = following.slice_segment_address;
            if !following.dependent_segment {
                previous_independent = following;
            }
        }
        let (_, _) = self.sets.resolve_pps(u32::from(slice.pps_id))?;
        if slice.slice_type != SliceType::I {
            next_dpb.build_reference_lists(
                current_poc,
                &slice.short_term_rps,
                &slice.long_term_rps,
                slice.slice_type,
                slice.active_references,
                [&slice.list_modifications[0], &slice.list_modifications[1]],
            )?;
        }
        next_dpb.apply_reference_set(current_poc, &slice.short_term_rps, &slice.long_term_rps)?;
        if !slice.nal.is_irap()
            && self
                .surfaces
                .iter()
                .any(|(_, retained_surface)| *retained_surface == surface_index)
            && next_dpb.pictures().iter().any(|picture| {
                self.surfaces.iter().any(|(id, retained_surface)| {
                    *retained_surface == surface_index && *id == picture.id
                })
            })
        {
            return Err(VideoError::new(
                VideoErrorKind::InvalidState,
                "HevcDecodeState::prepare",
                "target surface is retained by a picture in the current HEVC RPS",
            ));
        }
        let references: Vec<DxvaReferenceEntry> = dxva_references(&next_dpb, &self.surfaces)?;
        let reference_sets = dxva_reference_sets(
            &next_dpb,
            &self.surfaces,
            current_poc,
            &slice.short_term_rps,
            &slice.long_term_rps,
        )?;
        let picture_parameters = picture_parameters(
            &self.vps_nal,
            &self.sps_nal,
            &self.pps_nal,
            slice_nal,
            surface_index,
            current_poc,
            &references,
            [&reference_sets[0], &reference_sets[1], &reference_sets[2]],
        )?;
        let is_reference = slice.nal.is_irap() || matches!(slice.nal.unit_type, 1 | 3 | 5 | 7 | 9);
        if is_reference {
            let capacity = self.sps.max_dec_pic_buffering.last().copied().unwrap_or(1);
            next_dpb.insert_current(
                Picture {
                    id: self.next_picture_id,
                    poc: current_poc,
                    temporal_id: slice.nal.temporal_id,
                    long_term: false,
                },
                capacity,
            )?;
        }
        let next_picture_id = self.next_picture_id.checked_add(1).ok_or_else(|| {
            VideoError::new(
                VideoErrorKind::InvalidState,
                "HevcDecodeState::prepare",
                "HEVC picture identifier overflowed",
            )
        })?;
        let mut next_surfaces = Vec::new();
        next_surfaces
            .try_reserve_exact(next_dpb.pictures().len())
            .map_err(|_| allocation_error("HevcDecodeState::prepare"))?;
        for picture in next_dpb.pictures() {
            if picture.id == self.next_picture_id && is_reference {
                next_surfaces.push((picture.id, surface_index));
            } else if let Some((_, bound_surface)) = self
                .surfaces
                .iter()
                .find(|(picture_id, _)| *picture_id == picture.id)
            {
                next_surfaces.push((picture.id, *bound_surface));
            } else {
                return Err(hevc_error(
                    "HevcDecodeState::prepare",
                    "retained HEVC picture has no decoder surface",
                ));
            }
        }
        Ok(PreparedHevcPicture {
            picture_parameters,
            next_dpb,
            next_surfaces,
            next_picture_id,
        })
    }

    pub(crate) fn commit(&mut self, prepared: PreparedHevcPicture) {
        self.dpb = prepared.next_dpb;
        self.surfaces = prepared.next_surfaces;
        self.next_picture_id = prepared.next_picture_id;
    }
}

fn same_picture_header(first: &ReferenceSliceHeader, next: &ReferenceSliceHeader) -> bool {
    first.nal.unit_type == next.nal.unit_type
        && first.nal.layer_id == next.nal.layer_id
        && first.nal.temporal_id == next.nal.temporal_id
        && first.no_output_of_prior_pictures == next.no_output_of_prior_pictures
        && first.pps_id == next.pps_id
        && first.slice_type == next.slice_type
        && first.picture_output == next.picture_output
        && first.poc_lsb == next.poc_lsb
        && first.short_term_rps == next.short_term_rps
        && first.long_term_rps == next.long_term_rps
        && first.temporal_mvp_enabled == next.temporal_mvp_enabled
        && first.sao_enabled == next.sao_enabled
        && first.active_references == next.active_references
        && first.list_modifications == next.list_modifications
        && first.mvd_l1_zero == next.mvd_l1_zero
        && first.cabac_init == next.cabac_init
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
        "unable to allocate HEVC decoder state",
    )
}
