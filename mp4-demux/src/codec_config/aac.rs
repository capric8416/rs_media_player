use super::cursor::{BitCursor, Cursor, error};
use crate::{Mp4ErrorKind, Mp4Result};

const SAMPLE_RATES: [u32; 13] = [
    96_000, 88_200, 64_000, 48_000, 44_100, 32_000, 24_000, 22_050, 16_000, 12_000, 11_025, 8_000,
    7_350,
];
const CHANNELS: [u8; 15] = [0, 1, 2, 3, 4, 5, 6, 8, 0, 0, 0, 7, 8, 24, 8];

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AacConfig {
    pub object_type: u8,
    pub extension_object_type: Option<u8>,
    pub sample_rate: u32,
    pub output_sample_rate: u32,
    pub channel_configuration: u8,
    pub channels: u8,
    pub sbr: bool,
    pub ps: bool,
    pub frame_length: u16,
    pub raw: Vec<u8>,
}

impl AacConfig {
    /// Parses MPEG-4 `AudioSpecificConfig` for common AAC and explicit HE-AAC profiles.
    ///
    /// # Errors
    ///
    /// Returns a typed error for truncation, invalid rates/channels, PCE channel layouts, or an
    /// unsupported audio object type.
    pub fn parse_audio_specific_config(data: &[u8]) -> Mp4Result<Self> {
        let mut bits = BitCursor::new(data, "AacConfig::parse_audio_specific_config");
        let mut object_type = read_object_type(&mut bits)?;
        let sample_rate = read_sample_rate(&mut bits)?;
        let channel_configuration = to_u8(bits.bits(4)?, "AAC channel configuration")?;
        let channels = *CHANNELS
            .get(usize::from(channel_configuration))
            .ok_or_else(|| {
                error(
                    Mp4ErrorKind::InvalidData,
                    "AacConfig::parse_audio_specific_config",
                    0,
                    "AAC channel configuration is out of range",
                )
            })?;
        if channel_configuration == 0 {
            return Err(error(
                Mp4ErrorKind::Unsupported,
                "AacConfig::parse_audio_specific_config",
                0,
                "AAC Program Config Element layouts are deferred",
            ));
        }

        let mut extension_object_type = None;
        let mut output_sample_rate = sample_rate;
        let mut sbr = false;
        let mut ps = false;
        if matches!(object_type, 5 | 29) {
            ps = object_type == 29;
            sbr = true;
            extension_object_type = Some(5);
            output_sample_rate = read_sample_rate(&mut bits)?;
            object_type = read_object_type(&mut bits)?;
            if object_type == 22 {
                bits.bits(4)?;
            }
        }

        let frame_length = match object_type {
            1 | 2 | 3 | 4 | 6 | 7 | 17 => {
                let short = bits.bits(1)? != 0;
                let frame_length = if short { 960 } else { 1024 };
                skip_ga_flags(&mut bits)?;
                frame_length
            }
            23 => {
                let short = bits.bits(1)? != 0;
                let frame_length = if short { 480 } else { 512 };
                skip_ga_flags(&mut bits)?;
                frame_length
            }
            _ => {
                return Err(error(
                    Mp4ErrorKind::Unsupported,
                    "AacConfig::parse_audio_specific_config",
                    0,
                    format!("AAC audio object type {object_type} is unsupported"),
                ));
            }
        };
        Ok(Self {
            object_type,
            extension_object_type,
            sample_rate,
            output_sample_rate,
            channel_configuration,
            channels,
            sbr,
            ps,
            frame_length,
            raw: data.to_vec(),
        })
    }

