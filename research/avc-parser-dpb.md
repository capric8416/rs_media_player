# AVC parser and DPB characterization

## Source oracle

The pinned source is FFmpeg commit `74f5ea45ca78988784b810ad9bbb148ea7ff76d8`.
The behavior for this unit is characterized directly from these LGPL sources because the local
tree contains no built FFmpeg decoder or parser test executable:

- `libavcodec/h264_ps.c` and `h264_ps.h`: SPS/PPS syntax, scaling-list fallback, bounds and
  unsupported FMO handling.
- `libavcodec/h264_slice.c` around `h264_slice_header_parse`: slice type normalization, picture
  structure, POC fields, reference counts, QP and deblocking syntax.
- `libavcodec/h264_refs.c`: reference-list modification, MMCO parsing/execution, sliding-window
  marking and default P/B reference-list ordering.
- `libavcodec/h264.h` and `h264dec.h`: limits (`16` DPB frames, `32` references, `67` MMCOs) and
  picture/reference structures.
- `libavcodec/dxva2_h264.c`: fields later consumed by DXVA2, D3D11VA and D3D12 Video picture
  parameters.

## Port boundary

This unit parses AVC NAL headers, SPS, PPS and hardware-relevant slice-header syntax. It owns POC
state, short/long-term DPB marking and default/ref-modified P/B lists. Macroblock entropy decoding,
SEI, SVC/MVC extensions, FMO and software reconstruction stay outside this unit. FMO and separate
colour planes return typed unsupported errors, matching the practical FFmpeg hardware path.

## Semantic traps

1. Slice values `5..=9` are fixed variants of `0..=4`; IDR NALs must contain I/SI slices.
2. Frame and field pictures use different `curr_pic_num` and `max_pic_num` domains.
3. SPS crop offsets are expressed in chroma/frame-dependent crop units, not pixels directly.
4. A missing scaling list predicts from the previous list or the JVT default; an explicitly
   present list whose first computed scale is zero selects the JVT default.
5. MMCO short-picture operands are decoded relative to `curr_pic_num` with wrapping. Operation 4
   carries `max_long_term_frame_idx_plus1`, so the stored upper bound is one less.
6. Default P lists use descending short-term frame number followed by ascending long-term index.
   B lists partition short references around current POC and swap the first two entries of list 1
   when both lists would otherwise be identical.
7. Sliding-window marking evicts the oldest short-term picture when short plus long references
   reaches `max_num_ref_frames`.

## Gate vectors

The gate covers baseline and High/High10 SPS, crop conversion, SPS/PPS bounds, unsupported FMO,
PPS extension fields, I/P/B and fixed slice types, IDR validation, reference-count overrides,
list modification, IDR/adaptive MMCO, POC LSB wrap, type-2 POC, P/B default lists, sliding-window
eviction, short-to-long conversion, reset, malformed/truncated RBSP and all checked limits.
