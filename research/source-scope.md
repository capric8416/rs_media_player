# Source scope and analysis

## Reference identity

- Local tree: `C:\vcpkg\buildtrees\ffmpeg\src\n9.0.2-2dce354851.clean`
- Git commit: `74f5ea45ca78988784b810ad9bbb148ea7ff76d8`
- Git description observed on 2026-11-20: `2026.10.16-544-g74f5ea45ca`

The folder name says `n9.0.2`, but its Git description is a later snapshot-like identifier. Every
recording and provenance notice must therefore use the commit hash, not infer behavior from the
folder name alone.

## Direct source set

Decode entry points:

- `libavcodec/dxva2.c`, `dxva2_internal.h`, `dxva2_h264.c`, `dxva2_hevc.c`, `dxva2_av1.c`
- `libavcodec/d3d12va_decode.c`, `d3d12va_decode.h`, `d3d12va_h264.c`,
  `d3d12va_hevc.c`, `d3d12va_av1.c`
- `libavcodec/d3d11va.c` and `d3d11va.h` provide public compatibility/context pieces; D3D11
  codec submission is shared with the DXVA2 codec mapping files.

Scale entry points:

- `libavfilter/vf_scale_d3d11.c`
- `libavfilter/vf_scale_d3d12.c`

MP4 entry points:

- `libavformat/mov.c`, `mov_chan.c`, `mov_esds.c`, plus the tables and helpers named by the MOV
  Makefile target.

## Size and coupling findings

The directly named decode/scale files are several thousand lines, while `mov.c` alone is 12,223
lines in this tree. Codec submissions read internal AVC, HEVC, and AV1 decoder state; they are not
standalone bitstream-to-GPU functions. The MOV demuxer imports broad libavutil, libavcodec, and
libavformat infrastructure, including encryption and HEIF-related paths excluded by ADR-004.

The `rust-anything 2.1.0` whole-tree analysis saw 3,356 source files, about 1.42 million lines, and
2,022 C parse errors. Its broad unit ranking is not reliable enough to scaffold this scoped port
without manual boundary control. The dependency facts and Makefile object relationships above are
used to construct the scoped waves in `MAP.md`.

## Semantic risks

1. AVC/HEVC/AV1 DPB and reference ordering errors can decode successfully but corrupt later frames.
2. DXVA bitfield layouts and buffer alignment must match Windows ABI definitions exactly.
3. D3D12 requires explicit resource states, command allocator reuse discipline, and fence ordering.
4. MP4 sizes, offsets, timestamp arithmetic, and edit lists require checked integer operations.
5. Resolution changes must preserve ordered output while recreating resolution-dependent objects.
6. GPU output comparison requires controlled readback and tolerance rules for driver-dependent
   scaling; compressed decode should use exact hashes where the driver output is deterministic.

## Primary local sources

- FFmpeg Makefiles and files listed above at commit `74f5ea45ca78988784b810ad9bbb148ea7ff76d8`.
- `LICENSE.md` and `COPYING.LGPLv2.1` in the same tree.
