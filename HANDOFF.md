# Planning checkpoint handoff

The design interview is complete: ADR-001 through ADR-029 in [`DECISIONS.md`](DECISIONS.md) are
accepted. ADR-030 supersedes ADR-026 and sets Rust 1.100 as the baseline. `workspace-foundation`
completed on 2026-11-20: formatting, Clippy with warnings denied, 2/2 tests, Cargo metadata,
normalized license text, and safety/placeholder scans all pass.

Read next:

1. [`MAP.md`](MAP.md) for the wave plan and first unit.
2. [`GLOSSARY.md`](GLOSSARY.md) for shared names.
3. [`research/source-scope.md`](research/source-scope.md),
   [`research/windows-video-api.md`](research/windows-video-api.md), and
   [`research/licensing.md`](research/licensing.md) before foundation work.

`public-contracts` completed on 2026-11-20. It added shared codec, sample timing, frame, color/HDR,
typed error and synchronization contracts; separate `D3dDecode`, `D3dScale`, and `D3dRender`
traits; and a generic mock pipeline proving their associated frame types compose. Formatting,
Clippy and rustdoc with warnings denied, 5/5 workspace tests, and the safety/placeholder scan pass.
Differential recording and benchmarks are not applicable to these new backend-neutral contracts.

`end-to-end-player-pipeline` is now claimed (2026-11-23). `d3d-video/src/playback.rs` adds a
generic `Mp4VideoPlayback` chain shared by matching D3D9, D3D11, or D3D12 decode/scale/render
objects. It scans classic/fMP4 metadata, reads compressed access units one at a time by indexed
offset, derives decoder setup from AVC/HEVC/AV1 MP4 sample descriptions, submits timing in movie
timescale, filters visible output by frame PTS, and provides step/drain/seek/resize controls. A
D3D11 setup example is documented in `d3d-video/README.md`. Rust 1.100 host and Windows x64 checks,
Windows Clippy with warnings denied, and rustdoc pass. No tests were run in this continuation;
real MP4 playback and all adapter/GPU visual, seek, edit-list, device-loss and fault-injection
checks remain pending on a Windows test host. Do not mark this unit done until those checks are
recorded.

Follow-up on 2026-11-23: added `d3d-video/examples/playback_smoke.rs`, a Windows CLI smoke player
that creates its own HWND and runs the complete backend chain for `d3d9`, `d3d11`, or `d3d12`.
Example commands and track selection are in `d3d-video/README.md`. Rust 1.100 formatting, host and
Windows x64 workspace checks, workspace Windows Clippy, example Clippy and rustdoc pass. The
example compiles but has not been run against a real MP4 or a GPU; actual decode/presentation,
seek/edit-list correctness, device compatibility and fault injection remain open.

Follow-up 2 on 2026-11-23: linked the Windows x64 `playback_smoke.exe` successfully. Added
`scripts/generate_playback_fixtures.ps1` and generated five ignored local clips: AVC High with
B-frames, HEVC Main and Main10, and AV1 Main 8-bit and 10-bit. The PowerShell script parses cleanly;
the clips are synthetic inputs only, not validated decode goldens. The example has still not been
executed, so hardware playback remains unverified.

`mp4-box-io` completed on 2026-11-20. The local environment has no `ffprobe` executable, so its 13
characterization cases use the pinned `mov_read_default` and `mov_probe` source behavior directly:
8/16-byte headers, size zero extending to the parent, extended sizes, and child sizes clamped to the
parent. It provides checked `BoxHeader`/`FourCc` parsing, bounded seekable payload reads and nested
traversal, zero-copy incremental payload chunks with explicit consumption, allocation/buffer
ceilings, and typed failures. Workspace tests are 17/17; fmt, Clippy, rustdoc and safety scans pass.
Process replay and original-vs-Rust benchmark were unavailable because no FFmpeg executable exists.

`bitstream-core` completed on 2026-11-20. It adds checked MSB-first bit reads, unsigned/signed
Exp-Golomb and FFmpeg-compatible eight-byte LEB128; borrowed-or-owned RBSP extraction with escape
positions; Annex B and one-to-four-byte length-prefixed NAL iteration; and sized/unsized AV1 OBU
parsing. Its 20 source-characterized normal and malformed-input cases pass, bringing the workspace
to 37/37 tests. Formatting, Clippy and rustdoc with warnings denied, plus the safety scan, pass.
Process replay remains unavailable because the source tree contains no built FFmpeg test executable.

`avc-parser-dpb` completed on 2026-11-21. It adds checked AVC NAL, Baseline/High/High10 SPS, PPS and
hardware-relevant I/P/B slice-header parsing; scaling-list fallback; POC types 0/1/2; short/long
DPB state; sliding-window and adaptive MMCO; and default/modified P/B reference lists. FMO and
separate colour planes return typed unsupported errors. Its 14 source-characterized AVC/POC/DPB
cases pass, bringing the workspace to 52/52 tests. Formatting, Clippy and rustdoc with warnings
denied, plus the unsafe/unwrap/expect/panic/placeholder scan, pass.

The `rust-anything gate` command cannot consume this directly-created pure Rust crate because it
has no bridge-generated `rust-anything.json`; it reports that exact missing file. The equivalent
checks were run individually. Process replay and original-vs-Rust benchmarks remain unavailable
because the FFmpeg source tree contains no built parser/decoder executable. Source characterization
is documented in [`research/avc-parser-dpb.md`](research/avc-parser-dpb.md).

`hevc-parser-dpb` completed on 2026-11-21. It adds checked two-byte HEVC NAL headers; single-layer
Main/Main10 VPS/SPS/PPS and hardware-reference slice prefixes; explicit/predicted short-term and
long-term RPS; POC wrapping/BLA/IDR reset; RPS-driven DPB marking; and default, cyclic and modified
P/B lists. Seven source-characterized HEVC/RPS/DPB cases pass, bringing the workspace to 59/59
tests. Formatting, Clippy and rustdoc with warnings denied, plus the safety scan, pass.

Typed deferrals are recorded in [`research/hevc-parser-dpb.md`](research/hevc-parser-dpb.md): custom
scaling lists and additional/dependent slice segments must be closed during hardware integration;
multilayer, 3D, SCC and non-4:2:0 range extensions remain outside the accepted first-release scope.
As with the preceding units, CLI gate/replay/benchmark cannot run without bridge metadata and a
built FFmpeg parser executable; equivalent Cargo gates and direct source characterization were used.

`av1-parser-refs` completed on 2026-11-21. It adds checked AV1 Main/Main10 sequence headers;
reference-bearing key/inter/show-existing frame prefixes; frame-id invalidation and validation;
order-hint wrapping; explicit and short-signalled seven-reference selection; atomic eight-slot
refresh state; uniform/non-uniform tile layouts; and ordered tile-group completion. Its ten
source-characterized AV1 cases pass, bringing the workspace to 69/69 tests. Rust 1.100 formatting,
Clippy and rustdoc with warnings denied, plus the unsafe/unwrap/expect/panic/placeholder scan, pass.

The remaining AV1 uncompressed-header picture tools are explicitly deferred to the hardware
picture-parameter unit and recorded in [`research/av1-parser-refs.md`](research/av1-parser-refs.md).
`rust-anything gate` again cannot read this directly-created workspace because no bridge-generated
`rust-anything.json` exists; its equivalent Cargo gates passed. Process replay and original-vs-Rust
benchmarks remain unavailable because the pinned source tree has no built FFmpeg parser executable.

`mp4-codec-config` completed on 2026-11-21. It adds public, checked parsers and canonical encoders
for AVC and HEVC decoder-configuration records, AV1 Main/Main10 configuration records, AAC
`AudioSpecificConfig` and nested `esds` descriptors, and version-zero Opus `dOps` with FFmpeg-style
`OpusHead` conversion. Eleven source-characterized normal, malformed and round-trip cases pass,
bringing the workspace to 80/80 tests. Rust 1.100 formatting, Clippy and rustdoc with warnings
denied, plus the unsafe/unwrap/expect/panic/placeholder scan, pass.

AAC channel configuration zero needs a Program Config Element and returns a typed unsupported
error in this unit; the boundary and source evidence are in
[`research/mp4-codec-config.md`](research/mp4-codec-config.md). `rust-anything gate` cannot read
this directly-created workspace because no bridge-generated `rust-anything.json` exists; the
equivalent Cargo gates passed. Process replay and original-vs-Rust benchmarks remain unavailable
because the pinned tree contains no built `ffprobe` or callable parser executable.

`mp4-sample-tables` completed on 2026-11-21. It adds a per-track `SampleIndex` shared by classic
and fragmented MP4, checked `stts`/`ctts`, `stsc`, `stsz`/`stz2`, `stco`/`co64` and optional `stss`
expansion, DTS/PTS and byte-range generation, previous/next sync-sample seeking, plus `trex`,
`tfhd`, version-zero/one `tfdt` and `trun` default inheritance. Eight source-characterized cases
pass, bringing the workspace to 88/88 tests. Rust 1.100 formatting, Clippy and rustdoc with warnings
denied, plus the unsafe/unwrap/expect/panic/placeholder scan, pass.

Edit lists, sample groups, encryption auxiliary data, external data references and uncommon
QuickTime audio chunk coalescing are explicitly deferred in
[`research/mp4-sample-tables.md`](research/mp4-sample-tables.md). The pinned tree still has no
built `ffprobe`, so process replay and original-vs-Rust benchmarking are unavailable.

At the completed wave-2 checkpoint, `rust-anything map --write` generated
[`PORT_MAP.md`](PORT_MAP.md). It reports zero bridge units because this standalone workspace was
created directly and has no `rust-anything.json`; `rust-anything gate` has the same metadata
limitation. `MAP.md`, source-characterized cases and the passing Cargo gates are the authoritative
progress evidence.

`d3d11-device-frame` was approved and claimed by `/root` on 2026-11-21. Implement owned/injected
D3D11 devices, video capability probing, texture pools and synchronization without starting codec
submission, scaling or presentation behavior, then stop after its gate.

Skill to load: `rust-anything` and its whole-app workflow.

