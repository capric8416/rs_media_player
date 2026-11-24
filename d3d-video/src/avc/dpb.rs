use super::parameter_sets::SequenceParameterSet;
use super::slice::{MemoryManagementOperation, RefPicListModification, SliceHeader, SliceType};
use super::{MAX_DPB_FRAMES, PictureStructure, avc_error};
use crate::{VideoError, VideoErrorKind, VideoResult};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Picture {
    pub(crate) id: u64,
    pub(crate) frame_num: u32,
    pub(crate) poc: i32,
    pub(crate) field_poc: [i32; 2],
    pub(crate) structure: PictureStructure,
    pub(crate) reference_mask: u8,
    pub(crate) long_term_index: Option<u8>,
}

impl Picture {
    pub(crate) fn new(
        id: u64,
        frame_num: u32,
        field_poc: [i32; 2],
        structure: PictureStructure,
    ) -> Self {
        let poc = match structure {
            PictureStructure::TopField => field_poc[0],
            PictureStructure::BottomField => field_poc[1],
            PictureStructure::Frame => field_poc[0].min(field_poc[1]),
        };
        Self {
            id,
            frame_num,
            poc,
            field_poc,
            structure,
            reference_mask: structure.mask(),
            long_term_index: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ReferencePicture {
    pub(crate) id: u64,
    pub(crate) picture_number: u32,
    pub(crate) poc: i32,
    pub(crate) long_term: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct ReferenceLists {
    pub(crate) list0: Vec<ReferencePicture>,
    pub(crate) list1: Vec<ReferencePicture>,
}

#[derive(Clone, Debug)]
struct PicOrderState {
    frame_num: u32,
    frame_num_offset: u32,
    poc_msb: i32,
    poc_lsb: i32,
}

impl Default for PicOrderState {
    fn default() -> Self {
        Self {
            frame_num: 0,
            frame_num_offset: 0,
            poc_msb: 0,
            poc_lsb: -1,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct DecodedPictureBuffer {
    short: Vec<Picture>,
    long: [Option<Picture>; MAX_DPB_FRAMES],
    poc: PicOrderState,
}

impl DecodedPictureBuffer {
    pub(crate) fn short_references(&self) -> &[Picture] {
        &self.short
    }

    pub(crate) fn long_reference(&self, index: usize) -> Option<&Picture> {
        self.long.get(index).and_then(Option::as_ref)
    }

    pub(crate) fn clear(&mut self) {
        self.short.clear();
        self.long.fill(None);
        self.poc = PicOrderState::default();
    }

    pub(crate) fn derive_picture(
        &mut self,
        id: u64,
        header: &SliceHeader,
        sps: &SequenceParameterSet,
    ) -> VideoResult<Picture> {
        let field_poc = self.derive_field_poc(header, sps)?;
        Ok(Picture::new(
            id,
            header.frame_num,
            field_poc,
            header.structure,
        ))
    }

    pub(crate) fn mark_decoded(
        &mut self,
        mut current: Picture,
        header: &SliceHeader,
        sps: &SequenceParameterSet,
    ) -> VideoResult<()> {
        if header.nal.reference_idc == 0 {
            return Ok(());
        }
        if header.nal.is_idr() {
            self.short.clear();
            self.long.fill(None);
            if header.long_term_reference {
                current.long_term_index = Some(0);
                self.long[0] = Some(current);
            } else if sps.max_num_ref_frames > 0 {
                self.insert_short(current)?;
            }
            return Ok(());
        }

        let mut current_assigned = false;
        if header.adaptive_ref_pic_marking {
            for operation in &header.memory_management {
                match *operation {
                    MemoryManagementOperation::ForgetShort { picture_number } => {
                        self.short
                            .retain(|picture| picture.frame_num != picture_number);
                    }
                    MemoryManagementOperation::ForgetLong { index } => {
                        self.long[usize::from(index)] = None;
                    }
                    MemoryManagementOperation::ShortToLong {
                        picture_number,
                        index,
                    } => {
                        let position = self
                            .short
                            .iter()
                            .position(|picture| picture.frame_num == picture_number)
                            .ok_or_else(|| {
                                avc_error(
                                    "DecodedPictureBuffer::mark_decoded",
                                    "MMCO references a missing short-term picture",
                                )
                            })?;
                        let mut picture = self.short.remove(position);
                        picture.long_term_index = Some(index);
                        self.long[usize::from(index)] = Some(picture);
                    }
                    MemoryManagementOperation::SetMaxLong { max_index } => {
                        for (index, picture) in self.long.iter_mut().enumerate() {
                            if max_index.is_none_or(|maximum| index > usize::from(maximum)) {
                                *picture = None;
                            }
                        }
                    }
                    MemoryManagementOperation::Reset => {
                        self.short.clear();
                        self.long.fill(None);
                        current.frame_num = 0;
                        self.poc = PicOrderState::default();
                    }
                    MemoryManagementOperation::CurrentToLong { index } => {
                        current.long_term_index = Some(index);
                        self.long[usize::from(index)] = Some(current.clone());
                        current_assigned = true;
                    }
                }
            }
        } else if self.reference_count() >= usize::from(sps.max_num_ref_frames) {
            self.short.pop();
        }

        if !current_assigned && sps.max_num_ref_frames > 0 {
            self.insert_short(current)?;
        }
        if self.reference_count() > MAX_DPB_FRAMES {
            return Err(avc_error(
                "DecodedPictureBuffer::mark_decoded",
                "decoded picture buffer exceeds 16 frames",
            ));
        }
        Ok(())
    }

    pub(crate) fn build_reference_lists(
        &self,
        header: &SliceHeader,
        sps: &SequenceParameterSet,
        current_poc: i32,
    ) -> VideoResult<ReferenceLists> {
        if header.slice_type.is_intra() {
            return Ok(ReferenceLists::default());
        }
        let mut lists = if header.slice_type == SliceType::B {
            self.default_b_lists(current_poc)
        } else {
            ReferenceLists {
                list0: self.default_p_list(),
                list1: Vec::new(),
            }
        };
        self.apply_modifications(&mut lists.list0, &header.ref_modifications[0], header, sps)?;
        if header.slice_type == SliceType::B {
            self.apply_modifications(&mut lists.list1, &header.ref_modifications[1], header, sps)?;
        }
        require_and_truncate(&mut lists.list0, header.ref_count[0], "reference list 0")?;
        require_and_truncate(&mut lists.list1, header.ref_count[1], "reference list 1")?;
        Ok(lists)
    }

    fn derive_field_poc(
        &mut self,
        header: &SliceHeader,
        sps: &SequenceParameterSet,
    ) -> VideoResult<[i32; 2]> {
        if header.nal.is_idr() {
            self.poc = PicOrderState::default();
        }
        let max_frame_num = 1_u32 << sps.log2_max_frame_num;
        let frame_num_offset = if header.nal.is_idr() {
            0
        } else if self.poc.frame_num > header.frame_num {
            self.poc
                .frame_num_offset
                .checked_add(max_frame_num)
                .ok_or_else(|| avc_error("derive_field_poc", "frame number offset overflowed"))?
        } else {
            self.poc.frame_num_offset
        };

        let field_poc = match sps.poc_type {
            0 => self.type_zero_poc(header, sps)?,
            1 => type_one_poc(header, sps, frame_num_offset)?,
            2 => {
                let absolute = frame_num_offset
                    .checked_add(header.frame_num)
                    .ok_or_else(|| {
                        avc_error("derive_field_poc", "absolute frame number overflowed")
                    })?;
                let base = i32::try_from(absolute)
                    .map_err(|_| {
                        avc_error("derive_field_poc", "absolute frame number exceeds i32")
                    })?
                    .checked_mul(2)
                    .ok_or_else(|| avc_error("derive_field_poc", "type-2 POC overflowed"))?;
                let poc = if header.nal.reference_idc == 0 {
                    base - 1
                } else {
                    base
                };
                [poc, poc]
            }
            _ => return Err(avc_error("derive_field_poc", "unsupported POC type")),
        };
        if header.nal.reference_idc != 0 {
            self.poc.frame_num = header.frame_num;
            self.poc.frame_num_offset = frame_num_offset;
        }
        Ok(field_poc)
    }

    fn type_zero_poc(
        &mut self,
        header: &SliceHeader,
        sps: &SequenceParameterSet,
    ) -> VideoResult<[i32; 2]> {
        let maximum = 1_i32 << sps.log2_max_poc_lsb;
        let lsb = i32::try_from(header.poc_lsb)
            .map_err(|_| avc_error("type_zero_poc", "POC LSB exceeds i32"))?;
        if self.poc.poc_lsb < 0 {
            self.poc.poc_lsb = lsb;
        }
        let msb = if lsb < self.poc.poc_lsb && self.poc.poc_lsb - lsb >= maximum / 2 {
            self.poc.poc_msb + maximum
        } else if lsb > self.poc.poc_lsb && lsb - self.poc.poc_lsb > maximum / 2 {
            self.poc.poc_msb - maximum
        } else {
            self.poc.poc_msb
        };
        let top = msb
            .checked_add(lsb)
            .ok_or_else(|| avc_error("type_zero_poc", "top-field POC overflowed"))?;
        let bottom = top
            .checked_add(header.delta_poc_bottom)
            .ok_or_else(|| avc_error("type_zero_poc", "bottom-field POC overflowed"))?;
        if header.nal.reference_idc != 0 {
            self.poc.poc_msb = msb;
            self.poc.poc_lsb = lsb;
        }
        Ok(match header.structure {
            PictureStructure::TopField => [top, i32::MAX],
            PictureStructure::BottomField => [i32::MAX, top],
            PictureStructure::Frame => [top, bottom],
        })
    }

    fn insert_short(&mut self, picture: Picture) -> VideoResult<()> {
        if let Some(existing) = self
            .short
            .first_mut()
            .filter(|existing| existing.frame_num == picture.frame_num && existing.id == picture.id)
        {
            existing.reference_mask |= picture.reference_mask;
            existing.field_poc = picture.field_poc;
            existing.structure = PictureStructure::Frame;
            existing.poc = existing.field_poc[0].min(existing.field_poc[1]);
        } else {
            self.short.try_reserve(1).map_err(|_| {
                VideoError::new(
                    VideoErrorKind::Backend,
                    "DecodedPictureBuffer::insert_short",
                    "unable to allocate AVC short-term reference slot",
                )
            })?;
            self.short.insert(0, picture);
        }
        Ok(())
    }

    fn reference_count(&self) -> usize {
        self.short.len() + self.long.iter().flatten().count()
    }

    fn default_p_list(&self) -> Vec<ReferencePicture> {
        let mut result: Vec<_> = self.short.iter().map(short_reference).collect();
        result.extend(self.long.iter().flatten().map(long_reference));
        result
    }

    fn default_b_lists(&self, current_poc: i32) -> ReferenceLists {
        let mut before: Vec<_> = self
            .short
            .iter()
            .filter(|picture| picture.poc < current_poc)
            .collect();
        let mut after: Vec<_> = self
            .short
            .iter()
            .filter(|picture| picture.poc > current_poc)
            .collect();
        before.sort_by_key(|picture| std::cmp::Reverse(picture.poc));
        after.sort_by_key(|picture| picture.poc);

        let long: Vec<_> = self.long.iter().flatten().map(long_reference).collect();
        let mut list0: Vec<_> = before
            .iter()
            .chain(after.iter())
            .map(|picture| short_reference(picture))
            .collect();
        list0.extend_from_slice(&long);
        let mut list1: Vec<_> = after
            .iter()
            .chain(before.iter())
            .map(|picture| short_reference(picture))
            .collect();
        list1.extend_from_slice(&long);
        if list0 == list1 && list1.len() > 1 {
            list1.swap(0, 1);
        }
        ReferenceLists { list0, list1 }
    }

    fn apply_modifications(
        &self,
        list: &mut Vec<ReferencePicture>,
        modifications: &[RefPicListModification],
        header: &SliceHeader,
        sps: &SequenceParameterSet,
    ) -> VideoResult<()> {
        let maximum = header.maximum_picture_number(sps);
        let mut predicted = header.current_picture_number();
        for (index, modification) in modifications.iter().enumerate() {
            let reference = match *modification {
                RefPicListModification::Subtract { difference } => {
                    predicted = predicted.wrapping_sub(difference) & (maximum - 1);
                    self.short
                        .iter()
                        .find(|picture| picture.frame_num == predicted)
                        .map(short_reference)
                }
                RefPicListModification::Add { difference } => {
                    predicted = predicted.wrapping_add(difference) & (maximum - 1);
                    self.short
                        .iter()
                        .find(|picture| picture.frame_num == predicted)
                        .map(short_reference)
                }
                RefPicListModification::LongTerm { picture_number } => {
                    usize::try_from(picture_number)
                        .ok()
                        .and_then(|long_index| self.long.get(long_index))
                        .and_then(Option::as_ref)
                        .map(long_reference)
                }
            }
            .ok_or_else(|| avc_error("apply_modifications", "reference picture is missing"))?;
            list.retain(|entry| entry.id != reference.id);
            list.insert(index, reference);
        }
        Ok(())
    }
}

fn type_one_poc(
    header: &SliceHeader,
    sps: &SequenceParameterSet,
    frame_num_offset: u32,
) -> VideoResult<[i32; 2]> {
    let cycle_length = u32::try_from(sps.offset_for_ref_frame.len())
        .map_err(|_| avc_error("type_one_poc", "POC cycle length exceeds u32"))?;
    let mut absolute = frame_num_offset
        .checked_add(header.frame_num)
        .ok_or_else(|| avc_error("type_one_poc", "absolute frame number overflowed"))?;
    if header.nal.reference_idc == 0 && absolute > 0 {
        absolute -= 1;
    }
    let mut expected = 0_i32;
    if absolute > 0 && cycle_length > 0 {
        let expected_delta: i32 =
            sps.offset_for_ref_frame
                .iter()
                .try_fold(0_i32, |sum, value| {
                    sum.checked_add(*value)
                        .ok_or_else(|| avc_error("type_one_poc", "expected POC delta overflowed"))
                })?;
        let cycles = (absolute - 1) / cycle_length;
        expected = i32::try_from(cycles)
            .map_err(|_| avc_error("type_one_poc", "POC cycle count exceeds i32"))?
            .checked_mul(expected_delta)
            .ok_or_else(|| avc_error("type_one_poc", "expected POC overflowed"))?;
        let within = (absolute - 1) % cycle_length;
        for offset in sps.offset_for_ref_frame.iter().take(
            usize::try_from(within + 1)
                .map_err(|_| avc_error("type_one_poc", "cycle offset exceeds usize"))?,
        ) {
            expected = expected
                .checked_add(*offset)
                .ok_or_else(|| avc_error("type_one_poc", "expected POC overflowed"))?;
        }
    }
    if header.nal.reference_idc == 0 {
        expected = expected
            .checked_add(sps.offset_for_non_ref_pic)
            .ok_or_else(|| avc_error("type_one_poc", "non-reference POC overflowed"))?;
    }
    let top = expected
        .checked_add(header.delta_poc[0])
        .ok_or_else(|| avc_error("type_one_poc", "top-field POC overflowed"))?;
    let bottom = top
        .checked_add(sps.offset_for_top_to_bottom_field)
        .and_then(|value| value.checked_add(header.delta_poc[1]))
        .ok_or_else(|| avc_error("type_one_poc", "bottom-field POC overflowed"))?;
    Ok(match header.structure {
        PictureStructure::TopField => [top, i32::MAX],
        PictureStructure::BottomField => [i32::MAX, bottom],
        PictureStructure::Frame => [top, bottom],
    })
}

fn short_reference(picture: &Picture) -> ReferencePicture {
    ReferencePicture {
        id: picture.id,
        picture_number: picture.frame_num,
        poc: picture.poc,
        long_term: false,
    }
}

fn long_reference(picture: &Picture) -> ReferencePicture {
    ReferencePicture {
        id: picture.id,
        picture_number: u32::from(picture.long_term_index.unwrap_or_default()),
        poc: picture.poc,
        long_term: true,
    }
}

fn require_and_truncate(
    list: &mut Vec<ReferencePicture>,
    required: u8,
    name: &'static str,
) -> VideoResult<()> {
    let required = usize::from(required);
    if list.len() < required {
        return Err(avc_error(
            "DecodedPictureBuffer::build_reference_lists",
            format!("{name} has fewer pictures than requested"),
        ));
    }
    list.truncate(required);
    Ok(())
}
