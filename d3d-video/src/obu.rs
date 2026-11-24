use crate::{VideoError, VideoErrorKind, VideoResult};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct ObuType(u8);

impl ObuType {
    pub(crate) const SEQUENCE_HEADER: Self = Self(1);
    pub(crate) const TEMPORAL_DELIMITER: Self = Self(2);
    pub(crate) const FRAME_HEADER: Self = Self(3);
    pub(crate) const TILE_GROUP: Self = Self(4);
    pub(crate) const METADATA: Self = Self(5);
    pub(crate) const FRAME: Self = Self(6);
    pub(crate) const REDUNDANT_FRAME_HEADER: Self = Self(7);
    pub(crate) const TILE_LIST: Self = Self(8);
    pub(crate) const PADDING: Self = Self(15);

    pub(crate) const fn value(self) -> u8 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Obu<'data> {
    pub(crate) kind: ObuType,
    pub(crate) temporal_id: u8,
    pub(crate) spatial_id: u8,
    pub(crate) header_len: usize,
    pub(crate) payload: &'data [u8],
    pub(crate) raw: &'data [u8],
}

pub(crate) fn parse_obu(input: &[u8]) -> VideoResult<Obu<'_>> {
    let first = *input
        .first()
        .ok_or_else(|| obu_error("parse_obu", 0, "an OBU requires at least one header byte"))?;
    if first & 0x80 != 0 {
        return Err(obu_error("parse_obu", 0, "forbidden bit is set"));
    }
    let obu_type = ObuType((first >> 3) & 0x0f);
    let extension_flag = first & 0x04 != 0;
    let has_size_flag = first & 0x02 != 0;
    let mut cursor = 1_usize;
    let (temporal_id, spatial_id) = if extension_flag {
        let extension = *input
            .get(cursor)
            .ok_or_else(|| obu_error("parse_obu", cursor, "extension header is truncated"))?;
        cursor += 1;
        ((extension >> 5) & 0x07, (extension >> 3) & 0x03)
    } else {
        (0, 0)
    };

    let payload_size = if has_size_flag {
        let (size, length) = read_leb128(&input[cursor..], cursor)?;
        cursor = cursor
            .checked_add(length)
            .ok_or_else(|| obu_error("parse_obu", cursor, "OBU header length overflowed"))?;
        usize::try_from(size)
            .map_err(|_| obu_error("parse_obu", cursor, "OBU payload size is too large"))?
    } else {
        input.len() - cursor
    };
    let end = cursor
        .checked_add(payload_size)
        .filter(|end| *end <= input.len())
        .ok_or_else(|| {
            obu_error(
                "parse_obu",
                cursor,
                "declared OBU payload exceeds the packet",
            )
        })?;
    Ok(Obu {
        kind: obu_type,
        temporal_id,
        spatial_id,
        header_len: cursor,
        payload: &input[cursor..end],
        raw: &input[..end],
    })
}

pub(crate) struct ObuIter<'data> {
    data: &'data [u8],
    cursor: usize,
    failed: bool,
}

impl<'data> ObuIter<'data> {
    pub(crate) const fn new(data: &'data [u8]) -> Self {
        Self {
            data,
            cursor: 0,
            failed: false,
        }
    }
}

impl<'data> Iterator for ObuIter<'data> {
    type Item = VideoResult<Obu<'data>>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.failed || self.cursor == self.data.len() {
            return None;
        }
        match parse_obu(&self.data[self.cursor..]) {
            Ok(obu) => {
                self.cursor += obu.raw.len();
                Some(Ok(obu))
            }
            Err(error) => {
                self.failed = true;
                Some(Err(error))
            }
        }
    }
}

fn read_leb128(input: &[u8], offset: usize) -> VideoResult<(u64, usize)> {
    let mut value = 0_u64;
    for index in 0..8_usize {
        let byte = *input
            .get(index)
            .ok_or_else(|| obu_error("read_leb128", offset + index, "LEB128 is truncated"))?;
        value |= u64::from(byte & 0x7f) << (index * 7);
        if byte & 0x80 == 0 {
            return Ok((value, index + 1));
        }
    }
    Ok((value, 8))
}