`d3d11-device-frame` completed on 2026-11-21. It adds owned or injected D3D11 device/context
construction, video interface retention, AVC/HEVC/AV1 Main profile and NV12/P010 capability
discovery, dimension-specific decoder config enumeration, decoder-bound array texture allocation,
and an event-query completion token with polling and timed waiting. Rust 1.100 Windows x64 check,
formatting, workspace tests (90/90), Clippy and rustdoc with warnings denied pass. Pure tests cover
the profile GUID and surface mappings and 64-slice bound. No Windows GPU is available here, so
adapter-dependent decoder configs, texture creation, query completion, device-loss behavior and COM
lifetime remain hardware CI checks. The rust-anything CLI gate remains unavailable for this
directly created crate because bridge metadata is absent. Next approved unit: `d3d9-device-frame`.

`d3d9-device-frame` completed on 2026-11-21. It adds D3D9Ex-first/classic D3D9 fallback device
creation, wrapping an existing device and DXVA2 manager, decoder and acceleration services, AVC
Baseline/Main/High (H.264 E/F and Intel ClearVideo), HEVC Main/Main10 and AV1 Main profile/format
probing, coded-size decoder config enumeration, fixed surface pool allocation for decoder and
processor target types, and D3D9 event-query polling/waiting. Device-lost/reset HRESULTs map to the
shared typed error. Rust 1.100 Windows x64 check, fmt, 92/92 workspace tests, Clippy and rustdoc
with warnings denied pass. Pure tests cover profile GUIDs and NV12/P010 FOURCC values. Runtime
adapter/surface/query/device-loss checks need a Windows DXVA2 GPU host. The CLI gate is unavailable
because this standalone workspace lacks rust-anything bridge metadata; equivalent Cargo gates and
the safety/placeholder scan pass. Next unit: `d3d12-device-frame`.

`d3d12-device-frame` completed on 2026-11-21. It adds D3D12 device creation/injection and video
interface discovery, AVC/HEVC/AV1 profile and NV12/P010 capability enumeration, coded-size decode
support/tier queries, default-heap texture arrays, tracked resource-state transitions, command
queue submission, and per-token fence signaling/polling/timed waits. Rust 1.100 Windows x64 test
compilation, formatting, all 96 workspace tests, Clippy and rustdoc with warnings denied pass. Four
pure tests cover profile/depth and format mapping, resource state validation and Win32 timeout
bounds. Runtime adapter queries, texture transitions, fence behavior, removal handling and COM
lifetime still require a D3D12 GPU host. The rust-anything CLI gate remains unavailable without
bridge metadata; the equivalent Cargo gates passed. Next unit requires approval: `d3d9-decode`.

`d3d9-decode` remains in progress (not marked done). The first implementation slice adds
`d3d9_decode.rs` with DXVA2 decoder creation, codec-prepared picture buffer validation/copy and
`BeginFrame`/`Execute`/`EndFrame` submission, plus completion query output. It also normalizes
AVC/HEVC NAL framing; assembles AVC/HEVC VCL bitstreams and short slice-control tables with FFmpeg's
128-byte padding behavior; serializes AV1 tile controls with checked ranges; validates AV1 OBU
framing; and can choose the driver configuration and allocate the required surface pool from
`DecoderConfig`. The AVC IDR path now serializes DXVA H.264 picture parameters and quantization
matrix and combines them with the access unit buffers for progressive 8-bit 4:2:0 IDR frames. The
HEVC IDR path now serializes the 232-byte picture-parameter record and 1000-byte default quantization
matrix for Main/Main10 4:2:0, and combines those records with the IDR access unit. The AV1 parser now
extracts tile byte ranges from tile-group OBUs with FFmpeg-style offsets and coordinates; complete
AV1 tile groups are checked for ordered complete coverage and concatenated with relocated offsets.
AV1 key/intra frame parsing now continues through frame size, super-resolution, render size,
intrabc/CDF flags, tile layout, quantization, segmentation features, and delta-Q/delta-LF controls.
Loop-filter levels/reference/mode deltas, CDEF strengths, and restoration types/unit shifts are now
parsed too, along with tx-mode/reduced-tx-set and intra-frame film grain. Key/intra global motion is
identity-inferred; AV1 now serializes the 912-byte DXVA picture record for key/intra frames, and
`from_av1_intra_obus` builds parameters plus tile buffers. Inter-frame global motion/reference
mapping remains. Rust 1.100 Windows
x64 check, 111/111 workspace tests, fmt,
Clippy and rustdoc with warnings denied pass. AVC/HEVC inter-picture reference state, AV1 picture
parameters, and integration with the backend-neutral `D3dDecode` trait remain. This is not a complete
`EncodedSample` decoder, so `d3d9-decode` stays claimed/incomplete in `MAP.md`.
Source details and exact boundary are in [`research/d3d9-decode.md`](research/d3d9-decode.md).
No GPU frame hashes or stream-change tests exist yet, so the unit stays claimed/incomplete in
`MAP.md`.

Follow-up on 2026-11-22: `D3d9Decoder` now has direct `submit_avc_idr_sample`,
`submit_hevc_idr_sample`, and `submit_av1_intra_obus` entry points. They run the existing codec
parameter builders and submit the resulting picture through the same checked DXVA2 transport,
making the supported key/intra cases callable without manually assembling `Dxva2Picture`. Rust
1.100 Windows x64 test compilation passes; formatting was applied. At this checkpoint the work
still did not implement queued `EncodedSample` decode, reference-surface management, decoded
`GpuFrame` output, or the backend-neutral `D3dDecode` trait. No hardware decode was available.

Follow-up 2 on 2026-11-22: `D3d9DecodedFrame` now owns a retained output surface, frame timing/color
metadata, and its completion token, implements `GpuFrame`, and is returned by
`D3d9Decoder::submit_picture_frame`. This exposed that the backend-neutral `GpuFrame: Send` bound
was incompatible with D3D9 COM resources, so the bound was removed and the trait docs now state
that each backend's thread-affinity rules apply. Rust 1.100 Windows x64 test compilation and
Clippy pass; the workspace remains 111/111 tests, with formatting and rustdoc passing. This is a
prepared-picture submission/frame wrapper, not a stateful `D3dDecode` implementation; surface
reuse policy remains caller-managed and GPU validation is still outstanding.

Follow-up 3 on 2026-11-22: added `decode_avc_idr_sample`, `decode_hevc_idr_sample`, and
`decode_av1_intra_obus` methods. Each builds the supported codec picture from its input and returns
`D3d9DecodedFrame` with the sample PTS/duration (or explicit AV1 timing) and caller-supplied color
metadata. Rust 1.100 workspace tests remain 111/111; Windows x64 compile, Windows-target Clippy,
fmt and rustdoc pass. The APIs still cover only the already-supported IDR/key/intra picture subset;
reference-picture DPB scheduling and the general `D3dDecode` trait path are not implemented.

Follow-up 4 on 2026-11-22: began the AVC inter-picture DXVA translation layer. `avc/dxva.rs` now
has checked serializers for the H.264 DPB fields: `RefFrameList`, `FieldOrderCntList`,
`FrameNumList`, long-term association bits, and `UsedForReferenceFlags`. The field order was
checked against the installed Windows SDK `dxva.h`; a field-level test covers signed POC, long-term
entry encoding, masks, and unused slots. The existing IDR record now goes through the empty-DPB
form of these serializers, preserving IDR semantics. Rust 1.100 d3d-video tests pass (76 plus 4
contract tests), Windows x64 compile and Clippy pass, and fmt passes. The reference fields are not
yet populated from live AVC DPB state, so P/B decode remains unsupported and `d3d9-decode` remains
claimed.

Follow-up 5 on 2026-11-22: connected `DecodedPictureBuffer` contents to that serializer through
surface bindings keyed by stable picture IDs. `picture_parameters_with_references` now creates
progressive 8-bit 4:2:0 non-IDR picture parameters from a caller-supplied current DPB picture and
mapped references; missing surface bindings fail with `InvalidState`, and reference IDs/field
counts are checked against DXVA limits. A P-slice test starts from a decoded IDR, maps its surface,
and verifies reference entry, POC, frame number, used-field flags, and cleared `IntraPicFlag`.
Rust 1.100 workspace tests pass (77 d3d-video unit tests, 4 contracts, and 32 MP4 tests = 113),
Windows x64 compile, target Clippy, fmt, and rustdoc pass. D3D9's public submission path does not
yet own or update this DPB state, so these non-IDR parameters are currently an internal tested
building block rather than a supported end-to-end P decode path.

Follow-up 6 on 2026-11-22: added public `AvcDecodeState` and wired it into
`D3d9Decoder::decode_avc_sample`. The state owns the active SPS/PPS, cloned-and-committed AVC DPB,
stable picture IDs, and each live reference's surface index. It prepares picture parameters and
reference-list validation on a DPB copy, then advances state only after `BeginFrame`/`Execute`/
`EndFrame` and completion-query insertion succeed. The decode entry accepts multi-slice access
units, checks that all VCL NALs describe the same frame picture, enforces configured dimensions and
AVC codec selection, and refuses to overwrite a surface still used by a DPB reference. An IDR then
a P-slice state test verifies commit behavior and surface protection. Rust 1.100 workspace tests
pass (78 d3d-video unit + 4 contracts + 32 MP4 = 114); Windows x64 compile, Windows-target Clippy,
fmt and rustdoc pass. Hardware behavior is unverified; POC/DPB feature coverage remains limited by
the existing parser, and general `D3dDecode` queue/drain/flush behavior is still unimplemented.

Follow-up 7 on 2026-11-22: `AvcDecodeState` now rejects in-band SPS/PPS changes unless callers
explicitly call `reconfigure`, and exposes `reset` for seek/flush reference invalidation. Reconfigure
rebuilds parameter state and starts a fresh DPB epoch; a decoder must also be recreated if coded
dimensions/profile change. State-owned parameter sets are boxed because the parser's fixed-capacity
PPS table is large; keeping it inline overflowed the test thread stack. DPB insertion and prepared
surface-map allocations now use checked reservations, and surface bindings are fully prepared
before hardware submission so commit itself does not allocate. Rust 1.100 tests remain 114/114;
Windows x64 compile, target Clippy, fmt and rustdoc pass. Hardware stream-change and seek recovery
remain unverified.

Follow-up 8 on 2026-11-22: while comparing the next HEVC reference-list mapping with FFmpeg 9 and
the installed Windows SDK `dxva.h`, found that `DXVA_PicEntry_HEVC.Index` is also seven bits. The
HEVC IDR serializer now rejects surface indices above 127, and the shared D3D9 submission path
checks the selected target against the codec's DXVA entry width (AV1 retains its byte-wide index).
Added an HEVC serializer boundary test. Rust 1.100 workspace tests remain 114/114; Windows x64
compile, target Clippy, fmt and rustdoc pass. HEVC inter-reference arrays are characterized but not
yet serialized/populated, so HEVC inter decode remains unsupported.

