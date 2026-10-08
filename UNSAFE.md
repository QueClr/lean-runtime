# `unsafe` in lean-runtime

The crate root denies `unsafe` code (`#![deny(unsafe_code)]` in
`src/lib.rs`). A file may allow it for itself only, and only with an entry
here: `deny` does not stop a file's `allow`, so `scripts/check.sh` fails on
a file that names `unsafe_code` without one. A build with none of
`proc-title`, `startup-fds`, `stack-overflow` and `unsafe-fast` (the
default build, and `io`, `sched`, `threads` and `net` alone or together)
compiles no `unsafe` code of the crate: the root forbids it outright there.

No ELF constructor of the crate uses the global allocator (AR-36): they run
before the program configures its allocator. `tests/ctor_alloc.rs` checks
it.

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
| Where and what | `src/io/argv_title.rs`, compiled only with the feature `proc-title` (which turns on `io`; lean2rr enables it). Without it, the crate has no constructor and holds no arguments' memory: `setProcessTitle` fails with `ENOBUFS` (libuv's answer when `uv_setup_args` kept nothing), and `getProcessTitle` gives `argv[0]` (`docs/native-quirks.md`, "Without the feature"). An ELF constructor of the crate (glibc only) gets the process's `argc` and `argv` and does what libuv 1.48's `uv_setup_args` does: after checking that it is in the program's own executable (its address inside the kernel's `start_code` to `end_code`, `/proc/self/stat`; in a shared library it keeps nothing, and the title functions fail with `ENOBUFS` as natively; so does a launch through the dynamic loader, `ld.so ./prog`, where native writes: judged deviation LQ1-01), that no other thread runs and that the memory from `argv[0]` to the end of the last argument lies inside the kernel's span of the arguments (`/proc/self/stat`), it keeps that memory, copies the arguments into a block of its own and points the table at the copies. `uvsys::set_process_title` then writes the title into the memory. The constructor allocates nothing (AR-36): it reads `/proc` into stack buffers (rustix's `RawDir` for `/proc/self/task`), and maps the block of copies itself, which also holds a further copy of `argv[0]`, the first title. The `unsafe` operations: reads of the table and the strings (U1, U2), the write of the title, one `copy_nonoverlapping` (U3), the writes of the table's entries (U4), the `mmap` of the block (U5, rustix's `mm` feature) and the slice over it (U6), and the constructor's `#[link_section = ".init_array.00100"]` static: priority 100, so that it runs after the toolchain's constructors (std's `argv` record is 99) and before the program's own, a priority above 100 or none, among them the translators' startup constructors, as `lean_setup_args` runs before libuv's descriptors open (AR-20; earlier code, shared libraries' constructors and `.preinit_array`, still runs first: `docs/native-quirks.md`, G4). No glue writes `unsafe` for it |
| Native behaviour | `IO.setProcessTitle` (libuv's `uv_set_process_title`) writes the title over the original arguments: the title cut to their memory less one byte, then NUL bytes to the end of that memory; the environment is not moved. So `/proc/self/cmdline` shows the title, while Lean's `args` come from libuv's copy. Cases `uvsys/title_cmdline` (LIO2-06, resolved), `uvsys/title_in_initializer`, `uvsys/title_via_loader` (deviation LQ1-01: started through the dynamic loader, the title fails with `ENOBUFS`; native writes it), `uvsys/title_fd_limit` (AR-20: under `ulimit -n 12` the startup descriptors leave one free, and native writes the title) and `uvsys/process_title` |
| Why no safe route exists | std: `std::env::args_os` returns copies, and std keeps `argc` and `argv` in private statics. rustix 1.1.4: `process::set_name` only names the thread; `set_virtual_memory_map_address` (`PR_SET_MM_ARG_START`/`ARG_END`, which need `CAP_SYS_RESOURCE`) and `configure_virtual_memory_map` (`PR_SET_MM_MAP`, which needs no capability but resets the whole memory map, its `brk` racing with `sbrk`) are `unsafe`. nix 0.31.3: `sys::prctl` has `set_name` and no `PR_SET_MM`. Writing `/proc/self/mem` takes only safe calls but is the same write hidden from the compiler, outside Rust's safety guarantees (rejected in io-2's review, LIO2-06) |
| Invariant and proof | `docs/native-quirks.md`, "The process title in the arguments' memory": what the constructor relies on (glibc's `.init_array` convention, the kernel's layout, earlier code, the order of the constructors: G1 to G4), the checks, the invariants I1 to I7, and the proof of U1 to U4 and of the constructor |
| How it is checked | The cases `uvsys/title_cmdline`, `uvsys/title_in_initializer`, `uvsys/title_via_loader` and `uvsys/process_title` (and the other cases that set a title: `uv_limits`, `os_strings_lossy`, `rt_system`) through their twins in `tests/io2_cases.rs`, which link the crate's constructor as any binary does, and `uvsys/title_fd_limit` through its twin in `tests/io_cases.rs`, whose own startup constructor (plain `.init_array`, as a translator's) opens the eight startup descriptors (it fails with the crate's constructor in plain `.init_array`), or the crate's startup constructor (101, with `startup-fds`), in `scripts/check.sh`'s configurations with `proc-title` (`io,proc-title`, the two with every feature, and lean2rr's set `io,sched,net,proc-title,startup-fds,stack-overflow`). AR-36: `tests/ctor_alloc.rs` (a counting global allocator, and a constructor of its own after the crate's: no allocation, the title written, the arguments kept; the earlier constructor fails it). The constructor linked and working in downstream binaries built by cargo (debug, release), by plain `rustc` from rlibs (as lean2rr builds), and as a static library linked into a C `main`; a `cdylib` loaded by `dlopen` keeps nothing (`ENOBUFS`). Rechecked for AR-20 (2026-10-04): in each of the four, with a startup constructor that opens eight descriptors, `.init_array` lists the crate's constructor after std's `argv` record and before the startup constructor (GNU ld 2.42; also gold 1.16 and LLD 23.1 for the plain-`rustc` build), and under `ulimit -n 12` the title is written. The file's unit tests on blocks laid out as the kernel lays out the arguments (and a mapped block of copies, unmapped by the test), which Miri runs (Stacked Borrows; Tree Borrows with strict provenance; passed 2026-10-04, and again 2026-10-05 after AR-36; `LEAN_RUNTIME_MIRI=1 LEAN_RUNTIME_MIRI_FILTER=argv_title scripts/check.sh`). Miri cannot run the constructor on real arguments. Adversarial review, ours and leanrs's. Kani does not apply |

### `src/io/startup_fds.rs`: the startup descriptors in a constructor

| Part | |
|---|---|
| Where and what | `src/io/startup_fds.rs`, compiled only with the feature `startup-fds` (which turns on `io`; lean2rr enables it, leanrs leaves it off: DV19; audit item 4.3, lean2rr's `rt.rs` constructor moved into the crate). Without it, the crate has no such constructor, and a glue that wants native's descriptors writes its own (`io::startup::open_native_descriptors`). An ELF constructor of the crate (glibc only, not under Miri), in `.init_array.00101`: after the toolchain's constructors and `proc-title`'s (100), before the program's own (AR-20). It acts only in the program's own executable (its address inside `start_code` to `end_code`, `/proc/self/stat` read into a stack buffer by the safe `src/io/proc_stat.rs`, the title constructor's check): in a shared library it does nothing, so a library never opens descriptors in its host nor ends it (review RSH2-02); nor under `ld.so ./prog` or without `/proc`, two documented deviations (RSH2-11: the descriptors then open in `main`, where they land; an fd-free `dl_iterate_phdr` check, which needs a new `unsafe` callback, is recorded as the fix to adopt if those launches ever matter). There it reads `UV_USE_IO_URING` in place with `getenv`, opens libuv's startup descriptors through `io::startup` (once, kept in a `static`), and on failure reads `LEAN_ABORT_ON_PANIC` the same way and ends the process with the internal panic's line, written from a stack buffer. It allocates nothing (AR-36), installs no signal handler (review SO-2: Lean's stack-overflow report is installed from `main`), starts no thread of the process, and assumes nothing of a glue. `io::startup::ensure_native_descriptors()`, which the glue calls at `main`'s start, keeps the constructor linked and returns when the descriptors are open; if the constructor did not act, it opens them where they land. It closes nothing (lean2rr's recovery, which closed Rust's `/dev/null` standard descriptors first, was removed: reviews RSH2-04, LS2-01). The `unsafe` operations: `getenv` and the read of its result (U1), and the constructor's `#[used] #[link_section = ".init_array.00101"]` static. No glue writes `unsafe` for it |
| Native behaviour | Lean 4.34.0's runtime opens libuv's loop descriptors before any module code, at the lowest free numbers: a standard descriptor closed at startup is taken by the epoll descriptor (reads then fail with `EINVAL`), and the `EMFILE` point follows. Cases `io/startup_closed_stdio`, `io/startup_fd_limit`, `io/startup_rings`, `io/startup_fd_exhausted` (LB-30, LB-31: native crashes; the crate's internal panic) and `uvsys/title_fd_limit` (AR-20) |
| Why no safe route exists | A constructor needs `#[link_section]`, which the `unsafe_code` lint counts (the root's `deny`). Rust's runtime replaces closed standard descriptors with `/dev/null` before any Rust `main`, so opening later (leanrs's DV19) cannot tell them from a `/dev/null` the program was given. std's `env::var_os` copies a set variable (an allocation, AR-36); `/proc/self/environ` shows only the initial environment and needs `/proc` |
| Invariant and proof | `docs/native-quirks.md`, "The startup descriptors in a constructor": what it relies on (G1 glibc's convention and the program's own executable, G2 the order, G3 no change to the environment while it runs), the invariants I1 to I4 (I3: nothing is closed), and the proof of U1 and the constructor |
| How it is checked | The native cases above through their twins in `tests/io_cases.rs`, which has no constructor of its own with the feature, and the other io twins (`tests/io2_cases.rs`), in `scripts/check.sh`'s configurations with the feature (every feature with `sched`, with `threads`, and lean2rr's set `io,sched,net,proc-title,startup-fds,stack-overflow`); `io,proc-title` keeps the glue's own constructor. `tests/ctor_alloc.rs`: no allocation before the program's constructors, the epoll descriptor at 3 then, six descriptors with `UV_USE_IO_URING=0` set, the internal panic (status 1, or an abort with `LEAN_ABORT_ON_PANIC` set) under `ulimit -n 4` with no allocation first; mutation-checked (an allocating `kernel_version` fails it). The unit tests of `startup_fds.rs` (the constructor ran; the fallback in a child process whose constructor opens nothing, started with stdin closed: fd 0 stays `/dev/null`, the epoll descriptor lands at 3) and of `proc_stat.rs`. A `cdylib` with the feature loaded by a Python host, also short of descriptors: no descriptor opened, the host alive (review RSH2-02's repro). Miri does not apply (foreign calls on the process's real state). Adversarial review, ours and leanrs's. Kani does not apply |

### `src/sched/stack_overflow.rs`: Lean's stack-overflow report

| Part | |
|---|---|
| Where and what | `src/sched/stack_overflow.rs`, feature `stack-overflow` (with `sched`, or with `threads` in threads mode; alone a compile error; AR-11). lean2rr enables it; leanrs decides at its adoption of `sched` (its DV6 until then). Without it, a task that overflows its context's stack ends with a plain SIGSEGV (status 139), and the hub updates no record. `sched::install_stack_overflow_handler()`, the glue's one call, installs a process-wide SIGSEGV and SIGBUS handler (`SA_SIGINFO \| SA_ONSTACK`) over the one there (Rust's, kept as the previous action), and registers the calling thread: an alternate signal stack if it has none, and a record in an append-only table that grows with the live registered threads (review RS3-01; its `errno` address as the key, the guard below its own stack, the guard of the context running on it, which the hub publishes at every switch). `sched::start` registers its thread once the handler is installed. In threads mode (`src/sched/threads.rs` includes the same file) there is no context: every thread the task manager makes calls `install_stack_overflow_handler()` at its entry, after std's start, and its record's context guard stays 0 (`running_stack()` is `None`; nothing calls `publish`). An alternate stack the crate makes (for a thread that std gave none) is never freed. In threads mode only, when its thread ends, the `Registration`'s destructor disables it if it is still the thread's (U14) and puts it on a process-wide free list, which the next registration takes from before it makes a new block, so the blocks number at most the largest count of such threads alive at once (review RT1-03; threads mode makes a thread per dedicated task). With `sched`, a registered thread ends only at the exit, so that reuse buys nothing, and a `sched` build compiles exactly the `unsafe` of fb8f548: U1 to U13, no free list, no U14 (review AR-30). The `unsafe` operations: `__errno_location` and `errno`'s save and restore (U1, U2), the `siginfo_t` reads (U3), `write(2)` of the message (U4), `sigaction` back to the default (U5), the previous handler's address as a function pointer (U6), called as the kernel would call it (its mask and `SA_NODEFER` through `pthread_sigmask`, U13; the default restored first under `SA_RESETHAND`, review SO-1), the install (U7, U8; the glue installs from `main`, after Rust's runtime has started, never from an ELF constructor, review SO-2), `pthread_getattr_np` and its attribute calls (U9), `sysconf` and `getauxval` (U10), `sigaltstack` (U11, U12; U14, the disable at a thread's end, threads mode only). No glue writes `unsafe` for it |
| Native behaviour | `src/runtime/stack_overflow.cpp`: every Lean thread has an alternate signal stack; a fault in the page below the faulting thread's stack writes `\nStack overflow detected. Aborting.\n` to descriptor 2 and aborts (status 134, buffered output lost); any other fault resets the default action and returns (status 139). Here a task's context has a stack of its own, whose guard page counts as the thread's. Case `tasks/stack_overflow_in_task` |
| Why no safe route exists | std: its handler knows only the guards of threads, and prints Rust's message. signal-hook 0.3.18: refuses SIGSEGV (`FORBIDDEN`). corosensei 0.3.4: `CoroutineTrapHandler::setup_trap_handler` is `unsafe`, and resumes a coroutine after a trap rather than report. nix 0.31.3 and rustix 1.1.4: `sigaction` and `sigaltstack` are `unsafe` (rustix's in its `runtime` module, which bypasses libc's signal state). Installing a signal handler is `unsafe` in every crate, since the handler runs at any point of the program |
| Invariant and proof | `docs/native-quirks.md`, "Lean's stack-overflow report": what it relies on (G1 to G5), the invariants I1 to I7, the handler's needs A1 to A4 (delivery on the alternate stack, async-signal-safety without thread-locals, the window between a publication and the switch, the forward to the previous action), and the proof of U1 to U14 |
| How it is checked | The case `tasks/stack_overflow_in_task` through its twin in `tests/sched-driver`, which builds the crate with `stack-overflow`; `scripts/check.sh` also builds and tests `io,sched` without it, where the root forbids `unsafe`; the driver's tests `so_main_overflow`, `so_task_overflow_after_switches`, `so_segv_in_task`, `so_rust_thread_overflow`, `so_second_scheduler_thread` and `so_prev_resethand` (a one-shot previous handler, review SO-1), each also mutation-checked; the unit test `more_threads_than_a_chunk_all_have_records` (RS3-01); the file's unit tests (the guards, a record covering both guards in the window, a registered thread's record and alternate stack); in threads mode (`scripts/check.sh`'s `io,threads,proc-title,startup-fds,stack-overflow,unsafe-fast`), the same unit tests and `sched::mt`'s `every_thread_the_manager_makes_registers_with_the_report`; the reuse of the crate's alternate stacks (RT1-03, threads mode only): the unit test `ended_threads_give_their_alternate_stacks_back` (built only with `threads`), and the example `threads_altstack_reuse` (a C-style entry, 200 dedicated tasks; `check.sh` runs it), which fails before the fix (16.4 MB left behind). Miri cannot model it (foreign calls and signal delivery only). Adversarial review, ours and leanrs's. Kani does not apply |

## `unsafe-fast`

There are no entries yet.

## `unsafe` in dependencies

The crate's `unsafe` beyond this file's entries is in vetted dependencies:
rustix and nix (system calls, feature `io`; nix's `sigaction` and its
re-exported libc also for the feature `stack-overflow`; rustix's `mm` for
`proc-title`'s block; rustix also for
`sched` and `threads`, the event loop's `poll`, `epoll` and eventfd, and
its `thread` feature's `sched_getaffinity`, the count of processors when
`/sys` and `/proc` cannot be read, hunt HDW-01),
corosensei (stack switching, `sched`) and signal-hook (signal handlers, its
safe API only, `sched`, and `threads` for `sched::uv`'s signal watchers,
T2),
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
errors and the EAI mapping; `src/net/tests_mt.rs`, lookups in threads
mode) and the program cases `dns_localhost` and `dns_pending_at_exit`
against native Lean, in the debug and release builds of
`tests/sched-driver` and, in threads mode, `tests/sched-driver-mt`. Miri
does not run foreign calls.

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
  ".init_array"]`), as a translator's glue does without `startup-fds`, to
  open native Lean's startup descriptors (`io::startup`) before Rust's
  runtime starts (plain `.init_array`, so with `proc-title` the crate's own
  constructor runs first: AR-20); with `startup-fds` it has none, and the
  crate's opens them;
- `tests/ctor_alloc.rs` (AR-36) has a counting global allocator (an
  `unsafe impl GlobalAlloc` that forwards every call to `System`) and an ELF
  constructor of its own in plain `.init_array`, which records the count
  after the crate's constructors;
- `tests/keyed_alloc.rs` (AR-40) has a counting global allocator too (it
  forwards every call to `System`, and counts per thread);
- `tests/sched-driver/src/glue.rs` is the glue a translator writes around
  `sched`:
  - its `Glue::suspend` dereferences the yielder pointer the scheduler hands
    it, as every translator's glue does (`docs/sched.md`, "Why
    `Glue::suspend` is sound");
  - its stack-overflow report is the crate's
    (`sched::install_stack_overflow_handler`), with no `unsafe` in the
    glue;
- `tests/sched-driver/src/glue_common.rs`, the glue's part that both
  scheduler drivers compile (`tests/sched-driver-mt` by a path include),
  registers the same ELF constructor for native Lean's startup descriptors;
  the threads-mode driver's own glue (`tests/sched-driver-mt/src/glue.rs`)
  has no `unsafe`;
- `tests/sched-driver/src/review.rs`'s `so_segv_in_task` and
  `so_prev_resethand` write to an unmapped address, to make a fault that is
  no stack overflow, and `install_prev_resethand` installs a one-shot
  SIGSEGV handler of its own (libc's `sigaction`; its handler only calls
  `write`) before the crate's, for review SO-1 of AR-11.

The unit tests of `src/io/argv_title.rs` make regions over blocks of their
own, and repoint a table of their own, under the same contracts as the
constructor's calls; one unmaps the block of copies after its last use.
