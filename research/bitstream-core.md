# Bitstream core characterization

## Source oracle

The local environment has no built FFmpeg test binaries. Characterization is taken from the pinned
source at commit `74f5ea45ca78988784b810ad9bbb148ea7ff76d8`:

- `libavcodec/get_bits.h`: MSB-first bit cursor, peek/read/skip/alignment behavior.
- `libavcodec/golomb.h` and `libavcodec/tests/golomb.c`: unsigned and signed Exp-Golomb mapping;
  the upstream test round-trips 8,191 sequential values.
- `libavcodec/leb.h`: unsigned LEB128 reads at most eight bytes.
- `libavcodec/h2645_parse.c`: Annex B/length-prefixed splitting and RBSP extraction.
- `libavcodec/av1_parse.h` and `av1_parse.c`: OBU header, extension, size and bounds behavior.

## Recorded semantics

1. Bits are consumed most-significant bit first; peeking does not advance; byte alignment skips to
   the next byte boundary.
2. Unsigned Exp-Golomb maps `1 -> 0`, `010 -> 1`, `011 -> 2`, and so on. Signed mapping is
   `0, +1, -1, +2, -2, ...`.
3. H.264/HEVC RBSP extraction removes `0x03` following `00 00`, records the skipped location, and
   stops before a following `00 00 01` start code.
4. Annex B parsing searches for a start code, ignores leading junk, and ignores empty NAL units.
   Length-prefixed parsing uses a one-to-four-byte big-endian length and rejects lengths beyond the
   remaining packet.
5. AV1 OBU parsing rejects the forbidden bit, optionally reads one extension byte, optionally reads
   up to eight LEB128 bytes, and rejects a payload size beyond the input. Without a size field, the
   OBU consumes the remaining packet.

## Rust safety decisions

- Every bit read is bounds checked; truncated input returns `VideoError` instead of padded/undefined
  reads.
- Count/offset arithmetic is checked before slicing.
- NAL and OBU iteration borrows packet storage and performs no payload copies.
- RBSP borrows input when no emulation-prevention byte must be removed and allocates only when
  unescaping is necessary.
- The low-level modules remain crate-private until codec-parser units establish their final API.

## Gate vectors

The gate covers cross-byte reads, peek/skip/alignment, truncation, unsigned/signed Exp-Golomb,
32-leading-zero overflow, borrowed/owned RBSP paths, escape positions, embedded next start code,
three/four-byte Annex B prefixes, leading junk, empty units, all length-prefix widths, malformed
lengths, basic/extended/sized/unsized OBUs, eight-byte LEB128, forbidden bits and truncated headers.
