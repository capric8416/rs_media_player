use super::cursor::{Cursor, error};
use crate::{Mp4ErrorKind, Mp4Result};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OpusConfig {
    pub output_channel_count: u8,
    pub pre_skip: u16,
    pub input_sample_rate: u32,
    pub output_gain: i16,
    pub channel_mapping_family: u8,
    pub mapping_data: Vec<u8>,
}

impl OpusConfig {
    pub const SEEK_PREROLL_SAMPLES: u32 = 3_840;

    /// Parses a version-zero `OpusSpecificBox` (`dOps`) payload.
    ///
    /// # Errors
    ///
    /// Returns a typed error for unsupported versions, invalid channel mappings, or truncation.
    pub fn parse_dops(data: &[u8]) -> Mp4Result<Self> {
        let mut cursor = Cursor::new(data, "OpusConfig::parse_dops");
        if cursor.u8()? != 0 {
            return Err(error(
                Mp4ErrorKind::Unsupported,
                "OpusConfig::parse_dops",
                0,
                "unsupported OpusSpecificBox version",
            ));
        }
        let output_channel_count = cursor.u8()?;
        if output_channel_count == 0 {
            return Err(error(
                Mp4ErrorKind::InvalidData,
                "OpusConfig::parse_dops",
                1,
                "Opus output channel count is zero",
            ));
        }
        let pre_skip = cursor.u16()?;
        let input_sample_rate = cursor.u32()?;
        let output_gain = cursor.i16()?;
        let channel_mapping_family = cursor.u8()?;
        let mapping_data = cursor.tail().to_vec();
        if channel_mapping_family == 0 {
            if output_channel_count > 2 || !mapping_data.is_empty() {
                return Err(error(
                    Mp4ErrorKind::InvalidData,
                    "OpusConfig::parse_dops",
                    10,
                    "Opus mapping family zero requires mono/stereo without a mapping table",
                ));
            }
        } else {
            let minimum = 2_usize + usize::from(output_channel_count);
            if mapping_data.len() < minimum {
                return Err(error(
                    Mp4ErrorKind::Truncated,
                    "OpusConfig::parse_dops",
                    11,
                    "Opus channel mapping table is truncated",
                ));
            }
            let stream_count = mapping_data[0];
            let coupled_count = mapping_data[1];
            if stream_count == 0 || coupled_count > stream_count {
                return Err(error(
                    Mp4ErrorKind::InvalidData,
                    "OpusConfig::parse_dops",
                    11,
                    "Opus stream and coupled-stream counts are invalid",
                ));
            }
        }
        Ok(Self {
            output_channel_count,
            pre_skip,
            input_sample_rate,
            output_gain,
            channel_mapping_family,
            mapping_data,
        })
    }

    /// Encodes a canonical version-zero `dOps` payload.
    ///
    /// # Errors
    ///
    /// Returns a typed error if public fields no longer form a valid Opus configuration.
    pub fn normalize_dops(&self) -> Mp4Result<Vec<u8>> {
        let mut output = vec![0, self.output_channel_count];
        output.extend(self.pre_skip.to_be_bytes());
        output.extend(self.input_sample_rate.to_be_bytes());
        output.extend(self.output_gain.to_be_bytes());
        output.push(self.channel_mapping_family);
        output.extend(&self.mapping_data);
        Self::parse_dops(&output)?;
        Ok(output)
    }

    /// Converts the MP4 configuration to the Ogg-compatible `OpusHead` representation used by
    /// `FFmpeg` and common Opus decoders.
    #[must_use]
    pub fn opus_head(&self) -> Vec<u8> {
        let mut output = b"OpusHead".to_vec();
        output.push(1);
        output.push(self.output_channel_count);
        output.extend(self.pre_skip.to_le_bytes());
        output.extend(self.input_sample_rate.to_le_bytes());
        output.extend(self.output_gain.to_le_bytes());
        output.push(self.channel_mapping_family);
        output.extend(&self.mapping_data);
        output
    }
}
