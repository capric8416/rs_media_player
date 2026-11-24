# Architecture decisions

These ADRs govern the port from the FFmpeg source tree at
`C:\vcpkg\buildtrees\ffmpeg\src\n9.0.2-2dce354851.clean`. ADR numbers are
append-only and must never be reused.

## ADR-001: Produce standalone Rust crates
Status: accepted (2026-11-20)

Context: The result must be usable without loading or linking FFmpeg.  
Decision: Depend only on Rust crates and Windows system APIs at runtime.  
Consequences: FFmpeg remains a reference and oracle; required parsing and state management must be implemented in Rust.

## ADR-002: Map each operation to its matching trait
Status: accepted (2026-11-20)

Context: The requested file-to-trait wording was ambiguous.  
Decision: Every `*_decode.rs` implements `D3dDecode`, every `*_scale.rs` implements `D3dScale`, and every `*_render.rs` implements `D3dRender`.  
Consequences: Traits live in `d3d_decode.rs`, `d3d_scale.rs`, and `d3d_render.rs`; backends remain separate files.

## ADR-003: Own codec parsing and DPB state
Status: accepted (2026-11-20)

Context: DXVA submission parameters require parsed codec state and reference pictures.  
Decision: Accept compressed AVC, HEVC, and AV1 data and implement parameter-set, slice/tile, and DPB/reference state in Rust.  
Consequences: The public API does not require callers to construct DXVA picture parameters.

## ADR-004: Scope MP4 to playback-first MP4 and fragmented MP4
Status: accepted (2026-11-20)

Context: FFmpeg `mov.c` contains many formats and features beyond normal playback.  
Decision: Cover MP4/fMP4 tracks, sample tables, timestamps, extradata, seeking, and AVC/HEVC/AV1 playback first.  
Consequences: CENC decryption, HEIF, and uncommon QuickTime metadata are deferred.

## ADR-005: Add a DXVA2 video-processor D3D9 scaler
Status: accepted (2026-11-20)

Context: FFmpeg 9 has D3D11 and D3D12 scale filters but no equivalent D3D9 filter.  
Decision: Implement D3D9 scaling with the DXVA2 Video Processor.  
Consequences: This module is behavior-derived new work, not a line-for-line FFmpeg port.

## ADR-006: Render to windows and expose textures
Status: accepted (2026-11-20)

Context: Applications need both turnkey display and composition access.  
Decision: Support rendering to an `HWND` and returning/accepting backend GPU surfaces.  
Consequences: Texture/surface output is the core contract; swap-chain presentation is a convenience layer.

## ADR-007: Target Windows 10 and 11 x64
Status: accepted (2026-11-20)

Context: D3D video APIs and profiles vary by OS, driver, and adapter.  
Decision: Target Windows 10/11 x64 on stable Rust and probe every capability at runtime.  
Consequences: Windows 7/8, ARM64, and software fallback are out of scope; unsupported capabilities return errors.

## ADR-008: Gate progress by migration wave
Status: accepted (2026-11-20)

Context: Hardware and container behavior needs evidence beyond compilation.  
Decision: Stop for approval after each wave; require fixtures, tests, Clippy, and relevant GPU checks.  
Consequences: No later wave starts merely because an earlier unit builds.

## ADR-009: Submit synchronously and complete asynchronously
Status: accepted (2026-11-20)

Context: Waiting after every GPU submission would serialize CPU and GPU work.  
Decision: Submission methods are synchronous Rust calls that return frames carrying a synchronization token/fence.  
Consequences: Callers decide when to wait; the crate does not require an async runtime.

## ADR-010: Use mutable single-owner backend instances
Status: accepted (2026-11-20)

Context: Video contexts and command allocators need serialized mutation.  
Decision: Decoder, scaler, and renderer operations take `&mut self`; frames may be `Send`, but instances are not promised as concurrently callable.  
Consequences: The API does not claim `Sync` for mutable COM contexts.

## ADR-011: Keep the first pipeline within one D3D backend
Status: accepted (2026-11-20)

Context: Cross-device and cross-generation sharing adds synchronization and format constraints.  
Decision: Decode, scale, and render use the same backend in the first release.  
Consequences: `GpuFrame` leaves room for future interop, but automatic D3D9/11/12 conversion is out of scope.

## ADR-012: Limit first-release surface formats
Status: accepted (2026-11-20)

Context: The two FFmpeg hardware scale filters accept NV12 and P010.  
Decision: Scale NV12/P010; render to BGRA8 and additionally RGB10A2 for HDR.  
Consequences: Other formats produce an explicit unsupported-format error.

## ADR-013: Preserve color and HDR metadata
Status: accepted (2026-11-20)

Context: Correct video presentation depends on range, matrix, transfer, primaries, and HDR metadata.  
Decision: Carry these fields on frames and set the corresponding DXGI color space when supported.  
Consequences: Unsupported required color behavior is an error, not a silent conversion.

## ADR-014: Accept container and elementary stream framing
Status: accepted (2026-11-20)

Context: MP4 and raw streams frame codec data differently.  
Decision: Support length-prefixed and Annex B AVC/HEVC plus AV1 ISOBMFF samples and OBU input.  
Consequences: The MP4 crate emits the same `DecoderConfig` and `EncodedSample` model used by raw inputs.

## ADR-015: Provide seekable and incremental MP4 input
Status: accepted (2026-11-20)