Follow-up 9 on 2026-11-22: added HEVC DXVA reference-table helpers matching the SDK's 15-entry
`RefPicList`/POC table and three eight-entry current RPS arrays. They validate seven-bit surface
indices, duplicate surface bindings, missing RPS mappings, and array capacities. IDR serialization
now uses the FFmpeg-compatible `0xff` value for unused `RefPicSetStCurrBefore/After` and
`RefPicSetLtCurr` entries (the previous zeros were incorrect). Field-level tests cover long-term
entry flags, signed POC, RPS-to-reference indices, and IDR padding. Rust 1.100 workspace tests pass
(79 d3d-video + 4 contracts + 32 MP4 = 115); Windows x64 compile, target Clippy, fmt and rustdoc
pass. The helpers are not yet connected to live HEVC DPB state or non-IDR picture submission.

Follow-up 10 on 2026-11-22: added HEVC DPB-to-DXVA surface conversion and parsed-RPS conversion.
The live DPB order now maps to the 15-entry `RefPicList`/POC inputs, while a parsed short/long RPS
maps used references into `StCurrBefore`, `StCurrAfter`, and `LtCurr` surface groups. Missing POCs
or surface bindings fail explicitly; unused RPS entries are excluded and each current set is
bounded to the SDK's eight slots. A test covers a short-term reference, an unused candidate, a
long-term reference, and missing surface mapping. Rust 1.100 workspace tests pass (80 d3d-video +
4 contracts + 32 MP4 = 116), as do Windows x64 compile, target Clippy, fmt, and rustdoc. These are
tested mapping helpers; HEVC state ownership and non-IDR submission are still not wired.

Follow-up 11 on 2026-11-22: generalized HEVC picture-parameter serialization to accept current POC,
mapped DPB entries, and the three parsed RPS surface lists; the IDR API remains a wrapper using
empty lists. Non-IDR serialization checks the current POC against the slice POC LSB and currently
requires the short-term RPS to be selected from the SPS (slice-local RPS bitstream serialization
metadata is not yet parsed). A P-slice test builds a parsed RPS, DPB and surface map, then verifies
current POC, short/long DPB entries, and `StCurrBefore`/`LtCurr` indices in the 232-byte record.
Rust 1.100 workspace tests pass (81 d3d-video + 4 contracts + 32 MP4 = 117); Windows x64 compile,
target Clippy, fmt and rustdoc pass. This serializer is not yet called by a stateful HEVC D3D9
submission path.

Follow-up 12 on 2026-11-22: added public `HevcDecodeState`, which owns VPS/SPS/PPS, HEVC POC/DPB,
and decoder-surface bindings with prepare/commit semantics. `D3d9Decoder::decode_hevc_sample` now
validates the configured codec and coded dimensions, checks in-band parameter-set changes, builds
the HEVC picture/scaling buffers, submits the picture, and commits state only on success. The
current slice parser handles one first-slice segment per access unit; additional slices are
explicitly rejected. An IDR state test verifies uncommitted preparation does not publish a surface,
commit binds the output, and reset clears references. Rust 1.100 workspace tests pass (82 d3d-video
+ 4 contracts + 32 MP4 = 118); Windows x64 check, target Clippy with warnings denied, fmt, and
rustdoc with warnings denied pass. No GPU runtime or decoded-frame comparison was available; HEVC
dependent HEVC slices, broader parameter-set changes, D3D11/D3D12 decode, and remaining
AV1/inter-picture gaps keep the `d3d9-decode` migration unit incomplete.

Follow-up 13 on 2026-11-22: extended the HEVC slice parser to read independent non-first slice
segments, including CTB-address widths derived from SPS dimensions. Stateful D3D9 HEVC submission
now passes all VCL NALs into the state preparer; it validates that the first segment starts the
picture, subsequent addresses increase, and slice headers share the same picture-level parameters.
The existing DXVA assembler already emits a slice-control record per HEVC VCL NAL, so all supported
independent slices are submitted together. Dependent segments remain explicitly unsupported because
their inherited header syntax requires additional prior-slice parsing state. The HEVC IDR state test
now submits a two-slice fixture through prepare/commit. Rust 1.100 tests (82 d3d-video + 4 contracts
+ 32 MP4 = 118), Windows x64 check, target Clippy with warnings denied, fmt, and rustdoc with
warnings denied pass. GPU runtime comparison remains unavailable.

Follow-up 14 on 2026-11-22: added `d3d11_decode.rs` with a D3D11 decoder transport. It creates a
video decoder from a device-advertised profile/config, allocates an NV12/P010 decoder texture array,
creates one output view per array slice, copies unique caller-prepared codec buffers through
`GetDecoderBuffer`/`ReleaseDecoderBuffer`, submits them within a begin/end frame pair, and returns a
`D3d11DecodedFrame` retaining the texture, surface index, metadata, and completion query. It exposes
a generic transport for codec adapters; AVC/HEVC/AV1 D3D11 parameter translation and stateful
`D3dDecode` integration are not implemented yet. Rust 1.100 workspace tests remain 118/118;
Windows x64 check, target Clippy with warnings denied, fmt, and rustdoc with warnings denied pass.
No D3D11 GPU was available for runtime validation.

Follow-up 15 on 2026-11-22: wired `D3d11Decoder::decode_avc_sample` and
`D3d11Decoder::decode_hevc_sample` to the shared codec parsers, reference-state preparers, and
bitstream/slice-control builders. DXVA buffer-kind numeric values are mapped to their corresponding
D3D11 decoder buffer kinds; AVC/HEVC picture and inverse-quantization buffers are submitted before
bitstream and slice-control buffers. Codec state commits only after D3D11 submission and frame
completion token creation succeed. Rust 1.100 workspace tests pass 119/119, Windows x64 check,
target Clippy with warnings denied, fmt, and rustdoc with warnings denied pass. A Windows-only unit
test verifies DXVA2 picture, inverse-quantization, slice-control, and bitstream kind mappings to
D3D11. D3D11 AV1,
`D3dDecode` queue/drain/flush integration, GPU runtime tests, and decoded-frame comparisons remain
open; the D3D11 decode migration unit is incomplete. The added mapping test brings the workspace
test total to 119 (83 d3d-video + 4 contracts + 32 MP4).

Follow-up 16 on 2026-11-22: added `D3d11Decoder::decode_av1_intra_obus` for AV1 key and
intra-only frames. The AV1 input builder now also returns the parsed coded dimensions; the D3D11
adapter checks these against the configured texture size, then submits the 912-byte picture record,
packed tile payload, and tile-control table through the D3D11 buffer transport. A parser test checks
that the reported dimensions match the picture record fixture. Rust 1.100 workspace tests remain
119/119, Windows x64 check, target Clippy with warnings denied, fmt, and rustdoc with warnings
denied pass. AV1 inter/show-existing-frame state, queue/drain/flush integration, hardware runtime
validation, and output hash comparisons remain open.

Follow-up 17 on 2026-11-22: added `d3d12_decode.rs` with D3D12 decoder and heap creation plus a
generic `decode_frame` recorder. It checks the selected profile at the coded dimensions, accepts
only supported and unique inline frame-argument kinds, verifies the compressed bitstream range is
inside a buffer resource, and requires output/reference texture metadata and resource states to be
`VIDEO_DECODE_WRITE`/`VIDEO_DECODE_READ` as appropriate. It keeps frame argument bytes, reference
resources/heaps, and output/bitstream resources alive across `ID3D12VideoDecodeCommandList::DecodeFrame`;
the existing queue owns close/execute/fence work. A unit test covers empty and duplicate argument
rejection. Rust 1.100 workspace tests pass 120/120 (84 d3d-video + 4 contracts + 32 MP4); Windows
x64 check, target Clippy with warnings denied, fmt, and rustdoc with warnings denied pass. Codec
specific AVC/HEVC/AV1 argument adapters, upload-buffer creation, D3D12 decoded-frame ownership,
queue integration, and hardware validation remain incomplete.

Follow-up 18 on 2026-11-22: added `D3d12Device::create_upload_buffer`, which allocates an upload
heap buffer in `GENERIC_READ`, maps it with an empty CPU read range, copies the compressed bytes,
and unmaps with the exact written range. `D3d12InputBitstream::from_upload_buffer` now builds the
whole-resource bitstream range for `DecodeFrame`. This follows FFmpeg 9's D3D12 decode bitstream
resource path. Rust 1.100 workspace tests remain 120/120; Windows x64 check, target Clippy with
warnings denied, fmt, and rustdoc with warnings denied pass. The helper compiles but has no GPU
runtime test; codec state/argument adapters and command-queue integration remain open.

Follow-up 19 on 2026-11-22: added `D3d12Decoder::prepare_avc_sample` and
`prepare_hevc_sample`. They reuse the tested codec parser, DPB state, parameter serializers, and
bitstream/slice-control builders; compressed bitstreams are copied into the D3D12 upload heap and
picture, inverse-quantization, and slice-control records become D3D12 frame arguments. The returned
prepared-picture guard borrows codec state mutably, records against caller-selected output/reference
textures, and exposes an explicit `commit` after command-list queue submission succeeds; calling
commit before recording now returns `InvalidState`, and dropping it leaves DPB state unchanged. A
unit test checks D3D12 argument-kind ordering. Rust 1.100
workspace tests pass 121/121 (85 d3d-video + 4 contracts + 32 MP4); Windows x64 check, target
Clippy with warnings denied, fmt, and rustdoc with warnings denied pass. D3D12 AV1 arguments,
queue/fence orchestration, decoded-frame wrapper, and GPU runtime checks remain incomplete.

Follow-up 20 on 2026-11-22: added D3D12 AV1 key/intra OBU preparation and recording. It validates
the selected AV1 profile and coded dimensions, builds picture parameters and tile-control data, and
uploads the unpadded tile payload as the compressed bitstream, matching FFmpeg 9's D3D12 argument
mapping. Exported `D3d12PreparedAv1Picture` from the crate API. Inter/show-existing-frame reference
state, decoded-frame ownership, queue/fence orchestration, and hardware validation remain open.
Rust 1.100 workspace tests pass 121/121 (85 d3d-video + 4 contracts + 32 MP4); Windows x64 check,
target Clippy with warnings denied, fmt, and rustdoc with warnings denied pass. D3D12 AV1 inter/
show-existing-frame reference state, decoded-frame ownership, queue/fence orchestration, and hardware
validation remain open.

