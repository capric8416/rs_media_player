# FFmpeg D3D video and MP4 Rust port map

## Goal

Build a standalone Rust workspace containing `d3d-video` and `mp4-demux` from the selected FFmpeg
9-era source. The result must decode AVC/HEVC/AV1 through DXVA2, D3D11VA, and D3D12 Video; scale and
convert NV12/P010 through D3D9/11/12 video processors; render to an `HWND` or caller-owned surface;
and demux playback-focused MP4/fMP4 without linking FFmpeg.

Reference source identity and analysis limitations are recorded in
[`research/source-scope.md`](research/source-scope.md). Accepted design choices are authoritative in
[`DECISIONS.md`](DECISIONS.md).

## ADR index

- ADR-001..008: crate boundary, trait mapping, port scope, targets, and checkpoints.
- ADR-009..018: GPU execution, formats/color, stream and I/O forms, unsafe, licensing, and fixtures.
- ADR-019..029: device ownership, profiles, recovery, scale/render behavior, MP4 audio, bindings,
  original toolchain decision, crate names, errors, and performance.
- ADR-030: Rust 1.100 baseline; supersedes ADR-026.

## Planned units

`end-to-end-player-pipeline` is currently claimed. A unit is marked `done` only after its gate
evidence is linked here.

| Wave | Unit | State | Depends on | Completion evidence |
|---:|---|---|---|---|
| 0 | workspace-foundation: Cargo workspace, licenses, lints, shared test conventions | done 2026-11-20 | accepted ADRs | Rust 1.100 fmt pass; Clippy `-D warnings` pass; tests 2/2; metadata/license/safety scans pass |
| 0 | public-contracts: codec/frame/color/error types plus `D3dDecode`, `D3dScale`, `D3dRender` | done 2026-11-20 | workspace-foundation | fmt; Clippy `-D warnings`; rustdoc `-D warnings`; tests 5/5; safety scan; diff/bench N/A for new API contracts |
| 1 | bitstream-core: bit reader, RBSP, NAL framing, OBU framing | done 2026-11-20 | public-contracts | source-characterized cases 20/20; workspace tests 37/37; fmt/Clippy/rustdoc/safety pass; process replay unavailable (no FFmpeg test executable) |
| 1 | mp4-box-io: bounded box reader and `Read + Seek`/incremental input | done 2026-11-20 | workspace-foundation | source-characterized cases 13/13; workspace tests 17/17; fmt/Clippy/rustdoc/safety pass; process replay unavailable (no ffprobe) |
| 2 | mp4-sample-tables: tracks, timing, seeking, moov and moof/traf/trun | done 2026-11-21 | mp4-box-io | source-characterized classic/fragment/seek cases 8/8; workspace tests 88/88; fmt/Clippy/rustdoc/safety pass; typed deferrals recorded; CLI gate unavailable |
| 2 | mp4-codec-config: avcC, hvcC, av1C, AAC, Opus | done 2026-11-21 | mp4-box-io, public-contracts | source-characterized configuration/normalization cases 11/11; workspace tests 80/80; fmt/Clippy/rustdoc/safety pass; AAC PCE deferral recorded; CLI gate unavailable |
| 2 | mp4-video-sample-description: video `stsd` entries, `pasp` and `clap` | claimed by /root 2026-11-22; AVC/HEVC/AV1 descriptions, pixel aspect and clean aperture parsing added | mp4-box-io, mp4-codec-config | crop precedence, encryption, real-file differential comparison and rust-anything gate remain |
| 2 | mp4-video-track: connect video sample descriptions to sample tables | claimed by /root 2026-11-22; classic/fragmented tracks, bounded reads, version 0/1 `elst` mapping, synthetic decode-order timestamps/preroll and basic edit-aware sync seeking added | mp4-video-sample-description, mp4-sample-tables | exact FFmpeg keyframe/index-boundary behavior and decode-preroll seek integration, encrypted samples, real-file differential comparison and rust-anything gate remain |
| 2 | avc-parser-dpb | done 2026-11-21 | bitstream-core | source-characterized AVC/POC/DPB cases 14/14; workspace tests 52/52; fmt/Clippy/rustdoc/safety pass; CLI gate/process replay unavailable (no bridge metadata or FFmpeg executable) |
| 2 | hevc-parser-dpb | done 2026-11-21 | bitstream-core | source-characterized HEVC/RPS/DPB cases 7/7; workspace tests 59/59; fmt/Clippy/rustdoc/safety pass; typed deferrals recorded; CLI gate/process replay unavailable |
| 2 | av1-parser-refs | done 2026-11-21 | bitstream-core | source-characterized sequence/frame-reference/tile cases 10/10; workspace tests 69/69; fmt/Clippy/rustdoc/safety pass; typed picture-tool deferral recorded; CLI gate unavailable |
| 3 | d3d9-device-frame: DXVA2 device, surfaces, capability probing, synchronization | done 2026-11-21 | public-contracts | Rust 1.100 Windows x64 check; workspace tests 92/92; GUID/format tests; fmt/Clippy/rustdoc pass; DXVA2 adapter and GPU query checks require hardware |
| 3 | d3d11-device-frame: D3D11 device, textures, capability probing, synchronization | done 2026-11-21 | public-contracts | Rust 1.100 Windows x64 check; workspace tests 90/90; profile/format tests; fmt/Clippy/rustdoc pass; runtime adapter and GPU event-query checks require hardware |
| 3 | d3d12-device-frame: queues, resources, state tracking, fences, capability probing | done 2026-11-21 | public-contracts | Rust 1.100 Windows x64 check; workspace tests 96/96; profile/format/state/timeout tests; fmt/Clippy/rustdoc pass; GPU behavior requires hardware |
| 4 | d3d9-decode: `d3d9_decode.rs` for AVC/HEVC/AV1 | claimed by /root 2026-11-21; AVC/HEVC stateful submission, AV1 inter frames with `PRIMARY_REF_NONE` (including global motion), show-existing, DPB-resolved submission, render-aspect propagation and frame-drop query synchronization and poisoned-decoder handling for post-submit failure | codec parsers, d3d9-device-frame | decoded-frame hashes and stream-change cases; AV1 frames inheriting primary-reference state and GPU execution remain unverified |
| 4 | d3d11-decode: `d3d11_decode.rs` for AVC/HEVC/AV1 | claimed by /root 2026-11-22; AVC/HEVC stateful and AV1 inter frames with `PRIMARY_REF_NONE` (including global motion), show-existing, DPB-resolved submission, render-aspect propagation and frame-drop query synchronization and poisoned-decoder handling for post-submit failure | codec parsers, d3d11-device-frame | decoded-frame hashes and AV1 frames inheriting primary-reference state; GPU execution remains unverified |
| 4 | d3d12-decode: `d3d12_decode.rs` for AVC/HEVC/AV1 | claimed by /root 2026-11-22; stateful AVC/HEVC/AV1 inter frames with `PRIMARY_REF_NONE` (including global motion) plus `D3d12DecodeSession` send/receive, show-existing output, leased texture-array DPB, per-submit command resources and fence retention; prepared output slice is checked against picture parameters | codec parsers, d3d12-device-frame | end-to-end GPU decode, stream-change and decoded-frame checks remain unverified; AV1 frames inheriting primary-reference state; session is not hardware-gated |
| 5 | d3d9-scale: DXVA2 video processor in `d3d9_scale.rs` | claimed by /root 2026-11-22; processor GUID/output-format selection, NV12/P010/BGRA8 targets, crop/display-aspect rectangles, VideoProcessBlt, source-surface lease retention, event-query frame completion and conservative query-failure retention wired to `D3dScale` | d3d9-device-frame | GPU runtime/quality validation, HDR/color controls, processor reuse and capability reporting remain |
| 5 | d3d11-scale: port `vf_scale_d3d11` behavior in `d3d11_scale.rs` | claimed by /root 2026-11-22; D3D11 VideoProcessor blit, NV12/P010/BGRA8 output, crop/display-aspect rectangles, source-texture lease retention, conservative query-failure retention, format support checks and driver-default interpolation wired to `D3dScale` | d3d11-device-frame | GPU format/scale/color verification, HDR handling, size capability reporting and processor reuse remain |
| 5 | d3d12-scale: port `vf_scale_d3d12` behavior in `d3d12_scale.rs` | claimed by /root 2026-11-22; processor support/creation, `D3dScale` integration, tracked per-format transitions, NV12/P010/BGRA8 output, queue/fence-backed frame output, SDR color mapping, and stretch/contain/cover requests implemented | d3d12-device-frame | explicit interpolation selection, BGRA/P010/NV12 GPU verification, HDR/color conversion and full FFmpeg differential checks remain |
| 6 | d3d9-render: `d3d9_render.rs` presentation and external surfaces | claimed by /root 2026-11-22; NV12/P010 conversion through DXVA2 video processor to BGRA, RGB StretchRect to HWND-bound additional swap chain, resize, background fill and VSync/immediate mode wired to `D3dRender` | d3d9-device-frame, d3d9-scale | Windows x64 build/Clippy pass; hardware BGRA processor support and visual output remain unverified; rotation and external surfaces are incomplete |
| 6 | d3d11-render: `d3d11_render.rs` presentation and external textures | claimed by /root 2026-11-22; HWND flip-model BGRA swap chain, NV12/P010/BGRA8 video-processor presentation, capability-gated rotation, source-lease completion wait and resize drain wired to `D3dRender` | d3d11-device-frame, d3d11-scale | Windows x64 lint/build and CPU tests pass; GPU visual/resize/present validation, HDR and external-texture integration remain |
| 6 | d3d12-render: `d3d12_render.rs` presentation and external resources | claimed by /root 2026-11-22; D3dRender accepts decoder/scaler frames and runs D3D12 video-process scale/conversion to BGRA before HWND flip-model copy; caller-owned BGRA resources use `render_bgra_frame` | d3d12-device-frame, d3d12-scale | video-processor pass handles crop/rotation before copy; GPU visual/resize/present validation, HDR and rust-anything gate remain |
| 7 | end-to-end-player-pipeline | claimed by /root 2026-11-23; `Mp4VideoPlayback` wires seekable MP4 sample reads, timestamped decode submission, edit-list PTS filtering, scaling and presentation for matching D3D9/11/12 frame types; auto-config derives AVC/HEVC/AV1 setup from each MP4 sample description; runtime feedback fixed D3D11 BOOL/config handling, D3D9/11 `DXVA2_NoEncrypt` selection, D3D12 adapter/cross-queue transitions, HWND close handling, and FFmpeg-compatible D3D11 decoder-surface alignment | MP4, all requested backend units | Real AVC Baseline 1920x1080 playback/presentation confirmed for D3D9 and D3D12 through at least 1200 frames; Rust 1.100 fmt, Windows x64 check, linked example and Clippy pass; rerun D3D11 and graceful close, then verify display-matrix rotation, other codecs/profiles, timing, seek/edit lists and fault injection |
| 7 | differential-benchmarks | open | end-to-end-player-pipeline | >=90% FFmpeg throughput, allocation and copy audit |

