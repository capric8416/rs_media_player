use super::av1_error;
use super::sequence::SequenceHeader;
use crate::VideoResult;
use crate::bit_reader::BitReader;
use crate::obu::{ObuType, parse_obu};

const MAX_TILE_WIDTH: u32 = 4096;
const MAX_TILE_AREA: u32 = 4096 * 2304;
const MAX_TILE_COLS: u32 = 64;
const MAX_TILE_ROWS: u32 = 64;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TileLayout {
    pub(crate) uniform_spacing: bool,
    pub(crate) column_starts_sb: Vec<u16>,
    pub(crate) row_starts_sb: Vec<u16>,
    pub(crate) column_widths_sb: Vec<u16>,
    pub(crate) row_heights_sb: Vec<u16>,
    pub(crate) context_update_tile_id: u16,
    pub(crate) tile_size_bytes: u8,
}

impl TileLayout {
    #[expect(
        dead_code,
        reason = "generic AV1 tile-layout parsing is needed for inter-frame headers"
    )]
    pub(crate) fn parse(
        payload: &[u8],
        sequence: &SequenceHeader,
        frame_width: u16,
        frame_height: u16,
    ) -> VideoResult<Self> {
        let mut bits = BitReader::new(payload);
        Self::parse_bits(&mut bits, sequence, frame_width, frame_height)
    }

    #[expect(
        clippy::too_many_lines,
        reason = "ordered AV1 tile-info grammar mirrors FFmpeg"
    )]
    pub(crate) fn parse_bits(
        bits: &mut BitReader<'_>,
        sequence: &SequenceHeader,
        frame_width: u16,
        frame_height: u16,
    ) -> VideoResult<Self> {
        let mi_cols = 2 * ((u32::from(frame_width) + 7) >> 3);
        let mi_rows = 2 * ((u32::from(frame_height) + 7) >> 3);
        let sb_shift = if sequence.use_128x128_superblock {
            5
        } else {
            4
        };
        let sb_cols = (mi_cols + (1 << sb_shift) - 1) >> sb_shift;
        let sb_rows = (mi_rows + (1 << sb_shift) - 1) >> sb_shift;
        let sb_size = sb_shift + 2;
        let max_width_sb = MAX_TILE_WIDTH >> sb_size;
        let mut max_area_sb = MAX_TILE_AREA >> (2 * sb_size);
        let min_cols_log2 = tile_log2(max_width_sb, sb_cols);
        let max_cols_log2 = tile_log2(1, sb_cols.min(MAX_TILE_COLS));
        let max_rows_log2 = tile_log2(1, sb_rows.min(MAX_TILE_ROWS));
        let min_tiles_log2 = min_cols_log2.max(tile_log2(max_area_sb, sb_rows * sb_cols));
        let uniform_spacing = bits.read_bit()?;

        let (column_starts_sb, column_widths_sb, columns_log2) = if uniform_spacing {
            let columns_log2 = read_increment(bits, min_cols_log2, max_cols_log2)?;
            let width = (sb_cols + (1 << columns_log2) - 1) >> columns_log2;
            uniform_axis(sb_cols, width, "TileLayout::parse columns")?.map_log2(columns_log2)
        } else {
            let mut starts = Vec::new();
            let mut widths = Vec::new();
            let mut start = 0;
            while start < sb_cols && starts.len() < usize::from(64_u8) {
                starts.push(to_u16(start, "TileLayout::parse columns")?);
                let maximum = (sb_cols - start).min(max_width_sb);
                let width = read_ns(bits, maximum)? + 1;
                widths.push(to_u16(width, "TileLayout::parse columns")?);
                start += width;
            }
            if start != sb_cols {
                return Err(av1_error(
                    "TileLayout::parse",
                    "tile columns do not cover frame",
                ));
            }
            let count = u32::try_from(starts.len())
                .map_err(|_| av1_error("TileLayout::parse", "column count exceeds u32"))?;
            let log2 = tile_log2(1, count);
            (starts, widths, log2)
        };

        let (row_starts_sb, row_heights_sb, rows_log2) = if uniform_spacing {
            let minimum = min_tiles_log2.saturating_sub(columns_log2);
            let rows_log2 = read_increment(bits, minimum, max_rows_log2)?;
            let height = (sb_rows + (1 << rows_log2) - 1) >> rows_log2;
            uniform_axis(sb_rows, height, "TileLayout::parse rows")?.map_log2(rows_log2)
        } else {
            let widest = column_widths_sb.iter().copied().max().unwrap_or(1);
            if min_tiles_log2 > 0 {
                max_area_sb = (sb_rows * sb_cols) >> (min_tiles_log2 + 1);
            } else {
                max_area_sb = sb_rows * sb_cols;
            }
            let max_height = (max_area_sb / u32::from(widest)).max(1);
            let mut starts = Vec::new();
            let mut heights = Vec::new();
            let mut start = 0;
            while start < sb_rows && starts.len() < usize::from(64_u8) {
                starts.push(to_u16(start, "TileLayout::parse rows")?);
                let maximum = (sb_rows - start).min(max_height);
                let height = read_ns(bits, maximum)? + 1;
                heights.push(to_u16(height, "TileLayout::parse rows")?);
                start += height;
            }
            if start != sb_rows {
                return Err(av1_error(
                    "TileLayout::parse",
                    "tile rows do not cover frame",
                ));
            }
            let count = u32::try_from(starts.len())
                .map_err(|_| av1_error("TileLayout::parse", "row count exceeds u32"))?;
            let log2 = tile_log2(1, count);
            (starts, heights, log2)
        };

        let id_bits = columns_log2 + rows_log2;
        let context_update_tile_id = if id_bits == 0 {
            0
        } else {
            to_u16(
                u32::try_from(bits.read_bits(to_u8(id_bits)?)?)
                    .map_err(|_| av1_error("TileLayout::parse", "tile id exceeds u32"))?,
                "TileLayout::parse",
            )?
        };
        let tile_size_bytes = if id_bits == 0 {
            1
        } else {
            u8::try_from(bits.read_bits(2)? + 1)
                .map_err(|_| av1_error("TileLayout::parse", "tile size width exceeds u8"))?
        };
        let tile_count = column_starts_sb.len() * row_starts_sb.len();
        if usize::from(context_update_tile_id) >= tile_count {
            return Err(av1_error(
                "TileLayout::parse",
                "context update tile is outside the layout",
            ));
        }
        Ok(Self {
            uniform_spacing,
            column_starts_sb,
            row_starts_sb,
            column_widths_sb,
            row_heights_sb,
            context_update_tile_id,
            tile_size_bytes,
        })
    }

    pub(crate) fn tile_count(&self) -> usize {
        self.column_starts_sb.len() * self.row_starts_sb.len()
    }
}

