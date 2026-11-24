use mp4_demux::{
    AacConfig, Av1Config, AvcConfig, EsdsConfig, HevcConfig, HevcNalArray, Mp4ErrorKind, OpusConfig,
};

#[test]
fn parses_and_normalizes_avc_parameter_sets() -> Result<(), Box<dyn std::error::Error>> {
    let raw = [
        1, 66, 0, 30, 0xff, 0xe1, 0, 2, 0x67, 0x42, 1, 0, 2, 0x68, 0xce,
    ];
    let config = AvcConfig::parse(&raw)?;
    assert_eq!(config.profile, 66);
    assert_eq!(config.length_size, 4);
    assert_eq!(config.sequence_parameter_sets, [vec![0x67, 0x42]]);
    assert_eq!(config.picture_parameter_sets, [vec![0x68, 0xce]]);
    assert_eq!(config.normalize()?, raw);
    Ok(())
}

#[test]
fn parses_avc_high10_extension() -> Result<(), Box<dyn std::error::Error>> {
    let raw = [
        1, 100, 0, 40, 0xff, 0xe1, 0, 2, 0x67, 0x64, 1, 0, 1, 0x68, 0xfd, 0xfa, 0xfa, 1, 0, 1, 0x6d,
    ];
    let config = AvcConfig::parse(&raw)?;
    assert_eq!(config.chroma_format, Some(1));
    assert_eq!(config.bit_depth_luma, Some(10));
    assert_eq!(config.bit_depth_chroma, Some(10));
    assert_eq!(config.sequence_parameter_set_extensions, [vec![0x6d]]);
    assert_eq!(AvcConfig::parse(&config.normalize()?)?, config);
    Ok(())
}

