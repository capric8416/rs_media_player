mod dpb;
mod dxva;
pub(crate) mod parameter_sets;
mod slice;
mod state;

pub(crate) use dxva::{idr_picture_parameters, idr_quant_matrix};
pub use state::AvcDecodeState;
pub(crate) use state::PreparedAvcPicture;

#[cfg(test)]
mod tests;

use crate::{VideoError, VideoErrorKind};

pub(crate) const MAX_DPB_FRAMES: usize = 16;
pub(crate) const MAX_REFS: usize = 32;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PictureStructure {
    TopField,
    BottomField,
    Frame,
}

impl PictureStructure {
    pub(crate) const fn mask(self) -> u8 {
        match self {
            Self::TopField => 1,
            Self::BottomField => 2,
            Self::Frame => 3,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct NalHeader {
    pub(crate) reference_idc: u8,
    pub(crate) unit_type: u8,
}

impl NalHeader {
    pub(crate) fn parse(byte: u8) -> Result<Self, VideoError> {
        if byte & 0x80 != 0 {
            return Err(avc_error("NalHeader::parse", "forbidden_zero_bit is set"));
        }
        Ok(Self {
            reference_idc: (byte >> 5) & 0x03,
            unit_type: byte & 0x1f,
        })
    }

    pub(crate) const fn is_idr(self) -> bool {
        self.unit_type == 5
    }
}

pub(super) fn avc_error(operation: &'static str, detail: impl Into<String>) -> VideoError {
    VideoError::new(VideoErrorKind::InvalidInput, operation, detail)
}

pub(super) fn unsupported(operation: &'static str, detail: impl Into<String>) -> VideoError {
    VideoError::new(VideoErrorKind::Unsupported, operation, detail)
}