/// A tile's byte span within an AV1 tile-group OBU payload.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct TileDataRange {
    pub(crate) data_offset: u32,
    pub(crate) data_size: u32,
    pub(crate) row: u16,
    pub(crate) column: u16,
}

/// Extracts and packs tiles embedded after the header in a combined `OBU_FRAME`.
pub(crate) fn collect_embedded_frame_tiles(
    frame_obu: &[u8],
    layout: &TileLayout,
    header_bit_position: usize,
) -> VideoResult<(Vec<u8>, Vec<TileDataRange>)> {
    let obu = parse_obu(frame_obu)?;
    if obu.kind != ObuType::FRAME || obu.spatial_id != 0 {
        return Err(av1_error(
            "embedded_frame_tile_group",
            "input must be a single-layer AV1 frame OBU",
        ));
    }
    let ranges = parse_tile_group_payload(obu.payload, header_bit_position, layout)?;
    let columns = layout.column_starts_sb.len();
    let first = ranges
        .first()
        .ok_or_else(|| av1_error("collect_embedded_frame_tiles", "frame has no tile data"))?;
    let last = ranges
        .last()
        .ok_or_else(|| av1_error("collect_embedded_frame_tiles", "frame has no tile data"))?;
    let mut tracker = TileGroupTracker::new(layout.tile_count())?;
    let start = usize::from(first.row) * columns + usize::from(first.column);
    let end = usize::from(last.row) * columns + usize::from(last.column);
    tracker.accept(
        u16::try_from(start)
            .map_err(|_| av1_error("collect_embedded_frame_tiles", "tile index exceeds u16"))?,
        u16::try_from(end)
            .map_err(|_| av1_error("collect_embedded_frame_tiles", "tile index exceeds u16"))?,
    )?;
    if !tracker.is_complete() {
        return Err(av1_error(
            "collect_embedded_frame_tiles",
            "embedded tile group does not cover the complete frame",
        ));
    }
    let mut payload = Vec::new();
    let mut packed_ranges = Vec::new();
    packed_ranges.try_reserve_exact(ranges.len()).map_err(|_| {
        av1_error(
            "collect_embedded_frame_tiles",
            "tile range allocation failed",
        )
    })?;
    for range in ranges {
        let start = usize::try_from(range.data_offset)
            .map_err(|_| av1_error("collect_embedded_frame_tiles", "tile offset exceeds usize"))?;
        let size = usize::try_from(range.data_size)
            .map_err(|_| av1_error("collect_embedded_frame_tiles", "tile size exceeds usize"))?;
        let end = start
            .checked_add(size)
            .ok_or_else(|| av1_error("collect_embedded_frame_tiles", "tile range overflowed"))?;
        let bytes = obu
            .payload
            .get(start..end)
            .ok_or_else(|| av1_error("collect_embedded_frame_tiles", "tile range exceeds OBU"))?;
        let data_offset = u32::try_from(payload.len())
            .map_err(|_| av1_error("collect_embedded_frame_tiles", "packed payload exceeds u32"))?;
        payload.try_reserve(bytes.len()).map_err(|_| {
            av1_error(
                "collect_embedded_frame_tiles",
                "tile payload allocation failed",
            )
        })?;
        payload.extend_from_slice(bytes);
        packed_ranges.push(TileDataRange {
            data_offset,
            ..range
        });
    }
    Ok((payload, packed_ranges))
}

