use std::error::Error;
use std::fmt::{self, Display, Formatter};

/// Stable categories that callers can use for recovery decisions.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum VideoErrorKind {
    InvalidInput,
    Unsupported,
    NotConfigured,
    InvalidState,
    Timeout,
    DeviceLost,
    Backend,
}

pub(crate) const fn backend_kind_from_hresult(code: i32) -> VideoErrorKind {
    match code.cast_unsigned() {
        0x8876_0868 | 0x8876_0869 | 0x887a_0005..=0x887a_0007 => VideoErrorKind::DeviceLost,
        _ => VideoErrorKind::Backend,
    }
}

/// An error from a video contract or backend operation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VideoError {
    kind: VideoErrorKind,
    operation: &'static str,
    detail: String,
    backend_code: Option<i32>,
}

impl VideoError {
    #[must_use]
    pub fn new(kind: VideoErrorKind, operation: &'static str, detail: impl Into<String>) -> Self {
        Self {
            kind,
            operation,
            detail: detail.into(),
            backend_code: None,
        }
    }

    #[must_use]
    pub fn with_backend_code(mut self, backend_code: i32) -> Self {
        self.backend_code = Some(backend_code);
        self
    }

    #[must_use]
    pub const fn kind(&self) -> VideoErrorKind {
        self.kind
    }

    #[must_use]
    pub const fn operation(&self) -> &'static str {
        self.operation
    }

    #[must_use]
    pub fn detail(&self) -> &str {
        &self.detail
    }

    #[must_use]
    pub const fn backend_code(&self) -> Option<i32> {
        self.backend_code
    }
}

impl Display for VideoError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.operation, self.detail)?;
        if let Some(code) = self.backend_code {
            write!(formatter, " (backend code {code:#010x})")?;
        }
        Ok(())
    }
}

impl Error for VideoError {}

pub type VideoResult<T> = Result<T, VideoError>;