Context: Local MP4 and live/fragmented MP4 require different I/O.  
Decision: Provide `Read + Seek` parsing and a `push_bytes` incremental fMP4 interface without Tokio.  
Consequences: Async applications drive the incremental API from their own runtime.

## ADR-016: Isolate unsafe Windows calls
Status: accepted (2026-11-20)

Context: COM and Win32 graphics calls require `unsafe`.  
Decision: Keep unsafe code inside small backend boundary modules and document each operation with `// SAFETY:`.  
Consequences: Public APIs remain safe Rust and gate rejects undocumented unsafe blocks.

## ADR-017: License derived crates as LGPL-2.1-or-later
Status: accepted (2026-11-20)

Context: The implementation will be derived from LGPL FFmpeg source.  
Decision: License both crates under `LGPL-2.1-or-later` and preserve notices and provenance.  
Consequences: Distribution must follow the license; this ADR is not legal advice.

## ADR-018: Use generated and real media fixtures
Status: accepted (2026-11-20)

Context: Hardware paths need reproducible streams plus real-world coverage.  
Decision: Generate small AVC/HEVC/AV1 MP4/fMP4 fixtures and later add user-provided real media.  
Consequences: Fixtures stay short to control repository size.

## ADR-019: Support owned and injected devices
Status: accepted (2026-11-20)

Context: Standalone use favors automatic device creation while player integration favors reuse.  
Decision: Each backend offers constructors for an internally created device and an injected device/queue.  
Consequences: Injection is the zero-copy integration path.

## ADR-020: Limit first-release codec profiles to 8/10-bit 4:2:0
Status: accepted (2026-11-20)

Context: NV12/P010 cover 8/10-bit 4:2:0 and common hardware profiles.  
Decision: Support AVC Baseline/Main/High, HEVC Main/Main10, and AV1 Main at 8/10-bit 4:2:0.  
Consequences: 12-bit, 4:2:2, 4:4:4, AV1 High, and AV1 Professional are deferred.

## ADR-021: Reconfigure streams but surface device loss
Status: accepted (2026-11-20)

Context: Resolution/profile/DPB changes are normal; device loss requires application-wide coordination.  
Decision: Automatically recreate resolution-dependent decoder objects, but return a typed device-lost error.  
Consequences: The application owns full device recovery.

## ADR-022: Use typed scale parameters
Status: accepted (2026-11-20)

Context: FFmpeg filter expressions are a CLI/filtergraph concern rather than a GPU primitive.  
Decision: Accept explicit dimensions, source/destination rectangles, aspect policy, and supported filter selection.  
Consequences: Do not port FFmpeg's string dimension-expression evaluator.

## ADR-023: Keep presentation separate from playback timing
Status: accepted (2026-11-20)

Context: Rendering and A/V scheduling are different responsibilities.  
Decision: Render supports contain/cover/stretch, background color, rotation, viewport, resize, and VSync selection.  
Consequences: The renderer does not own the playback clock or A/V synchronization.

## ADR-024: Parse audio samples without decoding them
Status: accepted (2026-11-20)

Context: A useful MP4 demuxer must expose every track, while this project is focused on video decode.  
Decision: Emit generic samples for all tracks and typed configs for AVC/HEVC/AV1 and AAC/Opus.  
Consequences: Audio decoding is out of scope.

## ADR-025: Use the feature-gated `windows` crate
Status: accepted (2026-11-20)

Context: Typed COM bindings reduce handwritten ABI risk.  
Decision: Use narrowly selected `windows` crate features and hide binding types behind backend modules.  
Consequences: Revisit `windows-bindgen` only if dependency size or version coupling becomes material.

## ADR-026: Use Rust 2024 with MSRV 1.85
Status: accepted (2026-11-20)

Context: The workspace is new and can use the 2024 edition while retaining a reasonable compiler floor.  
Decision: Set `edition = "2024"` and `rust-version = "1.85"`; develop on the installed Rust 1.100 toolchain.  
Consequences: CI should verify both MSRV and the current stable toolchain.

## ADR-027: Name the crates `d3d-video` and `mp4-demux`
Status: accepted (2026-11-20)

Context: The GPU pipeline and container parser have separate reuse boundaries.  
Decision: Create those two crates in one workspace with root integration tests and examples.  
Consequences: Neither crate depends on an application binary.

## ADR-028: Preserve error semantics, not log wording
Status: accepted (2026-11-20)

Context: FFmpeg integer errors and log strings are not an appropriate Rust public API.  
Decision: Preserve failure categories and trigger conditions with typed Rust errors.  
Consequences: Exact FFmpeg log and error text is not a compatibility requirement.

## ADR-029: Require a zero-copy performance floor
Status: accepted (2026-11-20)

Context: The port must remain useful for playback.  
Decision: Avoid steady-state per-frame allocation, retain GPU surfaces through the pipeline, and target at least 90% of same-machine FFmpeg throughput.  
Consequences: Every backend wave records benchmarks and does not pass while below the target without a new ADR.

## ADR-030: Use Rust 1.100 as the toolchain baseline
Status: accepted (2026-11-20); supersedes ADR-026

Context: The available and user-selected toolchain is Rust 1.100.0, and support for Rust 1.85 is no longer required.  
Decision: Set `rust-version = "1.100"`, develop and gate with Rust 1.100.0, and retain Cargo edition 2024.  
Consequences: Rust versions older than 1.100 are unsupported unless a later ADR restores a lower MSRV.