    /// Returns the byte-stable `AudioSpecificConfig` representation retained from the MP4.
    #[must_use]
    pub fn normalize_audio_specific_config(&self) -> Vec<u8> {
        self.raw.clone()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EsdsConfig {
    pub es_id: u16,
    pub object_type_indication: u8,
    pub stream_type: u8,
    pub buffer_size_db: u32,
    pub max_bitrate: u32,
    pub average_bitrate: u32,
    pub decoder_specific: Vec<u8>,
    pub aac: AacConfig,
}

impl EsdsConfig {
    /// Parses a `FullBox` `esds` payload through its AAC `DecoderSpecificInfo` descriptor.
    ///
    /// # Errors
    ///
    /// Returns a typed error for malformed descriptor lengths, missing tags, or non-AAC objects.
    pub fn parse(data: &[u8]) -> Mp4Result<Self> {
        let mut outer = Cursor::new(data, "EsdsConfig::parse");
        let version_flags = outer.u32()?;
        if version_flags != 0 {
            return Err(error(
                Mp4ErrorKind::Unsupported,
                "EsdsConfig::parse",
                0,
                "unsupported esds version or flags",
            ));
        }
        let (tag, es_payload) = read_descriptor(&mut outer)?;
        if tag != 3 {
            return Err(error(
                Mp4ErrorKind::InvalidData,
                "EsdsConfig::parse",
                outer.position(),
                "esds is missing ES_Descriptor tag 3",
            ));
        }
        outer.finish()?;
        let mut es = Cursor::new(es_payload, "EsdsConfig::parse");
        let es_id = es.u16()?;
        let flags = es.u8()?;
        if flags & 0x80 != 0 {
            es.u16()?;
        }
        if flags & 0x40 != 0 {
            let length = usize::from(es.u8()?);
            es.bytes(length)?;
        }
        if flags & 0x20 != 0 {
            es.u16()?;
        }
        let (tag, decoder_payload) = read_descriptor(&mut es)?;
        if tag != 4 {
            return Err(error(
                Mp4ErrorKind::InvalidData,
                "EsdsConfig::parse",
                es.position(),
                "esds is missing DecoderConfigDescriptor tag 4",
            ));
        }
        let mut decoder = Cursor::new(decoder_payload, "EsdsConfig::parse");
        let object_type_indication = decoder.u8()?;
        if !matches!(object_type_indication, 0x40 | 0x66 | 0x67 | 0x68) {
            return Err(error(
                Mp4ErrorKind::Unsupported,
                "EsdsConfig::parse",
                0,
                "esds object type is not AAC",
            ));
        }
        let stream_type = decoder.u8()? >> 2;
        let buffer_size_db = decoder.u24()?;
        let max_bitrate = decoder.u32()?;
        let average_bitrate = decoder.u32()?;
        let (tag, specific) = read_descriptor(&mut decoder)?;
        if tag != 5 || specific.is_empty() {
            return Err(error(
                Mp4ErrorKind::InvalidData,
                "EsdsConfig::parse",
                decoder.position(),
                "esds is missing DecoderSpecificInfo tag 5",
            ));
        }
        decoder.finish()?;
        let decoder_specific = specific.to_vec();
        let aac = AacConfig::parse_audio_specific_config(specific)?;
        Ok(Self {
            es_id,
            object_type_indication,
            stream_type,
            buffer_size_db,
            max_bitrate,
            average_bitrate,
            decoder_specific,
            aac,
        })
    }

    /// Encodes a canonical zero-version `esds` payload with nested tags 3, 4, and 5.
    ///
    /// # Errors
    ///
    /// Returns a typed error when a public field exceeds its descriptor representation.
    pub fn normalize(&self) -> Mp4Result<Vec<u8>> {
        if self.stream_type > 0x3f || self.buffer_size_db > 0x00ff_ffff {
            return Err(error(
                Mp4ErrorKind::InvalidData,
                "EsdsConfig::normalize",
                0,
                "esds stream type or buffer size is out of range",
            ));
        }
        AacConfig::parse_audio_specific_config(&self.decoder_specific)?;
        let mut decoder = vec![self.object_type_indication, (self.stream_type << 2) | 1];
        decoder.extend(&self.buffer_size_db.to_be_bytes()[1..]);
        decoder.extend(self.max_bitrate.to_be_bytes());
        decoder.extend(self.average_bitrate.to_be_bytes());
        push_descriptor(&mut decoder, 5, &self.decoder_specific)?;

        let mut es = Vec::new();
        es.extend(self.es_id.to_be_bytes());
        es.push(0);
        push_descriptor(&mut es, 4, &decoder)?;

        let mut output = vec![0, 0, 0, 0];
        push_descriptor(&mut output, 3, &es)?;
        Ok(output)
    }
}

fn push_descriptor(output: &mut Vec<u8>, tag: u8, payload: &[u8]) -> Mp4Result<()> {
    let length = u32::try_from(payload.len()).map_err(|_| {
        error(
            Mp4ErrorKind::Overflow,
            "EsdsConfig::normalize",
            output.len(),
            "descriptor payload exceeds u32",
        )
    })?;
    if length > 0x0fff_ffff {
        return Err(error(
            Mp4ErrorKind::InvalidData,
            "EsdsConfig::normalize",
            output.len(),
            "descriptor payload exceeds four-byte length encoding",
        ));
    }
    output.push(tag);
    let groups = if length < 1 << 7 {
        1
    } else if length < 1 << 14 {
        2
    } else if length < 1 << 21 {
        3
    } else {
        4
    };
    for index in (0..groups).rev() {
        let shift = index * 7;
        let value = u8::try_from((length >> shift) & 0x7f).map_err(|_| {
            error(
                Mp4ErrorKind::Overflow,
                "EsdsConfig::normalize",
                output.len(),
                "descriptor length byte exceeds u8",
            )
        })?;
        output.push(value | if index == 0 { 0 } else { 0x80 });
    }
    output.extend(payload);
    Ok(())
}

fn read_descriptor<'data>(cursor: &mut Cursor<'data>) -> Mp4Result<(u8, &'data [u8])> {
    let tag = cursor.u8()?;
    let mut length = 0_usize;
    let mut complete = false;
    for _ in 0..4 {
        let byte = cursor.u8()?;
        length = length
            .checked_shl(7)
            .and_then(|value| value.checked_add(usize::from(byte & 0x7f)))
            .ok_or_else(|| {
                error(
                    Mp4ErrorKind::Overflow,
                    "EsdsConfig::parse",
                    cursor.position(),
                    "descriptor length overflowed",
                )
            })?;
        if byte & 0x80 == 0 {
            complete = true;
            break;
        }
    }
    if !complete {
        return Err(error(
            Mp4ErrorKind::InvalidData,
            "EsdsConfig::parse",
            cursor.position(),
            "descriptor length exceeds four bytes",
        ));
    }
    Ok((tag, cursor.bytes(length)?))
}

fn read_object_type(bits: &mut BitCursor<'_>) -> Mp4Result<u8> {
    let value = bits.bits(5)?;
    let value = if value == 31 {
        32 + bits.bits(6)?
    } else {
        value
    };
    to_u8(value, "AAC object type")
}

fn read_sample_rate(bits: &mut BitCursor<'_>) -> Mp4Result<u32> {
    let index = bits.bits(4)?;
    if index == 15 {
        let rate = bits.bits(24)?;
        if rate == 0 {
            return Err(error(
                Mp4ErrorKind::InvalidData,
                "AacConfig::parse_audio_specific_config",
                0,
                "explicit AAC sample rate is zero",
            ));
        }
        return Ok(rate);
    }
    SAMPLE_RATES
        .get(usize::try_from(index).unwrap_or(usize::MAX))
        .copied()
        .ok_or_else(|| {
            error(
                Mp4ErrorKind::InvalidData,
                "AacConfig::parse_audio_specific_config",
                0,
                "AAC sample-rate index is reserved",
            )
        })
}

fn skip_ga_flags(bits: &mut BitCursor<'_>) -> Mp4Result<()> {
    if bits.bits(1)? != 0 {
        bits.bits(14)?;
    }
    bits.bits(1)?;
    Ok(())
}

fn to_u8(value: u32, detail: &'static str) -> Mp4Result<u8> {
    u8::try_from(value).map_err(|_| {
        error(
            Mp4ErrorKind::InvalidData,
            "AacConfig::parse_audio_specific_config",
            0,
            detail,
        )
    })
}
