use crate::{VideoError, VideoErrorKind, VideoResult};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct NalUnit<'data> {
    pub(crate) data: &'data [u8],
    pub(crate) offset: usize,
}

pub(crate) struct AnnexBNalIter<'data> {
    data: &'data [u8],
    nal_start: usize,
}

impl<'data> AnnexBNalIter<'data> {
    pub(crate) fn new(data: &'data [u8]) -> VideoResult<Self> {
        let (_, nal_start) = find_start_code(data, 0)
            .ok_or_else(|| nal_error("AnnexBNalIter::new", 0, "no Annex B start code was found"))?;
        Ok(Self { data, nal_start })
    }
}

impl<'data> Iterator for AnnexBNalIter<'data> {
    type Item = NalUnit<'data>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if self.nal_start >= self.data.len() {
                return None;
            }
            let start = self.nal_start;
            let (end, next_start) =
                find_start_code(self.data, start).unwrap_or((self.data.len(), self.data.len()));
            self.nal_start = next_start;
            if end > start {
                return Some(NalUnit {
                    data: &self.data[start..end],
                    offset: start,
                });
            }
        }
    }
}

pub(crate) struct LengthPrefixedNalIter<'data> {
    data: &'data [u8],
    cursor: usize,
    length_size: usize,
    failed: bool,
}

impl<'data> LengthPrefixedNalIter<'data> {
    pub(crate) fn new(data: &'data [u8], length_size: u8) -> VideoResult<Self> {
        if !(1..=4).contains(&length_size) {
            return Err(nal_error(
                "LengthPrefixedNalIter::new",
                0,
                "NAL length size must contain one through four bytes",
            ));
        }
        Ok(Self {
            data,
            cursor: 0,
            length_size: usize::from(length_size),
            failed: false,
        })
    }
}

impl<'data> Iterator for LengthPrefixedNalIter<'data> {
    type Item = VideoResult<NalUnit<'data>>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.failed || self.cursor == self.data.len() {
            return None;
        }
        loop {
            let length_end = match self.cursor.checked_add(self.length_size) {
                Some(end) if end <= self.data.len() => end,
                _ => {
                    self.failed = true;
                    return Some(Err(nal_error(
                        "LengthPrefixedNalIter::next",
                        self.cursor,
                        "truncated NAL length field",
                    )));
                }
            };
            let mut length = 0_usize;
            for byte in &self.data[self.cursor..length_end] {
                length = (length << 8) | usize::from(*byte);
            }
            let payload_start = length_end;
            let payload_end = match payload_start.checked_add(length) {
                Some(end) if end <= self.data.len() => end,
                _ => {
                    self.failed = true;
                    return Some(Err(nal_error(
                        "LengthPrefixedNalIter::next",
                        self.cursor,
                        "declared NAL length exceeds the packet",
                    )));
                }
            };
            self.cursor = payload_end;
            if length > 0 {
                return Some(Ok(NalUnit {
                    data: &self.data[payload_start..payload_end],
                    offset: payload_start,
                }));
            }
            if self.cursor == self.data.len() {
                return None;
            }
        }
    }
}

fn find_start_code(data: &[u8], from: usize) -> Option<(usize, usize)> {
    let mut zero_start = from;
    let mut zero_count = 0_usize;
    for (index, byte) in data.iter().copied().enumerate().skip(from) {
        if byte == 0 {
            if zero_count == 0 {
                zero_start = index;
            }
            zero_count += 1;
        } else {
            if byte == 1 && zero_count >= 2 {
                return Some((zero_start, index + 1));
            }
            zero_count = 0;
        }
    }
    None
}

fn nal_error(operation: &'static str, offset: usize, detail: &'static str) -> VideoError {
    VideoError::new(
        VideoErrorKind::InvalidInput,
        operation,
        format!("at byte {offset}: {detail}"),
    )
}

#[cfg(test)]
mod tests {
    use super::{AnnexBNalIter, LengthPrefixedNalIter};
    use crate::{VideoError, VideoErrorKind, VideoResult};

    #[test]
    fn splits_three_and_four_byte_annex_b_prefixes() -> VideoResult<()> {
        let packet = [0xaa, 0, 0, 1, 0x67, 1, 2, 0, 0, 0, 1, 0x68, 3];
        let units: Vec<_> = AnnexBNalIter::new(&packet)?.collect();
        assert_eq!(units.len(), 2);
        assert_eq!(units[0].data, [0x67, 1, 2]);
        assert_eq!(units[1].data, [0x68, 3]);
        assert_eq!(units[0].offset, 4);
        Ok(())
    }

    #[test]
    fn ignores_empty_annex_b_units() -> VideoResult<()> {
        let packet = [0, 0, 1, 0, 0, 1, 0x65];
        let units: Vec<_> = AnnexBNalIter::new(&packet)?.collect();
        assert_eq!(units.len(), 1);
        assert_eq!(units[0].data, [0x65]);
        Ok(())
    }

    #[test]
    fn splits_all_length_prefix_widths() -> VideoResult<()> {
        for length_size in 1..=4_u8 {
            let mut packet = vec![0_u8; usize::from(length_size)];
            let last = packet.len() - 1;
            packet[last] = 2;
            packet.extend_from_slice(&[0x67, 0x42]);
            let units: VideoResult<Vec<_>> =
                LengthPrefixedNalIter::new(&packet, length_size)?.collect();
            let units = units?;
            assert_eq!(units.len(), 1);
            assert_eq!(units[0].data, [0x67, 0x42]);
        }
        Ok(())
    }

    #[test]
    fn rejects_length_beyond_packet() -> VideoResult<()> {
        let mut units = LengthPrefixedNalIter::new(&[0, 4, 0x67], 2)?;
        let error = units
            .next()
            .ok_or_else(|| {
                VideoError::new(
                    VideoErrorKind::InvalidState,
                    "rejects_length_beyond_packet",
                    "iterator emitted no result",
                )
            })?
            .err()
            .ok_or_else(|| {
                VideoError::new(
                    VideoErrorKind::InvalidState,
                    "rejects_length_beyond_packet",
                    "invalid NAL length unexpectedly passed",
                )
            })?;
        assert_eq!(error.kind(), VideoErrorKind::InvalidInput);
        Ok(())
    }

    #[test]
    fn rejects_missing_start_code_and_truncated_length() -> VideoResult<()> {
        let missing = AnnexBNalIter::new(&[0x67, 0x42]).err().ok_or_else(|| {
            VideoError::new(
                VideoErrorKind::InvalidState,
                "rejects_missing_start_code_and_truncated_length",
                "packet without a start code unexpectedly passed",
            )
        })?;
        assert_eq!(missing.kind(), VideoErrorKind::InvalidInput);

        let mut units = LengthPrefixedNalIter::new(&[0], 2)?;
        let truncated = units
            .next()
            .ok_or_else(|| {
                VideoError::new(
                    VideoErrorKind::InvalidState,
                    "rejects_missing_start_code_and_truncated_length",
                    "truncated prefix emitted no result",
                )
            })?
            .err()
            .ok_or_else(|| {
                VideoError::new(
                    VideoErrorKind::InvalidState,
                    "rejects_missing_start_code_and_truncated_length",
                    "truncated prefix unexpectedly passed",
                )
            })?;
        assert_eq!(truncated.kind(), VideoErrorKind::InvalidInput);
        Ok(())
    }
}
