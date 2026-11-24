use super::cursor::{Cursor, error};
use crate::{Mp4ErrorKind, Mp4Result};

#[derive(Clone, Debug, Eq, PartialEq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "fields mirror independent AV1CodecConfigurationRecord flags"
)]
pub struct Av1Config {
    pub sequence_profile: u8,
    pub sequence_level_idx_0: u8,
    pub sequence_tier_0: bool,
    pub high_bitdepth: bool,
    pub monochrome: bool,
    pub chroma_subsampling_x: bool,
    pub chroma_subsampling_y: bool,
    pub chroma_sample_position: u8,
    pub initial_presentation_delay_minus_one: Option<u8>,
    pub config_obus: Vec<u8>,
}

impl Av1Config {
    /// Parses an `AV1CodecConfigurationRecord` (`av1C`) payload for AV1 Main 8/10-bit 4:2:0.
    ///
    /// # Errors
    ///
    /// Returns a typed error for invalid markers, versions, reserved bits, or unsupported profiles.
    pub fn parse(data: &[u8]) -> Mp4Result<Self> {
        let mut cursor = Cursor::new(data, "Av1Config::parse");
        let marker_version = cursor.u8()?;
        if marker_version & 0x80 == 0 || marker_version & 0x7f != 1 {
            return Err(error(
                Mp4ErrorKind::Unsupported,
                "Av1Config::parse",
                0,
                "AV1 configuration requires marker one and version one",
            ));
        }
        let profile_level = cursor.u8()?;
        let sequence_profile = profile_level >> 5;
        if sequence_profile != 0 {
            return Err(error(
                Mp4ErrorKind::Unsupported,
                "Av1Config::parse",
                1,
                "first release supports AV1 Main profile only",
            ));
        }
        let sequence_level_idx_0 = profile_level & 0x1f;
        let flags = cursor.u8()?;
        let sequence_tier_0 = flags & 0x80 != 0;
        let high_bitdepth = flags & 0x40 != 0;
        let twelve_bit = flags & 0x20 != 0;
        let monochrome = flags & 0x10 != 0;
        let chroma_subsampling_x = flags & 8 != 0;
        let chroma_subsampling_y = flags & 4 != 0;
        let chroma_sample_position = flags & 3;
        if twelve_bit || (!monochrome && (!chroma_subsampling_x || !chroma_subsampling_y)) {
            return Err(error(
                Mp4ErrorKind::Unsupported,
                "Av1Config::parse",
                2,
                "first release supports AV1 8/10-bit 4:2:0 only",
            ));
        }
        let delay = cursor.u8()?;
        if delay & 0xe0 != 0 {
            return Err(error(
                Mp4ErrorKind::InvalidData,
                "Av1Config::parse",
                3,
                "invalid AV1 configuration reserved bits",
            ));
        }
        let initial_presentation_delay_minus_one = if delay & 0x10 != 0 {
            Some(delay & 0x0f)
        } else {
            if delay & 0x0f != 0 {
                return Err(error(
                    Mp4ErrorKind::InvalidData,
                    "Av1Config::parse",
                    3,
                    "AV1 delay reserved bits must be zero",
                ));
            }
            None
        };
        let config_obus = cursor.tail().to_vec();
        Ok(Self {
            sequence_profile,
            sequence_level_idx_0,
            sequence_tier_0,
            high_bitdepth,
            monochrome,
            chroma_subsampling_x,
            chroma_subsampling_y,
            chroma_sample_position,
            initial_presentation_delay_minus_one,
            config_obus,
        })
    }

    /// Encodes the canonical four-byte AV1 configuration header plus config OBUs.
    ///
    /// # Errors
    ///
    /// Returns a typed error for unsupported first-release fields.
    pub fn normalize(&self) -> Mp4Result<Vec<u8>> {
        if self
            .initial_presentation_delay_minus_one
            .is_some_and(|delay| delay > 15)
        {
            return Err(error(
                Mp4ErrorKind::InvalidData,
                "Av1Config::normalize",
                3,
                "AV1 presentation delay exceeds four bits",
            ));
        }
        if self.sequence_profile != 0
            || self.sequence_level_idx_0 > 31
            || self.chroma_sample_position > 3
            || (!self.monochrome && (!self.chroma_subsampling_x || !self.chroma_subsampling_y))
        {
            return Err(error(
                Mp4ErrorKind::Unsupported,
                "Av1Config::normalize",
                0,
                "unsupported AV1 configuration fields",
            ));
        }
        let mut output = vec![
            0x81,
            (self.sequence_profile << 5) | self.sequence_level_idx_0,
            (u8::from(self.sequence_tier_0) << 7)
                | (u8::from(self.high_bitdepth) << 6)
                | (u8::from(self.monochrome) << 4)
                | (u8::from(self.chroma_subsampling_x) << 3)
                | (u8::from(self.chroma_subsampling_y) << 2)
                | self.chroma_sample_position,
            self.initial_presentation_delay_minus_one
                .map_or(0, |delay| 0x10 | delay),
        ];
        output.extend(&self.config_obus);
        Ok(output)
    }
}
