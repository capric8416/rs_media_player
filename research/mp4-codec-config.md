# MP4 codec-configuration characterization

## Source oracle

The pinned FFmpeg source at commit `74f5ea45ca78988784b810ad9bbb148ea7ff76d8` is the oracle:

- `libavformat/mov.c::mov_read_glbl` copies `avcC`, `hvcC`, and `av1C` payloads as codec extradata,
  rejecting payloads over 1 GiB and ignoring duplicate global headers.
- `libavformat/mov_esds.c`, `libavformat/isom.c::ff_mp4_read_dec_config_descr`, and
  `libavcodec/mpeg4audio.c::ff_mpeg4audio_get_config_gb` decode MPEG-4 descriptors and AAC
  `AudioSpecificConfig` fields, including escaped object types, explicit sample rates and explicit
  SBR/PS extensions.
- `libavformat/mov.c::mov_read_dops` accepts version-zero `OpusSpecificBox` payloads of at least
  eleven bytes, converts the big-endian numeric fields to an Ogg-compatible little-endian
  `OpusHead`, exposes pre-skip as initial padding, and uses an 80 ms seek preroll at 48 kHz.
- `libavformat/movenc.c::mov_write_dops_tag` performs the inverse `OpusHead` to `dOps` mapping and
  requires mapping-table bytes when the channel-mapping family is nonzero.

The source tree has no built `ffprobe`, so direct process recording is unavailable. Tests use
source-derived normal and malformed vectors and require byte-stable parse/normalize/parse output.

## Accepted boundary

The Rust crate parses and normalizes AVC, HEVC and AV1 decoder configuration records; parses the
`esds` descriptor chain required to locate AAC decoder-specific bytes; parses common AAC Main/LC,
SSR/LTP and explicit HE-AAC/HE-AACv2 `AudioSpecificConfig`; and maps version-zero `dOps` to a valid
`OpusHead`. AAC channel configuration zero requires a Program Config Element and returns a typed
unsupported error in this unit. Unsupported future AV1C and dOps versions are rejected rather than
silently misinterpreted.

## Gate vectors

Vectors cover AVC SPS/PPS and high-profile extensions, HEVC VPS/SPS/PPS arrays, AV1C Main/Main10
headers with config OBUs, AAC-LC and explicit HE-AAC inside `esds`, mono/stereo Opus and mapped
multichannel Opus, normalized round trips, truncated length fields, invalid reserved/version bits,
invalid sample-rate/channel indices, and missing descriptor/config payloads.
