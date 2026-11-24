use mp4_demux::{
    ChunkOffsetBox, ClassicTableBoxes, FragmentRun, Mp4ErrorKind, SampleIndex, SampleSizeBox,
    SeekMode, TrackFragmentHeader, parse_tfdt, parse_trex,
};

#[test]
fn builds_classic_index_with_timestamps_chunks_and_sync_samples()
-> Result<(), Box<dyn std::error::Error>> {
    let decode_timing = table(0, &[(2, 1_000), (2, 500)]);
    let composition_timing = table(
        1,
        &[(2, 100), (2, u32::from_be_bytes((-50_i32).to_be_bytes()))],
    );
    let chunk_mapping = stsc(&[(1, 2, 1), (2, 1, 2)]);
    let sample_sizes = stsz(&[100, 110, 120, 130]);
    let chunk_offsets = offsets64(&[1_000, 2_000, 3_000]);
    let sync_samples = values32(&[1, 3]);

    let index = SampleIndex::from_classic(
        7,
        1_000,
        ClassicTableBoxes {
            stts: &decode_timing,
            ctts: Some(&composition_timing),
            stsc: &chunk_mapping,
            sample_sizes: SampleSizeBox::Stsz(&sample_sizes),
            chunk_offsets: ChunkOffsetBox::Co64(&chunk_offsets),
            stss: Some(&sync_samples),
        },
    )?;
    let samples = index.samples();
    assert_eq!(samples.len(), 4);
    assert_eq!(
        samples
            .iter()
            .map(|sample| sample.offset)
            .collect::<Vec<_>>(),
        [1_000, 1_100, 2_000, 3_000]
    );
    assert_eq!(
        samples.iter().map(|sample| sample.dts).collect::<Vec<_>>(),
        [0, 1_000, 2_000, 2_500]
    );
    assert_eq!(
        samples.iter().map(|sample| sample.pts).collect::<Vec<_>>(),
        [100, 1_100, 1_950, 2_450]
    );
    assert_eq!(
        samples
            .iter()
            .map(|sample| sample.is_sync)
            .collect::<Vec<_>>(),
        [true, false, true, false]
    );
    assert_eq!(samples[2].sample_description_index, 2);
    assert_eq!(index.duration(), 3_000);
    assert_eq!(index.seek(2_000, SeekMode::PreviousSync), Some(2));
    assert_eq!(index.seek(1_000, SeekMode::NextSync), Some(2));
    Ok(())
}

#[test]
fn supports_constant_stsz_stco_and_implicit_sync() -> Result<(), Box<dyn std::error::Error>> {
    let decode_timing = table(0, &[(3, 1_024)]);
    let chunk_mapping = stsc(&[(1, 1, 1)]);
    let sample_sizes = fixed_stsz(5, 3);
    let chunk_offsets = offsets32(&[10, 20, 30]);
    let index = SampleIndex::from_classic(
        1,
        48_000,
        ClassicTableBoxes {
            stts: &decode_timing,
            ctts: None,
            stsc: &chunk_mapping,
            sample_sizes: SampleSizeBox::Stsz(&sample_sizes),
            chunk_offsets: ChunkOffsetBox::Stco(&chunk_offsets),
            stss: None,
        },
    )?;
    assert_eq!(
        index
            .samples()
            .iter()
            .map(|sample| sample.size)
            .collect::<Vec<_>>(),
        [5, 5, 5]
    );
    assert!(index.samples().iter().all(|sample| sample.is_sync));
    Ok(())
}

#[test]
fn unpacks_four_bit_stz2_sizes() -> Result<(), Box<dyn std::error::Error>> {
    let stts = table(0, &[(3, 1)]);
    let stsc = stsc(&[(1, 3, 1)]);
    let stz2 = stz2_4(&[1, 2, 3]);
    let stco = offsets32(&[100]);
    let index = SampleIndex::from_classic(
        2,
        1,
        ClassicTableBoxes {
            stts: &stts,
            ctts: None,
            stsc: &stsc,
            sample_sizes: SampleSizeBox::Stz2(&stz2),
            chunk_offsets: ChunkOffsetBox::Stco(&stco),
            stss: None,
        },
    )?;
    assert_eq!(
        index
            .samples()
            .iter()
            .map(|sample| sample.size)
            .collect::<Vec<_>>(),
        [1, 2, 3]
    );
    assert_eq!(
        index
            .samples()
            .iter()
            .map(|sample| sample.offset)
            .collect::<Vec<_>>(),
        [100, 101, 103]
    );
    Ok(())
}

