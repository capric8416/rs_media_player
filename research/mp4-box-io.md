# MP4 box I/O characterization

## Oracle availability

No `ffprobe.exe` is installed or present under `C:\vcpkg\installed`, so process-level recording is
not available for this unit. The oracle is the pinned FFmpeg source at commit
`74f5ea45ca78988784b810ad9bbb148ea7ff76d8`, principally `mov_read_default` and `mov_probe` in
`libavformat/mov.c`.

## Recorded header behavior

| Case | Pinned FFmpeg behavior | Rust contract |
|---|---|---|
| 32-bit size >= 8 | Eight-byte header; remaining bytes are payload | Same |
| size field == 1 | Read a 64-bit total size; header is sixteen bytes | Same |
| size field == 0 | Extend the box to the end of its parent/root | Same; incremental input waits for end-of-stream |
| declared child exceeds parent | Clamp the child payload to the remaining parent bytes | Same; retain declared and effective sizes |
| declared size smaller than header | Stop parsing that parent | Return typed `InvalidSize`; higher parser stops |
| incomplete base/extended header | Stop at EOF | Return `NeedMoreData`, or typed `Truncated` after EOF |
| arithmetic beyond addressable range | Signed/guarded paths stop or clamp | Return typed `Overflow` before seeking or allocation |
| unknown type | Skip its bounded payload | Expose the header and bounded skip/read operations |

FFmpeg treats the sixteen UUID bytes as the payload of a normal `uuid` box, not as part of the
generic 8/16-byte header. Codec-specific UUID parsing is therefore outside this unit.

## Safety constraints added by the Rust API

- Payload reads cannot cross an effective box end.
- A caller cannot accidentally parse payload bytes as a sibling header without consuming,
  skipping, or explicitly entering the payload as a child container.
- Whole-payload allocation requires an explicit caller maximum; large `mdat` boxes are streamed.
- Incremental buffering has a caller-selected hard limit.
- Offsets and sizes use checked arithmetic.

These constraints preserve the pinned parser's accepted size semantics while making implicit C
cursor invariants explicit.

## Required cases

The unit gate covers standard and extended headers, size zero, parent clamping, undersized boxes,
offset overflow, split incremental headers and payloads, final truncation, buffer limits, nested
seekable traversal, and invalid cursor state.

## Primary source

- Local `libavformat/mov.c`, functions `mov_read_default` and `mov_probe`, at commit
  `74f5ea45ca78988784b810ad9bbb148ea7ff76d8`.
