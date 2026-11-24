# Media fixtures

Committed media fixtures must be short, reproducible, and listed in `manifest.toml`. Generated
outputs and readback frames belong under `generated/` and are ignored.

Every fixture entry records:

- codec, profile, bit depth, chroma format, dimensions, and container form;
- whether it is MP4, fragmented MP4, Annex B, length-prefixed, or OBU;
- generation command or redistribution provenance;
- expected frame count and timestamps;
- expected parser output and decoded-frame digest where deterministic;
- features exercised, including resolution changes, B-frame reordering, HDR metadata, and errors.

Do not add third-party media without a redistribution-compatible license and provenance entry.

For local GPU playback smoke checks, `scripts/generate_playback_fixtures.ps1` creates short AVC
High, HEVC Main/Main10, and AV1 Main 8/10-bit MP4 clips with B-frame reordering under the ignored
`fixtures/generated/` directory. These synthetic files are disposable local inputs and are not
committed or used as golden decode outputs.

