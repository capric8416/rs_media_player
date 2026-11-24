# MP4 sample-table and fragment characterization

## Source oracle

The pinned FFmpeg source at commit `74f5ea45ca78988784b810ad9bbb148ea7ff76d8` is the oracle:

- `libavformat/mov.c::mov_read_stts`, `mov_read_ctts`, `mov_read_stsc`, `mov_read_stsz`,
  `mov_read_stco`, and `mov_read_stss` parse classic sample-table payloads with checked entry
  counts and one-based chunk/sample numbers.
- `mov_build_index` combines chunk offsets, sample-to-chunk runs and sample sizes into byte ranges;
  expands decode durations; applies composition offsets to DTS; and marks `stss` entries as random
  access samples.
- `mov_read_trex`, `mov_read_tfhd`, `mov_read_tfdt`, and `mov_read_trun` apply fragment defaults in
  ISO BMFF override order, treat version-one composition offsets as signed, accumulate byte offsets
  and DTS, and use sample flag bit 16 as the non-sync marker.
- `mov_seek_stream` searches the timestamp index and backs up to a decodable key sample.

The source tree contains no built `ffprobe`, so tests use direct source-derived box payloads and
expected sample indexes rather than process replay.

## Port boundary

This unit parses classic `stts`, `ctts`, `stsc`, `stsz`/`stz2`, `stco`/`co64`, and optional `stss`
payloads into a checked per-track `SampleIndex`. It parses `trex`, `tfhd`, `tfdt`, and `trun`
payloads into the same sample type, including default inheritance, signed data offsets, signed
version-one composition offsets and sync flags. Seeking selects the previous or next sync sample on
the presentation timeline. Edit lists, sample groups, encryption auxiliary data, external data
references and uncommon QuickTime audio chunk coalescing are deferred; ordinary playback MP4 and
fMP4 tracks do not require them for this unit.

## Gate vectors

Cases cover variable and constant sample sizes, 32/64-bit chunk offsets, multiple `stsc` runs,
signed/unsigned composition offsets, implicit and explicit sync tables, previous/next sync seek,
`trex` defaults overridden by `tfhd` and `trun`, version-zero/one `tfdt`, multiple fragment samples,
and malformed counts, ordering, zero values, overflow and truncated optional fields.