Follow-up 21 on 2026-11-22: added `d3d12_scale.rs` with FFmpeg-compatible NV12/P010 DXGI color-space
selection, D3D12 video-process support querying and processor creation, plus single-input
`ProcessFrames` recording. Recording validates texture size/format, subresource indices and required
VIDEO_PROCESS_READ/WRITE resource states; `D3d12Texture::transition_process` records tracked
transitions on a video-process command list. `record_request` now applies validated source and
destination rectangles with stretch/contain/cover geometry, and explicitly rejects filter/color
requests this fixed processor cannot honor. The caller retains command allocator/list reset, queue
submission and fence synchronization responsibility. Rust 1.100 workspace tests pass 123/123 (87
d3d-video + 4 contracts + 32 MP4), Windows x64 check, target Clippy with warnings denied, fmt, and
rustdoc with warnings denied pass. `D3dScale` trait integration, output frame ownership, queue/fence
ownership, and hardware color/scale validation remain open; the scale unit is partial.

Follow-up 22 on 2026-11-22: added `D3d12Scaler`, implementing the shared `D3dScale` trait, and
`D3d12ScaleFrame`, a `GpuFrame` wrapper carrying texture metadata and a completion token. D3D12
output textures now use the render-target flag required by video processing. The scaler owns a
video-process queue/allocator/list, waits for input completion, tracks VIDEO_PROCESS_READ/WRITE
transitions, submits the operation and waits for its fence before returning; this synchronous path
keeps resources alive safely while the allocator/list is reused. Added distinct input/output color
configuration, per-adapter format-pair probes at 1080p, and even-dimension validation for NV12/P010.
Rust 1.100 workspace tests pass 124/124 (88 d3d-video + 4 contracts + 32 MP4), Windows x64 check,
target Clippy with warnings denied, fmt, and rustdoc with warnings denied pass. Runtime GPU checks,
asynchronous frame pipeline, broader dynamic capability enumeration, and HDR/color accuracy checks
remain open; the D3D12 scale unit remains partial.

Follow-up 23 on 2026-11-22: hardened D3D12 scaler failure handling. Queue idle timeout now aborts
before allocator reset; abandoned command-list recording restores the input texture's tracked state.
Fence events are allocated before queue signaling. If submitted GPU work cannot be tracked to
completion, the scaler rejects future submissions and retains referenced resources for process
lifetime to prevent premature release. Rust 1.100 workspace tests pass 124/124, Windows target
Clippy with warnings denied, fmt and rustdoc pass. Failure paths and GPU behavior still require
hardware validation.

Follow-up 24 on 2026-11-22: D3D12 processor creation now rejects output dimensions outside the
adapter-reported scale range and follows FFmpeg's 1:1 pixel-aspect fallback when that feature is
unavailable. Added coverage for output-size range boundaries. Rust 1.100 workspace tests pass
125/125 and Windows target Clippy with warnings denied passes. Hardware behavior and other
scale-support flags remain to be verified.

Follow-up 25 on 2026-11-22: D3D12 processor configuration now maps explicit full-range YCbCr
metadata to the corresponding DXGI full-range spaces and rejects full-range PQ when no matching
DXGI space exists. The original FFmpeg-compatible limited-range mapping remains available. Added
coverage for full-range Rec.709 and unsupported full-range PQ. Rust 1.100 workspace tests pass
126/126 and Windows target Clippy with warnings denied passes; GPU color accuracy remains untested.

Follow-up 26 on 2026-11-22: the D3D12 scaler now honors the adapter's POW2_ONLY size restriction
using matching width/height halving steps from its reported maximum, and rejects PQ/HLG metadata
that cannot be mapped to the supported P010 BT.2020 DXGI spaces. The size-range test covers matching
and mismatched halving steps. Rust 1.100 workspace tests pass 126/126 and Windows target Clippy with
warnings denied passes. GPU behavior and HDR output remain unverified.

Follow-up 27 on 2026-11-22: D3D12 scale processing now checks that input/output resources and the
video-process command list belong to the processor device. The managed scaler checks before
recording transitions, and the low-level recorder checks independently. Rust 1.100 workspace tests
pass 126/126 and Windows target Clippy with warnings denied passes. Cross-device rejection and GPU
synchronization require runtime validation; caller-wrapped input textures still require external
work to be synchronized before wrapping.

Follow-up 28 on 2026-11-22: D3D12 AVC/HEVC prepared-picture `commit` now returns its upload buffer;
callers must retain it until the decode fence completes. This changes the previous `VideoResult<()>`
return type and fixes immediate buffer release on commit when the caller follows the new contract.
AV1 prepared-picture documentation likewise requires retention through completion. Added
`D3d12ScaleFrame::from_texture_with_completion` for fence-backed decode output; the frame waits before
dropping its texture, while producer-owned allocator, decoder and upload buffers remain the caller's
responsibility. Rust 1.100 workspace tests pass 126/126; Windows target Clippy, fmt and rustdoc pass.
Decode submission ownership and GPU runtime validation remain incomplete.

Follow-up 29 on 2026-11-22: D3D12 scale frames can now select a texture-array slice, including
when wrapping a decode completion token; the scaler passes that slice to `ProcessFrames`. D3D12
decode recording now rejects out-of-range output and reference array slices. Added array-slice bounds
coverage. Rust 1.100 workspace tests pass 127/127 and Windows target Clippy passes with warnings
denied. Texture ownership and state tracking still apply to the whole array, so callers must
synchronize all its slices before transitions; GPU runtime validation remains outstanding.

Follow-up 30 on 2026-11-22: introduced `D3d12PendingDecode` to retain the decode upload buffer,
decoder and heap through a caller-supplied completion fence. AVC/HEVC prepared pictures can commit
with a completion token; AV1 pictures can hand resources to the same guard. The token must be
signaled after the actual decode submission; output/reference textures and command allocator remain
caller-owned. Rust 1.100 workspace tests pass 127/127 and Windows target Clippy passes with warnings
denied. This is a resource-lifetime increment, not full decode queue integration or GPU validation.

Follow-up 31 on 2026-11-22: `D3d12PendingDecode::retain_submission_resources` can retain a caller's
decode command list, allocator, output texture and reference textures alongside the decoder and
upload buffer. It checks that all supplied objects belong to the fence device and refuses a second
registration; on fence failure it retains those COM resources for process lifetime. Callers must
provide every referenced resource, avoid resetting the allocator before completion and signal the
token after submission. Rust 1.100 workspace tests pass 127/127 and Windows target Clippy passes
with warnings denied. Hardware submission and failure paths remain unverified.

Follow-up 32 on 2026-11-22: D3D12 `decode_frame` checks that the command list, compressed bitstream,
output texture and every reference texture belong to the decoder's device before recording GPU
work. Rust 1.100 workspace tests pass 127/127 and Windows target Clippy passes with warnings denied.
Cross-device rejection still requires GPU runtime validation; decode queue integration remains open.

Follow-up 33 on 2026-11-22: D3D12 completion tokens now use a manual-reset event, so concurrent
waiters observe the same completion signal rather than consuming it. A signaled wait rechecks the
fence and reports device loss or an unexpected early signal. Rust 1.100 workspace tests pass 127/127
and Windows target Clippy passes with warnings denied. Concurrent waits and device-loss behavior
still need hardware runtime validation; integrated decode submission remains open.

Follow-up 34 on 2026-11-22: added `D3d12Queue::execute_with_completion` to validate nonempty
command lists, queue type and device, allocate the event and fence value before submission, then
execute and signal a completion token. `D3d12SubmissionError::submitted` distinguishes validation
failure from a signal failure after execution; callers must retain all referenced GPU resources if
submission occurred and completion is unknown. List closure and allocator usage remain caller
responsibilities. Rust 1.100 workspace tests pass 127/127 and Windows target Clippy passes with
warnings denied. Automated prepared-picture submission and GPU runtime validation remain open.

Follow-up 35 on 2026-11-22: added `submit_recorded_decode` for closed video-decode command lists.
It checks queue and resource devices, retains the provided upload, decoder, heap, allocator, command
list and textures, then submits and returns `D3d12PendingDecode`. On post-submission signal failure,
it retains GPU resource references for process lifetime. Prepared picture upload buffers can now be
cloned for submission before separately committing AVC/HEVC state; callers must use the returned
`submitted` flag when handling errors and supply every referenced texture. Rust 1.100 workspace
tests pass 127/127; Windows target Clippy, fmt and rustdoc pass. GPU validation and fully automatic
codec state/queue integration remain open.

Follow-up 36 on 2026-11-22: AVC/HEVC prepared D3D12 pictures now offer `submit`, which validates
the closed list against the one used for recording and requires all recorded output/reference
textures in the retained resource set. A picture may be recorded only once. Submission commits DPB
state if command execution was submitted, including the signal-failure case, which requires decoder
recovery; validation failures leave state uncommitted. Caller-provided additional list resources and
allocator reset discipline remain required. Rust 1.100 workspace tests pass 127/127 and Windows
target Clippy passes with warnings denied. GPU runtime behavior and AV1 submission integration remain
unverified/incomplete.

Follow-up 37 on 2026-11-22: AV1 key/intra prepared pictures now record the command-list and output
resource identities, reject repeat recording, and expose `submit` using the same retained-resource
queue path as AVC/HEVC. `record` now requires a mutable prepared picture. Callers still supply all
other list resources and must not reset the allocator before completion. Rust 1.100 workspace tests
pass 127/127 and Windows target Clippy passes with warnings denied. AV1 inter/reference decoding and
GPU runtime verification remain open.

Follow-up 38 on 2026-11-22: D3D12 completion tokens now share their manual-reset event across clones,
and `D3d12PendingDecode::completion` exposes a clone for wrapping the decode output as a scale frame.
The pending guard still owns the other decode resources until the fence completes. Event ownership
is reference counted and the token is `Clone + Send + Sync`; a compile-time test checks those
traits. Rust 1.100 workspace tests pass 128/128, Windows target Clippy and rustdoc pass with warnings
denied. Cross-queue synchronization and real GPU behavior still need runtime validation.

Follow-up 39 on 2026-11-22: each shared D3D12 completion event is now registered with its fence
once, even with concurrent waits. Dropping the last token closes a registered event only after the
fence has completed and the event is signaled; an incomplete or unobservable registration retains
the handle for process lifetime to avoid premature closure after timeout. Rust 1.100 workspace tests
pass 128/128 and Windows target Clippy passes with warnings denied. This deliberately trades a
handle leak in unresolved/error cases for safety; hardware timeout and device-loss paths remain
unverified.

