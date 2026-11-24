use crate::{Mp4Error, Mp4ErrorKind, Mp4Result};

pub(super) struct Cursor<'data> {
    data: &'data [u8],
    position: usize,
    operation: &'static str,
}

impl<'data> Cursor<'data> {
    pub(super) const fn new(data: &'data [u8], operation: &'static str) -> Self {
        Self {
            data,
            position: 0,
            operation,
        }
    }

    pub(super) const fn position(&self) -> usize {
        self.position
    }

    pub(super) const fn remaining(&self) -> usize {
        self.data.len() - self.position
    }

    pub(super) fn u8(&mut self) -> Mp4Result<u8> {
        let value = *self
            .data
            .get(self.position)
            .ok_or_else(|| self.truncated())?;
        self.position += 1;
        Ok(value)
    }

    pub(super) fn u16(&mut self) -> Mp4Result<u16> {
        let bytes = self.bytes(2)?;
        Ok(u16::from_be_bytes([bytes[0], bytes[1]]))
    }

    pub(super) fn i16(&mut self) -> Mp4Result<i16> {
        let bytes = self.bytes(2)?;
        Ok(i16::from_be_bytes([bytes[0], bytes[1]]))
    }

    pub(super) fn u24(&mut self) -> Mp4Result<u32> {
        let bytes = self.bytes(3)?;
        Ok(u32::from_be_bytes([0, bytes[0], bytes[1], bytes[2]]))
    }

    pub(super) fn u32(&mut self) -> Mp4Result<u32> {
        let bytes = self.bytes(4)?;
        Ok(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    pub(super) fn u48(&mut self) -> Mp4Result<u64> {
        let bytes = self.bytes(6)?;
        Ok(u64::from_be_bytes([
            0, 0, bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5],
        ]))
    }

    pub(super) fn bytes(&mut self, length: usize) -> Mp4Result<&'data [u8]> {
        let end = self
            .position
            .checked_add(length)
            .filter(|end| *end <= self.data.len())
            .ok_or_else(|| self.truncated())?;
        let bytes = &self.data[self.position..end];
        self.position = end;
        Ok(bytes)
    }

    pub(super) fn tail(&mut self) -> &'data [u8] {
        let bytes = &self.data[self.position..];
        self.position = self.data.len();
        bytes
    }

    pub(super) fn finish(&self) -> Mp4Result<()> {
        if self.remaining() != 0 {
            return Err(error(
                Mp4ErrorKind::InvalidData,
                self.operation,
                self.position,
                "unexpected trailing bytes",
            ));
        }
        Ok(())
    }

    fn truncated(&self) -> Mp4Error {
        error(
            Mp4ErrorKind::Truncated,
            self.operation,
            self.position,
            "codec configuration is truncated",
        )
    }
}

pub(super) struct BitCursor<'data> {
    data: &'data [u8],
    bit_position: usize,
    operation: &'static str,
}

impl<'data> BitCursor<'data> {
    pub(super) const fn new(data: &'data [u8], operation: &'static str) -> Self {
        Self {
            data,
            bit_position: 0,
            operation,
        }
    }

    pub(super) fn bits(&mut self, count: u8) -> Mp4Result<u32> {
        if count > 32 || usize::from(count) > self.remaining() {
            return Err(error(
                Mp4ErrorKind::Truncated,
                self.operation,
                self.bit_position / 8,
                "audio configuration is truncated",
            ));
        }
        let mut value = 0_u32;
        for _ in 0..count {
            let byte = self.data[self.bit_position / 8];
            let shift = 7 - self.bit_position % 8;
            value = (value << 1) | u32::from((byte >> shift) & 1);
            self.bit_position += 1;
        }
        Ok(value)
    }

    pub(super) const fn remaining(&self) -> usize {
        self.data.len() * 8 - self.bit_position
    }
}

pub(super) fn error(
    kind: Mp4ErrorKind,
    operation: &'static str,
    offset: usize,
    detail: impl Into<String>,
) -> Mp4Error {
    Mp4Error::new(kind, operation, offset as u64, detail)
}
