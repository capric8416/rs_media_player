# D3D video and MP4 crates

This workspace is a standalone Rust port of the selected FFmpeg D3D video and MP4 demux behavior.
It does not link to FFmpeg at runtime.

The port was developed with the Rust Anything migration workflow and references the pinned
FFmpeg 9 source identified in [`PROVENANCE.md`](PROVENANCE.md).

## Crates

- `d3d-video`: Windows DXVA2, D3D11VA, and D3D12 Video decode, processing, and presentation.
- `mp4-demux`: playback-focused MP4 and fragmented MP4 parsing.

The implementation plan is in [`MAP.md`](MAP.md), accepted architecture decisions are in
[`DECISIONS.md`](DECISIONS.md), and provenance is in [`PROVENANCE.md`](PROVENANCE.md).

## Toolchain

- Cargo edition: 2024
- Minimum supported Rust version: 1.100
- Primary target: `x86_64-pc-windows-msvc` on Windows 10/11

## Foundation checks

Run from this directory:

```powershell
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --all-targets
```

Each later migration unit adds behavioral differential cases before it can pass its gate.

