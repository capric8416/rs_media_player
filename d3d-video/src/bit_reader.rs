use crate::{VideoError, VideoErrorKind, VideoResult};

/// Checked, MSB-first bit reader matching `FFmpeg`'s default `GetBitContext` order.
#[derive(Clone, Copy, Debug)]
pub(crate) struct BitReader<'data> {
    data: &'data [u8],
    bit_position: usize,
    bit_len: usize,
}

impl<'data> BitReader<'data> {
    pub(crate) fn new(data: &'data [u8]) -> Self {
        Self {
            data,
            bit_position: 0,
            bit_len: data.len().saturating_mul(8),
        }
    }

    pub(crate) fn with_bit_len(data: &'data [u8], bit_len: usize) -> VideoResult<Self> {
        let available = data.len().checked_mul(8).ok_or_else(|| {
            bit_error("BitReader::with_bit_len", 0, "input bit length overflowed")
        })?;
        if bit_len > available {
            return Err(bit_error(
                "BitReader::with_bit_len",
                0,
                "requested bit length exceeds the input",
            ));
        }
        Ok(Self {
            data,
            bit_position: 0,
            bit_len,
        })
    }

    pub(crate) const fn position(&self) -> usize {
        self.bit_position
    }

    pub(crate) const fn bits_remaining(&self) -> usize {
        self.bit_len - self.bit_position
    }

    pub(crate) fn read_bit(&mut self) -> VideoResult<bool> {
        Ok(self.read_bits(1)? != 0)
    }

    pub(crate) fn read_bits(&mut self, count: u8) -> VideoResult<u64> {
        if count > 64 {
            return Err(bit_error(
                "BitReader::read_bits",
                self.bit_position,
                "a single read cannot exceed 64 bits",
            ));
        }
        let count = usize::from(count);
        if count > self.bits_remaining() {
            return Err(bit_error(
                "BitReader::read_bits",
                self.bit_position,
                "not enough bits remain",
            ));
        }

        let mut value = 0_u64;
        let mut remaining = count;
        while remaining > 0 {
            let byte_index = self.bit_position / 8;
            let bit_offset = self.bit_position % 8;
            let available = 8 - bit_offset;
            let take = available.min(remaining);
            let shift = available - take;
            let mask = u16::MAX >> (16 - take);
            let chunk = (u16::from(self.data[byte_index]) >> shift) & mask;
            value = (value << take) | u64::from(chunk);
            self.bit_position += take;
            remaining -= take;
        }
        Ok(value)
    }

    pub(crate) fn peek_bits(&self, count: u8) -> VideoResult<u64> {
        let mut copy = *self;
        copy.read_bits(count)
    }

    pub(crate) fn skip_bits(&mut self, count: usize) -> VideoResult<()> {
        if count > self.bits_remaining() {
            return Err(bit_error(
                "BitReader::skip_bits",
                self.bit_position,
                "skip crosses the bitstream end",
            ));
        }
        self.bit_position += count;
        Ok(())
    }

    pub(crate) fn align_to_byte(&mut self) -> VideoResult<usize> {
        let skipped = (8 - self.bit_position % 8) % 8;
        self.skip_bits(skipped)?;
        Ok(skipped)
    }

