use std::collections::BTreeSet;

use super::reader::Reader;
use crate::{Mp4Error, Mp4ErrorKind, Mp4Result};

const MAX_SAMPLES: usize = 16_777_216;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SampleSizeBox<'data> {
    Stsz(&'data [u8]),
    Stz2(&'data [u8]),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChunkOffsetBox<'data> {
    Stco(&'data [u8]),
    Co64(&'data [u8]),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ClassicTableBoxes<'data> {
    pub stts: &'data [u8],
    pub ctts: Option<&'data [u8]>,
    pub stsc: &'data [u8],
    pub sample_sizes: SampleSizeBox<'data>,
    pub chunk_offsets: ChunkOffsetBox<'data>,
    pub stss: Option<&'data [u8]>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Sample {
    pub offset: u64,
    pub size: u32,
    pub dts: i64,
    pub pts: i64,
    pub duration: u32,
    pub sample_description_index: u32,
    pub is_sync: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SeekMode {
    PreviousSync,
    NextSync,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SampleIndex {
    pub track_id: u32,
    pub timescale: u32,
    samples: Vec<Sample>,
    has_composition_offsets: bool,
}

impl SampleIndex {
    /// Builds a checked classic MP4 sample index from `stbl` child payloads.
    ///
    /// # Errors
    ///
    /// Returns a typed error when tables are truncated, inconsistent, unordered, or overflow.
    pub fn from_classic(
        track_id: u32,
        timescale: u32,
        boxes: ClassicTableBoxes<'_>,
    ) -> Mp4Result<Self> {
        if track_id == 0 || timescale == 0 {
            return Err(table_error("track id and timescale must be non-zero"));
        }
        let sizes = parse_sample_sizes(boxes.sample_sizes)?;
        let timing = parse_stts(boxes.stts, sizes.len())?;
        let composition = match boxes.ctts {
            Some(data) => parse_ctts(data, sizes.len())?,
            None => vec![0; sizes.len()],
        };
        let chunks = parse_chunk_offsets(boxes.chunk_offsets)?;
        let mapping = parse_stsc(boxes.stsc, chunks.len())?;
        let sync = match boxes.stss {
            Some(data) => Some(parse_stss(data, sizes.len())?),
            None => None,
        };
        let samples = build_samples(
            &sizes,
            &timing,
            &composition,
            &chunks,
            &mapping,
            sync.as_ref(),
        )?;
        Ok(Self {
            track_id,
            timescale,
            samples,
            has_composition_offsets: boxes.ctts.is_some(),
        })
    }

    #[must_use]
    pub fn samples(&self) -> &[Sample] {
        &self.samples
    }

    #[must_use]
    pub const fn has_composition_offsets(&self) -> bool {
        self.has_composition_offsets
    }

    #[must_use]
    pub fn duration(&self) -> u64 {
        self.samples
            .iter()
            .map(|sample| u64::from(sample.duration))
            .sum()
    }

    #[must_use]
    pub fn seek(&self, presentation_time: i64, mode: SeekMode) -> Option<usize> {
        let candidates = self
            .samples
            .iter()
            .enumerate()
            .filter(|(_, sample)| sample.is_sync);
        match mode {
            SeekMode::PreviousSync => candidates
                .filter(|(_, sample)| sample.pts <= presentation_time)
                .max_by_key(|(_, sample)| sample.pts)
                .map(|(index, _)| index),
            SeekMode::NextSync => candidates
                .filter(|(_, sample)| sample.pts >= presentation_time)
                .min_by_key(|(_, sample)| sample.pts)
                .map(|(index, _)| index),
        }
    }

    /// Appends already-validated fragment samples for the same track.
    ///
    /// # Errors
    ///
    /// Returns a typed error if the resulting sample count exceeds the configured ceiling.
    pub fn append_fragment(&mut self, samples: &[Sample]) -> Mp4Result<()> {
        let new_length = self
            .samples
            .len()
            .checked_add(samples.len())
            .ok_or_else(|| table_error("sample count overflowed"))?;
        if new_length > MAX_SAMPLES {
            return Err(table_error("sample count exceeds safety ceiling"));
        }
        self.samples.try_reserve(samples.len()).map_err(|_| {
            Mp4Error::new(
                Mp4ErrorKind::BufferLimit,
                "SampleIndex::append_fragment",
                0,
                "could not reserve fragment sample index",
            )
        })?;
        self.has_composition_offsets |= samples.iter().any(|sample| sample.pts != sample.dts);
        self.samples.extend_from_slice(samples);
        Ok(())
    }
}

#[derive(Clone, Copy)]
struct Timing {
    dts: i64,
    duration: u32,
}

#[derive(Clone, Copy)]
struct ChunkMapping {
    first_chunk: u32,
    samples_per_chunk: u32,
    description_index: u32,
}

fn parse_stts(data: &[u8], sample_count: usize) -> Mp4Result<Vec<Timing>> {
    let mut reader = Reader::new(data, "parse_stts");
    reader.full_box(&[0])?;
    let entries = checked_count(reader.u32()?, reader.remaining() / 8, &reader)?;
    let mut timing = Vec::with_capacity(sample_count);
    let mut dts = 0_i64;
    for _ in 0..entries {
        let count = usize::try_from(reader.u32()?)
            .map_err(|_| reader.error(Mp4ErrorKind::Overflow, "stts count exceeds usize"))?;
        let duration = reader.u32()?;
        if count == 0 || duration == 0 || timing.len().saturating_add(count) > sample_count {
            return Err(reader.error(Mp4ErrorKind::InvalidData, "invalid stts run"));
        }
        for _ in 0..count {
            timing.push(Timing { dts, duration });
            dts = dts.checked_add(i64::from(duration)).ok_or_else(|| {
                reader.error(Mp4ErrorKind::Overflow, "decode timestamp overflowed")
            })?;
        }
    }
    reader.finish()?;
    if timing.len() != sample_count {
        return Err(reader.error(
            Mp4ErrorKind::InvalidData,
            "stts does not cover every sample",
        ));
    }
    Ok(timing)
}

fn parse_ctts(data: &[u8], sample_count: usize) -> Mp4Result<Vec<i64>> {
    let mut reader = Reader::new(data, "parse_ctts");
    let (version, _) = reader.full_box(&[0, 1])?;
    let entries = checked_count(reader.u32()?, reader.remaining() / 8, &reader)?;
    let mut offsets = Vec::with_capacity(sample_count);
    for _ in 0..entries {
        let count = usize::try_from(reader.u32()?)
            .map_err(|_| reader.error(Mp4ErrorKind::Overflow, "ctts count exceeds usize"))?;
        let raw = reader.u32()?;
        let offset = if version == 0 {
            i64::from(raw)
        } else {
            i64::from(i32::from_be_bytes(raw.to_be_bytes()))
        };
        if count == 0 || offsets.len().saturating_add(count) > sample_count {
            return Err(reader.error(Mp4ErrorKind::InvalidData, "invalid ctts run"));
        }
        offsets.resize(offsets.len() + count, offset);
    }
    reader.finish()?;
    if offsets.len() != sample_count {
        return Err(reader.error(
            Mp4ErrorKind::InvalidData,
            "ctts does not cover every sample",
        ));
    }
    Ok(offsets)
}

fn parse_stsc(data: &[u8], chunk_count: usize) -> Mp4Result<Vec<ChunkMapping>> {
    let mut reader = Reader::new(data, "parse_stsc");
    reader.full_box(&[0])?;
    let entries = checked_count(reader.u32()?, reader.remaining() / 12, &reader)?;
    if entries == 0 {
        return Err(reader.error(Mp4ErrorKind::InvalidData, "stsc has no entries"));
    }
    let mut mapping = Vec::with_capacity(entries);
    for index in 0..entries {
        let entry = ChunkMapping {
            first_chunk: reader.u32()?,
            samples_per_chunk: reader.u32()?,
            description_index: reader.u32()?,
        };
        let minimum = u32::try_from(index + 1).unwrap_or(u32::MAX);
        if entry.first_chunk < minimum
            || entry.samples_per_chunk == 0
            || entry.description_index == 0
            || mapping
                .last()
                .is_some_and(|previous: &ChunkMapping| previous.first_chunk >= entry.first_chunk)
        {
            return Err(reader.error(Mp4ErrorKind::InvalidData, "invalid stsc entry"));
        }
        mapping.push(entry);
    }
    reader.finish()?;
    if mapping[0].first_chunk != 1
        || usize::try_from(mapping.last().map_or(0, |entry| entry.first_chunk))
            .unwrap_or(usize::MAX)
            > chunk_count
    {
        return Err(reader.error(
            Mp4ErrorKind::InvalidData,
            "stsc does not map available chunks",
        ));
    }
    Ok(mapping)
}

fn parse_sample_sizes(kind: SampleSizeBox<'_>) -> Mp4Result<Vec<u32>> {
    match kind {
        SampleSizeBox::Stsz(data) => parse_stsz(data),
        SampleSizeBox::Stz2(data) => parse_stz2(data),
    }
}

fn parse_stsz(data: &[u8]) -> Mp4Result<Vec<u32>> {
    let mut reader = Reader::new(data, "parse_stsz");
    reader.full_box(&[0])?;
    let fixed = reader.u32()?;
    let count = checked_sample_count(reader.u32()?, &reader)?;
    let sizes = if fixed == 0 {
        if count > reader.remaining() / 4 {
            return Err(reader.error(Mp4ErrorKind::Truncated, "stsz entries are truncated"));
        }
        let mut sizes = Vec::with_capacity(count);
        for _ in 0..count {
            sizes.push(reader.u32()?);
        }
        sizes
    } else {
        vec![fixed; count]
    };
    reader.finish()?;
    Ok(sizes)
}

fn parse_stz2(data: &[u8]) -> Mp4Result<Vec<u32>> {
    let mut reader = Reader::new(data, "parse_stz2");
    reader.full_box(&[0])?;
    reader.u24()?;
    let field_size = reader.u8()?;
    if !matches!(field_size, 4 | 8 | 16) {
        return Err(reader.error(Mp4ErrorKind::InvalidData, "invalid stz2 field size"));
    }
    let count = checked_sample_count(reader.u32()?, &reader)?;
    let required = (count * usize::from(field_size)).div_ceil(8);
    let packed = reader.bytes(required)?;
    let mut sizes = Vec::with_capacity(count);
    match field_size {
        4 => {
            for index in 0..count {
                let byte = packed[index / 2];
                sizes.push(u32::from(if index.is_multiple_of(2) {
                    byte >> 4
                } else {
                    byte & 0x0f
                }));
            }
        }
        8 => sizes.extend(packed.iter().copied().map(u32::from)),
        16 => {
            for bytes in packed.as_chunks::<2>().0 {
                sizes.push(u32::from(u16::from_be_bytes([bytes[0], bytes[1]])));
            }
        }
        _ => {}
    }
    reader.finish()?;
    Ok(sizes)
}

fn parse_chunk_offsets(kind: ChunkOffsetBox<'_>) -> Mp4Result<Vec<u64>> {
    let (data, width, operation) = match kind {
        ChunkOffsetBox::Stco(data) => (data, 4, "parse_stco"),
        ChunkOffsetBox::Co64(data) => (data, 8, "parse_co64"),
    };
    let mut reader = Reader::new(data, operation);
    reader.full_box(&[0])?;
    let count = checked_count(reader.u32()?, reader.remaining() / width, &reader)?;
    let mut offsets = Vec::with_capacity(count);
    for _ in 0..count {
        offsets.push(if width == 4 {
            u64::from(reader.u32()?)
        } else {
            reader.u64()?
        });
    }
    reader.finish()?;
    Ok(offsets)
}

fn parse_stss(data: &[u8], sample_count: usize) -> Mp4Result<BTreeSet<u32>> {
    let mut reader = Reader::new(data, "parse_stss");
    reader.full_box(&[0])?;
    let entries = checked_count(reader.u32()?, reader.remaining() / 4, &reader)?;
    let mut sync = BTreeSet::new();
    for _ in 0..entries {
        let sample = reader.u32()?;
        if sample == 0 || usize::try_from(sample).unwrap_or(usize::MAX) > sample_count {
            return Err(reader.error(Mp4ErrorKind::InvalidData, "stss sample is out of range"));
        }
        if !sync.insert(sample) {
            return Err(reader.error(Mp4ErrorKind::InvalidData, "duplicate stss sample"));
        }
    }
    reader.finish()?;
    Ok(sync)
}

fn build_samples(
    sizes: &[u32],
    timing: &[Timing],
    composition: &[i64],
    chunks: &[u64],
    mapping: &[ChunkMapping],
    sync: Option<&BTreeSet<u32>>,
) -> Mp4Result<Vec<Sample>> {
    let mut result = Vec::with_capacity(sizes.len());
    let mut sample_index = 0_usize;
    let mut mapping_index = 0_usize;
    for (chunk_index, chunk_offset) in chunks.iter().copied().enumerate() {
        let chunk_number = u32::try_from(chunk_index + 1).unwrap_or(u32::MAX);
        while mapping_index + 1 < mapping.len()
            && mapping[mapping_index + 1].first_chunk == chunk_number
        {
            mapping_index += 1;
        }
        let entry = mapping[mapping_index];
        let mut offset = chunk_offset;
        for _ in 0..entry.samples_per_chunk {
            if sample_index >= sizes.len() {
                return Err(table_error("chunk mapping contains too many samples"));
            }
            let time = timing[sample_index];
            let pts = time
                .dts
                .checked_add(composition[sample_index])
                .ok_or_else(|| table_error("presentation timestamp overflowed"))?;
            let sample_number = u32::try_from(sample_index + 1).unwrap_or(u32::MAX);
            result.push(Sample {
                offset,
                size: sizes[sample_index],
                dts: time.dts,
                pts,
                duration: time.duration,
                sample_description_index: entry.description_index,
                is_sync: sync.is_none_or(|entries| entries.contains(&sample_number)),
            });
            offset = offset
                .checked_add(u64::from(sizes[sample_index]))
                .ok_or_else(|| table_error("sample byte offset overflowed"))?;
            sample_index += 1;
        }
    }
    if sample_index != sizes.len() {
        return Err(table_error("chunk mapping does not cover every sample"));
    }
    Ok(result)
}

fn checked_count(value: u32, available: usize, reader: &Reader<'_>) -> Mp4Result<usize> {
    let count = checked_sample_count(value, reader)?;
    if count > available {
        return Err(reader.error(Mp4ErrorKind::Truncated, "entry count exceeds box payload"));
    }
    Ok(count)
}

fn checked_sample_count(value: u32, reader: &Reader<'_>) -> Mp4Result<usize> {
    let count = usize::try_from(value)
        .map_err(|_| reader.error(Mp4ErrorKind::Overflow, "sample count exceeds usize"))?;
    if count > MAX_SAMPLES {
        return Err(reader.error(
            Mp4ErrorKind::BufferLimit,
            "sample count exceeds safety ceiling",
        ));
    }
    Ok(count)
}

fn table_error(detail: impl Into<String>) -> Mp4Error {
    Mp4Error::new(
        Mp4ErrorKind::InvalidData,
        "SampleIndex::from_classic",
        0,
        detail,
    )
}