Follow-up 41 on 2026-11-22: D3D12 processor creation now uses actual source/destination rectangle
sizes for processor ranges and checks hardware scaling support against those sizes. Managed scaler
requests compute geometry before creating the processor; `create_for_request` supports the same
configuration in the low-level API, which rejects rectangle sizes different from those configured.
The reported EVEN_DIMENSIONS_ONLY flag is checked against the destination rectangle. This corrects
the previous whole-frame range mismatch for crop/contain/cover; GPU behavior remains unverified.

Follow-up 42 on 2026-11-22: the D3D12 scaler now configures square source and destination pixel
aspect ratios instead of substituting frame dimensions for pixel ratios, because the public frame
metadata has no sample-aspect-ratio field. Capability queries describe the full input sample size;
processor source/destination size ranges continue to describe the actual rectangles. Non-square
pixels and hardware crop/scale behavior remain unverified. Rust 1.100 workspace tests pass 128/128
and Windows target Clippy passes with warnings denied.

Follow-up 43 on 2026-11-22: D3D12 scaler now uses checked `execute_with_completion` and restores the
input's tracked state if submission fails before execution. If execution occurred but fence signaling
fails, it retains in-flight GPU resources and refuses further submissions. Command-list interface
casting now happens before opening the list, and an abandoned list that cannot be closed disables
the scaler. Rust 1.100 workspace tests pass 128/128 and Windows target Clippy passes with warnings
denied; command-list and fence error paths remain unverified on hardware.

Follow-up 44 on 2026-11-22: corrected the D3D12 reference-table contract: picture-parameter surface
indices address the submitted texture array, so callers must arrange entries by surface index rather
than compact DPB order. Stateful AVC/HEVC recording rejects reference arrays too short to contain
the active surface indices; generic recording rejects reference tables larger than the configured
decoder heap capacity. Added a sparse-index bounds test. This checks table coverage, not that a
given texture actually belongs to the codec picture mapped to that index; callers must maintain that
mapping. Rust 1.100 workspace tests pass 129/129 and Windows target Clippy passes with warnings
denied. Sparse nullable entries and GPU validation remain open.

Follow-up 45 on 2026-11-22: D3D12 decoder reference tables now use optional texture/subresource
entries, allowing sparse surface-index slots to be null. AVC/HEVC recording checks that every
retained codec reference index points to a nonempty slot; resource validation and lifetime tracking
skip empty slots. The optional reference-heap array is null for same-resolution references, matching
FFmpeg's D3D12 path. This changes the `record` and `decode_frame` reference argument type to
`&[Option<(&D3d12Texture, u32)>]`; the caller still guarantees the picture-to-texture mapping.
Rust 1.100 workspace tests pass 129/129 and Windows target Clippy passes with warnings denied.
Sparse-table behavior still needs GPU runtime validation.

Follow-up 46 on 2026-11-22: D3D12 AVC, HEVC and AV1 prepared-picture creation now rejects target
surface indices outside the configured decoder heap capacity. Added boundary and overflow coverage.
Rust 1.100 workspace tests pass 130/130 and Windows target Clippy passes with warnings denied.
Caller-managed surface-to-texture mapping and GPU runtime validation remain open.

Follow-up 47 on 2026-11-22: D3D12 decode now rejects any reference sharing the output's texture
resource, because this implementation tracks and transitions the entire resource and cannot express
simultaneous decode read/write states for separate array slices. Added resource-conflict logic tests.
This deliberately leaves devices requiring an output/reference texture array unsupported until
per-subresource state tracking is implemented. Rust 1.100 workspace tests pass 131/131 and Windows
target Clippy passes with warnings denied; no GPU runtime validation was performed.

Follow-up 48 on 2026-11-22: added `D3d12Texture::plane_subresources` for one-mip NV12/P010 texture
arrays. It validates the array slice and returns both the luma and chroma plane subresource indices;
the first plane is the index used in video decode/process argument structures. Rust 1.100 workspace
tests pass 132/132 and Windows target Clippy passes with warnings denied. This is groundwork only:
per-slice barriers/state tracking and shared output/reference arrays remain unimplemented.

Follow-up 49 on 2026-11-22: D3D12 textures now track state per array slice, with decode and process
transitions recording barriers for both NV12/P010 planes. Decode validates each selected slice and
allows different slices of the same texture array as output and references; scaler transitions only
its selected slice. `state()` now returns `Option`, indicating a common state or mixed states, and
`discard_slice_transitions` restores bookkeeping after unsubmitted barriers are discarded. The
caller is responsible for GPU queue ordering and submitted-state correctness. Rust 1.100 workspace
tests pass 132/132; Windows target Clippy, fmt and rustdoc pass. This is not hardware-validated;
reference-only tier support and GPU debug-layer verification remain open.

Follow-up 50 on 2026-11-22: D3D12 decode capability queries now return driver configuration
flags. Decoder creation rejects configurations requiring separate reference-only allocations and
non-aligned output heights when 32-line alignment is required, because padded/reference-only
surfaces are not yet implemented. Decode recording enforces tier-1 reference texture-array identity
and 128-byte compressed bitstream offset alignment. Rust 1.100 workspace tests pass 133/133; Windows
target Clippy, fmt and rustdoc pass. These checks do not implement reference-only output conversion,
padded surfaces or GPU runtime validation.

Follow-up 51 on 2026-11-22: D3D12 decode separates coded dimensions from allocated texture
dimensions. When the driver requires 32-line alignment, `surface_size` rounds the height up and
`create_output_texture` allocates that size; decode validates against the padded allocation while
picture parsing uses coded dimensions. Scale frames accept a visible size inside a larger texture;
managed scaling defaults its source rectangle to the visible area to avoid showing padding. Rust
1.100 workspace tests pass 134/134 and Windows target Clippy passes with warnings denied. GPU
validation, nonzero crop offsets, and reference-only decode allocations remain open.

Follow-up 52 on 2026-11-22: `D3d12ScaleFrame` now supports a validated nonzero visible origin.
The scaler interprets `ScaleRequest::source_rect` relative to the visible frame and translates it
to texture coordinates, including overflow and bounds checks. The origin must currently be supplied
by the caller; deriving it from codec/container crop metadata remains open. Rust 1.100 workspace
tests pass 135/135 and Windows target Clippy passes with warnings denied. Hardware crop/scale output
and reference-only decode allocations remain unverified/incomplete.

Follow-up 53 on 2026-11-22: `AvcDecodeState::visible_rect` and `HevcDecodeState::visible_rect`
expose SPS crop/conformance rectangles in texture coordinates. `D3d12ScaleFrame::set_visible_rect`
accepts that geometry when the visible size matches frame metadata; the caller still performs this
wiring. HEVC conformance-window multiplication and addition now use checked arithmetic to reject
overflowing malformed SPS data. Tests cover nonzero left/top offsets and malformed offsets. Rust
1.100 workspace tests pass 138/138 and Windows target Clippy passes with warnings denied. Container
crop precedence, AV1 visible geometry, automatic frame propagation, and GPU verification remain open.

Follow-up 54 on 2026-11-22: AV1 intra headers retain render width/height, and D3D12 prepared AV1
pictures expose `render_size` separately from decoded dimensions. Corrected AV1 DXVA picture width
and decoder-size validation to use the upscaled output width rather than super-resolution coded
width; tile parsing continues to use coded width. Tests cover distinct render size, super-resolution
parsing and DXVA width serialization. Rust 1.100 workspace tests pass 139/139; Windows target
Clippy, fmt and rustdoc pass. Render-size/sample-aspect propagation and GPU AV1 validation remain open.

Follow-up 55 on 2026-11-22: added a reduced `SampleAspectRatio` type and AV1 prepared-picture
conversion from decoded/render dimensions. D3D12 scale frames accept this ratio; the managed scaler
applies it when computing contain/cover geometry and produces square-pixel output. Callers must
explicitly transfer the AV1 ratio to the scale frame. Tests cover non-square geometry, reduction and
large values. Rust 1.100 workspace tests pass 141/141 and Windows target Clippy passes with warnings
denied. Low-level processor requests still assume square input pixels; end-to-end GPU validation and
automatic metadata propagation remain open.

Follow-up 56 on 2026-11-22: `mp4-demux` now parses video `stsd` descriptions for avc1/avc3,
hvc1/hev1 and av01, extracting dimensions, data-reference index, matching codec config and optional
`pasp` spacing. Child boxes are bounded, unknown children skipped, duplicate codec configs rejected,
and invalid zero `pasp` denominator leaves an earlier ratio unchanged. Tests cover all three codec
families, malformed/truncated entries, duplicate config and aspect handling. Rust 1.100 workspace
tests pass 144/144; workspace Clippy, fmt and rustdoc pass with warnings denied. Full track traversal,
`clap`, encrypted sample entries, real-file FFmpeg differential tests and rust-anything gate remain
open. This new unit is partial and has not passed the rust-anything gate.

Follow-up 57 on 2026-11-22: video sample descriptions now retain `clap` clean aperture values and
validate them against the sample dimensions. `CleanAperture::crop` computes integer edge cropping
using rational arithmetic and FFmpeg's pixel-center/truncation rule, including signed offsets;
duplicate, truncated and invalid apertures produce typed errors. Malformed `clap` is rejected here,
whereas FFmpeg normally ignores it unless strict error recognition is enabled. Tests cover offset,
half-pixel, bounds, truncation and duplicate cases. Rust 1.100 workspace tests pass 146/146; workspace
Clippy, fmt and rustdoc pass with warnings denied. Precedence versus bitstream/container crop, real
file differential tests, full track traversal and the rust-anything gate remain open.

Follow-up 58 on 2026-11-22: added `VideoTrack::from_stbl` to assemble AVC/HEVC/AV1 sample
descriptions with a classic sample index, reject missing or conflicting required boxes, and validate
every sample's one-based description index. `description_for` provides checked lookup. Callers must
currently locate `stbl` and provide track ID/timescale; `moov`/`trak` traversal, fragmented tracks,
sample I/O and edit lists remain open. Rust 1.100 workspace tests pass 148/148; workspace Clippy,
fmt and rustdoc pass with warnings denied. Real-file FFmpeg differential testing and rust-anything
gate remain unavailable/incomplete; this unit is partial.

