use std::error::Error;
use std::fmt::{self, Display, Formatter};

use crate::FourCc;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum Mp4ErrorKind {
    Io,
    Truncated,
    InvalidSize,
    Overflow,
    Bounds,
    InvalidState,
    BufferLimit,
    InvalidData,
    Unsupported,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Mp4Error {
    kind: Mp4ErrorKind,
    operation: &'static str,
    offset: u64,
    box_type: Option<FourCc>,
    detail: String,
}

impl Mp4Error {
    #[must_use]
    pub fn new(
        kind: Mp4ErrorKind,
        operation: &'static str,
        offset: u64,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            kind,
            operation,
            offset,
            box_type: None,
            detail: detail.into(),
        }
    }

    #[must_use]
    pub const fn with_box_type(mut self, box_type: FourCc) -> Self {
        self.box_type = Some(box_type);
        self
    }

    #[must_use]
    pub const fn kind(&self) -> Mp4ErrorKind {
        self.kind
    }

    #[must_use]
    pub const fn operation(&self) -> &'static str {
        self.operation
    }

    #[must_use]
    pub const fn offset(&self) -> u64 {
        self.offset
    }

    #[must_use]
    pub const fn box_type(&self) -> Option<FourCc> {
        self.box_type
    }

    #[must_use]
    pub fn detail(&self) -> &str {
        &self.detail
    }
}

impl Display for Mp4Error {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} at byte {}: {}",
            self.operation, self.offset, self.detail
        )?;
        if let Some(box_type) = self.box_type {
            write!(formatter, " (box {box_type})")?;
        }
        Ok(())
    }
}

impl Error for Mp4Error {}

pub type Mp4Result<T> = Result<T, Mp4Error>;
