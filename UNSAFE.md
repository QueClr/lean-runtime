# `unsafe` in lean-runtime

The crate root denies `unsafe` code (`#![deny(unsafe_code)]` in
`src/lib.rs`). A file may allow it for itself only, and only with an entry
here: `deny` does not stop a file's `allow`, so `scripts/check.sh` fails on
a file that names `unsafe_code` without one. The default build (no
features) contains no `unsafe` code, and a build with neither `io` nor
`unsafe-fast` forbids it outright.

There are two kinds of entry:
- **Native quirks.** Behaviour of native Lean that no safe API can
  reproduce (owner, 2026-10-04: "no way around"). Each item is a small file
  of its own, in the build of the feature that needs it, with
  `#![allow(unsafe_code)]`, `#![deny(unsafe_op_in_unsafe_fn)]` and a
  `// SAFETY:` comment on every `unsafe` block. Its entry gives:
  - **Where and what:** the file and its `unsafe` operations.
  - **Native behaviour:** what it reproduces, and the case that records it.
  - **Why no safe route exists:** the crates and APIs checked.
  - **Invariant and proof:** a section of `docs/native-quirks.md`.
  - **How it is checked:** the native cases, adversarial review, and Miri
    or Kani where they apply.

  leanrs reviews each item before merge. No such item is in a hot path.
- **`unsafe-fast`.** A faster implementation behind the opt-in feature
  `unsafe-fast`, with the same observable behaviour as its safe twin, which
  stays. Its entry gives:
  - **Where and what:** file, function, the safe twin it replaces.
  - **What it gets us:** the measured speed or memory gain.
  - **Why it is sound:** a written proof.
  - **How it is checked:** Miri (and Kani where it applies), plus the test
    suite in both configurations.

## Native quirks

### `src/io/argv_title.rs`: the process title in the arguments' memory

| Part | |
|---|---|
| Where and what | `src/io/argv_title.rs`, feature `io`. An ELF constructor of the crate (glibc only) gets the process's `argc` and `argv` and does what libuv 1.48's `uv_setup_args` does: after checking that it is in the program's own executable (its address inside the kernel's `start_code` to `end_code`, `/proc/self/stat`; in a shared library it keeps nothing, and the title functions fail with `ENOBUFS` as natively; so does a launch through the dynamic loader, `ld.so ./prog`, where native writes: judged deviation LQ1-01), that no other thread runs and that the memory from `argv[0]` to the end of the last argument lies inside the kernel's span of the arguments (`/proc/self/stat`), it keeps that memory, copies the arguments into a block of its own and points the table at the copies. `uvsys::set_process_title` then writes the title into the memory. The `unsafe` operations: reads of the table and the strings (U1, U2), the write of the title, one `copy_nonoverlapping` (U3), the writes of the table's entries (U4), and the constructor's `#[link_section = ".init_array"]` static. No glue writes `unsafe` for it |
| Native behaviour | `IO.setProcessTitle` (libuv's `uv_set_process_title`) writes the title over the original arguments: the title cut to their memory less one byte, then NUL bytes to the end of that memory; the environment is not moved. So `/proc/self/cmdline` shows the title, while Lean's `args` come from libuv's copy. Cases `uvsys/title_cmdline` (LIO2-06, resolved), `uvsys/title_in_initializer`, `uvsys/title_via_loader` (deviation LQ1-01: started through the dynamic loader, the title fails with `ENOBUFS`; native writes it) and `uvsys/process_title` |
| Why no safe route exists | std: `std::env::args_os` returns copies, and std keeps `argc` and `argv` in private statics. rustix 1.1.4: `process::set_name` only names the thread; `set_virtual_memory_map_address` (`PR_SET_MM_ARG_START`/`ARG_END`, which need `CAP_SYS_RESOURCE`) and `configure_virtual_memory_map` (`PR_SET_MM_MAP`, which needs no capability but resets the whole memory map, its `brk` racing with `sbrk`) are `unsafe`. nix 0.31.3: `sys::prctl` has `set_name` and no `PR_SET_MM`. Writing `/proc/self/mem` takes only safe calls but is the same write hidden from the compiler, outside Rust's safety guarantees (rejected in io-2's review, LIO2-06) |
| Invariant and proof | `docs/native-quirks.md`, "The process title in the arguments' memory": what the constructor relies on (glibc's `.init_array` convention, the kernel's layout, earlier code), the checks, the invariants I1 to I7, and the proof of U1 to U4 and of the constructor |
| How it is checked | The cases `uvsys/title_cmdline`, `uvsys/title_in_initializer`, `uvsys/title_via_loader` and `uvsys/process_title` through their twins in `tests/io2_cases.rs`, which link the crate's constructor as any binary does. The constructor linked and working in downstream binaries built by cargo (debug, release), by plain `rustc` from rlibs (as lean2rr builds), and as a static library linked into a C `main`; a `cdylib` loaded by `dlopen` keeps nothing (`ENOBUFS`). The file's unit tests on blocks laid out as the kernel lays out the arguments, which Miri runs (Stacked Borrows; Tree Borrows with strict provenance; passed 2026-10-04; `LEAN_RUNTIME_MIRI=1 LEAN_RUNTIME_MIRI_FILTER=argv_title scripts/check.sh`). Miri cannot run the constructor on real arguments. Adversarial review, ours and leanrs's. Kani does not apply |

