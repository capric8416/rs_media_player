# D3D11 device and decoder-frame characterization

## Source oracle

The pinned FFmpeg source at commit `74f5ea45ca78988784b810ad9bbb148ea7ff76d8` is the oracle:

- `libavutil/hwcontext_d3d11va.c::d3d11va_device_create` creates a hardware D3D11 device with
  video support and can also wrap an injected device/context.
- `d3d11va_device_init` queries the immediate context, `ID3D11VideoDevice`, and
  `ID3D11VideoContext`; optional context locking is protected through the D3D multithread interface.
- `d3d11va_frames_init` uses NV12/P010 compatible textures with decoder binding; decoder-bound
  texture arrays are capped at 64 slices (`MAX_ARRAY_SIZE`).
- The D3D11 video-device interface enumerates profile GUIDs and validates output formats. FFmpeg's
  AVC decode path uses the H.264 no-FGT and FGT GUIDs for Baseline/Main/High; AV1 Main uses one
  Profile0 GUID for both bit depths, with NV12/P010 distinguishing output format. Decoder configs
  are enumerated against actual coded dimensions because D3D11 configuration availability depends
  on frame size.

The Windows SDK contract is consumed through the `windows` crate 0.62.2 bindings at Rust 1.100.
The local host can cross-compile for Windows x64 but has no Windows GPU adapter for runtime probes.

## Port boundary

This unit creates or retains a D3D11 device/context, queries D3D11 video interfaces, discovers
AVC/HEVC/AV1 8/10-bit 4:2:0 profile plus NV12/P010 format support, and allocates decoder output
texture arrays. It can insert D3D11 event queries as completion tokens and poll or wait for their
completion. It does not create decoder objects, submit compressed frames, or implement scale and
presentation. Runtime format/profile availability remains adapter and driver dependent.

## Gate vectors

Pure data tests cover profile GUID mappings, bit-depth matching, DXGI surface mappings and the
64-slice bound. Rust 1.100 Windows x64 checks compile the typed COM signatures; workspace tests,
Clippy, rustdoc and formatting pass. Runtime adapter capability, decoder config, texture allocation,
event-query completion, device-loss and COM lifetime tests require a Windows GPU host and remain a
hardware CI requirement.