    /// Returns whether bits other than the `rbsp_stop_one_bit` and its zero padding remain.
    pub(crate) fn has_more_rbsp_data(&self) -> VideoResult<bool> {
        if self.bits_remaining() == 0 {
            return Ok(false);
        }
        let mut copy = *self;
        if !copy.read_bit()? {
            return Ok(true);
        }
        while copy.bits_remaining() > 0 {
            if copy.read_bit()? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub(crate) fn read_ue(&mut self) -> VideoResult<u32> {
        let start = self.bit_position;
        let mut leading_zeroes = 0_u8;
        while !self.read_bit()? {
            leading_zeroes = leading_zeroes.checked_add(1).ok_or_else(|| {
                bit_error(
                    "BitReader::read_ue",
                    start,
                    "Exp-Golomb prefix length overflowed",
                )
            })?;
            if leading_zeroes > 32 {
                return Err(bit_error(
                    "BitReader::read_ue",
                    start,
                    "unsigned Exp-Golomb value exceeds 32 bits",
                ));
            }
        }
        if leading_zeroes == 0 {
            return Ok(0);
        }
        let suffix = self.read_bits(leading_zeroes)?;
        let value = ((1_u64 << leading_zeroes) - 1) + suffix;
        u32::try_from(value).map_err(|_| {
            bit_error(
                "BitReader::read_ue",
                start,
                "unsigned Exp-Golomb value exceeds u32",
            )
        })
    }

    pub(crate) fn read_se(&mut self) -> VideoResult<i32> {
        let start = self.bit_position;
        let code = self.read_ue()?;
        let magnitude = u64::from(code).div_ceil(2);
        if code & 1 == 0 {
            let magnitude = i32::try_from(magnitude).map_err(|_| {
                bit_error(
                    "BitReader::read_se",
                    start,
                    "negative Exp-Golomb magnitude exceeds i32",
                )
            })?;
            Ok(-magnitude)
        } else {
            i32::try_from(magnitude).map_err(|_| {
                bit_error(
                    "BitReader::read_se",
                    start,
                    "positive Exp-Golomb magnitude exceeds i32",
                )
            })
        }
    }

    pub(crate) fn read_leb128(&mut self) -> VideoResult<u64> {
        let start = self.bit_position;
        if !self.bit_position.is_multiple_of(8) {
            return Err(bit_error(
                "BitReader::read_leb128",
                start,
                "LEB128 must begin on a byte boundary",
            ));
        }
        let mut value = 0_u64;
        for index in 0..8_u32 {
            let byte = u8::try_from(self.read_bits(8)?).map_err(|_| {
                bit_error(
                    "BitReader::read_leb128",
                    start,
                    "byte read could not be represented",
                )
            })?;
            value |= u64::from(byte & 0x7f) << (index * 7);
            if byte & 0x80 == 0 {
                break;
            }
        }
        Ok(value)
    }
}

fn bit_error(operation: &'static str, bit_position: usize, detail: &'static str) -> VideoError {
    VideoError::new(
        VideoErrorKind::InvalidInput,
        operation,
        format!("at bit {bit_position}: {detail}"),
    )
}

#[cfg(test)]
mod tests {
    use super::BitReader;
    use crate::{VideoError, VideoErrorKind, VideoResult};

    #[test]
    fn reads_peeks_skips_and_aligns_msb_first() -> VideoResult<()> {
        let mut reader = BitReader::new(&[0b1011_0010, 0b0110_1101]);
        assert_eq!(reader.peek_bits(4)?, 0b1011);
        assert_eq!(reader.position(), 0);
        assert_eq!(reader.read_bits(3)?, 0b101);
        reader.skip_bits(2)?;
        assert_eq!(reader.read_bits(5)?, 0b1001);
        assert_eq!(reader.align_to_byte()?, 6);
        assert_eq!(reader.bits_remaining(), 0);
        Ok(())
    }

    #[test]
    fn respects_explicit_bit_length_and_truncation() -> VideoResult<()> {
        let mut reader = BitReader::with_bit_len(&[0xff, 0xff], 9)?;
        assert_eq!(reader.read_bits(9)?, 0x1ff);
        let error = reader.read_bit().err().ok_or_else(|| {
            VideoError::new(
                VideoErrorKind::InvalidState,
                "respects_explicit_bit_length_and_truncation",
                "truncated read unexpectedly succeeded",
            )
        })?;
        assert_eq!(error.kind(), VideoErrorKind::InvalidInput);
        Ok(())
    }

    #[test]
    fn decodes_unsigned_and_signed_exp_golomb() -> VideoResult<()> {
        // ue: 0=1, 1=010, 2=011, 3=00100; se uses the same code numbers.
        let mut unsigned = BitReader::with_bit_len(&[0b1010_0110, 0b0100_0000], 12)?;
        assert_eq!(unsigned.read_ue()?, 0);
        assert_eq!(unsigned.read_ue()?, 1);
        assert_eq!(unsigned.read_ue()?, 2);
        assert_eq!(unsigned.read_ue()?, 3);

        let mut signed = BitReader::with_bit_len(&[0b1010_0110, 0b0100_0000], 12)?;
        assert_eq!(signed.read_se()?, 0);
        assert_eq!(signed.read_se()?, 1);
        assert_eq!(signed.read_se()?, -1);
        assert_eq!(signed.read_se()?, 2);
        Ok(())
    }

    #[test]
    fn reads_ffmpeg_style_eight_byte_leb128() -> VideoResult<()> {
        let mut reader = BitReader::new(&[0x81, 0x82, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80]);
        assert_eq!(reader.read_leb128()?, 257);
        assert_eq!(reader.bits_remaining(), 0);
        Ok(())
    }

    #[test]
    fn rejects_exp_golomb_prefix_over_32_bits() -> VideoResult<()> {
        let mut reader = BitReader::new(&[0, 0, 0, 0, 0]);
        let error = reader.read_ue().err().ok_or_else(|| {
            VideoError::new(
                VideoErrorKind::InvalidState,
                "rejects_exp_golomb_prefix_over_32_bits",
                "oversized Exp-Golomb prefix unexpectedly passed",
            )
        })?;
        assert_eq!(error.kind(), VideoErrorKind::InvalidInput);
        Ok(())
    }

    #[test]
    fn recognizes_rbsp_trailing_bits() -> VideoResult<()> {
        let mut only_trailing = BitReader::new(&[0b1011_0000]);
        only_trailing.skip_bits(3)?;
        assert!(!only_trailing.has_more_rbsp_data()?);

        let mut more_data = BitReader::new(&[0b1010_1000]);
        more_data.skip_bits(3)?;
        assert!(more_data.has_more_rbsp_data()?);
        Ok(())
    }
}
