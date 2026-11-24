use super::{MAX_REFS, hevc_error};
use crate::VideoResult;
use crate::bit_reader::BitReader;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ShortTermReference {
    pub(crate) delta_poc: i32,
    pub(crate) used: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct ShortTermRps {
    pub(crate) references: Vec<ShortTermReference>,
    pub(crate) negative_count: u8,
}

impl ShortTermRps {
    pub(crate) fn decode(
        bits: &mut BitReader<'_>,
        previous: &[Self],
        is_slice_header: bool,
    ) -> VideoResult<Self> {
        let predicted = !previous.is_empty() && bits.read_bit()?;
        if predicted {
            Self::decode_predicted(bits, previous, is_slice_header)
        } else {
            Self::decode_explicit(bits)
        }
    }

    fn decode_explicit(bits: &mut BitReader<'_>) -> VideoResult<Self> {
        let negative = bounded_count(bits.read_ue()?, "negative picture count")?;
        let positive = bounded_count(bits.read_ue()?, "positive picture count")?;
        if usize::from(negative) + usize::from(positive) > MAX_REFS {
            return Err(hevc_error(
                "ShortTermRps::decode",
                "RPS exceeds 16 references",
            ));
        }
        let mut references = Vec::with_capacity(usize::from(negative + positive));
        let mut delta = 0_i32;
        for _ in 0..negative {
            let difference = read_delta(bits)?;
            delta = delta
                .checked_sub(difference)
                .ok_or_else(|| hevc_error("ShortTermRps::decode", "negative POC overflowed"))?;
            references.push(ShortTermReference {
                delta_poc: delta,
                used: bits.read_bit()?,
            });
        }
        delta = 0;
        for _ in 0..positive {
            let difference = read_delta(bits)?;
            delta = delta
                .checked_add(difference)
                .ok_or_else(|| hevc_error("ShortTermRps::decode", "positive POC overflowed"))?;
            references.push(ShortTermReference {
                delta_poc: delta,
                used: bits.read_bit()?,
            });
        }
        Ok(Self {
            references,
            negative_count: negative,
        })
    }

    fn decode_predicted(
        bits: &mut BitReader<'_>,
        previous: &[Self],
        is_slice_header: bool,
    ) -> VideoResult<Self> {
        let delta_index = if is_slice_header {
            bits.read_ue()?
                .checked_add(1)
                .ok_or_else(|| hevc_error("ShortTermRps::decode", "delta index overflowed"))?
        } else {
            1
        };
        let delta_index = usize::try_from(delta_index)
            .map_err(|_| hevc_error("ShortTermRps::decode", "delta index does not fit usize"))?;
        let reference = previous
            .len()
            .checked_sub(delta_index)
            .and_then(|index| previous.get(index))
            .ok_or_else(|| hevc_error("ShortTermRps::decode", "predicted RPS index is invalid"))?;
        let negative_delta = bits.read_bit()?;
        let magnitude = read_delta(bits)?;
        let delta_rps = if negative_delta {
            -magnitude
        } else {
            magnitude
        };
        let mut references = Vec::new();
        for index in 0..=reference.references.len() {
            let used = bits.read_bit()?;
            let use_delta = if used { false } else { bits.read_bit()? };
            if used || use_delta {
                let base = reference
                    .references
                    .get(index)
                    .map_or(0, |entry| entry.delta_poc);
                let delta_poc = delta_rps.checked_add(base).ok_or_else(|| {
                    hevc_error("ShortTermRps::decode", "predicted POC overflowed")
                })?;
                references.push(ShortTermReference { delta_poc, used });
            }
        }
        if references.len() > MAX_REFS {
            return Err(hevc_error(
                "ShortTermRps::decode",
                "predicted RPS exceeds 16 references",
            ));
        }
        references.sort_by_key(|entry| entry.delta_poc);
        let negative_count = references
            .iter()
            .take_while(|entry| entry.delta_poc < 0)
            .count();
        references[..negative_count].reverse();
        Ok(Self {
            references,
            negative_count: u8::try_from(negative_count)
                .map_err(|_| hevc_error("ShortTermRps::decode", "negative count exceeds u8"))?,
        })
    }
}

fn read_delta(bits: &mut BitReader<'_>) -> VideoResult<i32> {
    let value = bits
        .read_ue()?
        .checked_add(1)
        .filter(|value| *value <= 32_768)
        .ok_or_else(|| hevc_error("ShortTermRps::decode", "delta POC is out of range"))?;
    i32::try_from(value)
        .map_err(|_| hevc_error("ShortTermRps::decode", "delta POC does not fit i32"))
}

fn bounded_count(value: u32, field: &'static str) -> VideoResult<u8> {
    if value >= 16 {
        return Err(hevc_error(
            "ShortTermRps::decode",
            format!("{field} exceeds 15"),
        ));
    }
    u8::try_from(value)
        .map_err(|_| hevc_error("ShortTermRps::decode", format!("{field} does not fit u8")))
}
