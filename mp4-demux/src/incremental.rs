use crate::{BoxHeader, HeaderParse, Mp4Error, Mp4ErrorKind, Mp4Result, parse_box_header};

/// Incremental box reader with an explicit buffering ceiling.
pub struct IncrementalBoxReader {
    buffer: Vec<u8>,
    start: usize,
    absolute_offset: u64,
    max_buffered_bytes: usize,
    pending: Option<(BoxHeader, u64)>,
    finished: bool,
}

impl IncrementalBoxReader {
    /// Creates an incremental reader.
    ///
    /// # Errors
    ///
    /// Returns [`Mp4ErrorKind::InvalidSize`] when `max_buffered_bytes` is zero.
    pub fn new(max_buffered_bytes: usize) -> Mp4Result<Self> {
        if max_buffered_bytes == 0 {
            return Err(Mp4Error::new(
                Mp4ErrorKind::InvalidSize,
                "IncrementalBoxReader::new",
                0,
                "buffer limit must be non-zero",
            ));
        }
        Ok(Self {
            buffer: Vec::new(),
            start: 0,
            absolute_offset: 0,
            max_buffered_bytes,
            pending: None,
            finished: false,
        })
    }

    #[must_use]
    pub const fn absolute_offset(&self) -> u64 {
        self.absolute_offset
    }

    #[must_use]
    pub fn buffered_len(&self) -> usize {
        self.buffer.len() - self.start
    }

    #[must_use]
    pub fn pending_payload_len(&self) -> Option<u64> {
        self.pending.map(|(_, remaining)| remaining)
    }

    #[must_use]
    pub fn is_eof(&self) -> bool {
        self.finished && self.pending.is_none() && self.buffered_len() == 0
    }

    /// Appends bytes without exceeding the configured unconsumed-byte limit.
    ///
    /// # Errors
    ///
    /// Returns a buffer-limit, overflow, or invalid-state error.
    pub fn push_bytes(&mut self, bytes: &[u8]) -> Mp4Result<()> {
        if self.finished {
            return Err(Mp4Error::new(
                Mp4ErrorKind::InvalidState,
                "IncrementalBoxReader::push_bytes",
                self.absolute_offset,
                "cannot append bytes after end-of-stream",
            ));
        }
        let new_length = self
            .buffered_len()
            .checked_add(bytes.len())
            .ok_or_else(|| {
                Mp4Error::new(
                    Mp4ErrorKind::Overflow,
                    "IncrementalBoxReader::push_bytes",
                    self.absolute_offset,
                    "buffered byte count overflowed",
                )
            })?;
        if new_length > self.max_buffered_bytes {
            return Err(Mp4Error::new(
                Mp4ErrorKind::BufferLimit,
                "IncrementalBoxReader::push_bytes",
                self.absolute_offset,
                format!(
                    "buffered byte count {new_length} exceeds limit {}",
                    self.max_buffered_bytes
                ),
            ));
        }
        self.compact();
        self.buffer.extend_from_slice(bytes);
        Ok(())
    }

    /// Marks the input complete.
    ///
    /// # Errors
    ///
    /// Returns [`Mp4ErrorKind::Truncated`] if a declared payload is missing bytes.
    pub fn finish(&mut self) -> Mp4Result<()> {
        if let Some((header, remaining)) = self.pending
            && remaining > self.buffered_len() as u64
        {
            return Err(Mp4Error::new(
                Mp4ErrorKind::Truncated,
                "IncrementalBoxReader::finish",
                self.absolute_offset + self.buffered_len() as u64,
                "end-of-stream arrived before the declared payload",
            )
            .with_box_type(header.box_type()));
        }
        self.finished = true;
        Ok(())
    }