/// Parses FFmpeg-style tile offsets from one complete AV1 tile-group OBU.
pub(crate) fn parse_tile_group(
    input: &[u8],
    layout: &TileLayout,
) -> VideoResult<Vec<TileDataRange>> {
    let obu = parse_obu(input)?;
    if obu.kind != ObuType::TILE_GROUP || obu.spatial_id != 0 {
        return Err(av1_error(
            "parse_tile_group",
            "OBU must be a single-layer tile group",
        ));
    }
    let count = layout.tile_count();
    if count == 0 || count > 256 {
        return Err(av1_error(
            "parse_tile_group",
            "tile count must be between one and 256 for DXVA2",
        ));
    }
    parse_tile_group_payload(obu.payload, 0, layout)
}

#[expect(
    clippy::too_many_lines,
    reason = "tile-group syntax and byte-range validation follow FFmpeg's ordered parser"
)]
fn parse_tile_group_payload(
    payload: &[u8],
    start_bit: usize,
    layout: &TileLayout,
) -> VideoResult<Vec<TileDataRange>> {
    let count = layout.tile_count();
    if count == 0 || count > 256 {
        return Err(av1_error(
            "parse_tile_group",
            "tile count must be between one and 256 for DXVA2",
        ));
    }
    let mut bits = BitReader::new(payload);
    bits.skip_bits(start_bit)?;
    let (start, end) = if count > 1 && bits.read_bit()? {
        let width = ceil_log2(count);
        let start = usize::try_from(bits.read_bits(width)?)
            .map_err(|_| av1_error("parse_tile_group", "tile start index exceeds usize"))?;
        let end = usize::try_from(bits.read_bits(width)?)
            .map_err(|_| av1_error("parse_tile_group", "tile end index exceeds usize"))?;
        if start > end || end >= count {
            return Err(av1_error(
                "parse_tile_group",
                "tile-group range is outside the frame layout",
            ));
        }
        (start, end)
    } else {
        (0, count - 1)
    };

    if !bits.read_bit()? {
        return Err(av1_error(
            "parse_tile_group",
            "byte_alignment() marker bit is zero",
        ));
    }
    while !bits.position().is_multiple_of(8) {
        if bits.read_bit()? {
            return Err(av1_error(
                "parse_tile_group",
                "byte_alignment() padding bit is nonzero",
            ));
        }
    }
    let mut cursor = bits.position() / 8;
    let mut ranges = Vec::with_capacity(end - start + 1);
    for tile in start..=end {
        let size = if tile == end {
            payload
                .len()
                .checked_sub(cursor)
                .ok_or_else(|| av1_error("parse_tile_group", "tile payload offset exceeds OBU"))?
        } else {
            let byte_count = usize::from(layout.tile_size_bytes);
            let size_end = cursor.checked_add(byte_count).ok_or_else(|| {
                av1_error("parse_tile_group", "tile-size field offset overflowed")
            })?;
            let size_bytes = payload
                .get(cursor..size_end)
                .ok_or_else(|| av1_error("parse_tile_group", "tile-size field is truncated"))?;
            let mut encoded = 0_u32;
            for (shift, byte) in size_bytes.iter().enumerate() {
                encoded |= u32::from(*byte) << (shift * 8);
            }
            cursor = size_end;
            usize::try_from(
                encoded
                    .checked_add(1)
                    .ok_or_else(|| av1_error("parse_tile_group", "tile size overflows u32"))?,
            )
            .map_err(|_| av1_error("parse_tile_group", "tile size exceeds usize"))?
        };
        let tile_end = cursor
            .checked_add(size)
            .ok_or_else(|| av1_error("parse_tile_group", "tile data range overflowed"))?;
        if size == 0 || tile_end > payload.len() {
            return Err(av1_error(
                "parse_tile_group",
                "tile data is empty or exceeds tile-group payload",
            ));
        }
        let columns = layout.column_starts_sb.len();
        ranges.push(TileDataRange {
            data_offset: u32::try_from(cursor)
                .map_err(|_| av1_error("parse_tile_group", "tile data offset exceeds u32"))?,
            data_size: u32::try_from(size)
                .map_err(|_| av1_error("parse_tile_group", "tile size exceeds u32"))?,
            row: u16::try_from(tile / columns)
                .map_err(|_| av1_error("parse_tile_group", "tile row exceeds u16"))?,
            column: u16::try_from(tile % columns)
                .map_err(|_| av1_error("parse_tile_group", "tile column exceeds u16"))?,
        });
        cursor = tile_end;
        if tile != end && cursor == payload.len() {
            return Err(av1_error(
                "parse_tile_group",
                "tile payload ends before the final tile",
            ));
        }
    }
    if cursor != payload.len() {
        return Err(av1_error(
            "parse_tile_group",
            "tile-group payload contains trailing bytes",
        ));
    }
    Ok(ranges)
}

