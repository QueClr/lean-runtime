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

## `unsafe` in the tests

Some test files, which are not part of the library, use `unsafe` for test
plumbing only:
- `tests/cfile_glibc.rs` calls glibc's `FILE` functions through `extern "C"`
  declarations: glibc is the oracle the `FILE` model is compared against;
- `tests/cfile_glibc.rs` and `tests/io_rows.rs` read back `MaybeUninit`
  bytes after initializing every one of them first (a sentinel), to check
  the reads into uninitialized memory;
- `tests/io_cases.rs` registers an ELF constructor (`#[link_section =
  ".init_array"]`), as a translator's glue does, to open native Lean's
  startup descriptors (`io::startup`) before Rust's runtime starts;
- `tests/sched-driver/src/glue.rs` is the glue a translator writes around
  `sched`:
  - its `Glue::suspend` dereferences the yielder pointer the scheduler hands
    it, as every translator's glue does (`docs/sched.md`, "Why
    `Glue::suspend` is sound");
  - it installs Lean's stack-overflow report (`sigaltstack`, `sigaction`,
    `pthread_getattr_np`; `write` and `abort` in the handler), as
    `src/runtime/stack_overflow.cpp` does.
