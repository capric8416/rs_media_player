# HEVC parser and DPB characterization

## Source oracle

Behavior is characterized from pinned FFmpeg commit
`74f5ea45ca78988784b810ad9bbb148ea7ff76d8` because no built FFmpeg parser/decoder executable is
present locally:

- `libavcodec/hevc/ps.c` and `ps.h`: VPS/SPS/PPS limits and short-term RPS decoding.
- `libavcodec/hevc/hevcdec.c` (`hls_slice_header`): slice type, POC/RPS selection, reference counts,
  list modification, QP and filtering syntax.
- `libavcodec/hevc/refs.c`: DPB marking and ST-before/ST-after/LT reference categories.
- `libavcodec/hevc/parse.c`: POC MSB/LSB wrap behavior.
- `libavcodec/dxva2_hevc.c` and `d3d12va_hevc.c`: parameter fields consumed by Windows hardware
  decode.

## Port boundary

This unit owns HEVC NAL headers, single-layer Main/Main10 VPS/SPS/PPS state, explicit and predicted
short-term RPS, POC derivation, long-term references, DPB marking and L0/L1 construction. Multilayer,
3D, SCC, range extensions requiring non-4:2:0 formats, and software transform or entropy decoding
are outside the first Windows hardware profile. Custom scaling lists and additional/dependent slice
segments are deferred to hardware-backend integration. Every deferred form currently produces a
typed unsupported error rather than being silently misparsed and must be closed before decode gates.

## Semantic traps

1. HEVC NAL headers are two bytes; `forbidden_zero_bit`, temporal-id-plus-one, and layer id must be
   validated before parsing payload syntax.
2. Negative RPS deltas are accumulated downward and positive deltas upward. Predicted RPS entries
   are sorted with negative values closest to zero first.
3. POC wrap uses half of `MaxPicOrderCntLsb`; BLA/IDR reset the previous temporal-id-zero POC.
4. DPB membership is replaced by every picture's RPS: referenced frames absent from all current or
   following RPS categories cease to be references.
5. Default L0 order is ST-before, ST-after, LT; default L1 swaps the first two groups. Lists repeat
   cyclically when the active reference count exceeds the number of unique candidates.

## Gate vectors

The gate covers NAL validation, Main/Main10 parameter fields, dimensions/conformance crop, explicit
and predicted short-term RPS, POC wrap/reset, current/following and long-term DPB categories,
P/B default and modified lists, cyclic fill, missing references and malformed/truncated syntax.