/// Concatenates ordered tile-group payloads and relocates their tile offsets.
pub(crate) fn collect_tile_groups(
    inputs: &[&[u8]],
    layout: &TileLayout,
) -> VideoResult<(Vec<u8>, Vec<TileDataRange>)> {
    let count = layout.tile_count();
    if inputs.is_empty() || count == 0 || count > 256 {
        return Err(av1_error(
            "collect_tile_groups",
            "one or more OBUs and a one through 256 tile layout are required",
        ));
    }
    let mut tracker = TileGroupTracker::new(count)?;
    let mut payload = Vec::new();
    let mut output_ranges = Vec::with_capacity(count);
    for (group_index, input) in inputs.iter().enumerate() {
        let obu = parse_obu(input)?;
        let ranges = parse_tile_group(input, layout)?;
        let columns = layout.column_starts_sb.len();
        let start = usize::from(ranges[0].row) * columns + usize::from(ranges[0].column);
        let last = ranges.last().ok_or_else(|| {
            av1_error("collect_tile_groups", "tile group contains no tile ranges")
        })?;
        let end = usize::from(last.row) * columns + usize::from(last.column);
        let complete = tracker.accept(
            u16::try_from(start)
                .map_err(|_| av1_error("collect_tile_groups", "tile index exceeds u16"))?,
            u16::try_from(end)
                .map_err(|_| av1_error("collect_tile_groups", "tile index exceeds u16"))?,
        )?;
        if complete && group_index + 1 != inputs.len() {
            return Err(av1_error(
                "collect_tile_groups",
                "tile groups continue after the final frame tile",
            ));
        }
        for range in ranges {
            let start = usize::try_from(range.data_offset)
                .map_err(|_| av1_error("collect_tile_groups", "tile offset exceeds usize"))?;
            let size = usize::try_from(range.data_size)
                .map_err(|_| av1_error("collect_tile_groups", "tile size exceeds usize"))?;
            let end = start
                .checked_add(size)
                .ok_or_else(|| av1_error("collect_tile_groups", "tile range overflowed"))?;
            let bytes = obu.payload.get(start..end).ok_or_else(|| {
                av1_error("collect_tile_groups", "tile range exceeds its OBU payload")
            })?;
            let new_offset = u32::try_from(payload.len())
                .map_err(|_| av1_error("collect_tile_groups", "combined payload exceeds u32"))?;
            let new_size = u32::try_from(bytes.len())
                .map_err(|_| av1_error("collect_tile_groups", "tile size exceeds u32"))?;
            payload
                .try_reserve(bytes.len())
                .map_err(|_| av1_error("collect_tile_groups", "tile payload allocation failed"))?;
            payload.extend_from_slice(bytes);
            output_ranges.push(TileDataRange {
                data_offset: new_offset,
                data_size: new_size,
                ..range
            });
        }
    }
    if !tracker.is_complete() {
        return Err(av1_error(
            "collect_tile_groups",
            "tile groups do not cover the complete frame",
        ));
    }
    Ok((payload, output_ranges))
}

