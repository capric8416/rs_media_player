# Provenance

This Rust port uses the Rust Anything migration workflow and takes FFmpeg 9 as its source and
behavioral reference. Rust Anything guided source mapping, incremental port boundaries, and gate
checks; the resulting crates are standalone Rust and do not link to FFmpeg at runtime.

The behavioral and source reference is the local FFmpeg tree:

- path: `C:\vcpkg\buildtrees\ffmpeg\src\n9.0.2-2dce354851.clean`
- commit: `74f5ea45ca78988784b810ad9bbb148ea7ff76d8`
- observed description: `2026.10.16-544-g74f5ea45ca`

The directory name and Git description disagree, so the commit hash is authoritative. Derived
source files must name the relevant FFmpeg source file and this commit in their module-level
provenance comment. New behavior, including the D3D9 scaler and all three renderers, must be marked
as such rather than represented as a direct file port.

See [`research/source-scope.md`](research/source-scope.md) for the selected source boundary and
[`research/licensing.md`](research/licensing.md) for the license rule.

