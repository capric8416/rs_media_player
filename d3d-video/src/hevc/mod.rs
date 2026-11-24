mod dpb;
mod dxva;
pub(crate) mod parameter_sets;
mod rps;
mod slice;
mod state;

pub(crate) use dxva::{idr_picture_parameters, idr_quant_matrix};
pub use state::HevcDecodeState;
pub(crate) use state::PreparedHevcPicture;

#[cfg(test)]
mod tests;

use crate::{VideoError, VideoErrorKind};

pub(super) const MAX_REFS: usize = 16;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct NalHeader {
    pub(crate) unit_type: u8,
    pub(crate) layer_id: u8,
    pub(crate) temporal_id: u8,
}

impl NalHeader {
    pub(crate) fn parse(bytes: [u8; 2]) -> Result<Self, VideoError> {
        if bytes[0] & 0x80 != 0 {
            return Err(hevc_error("NalHeader::parse", "forbidden_zero_bit is set"));
        }
        let temporal_id_plus_one = bytes[1] & 0x07;
        if temporal_id_plus_one == 0 {
            return Err(hevc_error("NalHeader::parse", "temporal_id_plus1 is zero"));
        }
        Ok(Self {
            unit_type: (bytes[0] >> 1) & 0x3f,
            layer_id: ((bytes[0] & 1) << 5) | (bytes[1] >> 3),
            temporal_id: temporal_id_plus_one - 1,
        })
    }

    pub(crate) const fn is_idr(self) -> bool {
        matches!(self.unit_type, 19 | 20)
    }

    pub(crate) const fn is_bla(self) -> bool {
        matches!(self.unit_type, 16..=18)
    }

    pub(crate) const fn is_irap(self) -> bool {
        matches!(self.unit_type, 16..=23)
    }
}

pub(super) fn hevc_error(operation: &'static str, detail: impl Into<String>) -> VideoError {
    VideoError::new(VideoErrorKind::InvalidInput, operation, detail)
}

pub(super) fn unsupported(operation: &'static str, detail: impl Into<String>) -> VideoError {
    VideoError::new(VideoErrorKind::Unsupported, operation, detail)
}
