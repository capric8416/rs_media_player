use mp4_demux::{
    CleanAperture, CleanApertureCrop, DecodeSample, EditList, EditListEntry, FragmentRun,
    HevcConfig, HevcNalArray, Mp4ErrorKind, PixelAspectRatio, Sample, SeekMode, VideoCodecConfig,
    VideoMovie, VideoSampleDescription, VideoTrack, parse_moov_video_tracks,
};
use std::io::Cursor;

fn boxed(kind: [u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut data = Vec::new();
    let size = u32::try_from(payload.len() + 8).unwrap_or(u32::MAX);
    data.extend(size.to_be_bytes());
    data.extend(kind);
    data.extend(payload);
    data
}

fn full_box(version: u8, flags: u32, body: &[u8]) -> Vec<u8> {
    let mut data = vec![version];
    data.extend(flags.to_be_bytes()[1..].iter().copied());
    data.extend(body);
    data
}

fn stsd(entry_kind: [u8; 4], children: &[u8]) -> Vec<u8> {
    let mut visual = vec![0_u8; 78];
    visual[6..8].copy_from_slice(&1_u16.to_be_bytes());
    visual[24..26].copy_from_slice(&1920_u16.to_be_bytes());
    visual[26..28].copy_from_slice(&1080_u16.to_be_bytes());
    visual.extend(children);
    let mut payload = vec![0, 0, 0, 0, 0, 0, 0, 1];
    payload.extend(boxed(entry_kind, &visual));
    payload
}

fn clap_payload(values: [i32; 8]) -> Vec<u8> {
    values.into_iter().flat_map(i32::to_be_bytes).collect()
}

fn u32s(values: &[u32]) -> Vec<u8> {
    values
        .iter()
        .flat_map(|value| value.to_be_bytes())
        .collect()
}

fn video_sample_table(description_index: u32) -> Vec<u8> {
    let avcc = [
        1, 66, 0, 30, 0xff, 0xe1, 0, 2, 0x67, 0x42, 1, 0, 2, 0x68, 0xce,
    ];
    let description = stsd(*b"avc1", &boxed(*b"avcC", &avcc));
    let mut table = boxed(*b"stsd", &description);
    table.extend(boxed(*b"stts", &u32s(&[0, 1, 2, 1000])));
    table.extend(boxed(*b"stsc", &u32s(&[0, 1, 1, 2, description_index])));
    table.extend(boxed(*b"stsz", &u32s(&[0, 0, 2, 100, 110])));
    table.extend(boxed(*b"stco", &u32s(&[0, 1, 1000])));
    table
}

fn video_trak() -> Vec<u8> {
    let mut tkhd = vec![0; 16];
    tkhd[12..16].copy_from_slice(&7_u32.to_be_bytes());
    let mut mdhd = vec![0; 20];
    mdhd[12..16].copy_from_slice(&1000_u32.to_be_bytes());
    let mut hdlr = vec![0; 12];
    hdlr[8..12].copy_from_slice(b"vide");
    let mut mdia = boxed(*b"mdhd", &mdhd);
    mdia.extend(boxed(*b"hdlr", &hdlr));
    mdia.extend(boxed(*b"minf", &boxed(*b"stbl", &video_sample_table(1))));
    let mut trak = boxed(*b"tkhd", &tkhd);
    trak.extend(boxed(*b"mdia", &mdia));
    boxed(*b"trak", &trak)
}

#[test]
fn joins_video_descriptions_and_sample_index() -> Result<(), Box<dyn std::error::Error>> {
    let track = VideoTrack::from_stbl(7, 1000, &video_sample_table(1))?;
    assert_eq!((track.index.track_id, track.index.timescale), (7, 1000));
    assert_eq!(track.index.samples().len(), 2);
    assert_eq!(
        (
            track.index.samples()[1].offset,
            track.index.samples()[1].pts
        ),
        (1100, 1000)
    );
    assert_eq!(
        track.description_for(&track.index.samples()[0])?.width,
        1920
    );
    Ok(())
}

#[test]
fn reads_samples_with_a_limit_and_reports_truncation() -> Result<(), Box<dyn std::error::Error>> {
    let track = VideoTrack::from_stbl(7, 1000, &video_sample_table(1))?;
    let sample = track.index.samples()[0];
    let mut source = vec![0; 1200];
    source[1000..1100].copy_from_slice(&[0x5a; 100]);
    let mut source = Cursor::new(source);
    assert_eq!(
        track.read_sample(&mut source, &sample, 100)?,
        vec![0x5a; 100]
    );
    assert_eq!(
        track
            .read_sample(&mut source, &sample, 99)
            .err()
            .map(|error| error.kind()),
        Some(Mp4ErrorKind::BufferLimit)
    );
    let mut short = Cursor::new(vec![0; 1050]);
    assert_eq!(
        track
            .read_sample(&mut short, &sample, 100)
            .err()
            .map(|error| error.kind()),
        Some(Mp4ErrorKind::Truncated)
    );
    Ok(())
}

#[test]
fn appends_only_matching_fragment_runs_with_valid_descriptions()
-> Result<(), Box<dyn std::error::Error>> {
    let mut track = VideoTrack::from_stbl(7, 1000, &video_sample_table(1))?;
    let run = FragmentRun {
        track_id: 7,
        samples: vec![Sample {
            offset: 2200,
            size: 20,
            dts: 2000,
            pts: 2000,
            duration: 1000,
            sample_description_index: 1,
            is_sync: true,
        }],
        next_decode_time: 3000,
        next_data_offset: 2220,
    };
    track.append_fragment_run(&run)?;
    assert_eq!(track.index.samples().len(), 3);
    assert_eq!(track.index.samples()[2].offset, 2200);

    let wrong_track = FragmentRun {
        track_id: 8,
        ..run.clone()
    };
    assert_eq!(
        track
            .append_fragment_run(&wrong_track)
            .err()
            .map(|e| e.kind()),
        Some(Mp4ErrorKind::InvalidData)
    );
    let bad_description = FragmentRun {
        samples: vec![Sample {
            sample_description_index: 2,
            ..run.samples[0]
        }],
        ..run
    };
    assert_eq!(
        track
            .append_fragment_run(&bad_description)
            .err()
            .map(|e| e.kind()),
        Some(Mp4ErrorKind::InvalidData)
    );
    assert_eq!(
        track.index.samples().len(),
        3,
        "failed append must leave index unchanged"
    );
    Ok(())
}

#[test]
fn rejects_inconsistent_description_or_duplicate_sample_table() {
    assert_eq!(
        VideoTrack::from_stbl(7, 1000, &video_sample_table(2))
            .err()
            .map(|error| error.kind()),
        Some(Mp4ErrorKind::InvalidData)
    );
    let mut duplicate = video_sample_table(1);
    duplicate.extend(boxed(*b"stco", &u32s(&[0, 1, 2000])));
    assert_eq!(
        VideoTrack::from_stbl(7, 1000, &duplicate)
            .err()
            .map(|error| error.kind()),
        Some(Mp4ErrorKind::InvalidData)
    );
    assert_eq!(
        VideoTrack::from_stbl(7, 1000, &[])
            .err()
            .map(|error| error.kind()),
        Some(Mp4ErrorKind::InvalidData)
    );
}

#[test]
fn discovers_video_tracks_and_scans_moov_after_mdat() -> Result<(), Box<dyn std::error::Error>> {
    let mut moov = video_trak();
    // An audio track needs no video sample table and is omitted from the result.
    let mut audio_tkhd = vec![0; 16];
    audio_tkhd[12..16].copy_from_slice(&8_u32.to_be_bytes());
    let mut audio_hdlr = vec![0; 12];
    audio_hdlr[8..12].copy_from_slice(b"soun");
    let audio_mdia = boxed(*b"hdlr", &audio_hdlr);
    let mut audio_trak = boxed(*b"tkhd", &audio_tkhd);
    audio_trak.extend(boxed(*b"mdia", &audio_mdia));
    moov.extend(boxed(*b"trak", &audio_trak));

    let tracks = parse_moov_video_tracks(&moov)?;
    assert_eq!(tracks.len(), 1);
    assert_eq!(
        (tracks[0].index.track_id, tracks[0].index.timescale),
        (7, 1000)
    );

    let mut file = boxed(*b"mdat", &[1, 2, 3, 4]);
    file.extend(boxed(*b"moov", &moov));
    let mut movie = VideoMovie::read(Cursor::new(file), 1_000_000)?;
    assert_eq!(movie.tracks, tracks);
    movie.append_fragment_run(&FragmentRun {
        track_id: 7,
        samples: vec![Sample {
            offset: 1500,
            size: 25,
            dts: 2000,
            pts: 2000,
            duration: 1000,
            sample_description_index: 1,
            is_sync: true,
        }],
        next_decode_time: 3000,
        next_data_offset: 1525,
    })?;
    assert_eq!(movie.tracks[0].index.samples().len(), 3);
    Ok(())
}

#[test]
fn reads_fragmented_samples_from_moof_traf_and_trun() -> Result<(), Box<dyn std::error::Error>> {
    let mut trex = Vec::new();
    trex.extend(7_u32.to_be_bytes());
    trex.extend(1_u32.to_be_bytes());
    trex.extend(1000_u32.to_be_bytes());
    trex.extend(30_u32.to_be_bytes());
    trex.extend(0_u32.to_be_bytes());
    let movie_box = {
        let mut payload = video_trak();
        payload.extend(boxed(*b"mvex", &boxed(*b"trex", &full_box(0, 0, &trex))));
        boxed(*b"moov", &payload)
    };
    let mut file = boxed(*b"mdat", &[0; 4]);
    file.extend(movie_box);
    let moof_offset = file.len() as u64;

    let make_moof = |sample_offset: u64, include_tfdt: bool| {
        let mut tfhd_body = Vec::new();
        tfhd_body.extend(7_u32.to_be_bytes());
        tfhd_body.extend(sample_offset.to_be_bytes());
        let tfhd = boxed(*b"tfhd", &full_box(0, 1, &tfhd_body));
        let trun = boxed(*b"trun", &full_box(0, 0, &1_u32.to_be_bytes()));
        let mut traf = tfhd;
        if include_tfdt {
            traf.extend(boxed(*b"tfdt", &full_box(0, 0, &2000_u32.to_be_bytes())));
        }
        traf.extend(trun);
        boxed(*b"moof", &boxed(*b"traf", &traf))
    };
    let first_moof = make_moof(0, true);
    let sample_offset = moof_offset + first_moof.len() as u64 + 8;
    let fragment_box = make_moof(sample_offset, true);
    file.extend(fragment_box);
    file.extend(boxed(*b"mdat", &[0xa5; 30]));
    let second_moof_offset = file.len() as u64;
    let second_moof = make_moof(0, false);
    let second_sample_offset = second_moof_offset + second_moof.len() as u64 + 8;
    file.extend(make_moof(second_sample_offset, false));
    file.extend(boxed(*b"mdat", &[0x5a; 30]));

    let movie = VideoMovie::read(Cursor::new(file), 1_000_000)?;
    let samples = movie.tracks[0].index.samples();
    assert_eq!(samples.len(), 4);
    assert_eq!((samples[2].offset, samples[2].size), (sample_offset, 30));
    assert_eq!((samples[2].dts, samples[2].pts), (2000, 2000));
    assert_eq!(
        (samples[3].offset, samples[3].dts),
        (second_sample_offset, 3000)
    );
    let mut sample_file = vec![0; usize::try_from(sample_offset)? + 30];
    sample_file[usize::try_from(sample_offset)?..].fill(0xa5);
    let mut source = Cursor::new(sample_file);
    assert_eq!(
        movie.tracks[0].read_sample(&mut source, &samples[2], 30)?,
        vec![0xa5; 30]
    );
    Ok(())
}

#[test]
fn movie_parsing_attaches_edit_list_and_maps_track_pts() -> Result<(), Box<dyn std::error::Error>> {
    let mut edit_entries = 2_u32.to_be_bytes().to_vec();
    edit_entries.extend(100_u32.to_be_bytes());
    edit_entries.extend((-1_i32).to_be_bytes());
    edit_entries.extend(1_i16.to_be_bytes());
    edit_entries.extend(0_i16.to_be_bytes());
    edit_entries.extend(2000_u32.to_be_bytes());
    edit_entries.extend(0_i32.to_be_bytes());
    edit_entries.extend(1_i16.to_be_bytes());
    edit_entries.extend(0_i16.to_be_bytes());
    let mut edited_trak = video_trak();
    let mut trak_payload = edited_trak.split_off(8);
    trak_payload.extend(boxed(
        *b"edts",
        &boxed(*b"elst", &full_box(0, 0, &edit_entries)),
    ));
    let mut mvhd = vec![0; 20];
    mvhd[12..16].copy_from_slice(&1000_u32.to_be_bytes());
    let mut moov_payload = boxed(*b"mvhd", &mvhd);
    moov_payload.extend(boxed(*b"trak", &trak_payload));
    let movie = VideoMovie::read(Cursor::new(boxed(*b"moov", &moov_payload)), 100_000)?;
    assert_eq!(movie.movie_timescale, Some(1000));
    let track = &movie.tracks[0];
    assert!(track.edit_list.is_some());
    assert_eq!(
        track.presentation_time_in_movie_timescale(&track.index.samples()[0], 1000)?,
        Some(100)
    );
    assert_eq!(track.decode_samples(1000)?[0].decode_timestamp, 100);
    Ok(())
}

#[test]
fn builds_presented_sequence_with_empty_and_repeated_edits()
-> Result<(), Box<dyn std::error::Error>> {
    let mut track = VideoTrack::from_stbl(7, 1000, &video_sample_table(1))?;
    track.edit_list = Some(EditList {
        entries: vec![
            EditListEntry {
                segment_duration: 500,
                media_time: -1,
                media_rate_integer: 1,
                media_rate_fraction: 0,
            },
            EditListEntry {
                segment_duration: 1000,
                media_time: 0,
                media_rate_integer: 1,
                media_rate_fraction: 0,
            },
            EditListEntry {
                segment_duration: 1000,
                media_time: 1000,
                media_rate_integer: 1,
                media_rate_fraction: 0,
            },
            EditListEntry {
                segment_duration: 1000,
                media_time: 0,
                media_rate_integer: 1,
                media_rate_fraction: 0,
            },
        ],
    });
    let presented = track.presented_samples(1000)?;
    assert_eq!(
        presented
            .iter()
            .map(|sample| (sample.sample_index, sample.presentation_time))
            .collect::<Vec<_>>(),
        [(0, 500), (1, 1500), (0, 2500)]
    );
    assert_eq!(
        track.seek_sync_sample(1600, 1000, SeekMode::PreviousSync)?,
        Some(presented[1])
    );
    assert_eq!(
        track.seek_sync_sample(1600, 1000, SeekMode::NextSync)?,
        Some(presented[2])
    );
    assert_eq!(
        track.seek_sync_sample(2500, 1000, SeekMode::PreviousSync)?,
        Some(presented[2])
    );
    assert_eq!(
        track.seek_sync_sample(3000, 1000, SeekMode::NextSync)?,
        None
    );
    Ok(())
}

#[test]
fn decode_sequence_keeps_sync_preroll_and_marks_it_discardable()
-> Result<(), Box<dyn std::error::Error>> {
    let mut stbl = video_sample_table(1);
    stbl.extend(boxed(*b"stss", &u32s(&[0, 1, 1])));
    let mut track = VideoTrack::from_stbl(7, 1000, &stbl)?;
    track.edit_list = Some(EditList {
        entries: vec![EditListEntry {
            segment_duration: 1000,
            media_time: 1000,
            media_rate_integer: 1,
            media_rate_fraction: 0,
        }],
    });
    assert_eq!(
        track.decode_samples(1000)?,
        [
            DecodeSample {
                sample_index: 0,
                edit_index: Some(0),
                discard_output: true,
                decode_timestamp: -1000,
                presentation_time: None,
            },
            DecodeSample {
                sample_index: 1,
                edit_index: Some(0),
                discard_output: false,
                decode_timestamp: 0,
                presentation_time: Some(0),
            },
        ]
    );
    Ok(())
}

#[test]
fn decode_sequence_keeps_trailing_reordered_samples_after_edit_end()
-> Result<(), Box<dyn std::error::Error>> {
    let mut stbl = video_sample_table(1);
    stbl.extend(boxed(*b"stss", &u32s(&[0, 1, 1])));
    let mut track = VideoTrack::from_stbl(7, 1000, &stbl)?;
    track.append_fragment_run(&FragmentRun {
        track_id: 7,
        samples: vec![
            Sample {
                offset: 1200,
                size: 10,
                dts: 2000,
                pts: 3000,
                duration: 1000,
                sample_description_index: 1,
                is_sync: true,
            },
            Sample {
                offset: 1210,
                size: 10,
                dts: 3000,
                pts: 2000,
                duration: 1000,
                sample_description_index: 1,
                is_sync: false,
            },
        ],
        next_decode_time: 4000,
        next_data_offset: 1220,
    })?;
    track.edit_list = Some(EditList {
        entries: vec![EditListEntry {
            segment_duration: 1500,
            media_time: 0,
            media_rate_integer: 1,
            media_rate_fraction: 0,
        }],
    });
    let decoded = track.decode_samples(1000)?;
    assert_eq!(decoded.len(), 4);
    assert_eq!(decoded[0].presentation_time, Some(0));
    assert_eq!(decoded[1].presentation_time, Some(1000));
    assert!(decoded[2].discard_output);
    assert!(decoded[3].discard_output);
    assert_eq!(
        decoded
            .iter()
            .map(|sample| sample.decode_timestamp)
            .collect::<Vec<_>>(),
        [0, 1000, 2000, 3000]
    );
    Ok(())
}

#[test]
fn edit_decode_timestamps_normalize_first_visible_pts_to_zero()
-> Result<(), Box<dyn std::error::Error>> {
    let mut stbl = video_sample_table(1);
    stbl.extend(boxed(*b"ctts", &u32s(&[0, 1, 2, 1000])));
    stbl.extend(boxed(*b"stss", &u32s(&[0, 1, 1])));
    let mut track = VideoTrack::from_stbl(7, 1000, &stbl)?;
    track.edit_list = Some(EditList {
        entries: vec![EditListEntry {
            segment_duration: 1000,
            media_time: 1000,
            media_rate_integer: 1,
            media_rate_fraction: 0,
        }],
    });
    let decoded = track.decode_samples(1000)?;
    assert_eq!(decoded[0].decode_timestamp, -1000);
    assert_eq!(decoded[0].presentation_time, Some(0));
    assert_eq!(decoded[1].decode_timestamp, 0);
    assert!(decoded[1].discard_output);
    Ok(())
}

#[test]
fn reports_missing_or_duplicate_movie_metadata() {
    assert_eq!(
        VideoMovie::read(Cursor::new(boxed(*b"mdat", &[])), 100)
            .err()
            .map(|e| e.kind()),
        Some(Mp4ErrorKind::InvalidData)
    );
    let mut two_moov = boxed(*b"moov", &[]);
    two_moov.extend(boxed(*b"moov", &[]));
    assert_eq!(
        VideoMovie::read(Cursor::new(two_moov), 100)
            .err()
            .map(|e| e.kind()),
        Some(Mp4ErrorKind::InvalidData)
    );
    assert_eq!(
        VideoMovie::read(Cursor::new(boxed(*b"moov", &boxed(*b"free", &[1]))), 0)
            .err()
            .map(|e| e.kind()),
        Some(Mp4ErrorKind::BufferLimit)
    );
    assert!(parse_moov_video_tracks(&boxed(*b"trak", &[])).is_err());
}

#[test]
fn parses_and_computes_clean_aperture() -> Result<(), Box<dyn std::error::Error>> {
    let raw = clap_payload([1880, 1, 1000, 1, 10, 1, -10, 1]);
    let aperture = CleanAperture::parse(&raw)?;
    assert_eq!(
        aperture.crop(1920, 1080)?,
        CleanApertureCrop {
            left: 30,
            right: 10,
            top: 30,
            bottom: 50
        }
    );
    let half_pixel = CleanAperture::parse(&clap_payload([1919, 1, 1080, 1, 0, 1, 0, 1]))?;
    assert_eq!(
        half_pixel.crop(1920, 1080)?,
        CleanApertureCrop {
            left: 0,
            right: 1,
            top: 0,
            bottom: 0
        }
    );
    let avcc = [
        1, 66, 0, 30, 0xff, 0xe1, 0, 2, 0x67, 0x42, 1, 0, 2, 0x68, 0xce,
    ];
    let mut children = boxed(*b"avcC", &avcc);
    children.extend(boxed(*b"clap", &raw));
    let entry = VideoSampleDescription::parse_stsd(&stsd(*b"avc1", &children))?;
    assert_eq!(entry[0].clean_aperture, Some(aperture));
    children.extend(boxed(*b"clap", &raw));
    assert_eq!(
        VideoSampleDescription::parse_stsd(&stsd(*b"avc1", &children))
            .err()
            .map(|e| e.kind()),
        Some(Mp4ErrorKind::InvalidData)
    );
    Ok(())
}

#[test]
fn rejects_invalid_clean_apertures() -> Result<(), Box<dyn std::error::Error>> {
    assert_eq!(
        CleanAperture::parse(&[0; 31]).err().map(|e| e.kind()),
        Some(Mp4ErrorKind::Truncated)
    );
    assert_eq!(
        CleanAperture::parse(&clap_payload([1, 0, 1, 1, 0, 1, 0, 1]))
            .err()
            .map(|e| e.kind()),
        Some(Mp4ErrorKind::InvalidData)
    );
    let too_wide = CleanAperture::parse(&clap_payload([1921, 1, 1080, 1, 0, 1, 0, 1]))?;
    assert!(too_wide.crop(1920, 1080).is_err());
    let outside = CleanAperture::parse(&clap_payload([1920, 1, 1080, 1, -1, 1, 0, 1]))?;
    assert!(outside.crop(1920, 1080).is_err());
    Ok(())
}

#[test]
fn parses_avc_entry_and_pixel_aspect() -> Result<(), Box<dyn std::error::Error>> {
    let avcc = [
        1, 66, 0, 30, 0xff, 0xe1, 0, 2, 0x67, 0x42, 1, 0, 2, 0x68, 0xce,
    ];
    let mut children = boxed(*b"avcC", &avcc);
    children.extend(boxed(*b"pasp", &[0, 0, 0, 32, 0, 0, 0, 30]));
    children.extend(boxed(*b"free", &[1, 2]));
    let parsed = VideoSampleDescription::parse_stsd(&stsd(*b"avc1", &children))?;
    assert_eq!(parsed.len(), 1);
    assert_eq!((parsed[0].width, parsed[0].height), (1920, 1080));
    assert_eq!(
        parsed[0]
            .pixel_aspect_ratio
            .and_then(PixelAspectRatio::reduced),
        Some((16, 15))
    );
    assert!(matches!(&parsed[0].codec, VideoCodecConfig::Avc(config) if config.length_size == 4));
    Ok(())
}

#[test]
fn parses_hevc_and_av1_sample_descriptions() -> Result<(), Box<dyn std::error::Error>> {
    let hevc = HevcConfig {
        profile_space: 0,
        tier_flag: false,
        profile_idc: 2,
        profile_compatibility_flags: 0x6000_0000,
        constraint_indicator_flags: 0x9000_0000_0000,
        level_idc: 120,
        min_spatial_segmentation_idc: 0,
        parallelism_type: 0,
        chroma_format: 1,
        bit_depth_luma: 10,
        bit_depth_chroma: 10,
        average_frame_rate: 0,
        constant_frame_rate: 0,
        num_temporal_layers: 1,
        temporal_id_nested: true,
        length_size: 4,
        arrays: [32_u8, 33, 34]
            .into_iter()
            .map(|nal_unit_type| HevcNalArray {
                array_completeness: true,
                nal_unit_type,
                nal_units: vec![vec![nal_unit_type << 1, 1, 0xaa]],
            })
            .collect(),
    };
    let hevc_entry = stsd(*b"hvc1", &boxed(*b"hvcC", &hevc.normalize()?));
    let parsed = VideoSampleDescription::parse_stsd(&hevc_entry)?;
    assert!(matches!(&parsed[0].codec, VideoCodecConfig::Hevc(config) if config == &hevc));
    let av1_entry = stsd(
        *b"av01",
        &boxed(*b"av1C", &[0x81, 8, 0x4c, 0, 0x0a, 1, 0x80]),
    );
    let parsed = VideoSampleDescription::parse_stsd(&av1_entry)?;
    assert!(matches!(&parsed[0].codec, VideoCodecConfig::Av1(config) if config.high_bitdepth));
    Ok(())
}

#[test]
fn handles_pasp_override_and_invalid_entries() {
    let avcc = [
        1, 66, 0, 30, 0xff, 0xe1, 0, 2, 0x67, 0x42, 1, 0, 2, 0x68, 0xce,
    ];
    let mut children = boxed(*b"avcC", &avcc);
    children.extend(boxed(*b"pasp", &[0, 0, 0, 1, 0, 0, 0, 1]));
    children.extend(boxed(*b"pasp", &[0, 0, 0, 2, 0, 0, 0, 0]));
    let parsed = VideoSampleDescription::parse_stsd(&stsd(*b"avc1", &children));
    assert_eq!(
        parsed
            .ok()
            .and_then(|v| v[0].pixel_aspect_ratio)
            .and_then(PixelAspectRatio::reduced),
        Some((1, 1))
    );
    assert_eq!(
        PixelAspectRatio::parse(&[0; 7]).err().map(|e| e.kind()),
        Some(Mp4ErrorKind::Truncated)
    );
    assert_eq!(
        PixelAspectRatio {
            horizontal_spacing: 0,
            vertical_spacing: 1
        }
        .reduced(),
        None
    );
    let mut truncated = stsd(*b"avc1", &children);
    truncated.pop();
    assert_eq!(
        VideoSampleDescription::parse_stsd(&truncated)
            .err()
            .map(|e| e.kind()),
        Some(Mp4ErrorKind::Truncated)
    );
    children.extend(boxed(*b"avcC", &avcc));
    assert_eq!(
        VideoSampleDescription::parse_stsd(&stsd(*b"avc1", &children))
            .err()
            .map(|e| e.kind()),
        Some(Mp4ErrorKind::InvalidData)
    );
    assert_eq!(
        VideoSampleDescription::parse_stsd(&stsd(*b"encv", &[]))
            .err()
            .map(|e| e.kind()),
        Some(Mp4ErrorKind::Unsupported)
    );
}
