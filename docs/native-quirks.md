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
  without an entry in `UNSAFE.md`; a build with neither `proc-title` nor
  `unsafe-fast` has no such file, and there the root forbids `unsafe`
  outright;
- every `unsafe` block has a `// SAFETY:` comment naming the invariant it
  relies on;
- `UNSAFE.md` has its entry, and this file its invariants and proof;
- leanrs reviews it before merge.

So far there is one item.

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
  (`#[link_section = ".init_array"]`, cfg `target_os = "linux"` and
  `target_env = "gnu"`). glibc calls it, as every `.init_array` function,
  with the process's `argc`, `argv` and `envp`: before `main` when it is
  in the program's executable, when the library is loaded when it is in a
  shared library (by `dlopen`, also after `main`). It calls `setup_args`,
  so no translator's glue takes part and none writes `unsafe`.
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
    `PF_VCPU`, so on an older kernel any other task counts). An earlier
    constructor may have started a thread; the io_uring thread is the
    polling ring's, when the startup descriptors are open first;
  - `/proc/self/stat` cannot be read: no `/proc`, or no free descriptor;
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
`tests/io2_cases.rs` checks their twins only with `proc-title`;
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
- The constructor itself: a `#[used] #[link_section = ".init_array"]`
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

The checks make the rest hold: the program's executable before `main`,
one thread while the table is read and written, and a write that stays
inside the kernel's span.

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
  too; the reference makes it a guarantee. In a `cdylib` that a C host
  loads with `dlopen`, the title functions fail with `ENOBUFS`, and the
  host's `/proc/self/cmdline` and `argv` stay unchanged. Started through
  the dynamic loader, a program keeps no arguments' memory (LQ1-01). leanrs
  also checked a fat-LTO, one-codegen-unit, `panic=abort`,
  `--gc-sections` build.
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
