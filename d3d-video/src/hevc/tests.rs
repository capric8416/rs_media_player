use super::NalHeader;
use super::dpb::{DecodedPictureBuffer, LongTermReference, Picture, SliceType};
use super::parameter_sets::ParameterSets;
use super::rps::{ShortTermReference, ShortTermRps};
use super::slice::ReferenceSliceHeader;
use crate::bit_reader::BitReader;
use crate::{VideoErrorKind, VideoResult};

#[derive(Default)]
struct BitWriter(Vec<bool>);

impl BitWriter {
    fn bit(&mut self, value: bool) {
        self.0.push(value);
    }
    fn bits(&mut self, value: u32, count: u8) {
        for shift in (0..count).rev() {
            self.bit(value & (1_u32 << shift) != 0);
        }
    }
    fn ue(&mut self, value: u32) {
        let code = u64::from(value) + 1;
        let width = 64 - code.leading_zeros();
        for _ in 1..width {
            self.bit(false);
        }
        for shift in (0..width).rev() {
            self.bit(code & (1_u64 << shift) != 0);
        }
    }
    fn se(&mut self, value: i32) {
        self.ue(if value <= 0 {
            value.unsigned_abs() * 2
        } else {
            value.unsigned_abs() * 2 - 1
        });
    }
    fn rbsp(mut self) -> Vec<u8> {
        self.bit(true);
        while !self.0.len().is_multiple_of(8) {
            self.bit(false);
        }
        self.0
            .as_chunks::<8>()
            .0
            .iter()
            .map(|chunk| {
                chunk
                    .iter()
                    .fold(0_u8, |byte, bit| (byte << 1) | u8::from(*bit))
            })
            .collect()
    }
}

fn ptl(bits: &mut BitWriter, profile: u32, level: u32) {
    bits.bits(0, 2);
    bits.bit(false);
    bits.bits(profile, 5);
    for _ in 0..80 {
        bits.bit(false);
    }
    bits.bits(level, 8);
}

fn nal(unit_type: u8, payload: Vec<u8>) -> Vec<u8> {
    let mut output = vec![unit_type << 1, 1];
    output.extend(payload);
    output
}

fn vps_nal() -> Vec<u8> {
    let mut bits = BitWriter::default();
    bits.bits(0, 4);
    bits.bits(3, 2);
    bits.bits(0, 6);
    bits.bits(0, 3);
    bits.bit(true);
    bits.bits(0xffff, 16);
    ptl(&mut bits, 1, 120);
    bits.bit(true);
    bits.ue(3);
    bits.ue(0);
    bits.ue(0);
    nal(32, bits.rbsp())
}

fn sps_nal() -> Vec<u8> {
    sps_nal_with_crop([0, 0, 0, 4])
}

fn sps_nal_with_crop(crop: [u32; 4]) -> Vec<u8> {
    let mut bits = BitWriter::default();
    bits.bits(0, 4);
    bits.bits(0, 3);
    bits.bit(true);
    ptl(&mut bits, 2, 120);
    bits.ue(0);
    bits.ue(1);
    bits.ue(1920);
    bits.ue(1088);
    bits.bit(true);
    for value in crop {
        bits.ue(value);
    }
    bits.ue(2);
    bits.ue(2);
    bits.ue(4);
    bits.bit(true);
    bits.ue(3);
    bits.ue(2);
    bits.ue(0);
    bits.ue(0);
    bits.ue(3);
    bits.ue(0);
    bits.ue(3);
    bits.ue(0);
    bits.ue(0);
    bits.bit(false);
    bits.bit(true);
    bits.bit(true);
    bits.bit(false);
    bits.ue(1);
    bits.ue(1);
    bits.ue(1);
    bits.ue(0);
    bits.bit(true);
    bits.ue(1);
    bits.bit(false);
    bits.bit(true);
    bits.ue(1);
    bits.bits(3, 8);
    bits.bit(true);
    bits.bit(true);
    bits.bit(true);
    nal(33, bits.rbsp())
}

#[test]
fn state_reports_hevc_conformance_rectangle() -> VideoResult<()> {
    let state =
        super::HevcDecodeState::new(&vps_nal(), &sps_nal_with_crop([2, 4, 3, 1]), &pps_nal())?;
    let rect = state.visible_rect()?;
    assert_eq!(
        (rect.x, rect.y, rect.size.width(), rect.size.height()),
        (4, 6, 1908, 1080)
    );
    Ok(())
}