Follow-up 59 on 2026-11-22: added `parse_moov_video_tracks` and `VideoMovie::read`. The movie reader
scans bounded top-level boxes, skips media payloads by seeking (including `mdat` before `moov`),
enforces a caller-supplied `moov` allocation ceiling, discovers video `trak` hierarchies, obtains
track ID/timescale from `tkhd`/`mdhd`, and omits non-video tracks. Tests cover video plus audio tracks,
late `moov`, missing/duplicate `moov`, allocation limits, malformed track metadata, and
`VideoTrack::read_sample` byte reads including buffer limits and truncated input. MP4 crate tests
pass 42/42; Rust 1.100 workspace tests pass 151/151, workspace Clippy and rustdoc pass with warnings
denied, and fmt passes. Raw `moof`/`traf` parsing, edit lists, real-file FFmpeg differential tests
and the rust-anything gate remain incomplete.

Follow-up 60 on 2026-11-22: integrated existing parsed `FragmentRun` values into `VideoTrack` and
`VideoMovie`, checking track identity and every sample's description reference before mutation; the
shared `SampleIndex` reserves capacity fallibly and enforces its sample ceiling. Tests cover valid
appends, wrong tracks, invalid description indexes and unchanged state on rejected appends. This is
the append half of fragmented playback only: parsing raw `moof`/`traf` boxes, `trex` defaults and
running fragment offsets through the movie reader remain open. MP4 crate tests pass 43/43; full
workspace verification follows.

Follow-up 61 on 2026-11-22: `VideoMovie::read` now parses `mvex/trex` defaults and consumes
top-level `moof`, nested `traf`, `tfhd`, optional `tfdt`, and one or more `trun` boxes. Runs for
video tracks append to their sample indexes; audio runs are parsed for data/decode-time continuation
but omitted from the video-only output. Explicit and implicit base offsets, run-to-run offsets, and
decode-time continuation when `tfdt` is absent are wired. The caller's metadata allocation ceiling
applies to both `moov` and `moof`. A synthetic file test checks two fragments, `mdat` sample offsets
and byte reads. MP4 tests pass 44/44; Rust 1.100 workspace tests pass 153/153; workspace Clippy,
fmt and rustdoc pass with warnings denied. Edit lists, CENC, real-file FFmpeg comparison and the
rust-anything gate remain open.

Follow-up 62 on 2026-11-22: added bounded version 0/1 `elst` parsing, empty-edit handling, and
movie-timescale PTS mapping for normal-rate edits. Movie parsing reads `mvhd`, attaches `edts/elst`
to video tracks, and exposes `VideoTrack::presentation_time_in_movie_timescale`; edit mappings use
checked-width integer arithmetic. Tests cover version widths, timebase conversion, empty edits,
boundaries, malformed inputs, unsupported rates, and movie-to-track wiring. MP4 tests pass 47/47;
Rust 1.100 workspace tests pass 156/156; workspace Clippy, fmt and rustdoc pass with warnings
denied. Non-unit rates, edit-list sample selection/reordering, real-file FFmpeg differential checks
and the rust-anything gate remain open.

Follow-up 63 on 2026-11-22: `EditList::map_presentation_times` now returns every matching movie
timestamp, preserving repeated edit entries. `VideoTrack::presented_samples` builds a bounded,
stable movie-time ordered list of sample indexes and timestamps, omitting empty edits and samples
outside media edit intervals. Tests cover empty segments, repeated intervals and ordering. Full
workspace verification follows; non-unit media rates, frame-overlap clipping at edit boundaries,
real-file differential comparison and the rust-anything gate remain open.

Follow-up 64 on 2026-11-22: checked the pinned FFmpeg 9.0.2 `mov_read_elst` and `mov_fix_index`
source. FFmpeg stores the media-rate field but does not apply it when fixing the sample index, so
Rust now retains but ignores non-unit rates instead of rejecting them. The default-compliance
parser also retains negative media times other than `-1`; timebase conversion uses nearest,
ties-away-from-zero rounding. Tests cover ignored rate values, permissive negative media times and
rounding. MP4 tests pass 49/49; Rust 1.100 workspace tests pass 158/158; workspace Clippy, fmt and
rustdoc pass with warnings denied. FFmpeg-compatible keyframe preroll/discard signaling, encrypted
samples, real-file differential checks and the rust-anything gate remain open.

Follow-up 65 on 2026-11-22: added `VideoTrack::decode_samples`, which emits decode-order sample
references per edit, including the previous sync sample as discard-only preroll, selected output
samples with mapped PTS, and tail samples through the post-edit sync boundary for reordered video.
`SampleIndex` now retains explicit `ctts` presence even when all offsets are zero. Tests cover
preroll flags and trailing reordered samples. This is a caller-facing decode plan, not the full
FFmpeg `mov_fix_index` implementation: the current synthetic decode timestamps normalize visible
output but do not yet match all FFmpeg index rewriting and keyframe-selection rules. Seek
integration, encrypted samples and real-file differential checks remain. MP4 tests pass 52/52;
Rust 1.100 workspace tests pass 161/161; workspace Clippy, fmt and rustdoc pass with warnings
denied.

Follow-up 66 on 2026-11-22: refactored edit-list decode sample construction into a context with
separate visible-sample, decode-boundary and frame-duration steps. This removes the Clippy
argument-count and function-length failures without changing output semantics. Rust 1.100 workspace
tests pass 161/161; workspace Clippy with warnings denied, fmt and rustdoc pass. Full FFmpeg
`mov_fix_index` compatibility, seek integration, encryption, real-file differential tests and the
rust-anything gate remain incomplete.

Follow-up 67 on 2026-11-22: added `VideoTrack::seek_sync_sample`, which maps edit-list and repeated
presentation intervals to movie time before selecting the previous or next sync sample. Tests
cover repeated mappings, previous/next selection and out-of-range targets. Rust 1.100 workspace
tests pass 161/161; workspace Clippy with warnings denied, fmt and rustdoc pass. This API provides
basic edit-aware seeking; exact FFmpeg sync-index selection, decode preroll integration, encrypted
samples, real-file differential checks and the rust-anything gate remain incomplete.

Follow-up 68 on 2026-11-22: added `d3d11_scale.rs` with a `D3d11Scaler` implementation of
`D3dScale`. It creates D3D11 video-processor enumerator/processor and per-frame input/output views,
submits `VideoProcessorBlt`, and returns the output texture with an event-query completion token.
The path supports NV12/P010 input/output and driver-default full-frame stretch; it checks the
enumerator's input/output format support flags and matches input texture format to frame metadata.
Unsupported filters, rectangles, custom color conversion, RGB formats and odd chroma dimensions
are rejected. This
follows FFmpeg 9.0.2 `vf_scale_d3d11`'s NV12/P010 format selection, content-size processor setup
and blit path. Rust 1.100 workspace tests pass 163/163; Windows x64 workspace Clippy with warnings
denied, fmt, rustdoc and target check pass. GPU runtime/color-output behavior, size capability
reporting and processor reuse are not yet implemented; this unit is partial and has not
passed the rust-anything gate. The gate tool was attempted and stopped because
`d3d-video/rust-anything.json` is absent (`run rust-anything bridge first`); FFmpeg's C source is
outside this standalone Rust workspace, so source behavior cases have not been recorded there.

Follow-up 69 on 2026-11-22: added `d3d9_scale.rs` with a `D3d9Scaler` implementation of `D3dScale`.
It queries the DXVA2 processor service lazily (so decode-only device setup remains available),
selects an advertised processor GUID and output target format, allocates an NV12/P010 processor
surface, submits a progressive single-sample `VideoProcessBlt`, and signals completion with the
D3D9 event query. Requests support driver-default full-frame stretch and reject unsupported
rectangles, filters, explicit color changes, RGB formats and odd NV12/P010 output sizes. Rust 1.100
workspace tests pass 165/165; workspace Clippy with warnings denied, fmt and rustdoc pass. GPU
runtime/quality, color conversion controls, processor reuse, and size capability reporting remain
unverified or incomplete; rust-anything gate remains unavailable because the crate lacks
`rust-anything.json` and FFmpeg source behavior cases were not recorded.

Follow-up 40 on 2026-11-22: added `D3d12Queue::wait_for_completion` for a GPU-side queue wait on
a completion token from the same device. The caller must retain the token/resources and record
resource barriers; this primitive does not change the existing synchronous scaler path. Rust 1.100
workspace tests pass 128/128 and Windows target Clippy passes with warnings denied. Cross-queue
decode-to-process behavior still requires GPU runtime validation.


Follow-up 70 on 2026-11-22: added `d3d11_render.rs` with `D3d11Renderer`, implementing the shared
`D3dRender` trait for an HWND-backed flip-discard BGRA swap chain. The renderer validates NV12/P010
frame metadata and video-processor input/output format support, calculates source/destination aspect
rectangles, converts through the D3D11 video processor, supports resize and VSync/immediate present,
and maps DXGI occlusion to `PresentResult::Occluded`. Rust 1.100 Windows x64 D3D11 Clippy passes;
workspace tests pass 165/165, workspace Clippy with warnings denied, fmt and rustdoc pass. This is
still a partial unit: nonzero rotation is rejected, external texture/surface presentation, HDR and
actual GPU visual/resize/present validation remain. The rust-anything gate remains unavailable
because `d3d-video/rust-anything.json` and recorded source cases are absent.

Follow-up 71 on 2026-11-22: added `d3d9_render.rs` with `D3d9Renderer`, implementing the shared
`D3dRender` trait. It waits on the DXVA2 frame completion token, stretches the decoder surface into
the D3D9 backbuffer using the shared aspect rectangle calculation, then presents to an HWND override.
It rejects null HWNDs, nonzero rotation, immediate presentation and resize requests that differ from
this device's fixed backbuffer. The adapter/driver must support YUV decoder-surface to backbuffer
`StretchRect`; that runtime capability is not guaranteed. Rust 1.100 Windows x64 check and D3D9
Clippy pass; workspace tests pass 165/165, workspace Clippy with warnings denied, fmt and rustdoc
pass. GPU output remains unverified. The rust-anything gate remains unavailable because
`d3d-video/rust-anything.json` and
recorded source cases are absent.

