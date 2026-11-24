use std::io::{ErrorKind, Read, Seek, SeekFrom};

use crate::{BoxHeader, HeaderParse, Mp4Error, Mp4ErrorKind, Mp4Result, parse_box_header};

/// Bounded box reader for a seekable source.
pub struct SeekBoxReader<R> {
    inner: R,
    stream_end: u64,
    pending: Option<BoxHeader>,
}

impl<R: Read + Seek> SeekBoxReader<R> {
    /// Creates a reader while preserving the source's current position.
    ///
    /// # Errors
    ///
    /// Returns [`Mp4ErrorKind::Io`] when the source length or position cannot be queried.
    pub fn new(mut inner: R) -> Mp4Result<Self> {
        let position = inner
            .stream_position()
            .map_err(|error| io_error("SeekBoxReader::new", 0, "query current position", &error))?;
        let stream_end = inner.seek(SeekFrom::End(0)).map_err(|error| {
            io_error("SeekBoxReader::new", position, "query stream end", &error)
        })?;
        inner.seek(SeekFrom::Start(position)).map_err(|error| {
            io_error(
                "SeekBoxReader::new",
                position,
                "restore current position",
                &error,
            )
        })?;
        Ok(Self {
            inner,
            stream_end,
            pending: None,
        })
    }

    #[must_use]
    pub const fn stream_end(&self) -> u64 {
        self.stream_end
    }

    #[must_use]
    pub fn into_inner(self) -> R {
        self.inner
    }

    /// Reads the next top-level box header.
    ///
    /// # Errors
    ///
    /// Returns a typed I/O, truncation, size, bounds, overflow, or cursor-state error.
    pub fn next_top_level_box(&mut self) -> Mp4Result<Option<BoxHeader>> {
        self.next_box(self.stream_end)
    }

    /// Reads the next child header whose exclusive parent boundary is `container_end`.
    ///
    /// # Errors
    ///
    /// Returns a typed error when a previous payload remains, the boundary is invalid, the header
    /// is truncated/malformed, or the source fails.
    pub fn next_box(&mut self, container_end: u64) -> Mp4Result<Option<BoxHeader>> {
        let position = self.position("SeekBoxReader::next_box")?;
        self.complete_pending_at_end(position)?;
        if let Some(header) = self.pending {
            return Err(Mp4Error::new(
                Mp4ErrorKind::InvalidState,
                "SeekBoxReader::next_box",
                position,
                "consume, skip, or enter the current box payload before reading a sibling",
            )
            .with_box_type(header.box_type()));
        }
        if container_end > self.stream_end || position > container_end {
            return Err(Mp4Error::new(
                Mp4ErrorKind::Bounds,
                "SeekBoxReader::next_box",
                position,
                "container boundary is outside the stream or before the cursor",
            ));
        }
        if position == container_end {
            return Ok(None);
        }
        if container_end - position < 8 {
            return Err(Mp4Error::new(
                Mp4ErrorKind::Truncated,
                "SeekBoxReader::next_box",
                position,
                "fewer than eight bytes remain for a box header",
            ));
        }

        let mut bytes = [0_u8; 16];
        self.read_exact_header(&mut bytes[..8], position)?;
        let size32 = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        let required = if size32 == 1 { 16 } else { 8 };
        if required == 16 {
            if container_end - position < 16 {
                return Err(Mp4Error::new(
                    Mp4ErrorKind::Truncated,
                    "SeekBoxReader::next_box",
                    position,
                    "extended box header crosses the container end",
                ));
            }
            self.read_exact_header(&mut bytes[8..16], position + 8)?;
        }
        let header = match parse_box_header(&bytes[..required], position, Some(container_end))? {
            HeaderParse::Complete(header) => header,
            HeaderParse::NeedMoreData { .. } | HeaderParse::NeedsContainerEnd => {
                return Err(Mp4Error::new(
                    Mp4ErrorKind::InvalidState,
                    "SeekBoxReader::next_box",
                    position,
                    "complete seekable header produced an incomplete parse state",
                ));
            }
        };
        self.pending = Some(header);
        Ok(Some(header))
    }

    /// Reads at most `output.len()` bytes without crossing the current box end.
    ///
    /// # Errors
    ///
    /// Returns a typed cursor-state, truncation, or I/O error.
    pub fn read_payload(&mut self, header: BoxHeader, output: &mut [u8]) -> Mp4Result<usize> {
        let position = self.validate_pending(header, "SeekBoxReader::read_payload")?;
        let remaining = header.end() - position;
        if remaining == 0 || output.is_empty() {
            self.complete_pending_at_end(position)?;
            return Ok(0);
        }
        let count = usize::try_from(remaining.min(output.len() as u64)).map_err(|_| {
            Mp4Error::new(
                Mp4ErrorKind::Overflow,
                "SeekBoxReader::read_payload",
                position,
                "payload read length cannot be represented",
            )
            .with_box_type(header.box_type())
        })?;
        let read = self.inner.read(&mut output[..count]).map_err(|error| {
            io_error(
                "SeekBoxReader::read_payload",
                position,
                "read box payload",
                &error,
            )
            .with_box_type(header.box_type())
        })?;
        if read == 0 {
            return Err(Mp4Error::new(
                Mp4ErrorKind::Truncated,
                "SeekBoxReader::read_payload",
                position,
                "source ended before the declared box payload",
            )
            .with_box_type(header.box_type()));
        }
        let new_position = position + read as u64;
        self.complete_pending_at_end(new_position)?;
        Ok(read)
    }