#[test]
fn rejects_overflowing_hevc_conformance_window() -> VideoResult<()> {
    let mut sets = ParameterSets::default();
    sets.insert_vps_nal(&vps_nal())?;
    assert!(
        sets.insert_sps_nal(&sps_nal_with_crop([0x8000_0000, 0, 0, 0]))
            .is_err()
    );
    assert!(
        sets.insert_sps_nal(&sps_nal_with_crop([0x7fff_ffff, 1, 0, 0]))
            .is_err()
    );
    Ok(())
}

fn pps_nal() -> Vec<u8> {
    let mut bits = BitWriter::default();
    bits.ue(0);
    bits.ue(0);
    bits.bit(false);
    bits.bit(false);
    bits.bits(0, 3);
    bits.bit(true);
    bits.bit(true);
    bits.ue(0);
    bits.ue(0);
    bits.se(0);
    bits.bit(false);
    bits.bit(true);
    bits.bit(false);
    bits.se(0);
    bits.se(0);
    bits.bit(false);
    bits.bit(true);
    bits.bit(true);
    bits.bit(false);
    bits.bit(false);
    bits.bit(true);
    bits.bit(true);
    bits.bit(false);
    bits.bit(false);
    bits.bit(true);
    bits.ue(0);
    bits.bit(false);
    bits.bit(false);
    nal(34, bits.rbsp())
}

#[test]
fn serializes_dxva2_hevc_idr_buffers() -> VideoResult<()> {
    let vps = vps_nal();
    let sps = sps_nal();
    let pps = pps_nal();
    let mut bits = BitWriter::default();
    bits.bit(true); // first_slice_segment_in_pic_flag
    bits.bit(true); // no_output_of_prior_pics_flag
    bits.ue(0); // PPS id
    bits.ue(2); // I slice
    bits.bit(true); // SAO luma
    bits.bit(true); // SAO chroma
    let slice = nal(19, bits.rbsp());

    let picture = super::dxva::idr_picture_parameters(&vps, &sps, &pps, &slice, 7)?;
    assert_eq!(picture.len(), 232);
    assert_eq!(picture[0..2], [240, 0]); // 1920 pixels / 8-pixel minimum CB.
    assert_eq!(picture[6], 7);
    assert_eq!(
        u32::from_le_bytes(
            picture[24..28].try_into().map_err(|_| {
                super::hevc_error("test", "DXVA HEVC coding flags are truncated")
            })?
        ),
        0x060e_0006
    ); // FFmpeg DXVA bit layout: AMP, SAO, LT refs, temporal MVP, smoothing, sign hiding, CABAC.
    assert_eq!(picture[160..162], [0; 2]); // POC list starts with zero for IDR.
    assert_eq!(&picture[200..224], &[0xff; 24]); // FFmpeg initializes unused RPS entries to 0xff.
    assert_eq!(super::dxva::idr_quant_matrix(&vps, &sps, &pps)?.len(), 1000);
    let invalid_surface = super::dxva::idr_picture_parameters(&vps, &sps, &pps, &slice, 128)
        .err()
        .ok_or_else(|| super::hevc_error("test", "out-of-range DXVA surface passed"))?;
    assert_eq!(invalid_surface.kind(), VideoErrorKind::InvalidInput);
    Ok(())
}

#[test]
fn hevc_state_commits_idr_surface_only_after_prepared_picture_commit() -> VideoResult<()> {
    let vps = vps_nal();
    let sps = sps_nal();
    let pps = pps_nal();
    let mut bits = BitWriter::default();
    bits.bit(true); // first_slice_segment_in_pic_flag
    bits.bit(true); // no_output_of_prior_pics_flag
    bits.ue(0); // PPS id
    bits.ue(2); // I slice
    bits.bit(true); // SAO luma
    bits.bit(true); // SAO chroma
    let slice = nal(19, bits.rbsp());
    let mut sets = ParameterSets::default();
    sets.insert_vps_nal(&vps)?;
    let parsed_sps = sets.insert_sps_nal(&sps)?.clone();
    sets.insert_pps_nal(&pps)?;
    let address_bits = u8::try_from((parsed_sps.ctb_count() - 1).bit_width())
        .map_err(|_| super::hevc_error("test", "CTB address width exceeds u8"))?;
    let mut next_bits = BitWriter::default();
    next_bits.bit(false); // not first slice in picture
    next_bits.bit(true); // no_output_of_prior_pics_flag
    next_bits.ue(0); // PPS id
    next_bits.bits(1, address_bits); // slice_segment_address
    next_bits.ue(2); // I slice
    next_bits.bit(true); // SAO luma
    next_bits.bit(true); // SAO chroma
    let next_slice = nal(19, next_bits.rbsp());

    let mut state = super::HevcDecodeState::new(&vps, &sps, &pps)?;
    let prepared = state.prepare(&[&slice, &next_slice], 4)?;
    assert_eq!(state.reference_surfaces(), []);
    assert_eq!(prepared.picture_parameters.len(), 232);
    state.commit(prepared);
    assert_eq!(state.reference_surfaces(), &[(1, 4)]);
    state.reset();
    assert_eq!(state.reference_surfaces(), []);
    Ok(())
}