#[test]
fn applies_fragment_defaults_overrides_and_signed_cts() -> Result<(), Box<dyn std::error::Error>> {
    let defaults = parse_trex(&trex(1, 1, 1_000, 100, 0x0001_0000))?;
    let tfhd =
        TrackFragmentHeader::parse_tfhd(&tfhd(0x0002_0018, 1, &[900, 90]), defaults, 1_000, 500)?;
    assert_eq!(tfhd.base_data_offset, 1_000);
    assert_eq!(tfhd.default_sample_duration, 900);
    assert_eq!(tfhd.default_sample_size, 90);
    let decode_time = parse_tfdt(&tfdt(1, 5_000_000_000))?;
    let run = FragmentRun::parse_trun(&trun_v1(), &tfhd, decode_time, None)?;
    assert_eq!(run.samples.len(), 2);
    assert_eq!(
        (run.samples[0].offset, run.samples[1].offset),
        (1_200, 1_290)
    );
    assert_eq!(run.samples[0].pts, 4_999_999_900);
    assert_eq!(run.samples[1].dts, 5_000_000_900);
    assert_eq!(run.samples[1].pts, 5_000_001_100);
    assert!(run.samples[0].is_sync);
    assert!(!run.samples[1].is_sync);
    assert_eq!(run.next_decode_time, 5_000_002_000);
    assert_eq!(run.next_data_offset, 1_400);
    Ok(())
}

#[test]
fn continues_offsets_and_decode_time_across_truns() -> Result<(), Box<dyn std::error::Error>> {
    let defaults = parse_trex(&trex(3, 1, 1_000, 100, 0))?;
    let header = TrackFragmentHeader::parse_tfhd(&tfhd(0, 3, &[]), defaults, 900, 700)?;
    let first = FragmentRun::parse_trun(&trun_defaults(2), &header, 4_000, None)?;
    let second = FragmentRun::parse_trun(
        &trun_defaults(1),
        &header,
        first.next_decode_time,
        Some(first.next_data_offset),
    )?;
    assert_eq!(
        first
            .samples
            .iter()
            .map(|sample| sample.offset)
            .collect::<Vec<_>>(),
        [700, 800]
    );
    assert_eq!(second.samples[0].offset, 900);
    assert_eq!(second.samples[0].dts, 6_000);
    Ok(())
}

#[test]
fn parses_version_zero_tfdt() -> Result<(), Box<dyn std::error::Error>> {
    assert_eq!(parse_tfdt(&tfdt(0, 123_456))?, 123_456);
    Ok(())
}

#[test]
fn rejects_inconsistent_classic_tables() {
    let stts = table(0, &[(2, 1)]);
    let bad_stsc = stsc(&[(2, 1, 1)]);
    let stsz = stsz(&[1, 1]);
    let stco = offsets32(&[0, 1]);
    let result = SampleIndex::from_classic(
        1,
        1,
        ClassicTableBoxes {
            stts: &stts,
            ctts: None,
            stsc: &bad_stsc,
            sample_sizes: SampleSizeBox::Stsz(&stsz),
            chunk_offsets: ChunkOffsetBox::Stco(&stco),
            stss: None,
        },
    );
    assert_eq!(
        result.err().map(|error| error.kind()),
        Some(Mp4ErrorKind::InvalidData)
    );
}

#[test]
fn rejects_truncated_and_conflicting_fragment_fields() -> Result<(), Box<dyn std::error::Error>> {
    let defaults = parse_trex(&trex(1, 1, 1_000, 100, 0))?;
    let header_result =
        TrackFragmentHeader::parse_tfhd(&tfhd(0x0002_0001, 1, &[0, 0]), defaults, 100, 100);
    assert!(header_result.is_err());

    let header = TrackFragmentHeader::parse_tfhd(&tfhd(0, 1, &[]), defaults, 100, 100)?;
    let conflicting = full_box(0, 0x0000_0404, &[0, 0, 0, 1, 0, 0, 0, 0]);
    assert!(FragmentRun::parse_trun(&conflicting, &header, 0, None).is_err());
    let truncated = full_box(0, 0x0000_0100, &[0, 0, 0, 2, 0, 0, 0, 1]);
    assert_eq!(
        FragmentRun::parse_trun(&truncated, &header, 0, None)
            .err()
            .map(|error| error.kind()),
        Some(Mp4ErrorKind::Truncated)
    );
    Ok(())
}