Follow-up 72 on 2026-11-22: added `d3d12_render.rs` with `D3d12Renderer` and `D3d12RenderFrame`.
The frame wrapper validates a same-device, single-sample BGRA8 texture and retains its producer
completion token. The renderer creates a direct queue, allocator/list and HWND flip-discard swap chain;
it waits for producer work, copies matching-size BGRA input through COPY_SOURCE/COPY_DEST transitions,
waits for its own fence and presents with VSync or immediate mode. Resize waits for queue idle before
DXGI buffer recreation. Rust 1.100 Windows x64 check/Clippy pass; workspace tests pass 165/165,
workspace Clippy with warnings denied, fmt and rustdoc pass. This is a partial renderer path: it does
not consume NV12/P010 decoder frames, convert YUV to BGRA, scale/crop, or use the D3D12 video
processor. Caller must place source in COPY_SOURCE and retain producer-side resources through its
completion token. GPU runtime and visual checks, plus the rust-anything gate, remain unavailable.

Follow-up 73 on 2026-11-23: extended all three scaler-to-present paths with BGRA8 output. D3D11
and DXVA2 now request/check BGRA8 processor output and canonical SDR BT.709/Gamma 2.2 metadata;
D3D11 renderer accepts these frames. DXVA2 output target enumeration checks A8R8G8B8 support. D3D12
texture state tracking now emits one barrier for single-plane BGRA and two for NV12/P010; its video
processor can allocate BGRA output, uses RGB full-range G22/P709, and transitions processed output
to COMMON. `D3d12RenderFrame::from_scale_frame` carries that output and fence into the D3D12 direct
queue copy renderer, which transitions COMMON/COPY_SOURCE around the copy. PQ/HLG to BGRA is
rejected because tone mapping is not implemented. Rust 1.100 Windows x64 workspace check/Clippy and
rustdoc pass; GPU runtime/color/presentation behavior remains unverified. Unit tests were updated for
BGRA acceptance but not run in this continuation.

Follow-up 74 on 2026-11-23: replaced the D3D9 renderer's fixed default-device backbuffer with an
HWND-bound additional swap chain. Resize now drains queued work and recreates the chain transactionally;
VSync/immediate changes likewise recreate it. Rendering uses the chain backbuffer, fills the requested
RGBA background, stretches the DXVA2 surface into the computed aspect rectangle, then presents.
Rust 1.100 Windows x64 workspace check, Clippy with warnings denied and rustdoc pass. Runtime
YUV-to-RGB `StretchRect` support is still driver-dependent and unverified; rotation and external
surfaces remain. Full decode codec support is still incomplete (notably AV1 inter/show-existing and
HEVC advanced syntax); this is not a completion claim for the overall port.

Follow-up 75 on 2026-11-23: D3D9 and D3D11 scalers now consume `ScaleRequest` source/destination
rectangles and Stretch/Contain/Cover aspect policy through the shared checked rectangle helper.
D3D11 records stream source/destination rectangles; DXVA2 passes them in the video sample while
keeping the full output target/constriction. Both reject odd 4:2:0 source boundaries and odd
4:2:0 destination boundaries while allowing odd BGRA output dimensions. Rust 1.100 Windows x64
workspace check and Clippy with warnings denied pass. GPU crop quality, contain-background behavior
and driver-specific rectangle limits remain unverified; HDR tone mapping and custom interpolation
remain unimplemented.

Follow-up 76 on 2026-11-23: D3D11 scale and render paths now query `ID3D11VideoContext1` and set
input/output DXGI color spaces before processor blits. YUV matrix, range, P010 PQ/HLG and RGB BGRA
spaces reuse the D3D12 mapping helper; BGRA output metadata is canonical SDR BT.709/Gamma 2.2, and
HDR-to-SDR is explicitly rejected until tone mapping is implemented. Rust 1.100 Windows x64 check
and Clippy pass. Adapter color-space behavior remains a GPU validation item.

Follow-up 77 on 2026-11-23: DXVA2 scaler now writes supported SDR color metadata to both
`DXVA2_VideoDesc.SampleFormat` and `DXVA2_VideoProcessBltParams.DestFormat` (range, matrix,
primaries, transfer, chroma and progressive sample mode). It returns typed `Unsupported` errors for
color spaces DXVA2 cannot express, including BT.2020, Display-P3, PQ and HLG, rather than relying on
an unreachable mapping branch. Rust 1.100 Windows x64 workspace check, Clippy with warnings denied,
formatting and rustdoc pass. Hardware interpretation of the DXVA2 metadata is unverified.

Follow-up 78 on 2026-11-23: the shared HEVC parser/state path now accepts dependent slice segments.
It parses the segment address and inherits picture/slice syntax from the preceding independent
segment, checks PPS and NAL picture identity, and still enforces ordered nonduplicate segments.
Because D3D9, D3D11, and D3D12 use this shared state path, all three codec adapters receive the
same behavior; their API docs were updated. Rust 1.100 Windows x64 check, Clippy with warnings
denied and rustdoc pass. No test suite was run in this continuation; GPU decode behavior remains
unverified, and other advanced HEVC syntax limits remain.

Follow-up 79 on 2026-11-23: D3D11 presentation now supports 90, 180, and 270 degree stream
rotation when the enumerated video processor advertises rotation. Aspect calculations use the
rotated dimensions and map the selected crop back into the source texture before the processor
blit. Unsupported adapters return a typed `Unsupported` error. Rust 1.100 Windows x64 check,
Clippy with warnings denied, formatting and rustdoc pass. Visual orientation and crop behavior
still need GPU validation; D3D9/D3D12 rotation remain unimplemented.

Follow-up 80 on 2026-11-23: added public `Av1DecodeState` for the eight AV1 reference slots,
including show-existing surface lookup, decode-target overwrite validation, and post-submit
key/intra slot commits, preserving slot invalidation parsed from the frame header. D3D9 and D3D11
now expose a `decode_av1_frame_or_show_existing` path that
either submits key/intra decode or reuses the selected output surface for display. D3D12 now offers
`prepare_av1_intra_obus_with_state`; its wrapper commits slots only when queue execution was
submitted, and callers can use the shared state to resolve show-existing surfaces. Rust 1.100
Windows x64 check, Clippy with warnings denied and rustdoc pass. AV1 inter-frame decode remains
unsupported, and slot/display behavior has not been GPU validated.

Follow-up 81 on 2026-11-23: D3D12 HWND presentation now has `render_scaled_frame`, which accepts
`D3d12ScaleFrame`, runs the D3D12 video processor to target-size BGRA using the requested source
aspect policy/destination rectangle, then copies and presents through the existing direct queue.
The video-process scaler is initialized lazily. This enables NV12/P010-to-HWND presentation and
BGRA scaling without a caller-managed intermediate. Non-black background and rotation are rejected
explicitly. Rust 1.100 Windows x64 check, Clippy with warnings denied, formatting and rustdoc pass;
GPU color, crop, and presentation behavior remains unverified.

Follow-up 82 on 2026-11-23: `ScaleRequest` now carries clockwise rotation. D3D11 scaler checks
video-processor rotation capability, rotates the stream and computes crop/aspect rectangles in
rotated coordinates. D3D12 scaler enables video-process orientation in support/configuration and
passes the requested orientation to `ProcessFrames`; the combined D3D12 HWND path now uses this
instead of rejecting rotation. DXVA2 scaler returns `Unsupported` for nonzero rotation. Rust 1.100
Windows x64 check, Clippy with warnings denied, formatting and rustdoc pass. Driver rotation support
and visual/crop behavior need GPU validation.

Follow-up 83 on 2026-11-23: D3D9 scaler capability discovery now probes DXVA2 video-processor
input/output format pairs at a small valid progressive size and reports only NV12/P010/BGRA outputs
accepted by the processor service. Unsupported format pairs are omitted; unexpected service or
backend errors still propagate. Rust 1.100 Windows x64 check, Clippy with warnings denied,
formatting and rustdoc pass. Actual-size driver limits remain checked when each scale operation
creates its processor.

Follow-up 84 on 2026-11-23: D3D11 scaler capability discovery now queries a progressive video
processor enumerator for each NV12/P010 input at 2x2 and includes only formats reported for both
input and output roles. Unsupported input/output combinations are omitted, while unexpected
enumerator errors propagate. D3D9/D3D11 minimum reported scale size is now 2x2 to reflect 4:2:0
constraints. Rust 1.100 Windows x64 check, Clippy with warnings denied, formatting and rustdoc pass.
Actual-size limits and runtime color/crop support remain adapter-dependent.

Follow-up 85 on 2026-11-23: added shared-`DecoderConfig` configure entry points for D3D11 and
D3D12. D3D11 now enumerates size-specific configs, selects an unencrypted config with raw mode 2
for AVC or mode 1 for HEVC/AV1, applies its minimum render-target count, and creates the surface
array. D3D9 now applies the same raw-mode and no-encryption policy instead of accepting any HEVC/
AV1 config. D3D12 resolves profile/bit-depth capability before creation. Rust 1.100 Windows x64
check, Clippy with warnings denied, formatting and rustdoc pass. These constructors create hardware
resources but do not yet implement the generic `D3dDecode` packet/drain/flush lifecycle.

Follow-up 86 on 2026-11-23: `d3d-video` now depends on the independent `mp4-demux` crate for
`avcC`/`hvcC`/`av1C` parsing. `DecoderConfig::parse_codec_private` extracts AVC SPS/PPS, HEVC VPS/SPS/
PPS, or the AV1 sequence-header OBU; it validates length-size, bit-depth and AV1 chroma consistency.
`DecoderConfig::create_codec_state` builds the shared AVC/HEVC/AV1 DPB/reference state. Rust 1.100
host and Windows x64 checks, Clippy with warnings denied, formatting and rustdoc pass. AVC/HEVC
multi-parameter-set activation and codec-private changes during a stream still require explicit
reconfiguration.

Follow-up 87 on 2026-11-23: added `D3d12DecodeSession` implementing the common send/receive contract
for AVC, HEVC and AV1. It owns a texture-array DPB, selects unleased/unreferenced output slices,
records per-picture decode command resources, submits to a video-decode queue, retains resources
through fence completion, and emits AV1 show-existing frames without a redundant decode. A
post-submit signal failure now requires reconfiguration; `flush` cannot resume an indeterminate
queue. Rust 1.100 Windows x64 check, Clippy with warnings denied, formatting, host check and rustdoc
pass. No tests or GPU execution were run; output/reference array behavior and stream changes remain
hardware-gate items.