#[test]
fn maps_live_hevc_dpb_surfaces_into_dxva_reference_fields() -> VideoResult<()> {
    let mut dpb = DecodedPictureBuffer::default();
    dpb.insert_current(
        Picture {
            id: 11,
            poc: 4,
            temporal_id: 0,
            long_term: false,
        },
        4,
    )?;
    dpb.insert_current(
        Picture {
            id: 12,
            poc: -2,
            temporal_id: 0,
            long_term: true,
        },
        4,
    )?;
    let references = super::dxva::dxva_references(&dpb, &[(11, 5), (12, 9)])?;
    assert_eq!(references.len(), 2);
    assert_eq!(references[0].surface_index, 5);
    assert_eq!(references[0].poc, 4);
    assert!(!references[0].long_term);
    assert_eq!(references[1].surface_index, 9);
    assert!(references[1].long_term);

    let short = ShortTermRps {
        references: vec![
            ShortTermReference {
                delta_poc: -1,
                used: true,
            },
            ShortTermReference {
                delta_poc: 2,
                used: false,
            },
        ],
        negative_count: 1,
    };
    let long = [LongTermReference {
        poc: -2,
        used: true,
    }];
    let sets = super::dxva::dxva_reference_sets(&dpb, &[(11, 5), (12, 9)], 5, &short, &long)?;
    assert_eq!(sets, [vec![5], vec![], vec![9]]);

    let missing = super::dxva::dxva_references(&dpb, &[(11, 5)])
        .err()
        .ok_or_else(|| super::hevc_error("test", "missing DPB surface mapping passed"))?;
    assert_eq!(missing.kind(), VideoErrorKind::InvalidInput);
    Ok(())
}

#[test]
fn serializes_hevc_non_irap_picture_from_dpb_and_parsed_rps() -> VideoResult<()> {
    let vps = vps_nal();
    let sps = sps_nal();
    let pps = pps_nal();
    let mut slice_bits = BitWriter::default();
    slice_bits.bit(true); // first_slice_segment_in_pic_flag
    slice_bits.ue(0); // PPS id
    slice_bits.ue(1); // P slice
    slice_bits.bits(5, 8); // POC LSB
    slice_bits.bit(true); // select short-term RPS from SPS
    slice_bits.ue(1); // use the SPS long-term reference
    slice_bits.ue(0); // no slice-signalled long-term references
    slice_bits.bit(false); // delta POC MSB absent
    slice_bits.bit(true); // temporal MVP enabled
    slice_bits.bit(true); // SAO luma
    slice_bits.bit(true); // SAO chroma
    slice_bits.bit(false); // use default active reference count
    slice_bits.bit(false); // default reference-list order
    slice_bits.bit(false); // CABAC init
    let slice = nal(1, slice_bits.rbsp());

    let mut sets = ParameterSets::default();
    sets.insert_vps_nal(&vps)?;
    sets.insert_sps_nal(&sps)?;
    sets.insert_pps_nal(&pps)?;
    let header = ReferenceSliceHeader::parse_nal(&slice, &sets, 0)?;
    assert_eq!(header.slice_type, SliceType::P);
    assert_eq!(header.poc_lsb, 5);

    let mut dpb = DecodedPictureBuffer::default();
    for (id, poc, long_term) in [(21, 4, false), (22, 7, false), (23, 3, true)] {
        dpb.insert_current(
            Picture {
                id,
                poc,
                temporal_id: 0,
                long_term,
            },
            4,
        )?;
    }
    dpb.apply_reference_set(5, &header.short_term_rps, &header.long_term_rps)?;
    let bindings = [(21, 5), (22, 6), (23, 9)];
    let references = super::dxva::dxva_references(&dpb, &bindings)?;
    let sets = super::dxva::dxva_reference_sets(
        &dpb,
        &bindings,
        5,
        &header.short_term_rps,
        &header.long_term_rps,
    )?;
    assert_eq!(sets, [vec![5], vec![], vec![9]]);
    let picture = super::dxva::picture_parameters(
        &vps,
        &sps,
        &pps,
        &slice,
        7,
        5,
        &references,
        [&sets[0], &sets[1], &sets[2]],
    )?;
    assert_eq!(picture.len(), 232);
    assert_eq!(&picture[120..124], &5_i32.to_le_bytes());
    assert_eq!(&picture[124..127], &[5, 6, 0x89]);
    assert_eq!(&picture[140..144], &4_i32.to_le_bytes());
    assert_eq!(&picture[144..148], &7_i32.to_le_bytes());
    assert_eq!(&picture[148..152], &3_i32.to_le_bytes());
    assert_eq!(&picture[200..203], &[0, 0xff, 0xff]);
    assert_eq!(&picture[216..218], &[2, 0xff]);
    let flags = u32::from_le_bytes(
        picture[32..36]
            .try_into()
            .map_err(|_| super::hevc_error("test", "invalid HEVC picture flags"))?,
    );
    assert_eq!(flags & (1 << 17 | 1 << 18), 0);
    Ok(())
}