Units in a wave may proceed independently only after all dependencies are done. Wiring is tested one
unit at a time so a regression has one attributable change.

## Recommended next unit

The next backend work is closing the Wave 4–6 hardware gates: run AVC/HEVC/AV1 decode, AV1
show-existing, scale/color conversion and presentation on a Windows adapter for each backend, then
record format and stream-change results. The current code has compile/lint/doc validation, but no
adapter execution or decoded-frame differential gate has been recorded.

## Open questions

None. New scope discoveries must be captured as new ADRs rather than silently changing an accepted
decision.

## Explicitly out of scope for the first release

- Linking to FFmpeg or exposing FFmpeg C structs in the public API.
- Software decoding and non-Windows graphics backends.
- Windows 7/8 and ARM64.
- Cross-backend D3D9/D3D11/D3D12 resource sharing.
- 12-bit, 4:2:2, 4:4:4, AV1 High/Professional, and surface formats beyond ADR-012.
- Audio decoding, playback scheduling, and A/V synchronization.
- CENC decryption, HEIF, and uncommon QuickTime metadata.
- FFmpeg scale expression strings.

## Checkpoint policy

At every wave boundary update this map, generate the measured port map when rust-anything crates
exist, write `HANDOFF.md`, and report tests, differential counts, benchmark results, fallback or
unimplemented capability counts, and remaining risks. Continue only after user approval.