Follow-up 88 on 2026-11-23: AV1 render dimensions now propagate as sample aspect through D3D9,
D3D11 and D3D12 decoded frames. D3D9/D3D11 scale and HWND presentation use that aspect when fitting
the source. D3D9/D3D11 decoded-frame drop waits for the frame query before releasing surface leases;
asynchronous scaler outputs retain source leases and COM textures until their completion query.
D3D12 now permanently retains the surface lease if its completion fence cannot be queried. Rust
1.100 Windows x64 check, Clippy with warnings denied, formatting, host check and rustdoc pass. No
tests were run; device-loss behavior is conservatively handled but unverified on hardware.

Follow-up 89 on 2026-11-23: D3D9 HWND rendering now routes NV12/P010 through the DXVA2 video
processor to BGRA before the swap-chain blit, instead of relying on driver-specific YUV `StretchRect`
conversion. It waits for queued blits before releasing temporary/source surfaces; uncertain
completion retains those resources and leases. Non-black background for this YUV conversion path,
rotation and external surfaces remain unsupported. D3D12 `D3d12ScaleProcessor` documentation was
corrected to describe its sample-aspect-aware rectangle path. Rust 1.100 Windows x64 check, Clippy
with warnings denied, formatting, host check and rustdoc pass. No tests or GPU visual validation
were run.

Follow-up 90 on 2026-11-23: the shared AV1 tail parser now decodes global-motion flags and
identity/translation/rotzoom/affine parameter payloads for the accepted error-resilient,
`PRIMARY_REF_NONE` inter subset. It performs AV1 signed-subexponential reconstruction from identity
defaults, derives the DXVA motion matrices and computes the shear-invalid flag used by
`DXVA_PicParams_AV1`. This extends all three hardware backends through their shared picture
translator. Rust 1.100 Windows x64 check, Clippy with warnings denied and formatting pass. No tests
were run; no bitstream differential or GPU decode validation has been done. Inter frames using
non-error-resilient modes or a non-none primary reference remain unsupported.

Follow-up 91 on 2026-11-23: D3D11 HWND rendering now waits for an immediate-context event query after
video-processor blits and Present, retaining the source texture and surface lease if completion
cannot be established. Resize drains the context before releasing swap-chain buffers. Existing
D3D11 render rotation is capability-gated; its color-space and format checks remain intact. Rust
1.100 Windows x64 check, Clippy with warnings denied, host check, formatting and rustdoc pass. No
tests or GPU runtime validation were run.

Follow-up 92 on 2026-11-23: D3D9 and D3D11 scaler submission now handles completion-query creation
failure after a successful GPU blit by retaining the input resource, output resource and any decoder
surface lease. This avoids returning an uncertain source surface to the pool when no completion
marker can be established. Rust 1.100 Windows x64 check, Clippy with warnings denied, formatting,
host check and rustdoc pass. No tests or GPU fault-injection were run.

Follow-up 93 on 2026-11-23: D3D9 and D3D11 decoders now become poisoned when picture submission
succeeds but completion-query creation fails. They reject later decode submissions and intentionally
retain decoder/device/output resources so the uncertain target cannot be reused or freed. This
matches the D3D12 session policy for post-submit fence failure. Rust 1.100 Windows x64 check,
Clippy with warnings denied, formatting, host check and rustdoc pass. No tests or GPU fault-injection
were run.

Follow-up 94 on 2026-11-23: D3D9/11 decoders also poison and retain their submission resources when
buffer submission/execution or `EndFrame` fails after a successful `BeginFrame`. This closes
post-begin paths where command completion and target reuse cannot be proven. Rust 1.100 Windows x64
check, Clippy with warnings denied, formatting, host check and rustdoc pass. No tests or GPU
fault-injection were run.


Follow-up 95 on 2026-11-23: the D3D12 `D3dRender` implementation now accepts the decoder/scaler
pipeline's `D3d12ScaleFrame` directly and runs the video-process BGRA conversion/fit pass before
presenting. Caller-owned BGRA resources retain a separate public `render_bgra_frame` entry point.
Rust 1.100 Windows x64 check, Clippy with warnings denied, formatting, host check and rustdoc pass.
No tests or GPU presentation validation were run.

Follow-up 96 on 2026-11-23: D3D12 renderer now forwards `RenderParameters.background` into the
video-processor output stream description, enabling adapter-supported non-black letterbox/pillarbox
colors in its pre-present conversion pass. The standalone scaler defaults to opaque black and exposes
`set_background_color` for caller control. Rust 1.100 Windows x64 check, Clippy with warnings denied,
formatting, host check and rustdoc pass. No GPU visual validation was run.

Follow-up 97 on 2026-11-23: D3D9 rendering now supports non-black backgrounds for NV12/P010 input.
It fills the backbuffer with the requested color and copies only the video rectangle from the scaler's
BGRA output, preserving the surrounding background without relying on DXVA2 AYUV background
interpretation. Rust 1.100 Windows x64 check, Clippy with warnings denied, formatting, host check and
rustdoc pass. No GPU visual validation was run.

Follow-up 98 on 2026-11-23: D3D9 render/decode error mapping now recognizes `D3DERR_DEVICELOST` and
`D3DERR_DEVICENOTRESET` rather than DXGI HRESULTs. D3D11 render/decode and D3D12 render/decode now
classify DXGI device removed/hung/reset HRESULTs as `VideoErrorKind::DeviceLost`. This gives callers
a consistent recovery signal across backends. Rust 1.100 Windows x64 check, Clippy with warnings
denied, formatting, host check and rustdoc pass. No tests or device-loss injection were run.

Follow-up 99 on 2026-11-23: consolidated HRESULT classification in the shared error module and
applied it to D3D9/11/12 device, decode, scale, and render helpers. D3D9 loss/reset HRESULTs and
DXGI removed/hung/reset HRESULTs now consistently produce `VideoErrorKind::DeviceLost` throughout
all backend operations. Rust 1.100 Windows x64 check, Clippy with warnings denied, formatting, host
check and rustdoc pass. No device-loss injection was run.

Follow-up 100 on 2026-11-23: bounded all GPU completion waits that previously retried 30-second
timeouts forever. D3D9/11 decoded-frame Drop, D3D12 pending-decode/render-frame/scale-frame Drop now
wait once and retain resources/leases on timeout or query/fence error. D3D9/11 renderer idle waits
return `Timeout`; D3D12 scaler marks itself failed and retains in-flight resources when a submission
does not complete in 30 seconds. A D3D12 scale frame also avoids a second blocking pending-decode
Drop after it already retained the timed-out resource. Rust 1.100 Windows x64 check, Clippy with
warnings denied, formatting, host check and rustdoc pass. No tests or hung-GPU fault-injection ran.

Follow-up 101 on 2026-11-23: expanded the accepted AV1 inter subset from error-resilient frames
with `PRIMARY_REF_NONE` to any inter frame using `PRIMARY_REF_NONE`. AV1 infers default segmentation,
CDF, loop-filter and identity global-motion state in this case, so the shared parser/DXVA translator
can process non-error-resilient frames without inheriting another frame's primary state. AV1 frames
whose primary reference is one of LAST..ALTREF remain unsupported. Rust 1.100 Windows x64 check,
Clippy with warnings denied, formatting, host check and rustdoc pass. No bitstream differential or
GPU decode validation was run.

Follow-up on 2026-11-24: investigated the user's first playback attempts. D3D11's
`SetMultithreadProtected` returns the prior BOOL state, not HRESULT; removed the invalid `.ok()`
error conversion. D3D12 cannot QI `ID3D12Device` to `IDXGIDevice`; its HWND renderer now resolves
`IDXGIAdapter1` by `GetAdapterLuid()` using `IDXGIFactory4::EnumAdapterByLuid`. D3D9 processor
capability probing used a 2x2 zero-frequency descriptor; changed to a progressive 640x480 30 Hz
descriptor and classify processor `E_FAIL` as unsupported capability. The smoke runner now prints
errors via `Display`, and root `README.md` explains that Cargo commands run under `rust/`. Rust
1.100 Windows x64 check/Clippy and a linked smoke executable pass. The actual sample/backend runs
have not been repeated; D3D9 support and all GPU output remain to be confirmed on the user's host.

Follow-up 2 on 2026-11-24: the user's second run reached decoder configuration on D3D9/D3D11 and
the first D3D12 process pass. D3D9/D3D11 incorrectly compared the driver's encryption GUID with a
zero GUID; Windows and FFmpeg use `DXVA2_NoEncrypt` (`1b81bed0-...`). Configuration selection now
matches FFmpeg: accept raw mode 1, prefer AVC mode 2, and strongly prefer `DXVA2_NoEncrypt` while
retaining FFmpeg's fallback to another compatible configuration. D3D12 decode now transitions all
output/reference texture planes back to their prior/common states on the video-decode queue before
cross-queue video processing; the process arguments also set alpha to 1.0 and restore input state
after processing. Rust 1.100 Windows x64 check, Clippy with warnings denied, and linked smoke build
pass. The real file is AVC Baseline 1920x1080 progressive, 24 fps with a -90 degree display matrix.
The runtime fixes have not yet been rerun on the user's adapter.

Follow-up 3 on 2026-11-24: user confirmed real AVC Baseline playback and presentation work through
D3D9 and D3D12 for at least 1200 frames. Closing the smoke window did not terminate because its
window procedure never posted `WM_QUIT`; it now handles `WM_DESTROY` with `PostQuitMessage(0)`, and
the playback loop returns without draining after a user close. D3D11 reached its first scale pass
but `CreateVideoProcessorInputView` returned `DXGI_ERROR_UNSUPPORTED`. Its decoder array texture
was allocated at the 1920x1080 coded dimensions; FFmpeg aligns AVC D3D11VA surfaces to 16 pixels
(1920x1088), and HEVC/AV1 to 128 pixels. D3D11 decoder texture allocation now uses those aligned
surface dimensions while decoder/frame metadata retain the coded size. Rust 1.100 Windows x64
check, Clippy with warnings denied, and linked smoke build pass. D3D11 runtime and graceful window
exit must be rerun on the user's adapter.

Follow-up 4 on 2026-11-24: graceful close is confirmed fixed, while D3D11 input-view creation still
returned `DXGI_ERROR_UNSUPPORTED` after surface alignment. The D3D11 scaler and renderer had put the
numeric `DXGI_FORMAT_NV12`/`P010` enum value in `D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC::FourCC`.
Microsoft documents that zero selects the resource's native DXGI format; the numeric DXGI enum is
not a FOURCC and the user's driver rejected it as unsupported. Both D3D11 input-view paths now use
`FourCC = 0`. Rust 1.100 Windows x64 check, Clippy with warnings denied, and linked smoke build pass.
The D3D11 runtime change has not yet been rerun on the user's adapter.
