# Native quirks written with `unsafe`

Some behaviour of native Lean cannot be reproduced through any safe API.
The owner's decision (2026-10-04): "the quirks one since need unsafe (no
other way) then we can just write unsafe for them (no way around)". Each
such item follows the pattern agreed with leanrs:
- it lives in its own small file, with `#![allow(unsafe_code)]` and
  `#![deny(unsafe_op_in_unsafe_fn)]`, compiled only with a feature of its
  own, so that a translator that does not need the quirk compiles no
  `unsafe` (owner: avoid `unsafe` where it is not needed; AR-14). The crate
  root denies `unsafe_code`. A `deny` can be overridden by a file's
  `allow`, so `scripts/check.sh` fails on any file that names `unsafe_code`
  without an entry in `UNSAFE.md`; a build with none of `proc-title`,
  `stack-overflow` and `unsafe-fast` has no such file, and there the root
  forbids `unsafe` outright;
- every `unsafe` block has a `// SAFETY:` comment naming the invariant it
  relies on;
- `UNSAFE.md` has its entry, and this file its invariants and proof;
- leanrs reviews it before merge.

So far there are two items: the process title (feature `proc-title`) and
Lean's stack-overflow report (feature `stack-overflow`).

## The process title in the arguments' memory (`src/io/argv_title.rs`)