fn ceil_log2(value: usize) -> u8 {
    u8::try_from((value - 1).bit_width()).unwrap_or(8)
}

struct Axis(Vec<u16>, Vec<u16>);

impl Axis {
    fn map_log2(self, log2: u32) -> (Vec<u16>, Vec<u16>, u32) {
        (self.0, self.1, log2)
    }
}

fn uniform_axis(total: u32, size: u32, operation: &'static str) -> VideoResult<Axis> {
    let mut starts = Vec::new();
    let mut sizes = Vec::new();
    let mut start = 0;
    while start < total {
        starts.push(to_u16(start, operation)?);
        let current = size.min(total - start);
        sizes.push(to_u16(current, operation)?);
        start += current;
    }
    Ok(Axis(starts, sizes))
}

fn tile_log2(block_size: u32, target: u32) -> u32 {
    let mut value = block_size;
    let mut log2 = 0;
    while value < target {
        value <<= 1;
        log2 += 1;
    }
    log2
}

pub(super) fn read_increment(
    bits: &mut BitReader<'_>,
    minimum: u32,
    maximum: u32,
) -> VideoResult<u32> {
    let mut value = minimum;
    while value < maximum && bits.read_bit()? {
        value += 1;
    }
    Ok(value)
}

fn read_ns(bits: &mut BitReader<'_>, maximum: u32) -> VideoResult<u32> {
    if maximum <= 1 {
        return Ok(0);
    }
    let width = maximum.bit_width();
    let marker = (1_u32 << width) - maximum;
    let value = u32::try_from(bits.read_bits(to_u8(width - 1)?)?)
        .map_err(|_| av1_error("TileLayout::read_ns", "value exceeds u32"))?;
    if value < marker {
        Ok(value)
    } else {
        Ok((value << 1) - marker + u32::from(bits.read_bit()?))
    }
}

fn to_u16(value: u32, operation: &'static str) -> VideoResult<u16> {
    u16::try_from(value).map_err(|_| av1_error(operation, "value exceeds u16"))
}

fn to_u8(value: u32) -> VideoResult<u8> {
    u8::try_from(value).map_err(|_| av1_error("TileLayout::parse", "bit width exceeds u8"))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct TileGroupTracker {
    tile_count: u16,
    next_tile: u16,
}

impl TileGroupTracker {
    pub(crate) fn new(tile_count: usize) -> VideoResult<Self> {
        let count = u16::try_from(tile_count)
            .map_err(|_| av1_error("TileGroupTracker::new", "tile count exceeds u16"))?;
        if count == 0 {
            return Err(av1_error("TileGroupTracker::new", "tile count is zero"));
        }
        Ok(Self {
            tile_count: count,
            next_tile: 0,
        })
    }

    pub(crate) fn accept(&mut self, start: u16, end: u16) -> VideoResult<bool> {
        if start != self.next_tile || end < start || end >= self.tile_count {
            return Err(av1_error(
                "TileGroupTracker::accept",
                "tile group is out of order, overlapping, or out of range",
            ));
        }
        self.next_tile = end + 1;
        Ok(self.next_tile == self.tile_count)
    }

    pub(crate) const fn is_complete(self) -> bool {
        self.next_tile == self.tile_count
    }
}
