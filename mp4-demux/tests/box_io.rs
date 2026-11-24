use std::io::Cursor;

use mp4_demux::{
    BoxHeader, FourCc, HeaderParse, IncrementalBoxReader, Mp4Error, Mp4ErrorKind, Mp4Result,
    SeekBoxReader, parse_box_header,
};

fn standard_box(box_type: [u8; 4], payload: &[u8]) -> Mp4Result<Vec<u8>> {
    let size = u32::try_from(8_usize.checked_add(payload.len()).ok_or_else(|| {
        Mp4Error::new(
            Mp4ErrorKind::Overflow,
            "standard_box",
            0,
            "test box length overflowed",
        )
    })?)
    .map_err(|_| {
        Mp4Error::new(
            Mp4ErrorKind::Overflow,
            "standard_box",
            0,
            "test box does not fit a 32-bit size",
        )
    })?;
    let mut bytes = Vec::with_capacity(size as usize);
    bytes.extend_from_slice(&size.to_be_bytes());
    bytes.extend_from_slice(&box_type);
    bytes.extend_from_slice(payload);
    Ok(bytes)
}

fn complete_header(input: &[u8], offset: u64, container_end: Option<u64>) -> Mp4Result<BoxHeader> {
    match parse_box_header(input, offset, container_end)? {
        HeaderParse::Complete(header) => Ok(header),
        state => Err(Mp4Error::new(
            Mp4ErrorKind::InvalidState,
            "complete_header",
            offset,
            format!("expected complete header, got {state:?}"),
        )),
    }
}

#[test]
fn parses_standard_and_extended_sizes() -> Mp4Result<()> {
    let standard = standard_box(*b"free", &[1, 2, 3, 4])?;
    let standard_header = complete_header(&standard, 0, Some(standard.len() as u64))?;
    assert_eq!(standard_header.box_type(), FourCc::new(*b"free"));
    assert_eq!(standard_header.header_size(), 8);
    assert_eq!(standard_header.payload_size(), 4);
    assert_eq!(standard_header.declared_size(), Some(12));

    let mut extended = Vec::new();
    extended.extend_from_slice(&1_u32.to_be_bytes());
    extended.extend_from_slice(b"uuid");
    extended.extend_from_slice(&24_u64.to_be_bytes());
    let extended_header = complete_header(&extended, 100, Some(124))?;
    assert_eq!(extended_header.header_size(), 16);
    assert_eq!(extended_header.payload_size(), 8);
    assert_eq!(extended_header.end(), 124);
    Ok(())
}

#[test]
fn size_zero_waits_for_and_uses_the_parent_end() -> Mp4Result<()> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&0_u32.to_be_bytes());
    bytes.extend_from_slice(b"mdat");
    assert_eq!(
        parse_box_header(&bytes, 32, None)?,
        HeaderParse::NeedsContainerEnd
    );
    let header = complete_header(&bytes, 32, Some(80))?;
    assert!(header.extends_to_parent_end());
    assert_eq!(header.effective_size(), 48);
    assert_eq!(header.payload_size(), 40);
    Ok(())
}

#[test]
fn declared_child_size_is_clamped_to_parent() -> Mp4Result<()> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&100_u32.to_be_bytes());
    bytes.extend_from_slice(b"free");
    let header = complete_header(&bytes, 10, Some(30))?;
    assert_eq!(header.declared_size(), Some(100));
    assert_eq!(header.effective_size(), 20);
    assert_eq!(header.payload_size(), 12);
    assert!(header.was_clamped());
    Ok(())
}

#[test]
fn invalid_sizes_and_offset_overflow_are_typed() -> Mp4Result<()> {
    let mut too_small = Vec::new();
    too_small.extend_from_slice(&4_u32.to_be_bytes());
    too_small.extend_from_slice(b"free");
    let size_error = parse_box_header(&too_small, 0, Some(8))
        .err()
        .ok_or_else(|| {
            Mp4Error::new(
                Mp4ErrorKind::InvalidState,
                "invalid_sizes_and_offset_overflow_are_typed",
                0,
                "undersized box unexpectedly parsed",
            )
        })?;
    assert_eq!(size_error.kind(), Mp4ErrorKind::InvalidSize);

    let valid = standard_box(*b"free", &[])?;
    let overflow_error = parse_box_header(&valid, u64::MAX - 4, None)
        .err()
        .ok_or_else(|| {
            Mp4Error::new(
                Mp4ErrorKind::InvalidState,
                "invalid_sizes_and_offset_overflow_are_typed",
                0,
                "overflowing box unexpectedly parsed",
            )
        })?;
    assert_eq!(overflow_error.kind(), Mp4ErrorKind::Overflow);
    Ok(())
}

