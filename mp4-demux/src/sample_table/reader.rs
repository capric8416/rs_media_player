use crate::{Mp4Error, Mp4ErrorKind, Mp4Result};

pub(super) struct Reader<'data> {
    data: &'data [u8],
    position: usize,
    operation: &'static str,
}

impl<'data> Reader<'data> {
    pub(super) const fn new(data: &'data [u8], operation: &'static str) -> Self {
        Self {
            data,
            position: 0,
            operation,
        }
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

    pub(super) fn u24(&mut self) -> Mp4Result<u32> {
        let bytes = self.bytes(3)?;
        Ok(u32::from_be_bytes([0, bytes[0], bytes[1], bytes[2]]))
    }

    pub(super) fn u32(&mut self) -> Mp4Result<u32> {
        let bytes = self.bytes(4)?;
        Ok(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    pub(super) fn i32(&mut self) -> Mp4Result<i32> {
        let bytes = self.bytes(4)?;
        Ok(i32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    pub(super) fn u64(&mut self) -> Mp4Result<u64> {
        let bytes = self.bytes(8)?;
        Ok(u64::from_be_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
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

    pub(super) fn full_box(&mut self, supported_versions: &[u8]) -> Mp4Result<(u8, u32)> {
        let version = self.u8()?;
        let flags = self.u24()?;
        if !supported_versions.contains(&version) {
            return Err(self.error(Mp4ErrorKind::Unsupported, "unsupported full-box version"));
        }
        Ok((version, flags))
    }

    pub(super) fn finish(&self) -> Mp4Result<()> {
        if self.remaining() != 0 {
            return Err(self.error(Mp4ErrorKind::InvalidData, "unexpected trailing bytes"));
        }
        Ok(())
    }

    pub(super) fn error(&self, kind: Mp4ErrorKind, detail: impl Into<String>) -> Mp4Error {
        Mp4Error::new(
            kind,
            self.operation,
            u64::try_from(self.position).unwrap_or(u64::MAX),
            detail,
        )
    }

    fn truncated(&self) -> Mp4Error {
        self.error(Mp4ErrorKind::Truncated, "sample-table box is truncated")
    }
}
