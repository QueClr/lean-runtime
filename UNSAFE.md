# `unsafe` in lean-runtime

The default build contains no `unsafe` code: `src/lib.rs` has
`#![forbid(unsafe_code)]` unless the feature `unsafe-fast` is enabled.

Each `unsafe` block behind `unsafe-fast` gets an entry here with four parts:
- **Where and what:** file, function, the safe twin it replaces.
- **What it gets us:** the measured speed or memory gain.
- **Why it is sound:** a written proof.
- **How it is checked:** Miri (and Kani where it applies), plus the test
  suite in both configurations.

There are no entries yet.
