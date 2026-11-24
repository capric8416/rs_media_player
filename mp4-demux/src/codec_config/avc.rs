use super::cursor::{Cursor, error};
use crate::{Mp4ErrorKind, Mp4Result};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AvcConfig {
    pub profile: u8,
    pub profile_compatibility: u8,
    pub level: u8,
    pub length_size: u8,
    pub sequence_parameter_sets: Vec<Vec<u8>>,
    pub picture_parameter_sets: Vec<Vec<u8>>,
    pub chroma_format: Option<u8>,
    pub bit_depth_luma: Option<u8>,
    pub bit_depth_chroma: Option<u8>,
    pub sequence_parameter_set_extensions: Vec<Vec<u8>>,
}

impl AvcConfig {
    /// Parses an `AVCDecoderConfigurationRecord` (`avcC`) payload.
    ///
    /// # Errors
    ///
    /// Returns a typed error for invalid versions, reserved bits, lengths, or truncation.
    pub fn parse(data: &[u8]) -> Mp4Result<Self> {
        let mut cursor = Cursor::new(data, "AvcConfig::parse");
        if cursor.u8()? != 1 {
            return Err(error(
                Mp4ErrorKind::Unsupported,
                "AvcConfig::parse",
                0,
                "unsupported AVC configuration version",
            ));
        }
        let profile = cursor.u8()?;
        let profile_compatibility = cursor.u8()?;
        let level = cursor.u8()?;
        let length_field = cursor.u8()?;
        if length_field & 0xfc != 0xfc {
            return Err(error(
                Mp4ErrorKind::InvalidData,
                "AvcConfig::parse",
                4,
                "invalid AVC length-size reserved bits",
            ));
        }
        let length_size = (length_field & 3) + 1;
        let count_field = cursor.u8()?;
        if count_field & 0xe0 != 0xe0 {
            return Err(error(
                Mp4ErrorKind::InvalidData,
                "AvcConfig::parse",
                5,
                "invalid AVC SPS-count reserved bits",
            ));
        }
        let sequence_parameter_sets = read_nalus(&mut cursor, usize::from(count_field & 0x1f))?;
        let picture_count = usize::from(cursor.u8()?);
        let picture_parameter_sets = read_nalus(&mut cursor, picture_count)?;

        let mut chroma_format = None;
        let mut bit_depth_luma = None;
        let mut bit_depth_chroma = None;
        let mut extensions = Vec::new();
        if cursor.remaining() != 0 {
            if !matches!(profile, 100 | 110 | 122 | 144) {
                return Err(error(
                    Mp4ErrorKind::InvalidData,
                    "AvcConfig::parse",
                    cursor.position(),
                    "unexpected AVC profile extension",
                ));
            }
            let chroma = cursor.u8()?;
            let luma = cursor.u8()?;
            let chroma_depth = cursor.u8()?;
            if chroma & 0xfc != 0xfc || luma & 0xf8 != 0xf8 || chroma_depth & 0xf8 != 0xf8 {
                return Err(error(
                    Mp4ErrorKind::InvalidData,
                    "AvcConfig::parse",
                    cursor.position().saturating_sub(3),
                    "invalid AVC extension reserved bits",
                ));
            }
            chroma_format = Some(chroma & 3);
            bit_depth_luma = Some((luma & 7) + 8);
            bit_depth_chroma = Some((chroma_depth & 7) + 8);
            let count = usize::from(cursor.u8()?);
            extensions = read_nalus(&mut cursor, count)?;
        }
        cursor.finish()?;
        Ok(Self {
            profile,
            profile_compatibility,
            level,
            length_size,
            sequence_parameter_sets,
            picture_parameter_sets,
            chroma_format,
            bit_depth_luma,
            bit_depth_chroma,
            sequence_parameter_set_extensions: extensions,
        })
    }

