use super::rps::ShortTermRps;
use super::{MAX_REFS, NalHeader, hevc_error};
use crate::VideoResult;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SliceType {
    B,
    P,
    I,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct LongTermReference {
    pub(crate) poc: i32,
    pub(crate) used: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Picture {
    pub(crate) id: u64,
    pub(crate) poc: i32,
    pub(crate) temporal_id: u8,
    pub(crate) long_term: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct ReferenceLists {
    pub(crate) list0: Vec<u64>,
    pub(crate) list1: Vec<u64>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct DecodedPictureBuffer {
    pictures: Vec<Picture>,
    poc_tid0: i32,
}

impl DecodedPictureBuffer {
    pub(crate) fn pictures(&self) -> &[Picture] {
        &self.pictures
    }

    #[expect(
        clippy::similar_names,
        reason = "previous POC MSB and LSB are distinct normative HEVC quantities"
    )]
    pub(crate) fn compute_poc(
        &mut self,
        header: NalHeader,
        log2_max_poc_lsb: u8,
        poc_lsb: u32,
    ) -> VideoResult<i32> {
        if header.is_idr() {
            self.poc_tid0 = 0;
            return Ok(0);
        }
        let maximum = 1_i32
            .checked_shl(u32::from(log2_max_poc_lsb))
            .ok_or_else(|| hevc_error("compute_poc", "POC range overflowed"))?;
        let lsb =
            i32::try_from(poc_lsb).map_err(|_| hevc_error("compute_poc", "POC LSB exceeds i32"))?;
        if !(0..maximum).contains(&lsb) {
            return Err(hevc_error(
                "compute_poc",
                "POC LSB is outside its bit range",
            ));
        }
        let previous_lsb = self.poc_tid0.rem_euclid(maximum);
        let previous_msb = self.poc_tid0 - previous_lsb;
        let mut msb = if lsb < previous_lsb && previous_lsb - lsb >= maximum / 2 {
            previous_msb + maximum
        } else if lsb > previous_lsb && lsb - previous_lsb > maximum / 2 {
            previous_msb - maximum
        } else {
            previous_msb
        };
        if header.is_bla() {
            msb = 0;
        }
        let poc = msb + lsb;
        if header.temporal_id == 0 && !matches!(header.unit_type, 8..=9) {
            self.poc_tid0 = poc;
        }
        Ok(poc)
    }

    pub(crate) fn insert_current(&mut self, picture: Picture, capacity: u8) -> VideoResult<()> {
        if self.pictures.iter().any(|entry| entry.poc == picture.poc) {
            return Err(hevc_error("insert_current", "duplicate POC in DPB"));
        }
        if self.pictures.len() >= usize::from(capacity) {
            return Err(hevc_error(
                "insert_current",
                "decoded picture buffer is full",
            ));
        }
        self.pictures.push(picture);
        Ok(())
    }

    pub(crate) fn apply_reference_set(
        &mut self,
        current_poc: i32,
        short: &ShortTermRps,
        long: &[LongTermReference],
    ) -> VideoResult<()> {
        let mut retained = Vec::new();
        for entry in &short.references {
            let poc = current_poc
                .checked_add(entry.delta_poc)
                .ok_or_else(|| hevc_error("apply_reference_set", "short-term POC overflowed"))?;
            let picture = self
                .pictures
                .iter()
                .find(|picture| picture.poc == poc)
                .cloned()
                .ok_or_else(|| {
                    hevc_error(
                        "apply_reference_set",
                        format!("missing short-term POC {poc}"),
                    )
                })?;
            retained.push(Picture {
                long_term: false,
                ..picture
            });
        }
        for reference in long {
            let picture = self
                .pictures
                .iter()
                .find(|picture| picture.poc == reference.poc)
                .cloned()
                .ok_or_else(|| hevc_error("apply_reference_set", "missing long-term reference"))?;
            if !retained.iter().any(|entry| entry.id == picture.id) {
                retained.push(Picture {
                    long_term: true,
                    ..picture
                });
            }
        }
        self.pictures = retained;
        Ok(())
    }

    pub(crate) fn build_reference_lists(
        &self,
        current_poc: i32,
        short: &ShortTermRps,
        long: &[LongTermReference],
        slice_type: SliceType,
        active: [u8; 2],
        modifications: [&[u8]; 2],
    ) -> VideoResult<ReferenceLists> {
        if slice_type == SliceType::I {
            return Ok(ReferenceLists::default());
        }
        let mut before = Vec::new();
        let mut after = Vec::new();
        for entry in short.references.iter().filter(|entry| entry.used) {
            let poc = current_poc + entry.delta_poc;
            let id = self.id_for_poc(poc)?;
            if entry.delta_poc < 0 {
                before.push(id);
            } else {
                after.push(id);
            }
        }
        let mut long_ids = Vec::new();
        for entry in long.iter().filter(|entry| entry.used) {
            long_ids.push(self.id_for_poc(entry.poc)?);
        }
        let candidates0: Vec<_> = before
            .iter()
            .chain(&after)
            .chain(&long_ids)
            .copied()
            .collect();
        let candidates1: Vec<_> = after
            .iter()
            .chain(&before)
            .chain(&long_ids)
            .copied()
            .collect();
        if candidates0.is_empty() {
            return Err(hevc_error(
                "build_reference_lists",
                "inter slice has no current references",
            ));
        }
        let list0 = select_list(&candidates0, active[0], modifications[0])?;
        let list1 = if slice_type == SliceType::B {
            select_list(&candidates1, active[1], modifications[1])?
        } else {
            Vec::new()
        };
        Ok(ReferenceLists { list0, list1 })
    }

    fn id_for_poc(&self, poc: i32) -> VideoResult<u64> {
        self.pictures
            .iter()
            .find(|picture| picture.poc == poc)
            .map(|picture| picture.id)
            .ok_or_else(|| {
                hevc_error(
                    "build_reference_lists",
                    format!("missing reference POC {poc}"),
                )
            })
    }
}

fn select_list(candidates: &[u64], count: u8, modifications: &[u8]) -> VideoResult<Vec<u64>> {
    if usize::from(count) > MAX_REFS {
        return Err(hevc_error(
            "select_list",
            "active reference count exceeds 16",
        ));
    }
    let mut output = Vec::with_capacity(usize::from(count));
    for index in 0..usize::from(count) {
        let candidate = modifications
            .get(index)
            .map_or(index % candidates.len(), |entry| usize::from(*entry));
        let id = candidates.get(candidate).ok_or_else(|| {
            hevc_error("select_list", "reference list modification is out of range")
        })?;
        output.push(*id);
    }
    Ok(output)
}