#[test]
fn seek_reader_supports_bounded_nested_traversal() -> Mp4Result<()> {
    let child = standard_box(*b"free", &[9, 8, 7, 6])?;
    let outer = standard_box(*b"moov", &child)?;
    let mut reader = SeekBoxReader::new(Cursor::new(outer))?;

    let parent = reader.next_top_level_box()?.ok_or_else(|| {
        Mp4Error::new(
            Mp4ErrorKind::InvalidState,
            "seek_reader_supports_bounded_nested_traversal",
            0,
            "missing parent box",
        )
    })?;
    let parent_end = reader.enter_children(parent)?;
    let child_header = reader.next_box(parent_end)?.ok_or_else(|| {
        Mp4Error::new(
            Mp4ErrorKind::InvalidState,
            "seek_reader_supports_bounded_nested_traversal",
            8,
            "missing child box",
        )
    })?;
    let payload = reader.read_payload_to_vec(child_header, 4)?;
    assert_eq!(payload, [9, 8, 7, 6]);
    assert!(reader.next_box(parent_end)?.is_none());
    assert!(reader.next_top_level_box()?.is_none());
    Ok(())
}

#[test]
fn seek_reader_requires_payload_resolution_before_sibling() -> Mp4Result<()> {
    let mut stream = standard_box(*b"free", &[1, 2, 3, 4])?;
    stream.extend_from_slice(&standard_box(*b"skip", &[])?);
    let mut reader = SeekBoxReader::new(Cursor::new(stream))?;
    let first = reader.next_top_level_box()?.ok_or_else(|| {
        Mp4Error::new(
            Mp4ErrorKind::InvalidState,
            "seek_reader_requires_payload_resolution_before_sibling",
            0,
            "missing first box",
        )
    })?;
    let state_error = reader.next_top_level_box().err().ok_or_else(|| {
        Mp4Error::new(
            Mp4ErrorKind::InvalidState,
            "seek_reader_requires_payload_resolution_before_sibling",
            8,
            "reader accepted a sibling before resolving payload",
        )
    })?;
    assert_eq!(state_error.kind(), Mp4ErrorKind::InvalidState);
    reader.skip_payload(first)?;
    assert!(reader.next_top_level_box()?.is_some());
    Ok(())
}

#[test]
fn seek_payload_read_stops_at_box_boundary() -> Mp4Result<()> {
    let mut stream = standard_box(*b"free", &[1, 2, 3, 4])?;
    stream.extend_from_slice(&standard_box(*b"skip", &[5, 6])?);
    let mut reader = SeekBoxReader::new(Cursor::new(stream))?;
    let first = reader.next_top_level_box()?.ok_or_else(|| {
        Mp4Error::new(
            Mp4ErrorKind::InvalidState,
            "seek_payload_read_stops_at_box_boundary",
            0,
            "missing first box",
        )
    })?;
    let mut output = [0_u8; 16];
    let read = reader.read_payload(first, &mut output)?;
    assert_eq!(read, 4);
    assert_eq!(&output[..read], [1, 2, 3, 4]);
    let second = reader.next_top_level_box()?.ok_or_else(|| {
        Mp4Error::new(
            Mp4ErrorKind::InvalidState,
            "seek_payload_read_stops_at_box_boundary",
            12,
            "payload read crossed the sibling header",
        )
    })?;
    assert_eq!(second.box_type(), FourCc::new(*b"skip"));
    Ok(())
}

#[test]
fn whole_payload_allocation_requires_sufficient_limit() -> Mp4Result<()> {
    let stream = standard_box(*b"mdat", &[1, 2, 3, 4])?;
    let mut reader = SeekBoxReader::new(Cursor::new(stream))?;
    let header = reader.next_top_level_box()?.ok_or_else(|| {
        Mp4Error::new(
            Mp4ErrorKind::InvalidState,
            "whole_payload_allocation_requires_sufficient_limit",
            0,
            "missing mdat box",
        )
    })?;
    let error = reader.read_payload_to_vec(header, 3).err().ok_or_else(|| {
        Mp4Error::new(
            Mp4ErrorKind::InvalidState,
            "whole_payload_allocation_requires_sufficient_limit",
            8,
            "allocation limit unexpectedly accepted the payload",
        )
    })?;
    assert_eq!(error.kind(), Mp4ErrorKind::BufferLimit);
    reader.skip_payload(header)?;
    Ok(())
}

