# `unsafe` in lean-runtime

The crate root denies `unsafe` code (`#![deny(unsafe_code)]` in
`src/lib.rs`). A file may allow it for itself only, and only with an entry
here: `deny` does not stop a file's `allow`, so `scripts/check.sh` fails on
a file that names `unsafe_code` without one. A build with neither
`proc-title` nor `unsafe-fast` (the default build, and `io`, `sched` and
`net` alone or together) compiles no `unsafe` code of the crate: the root
forbids it outright there.

There are two kinds of entry:
- **Native quirks.** Behaviour of native Lean that no safe API can
  reproduce (owner, 2026-10-04: "no way around"). Each item is a small file
  of its own, behind a feature of its own (so a translator that does not
  need it compiles no `unsafe`; owner: avoid `unsafe` where it is not
  needed), with
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
| Where and what | `src/io/argv_title.rs`, compiled only with the feature `proc-title` (which turns on `io`; lean2rr enables it). Without it, the crate has no constructor and holds no arguments' memory: `setProcessTitle` fails with `ENOBUFS` (libuv's answer when `uv_setup_args` kept nothing), and `getProcessTitle` gives `argv[0]` (`docs/native-quirks.md`, "Without the feature"). An ELF constructor of the crate (glibc only) gets the process's `argc` and `argv` and does what libuv 1.48's `uv_setup_args` does: after checking that it is in the program's own executable (its address inside the kernel's `start_code` to `end_code`, `/proc/self/stat`; in a shared library it keeps nothing, and the title functions fail with `ENOBUFS` as natively; so does a launch through the dynamic loader, `ld.so ./prog`, where native writes: judged deviation LQ1-01), that no other thread runs and that the memory from `argv[0]` to the end of the last argument lies inside the kernel's span of the arguments (`/proc/self/stat`), it keeps that memory, copies the arguments into a block of its own and points the table at the copies. `uvsys::set_process_title` then writes the title into the memory. The `unsafe` operations: reads of the table and the strings (U1, U2), the write of the title, one `copy_nonoverlapping` (U3), the writes of the table's entries (U4), and the constructor's `#[link_section = ".init_array"]` static. No glue writes `unsafe` for it |
| Native behaviour | `IO.setProcessTitle` (libuv's `uv_set_process_title`) writes the title over the original arguments: the title cut to their memory less one byte, then NUL bytes to the end of that memory; the environment is not moved. So `/proc/self/cmdline` shows the title, while Lean's `args` come from libuv's copy. Cases `uvsys/title_cmdline` (LIO2-06, resolved), `uvsys/title_in_initializer`, `uvsys/title_via_loader` (deviation LQ1-01: started through the dynamic loader, the title fails with `ENOBUFS`; native writes it) and `uvsys/process_title` |
| Why no safe route exists | std: `std::env::args_os` returns copies, and std keeps `argc` and `argv` in private statics. rustix 1.1.4: `process::set_name` only names the thread; `set_virtual_memory_map_address` (`PR_SET_MM_ARG_START`/`ARG_END`, which need `CAP_SYS_RESOURCE`) and `configure_virtual_memory_map` (`PR_SET_MM_MAP`, which needs no capability but resets the whole memory map, its `brk` racing with `sbrk`) are `unsafe`. nix 0.31.3: `sys::prctl` has `set_name` and no `PR_SET_MM`. Writing `/proc/self/mem` takes only safe calls but is the same write hidden from the compiler, outside Rust's safety guarantees (rejected in io-2's review, LIO2-06) |
| Invariant and proof | `docs/native-quirks.md`, "The process title in the arguments' memory": what the constructor relies on (glibc's `.init_array` convention, the kernel's layout, earlier code), the checks, the invariants I1 to I7, and the proof of U1 to U4 and of the constructor |
| How it is checked | The cases `uvsys/title_cmdline`, `uvsys/title_in_initializer`, `uvsys/title_via_loader` and `uvsys/process_title` (and the other cases that set a title: `uv_limits`, `os_strings_lossy`, `rt_system`) through their twins in `tests/io2_cases.rs`, which link the crate's constructor as any binary does, in `scripts/check.sh`'s configurations with `proc-title` (`io,proc-title` and `io,sched,proc-title,unsafe-fast`). The constructor linked and working in downstream binaries built by cargo (debug, release), by plain `rustc` from rlibs (as lean2rr builds), and as a static library linked into a C `main`; a `cdylib` loaded by `dlopen` keeps nothing (`ENOBUFS`). The file's unit tests on blocks laid out as the kernel lays out the arguments, which Miri runs (Stacked Borrows; Tree Borrows with strict provenance; passed 2026-10-04; `LEAN_RUNTIME_MIRI=1 LEAN_RUNTIME_MIRI_FILTER=argv_title scripts/check.sh`). Miri cannot run the constructor on real arguments. Adversarial review, ours and leanrs's. Kani does not apply |

## `unsafe-fast`

There are no entries yet.

## `unsafe` in dependencies

