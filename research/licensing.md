# Licensing and provenance

## Finding

The selected FFmpeg files state that they are distributed under GNU LGPL version 2.1 or, at the
recipient's option, any later version. The tree-level `LICENSE.md` says the default FFmpeg
combination is LGPL-2.1-or-later unless GPL or nonfree components are enabled. The specifically
selected DXVA/D3D and MOV files inspected carry LGPL headers.

## Port rule

- New crates use SPDX `LGPL-2.1-or-later`.
- Preserve applicable copyright notices and identify FFmpeg plus commit
  `74f5ea45ca78988784b810ad9bbb148ea7ff76d8` as the behavioral/source reference.
- Do not copy code from optional GPL/nonfree FFmpeg components into these crates.
- Generated media fixtures must record their generation command and must not import third-party
  copyrighted content.

This is engineering provenance guidance, not legal advice.

## Primary sources

- Local FFmpeg `LICENSE.md` at the recorded commit.
- Local FFmpeg `COPYING.LGPLv2.1` at the recorded commit.
- License headers in `libavcodec/dxva2.c`, `libavfilter/vf_scale_d3d11.c`, and
  `libavformat/mov.c` at the recorded commit.