    /// Parses the next header when enough data and, for size-zero boxes, the parent end are known.
    ///
    /// # Errors
    ///
    /// Returns a typed malformed-header, overflow, truncation, or cursor-state error.
    pub fn next_box(&mut self) -> Mp4Result<Option<BoxHeader>> {
        if let Some((header, remaining)) = self.pending {
            if remaining != 0 {
                return Err(Mp4Error::new(
                    Mp4ErrorKind::InvalidState,
                    "IncrementalBoxReader::next_box",
                    self.absolute_offset,
                    "consume the current payload before reading a sibling",
                )
                .with_box_type(header.box_type()));
            }
            self.pending = None;
        }
        if self.finished && self.buffered_len() == 0 {
            return Ok(None);
        }
        let container_end = if self.finished {
            Some(
                self.absolute_offset
                    .checked_add(self.buffered_len() as u64)
                    .ok_or_else(|| {
                        Mp4Error::new(
                            Mp4ErrorKind::Overflow,
                            "IncrementalBoxReader::next_box",
                            self.absolute_offset,
                            "end-of-stream offset overflowed",
                        )
                    })?,
            )
        } else {
            None
        };
        let input = &self.buffer[self.start..];
        match parse_box_header(input, self.absolute_offset, container_end)? {
            HeaderParse::Complete(header) => {
                let header_size = usize::try_from(header.header_size()).map_err(|_| {
                    Mp4Error::new(
                        Mp4ErrorKind::Overflow,
                        "IncrementalBoxReader::next_box",
                        self.absolute_offset,
                        "header length cannot be represented",
                    )
                    .with_box_type(header.box_type())
                })?;
                self.consume_raw(header_size)?;
                if header.payload_size() > 0 {
                    self.pending = Some((header, header.payload_size()));
                }
                Ok(Some(header))
            }
            HeaderParse::NeedMoreData { required } => {
                if self.finished {
                    Err(Mp4Error::new(
                        Mp4ErrorKind::Truncated,
                        "IncrementalBoxReader::next_box",
                        self.absolute_offset,
                        format!(
                            "end-of-stream left {} header bytes; {required} required",
                            self.buffered_len()
                        ),
                    ))
                } else {
                    Ok(None)
                }
            }
            HeaderParse::NeedsContainerEnd => Ok(None),
        }
    }

    /// Returns the currently buffered portion of the active payload without copying it.
    #[must_use]
    pub fn payload_chunk(&self) -> &[u8] {
        let Some((_, remaining)) = self.pending else {
            return &[];
        };
        let available = self.buffered_len();
        let count = usize::try_from(remaining.min(available as u64)).unwrap_or(available);
        &self.buffer[self.start..self.start + count]
    }

    /// Consumes bytes previously exposed by [`Self::payload_chunk`].
    ///
    /// # Errors
    ///
    /// Returns a cursor-state or overflow error when `count` exceeds the buffered active payload.
    pub fn consume_payload(&mut self, count: usize) -> Mp4Result<()> {
        let Some((header, remaining)) = self.pending else {
            return Err(Mp4Error::new(
                Mp4ErrorKind::InvalidState,
                "IncrementalBoxReader::consume_payload",
                self.absolute_offset,
                "no box payload is active",
            ));
        };
        let count_u64 = count as u64;
        if count > self.payload_chunk().len() || count_u64 > remaining {
            return Err(Mp4Error::new(
                Mp4ErrorKind::Bounds,
                "IncrementalBoxReader::consume_payload",
                self.absolute_offset,
                "consume count exceeds the buffered active payload",
            )
            .with_box_type(header.box_type()));
        }
        self.consume_raw(count)?;
        let new_remaining = remaining - count_u64;
        self.pending = if new_remaining == 0 {
            None
        } else {
            Some((header, new_remaining))
        };
        Ok(())
    }

    fn consume_raw(&mut self, count: usize) -> Mp4Result<()> {
        self.start = self.start.checked_add(count).ok_or_else(|| {
            Mp4Error::new(
                Mp4ErrorKind::Overflow,
                "IncrementalBoxReader::consume_raw",
                self.absolute_offset,
                "buffer cursor overflowed",
            )
        })?;
        self.absolute_offset = self
            .absolute_offset
            .checked_add(count as u64)
            .ok_or_else(|| {
                Mp4Error::new(
                    Mp4ErrorKind::Overflow,
                    "IncrementalBoxReader::consume_raw",
                    self.absolute_offset,
                    "absolute stream offset overflowed",
                )
            })?;
        Ok(())
    }

    fn compact(&mut self) {
        if self.start > 0 {
            self.buffer.drain(..self.start);
            self.start = 0;
        }
    }
}