The crate's `unsafe` beyond this file's entries is in vetted dependencies:
rustix and nix (system calls, feature `io`), corosensei (stack switching,
`sched`) and signal-hook (signal handlers, its safe API only, `sched`),
approved by the owner; io-uring, accepted by leanrs's shared-runtime
coordinator under the owner's delegation of dependency decisions
(2026-10-04); and, for the feature `net`, dns-lookup (approved the same
day) and the `net` features of nix and rustix.

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

### dns-lookup 2.1.1 (`net`; approved 2026-10-04, pinned `=2.1.1`)

`net::dns` calls `dns_lookup::getaddrinfo(Some(host), Some(service),
Some(hints))` and `dns_lookup::getnameinfo(&addr, 0)`, on the crate's two
lookup threads, and nothing else of the crate. Its dependencies are libc
(the declarations), socket2 0.6 (`SockAddr`) and cfg-if; windows-sys is for
Windows only. Audited at 2.1.1 (`src/addrinfo.rs`, `src/nameinfo.rs`,
`src/err.rs`):

- **`getaddrinfo` and `freeaddrinfo` pair up.** The hints are an all-zero
  `addrinfo` (null pointers) with the four integer fields set; host and
  service are `CString`s alive for the call (an interior NUL is an error
  before the call; Lean's own check lets none through). On failure the
  function returns the error at once and builds no iterator: glibc leaves
  `*res` unset and frees its partial list itself, so nothing is freed or
  leaked. On success the list is owned by `AddrInfoIter`, whose `Drop`
  calls `freeaddrinfo` once on the list's head, whether the iteration
  finished, stopped early or met an entry it could not convert; each entry
  is copied out (`AddrInfo::from_ptr`) before the next pointer is followed,
  so no entry outlives the list. `net::dns` consumes the iterator on the
  thread that made it (the crate's `unsafe impl Send/Sync` is not relied on).
- **Copying an entry's address.** `from_ptr` copies `ai_addrlen` bytes into
  a `sockaddr_storage` (socket2's `SockAddr::try_init`): in bounds because
  glibc's entries are `sockaddr_in` or `sockaddr_in6` (16 or 28 bytes, at
  most 128). An entry of another family is an `Err` item, which `net::dns`
  skips as Lean does. `ai_canonname` is read only when it is not null, and
  it is null without `AI_CANONNAME` (the hints' flags are 0): its
  `.unwrap()` on UTF-8 is never reached.
- **`getnameinfo`'s buffers.** The host buffer has 1024 bytes and the
  service buffer 32, each passed with its own length, so glibc never writes
  past them; on success glibc writes NUL-terminated strings that fit, so
  `CStr::from_ptr` stays inside the buffers; on failure they are not read.
  `NI_MAXSERV` is 32; glibc's `NI_MAXHOST` is 1025 (libuv's buffer), so a
  host name of exactly 1024 bytes fails here with `EAI_OVERFLOW` where libuv
  gets it: LNET-01, a judged behaviour difference, not a soundness one
  (`docs/net.md`), kept because a buffer of our own needs `unsafe`. A name
  that is not UTF-8 is an `io::Error` with error number 0, not a panic;
  `net::dns` reports it as `EAI_FAIL` where native decodes it lossily:
  LNET-02, kept because reading the bytes from the C buffer needs `unsafe`.
- **`EAI_SYSTEM` and `errno`.** `LookupError::new` runs right after the C
  call, on the same thread, and reads `errno` there
  (`io::Error::last_os_error`), before any other C call; `net::dns` takes
  the number back with `raw_os_error` and makes libuv's code of it. For the
  other codes it calls `gai_strerror`, whose result is a static string,
  read up to its NUL; its `.unwrap()` on UTF-8 holds for glibc's messages
  in the C locale, which neither translator's programs leave.
- **The rest of the crate** (`gethostname`, `lookup_host`, Windows) is not
  called.

How it is checked: the unit tests of `net` (`src/net/tests.rs`, the sync
errors and the EAI mapping) and the program cases `dns_localhost` and
`dns_pending_at_exit` against native Lean, in the debug and release builds
of `tests/sched-driver`. Miri does not run foreign calls.

The audit holds for socket2 0.6.5 (`SockAddr::try_init`, `as_socket`), the
version `Cargo.lock` pins under dns-lookup 2.1.1 (whose requirement admits
`>= 0.6.0, < 0.7`). A consumer that resolves its own lock file (leanrs)
must pin socket2 to 0.6.5 too, or audit the version it takes.

### nix's `net` feature (`net`)

`net::iface` and `net::tcp` (the link-local scope of an IPv6 connect) call
`nix::ifaddrs::getifaddrs`, whose iterator walks glibc's list, converts each
address with `SockaddrStorage::from_raw` (reading `sa_family`, then
copying that family's structure), and frees the list with `freeifaddrs` in
its `Drop`. The feature pulls in memoffset 0.9.1 (offsets of `sockaddr`
fields; build dependency autocfg, already in `Cargo.lock`).

### rustix's `net` feature (`net`)

Sockets, socket options, `sendmmsg`, `recvfrom`, `accept4` and
`ioctl(FIONREAD)` through rustix's raw system calls, as the rest of the
crate already uses rustix. No new crate.

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