    /// Reads the remaining payload into memory under an explicit allocation ceiling.
    ///
    /// # Errors
    ///
    /// Returns [`Mp4ErrorKind::BufferLimit`] when the remaining payload exceeds `max_bytes`, or a
    /// typed state, truncation, overflow, or I/O error.
    pub fn read_payload_to_vec(
        &mut self,
        header: BoxHeader,
        max_bytes: usize,
    ) -> Mp4Result<Vec<u8>> {
        let position = self.validate_pending(header, "SeekBoxReader::read_payload_to_vec")?;
        let remaining = header.end() - position;
        let length = usize::try_from(remaining).map_err(|_| {
            Mp4Error::new(
                Mp4ErrorKind::BufferLimit,
                "SeekBoxReader::read_payload_to_vec",
                position,
                "remaining payload does not fit in memory address space",
            )
            .with_box_type(header.box_type())
        })?;
        if length > max_bytes {
            return Err(Mp4Error::new(
                Mp4ErrorKind::BufferLimit,
                "SeekBoxReader::read_payload_to_vec",
                position,
                format!("payload length {length} exceeds caller limit {max_bytes}"),
            )
            .with_box_type(header.box_type()));
        }
        let mut payload = vec![0_u8; length];
        self.inner.read_exact(&mut payload).map_err(|error| {
            let kind = if error.kind() == ErrorKind::UnexpectedEof {
                Mp4ErrorKind::Truncated
            } else {
                Mp4ErrorKind::Io
            };
            Mp4Error::new(
                kind,
                "SeekBoxReader::read_payload_to_vec",
                position,
                format!("failed to read payload: {error}"),
            )
            .with_box_type(header.box_type())
        })?;
        self.pending = None;
        Ok(payload)
    }

    /// Seeks to the effective end of the current box.
    ///
    /// # Errors
    ///
    /// Returns a typed cursor-state or I/O error.
    pub fn skip_payload(&mut self, header: BoxHeader) -> Mp4Result<()> {
        self.validate_pending(header, "SeekBoxReader::skip_payload")?;
        self.inner
            .seek(SeekFrom::Start(header.end()))
            .map_err(|error| {
                io_error(
                    "SeekBoxReader::skip_payload",
                    header.payload_offset(),
                    "seek to box end",
                    &error,
                )
                .with_box_type(header.box_type())
            })?;
        self.pending = None;
        Ok(())
    }

    /// Treats the current payload as a child-box container and returns its exclusive end.
    ///
    /// # Errors
    ///
    /// Returns [`Mp4ErrorKind::InvalidState`] unless the cursor is at this header's payload start.
    pub fn enter_children(&mut self, header: BoxHeader) -> Mp4Result<u64> {
        let position = self.validate_pending(header, "SeekBoxReader::enter_children")?;
        if position != header.payload_offset() {
            return Err(Mp4Error::new(
                Mp4ErrorKind::InvalidState,
                "SeekBoxReader::enter_children",
                position,
                "child traversal must begin at the payload start",
            )
            .with_box_type(header.box_type()));
        }
        self.pending = None;
        Ok(header.end())
    }

    fn position(&mut self, operation: &'static str) -> Mp4Result<u64> {
        self.inner
            .stream_position()
            .map_err(|error| io_error(operation, 0, "query stream position", &error))
    }

    fn validate_pending(&mut self, header: BoxHeader, operation: &'static str) -> Mp4Result<u64> {
        let position = self.position(operation)?;
        if self.pending != Some(header)
            || position < header.payload_offset()
            || position > header.end()
        {
            return Err(Mp4Error::new(
                Mp4ErrorKind::InvalidState,
                operation,
                position,
                "header is not the current bounded payload",
            )
            .with_box_type(header.box_type()));
        }
        Ok(position)
    }

    fn complete_pending_at_end(&mut self, position: u64) -> Mp4Result<()> {
        if let Some(header) = self.pending {
            if position > header.end() {
                return Err(Mp4Error::new(
                    Mp4ErrorKind::Bounds,
                    "SeekBoxReader::complete_pending_at_end",
                    position,
                    "cursor crossed the current box end",
                )
                .with_box_type(header.box_type()));
            }
            if position == header.end() {
                self.pending = None;
            }
        }
        Ok(())
    }

    fn read_exact_header(&mut self, output: &mut [u8], offset: u64) -> Mp4Result<()> {
        self.inner.read_exact(output).map_err(|error| {
            let kind = if error.kind() == ErrorKind::UnexpectedEof {
                Mp4ErrorKind::Truncated
            } else {
                Mp4ErrorKind::Io
            };
            Mp4Error::new(
                kind,
                "SeekBoxReader::next_box",
                offset,
                format!("failed to read box header: {error}"),
            )
        })
    }
}

fn io_error(
    operation: &'static str,
    offset: u64,
    action: &'static str,
    error: &std::io::Error,
) -> Mp4Error {
    Mp4Error::new(
        Mp4ErrorKind::Io,
        operation,
        offset,
        format!("{action}: {error}"),
    )
}