fn table(version: u8, entries: &[(u32, u32)]) -> Vec<u8> {
    let mut body = Vec::new();
    push_u32(&mut body, u32::try_from(entries.len()).unwrap_or_default());
    for &(count, value) in entries {
        push_u32(&mut body, count);
        push_u32(&mut body, value);
    }
    full_box(version, 0, &body)
}

fn stsc(entries: &[(u32, u32, u32)]) -> Vec<u8> {
    let mut body = Vec::new();
    push_u32(&mut body, u32::try_from(entries.len()).unwrap_or_default());
    for &(first, count, description) in entries {
        push_u32(&mut body, first);
        push_u32(&mut body, count);
        push_u32(&mut body, description);
    }
    full_box(0, 0, &body)
}

fn stsz(sizes: &[u32]) -> Vec<u8> {
    let mut body = Vec::new();
    push_u32(&mut body, 0);
    push_u32(&mut body, u32::try_from(sizes.len()).unwrap_or_default());
    for &size in sizes {
        push_u32(&mut body, size);
    }
    full_box(0, 0, &body)
}

fn fixed_stsz(size: u32, count: u32) -> Vec<u8> {
    let mut body = Vec::new();
    push_u32(&mut body, size);
    push_u32(&mut body, count);
    full_box(0, 0, &body)
}

fn stz2_4(sizes: &[u8]) -> Vec<u8> {
    let mut body = vec![0, 0, 0, 4];
    push_u32(&mut body, u32::try_from(sizes.len()).unwrap_or_default());
    for pair in sizes.chunks(2) {
        body.push((pair[0] << 4) | pair.get(1).copied().unwrap_or_default());
    }
    full_box(0, 0, &body)
}

fn offsets32(offsets: &[u32]) -> Vec<u8> {
    let mut body = Vec::new();
    push_u32(&mut body, u32::try_from(offsets.len()).unwrap_or_default());
    for &offset in offsets {
        push_u32(&mut body, offset);
    }
    full_box(0, 0, &body)
}

fn offsets64(offsets: &[u64]) -> Vec<u8> {
    let mut body = Vec::new();
    push_u32(&mut body, u32::try_from(offsets.len()).unwrap_or_default());
    for &offset in offsets {
        body.extend(offset.to_be_bytes());
    }
    full_box(0, 0, &body)
}

fn values32(values: &[u32]) -> Vec<u8> {
    offsets32(values)
}

fn trex(track: u32, description: u32, duration: u32, size: u32, flags: u32) -> Vec<u8> {
    let mut body = Vec::new();
    for value in [track, description, duration, size, flags] {
        push_u32(&mut body, value);
    }
    full_box(0, 0, &body)
}

fn tfhd(flags: u32, track: u32, optional_values: &[u32]) -> Vec<u8> {
    let mut body = Vec::new();
    push_u32(&mut body, track);
    for &value in optional_values {
        push_u32(&mut body, value);
    }
    full_box(0, flags, &body)
}

fn tfdt(version: u8, time: u64) -> Vec<u8> {
    let body = if version == 0 {
        u32::try_from(time)
            .unwrap_or_default()
            .to_be_bytes()
            .to_vec()
    } else {
        time.to_be_bytes().to_vec()
    };
    full_box(version, 0, &body)
}

fn trun_v1() -> Vec<u8> {
    let mut body = Vec::new();
    push_u32(&mut body, 2);
    push_u32(&mut body, 200);
    push_u32(&mut body, 0);
    for (duration, size, cts) in [(900, 90, -100_i32), (1_100, 110, 200)] {
        push_u32(&mut body, duration);
        push_u32(&mut body, size);
        body.extend(cts.to_be_bytes());
    }
    full_box(1, 0x0000_0b05, &body)
}

fn trun_defaults(count: u32) -> Vec<u8> {
    full_box(0, 0, &count.to_be_bytes())
}

fn full_box(version: u8, flags: u32, body: &[u8]) -> Vec<u8> {
    let mut output = vec![
        version,
        u8::try_from((flags >> 16) & 0xff).unwrap_or_default(),
        u8::try_from((flags >> 8) & 0xff).unwrap_or_default(),
        u8::try_from(flags & 0xff).unwrap_or_default(),
    ];
    output.extend(body);
    output
}

fn push_u32(output: &mut Vec<u8>, value: u32) {
    output.extend(value.to_be_bytes());
}