#[test]
fn validates_two_byte_nal_header() -> VideoResult<()> {
    let header = NalHeader::parse([0x42, 0x01])?;
    assert_eq!(header.unit_type, 33);
    assert_eq!(header.layer_id, 0);
    assert_eq!(header.temporal_id, 0);
    for invalid in [[0x80, 1], [0, 0]] {
        let error = NalHeader::parse(invalid)
            .err()
            .ok_or_else(|| super::hevc_error("test", "invalid header passed"))?;
        assert_eq!(error.kind(), VideoErrorKind::InvalidInput);
    }
    Ok(())
}

#[test]
fn parses_main10_parameter_sets() -> VideoResult<()> {
    let mut sets = ParameterSets::default();
    let vps = sets.insert_vps_nal(&vps_nal())?;
    assert_eq!(vps.profile.profile_idc, 1);
    assert_eq!(vps.max_dec_pic_buffering, [4]);
    let sps = sets.insert_sps_nal(&sps_nal())?;
    assert_eq!(sps.profile.profile_idc, 2);
    assert_eq!(sps.bit_depth_luma, 10);
    assert_eq!(sps.display_width(), 1920);
    assert_eq!(sps.display_height(), 1080);
    assert_eq!(sps.log2_ctb_size, 6);
    assert_eq!(sps.ctb_count(), 510);
    assert_eq!(sps.short_term_rps[0].references.len(), 2);
    let pps = sets.insert_pps_nal(&pps_nal())?;
    assert!(pps.cabac_init_present);
    assert!(pps.transform_skip_enabled);
    assert!(pps.weighted_pred);
    assert!(pps.weighted_bipred);
    assert!(pps.entropy_coding_sync_enabled);
    assert!(pps.lists_modification_present);
    let (_, resolved) = sets.resolve_pps(0)?;
    assert_eq!(resolved.id, 0);
    Ok(())
}

#[test]
fn decodes_explicit_and_predicted_short_term_rps() -> VideoResult<()> {
    let mut explicit_bits = BitWriter::default();
    explicit_bits.ue(2);
    explicit_bits.ue(1);
    explicit_bits.ue(0);
    explicit_bits.bit(true);
    explicit_bits.ue(1);
    explicit_bits.bit(false);
    explicit_bits.ue(2);
    explicit_bits.bit(true);
    let data = explicit_bits.rbsp();
    let explicit = ShortTermRps::decode(&mut BitReader::new(&data), &[], false)?;
    assert_eq!(explicit.negative_count, 2);
    assert_eq!(
        explicit.references,
        [
            ShortTermReference {
                delta_poc: -1,
                used: true
            },
            ShortTermReference {
                delta_poc: -3,
                used: false
            },
            ShortTermReference {
                delta_poc: 3,
                used: true
            },
        ]
    );

    let mut predicted_bits = BitWriter::default();
    predicted_bits.bit(true);
    predicted_bits.bit(false);
    predicted_bits.ue(0);
    for _ in 0..=explicit.references.len() {
        predicted_bits.bit(true);
    }
    let data = predicted_bits.rbsp();
    let predicted = ShortTermRps::decode(&mut BitReader::new(&data), &[explicit], false)?;
    assert_eq!(predicted.references.len(), 4);
    assert_eq!(predicted.negative_count, 1);
    Ok(())
}