#[test]
fn incremental_reader_handles_split_header_and_payload() -> Mp4Result<()> {
    let bytes = standard_box(*b"free", &[1, 2, 3, 4])?;
    let mut reader = IncrementalBoxReader::new(32)?;
    reader.push_bytes(&bytes[..4])?;
    assert!(reader.next_box()?.is_none());
    reader.push_bytes(&bytes[4..10])?;
    let header = reader.next_box()?.ok_or_else(|| {
        Mp4Error::new(
            Mp4ErrorKind::InvalidState,
            "incremental_reader_handles_split_header_and_payload",
            0,
            "complete header was not emitted",
        )
    })?;
    assert_eq!(header.payload_size(), 4);
    assert_eq!(reader.payload_chunk(), [1, 2]);
    reader.consume_payload(2)?;
    reader.push_bytes(&bytes[10..])?;
    assert_eq!(reader.payload_chunk(), [3, 4]);
    reader.consume_payload(2)?;
    reader.finish()?;
    assert!(reader.next_box()?.is_none());
    assert!(reader.is_eof());
    Ok(())
}

#[test]
fn incremental_size_zero_resolves_at_end_of_stream() -> Mp4Result<()> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&0_u32.to_be_bytes());
    bytes.extend_from_slice(b"mdat");
    bytes.extend_from_slice(&[5, 6, 7]);
    let mut reader = IncrementalBoxReader::new(32)?;
    reader.push_bytes(&bytes)?;
    assert!(reader.next_box()?.is_none());
    reader.finish()?;
    let header = reader.next_box()?.ok_or_else(|| {
        Mp4Error::new(
            Mp4ErrorKind::InvalidState,
            "incremental_size_zero_resolves_at_end_of_stream",
            0,
            "size-zero box did not resolve at end-of-stream",
        )
    })?;
    assert!(header.extends_to_parent_end());
    assert_eq!(reader.payload_chunk(), [5, 6, 7]);
    reader.consume_payload(3)?;
    assert!(reader.is_eof());
    Ok(())
}

#[test]
fn incremental_finish_detects_truncated_payload() -> Mp4Result<()> {
    let bytes = standard_box(*b"free", &[1, 2, 3, 4])?;
    let mut reader = IncrementalBoxReader::new(32)?;
    reader.push_bytes(&bytes[..10])?;
    assert!(reader.next_box()?.is_some());
    let error = reader.finish().err().ok_or_else(|| {
        Mp4Error::new(
            Mp4ErrorKind::InvalidState,
            "incremental_finish_detects_truncated_payload",
            10,
            "truncated payload unexpectedly finished",
        )
    })?;
    assert_eq!(error.kind(), Mp4ErrorKind::Truncated);
    Ok(())
}

#[test]
fn incremental_eof_detects_partial_extended_header() -> Mp4Result<()> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&1_u32.to_be_bytes());
    bytes.extend_from_slice(b"free");
    bytes.extend_from_slice(&[0_u8; 4]);
    let mut reader = IncrementalBoxReader::new(32)?;
    reader.push_bytes(&bytes)?;
    assert!(reader.next_box()?.is_none());
    reader.finish()?;
    let error = reader.next_box().err().ok_or_else(|| {
        Mp4Error::new(
            Mp4ErrorKind::InvalidState,
            "incremental_eof_detects_partial_extended_header",
            0,
            "partial extended header unexpectedly parsed",
        )
    })?;
    assert_eq!(error.kind(), Mp4ErrorKind::Truncated);
    Ok(())
}

#[test]
fn incremental_buffer_limit_is_enforced() -> Mp4Result<()> {
    let mut reader = IncrementalBoxReader::new(8)?;
    let error = reader.push_bytes(&[0_u8; 9]).err().ok_or_else(|| {
        Mp4Error::new(
            Mp4ErrorKind::InvalidState,
            "incremental_buffer_limit_is_enforced",
            0,
            "buffer limit unexpectedly accepted excess data",
        )
    })?;
    assert_eq!(error.kind(), Mp4ErrorKind::BufferLimit);
    Ok(())
}
