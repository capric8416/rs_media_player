# DXVA2 and D3D9 device-frame characterization

## Source oracle

The pinned FFmpeg source at commit `74f5ea45ca78988784b810ad9bbb148ea7ff76d8` defines the
behavior in `libavutil/hwcontext_dxva2.c` and `libavcodec/dxva2.c`:

- Device creation prefers D3D9Ex, retries with classic D3D9, uses a windowed video presentation
  device, creates a DXVA2 device manager, resets it to the selected D3D device, then opens/closes
  manager handles around video-service requests.
- Frame setup asks `IDirectXVideoAccelerationService::CreateSurface` for a fixed-size pool and
  passes `pool_size - 1` as the backbuffer count. Decoder render-target surfaces are NV12 or P010
  for the first-release codec scope.
- Codec capability probing enumerates decoder GUIDs, maps AVC Baseline/Main/High to H.264 E/F and
  Intel ClearVideo GUIDs, HEVC Main/Main10 to their profile GUIDs, and AV1 Main 8/10-bit to
  Profile0. It validates
  NV12/P010 against each GUID's render-target formats. Decoder configuration lookup uses the actual
  coded dimensions and pixel format; the remaining `DXVA2_VideoDesc` fields stay zero-initialized.

## Port boundary

`d3d9_device.rs` owns or wraps the D3D9 device and DXVA2 manager, retains decoder/acceleration
services, discovers AVC/HEVC/AV1 Main 8/10-bit 4:2:0 surfaces, enumerates size-specific decoder
configurations, allocates caller-sized fixed pools for decoder/processor target types, and exposes
D3D9 event-query polling/waiting. Device-lost/reset HRESULTs become the shared `DeviceLost` error.
Compressed-picture submission, processing and rendering remain separate units.

## Gate and hardware limits

Pure tests cover GUID/profile and NV12/P010 FOURCC mappings. Rust 1.100 Windows x64 checks compile
all Direct3D9, DXVA2 and COM signatures. Adapter capability lists, actual surface/config creation,
event-query completion and device-loss behavior require a Windows GPU host; this environment has no
DXVA2 hardware runtime for those probes.