fn obu_error(operation: &'static str, offset: usize, detail: &'static str) -> VideoError {
    VideoError::new(
        VideoErrorKind::InvalidInput,
        operation,
        format!("at byte {offset}: {detail}"),
    )
}

#[cfg(test)]
mod tests {
    use super::{ObuIter, ObuType, parse_obu};
    use crate::{VideoError, VideoErrorKind, VideoResult};

    #[test]
    fn parses_sized_obu_with_extension() -> VideoResult<()> {
        // type=sequence header, extension=1, has_size=1; temporal=3, spatial=2.
        let input = [0x0e, 0x70, 0x02, 0xaa, 0xbb];
        let obu = parse_obu(&input)?;
        assert_eq!(obu.kind, ObuType::SEQUENCE_HEADER);
        assert_eq!(obu.temporal_id, 3);
        assert_eq!(obu.spatial_id, 2);
        assert_eq!(obu.header_len, 3);
        assert_eq!(obu.payload, [0xaa, 0xbb]);
        Ok(())
    }

    #[test]
    fn unsized_obu_consumes_remaining_packet() -> VideoResult<()> {
        let input = [0x30, 1, 2, 3]; // frame OBU, no extension and no size field.
        let obu = parse_obu(&input)?;
        assert_eq!(obu.kind, ObuType::FRAME);
        assert_eq!(obu.payload, [1, 2, 3]);
        assert_eq!(obu.raw, input);
        Ok(())
    }

    #[test]
    fn iterator_splits_sized_obus() -> VideoResult<()> {
        let input = [0x12, 0x00, 0x7a, 0x01, 0xff];
        let units: VideoResult<Vec<_>> = ObuIter::new(&input).collect();
        let units = units?;
        assert_eq!(units.len(), 2);
        assert_eq!(units[0].kind, ObuType::TEMPORAL_DELIMITER);
        assert_eq!(units[1].kind.value(), 15);
        assert_eq!(units[1].payload, [0xff]);
        Ok(())
    }

    #[test]
    fn exposes_ffmpeg_obu_type_values() {
        let types = [
            ObuType::SEQUENCE_HEADER,
            ObuType::TEMPORAL_DELIMITER,
            ObuType::FRAME_HEADER,
            ObuType::TILE_GROUP,
            ObuType::METADATA,
            ObuType::FRAME,
            ObuType::REDUNDANT_FRAME_HEADER,
            ObuType::TILE_LIST,
            ObuType::PADDING,
        ];
        assert_eq!(types.map(ObuType::value), [1, 2, 3, 4, 5, 6, 7, 8, 15]);
    }

    #[test]
    fn rejects_forbidden_bit_and_truncated_size() -> VideoResult<()> {
        let forbidden = parse_obu(&[0x80]).err().ok_or_else(|| {
            VideoError::new(
                VideoErrorKind::InvalidState,
                "rejects_forbidden_bit_and_truncated_size",
                "forbidden bit unexpectedly passed",
            )
        })?;
        assert_eq!(forbidden.kind(), VideoErrorKind::InvalidInput);

        let truncated = parse_obu(&[0x32, 0x80]).err().ok_or_else(|| {
            VideoError::new(
                VideoErrorKind::InvalidState,
                "rejects_forbidden_bit_and_truncated_size",
                "truncated LEB128 unexpectedly passed",
            )
        })?;
        assert_eq!(truncated.kind(), VideoErrorKind::InvalidInput);
        Ok(())
    }

    #[test]
    fn accepts_ffmpeg_style_eight_byte_leb128() -> VideoResult<()> {
        let input = [0x32, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80];
        let obu = parse_obu(&input)?;
        assert_eq!(obu.payload, []);
        assert_eq!(obu.header_len, 9);
        Ok(())
    }

    #[test]
    fn rejects_truncated_extension_and_payload() -> VideoResult<()> {
        for input in [&[0x0c][..], &[0x32, 0x02, 0xaa][..]] {
            let error = parse_obu(input).err().ok_or_else(|| {
                VideoError::new(
                    VideoErrorKind::InvalidState,
                    "rejects_truncated_extension_and_payload",
                    "truncated OBU unexpectedly passed",
                )
            })?;
            assert_eq!(error.kind(), VideoErrorKind::InvalidInput);
        }
        Ok(())
    }
}