#[test]
fn computes_poc_wrap_and_bla_reset() -> VideoResult<()> {
    let mut dpb = DecodedPictureBuffer::default();
    let trail = NalHeader {
        unit_type: 1,
        layer_id: 0,
        temporal_id: 0,
    };
    assert_eq!(dpb.compute_poc(trail, 4, 14)?, -2);
    assert_eq!(dpb.compute_poc(trail, 4, 1)?, 1);
    let bla = NalHeader {
        unit_type: 16,
        layer_id: 0,
        temporal_id: 0,
    };
    assert_eq!(dpb.compute_poc(bla, 4, 14)?, 14);
    let idr = NalHeader {
        unit_type: 19,
        layer_id: 0,
        temporal_id: 0,
    };
    assert_eq!(dpb.compute_poc(idr, 4, 7)?, 0);
    Ok(())
}

#[test]
fn applies_rps_and_builds_default_modified_lists() -> VideoResult<()> {
    let mut dpb = DecodedPictureBuffer::default();
    for (id, poc) in [(1, 8), (2, 12), (3, 16)] {
        dpb.insert_current(
            Picture {
                id,
                poc,
                temporal_id: 0,
                long_term: false,
            },
            4,
        )?;
    }
    let short = ShortTermRps {
        references: vec![
            ShortTermReference {
                delta_poc: -4,
                used: true,
            },
            ShortTermReference {
                delta_poc: 4,
                used: true,
            },
        ],
        negative_count: 1,
    };
    dpb.apply_reference_set(12, &short, &[])?;
    assert_eq!(
        dpb.pictures()
            .iter()
            .map(|picture| picture.id)
            .collect::<Vec<_>>(),
        [1, 3]
    );
    let lists = dpb.build_reference_lists(12, &short, &[], SliceType::B, [3, 2], [&[], &[]])?;
    assert_eq!(lists.list0, [1, 3, 1]);
    assert_eq!(lists.list1, [3, 1]);
    let modified = dpb.build_reference_lists(12, &short, &[], SliceType::P, [1, 0], [&[1], &[]])?;
    assert_eq!(modified.list0, [3]);
    Ok(())
}

#[test]
fn tracks_long_term_and_rejects_missing_reference() -> VideoResult<()> {
    let mut dpb = DecodedPictureBuffer::default();
    dpb.insert_current(
        Picture {
            id: 9,
            poc: 20,
            temporal_id: 0,
            long_term: false,
        },
        2,
    )?;
    dpb.apply_reference_set(
        24,
        &ShortTermRps::default(),
        &[LongTermReference {
            poc: 20,
            used: true,
        }],
    )?;
    assert!(dpb.pictures()[0].long_term);
    let error = dpb
        .apply_reference_set(
            24,
            &ShortTermRps {
                references: vec![ShortTermReference {
                    delta_poc: -1,
                    used: true,
                }],
                negative_count: 1,
            },
            &[],
        )
        .err()
        .ok_or_else(|| super::hevc_error("test", "missing reference passed"))?;
    assert_eq!(error.kind(), VideoErrorKind::InvalidInput);
    Ok(())
}

#[test]
fn parses_idr_reference_slice_header() -> VideoResult<()> {
    let mut sets = ParameterSets::default();
    sets.insert_vps_nal(&vps_nal())?;
    sets.insert_sps_nal(&sps_nal())?;
    sets.insert_pps_nal(&pps_nal())?;
    let mut bits = BitWriter::default();
    bits.bit(true);
    bits.bit(false);
    bits.ue(0);
    bits.ue(2);
    bits.bit(false);
    bits.bit(false);
    let header = ReferenceSliceHeader::parse_nal(&nal(19, bits.rbsp()), &sets, 0)?;
    assert_eq!(header.slice_type, SliceType::I);
    assert!(header.first_slice_in_picture);
    assert_eq!(header.poc_lsb, 0);
    assert_eq!(header.active_references, [0, 0]);
    Ok(())
}