    /// Encodes a canonical `AVCDecoderConfigurationRecord` payload.
    ///
    /// # Errors
    ///
    /// Returns a typed error when counts or NAL lengths cannot be represented.
    pub fn normalize(&self) -> Mp4Result<Vec<u8>> {
        if !(1..=4).contains(&self.length_size) || self.sequence_parameter_sets.len() > 31 {
            return Err(error(
                Mp4ErrorKind::InvalidData,
                "AvcConfig::normalize",
                0,
                "invalid AVC length size or SPS count",
            ));
        }
        if (self.chroma_format.is_some() && !matches!(self.profile, 100 | 110 | 122 | 144))
            || (self.chroma_format.is_none()
                && (self.bit_depth_luma.is_some()
                    || self.bit_depth_chroma.is_some()
                    || !self.sequence_parameter_set_extensions.is_empty()))
        {
            return Err(error(
                Mp4ErrorKind::InvalidData,
                "AvcConfig::normalize",
                0,
                "inconsistent AVC profile-extension fields",
            ));
        }
        let mut output = vec![
            1,
            self.profile,
            self.profile_compatibility,
            self.level,
            0xfc | (self.length_size - 1),
            0xe0 | to_u8(self.sequence_parameter_sets.len(), "AVC SPS count")?,
        ];
        write_nalus(&mut output, &self.sequence_parameter_sets)?;
        output.push(to_u8(self.picture_parameter_sets.len(), "AVC PPS count")?);
        write_nalus(&mut output, &self.picture_parameter_sets)?;
        if let Some(chroma) = self.chroma_format {
            let luma = self.bit_depth_luma.ok_or_else(|| {
                error(
                    Mp4ErrorKind::InvalidData,
                    "AvcConfig::normalize",
                    output.len(),
                    "AVC luma bit depth is missing",
                )
            })?;
            let chroma_depth = self.bit_depth_chroma.ok_or_else(|| {
                error(
                    Mp4ErrorKind::InvalidData,
                    "AvcConfig::normalize",
                    output.len(),
                    "AVC chroma bit depth is missing",
                )
            })?;
            if chroma > 3 || !(8..=15).contains(&luma) || !(8..=15).contains(&chroma_depth) {
                return Err(error(
                    Mp4ErrorKind::InvalidData,
                    "AvcConfig::normalize",
                    output.len(),
                    "invalid AVC extension values",
                ));
            }
            output.extend([0xfc | chroma, 0xf8 | (luma - 8), 0xf8 | (chroma_depth - 8)]);
            output.push(to_u8(
                self.sequence_parameter_set_extensions.len(),
                "AVC SPS extension count",
            )?);
            write_nalus(&mut output, &self.sequence_parameter_set_extensions)?;
        }
        Ok(output)
    }
}

fn read_nalus(cursor: &mut Cursor<'_>, count: usize) -> Mp4Result<Vec<Vec<u8>>> {
    let mut units = Vec::with_capacity(count);
    for _ in 0..count {
        let length = usize::from(cursor.u16()?);
        if length == 0 {
            return Err(error(
                Mp4ErrorKind::InvalidData,
                "AvcConfig::parse",
                cursor.position(),
                "zero-length AVC parameter set",
            ));
        }
        units.push(cursor.bytes(length)?.to_vec());
    }
    Ok(units)
}

fn write_nalus(output: &mut Vec<u8>, units: &[Vec<u8>]) -> Mp4Result<()> {
    for unit in units {
        let length = u16::try_from(unit.len()).map_err(|_| {
            error(
                Mp4ErrorKind::InvalidData,
                "AvcConfig::normalize",
                output.len(),
                "AVC parameter set exceeds 65535 bytes",
            )
        })?;
        output.extend(length.to_be_bytes());
        output.extend(unit);
    }
    Ok(())
}

fn to_u8(value: usize, detail: &'static str) -> Mp4Result<u8> {
    u8::try_from(value)
        .map_err(|_| error(Mp4ErrorKind::InvalidData, "AvcConfig::normalize", 0, detail))
}
