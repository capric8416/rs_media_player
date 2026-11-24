# d3d-video

Standalone Windows hardware video decode, scaling/conversion, and presentation for DXVA2/D3D9,
D3D11VA, and D3D12 Video. The crate contains stateful AVC/HEVC/AV1 decode paths, video-processor
scalers for NV12/P010 and BGRA8 conversion, and HWND renderers. D3D12's `D3dRender` path accepts
decode/scaler frames directly and performs a video-process pass before swap-chain copy.

## Backend shape

- D3D9 and D3D11 decode sessions return their backend decoded frame types; their renderers accept
  those frames and perform scaling/conversion where required.
- D3D12 decode sessions return `D3d12ScaleFrame`; the scaler can scale/convert it and the renderer
  presents it through `D3dRender`. Caller-owned BGRA resources can use `render_bgra_frame`.
- Completion queries/fences and surface leases track submitted GPU work. If decoder completion is
  unknown after submission, the decoder is poisoned; if scaler completion is unknown, its input
  lease and submitted resources are retained rather than reused.

## MP4 playback chain

`Mp4VideoPlayback` wires a seekable source to demux, packet submission, decode, scale, and present.
Create the HWND and backend device on the UI/application thread, then choose matching backend
objects. For D3D11, the construction shape is:

```rust,ignore
use d3d_video::d3d11_device::{D3d11Device, D3d11DeviceOptions};
use d3d_video::{
    D3d11DecodeSession, D3d11Renderer, D3d11Scaler, FrameSize, Mp4VideoPlayback,
    PlaybackOptions, PresentMode, RenderParameters, Rotation, ScaleFilter, ScaleRequest,
};

let device = D3d11Device::create(D3d11DeviceOptions::default())?;
let size = FrameSize::new(1920, 1080)?;
let decoder = D3d11DecodeSession::new(device.clone(), 16)?;
let scaler = D3d11Scaler::new(device.clone())?;
let renderer = D3d11Renderer::new(device, hwnd, size)?;
let options = PlaybackOptions {
    track_number: 0,
    scale_request: ScaleRequest {
        output_size: size,
        output_format: d3d_video::PixelFormat::Bgra8,
        source_rect: None,
        destination_rect: None,
        aspect_mode: d3d_video::AspectMode::Contain,
        rotation: Rotation::Degrees0,
        filter: ScaleFilter::DriverDefault,
        output_color: None,
    },
    render_parameters: RenderParameters {
        present_mode: PresentMode::VSync,
        ..RenderParameters::default()
    },
    max_metadata_bytes: 64 * 1024 * 1024,
    max_sample_bytes: 32 * 1024 * 1024,
};
let mut playback = Mp4VideoPlayback::open(
    std::fs::File::open("video.mp4")?, decoder, scaler, renderer, options,
)?;
while !playback.step()?.end_of_stream {}
playback.drain()?;
```

Use the corresponding D3D9 or D3D12 device/session/scaler/renderer types for those backends. The
source must implement `Read + Seek`; metadata and sample indexes are retained, while compressed
sample bytes are read and released one access unit at a time. `step` presents zero or more frames
because decoder output can be delayed or reordered. `seek` takes movie-timescale ticks and flushes
decoder references before restarting at the selected sync sample.

On Windows, the `playback_smoke` example creates its own test window and runs the selected complete
backend chain. Build or run it with:

```powershell
cargo +1.100.0 build -p d3d-video --example playback_smoke --target x86_64-pc-windows-msvc
cargo +1.100.0 run -p d3d-video --example playback_smoke -- d3d11 .\fixtures\generated\avc_high_bframes.mp4
cargo +1.100.0 run -p d3d-video --example playback_smoke -- d3d9 .\fixtures\generated\hevc_main_bframes.mp4 0
cargo +1.100.0 run -p d3d-video --example playback_smoke -- d3d12 .\fixtures\generated\av1_main_bframes.mp4 0
```

Generate local AVC/HEVC/AV1 8/10-bit clips before running the commands:

```powershell
.\scripts\generate_playback_fixtures.ps1
```

The example accepts any generated clip/backend combination, including Main10 clips, for checking
which profiles your adapter supports.

The optional final argument selects a zero-based video-track index. A playback failure is returned
with the MP4, decoder, scaler, or renderer error; the example prints indexed sample and presented
frame counts.

## Known limits

The current AV1 inter-frame path is limited to frames with `PRIMARY_REF_NONE`; frames that inherit
segmentation, CDF, loop-filter, or global-motion state from another primary reference remain
unsupported. 12-bit/4:2:2/4:4:4 profiles and software fallback are outside the accepted first-release
scope. D3D9 rendering supports zero-degree rotation; D3D11/D3D12 rotation is adapter capability
dependent. HDR-to-SDR tone mapping, GPU visual/differential validation, and hardware fault-injection
remain outstanding. Consult [`../MAP.md`](../MAP.md) for per-unit status.
