# AV1 parser and reference-slot characterization

## Source oracle

The local FFmpeg tree has no built AV1 parser/decoder executable, so characterization uses pinned
commit `74f5ea45ca78988784b810ad9bbb148ea7ff76d8` directly:

- `libavcodec/cbs_av1.h`: raw sequence/frame/tile structures and the eight reference-slot state.
- `libavcodec/cbs_av1_syntax_template.c`: sequence header, uncompressed frame header, short
  signalling, tile layout and reference update inference.
- `libavcodec/av1_parse.c` and `av1_parse.h`: OBU framing and reference-state helpers.
- `libavcodec/av1dec.c` and `av1dec.h`: reference-slot refresh, order-hint relative distance,
  skip-mode derivation and tile-group validation.
- `libavcodec/dxva2_av1.c` and `d3d12va_av1.c`: sequence/frame/tile fields consumed by Windows
  hardware decode.

## Port boundary

This unit parses sequence-header OBUs and the reference-bearing prefix of frame-header,
redundant-frame-header and frame OBUs for AV1 Main profile 8/10-bit 4:2:0. It maintains all eight
reference slots, frame IDs, order hints, refresh flags and show-existing-frame behavior. It also
parses uniform/non-uniform tile layout syntax at the tile-info cursor and validates ordered,
non-overlapping tile-group completion. The remaining uncompressed-header picture tools
(quantization, segmentation, loop filters, CDEF, restoration, global motion and film-grain payload)
are deliberately coupled to the later AV1 hardware picture-parameter unit, where every consumed
field can be checked against the DXVA/D3D12 structures. Decoder-model timing returns a typed
unsupported error. High/Professional profile chroma modes, 12-bit, operating-point layer filtering
and scalability metadata remain outside the accepted first release.

## Recorded semantics

1. `get_relative_dist(a, b)` sign-extends `(a - b)` using `order_hint_bits`; it returns zero when
   order hints are disabled.
2. A key or switch frame can refresh all eight slots. Otherwise each bit in
   `refresh_frame_flags` replaces exactly one slot after successful frame completion.
3. `show_existing_frame` selects one existing slot and does not parse a new coded frame; frame-id
   validation still applies when frame IDs are enabled.
4. Inter frames name seven logical references using eight physical slots. Short signalling fixes
   LAST and GOLDEN, then derives ALTREF/BWDREF/ALTREF2 and remaining references from relative order
   hints exactly as `set_frame_refs()` does.
5. When frame IDs are enabled, references whose stored ID falls outside the allowed delta window
   are invalidated before use.
6. Tile count is `tile_cols * tile_rows`; tile groups must be ordered, non-overlapping and finish
   at the final tile before the frame can refresh reference slots.
7. Reference state is committed only after the frame is complete; a truncated or failed frame must
   not partially refresh slots.

## Gate vectors

Ten source-characterized cases cover reduced-still-picture and normal Main/Main10 sequences,
order-hint wrap, frame-id invalidation, key/inter/show-existing frames, full and partial refresh
masks, short-signalled and explicit seven-reference selection, uniform/non-uniform tiles and
multi-group completion. Checked bit reads and slot lookup provide typed failures for truncated OBUs,
malformed indices and missing references. Switch-frame inference follows the same common parser
path and will receive an end-to-end stream fixture in the hardware integration wave.
