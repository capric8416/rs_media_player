use std::fmt::{self, Debug, Display, Formatter};

use crate::{Mp4Error, Mp4ErrorKind, Mp4Result};

const BASE_HEADER_SIZE: usize = 8;
const EXTENDED_HEADER_SIZE: usize = 16;

#[derive(Clone, Copy, Eq, Hash, PartialEq)]
pub struct FourCc([u8; 4]);

impl FourCc {
    #[must_use]
    pub const fn new(bytes: [u8; 4]) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub const fn as_bytes(self) -> [u8; 4] {
        self.0
    }
}

impl Display for FourCc {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            if byte.is_ascii_graphic() || byte == b' ' {
                write!(formatter, "{}", char::from(byte))?;
            } else {
                write!(formatter, "\\x{byte:02X}")?;
            }
        }
        Ok(())
    }
}

impl Debug for FourCc {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        write!(formatter, "FourCc(\"{self}\")")
    }
}

/// Parsed generic ISO BMFF box header.
///
/// `declared_size` is `None` for a size-zero box. `effective_size` is bounded by the parent end,
/// matching the pinned `FFmpeg` MOV parser's child-size clamping.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BoxHeader {
    box_type: FourCc,
    offset: u64,
    header_size: u64,
    declared_size: Option<u64>,
    effective_size: u64,
    was_clamped: bool,
}

impl BoxHeader {
    #[must_use]
    pub const fn box_type(self) -> FourCc {
        self.box_type
    }

    #[must_use]
    pub const fn offset(self) -> u64 {
        self.offset
    }

    #[must_use]
    pub const fn header_size(self) -> u64 {
        self.header_size
    }

    #[must_use]
    pub const fn declared_size(self) -> Option<u64> {
        self.declared_size
    }

    #[must_use]
    pub const fn effective_size(self) -> u64 {
        self.effective_size
    }

    #[must_use]
    pub const fn payload_size(self) -> u64 {
        self.effective_size - self.header_size
    }

    #[must_use]
    pub const fn payload_offset(self) -> u64 {
        self.offset + self.header_size
    }

    #[must_use]
    pub const fn end(self) -> u64 {
        self.offset + self.effective_size
    }

    #[must_use]
    pub const fn extends_to_parent_end(self) -> bool {
        self.declared_size.is_none()
    }

    #[must_use]
    pub const fn was_clamped(self) -> bool {
        self.was_clamped
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HeaderParse {
    NeedMoreData { required: usize },
    NeedsContainerEnd,
    Complete(BoxHeader),
}

/// Parses one generic MP4 box header without reading its payload.
///
/// `container_end` is the absolute exclusive end of the parent. Declared sizes that cross it are
/// safely clamped to preserve the pinned `FFmpeg` parser's behavior.
///
/// # Errors
///
/// Returns a typed error for invalid sizes, impossible parent bounds, or checked-arithmetic
/// overflow.
pub fn parse_box_header(
    input: &[u8],
    offset: u64,
    container_end: Option<u64>,
) -> Mp4Result<HeaderParse> {
    if input.len() < BASE_HEADER_SIZE {
        return Ok(HeaderParse::NeedMoreData {
            required: BASE_HEADER_SIZE,
        });
    }

    let size32 = u32::from_be_bytes([input[0], input[1], input[2], input[3]]);
    let box_type = FourCc::new([input[4], input[5], input[6], input[7]]);
    let required = if size32 == 1 {
        EXTENDED_HEADER_SIZE
    } else {
        BASE_HEADER_SIZE
    };
    if input.len() < required {
        return Ok(HeaderParse::NeedMoreData { required });
    }

    let header_size = u64::try_from(required).map_err(|_| {
        Mp4Error::new(
            Mp4ErrorKind::Overflow,
            "parse_box_header",
            offset,
            "header size cannot be represented",
        )
        .with_box_type(box_type)
    })?;
    let parent_remaining = if let Some(end) = container_end {
        let remaining = end.checked_sub(offset).ok_or_else(|| {
            Mp4Error::new(
                Mp4ErrorKind::Bounds,
                "parse_box_header",
                offset,
                "box begins after its parent end",
            )
            .with_box_type(box_type)
        })?;
        if remaining < header_size {
            return Err(Mp4Error::new(
                Mp4ErrorKind::Truncated,
                "parse_box_header",
                offset,
                "box header crosses its parent end",
            )
            .with_box_type(box_type));
        }
        Some(remaining)
    } else {
        None
    };

    let declared_size = match size32 {
        0 => None,
        1 => Some(u64::from_be_bytes([
            input[8], input[9], input[10], input[11], input[12], input[13], input[14], input[15],
        ])),
        value => Some(u64::from(value)),
    };

    let raw_size = match (declared_size, parent_remaining) {
        (Some(size), _) => size,
        (None, Some(remaining)) => remaining,
        (None, None) => return Ok(HeaderParse::NeedsContainerEnd),
    };
    if raw_size < header_size {
        return Err(Mp4Error::new(
            Mp4ErrorKind::InvalidSize,
            "parse_box_header",
            offset,
            format!("declared size {raw_size} is smaller than header size {header_size}"),
        )
        .with_box_type(box_type));
    }

    offset.checked_add(raw_size).ok_or_else(|| {
        Mp4Error::new(
            Mp4ErrorKind::Overflow,
            "parse_box_header",
            offset,
            "box end overflows a 64-bit offset",
        )
        .with_box_type(box_type)
    })?;

    let effective_size = parent_remaining.map_or(raw_size, |remaining| raw_size.min(remaining));
    let was_clamped = effective_size != raw_size;
    Ok(HeaderParse::Complete(BoxHeader {
        box_type,
        offset,
        header_size,
        declared_size,
        effective_size,
        was_clamped,
    }))
}