#[test]
fn normalizes_hevc_vps_sps_pps_arrays() -> Result<(), Box<dyn std::error::Error>> {
    let config = HevcConfig {
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
    let normalized = config.normalize()?;
    assert_eq!(HevcConfig::parse(&normalized)?, config);
    Ok(())
}

#[test]
fn parses_av1_main10_with_config_obu() -> Result<(), Box<dyn std::error::Error>> {
    let raw = [0x81, 8, 0x4c, 0, 0x0a, 1, 0x80];
    let config = Av1Config::parse(&raw)?;
    assert_eq!(config.sequence_profile, 0);
    assert!(config.high_bitdepth);
    assert!(config.chroma_subsampling_x);
    assert!(config.chroma_subsampling_y);
    assert_eq!(config.config_obus, [0x0a, 1, 0x80]);
    assert_eq!(config.normalize()?, raw);
    Ok(())
}

#[test]
fn parses_aac_lc_and_explicit_he_aac() -> Result<(), Box<dyn std::error::Error>> {
    let lc = AacConfig::parse_audio_specific_config(&[0x12, 0x10])?;
    assert_eq!(lc.object_type, 2);
    assert_eq!(lc.sample_rate, 44_100);
    assert_eq!(lc.output_sample_rate, 44_100);
    assert_eq!(lc.channels, 2);
    assert_eq!(lc.frame_length, 1024);

    let he = AacConfig::parse_audio_specific_config(&[0x2b, 0x92, 0x08, 0x00])?;
    assert_eq!(he.object_type, 2);
    assert_eq!(he.extension_object_type, Some(5));
    assert_eq!(he.sample_rate, 22_050);
    assert_eq!(he.output_sample_rate, 44_100);
    assert!(he.sbr);
    assert_eq!(
        he.normalize_audio_specific_config(),
        [0x2b, 0x92, 0x08, 0x00]
    );
    Ok(())
}

#[test]
fn extracts_aac_from_esds_descriptors() -> Result<(), Box<dyn std::error::Error>> {
    let esds = [
        0, 0, 0, 0, // FullBox version and flags.
        3, 22, 0, 1, 0, // ES descriptor, id, flags.
        4, 17, 0x40, 0x15, 0, 0, 0, // Decoder config and buffer size.
        0, 1, 0xf4, 0, // max bitrate 128000.
        0, 1, 0x77, 0, // average bitrate 96000.
        5, 2, 0x12, 0x10, // AAC-LC AudioSpecificConfig.
    ];
    let config = EsdsConfig::parse(&esds)?;
    assert_eq!(config.es_id, 1);
    assert_eq!(config.object_type_indication, 0x40);
    assert_eq!(config.stream_type, 5);
    assert_eq!(config.max_bitrate, 128_000);
    assert_eq!(config.average_bitrate, 96_000);
    assert_eq!(config.aac.channels, 2);
    assert_eq!(EsdsConfig::parse(&config.normalize()?)?, config);
    Ok(())
}

#[test]
fn converts_stereo_dops_to_opus_head() -> Result<(), Box<dyn std::error::Error>> {
    let dops = [0, 2, 0x01, 0x38, 0, 0, 0xbb, 0x80, 0, 0, 0];
    let config = OpusConfig::parse_dops(&dops)?;
    assert_eq!(config.pre_skip, 312);
    assert_eq!(config.input_sample_rate, 48_000);
    assert_eq!(config.normalize_dops()?, dops);
    assert_eq!(
        config.opus_head(),
        [
            b'O', b'p', b'u', b's', b'H', b'e', b'a', b'd', 1, 2, 0x38, 0x01, 0x80, 0xbb, 0, 0, 0,
            0, 0,
        ]
    );
    assert_eq!(OpusConfig::SEEK_PREROLL_SAMPLES, 3_840);
    Ok(())
}

#[test]
fn parses_multichannel_opus_mapping() -> Result<(), Box<dyn std::error::Error>> {
    let dops = [
        0, 6, 0, 0, 0, 0, 0xbb, 0x80, 0, 0, 1, 4, 2, 0, 1, 2, 3, 4, 5,
    ];
    let config = OpusConfig::parse_dops(&dops)?;
    assert_eq!(config.channel_mapping_family, 1);
    assert_eq!(config.mapping_data, [4, 2, 0, 1, 2, 3, 4, 5]);
    assert_eq!(config.normalize_dops()?, dops);
    Ok(())
}

#[test]
fn rejects_malformed_codec_configurations() {
    let cases = [
        AvcConfig::parse(&[1, 66, 0]).map(|_| ()),
        AvcConfig::parse(&[2, 66, 0, 30, 0xff, 0xe0, 0]).map(|_| ()),
        HevcConfig::parse(&[1; 10]).map(|_| ()),
        Av1Config::parse(&[0x01, 0, 0, 0]).map(|_| ()),
        AacConfig::parse_audio_specific_config(&[0x12]).map(|_| ()),
        OpusConfig::parse_dops(&[1, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0]).map(|_| ()),
    ];
    assert!(cases.into_iter().all(|result| result.is_err()));
}

#[test]
fn reports_pce_and_mapping_table_as_typed_errors() {
    let pce = AacConfig::parse_audio_specific_config(&[0x12, 0x00]);
    assert_eq!(
        pce.err().map(|error| error.kind()),
        Some(Mp4ErrorKind::Unsupported)
    );

    let mapped = OpusConfig::parse_dops(&[0, 6, 0, 0, 0, 0, 0, 0, 0, 0, 1]);
    assert_eq!(
        mapped.err().map(|error| error.kind()),
        Some(Mp4ErrorKind::Truncated)
    );
}

#[test]
fn rejects_reserved_bits_rates_and_missing_descriptors() {
    let avc_result = AvcConfig::parse(&[1, 66, 0, 30, 0x03, 0xe0, 0]);
    assert_eq!(
        avc_result.err().map(|error| error.kind()),
        Some(Mp4ErrorKind::InvalidData)
    );

    let hevc_result = HevcConfig::parse(&[
        1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 120, 0, 0, 0xfc, 0xfd, 0xf8, 0xf8, 0, 0, 0x0f, 0,
    ]);
    assert_eq!(
        hevc_result.err().map(|error| error.kind()),
        Some(Mp4ErrorKind::InvalidData)
    );

    let sequence_config_result = Av1Config::parse(&[0x81, 8, 0x0c, 0x20]);
    assert_eq!(
        sequence_config_result.err().map(|error| error.kind()),
        Some(Mp4ErrorKind::InvalidData)
    );

    let reserved_aac_rate = AacConfig::parse_audio_specific_config(&[0x16, 0x90]);
    assert_eq!(
        reserved_aac_rate.err().map(|error| error.kind()),
        Some(Mp4ErrorKind::InvalidData)
    );

    let missing_decoder_descriptor = EsdsConfig::parse(&[0, 0, 0, 0, 3, 3, 0, 1, 0]);
    assert!(missing_decoder_descriptor.is_err());
}
