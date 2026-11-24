# DXVA2 decoder submission characterization

## Source oracle

The pinned FFmpeg 9 source at commit `74f5ea45ca78988784b810ad9bbb148ea7ff76d8` uses
`libavcodec/dxva2.c` as the shared DXVA2 transport and `dxva2_h264.c`, `dxva2_hevc.c`, and
`dxva2_av1.c` for codec-specific picture construction.

- The shared path creates an `IDirectXVideoDecoder` from a decoder GUID, coded-size descriptor,
  selected `DXVA2_ConfigPictureDecode`, and the decoder target surfaces.
- A picture is submitted in `BeginFrame`/`Execute`/`EndFrame` order. `Execute` references an array
  of buffer descriptors whose payloads were populated through matching `GetBuffer` and
  `ReleaseBuffer` pairs.
- Codec adapters construct different picture-parameter, quantization, bitstream, slice-control or
  AV1 tile buffers and maintain codec-specific reference surfaces. These payload layouts are part
  of the DXVA specifications and FFmpeg's codec mapping, not a common byte-stream conversion.

## Port status

`d3d9_decode.rs` currently implements the shared decoder creation and buffer transport boundary:
selects a size-specific driver config, allocates the required target pool, retains those targets,
validates per-picture buffer lists, copies payloads into driver-owned buffers, releases each
acquired buffer, performs the frame submission sequence, and returns a D3D9 event-query token. It
can also normalize AVC/HEVC Annex B or length-prefixed packets to the three-byte start-code form and
structurally validate AV1 OBU packets. It intentionally takes codec-prepared `Dxva2Picture` values.
It also assembles AVC and HEVC VCL-only bitstreams with three-byte start codes, 128-byte padding,
and DXVA short slice-control records (HEVC reports zero macroblocks; AVC derives macroblock count
from coded dimensions). For AV1 it validates caller-provided tile ranges and serializes the 16-byte
`DXVA_Tile_AV1` control records alongside padded tile payload data.

The current codec-parameter mapping builds the 1040-byte H.264 DXVA picture-parameter record and
the 224-byte inverse-quantization matrix for progressive 8-bit 4:2:0 IDR pictures. The
`Dxva2Picture::from_avc_idr_sample` convenience path combines those records with AVC bitstream and
slice-control buffers. It deliberately rejects inter pictures, fields, and unsupported sample
formats because reference-surface mapping and stateful AVC decode have not been implemented.

For HEVC, `hevc/dxva.rs` now serializes the 232-byte `DXVA_PicParams_HEVC` record and 1000-byte
default inverse-quantization matrix for a first IDR/I slice. `Dxva2Picture::from_hevc_idr_sample`
combines these records with the access-unit bitstream and slice controls. Main/Main10 4:2:0 default
matrices are covered; custom scaling-list data, inter-picture reference state, non-IDR pictures,
and codec-specific end-to-end driver validation remain unsupported. The parser retains the SPS
transform and PPS tile/deblocking fields needed by this mapping.

The AV1 parser now extracts FFmpeg-style tile byte ranges from tile-group OBUs, including the
optional start/end range, byte alignment, little-endian tile sizes, and row/column mapping. It can
also validate ordered, complete multi-group coverage, concatenate tile payloads and relocate their
offsets for the existing DXVA tile-control serialization. Complete AV1 picture parameters remain
deferred. The frame parser now continues key/intra headers through frame dimensions, super-resolution,
render-size, intrabc/CDF flags, tile layout, quantization, intra-frame segmentation feature maps,
delta-Q/delta-LF controls, loop-filter levels/reference/mode deltas, CDEF damping/set strengths,
restoration types/unit shifts, tx mode, reduced-tx-set, and film-grain parameters for intra frames.
`av1/dxva.rs` serializes the 912-byte SDK picture-parameter structure for key/intra frames, and
`Dxva2Picture::from_av1_intra_obus` combines it with complete tile-group payloads/control records.
Key/intra global motion is identity-inferred; inter-frame global-motion parsing/reference mapping
remain unsupported. CDEF/restoration honor lossless, intrabc, and sequence-feature inference.

This is not yet the complete approved `d3d9-decode` unit. Stateful AVC/HEVC inter-picture reference
translation, AV1 picture parameters and tile-offset extraction are still missing, and this transport
does not implement the backend-neutral `D3dDecode` trait. Those adapters are required before claiming
general decoded-frame behavior or closing the unit. Existing Rust parsers cover only a subset of
FFmpeg's full codec state, so translation gaps must be characterized before mapping all syntax into
DXVA structures.

## Current gate evidence

Rust 1.100 Windows x64 test compilation, formatting, all 111 workspace tests, Clippy and rustdoc
with warnings denied pass. Tests cover the H.264 IDR parameter/matrix layout, AVC/HEVC slice
assembly, HEVC IDR parameter/matrix sizes, AV1 tile serialization, empty/duplicate buffer checks,
and bitstream framing. No
DXVA2 GPU was available, and no codec-prepared streams are yet submitted; there are no decoded-frame
hash or stream-change results for this unit.