This section is self-contained: with it and the source of
`src/io/argv_title.rs` and `src/io/uvsys.rs` (the title functions), a reader
can check the item. It replaces the first version's contract on the glue
(leanrs's review of quirks-1): the crate now runs its own constructor,
checks what it would write, and keeps libuv's copy of the arguments.

The item is compiled only with the feature `proc-title` (which turns on
`io`). lean2rr enables it. A translator that leaves it off compiles no
`unsafe` code of the crate, and its programs get `ENOBUFS` from
`setProcessTitle` ("Without the feature", below).

### What native does

Lean 4.34.0's generated `main` calls `lean_setup_args(argc, argv)`, which
is libuv 1.48's `uv_setup_args` (`src/unix/proctitle.c`), before the module
initializers. `Std.Internal.UV.System.setProcessTitle` is
`uv_set_process_title`, after Lean's check for a NUL byte
(`src/runtime/uv/system.cpp`).

- **`uv_setup_args`.** With `argc > 0`, the title's storage is `argv[0]`'s
  memory, and its capacity `cap` is every byte from `argv[0]` to the NUL
  that ends `argv[argc - 1]`:
  `argv[argc - 1] + strlen(argv[argc - 1]) + 1 - argv[0]`. libuv copies the
  strings and the pointer table into a block of its own, and the generated
  `main` builds Lean's `args` from that copy. With `argc <= 0`, libuv keeps
  nothing and the title functions fail with `UV_ENOBUFS`.
- **`uv_set_process_title(title)`.** A title of `cap` bytes or more is cut
  to `cap - 1` bytes, without an error. libuv copies the title to the start
  of the memory (`memcpy`) and sets the rest of the `cap` bytes to NUL
  (`memset`), which wipes the other arguments. Then `prctl(PR_SET_NAME)`
  names the calling thread (its first 15 bytes).
- **The environment is not moved.** The kernel puts the environment's
  strings right after the arguments. libuv never writes past `cap`, so a
  title can never be longer than the original arguments less one byte.
- **Who sees it.** The kernel's `/proc/<pid>/cmdline` reads that memory
  (`arg_start` to `arg_end`), so `ps` and the program itself see the new
  title followed by NUL bytes. So does glibc's `program_invocation_name`,
  which points to `argv[0]`. Lean's `args` do not change, since the
  generated `main` builds them from libuv's copy, after the initializers;
  the original table (`__libc_argv`, which Rust's `std::env::args` reads)
  still points at the overwritten memory natively.

Example: `prog ab c` starts with the memory `prog\0ab\0c\0`, so `cap` is 10.
- `setProcessTitle "new"` leaves `new\0\0\0\0\0\0\0`.
- `setProcessTitle "0123456789abc"` leaves `012345678\0`, and
  `getProcessTitle` gives `"012345678"`.
- The environment's bytes after the tenth byte never change.

The case `uvsys/title_cmdline` records this natively,
`uvsys/title_in_initializer` a title set by a module initializer, and
`uvsys/process_title` the rest of the title's behaviour.

### What the crate does

With the feature `proc-title`:
- **The constructor.** On glibc, `argv_title.rs` has an ELF constructor
  (`#[link_section = ".init_array.00100"]`, cfg `target_os = "linux"` and
  `target_env = "gnu"`). glibc calls it, as every `.init_array` function,
  with the process's `argc`, `argv` and `envp`: before `main` when it is
  in the program's executable, when the library is loaded when it is in a
  shared library (by `dlopen`, also after `main`). It calls `setup_args`,
  so no translator's glue takes part and none writes `unsafe`.
- **Its place among the constructors** (AR-20, lean2rr's review RST3-01).
  Natively the generated `main` calls `lean_setup_args` before libuv opens
  its startup descriptors. Here the translators open them from constructors
  of their own, in plain `.init_array`. The priority 100 puts the crate's
  constructor before those, and after the toolchain's own (G4). So under
  `ulimit -n 12`, where libuv's eight descriptors leave one free, the checks
  below still find the two descriptors they need, and the title is written,
  as natively (case `uvsys/title_fd_limit`). In plain `.init_array` it ran
  after them (link order), kept nothing, and `/proc/self/cmdline` kept the
  arguments.
- **A library.** First, `setup_args` checks that the constructor is in the
  program's own executable: its address lies inside the kernel's record of
  the program's code, `start_code` to `end_code` (`/proc/self/stat`,
  fields 26 and 27). (A comparison of the device and inode of
  `/proc/self/maps` and `/proc/self/exe`, this item's second version,
  fails on btrfs and on overlayfs before Linux 6.7: leanrs's review.) In a
  shared library it reads nothing and keeps "no arguments", so the title
  functions fail with `ENOBUFS`: natively a library calls no
  `uv_setup_args` either. A running host may own its arguments' memory
  (nginx's and PostgreSQL's own `setproctitle` keep pointers into it), so a
  library must never write it (leanrs's review of quirks-1).
- **A launch through the dynamic loader** (`ld.so ./prog`, also with
  `--argv0`) keeps "no arguments" too, and the title functions fail with
  `ENOBUFS`: the kernel's record of the code is then the loader's, so the
  program's own code reads as a library's. Natively the generated `main`
  hands its arguments to `lean_setup_args` and the title is written. This
  is a judged deviation of the shared runtime, **LQ1-01** (leanrs's review
  of quirks-1): telling that launch apart is possible in safe code (read
  the ELF header of the file mapped at `start_code` and see whether it is
  the program's `PT_INTERP`), but not worth the complexity for this rare
  launch mode. Case `uvsys/title_via_loader`
  (native's outcome, and ours as `alt1`).
- **The checks.** In the program's executable, `setup_args` keeps nothing,
  and the title is modelled from `std::env::args_os` without writing (the
  title, its cut and the thread's name are native's; `/proc/self/cmdline`
  keeps the arguments), when:
  - it cannot tell whether it is in the executable (no `/proc`, the code in
    an anonymous mapping);
  - another thread of the process runs code: `/proc/self/task` holds a
    task other than this one that is not an io_uring kernel thread
    (`PF_IO_WORKER` in its `stat` flags, on Linux 5.12 or later, where
    such threads and the flag exist; before 5.5 the same bit meant
    `PF_VCPU`, so on an older kernel any other task counts). Code that ran
    earlier (G4) may have started a thread; the io_uring thread is the
    polling ring's, when that code opened the startup descriptors;
  - `/proc/self/stat` or `/proc/self/task` cannot be read: no `/proc`, or
    too few free descriptors (the checks need two at once:
    `/proc/self/task` and a task's `stat`);
  - the memory from `argv[0]` to the NUL ending `argv[argc - 1]` is not
    inside the kernel's span of the arguments, `arg_start` to `arg_end`
    (`/proc/self/stat`, fields 48 and 49). That happens when an earlier
    constructor (a preloaded library, a dependency) pointed an entry of the
    table elsewhere, for example `argv[1]` to the heap: libuv's `cap` would
    then run from the stack into other memory. On a normal `execve`,
    `argv[0]` is `arg_start` and the memory ends at `arg_end` exactly.

  With `argc <= 0` it keeps "no arguments", and the title functions fail
  with `ENOBUFS`, as natively.
- **The region.** `Region::from_argv` computes `cap` as libuv does, makes
  the span check, and copies `argv[0]` as the first title. `setup_args`
  keeps the start, `cap` and that copy in the `static REGION`, a
  `Mutex<Option<Option<Region>>>`: `None` when it kept nothing it could
  check, `Some(None)` for no arguments, else the region.
- **libuv's copy.** `copy_and_repoint` copies every argument into one block,
  never freed (libuv's `args_mem`), and points each entry of the table at
  its copy. So `main`'s `argv`, glibc's `__libc_argv` and `std::env::args`
  give the original arguments for good, as Lean's `args` are natively: a
  translator that builds `args` after the initializers, or calls
  `std::env::args` late, gets them (case `uvsys/title_in_initializer`). No
  safe code reaches the arguments' memory any more.
- **The write.** `uvsys::set_process_title` cuts the title as libuv does
  and keeps it as the model's title, then calls `argv_title::write`.
  `Region::write` builds the memory's new contents in a fresh `Vec` of
  `cap` bytes (the title, then NUL bytes), and copies it over the arguments
  in one `copy_nonoverlapping`. The final bytes are those of libuv's
  `memcpy` and `memset`.
- **Linking.** A `#[used]` static alone does not keep its object file when
  the linker takes the crate from an archive (an rlib, a static library).
  `argv_title::initial` and `write`, which every title function calls,
  refer to the constructor (`std::hint::black_box`), so the object that
  holds it is linked wherever the title functions are.

### Without the feature

Without `proc-title`, `argv_title.rs` is not compiled: the crate has no
constructor, holds no arguments' memory, and compiles no `unsafe` code (the
root forbids it). The title functions of `src/io/uvsys.rs` then behave as
native's when libuv holds no arguments' memory, except that the title can
still be read:
- **`setProcessTitle`** gives Lean's embedded-NUL error for a title with a
  NUL byte (Lean checks before it calls libuv), and `UV_ENOBUFS` for any
  other: `uv_set_process_title` returns it when `uv_setup_args` kept nothing
  (`args_mem` null). Nothing changes: not the title, not the arguments'
  memory (`/proc/self/cmdline`), not the thread's name.
- **`getProcessTitle`** gives `argv[0]`, read once from `std::env::args_os`:
  the title a native program starts with (its generated `main` always calls
  `lean_setup_args`), and the model the feature's build uses when the
  constructor kept nothing. As natively, it is `UV_ENOBUFS` with no
  arguments, or for 512 bytes or more. (libuv without `uv_setup_args`
  would fail `uv_get_process_title` with `UV_ENOBUFS` too; no native Lean
  program runs without it, so the crate keeps native's `argv[0]`.)

So a program that only reads the title behaves as natively, and one that
sets it gets `ENOBUFS` where native writes it: a difference of the
translator that leaves the feature off (leanrs refuses `setProcessTitle` at
translation, DV2). The cases that set a title expect native's outcome, so
`tests/io2_cases.rs` and `tests/io_cases.rs` (`title_fd_limit`) check their
twins only with `proc-title`;
`uvsys/title_via_loader` passes in both builds, since its alternative
(LQ1-01) is that `ENOBUFS`.

### The `unsafe` operations

All are in `src/io/argv_title.rs`:
- **U1.** Reads of the pointer table: `argv[0]` and `argv[argc - 1]` in
  `Region::from_argv`, every entry in `copy_and_repoint`.
- **U2.** Reads of the strings (`CStr::from_ptr`): `argv[0]` and
  `argv[argc - 1]` in `Region::from_argv`, every argument in
  `copy_and_repoint`.
- **U3.** `Region::write` copies the new contents over the arguments:
  `copy_nonoverlapping(image.as_ptr(), start, cap)`.
- **U4.** `copy_and_repoint` writes each entry of the table:
  `argv[i] = copy`.
- The constructor itself: a `#[used] #[link_section = ".init_array.00100"]`
  static, and its call of `setup_args`.

The unit tests in the same file make `Region`s over blocks of their own,
and repoint a table of their own, under the same contracts.

### What the constructor relies on

- **G1. glibc's convention.** glibc calls each `.init_array` function with
  `argc`, `argv` and `envp`: `call_init` in `csu/libc-start.c` for the
  program (dynamic or static), before `main`; `_dl_init` in
  `elf/dl-init.c` for a shared library, when it is loaded, also later by
  `dlopen`, after `main` (so only the executable's case is used: "A
  library" above). `argv` is the process's table
  (`__libc_argv`): `argc` entries, then a null one, each pointing to a
  NUL-terminated string, on the stack. Rust's std relies on the same
  convention for `std::env::args` on glibc. musl calls them with no
  arguments, hence the cfg.
- **G2. The kernel's layout.** At `execve` the kernel copies the argument
  strings one after another, in order, onto the stack it maps for the new
  process, from `mm->arg_start` to `mm->arg_end` (`copy_strings` in
  `fs/exec.c`), which `/proc/self/stat` shows. The environment's strings
  follow. The main thread's stack is never unmapped while the process runs.
  `arg_start` and `arg_end` move only through `prctl(PR_SET_MM)`
  (`CAP_SYS_RESOURCE`, or `PR_SET_MM_MAP` for checkpoint/restore tools).
  The kernel also records the executable's code, `start_code` to
  `end_code` (`load_elf_binary`: its executable segments), which tells the
  program's code from a library's; for a launch through the dynamic loader
  it records the loader's (LQ1-01).
- **G3. Earlier code.** Code that ran before the constructor (a preloaded
  library, another constructor) may have changed the table's entries, but
  keeps each one null or pointing to a NUL-terminated string, holds no
  Rust reference into the arguments' memory across a title change, and did
  not call `PR_SET_MM`. A program that breaks G3 breaks native libuv the
  same way: in a program's executable, where alone the crate writes, the
  generated `main` hands `argv` to `uv_setup_args`, which trusts it
  entirely.
- **G4. The order of the constructors** (AR-20). The constructor's section
  is `.init_array.00100`. The linkers put the input sections
  `.init_array.N` first, by increasing priority N, then the plain
  `.init_array` sections, in link order: GNU ld's default script
  (`KEEP (*(SORT_BY_INIT_PRIORITY(.init_array.*) ...))`, then
  `KEEP (*(.init_array ...))`), gold and lld alike. glibc calls the
  executable's entries in that order (`call_init` in `csu/libc-start.c`).
  So in the executable the constructor runs:
  - after the entries with a lower priority, the toolchain's own: on
    aarch64, compiler-rt's detection of the CPU's features,
    `init_have_lse_atomics` and `__init_cpu_features` (90, in std's
    compiler-builtins; libgcc has the same at 90), libstdc++'s streams
    (90, when linked), and std's record of `argc` and `argv` (99). GCC
    reserves the priorities 0 to 100 for the implementation; the toolchain
    uses 90 and 99 of them, and the crate, the runtime of the program, 100;
  - before the entries with a priority above 100 and the plain ones: every
    translator's startup constructor (lean2rr's and the test glue's in
    `tests/io_cases.rs` are plain), C and C++ constructors without a
    priority, and crtbegin's `frame_dummy` (which std's 99 precedes too; on
    Linux the unwinder finds the frames through `PT_GNU_EH_FRAME`, not
    through it).

  The limits. Some code still runs before it: the executable's
  `.preinit_array`; the constructors of every shared library, preloaded
  (`LD_PRELOAD`) or a dependency (`_dl_init` runs them before
  `__libc_start_main` calls the executable's); and an entry of the
  executable with a priority below 100, or of 100 placed before it by link
  order. Such code may start a thread or take descriptors, which the checks
  see (the crate then keeps nothing); the proof does not depend on the
  order. A process that starts with fewer than two free descriptors keeps
  nothing either, where native writes the title. The constructor allocates
  (the copies, the `/proc` reads), so the program's global allocator must
  work before the program's other constructors run, as glibc's `malloc` and
  mimalloc do.

The checks make the rest hold: the program's executable before `main`,
one thread while the table is read and written, and a write that stays
inside the kernel's span. G4 decides only whether the title is written
when other startup code takes descriptors; no invariant below depends on
it.

### The invariants the crate maintains

- **I1. One origin.** Only `Region::from_argv` makes a `Region` (the type is
  private to the file). `REGION` is set only in `setup_args`, at most once:
  a later call sees it set (to a region, or to "no arguments") and returns
  before reading anything. `setup_args` is private, and only the
  constructor calls it.
- **I2. Bounds.** `from_argv` returns `None` unless `argc > 0`, `argv`,
  `argv[0]` and `argv[argc - 1]` are not null, the computation of `cap`
  does not overflow (checked arithmetic on the addresses),
  `cap > strlen(argv[0])` (so `cap >= 1`), and `[argv[0], argv[0] + cap)`
  lies inside `[arg_start, arg_end)`. By G2, those bytes are on the stack,
  valid for reads and writes for the rest of the process. `REGION` keeps
  the region until then.
- **I3. One lock.** Every access of `argv_title.rs` to the arguments'
  memory and the table holds `REGION`'s lock: `setup_args` holds it while
  `from_argv` and `copy_and_repoint` read (U1, U2) and repoint (U4);
  `write` takes it, and `Region::write` gets its `&mut Region` from the
  guard (U3). The file keeps no reference into the memory: it copies
  `argv[0]` into `first` and every argument into the block at once, and
  the `&CStr`s of U2 end inside their functions.
- **I4. One thread at setup.** `setup_args` reads and writes the table only
  after `in_main_executable` found the constructor inside the program's
  code, `start_code` to `end_code` (so it runs before `main`, G1, G2) and `no_other_thread` saw no
  task but this one and io_uring kernel threads (Linux 5.12 or later),
  which run no code of the process. No other thread can start
  while the constructor runs, since only this thread runs code. Threads
  started later start after the table was written (thread creation orders
  it).
- **I5. No reader after setup.** After `copy_and_repoint`, the table points
  at the copies, so `std::env::args`, `main`'s `argv` and glibc's
  `__libc_argv` read the copies. The crate's own fallback
  (`std::env::args_os` in `uvsys`) runs only when no region was kept. The
  only pointers into the arguments' memory left are the `Region`'s and
  glibc's `program_invocation_name` and `program_invocation_short_name`
  (below, "outside the proof").
- **I6. Lengths.** `Region::write` keeps `len = min(title.len(), cap - 1)`
  bytes, and its `image` has exactly `cap` bytes.
- **I7. No overlap.** `image` is a fresh allocation, and so is the block of
  copies. The arguments' memory is live memory neither owns: the kernel's
  stack, or a test's own buffer. So they do not overlap.

### Proof

- **U1** reads entries below `argc` of glibc's table (G1), which no other
  thread touches (I4); in `from_argv`, indices 0 and `argc - 1`, with
  `argc > 0`.
- **U2** reads NUL-terminated strings (G1, G3). No write can run at the same
  time: the crate's writes hold `REGION`'s lock, which `setup_args` holds
  (I3), and no other thread runs code (I4).
- **U3**:
  - the destination is valid for `cap` writes (I2, G2);
  - the source is valid for `cap` reads (I6) and does not overlap the
    destination (I7);
  - no other access runs at the same time: the crate's own are under the
    lock (I3), safe code reads the copies (I5), and earlier code holds no
    reference into the memory (G3);
  - `u8` needs no alignment.
- **U4** writes entries below `argc` of glibc's table, on the writable
  stack (G1), with one thread (I4); no reference into the table is live
  (the crate holds none; G3).
- **The constructor.** glibc calls it with the process's `argc` and
  `argv` (G1); `setup_args` reads and writes only when the constructor is
  in the program's executable, where glibc calls it before `main`. Nothing
  in `setup_args` unwinds but a failed
  allocation, which aborts; an unwind could not leave an `extern "C"`
  function anyway (Rust aborts there). `REGION`'s lock is a `std` mutex,
  usable before `main`.

`Region` holds its start in an `AtomicPtr<u8>`, read through `get_mut`
under the lock. That makes `Region` `Send`, and `REGION` a valid `static`,
without an `unsafe impl`. Moving the pointer between threads is sound: the
memory belongs to the process, not to a thread.

### What is outside the proof, as natively

- glibc's `program_invocation_name` and `program_invocation_short_name`
  point into `argv[0]`'s memory. A C function that prints them (`error`,
  `err`, `warn`, glibc's assertion message, `syslog` without an ident)
  while another thread changes the title would race with the write.
  Native has the same race, since libuv's lock is its own; neither Lean's
  runtime nor this crate calls such functions.
- The kernel reads `/proc/<pid>/cmdline` without the program's
  synchronization. That is not an access of the program, so not a data
  race. A concurrent reader may see part of the old title and part of the
  new one, as natively.

### How it is checked

- The native cases `uvsys/title_cmdline` (the memory after a title, the
  cut, the environment, `args`), `uvsys/title_in_initializer` (a title set
  by a module initializer leaves `args` whole), `uvsys/title_via_loader`
  (LQ1-01) and `uvsys/process_title`
  run through their twins in `tests/io2_cases.rs`, which links the crate's
  constructor as any binary does and has no constructor of its own, in
  `scripts/check.sh`'s configurations with `proc-title` (`io,proc-title`
  and `io,sched,proc-title,unsafe-fast`).
- The constructor is linked, and works, in a downstream binary built each
  way the translators build (2026-10-04): cargo, debug and release; plain
  `rustc` rlibs linked by `rustc` (lean2rr's leanrt is a plain-rustc rlib
  over the cargo-built crate); and a static library linked into a C `main`
  by `cc`. With the reference removed, these builds kept the constructor
  too; the reference makes it a guarantee. Rechecked for AR-20
  (2026-10-04), each with a startup constructor of its own (plain
  `.init_array`, or a C constructor without a priority) that opens eight
  descriptors: the binaries' `.init_array` lists the toolchain's entries,
  std's `argv` record, the crate's constructor, `frame_dummy`, then the
  startup constructor, and under `ulimit -n 12` each writes the title. With
  GNU ld 2.42 for the four builds, and with gold 1.16 and LLD 23.1 (rust-lld)
  for the plain-`rustc` one. In a `cdylib` that a C host
  loads with `dlopen`, the title functions fail with `ENOBUFS`, and the
  host's `/proc/self/cmdline` and `argv` stay unchanged. Started through
  the dynamic loader, a program keeps no arguments' memory (LQ1-01). leanrs
  also checked a fat-LTO, one-codegen-unit, `panic=abort`,
  `--gc-sections` build.
- The case `uvsys/title_fd_limit` (AR-20: `ulimit -n 12`, native writes the
  title) through its twin in `tests/io_cases.rs`, whose startup
  constructor opens native's startup descriptors in plain `.init_array`, as
  a translator's does; checked with `proc-title` only. With the crate's
  constructor in plain `.init_array` (before AR-20), the twin fails:
  `/proc/self/cmdline` keeps the arguments.
- The unit tests in `argv_title.rs`: `cap`, the copy, the NUL fill and the
  cut on a block laid out as the kernel lays out the arguments; a table
  that skips a string (libuv's rule; such a table comes from a launch
  through the dynamic loader, which keeps nothing here); the layouts that keep
  nothing, including memory outside the kernel's span; the repointed
  table; the parsing of `/proc/self/stat`. Miri runs them
  (`LEAN_RUNTIME_MIRI=1 LEAN_RUNTIME_MIRI_FILTER=argv_title
  scripts/check.sh`; also passed with Tree Borrows and strict provenance,
  2026-10-04), except the one that reads this process's `/proc`. Miri
  cannot run the constructor: it has no process arguments' memory.
- Adversarial review: ours, then leanrs's before merge.
- Kani does not apply: the proof is about ownership and ordering of memory
  accesses, not about arithmetic.

### Alternatives ruled out

- **Writing `/proc/self/mem`** at the address `/proc/self/stat` gives as
  `arg_start`. It needs only safe calls, but it is the same write, hidden
  from the compiler and from the lint. Rust's safety guarantees exclude it
  (std's documentation on `/proc/self/mem`). io-2's review rejected it
  (LIO2-06).
- **`prctl(PR_SET_MM, ...)`** to point the kernel at a buffer of the
  crate's own. `PR_SET_MM_ARG_START` and `PR_SET_MM_ARG_END` need
  `CAP_SYS_RESOURCE`, which a normal process lacks. `PR_SET_MM_MAP` needs
  none, but it resets the whole memory map at once (code, data, stack,
  arguments, environment, auxiliary vector), and its `brk` can race with
  `sbrk` (the allocator). In rustix 1.1.4 both are `unsafe`
  (`set_virtual_memory_map_address`, `configure_virtual_memory_map`), and
  nix 0.31.3 has no `PR_SET_MM`.
- **std** exposes no access to the arguments' memory: `std::env::args_os`
  returns copies, and std keeps `argc` and `argv` in private statics.
- **The constructor in each translator's glue**, with a `pub unsafe fn` for
  it to call (this item's first version): each glue would write `unsafe`,
  and the contract asked the whole program not to read the arguments late.
  The crate's own constructor needs neither, and the reference keeps it
  linked.
- **Keeping the deviation** (io-2's LIO2-06): the owner decided against it.

## Lean's stack-overflow report (`src/sched/stack_overflow.rs`)

This section is self-contained: with it and the source of
`src/sched/stack_overflow.rs`, `src/sched/ctx.rs` (the stacks and the hub)
and corosensei 0.3.4's `DefaultStack`, a reader can check the item
(AR-11). It replaces the earlier contract, under which each glue wrote its
own SIGSEGV handler (lean2rr's `rt.rs`, the driver's `glue.rs`) and leanrs
had none. The file is compiled only with the feature `stack-overflow`
(with `sched`, or with `threads` in threads mode); lean2rr enables it, and
leanrs decides at its adoption of `sched` (its DV6 until then).

### What native does

`src/runtime/stack_overflow.cpp` (Lean 4.34.0):
- **Per thread.** Every thread Lean starts (the main thread, and each task
  manager worker: `lthread`'s `_main` holds a `stack_guard`) gets an
  alternate signal stack of `SIGSTKSZ` bytes (`malloc`, `sigaltstack`).
- **Process-wide.** `initialize_stack_overflow` installs `segv_handler`
  for SIGSEGV and SIGBUS with `SA_SIGINFO | SA_ONSTACK`, where the
  disposition is the default.
- **The handler.** If the fault's address lies in the page below the
  faulting thread's stack (`is_within_stack_guard`: the stack address that
  `pthread_getattr_np` reports, less one page), it writes
  `\nStack overflow detected. Aborting.\n` to descriptor 2 and calls
  `abort`: status 134, and buffered output is lost. Otherwise it sets the
  disposition back to the default and returns: a fault runs its
  instruction again and the default action ends the process (status 139);
  a SIGSEGV sent by `kill` is consumed.

Case `tasks/stack_overflow_in_task`: a task overflows, Lean's message,
status 134.

### What the crate does

A task runs on a context: a corosensei coroutine on a `DefaultStack`, an
`mmap` of `PROT_NONE` whose part above the lowest page is made writable, so
one guard page lies below each stack (`bounds_of` in `ctx.rs`). Rust's
handler knows only the guards of threads, and a glue's handler would need
the scheduler's knowledge of which stack runs. So the crate owns the
handler:
- **The opt-in.** `sched::install_stack_overflow_handler()` registers the
  calling thread, then, once per process (`Once`), installs `on_fault` for
  SIGSEGV and SIGBUS with `SA_SIGINFO | SA_ONSTACK`, over whatever handler
  is there (Rust's), which it keeps as the previous action; a disposition
  set to ignore is left alone. `sched::start` registers its thread too,
  once the handler is installed.
- **Registering a thread** gives it an alternate signal stack if it has
  none (Rust gives its own threads one where std installed its handler at
  its runtime start), and a record in the table: the
  thread's key (the address of its `errno`), the guard below its own stack
  (`pthread_getattr_np`, as Lean computes it), and the guard of the context
  running on it. A thread's `Registration`, a thread-local with a
  destructor, frees the record for reuse when the thread ends. In threads
  mode only, it also gives the crate's alternate stack, if it made one,
  back to a free list for the next registration (I6, review RT1-03). With
  `sched`, a registered thread ends only at the exit, so a block stays
  with its thread, and a `sched` build compiles the code of fb8f548 there
  (review AR-30: no `unsafe` that nothing needs). The table
  is a list of chunks of 64 records: a thread that finds every record taken
  appends a chunk (`OnceLock<Box<Chunk>>`, allocated at registration, never
  freed), so the table grows with the number of live registered threads,
  without a bound (review RS3-01).
- **Publication.** The hub calls `ctx::publish` right before it resumes a
  context and right after the context suspends or ends (and the panic
  guard after a panic): it updates the thread-locals behind
  `running_stack()` and, through `stack_overflow::publish`, the record's
  context guard.
- **The handler** (`on_fault`) finds the faulting thread's record by its
  key, with atomic loads, and when the fault's address lies in either of
  its guards, writes Lean's message and aborts. Otherwise it forwards the
  fault to the previous action.
- **Threads mode** (feature `threads`, `docs/threads.md`). A task runs on
  its thread's own stack, as natively: there is no context. Every thread
  the task manager makes (a standard worker, a dedicated task's thread)
  calls `install_stack_overflow_handler()` at its entry, after std's
  start, as each native `lthread` builds its `stack_guard`. Its record
  holds its own guard; the context guard stays 0, since `running_stack()`
  is always `None` and nothing calls `publish`. So I4 holds vacuously, and
  A1 holds for an overflow of a thread's own stack.

### The `unsafe` operations

All are in `src/sched/stack_overflow.rs`:
- **U1.** `__errno_location()`, the thread's key.
- **U2.** The read and the write back of `errno` in the handler.
- **U3.** The handler's reads of `si_code` and `si_addr` from the
  `siginfo_t` the kernel passes.
- **U4.** `write(2)` of the message.
- **U5.** `sigaction` back to the default (nix's wrapper), in the handler.
- **U6.** The previous handler's address made a function pointer
  (`transmute`), to call it.
- **U7, U8.** The install: a query of the current disposition and of its
  mask (`sigismember`), then `sigaction` with `on_fault` (nix's wrapper).
- **U9.** `pthread_self`, `pthread_getattr_np`, `pthread_attr_getstack`
  and `pthread_attr_destroy`, for the thread's own guard.
- **U10.** `sysconf(_SC_PAGESIZE)` and `getauxval(AT_MINSIGSTKSZ)`.
- **U11, U12.** `sigaltstack`: a query, then a new alternate stack.
- **U13.** In the handler, for a call of the previous handler:
  `sigemptyset`, `sigaddset` and `pthread_sigmask` on local `sigset_t`s
  (its mask blocked, the signal unblocked under `SA_NODEFER`, then the
  mask before restored).
- **U14.** `sigaltstack` with `SS_DISABLE`, at a thread's end, before its
  block goes to the free list (review RT1-03); U11's query comes first.
  Threads mode only (feature `threads`, review AR-30).

Which items each mode compiles (review AR-30): with `sched`, U1 to U13,
and I6 without its free list; with `threads`, U1 to U14, and I6 with its
free list. The free list (`FREE_ALTSTACKS`, `give_back_altstack`, the
thread-local `OWN_ALTSTACK`) and its reuse in `ensure_altstack` (an
address exposed with `expose_provenance` and taken back with
`with_exposed_provenance_mut`) exist only with `threads`.

### What it relies on

- **G1. Linux signal delivery.** A SIGSEGV or SIGBUS caused by a fault is
  delivered to the faulting thread, which runs the handler and runs
  nothing else until it returns; the handler sees every store the
  interrupted code made before the fault, in program order (one thread).
  With `SA_ONSTACK`, the kernel builds the signal frame on the thread's
  alternate stack when one is set and the thread is not already on it;
  without one, an overflowed stack cannot take the frame, and the kernel
  kills the process with SIGSEGV. A fault while the handler runs with the
  signal blocked kills the process too. `SA_SIGINFO` handlers get a valid
  `siginfo_t`; `si_addr` is meaningful when the kernel generated the
  signal (`si_code > 0`).
- **G2. Async-signal-safe calls.** POSIX lists `write`, `abort`,
  `sigaction`, `sigemptyset`, `sigaddset` and `pthread_sigmask` as
  async-signal-safe (glibc's `pthread_sigmask` is the `rt_sigprocmask`
  system call). `errno` may be used in a handler (a
  handler must save and restore it), so `__errno_location` is too: glibc's
  `errno` is glibc's own initial-exec thread-local, set up when the thread
  is created. std's own SIGSEGV handler keys its per-thread data by the
  same address (`library/std/src/sys/pal/unix/stack_overflow/thread_info.rs`).
- **G3. The context stacks.** corosensei 0.3.4's `DefaultStack::new(size)`
  maps `size` plus one page `PROT_NONE` and makes all but the lowest page
  writable, so the guard is exactly that page, `[limit, limit + page)`
  (`bounds_of`). A stack is unmapped only when its `DefaultStack` is
  dropped: in `after_resume` after its context ended, or when the pool
  drops it, both after the context's last `publish(None)`.
- **G4. The previous action.** The disposition a handler replaces was set
  by its owner with `sigaction`: a function with one argument, or three
  when `SA_SIGINFO` is set, that stays mapped while it is installed, with
  the flags and mask its owner chose; the kernel would call it with that
  mask (and the signal) blocked, the signal unblocked under `SA_NODEFER`,
  and with the disposition reset to the default first under
  `SA_RESETHAND` (a one-shot handler). Which it is depends on when the glue
  installs (review SO-2):
  - in a Rust program whose `main` is Rust's, installed from `main` or
    later, it is std's handler (the executable's code), which std's
    runtime start (`std::rt::init`, before `main`) installs where it finds
    the default disposition, together with alternate stacks for the
    threads std spawns;
  - with a C-style entry, where std's runtime start never runs (lean2rr's
    `leanrt`), or in a C program that embeds the crate, it is the default;
  - installed before std's runtime start (from an ELF constructor), it is
    the default too, and std then finds the crate's handler, installs
    neither its handler nor the alternate stacks of the threads it spawns:
    a Rust thread that does not register loses Rust's report (its
    overflow ends with SIGSEGV, 139). Hence the rule: the glue installs
    from `main`, after Rust's runtime has started, never from an ELF
    constructor.
- **G5. No concurrent change at install.** No other code changes the
  dispositions of SIGSEGV and SIGBUS while `install_stack_overflow_handler`
  runs the first time (at the program's start). If one did, the handler
  would forward to the disposition it read, still a valid action of G4.

### The invariants the crate maintains

- **I1. One handler.** `on_fault` is installed once per process
  (`INSTALL`), for SIGSEGV and SIGBUS, with `SA_SIGINFO | SA_ONSTACK` and
  an empty mask (the signal itself is blocked while it runs).
- **I2. One writer per record.** A thread claims a free record with a
  compare-and-swap of its key from 0 to `CLAIMING` (a value no `errno`
  address has), or reuses the record that already has its key (left by
  a thread with the same `errno` whose end did not free it). From then on
  only that thread writes the record: `register_thread`, `publish` (called
  only by the hub on its thread) and the `Registration`'s destructor. It
  stores its key last (`Release`). Other threads only compare the key with
  their own, and keys are unique among live threads.
- **I3. A whole guard or none.** Each guard is written `lo = 0`, then
  `hi`, then `lo` (`Record::set_pair`), with compiler fences between the
  stores, so a handler that interrupts the writes on the same thread (G1)
  reads `lo = 0` (no guard) or a guard whose `lo` and `hi` belong
  together. While the handler runs, its record does not change (I2, G1).
- **I4. A published guard is a live guard page.** `ctx::publish(Some)` runs
  in `hub` right before `resume`; `publish(None)` right after `resume`
  returns, and in `PanicGuard::drop` when a panic comes out of it; both
  before `after_resume` pools or drops the stack (G3). So the record's
  context guard is the guard page of a mapped stack, and it is set exactly
  while code may run on that stack.
- **I5. The previous action.** For each signal, `PREV_FLAGS` (the whole
  `sa_flags`), `PREV_MASK` (its `sa_mask`, signals 1 to 64) and `PREV` (the
  `sa_sigaction`, last) are stored (`Release`) before `on_fault` is
  installed for it and never again; the handler loads them (`Acquire`). So
  the handler sees the action read from the kernel at install (G5).
- **I6. Every registered thread has an alternate stack**: `register_thread`
  calls `ensure_altstack` before it claims the record. An alternate stack
  the crate makes is `AT_MINSIGSTKSZ` (the kernel's largest signal frame on
  this machine, at least `SIGSTKSZ`) plus 64 KiB, a heap block kept for the
  life of the process, never referenced by Rust code. With `sched`, each
  block is made for one thread and stays its alternate stack (as at
  fb8f548). In threads mode only (review AR-30), **a block is the
  alternate stack of at most one live thread** (review RT1-03): a new one,
  or one taken from the free list (`FREE_ALTSTACKS`), which holds only
  blocks that no thread has as its alternate stack. A thread puts its block
  there in its `Registration`'s destructor, and only after the kernel no
  longer uses it for that thread: if the block is still the thread's
  alternate stack, the thread disables it first (U14), and it never does so
  while it runs on it (`SS_ONSTACK`). This assumes that no code re-installs
  the crate's block on that thread afterwards, in a later destructor. So
  the blocks number at most the largest count of threads alive at once
  with one of the crate's.
- **I7. An append-only table.** A chunk is reached from the static first
  chunk through `OnceLock`s that are set once, at a registration, and
  never cleared; a chunk is never freed. The handler walks the chunks with
  `OnceLock::get`, which never blocks and allocates nothing (its
  documented contract; that it is one atomic load is std's current
  implementation, not part of the contract): a chunk being added on the same
  thread when the handler interrupts it reads as absent, and it holds no
  record of that thread yet.

### Proof of what the handler needs

- **A1. Delivery on the alternate stack.** A context's stack overflows on
  a registered thread (only a scheduler's thread runs contexts, and
  `sched::start` registers it once the handler is installed). The fault
  is delivered to that thread (G1), which has an alternate stack (I6), and
  `on_fault` has `SA_ONSTACK` (I1): the frame goes on the alternate stack,
  not on the overflowed one. The same holds for an overflow of the
  thread's own stack. Which alternate stack it is (review RS3-02):
  - on a Rust thread (the translators' `main` threads, std-spawned
    threads), std's own: the larger of `SIGSTKSZ` and `AT_MINSIGSTKSZ`
    (16 KiB on this aarch64 host, whose kernel frame is at most 4720
    bytes), with a guard page below it. The kernel's frame, the handler's
    own frames (a few hundred bytes, two local `sigset_t`s) and, on a
    forward, the previous handler's share std's slack, as std's handler
    alone would; were it exceeded, the fault on the guard page with the
    signal blocked kills the process (G1), it corrupts nothing;
  - on a thread that had none (a C thread), the crate's: `AT_MINSIGSTKSZ`
    (at least `SIGSTKSZ`) plus 64 KiB.
- **A2. Async-signal-safety.** The handler allocates nothing, takes no
  lock and reads no thread-local of the crate:
  - `errno_location` is `__errno_location` (G2);
  - `record_of` and `Record::covers` are loads of atomics in the table's
    chunks (`AtomicUsize` is lock-free on the crate's targets), reached
    with `OnceLock::get` (I7);
  - the report is `write(2)` and `abort(3)` (G2; `std::process::abort` is
    `abort`);
  - the forward is `sigaction(2)` through nix's wrapper, which only fills a
    `sigaction` on the stack (G2), or a call of the previous handler, whose
    safety as a SIGSEGV handler is its owner's (G4), made with that
    handler's mask: `sigemptyset(3)`, `sigaddset(3)` and
    `pthread_sigmask(3)` around the call (G2, U13), all async-signal-safe;
  - `errno` is saved first and restored before the handler returns.

  It reads no thread-local of the crate on purpose. A Rust `thread_local!`
  with a constant initializer and no destructor is a plain load at a fixed
  offset from the thread pointer in an executable, but in a library loaded
  by `dlopen`, glibc may allocate the thread's block of the library's
  thread-locals at its first access (`__tls_get_addr`), which is not
  async-signal-safe; std moved its own handler off thread-locals for that
  reason. The table needs no thread-local: the key is `errno`'s address.
  (`SLOT` and `REGISTRATION` are read only outside the handler.)
- **A3. The window around a switch.** `publish(Some(b))` runs on `main`'s
  stack before the switch to the context, and `publish(None)` on `main`'s
  stack after the switch back (I4). In each window, code runs on `main`'s
  stack (the hub's frames and corosensei's switch) while the record names
  the context's guard. The handler classifies by address, and checks both
  guards: the thread's own guard and the context's are different pages (a
  thread stack and an `mmap` of corosensei's), so
  - an overflow of `main`'s stack in the window faults in the thread's
    guard, which the record holds whatever the context guard says: Lean's
    message, as natively;
  - the context's guard can be touched only by code on the context's stack,
    which runs only between the two publications, when the record holds
    that guard;
  - a handler that interrupts a publication sees no context guard or a
    whole one (I3), and either is right, since the code being interrupted
    runs on `main`'s stack.

  (The driver's former handler checked only the context's guard while one
  was published, and would have taken an overflow of `main`'s stack in the
  window for another fault: status 139 without the message.)
- **A4. Other faults go where they went before.** A fault on a thread
  without a record, or whose address lies in neither guard, or a signal
  not raised by a fault (`si_code <= 0`, whose `si_addr` is no address), is
  forwarded (I5):
  - a previous handler is called as the kernel would call it (G4; review
    SO-1): under `SA_RESETHAND` the default action is restored first; its
    mask is blocked for the call, and the signal unblocked under
    `SA_NODEFER` (U13), the mask before restored after; a handler with
    `SA_SIGINFO` gets the kernel's three arguments, another the signal.
    So a one-shot handler that returns without repairing the fault runs
    once, and the fault, run again, takes the default action (139), as
    without the crate; calling it with `on_fault` still installed would
    loop forever (`so_prev_resethand`);
  - where the previous handler is std's (Rust's runtime started before the
    install, G4), it reports an overflow of a Rust thread's own stack (its
    message, `abort`); otherwise it sets the default action and returns,
    and so does `on_fault`: the faulting instruction runs again and the
    default action ends the process (status 139), as natively;
  - a previous default (or ignore, or `on_fault` itself, which cannot
    happen under I1) restores the default and returns, as Lean's handler
    does.

### Proof of the `unsafe` operations

- **U1** has no precondition (G2); its result is valid for the thread's
  life.
- **U2** reads and writes the calling thread's `errno` through the pointer
  of U1, aligned and valid; no reference to it is live in Rust code (the
  interrupted code reaches it through the same function).
- **U3** dereferences the `siginfo_t` pointer the kernel passes to an
  `SA_SIGINFO` handler (G1), read only; `si_addr` is read only when
  `si_code > 0`.
- **U4** writes `MESSAGE.len()` bytes from a static buffer.
- **U5** installs the default action, a valid disposition for both
  signals; nix's wrapper passes a `sigaction` it built on the stack.
- **U6** turns `PREV[k]` into a function pointer: by I5 and G5 it is the
  `sa_sigaction` the kernel recorded before `on_fault`, neither `SIG_DFL`
  nor `SIG_IGN` (checked) nor `on_fault`; by G4 it is a function of the
  signature that `SA_SIGINFO` in `PREV_FLAGS[k]` names, still mapped. It
  is called with the arguments the kernel gave `on_fault`.
- **U7** passes a null new action and a `MaybeUninit<sigaction>` to write;
  it is read only on success, and `sigismember` reads its initialized
  `sa_mask`. **U8** installs `on_fault`: an `extern "C"`
  function with the `SA_SIGINFO` signature, in the crate's code, sound at
  any point of the program (A2), for SIGSEGV and SIGBUS (never SIGKILL or
  SIGSTOP).
- **U9.** `pthread_getattr_np` initializes the zeroed `pthread_attr_t` for
  the calling thread; `pthread_attr_getstack` reads it after success into
  two locals; `pthread_attr_destroy` destroys it once. Neither runs in the
  handler (`pthread_getattr_np` reads `/proc/self/maps` for the main
  thread, as Lean's handler does at each fault).
- **U10** have no precondition.
- **U13** works on local `sigset_t`s: `sigemptyset` initializes one before
  `sigaddset` and `pthread_sigmask` read it (glibc ignores the signals it
  reserves); the mask before is written by a successful `pthread_sigmask`
  and only then restored.
- **U11** passes a null new stack and a `MaybeUninit<stack_t>` to write,
  read only on success. **U12** gives the kernel `size` bytes of a block
  allocated for that and leaked (`Box::into_raw`), or, in threads mode, of
  such a block from the free list: valid for the rest of the process, no
  Rust reference points into it, so the kernel's writes alias nothing, and
  no other thread has it as its alternate stack (I6). It is set only when
  the thread had no alternate stack (`SS_DISABLE`), so the thread does not
  run on one at that moment. In threads mode, a block whose `sigaltstack`
  fails goes back to the free list.
- **U14** (RT1-03, threads mode only) disables the calling thread's
  alternate stack in its `Registration`'s destructor, outside any handler,
  after U11 showed that the block is the current one and that the thread
  does not run on it (`SS_ONSTACK` clear). Disabling hands the kernel no
  memory. From then on the kernel delivers no signal of this thread on the
  block, so another thread may take it from the free list (I6).

### What is outside the proof, as natively

- **A frame larger than the guard page** that skips it (C code without
  stack probes, `alloca`) faults below the guard, or writes into whatever
  lies there. Native Lean's rule is the guard page only, and so is this
  one: no message, status 139 (or memory corruption, as natively). Rust
  and both translators' generated code probe their stacks page by page.
  lean2rr's former handler also took a fault below the guard with the
  stack pointer below it (`past_end`, from the interrupted context's stack
  pointer, read at architecture-specific offsets of `ucontext_t`); that
  goes beyond native's rule and is not taken over.
- **The table** keeps every chunk it ever appended, 64 records each: its
  size follows the largest number of registered threads alive at once.
- **Unloading the crate.** A library that holds the crate, unloaded with
  `dlclose` while the handler is installed, leaves the kernel pointing at
  unmapped code: the next SIGSEGV or SIGBUS jumps there. Nothing takes the
  handler back (it is installed for the life of the process, as Lean's);
  a host that loads such a library must never unload it (review RS3-02).
  Neither translator builds a loadable library.
- **Another handler installed later** (by the program or a library)
  replaces `on_fault`; the report is then that handler's business.
- **The alternate stacks the crate makes** are kept for the life of the
  process: one per registered thread that had none. In threads mode they
  are given back to a free list when the thread ends, and reused (I6,
  review RT1-03); with `sched`, a registered thread ends only at the exit. A thread has
  none when std installed no handler at its runtime start, so it spawns its
  threads without one: a C-style entry (lean2rr's `leanrt`), SIGSEGV or
  SIGBUS ignored when the program started, or another handler installed
  before std's start. Otherwise std gives its threads one, and the crate
  makes none. The blocks number at most the largest count of such threads
  alive at once; in threads mode, which makes a thread per dedicated task,
  they no longer grow with the number of ended threads.

### How it is checked

- The case `tasks/stack_overflow_in_task` (native: the message, status
  134) through its twin in `tests/sched-driver`, which builds the crate
  with `stack-overflow` and whose glue only calls
  `sched::install_stack_overflow_handler()`. `scripts/check.sh` builds and
  tests `io,sched` and `net` without the feature (the root forbids
  `unsafe` there), and the configuration with every feature but `net`
  with it.
- The driver's tests `so_main_overflow` (`main`'s own stack, after a task
  ran on a context), `so_task_overflow_after_switches` (a task overflows
  after it and another blocked and resumed), `so_segv_in_task` (a fault
  that is no overflow, in a task: status 139, no message),
  `so_rust_thread_overflow` (a Rust thread that never registered: Rust's
  report, so the forward reaches std's handler) and
  `so_second_scheduler_thread` (`sched::start` on another thread registers
  it), and `so_prev_resethand` (review SO-1: a one-shot previous handler
  with `SA_SIGINFO | SA_RESETHAND | SA_ONSTACK` and a mask, installed
  before the crate's: one `prev`, then 139, as without the crate).
- The unit tests in `stack_overflow.rs`: the guard arithmetic, a record
  covering both guards (the window, A3), and a registered thread's record,
  alternate stack and release at its end; in threads mode only, the reuse
  of the alternate stacks (`ended_threads_give_their_alternate_stacks_back`,
  review RT1-03). `scripts/check.sh` runs them with `sched,stack-overflow`
  and with `threads` and `stack-overflow` (review AR-30).
- Mutation checks (2026-10-04): forwarding always to the default fails
  `so_rust_thread_overflow`; no publication fails
  `stack_overflow_in_task` and `so_task_overflow_after_switches`; no
  registration in `sched::start` fails `so_second_scheduler_thread`; a
  record that checks only the context's guard while one is published (the
  former driver's rule) fails `a_record_covers_both_guards`; calling a
  one-shot previous handler without restoring the default first (review
  SO-1) loops in `so_prev_resethand` (a timeout, `prev` printed again and
  again).
- Miri cannot model the item: every `unsafe` operation is a foreign call
  or signal delivery. Kani does not apply.
- Adversarial review: ours, then leanrs's before merge.

### Without the feature

Without `stack-overflow`, `stack_overflow.rs` is not compiled: no handler,
no records, no alternate stack of the crate's, no `unsafe` code (the root
forbids it), and `install_stack_overflow_handler` does not exist. The hub's
`publish` updates only the thread-locals of `running_stack()`, as before
AR-11, so a switch costs nothing more. A task that overflows its context's
stack then ends with a plain SIGSEGV (status 139, no message): Rust's
handler knows only the guards of threads, takes the fault for another one,
restores the default action and returns. An overflow of a Rust thread's own
stack gets Rust's message (`thread '...' has overflowed its stack`) and an
abort (134), where native prints Lean's message.

### Alternatives ruled out

- **Rust's handler** reports only the guard of a thread's own stack,
  with Rust's message.
- **corosensei's trap support** (`CoroutineTrapHandler::setup_trap_handler`)
  is `unsafe`, and is for resuming a coroutine after a trap, not for a
  report.
- **signal-hook** refuses SIGSEGV (`FORBIDDEN`).
- **A handler in each glue** (the former contract): each translator
  writes the same `unsafe` handler, and must know the scheduler's stacks
  (`running_stack()`), whose thread-locals are not async-signal-safe in
  every link mode (A2).
