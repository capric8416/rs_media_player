use super::cursor::{Cursor, error};
use crate::{Mp4ErrorKind, Mp4Result};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HevcNalArray {
    pub array_completeness: bool,
    pub nal_unit_type: u8,
    pub nal_units: Vec<Vec<u8>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HevcConfig {
    pub profile_space: u8,
    pub tier_flag: bool,
    pub profile_idc: u8,
    pub profile_compatibility_flags: u32,
    pub constraint_indicator_flags: u64,
    pub level_idc: u8,
    pub min_spatial_segmentation_idc: u16,
    pub parallelism_type: u8,
    pub chroma_format: u8,
    pub bit_depth_luma: u8,
    pub bit_depth_chroma: u8,
    pub average_frame_rate: u16,
    pub constant_frame_rate: u8,
    pub num_temporal_layers: u8,
    pub temporal_id_nested: bool,
    pub length_size: u8,
    pub arrays: Vec<HevcNalArray>,
}

impl HevcConfig {
    /// Parses an `HEVCDecoderConfigurationRecord` (`hvcC`) payload.
    ///
    /// # Errors
    ///
    /// Returns a typed error for invalid versions, reserved bits, lengths, or truncation.
    pub fn parse(data: &[u8]) -> Mp4Result<Self> {
        let mut cursor = Cursor::new(data, "HevcConfig::parse");
        if cursor.u8()? != 1 {
            return Err(error(
                Mp4ErrorKind::Unsupported,
                "HevcConfig::parse",
                0,
                "unsupported HEVC configuration version",
            ));
        }
        let profile = cursor.u8()?;
        let profile_space = profile >> 6;
        let tier_flag = profile & 0x20 != 0;
        let profile_idc = profile & 0x1f;
        let profile_compatibility_flags = cursor.u32()?;
        let constraint_indicator_flags = cursor.u48()?;
        let level_idc = cursor.u8()?;
        let spatial = cursor.u16()?;
        let parallelism = cursor.u8()?;
        let chroma = cursor.u8()?;
        let luma = cursor.u8()?;
        let chroma_depth = cursor.u8()?;
        if spatial & 0xf000 != 0xf000
            || parallelism & 0xfc != 0xfc
            || chroma & 0xfc != 0xfc
            || luma & 0xf8 != 0xf8
            || chroma_depth & 0xf8 != 0xf8
        {
            return Err(error(
                Mp4ErrorKind::InvalidData,
                "HevcConfig::parse",
                13,
                "invalid HEVC reserved bits",
            ));
        }
        let average_frame_rate = cursor.u16()?;
        let packed = cursor.u8()?;
        let array_count = usize::from(cursor.u8()?);
        let mut arrays = Vec::with_capacity(array_count);
        for _ in 0..array_count {
            let header = cursor.u8()?;
            if header & 0x40 != 0 {
                return Err(error(
                    Mp4ErrorKind::InvalidData,
                    "HevcConfig::parse",
                    cursor.position().saturating_sub(1),
                    "invalid HEVC array reserved bit",
                ));
            }
            let count = usize::from(cursor.u16()?);
            let mut nal_units = Vec::with_capacity(count);
            for _ in 0..count {
                let length = usize::from(cursor.u16()?);
                if length == 0 {
                    return Err(error(
                        Mp4ErrorKind::InvalidData,
                        "HevcConfig::parse",
                        cursor.position(),
                        "zero-length HEVC parameter set",
                    ));
                }
                nal_units.push(cursor.bytes(length)?.to_vec());
            }
            arrays.push(HevcNalArray {
                array_completeness: header & 0x80 != 0,
                nal_unit_type: header & 0x3f,
                nal_units,
            });
        }
        cursor.finish()?;
        Ok(Self {
            profile_space,
            tier_flag,
            profile_idc,
            profile_compatibility_flags,
            constraint_indicator_flags,
            level_idc,
            min_spatial_segmentation_idc: spatial & 0x0fff,
            parallelism_type: parallelism & 3,
            chroma_format: chroma & 3,
            bit_depth_luma: (luma & 7) + 8,
            bit_depth_chroma: (chroma_depth & 7) + 8,
            average_frame_rate,
            constant_frame_rate: packed >> 6,
            num_temporal_layers: (packed >> 3) & 7,
            temporal_id_nested: packed & 4 != 0,
            length_size: (packed & 3) + 1,
            arrays,
        })
    }

    /// Encodes a canonical `HEVCDecoderConfigurationRecord` payload.
    ///
    /// # Errors
    ///
    /// Returns a typed error when fields, counts, or NAL lengths cannot be represented.
    pub fn normalize(&self) -> Mp4Result<Vec<u8>> {
        if self.profile_space > 3
            || self.profile_idc > 31
            || self.constraint_indicator_flags >= (1_u64 << 48)
            || self.min_spatial_segmentation_idc > 0x0fff
            || self.parallelism_type > 3
            || self.chroma_format > 3
            || !(8..=15).contains(&self.bit_depth_luma)
            || !(8..=15).contains(&self.bit_depth_chroma)
            || self.constant_frame_rate > 3
            || self.num_temporal_layers > 7
            || !(1..=4).contains(&self.length_size)
        {
            return Err(error(
                Mp4ErrorKind::InvalidData,
                "HevcConfig::normalize",
                0,
                "HEVC configuration field is out of range",
            ));
        }
        let mut output = Vec::new();
        output.push(1);
        output.push((self.profile_space << 6) | (u8::from(self.tier_flag) << 5) | self.profile_idc);
        output.extend(self.profile_compatibility_flags.to_be_bytes());
        output.extend(&self.constraint_indicator_flags.to_be_bytes()[2..]);
        output.push(self.level_idc);
        output.extend((0xf000 | self.min_spatial_segmentation_idc).to_be_bytes());
        output.push(0xfc | self.parallelism_type);
        output.push(0xfc | self.chroma_format);
        output.push(0xf8 | (self.bit_depth_luma - 8));
        output.push(0xf8 | (self.bit_depth_chroma - 8));
        output.extend(self.average_frame_rate.to_be_bytes());
        output.push(
            (self.constant_frame_rate << 6)
                | (self.num_temporal_layers << 3)
                | (u8::from(self.temporal_id_nested) << 2)
                | (self.length_size - 1),
        );
        output.push(to_u8(self.arrays.len(), "HEVC array count")?);
        for array in &self.arrays {
            if array.nal_unit_type > 63 {
                return Err(error(
                    Mp4ErrorKind::InvalidData,
                    "HevcConfig::normalize",
                    output.len(),
                    "HEVC NAL unit type exceeds six bits",
                ));
            }
            output.push((u8::from(array.array_completeness) << 7) | array.nal_unit_type);
            let count = u16::try_from(array.nal_units.len()).map_err(|_| {
                error(
                    Mp4ErrorKind::InvalidData,
                    "HevcConfig::normalize",
                    output.len(),
                    "too many HEVC NAL units",
                )
            })?;
            output.extend(count.to_be_bytes());
            for unit in &array.nal_units {
                if unit.is_empty() {
                    return Err(error(
                        Mp4ErrorKind::InvalidData,
                        "HevcConfig::normalize",
                        output.len(),
                        "zero-length HEVC parameter set",
                    ));
                }
                let length = u16::try_from(unit.len()).map_err(|_| {
                    error(
                        Mp4ErrorKind::InvalidData,
                        "HevcConfig::normalize",
                        output.len(),
                        "HEVC NAL unit exceeds 65535 bytes",
                    )
                })?;
                output.extend(length.to_be_bytes());
                output.extend(unit);
            }
        }
        Ok(output)
    }
}

fn to_u8(value: usize, detail: &'static str) -> Mp4Result<u8> {
    u8::try_from(value).map_err(|_| {
        error(
            Mp4ErrorKind::InvalidData,
            "HevcConfig::normalize",
            0,
            detail,
        )
    })
}
