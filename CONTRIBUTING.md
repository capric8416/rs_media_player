# Contributing

## Unit workflow

1. Read `MAP.md`, `DECISIONS.md`, `GLOSSARY.md`, and the research files for the unit.
2. Claim exactly one open unit in `MAP.md` before editing its implementation.
3. Add characterization or differential cases before translating behavior.
4. Keep unsafe Win32/COM operations inside the backend boundary and add a `// SAFETY:` explanation
   to every unsafe block.
5. Run formatting, Clippy with warnings denied, all tests, the unit's differential comparison, and
   its benchmark where applicable.
6. Mark the unit done only after its gate passes and record the evidence in `MAP.md`.

Do not use `unwrap`, `expect`, or `panic!` in production paths that can report an error. Do not add
an unsupported fallback silently; return a typed capability error.

## Test categories

- Unit tests cover checked parsing, state transitions, and pure parameter construction.
- Differential tests compare Rust metadata or DXVA parameter buffers with the recorded reference.
- GPU integration tests declare their required backend/profile and skip only with an explicit
  capability reason.
- End-to-end tests demux, decode, scale, and render the fixtures listed in
  `fixtures/manifest.toml`.
- Benchmarks compare the same media, adapter, output format, and synchronization boundary.

