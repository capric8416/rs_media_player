# Windows video API research

## Findings that constrain the design

- D3D12 video requires explicit capability queries through
  `ID3D12VideoDevice::CheckFeatureSupport`; support cannot be inferred from OS version alone.
- D3D12 keeps reference-frame management in the application and submits decode work on a video
  command queue. The application is responsible for decoder access synchronization.
- Decoder/heap recreation requirements differ: resolution-dependent state belongs to the heap, and
  the decoder carries session state including DPB limits.
- D3D11 video decoder output views map to D3D12 texture resources rather than a directly equivalent
  view object. A shared trait must therefore abstract frames without pretending the native handles
  are identical.
- The Windows Rust bindings expose feature groups for D3D9, DXVA2, D3D11, D3D12, and DXGI. Features
  must be limited in `Cargo.toml`, and binding types must stay behind backend-specific modules.

These facts support ADR-009, ADR-010, ADR-011, ADR-019, ADR-021, and ADR-025.

## Semantic traps to test

- Capability matrices by codec profile, bit depth, resolution, format, and adapter.
- D3D12 command allocator/list reuse only after the associated fence completes.
- Resource transitions around video decode, video process, copy/readback, and presentation.
- Decoder access serialization when callers move frames across threads.
- Device removal during submission and presentation.

## Primary sources

- Microsoft, [Direct3D 12 Video Overview](https://learn.microsoft.com/en-us/windows/win32/medfound/direct3d-12-video-overview)
- Microsoft, [Migrating to Direct3D 12 video](https://learn.microsoft.com/en-us/windows/win32/medfound/direct3d-12-video-migration)
- Microsoft, [Direct3D 12 Video Structures](https://learn.microsoft.com/en-us/windows/win32/medfound/direct3d-12-video-structures)
- Microsoft, [windows-rs repository](https://github.com/microsoft/windows-rs)
- Microsoft, [windows crate manifest](https://github.com/microsoft/windows-rs/blob/master/crates/libs/windows/Cargo.toml)