## `unsafe-fast`

There are no entries yet.

## `unsafe` in dependencies

The crate's `unsafe` beyond this file's entries is in vetted dependencies:
rustix and nix (system calls, feature `io`), corosensei (stack switching,
`sched`) and signal-hook (signal handlers, its safe API only, `sched`),
approved by the owner; and io-uring, accepted by leanrs's shared-runtime
coordinator under the owner's delegation of dependency decisions
(2026-10-04).

### io-uring 0.7.13 (tokio-rs), feature `io`

Pinned exactly (`=0.7.13`), with only `io_safety` (std's `OwnedFd` and
`AsFd`; no other crate), accepted by leanrs's shared-runtime coordinator
under the owner's delegation of dependency decisions (2026-10-04). Its
dependencies are bitflags, cfg-if and libc, all already in `Cargo.lock`
(its optional `sc`, for direct system calls, is not enabled).
`io::startup` uses it for the two rings libuv 1.48 makes at startup, and
calls only safe functions. Audit of what they do (io-uring's `src/lib.rs`,
`src/util.rs`, `src/sys/mod.rs`):
- `IoUring::builder()` starts from zeroed `io_uring_params` with the flags of
  the default entry types, which are none; `setup_sqpoll(10)` sets
  `IORING_SETUP_SQPOLL` and `sq_thread_idle = 10`. Nothing else is set, so
  the parameters are libuv's `uv__iou_init`'s: never `ATTACH_WQ`,
  `NO_MMAP`, `REGISTERED_FD_ONLY`, `SQ_AFF` or `CQSIZE`.
- `build(entries)` calls `io_uring_setup` through `libc::syscall` with a
  pointer to that struct (a `repr(C)` mirror of the kernel's), wraps the
  descriptor in an `OwnedFd`, and maps the rings as libuv does (`mmap`,
  `PROT_READ | PROT_WRITE`, `MAP_SHARED | MAP_POPULATE`: one map for the
  submission and completion rings with `IORING_FEAT_SINGLE_MMAP`, one for
  the submission entries). The mappings live as long as the `IoUring`;
  its `Drop` unmaps them, then closes the descriptor. The crate keeps the
  rings in a `static`, so neither happens.
- `params().is_feature_resource_tagging()`, `is_feature_single_mmap()`,
  `is_feature_nodrop()` read the features the kernel returned, for libuv's
  check; `as_fd()` lends the descriptor to rustix's safe `epoll::add`.

Nothing submits to the rings, registers with them, or reads their memory.
Checked natively: case `io/startup_rings` (the rings' `/proc/self/fdinfo`
lines, their inodes, the four `anon_inode:[io_uring]` mappings, the polling
thread) and the descriptor numbers in `io/startup_fd_limit` and
`io/startup_closed_stdio`.

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

The unit tests of `src/io/argv_title.rs` make regions over blocks of their
own, and repoint a table of their own, under the same contracts as the
constructor's calls.
