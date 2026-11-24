# D3D12 device and decoder-frame characterization

## Source oracle

The pinned FFmpeg 9 source is the oracle:

- `libavutil/hwcontext_d3d12va.c` dynamically loads D3D12/DXGI device creation, initializes
  `ID3D12VideoDevice`, creates default-heap NV12/P010 texture resources, and implements queue/fence
  synchronization with `Signal` and `SetEventOnCompletion`.
- `libavcodec/d3d12va_decode.c` queries decode support for coded dimensions and rejects decode tiers
  below Tier 2. It transitions destination and reference resources between COMMON,
  VIDEO_DECODE_READ and VIDEO_DECODE_WRITE states, and signals completion fences after submissions.
- `libavcodec/d3d12va_av1.c` uses the AV1 Profile0 video decode profile; NV12/P010 distinguishes
  8-bit and 10-bit output.

The Rust implementation uses the Windows SDK bindings from `windows` 0.62.2 and targets Rust
1.100.0. Windows x64 cross-compilation is available; GPU runtime validation requires a D3D12 host.

## Port boundary

This unit creates or wraps a D3D12 device and video interface, enumerates AVC/HEVC/AV1 Main/Main10
decode profile and format combinations, checks coded-size support and exposes the driver's decode
tier. It allocates NV12/P010 texture arrays in COMMON state, tracks explicit graphics/decode
transitions, executes command lists, and provides per-token fence polling and timed waits. Tier 2
policy enforcement, decoder configuration, compressed-picture submission, scale and presentation
belong to later units.

## Gate vectors

Four pure tests cover profile/depth mappings, output formats, valid/no-op state transitions, and
Win32 timeout bounds. Rust 1.100 Windows x64 test compilation passes. Workspace tests pass 96/96;
formatting, Clippy and rustdoc with warnings denied pass. Device creation, feature-query results,
resource transitions under GPU execution, fence completion/device removal and COM lifetime behavior
remain hardware CI checks because no D3D12 adapter was available here.
