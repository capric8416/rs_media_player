use mp4_demux::{EditList, Mp4ErrorKind};

fn full_box(version: u8, body: &[u8]) -> Vec<u8> {
    let mut bytes = vec![version, 0, 0, 0];
    bytes.extend(body);
    bytes
}

fn entry_v0(duration: u32, media_time: i32, rate: (i16, i16)) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend(duration.to_be_bytes());
    bytes.extend(media_time.to_be_bytes());
    bytes.extend(rate.0.to_be_bytes());
    bytes.extend(rate.1.to_be_bytes());
    bytes
}

#[test]
fn parses_empty_and_media_edits_and_maps_pts_with_timescales()
-> Result<(), Box<dyn std::error::Error>> {
    let mut body = Vec::new();
    body.extend(2_u32.to_be_bytes());
    body.extend(entry_v0(500, -1, (1, 0)));
    body.extend(entry_v0(2000, 1000, (1, 0)));
    let edits = EditList::parse(&full_box(0, &body))?;
    assert_eq!(edits.entries.len(), 2);
    assert_eq!(edits.map_presentation_time(999, 1000, 1000)?, None);
    assert_eq!(edits.map_presentation_time(1000, 1000, 1000)?, Some(500));
    assert_eq!(edits.map_presentation_time(1500, 1000, 1000)?, Some(1000));
    assert_eq!(edits.map_presentation_time(3000, 1000, 1000)?, None);

    let mut scaled = Vec::new();
    scaled.extend(1_u32.to_be_bytes());
    scaled.extend(entry_v0(1000, 0, (1, 0)));
    assert_eq!(
        EditList::parse(&full_box(0, &scaled))?.map_presentation_time(24000, 48000, 1000)?,
        Some(500)
    );
    Ok(())
}

#[test]
fn supports_version_one_and_rejects_invalid_edits() -> Result<(), Box<dyn std::error::Error>> {
    let mut body = Vec::new();
    body.extend(1_u32.to_be_bytes());
    body.extend(5_000_000_000_u64.to_be_bytes());
    body.extend(4_000_000_000_i64.to_be_bytes());
    body.extend(1_i16.to_be_bytes());
    body.extend(0_i16.to_be_bytes());
    let edits = EditList::parse(&full_box(1, &body))?;
    assert_eq!(edits.map_presentation_time(4_000_000_000, 1, 1)?, Some(0));

    let mut non_unit = Vec::new();
    non_unit.extend(1_u32.to_be_bytes());
    non_unit.extend(entry_v0(100, 0, (2, 0)));
    let non_unit = EditList::parse(&full_box(0, &non_unit))?;
    assert_eq!(non_unit.map_presentation_time(50, 1, 1)?, Some(50));
    assert_eq!(non_unit.entries[0].media_rate_integer, 2);
    let mut negative_time = Vec::new();
    negative_time.extend(1_u32.to_be_bytes());
    negative_time.extend(entry_v0(100, -2, (1, 0)));
    let negative_time = EditList::parse(&full_box(0, &negative_time))?;
    assert_eq!(negative_time.entries[0].media_time, -2);
    assert_eq!(negative_time.map_presentation_time(0, 1, 1)?, Some(2));
    assert_eq!(
        EditList::parse(&[0; 7]).err().map(|e| e.kind()),
        Some(Mp4ErrorKind::Truncated)
    );
    assert_eq!(
        EditList::parse(&[2, 0, 0, 0, 0, 0, 0, 0])
            .err()
            .map(|e| e.kind()),
        Some(Mp4ErrorKind::Unsupported)
    );
    assert_eq!(
        edits.map_presentation_time(0, 0, 1).err().map(|e| e.kind()),
        Some(Mp4ErrorKind::InvalidData)
    );
    Ok(())
}

#[test]
fn rounds_timebase_conversion_to_nearest_away_from_zero() -> Result<(), Box<dyn std::error::Error>>
{
    let mut body = Vec::new();
    body.extend(1_u32.to_be_bytes());
    body.extend(entry_v0(3, 0, (1, 0)));
    let edits = EditList::parse(&full_box(0, &body))?;
    assert_eq!(edits.map_presentation_time(1, 2, 3)?, Some(2));
    Ok(())
}
