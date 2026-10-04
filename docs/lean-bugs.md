# Bugs in Lean's own runtime that this crate does not reproduce

lean-runtime mirrors Lean 4.34.0's C runtime, with one exception: where
Lean's runtime is wrong, both translators do the right thing instead. A
behaviour counts as a bug only after a judged verdict:

- the C source lines responsible;
- why it is wrong: the C standard, POSIX, Lean's own documentation or
  evident intent, or lost data or a crash;
- a minimal native repro showing a wrong value, not just an odd symptom.

Anything not confirmed is followed exactly as native does it. The "Not bugs"
section records the candidates that failed this bar, so they aren't raised
again.

Every confirmed bug has:
- an entry below;
- a case in `tests/cases/` with `deviations` naming the entry, whose
  `expected` is native's output, or the correct output with native's
  recorded in a `native` field (rows; program cases such as LB-13's; also
  when native is nondeterministic). io-2's program cases (LB-03, LB-14 to
  LB-17) keep native's output and give the correct one as `<id>.alt1.*`,
  which alone `scripts/cases.py check` accepts (to move to `native` later).

Each translator also lists it among its intended differences. The owner's
decision (2026-10-03): these bugs are not reported upstream. They are
recorded here only; the Upstream field notes what upstream already knows.

## Entry format

| Field | Content |
|---|---|
| Id | `LB-nn` |
| Summary | One line |
| Where | File and lines in Lean 4.34.0's source |
| Why it is a bug | The standard, the documentation or the data loss, quoted where possible |
| Native repro | What native does; the case id |
| Our behaviour | What lean-runtime and both translators do instead |
| Translators | lean2rr: plan §10 entry; leanrs: DV id |
| Upstream | Not reported / issue or PR / known |
| Verdict | Which judge, when |

## Confirmed

### LB-01: a concurrent `IO.Ref.set` can be lost

| Field | Content |
|---|---|
| Summary | A multi-threaded `IO.Ref.get` can undo a concurrent `set`: the ref reverts to its old value after the `set` has returned |
| Where | `src/runtime/io.cpp`, `lean_st_ref_get` (lines 1459-1484). The value is taken out with `exchange(nullptr)` (1467) and put back with an unconditional `exchange(val)` (1470), which releases whatever a concurrent writer stored meanwhile (1471-1474). `lean_st_ref_set` (1504-1514) stores with a bare `exchange` (1511) |
| Why it is a bug | Lost data, and not linearizable. io.cpp (1446-1456) says the ref API is thread-safe so that a ref "may be used to communicate data between threads", and `IO.CancelToken` relies on exactly that. Upstream's own analysis (leanprover/stref-veil, "F3") calls it a lost update and a linearizability violation |
| Native repro | A task does `r.set 1` once while `main` reads `r`; after `IO.wait` on the task, `main` reads 0 in 841 of 2000 trials on a dedicated thread and 13 of 2000 on the pool (0 of 3000 without concurrent reads). A spin `while !(← flag.get) do pure ()` hangs in about half the runs for this reason. Native output is nondeterministic, so the case expects the correct result |
| Our behaviour | Every ref operation is atomic: a completed `set` is seen by every later `get`; `swap` and `modify` are atomic, as in Lean 4.35 |
| Translators | lean2rr: plan §10, "Runtime"; leanrs: no deviation needed (single-threaded `Rc<RefCell>`) |
| Upstream | Known and fixed: lean4 PR #14775 (merged 2026-08-14), first released in v4.35.0-rc1; not in v4.34.0 or v4.34.1. Related: #14584 / PR #14585 |
| Verdict | lean2rr-side judge, 2026-10-03 |

### LB-02: output followed by a large read on one handle is lost

| Field | Content |
|---|---|
| Summary | After output on a handle, a read of at least one buffer (4096 bytes here) makes glibc discard the pending output; it never reaches the file |
| Where | `src/runtime/io.cpp`: `lean_io_prim_handle_put_str` (671-680) and `lean_io_prim_handle_write` (619-628) are a bare `fwrite`; `lean_io_prim_handle_read` (594-616) is a bare `fread`, with no `fflush` or seek in between. glibc 2.39's `_IO_file_xsgetn` (libio/fileops.c:1331) resets the put area before such a read |
| Why it is a bug | C11 7.21.5.3p7: "output shall not be directly followed by input without an intervening call to the fflush function or to a file positioning function" (undefined behaviour). For write-only streams POSIX requires the read to fail with EBADF, but not the data to be dropped. Lean's documentation describes one cursor that "includes buffered writes", and `flush` writing "any unwritten data". The result depends only on the read size (4095 bytes keep the data) and on the C library (musl, FreeBSD and macOS keep it) |
| Native repro | `write` handle: `putStr "lost"`, `read 5000` fails with EBADF and the file stays `""` after flush, close and exit (correct: `"lost"`). `readWrite` on `"0123456789"`: `putStr "abc"`, `read 5000` returns `"0123456789"` (correct: `"3456789"`, file `"abc3456789"`). `append` and stdout lose the same way. Deterministic. Case: `io/read_after_write` |
| Our behaviour | Before `read n` (n > 0) or `getLine` on a handle whose last operation was output, write the pending bytes, then read from the cursor (or fail with native's EBADF on a write-only handle). `read 0` stays a no-op |
| Translators | lean2rr: plan §10, "Runtime"; leanrs: DV20 (a) |
| Upstream | Not reported (owner: record only) |
| Verdict | lean2rr-side judge, 2026-10-03 |

### LB-03: an error without a file name can crash

| Field | Content |
|---|---|
| Summary | `IO.Process.getCurrentDir` after the working directory was removed crashes with SIGSEGV instead of raising an `IO.Error`: the error decoder dereferences a null file name |
| Where | `src/runtime/process.cpp:318-325` (`decode_io_error(errno, nullptr)` after `getcwd` fails); `src/runtime/io.cpp:276-279` (`UV_ENOENT`: `lean_assert(fname != nullptr); inc_ref(fname)`), and `io.cpp:259-262` (`UV_EINTR`, same shape). The same applies to every caller passing no name: `waitpid`, `kill`, `flock`, `fflush`, `fseek`, `ftruncate`, `fread`, `fwrite`, getline, `fputs`, and `decode_uv_error(ret, nullptr)` in `createTempFile`/`createTempDir` (io.cpp:1266, 1297, 1312, 1343) and at io.cpp:919 |
| Why it is a bug | A crash (status 139, buffered stdout lost) where the type promises an `IO.Error`, and Lean's own `lean_assert(fname != nullptr)` states the intent |
| Native repro | `createDirAll d; setCurrentDir d; removeDir d; getCurrentDir` inside a `try … catch`: segmentation fault, the `catch` never runs. Also `TMPDIR=/nonexistent` then `IO.FS.createTempFile` inside `try … catch`: segmentation fault (found by leanrs's docs review). Cases: `io/error_without_file_name`, `io/temp_file_error` |
| Our behaviour | Raise the error class's `IO.Error` without a file name (`noFileOrDirectory "" 2 "no such file or directory"` here), for every error class |
| Translators | lean2rr: plan §10, "Runtime"; leanrs: DV18 (a) |
| Upstream | Not reported (owner: record only) |
| Verdict | leanrs-side judge, 2026-10-03 |

### LB-13: a pool task enqueued after `main` once no standard worker is left never runs

| Field | Content |
|---|---|
| Summary | After `main` returns, a default-priority (pool) task enqueued once no standard worker exists never runs: it stays `waiting`, its effects are lost (exit 0), and an `IO.wait`/`Task.get` on it from any task hangs the process. This includes tasks created by a dedicated task after `main`, and dependents (`mapTask`/`bindTask`) of a dedicated task that finishes after `main` |
| Where | `src/runtime/object.cpp` (v4.34.0): `spawn_worker` returns at once during shutdown (831-833, commit 380dd9e, 2023); `enqueue_core` either spawns a worker or notifies an idle one (805-806), and with no worker neither does anything; a live worker exits only when the queue is empty and shutdown has begun, so it drains queued work (839-845), skipping the throttle during shutdown (857-858, PR #12052); `handle_finished` re-enqueues dependents through `enqueue_core` (938-950); `wait_for`'s pool-size increase spawns nothing (1036-1043); `~task_manager` joins the standard workers, then waits for the dedicated ones (972-988, #7958); called by `lean_finalize_task_manager` (1129-1134) after `main`. No path runs such a task: its only consumers are standard workers, none exists, and none can be created. 380dd9e's early return protects `m_std_workers` while the destructor joins it (976-977): a memory-safety consequence of joining threads instead of detaching them, with no stated decision to drop tasks |
| Why it is a bug | `Task.get`'s documentation (Init/Core.lean): when a pool task waits, "the maximum threadpool size is temporarily increased by one while waiting so as to ensure the process cannot be deadlocked by threadpool starvation"; a pool task waiting for a late task deadlocks exactly so. `IO.asTask`/`mapTask` (Init/System/IO.lean): "started eagerly … will run even if the last reference to the task is dropped"; the late task never starts and its write is lost. Shutdown intends to finish work: #7958 waits on dedicated tasks "instead of exiting forcefully"; live workers drain the queue; #12094 / PR #12052 fixed a hang of that drain as a bug. The outcome is an accident of thread history: the same late task runs when an unrelated pool task is still busy |
| Native repro | Lean 4.34.0, 100 ms sleeps, deterministic (10 of 10 runs per case, and 3 of 3 for each judge): a dedicated task enqueues a pool task after `main` returned: it never runs (`tasks/late_task_after_main`; the judges' LateWrite: exit 0, its file never written); a dependent of a dedicated task that finishes after `main` never runs (`tasks/late_dependent_of_dedicated`); a dedicated or a pool task that waits for such a task hangs the process after `main returns` (`tasks/late_wait_dedicated`, `tasks/late_wait_pool`). Controls that follow native: a pool task's own late child runs (`tasks/late_pool_child_runs`), so does a dedicated task's late dedicated child (`tasks/late_dedicated_child_runs`), and the program finishes if `main` waits for the dedicated task (`tasks/main_waits_dedicated_child`). Probes: lean-runtime coordination `judge/rs1s-02/` |
| Our behaviour | After `main` returns, Lean's shutdown flag is set (`IO.checkCanceled` is true in tasks, as natively), and the exit waits until no task is queued or running: a pool task enqueued during shutdown, by any task or by a dependency that finishes, gets a worker and runs, and a wait on it returns. Dedicated tasks run to completion, as natively. A task whose dependency never finishes (an unresolved promise, a cycle) is not waited for, as natively. The cases expect this outcome and record native's in `native` |
| Translators | lean2rr: plan §10, "Runtime"; leanrs: no DV (its tasks run at creation, chapter 05 D30, so late tasks already run; it lists LB-13 as a judged bug it does not exhibit) |
| Upstream | Not reported (owner: record only). Related: #7958, #12094 / PR #12052, commit 380dd9e |
| Verdict | lean2rr-side judge, 2026-10-03; leanrs-side judge confirms |

### LB-14: after `takeStdin`, `kill` no longer reaches a `setsid` child's group

| Field | Content |
|---|---|
| Summary | The child `IO.Process.Child.takeStdin` returns has lost its `setsid` flag, so `kill` sends `SIGKILL` to the pid alone, not to the process group |
| Where | `src/runtime/process.cpp`: `lean_io_process_child_take_stdin` (550-556) allocates the new child with `sizeof(pid_t)` scalar bytes (552) and copies only the pid (553); `spawn` allocates `sizeof(pid_t) + 1` (543) with the flag at scalar byte 4 (546, object offset 28); `lean_io_process_child_kill` (392-400) reads that byte (395) and picks `killpg` or `kill` (396). Under `LEAN_MIMALLOC` the 36-byte object rounds up to 40 and `lean_alloc_ctor_memory` zeroes the last word (lean.h 501-518), so the flag reads 0; without mimalloc it is read past a 44-byte block. Introduced by 9901804 (2023), which left `take_stdin` unchanged |
| Why it is a bug | `Child.kill`'s documentation: "If the process was started using `SpawnArgs.setsid`, terminates the entire process group instead", and `takeStdin` returns the same process. The flag is read past the object's scalar area (padding with mimalloc, an out-of-bounds read without). The group survives, and a `readToEnd` on the child's pipe then waits for the survivors |
| Native repro | A `setsid` child with a background grandchild: `kill` kills the group; `takeStdin` then `kill` leaves the grandchild alive (`strace`: `kill(pid)` instead of `kill(-pid)`). Case: `process/take_stdin_setsid` |
| Our behaviour | `takeStdin` keeps the pid and the `setsid` flag, so `kill` uses `killpg` (`process::ChildProcess::take_stdin`) |
| Translators | leanrs mimics native today (`child_take_stdin` sets `setsid: false`) and will keep the flag (its DV15 rows cite LB-14); lean2rr already keeps the flag (`Lower/Process.lean`) |
| Upstream | Not reported (owner: record only); still present on lean4 master |
| Verdict | lean2rr-side judge, 2026-10-04; leanrs-side judge confirms |

### LB-15: a `null` stream leaks a `/dev/null` descriptor into the program

| Field | Content |
|---|---|
| Summary | For each `null` standard stream, the program a child runs has one more open descriptor on `/dev/null`, inherited by its descendants |
| Where | `src/runtime/process.cpp` `spawn`, in the forked child: `open("/dev/null", ...)` then `dup2(fd, n)` (474-477, 482-485, 490-493), without `O_CLOEXEC` and without closing `fd` before `execvp` (510); the pipes of the same function are `pipe2(fds, O_CLOEXEC)` (424) |
| Why it is a bug | Lean's evident intent: PR #2138 (51e77d1, "Fix leaking of file descriptors", for #2137) made every descriptor the runtime opens close-on-exec (`Handle.mk`'s `O_CLOEXEC`, io.cpp 400-404: "do not inherit across process creation"; the pipes); this path was missed. A descriptor-auditing program reports it (lvm: "File descriptor 15 (/dev/null) leaked on lvm invocation"). A resource leak into the child, without data loss; the leaked descriptor takes the lowest number free in the forked child, above the parent's close-on-exec ones, so it shifts no descriptor the program sees first |
| Native repro | The child lists `/proc/$$/fd`: with `stdin := .null` there is an extra `13 -> /dev/null`. Case: `process/null_fd_leak` |
| Our behaviour | `/dev/null` is opened close-on-exec in the parent and `posix_spawn` `dup2`s it, so the program starts with 0-2 and what the parent lets through (`process::Ends::new`) |
| Translators | leanrs is already correct; lean2rr's leanrt `proc.rs` leaks as native today (a fix and tests are coming on the lean2rr side) |
| Upstream | Not reported (owner: record only); still present on lean4 master |
| Verdict | lean2rr-side judge, 2026-10-04; leanrs-side judge confirms |

### LB-16: an over-long temporary directory aborts `createTempFile` and `createTempDir`

| Field | Content |
|---|---|
| Summary | A temporary directory (`TMPDIR` and its fallbacks) of 4083 to 4095 bytes makes `IO.FS.createTempFile` and `createTempDir` abort with `LEAN ASSERTION VIOLATION` (status 134) instead of raising an `IO.Error` |
| Where | `src/runtime/io.cpp` `lean_io_create_tempfile` (1261-1304): `uv_os_tmpdir` into `char path[PATH_MAX]` (libuv 1.48 accepts up to 4095 bytes, `ENOBUFS` from 4096), then `lean_always_assert(PATH_MAX >= base_len + 1 + 1)` (1281) and `lean_always_assert(PATH_MAX >= strlen(path) + file_pattern_size + 1)` (1288); `lean_io_create_tempdir` has the same at 1327 and 1334. `lean_always_assert` throws `lean::unreachable_reached` through `extern "C"` frames, so `std::terminate` raises `SIGABRT` |
| Why it is a bug | A crash where the type promises an `IO.Error` (as LB-03), on input from the environment: every neighbouring length gives one (4082 bytes: the system's `ENAMETOOLONG`; 4096: libuv's `ENOBUFS`). Not a limit: there is no value to compute past the cap, and without the assertion the system answers `ENAMETOOLONG`. Data written to other handles and not flushed is lost (standard output survives: `std::cerr` is tied to `std::cout`) |
| Native repro | `TMPDIR` of N `a`s, the call in `try`/`catch`: 4082, caught `invalid argument (error code: 36, name too long)`; 4083 to 4094, the assertion at 1288 (1334 for a directory), 134; 4095, the assertion at 1281 (1327), 134; 4096, caught `resource exhausted (error code: 105, no buffer space available)`. Cases: `temp/temp_long_dir`, `temp/temp_long_file`, `temp/temp_long_dir_4095`, and the boundaries `temp/temp_long_bounds` (followed as native) |
| Our behaviour | No assertion: the template is built at any length and the system's `ENAMETOOLONG` is the error (`invalid argument (error code: 36, name too long)`, no file name); 4096 bytes or more keep libuv's `ENOBUFS` (`temp.rs`) |
| Translators | leanrs is already correct (DV15 (b)); lean2rr's leanrt `fs.rs` (`temp_template`) has no assertion and is already correct |
| Upstream | Not reported (owner: record only); still present on lean4 master |
| Verdict | lean2rr-side judge, 2026-10-04; leanrs-side judge confirms |

### LB-17: a `null` stream falls back to the parent's stream when `/dev/null` cannot be opened

| Field | Content |
|---|---|
| Summary | When the forked child cannot open `/dev/null` (the parent's descriptors exhausted), a `null` stream is silently the parent's: a `null` standard output writes to the parent's, a `null` standard input reads the parent's input |
| Where | The same lines as LB-15: the result of `open("/dev/null", ...)` is unchecked, `dup2(-1, n)` fails with `EBADF` and is ignored, and `execvp` runs with descriptor `n` still the parent's |
| Why it is a bug | `IO.Process.Stdio.null`'s documentation: "The stream should be empty". Lost data: the child consumes the parent's standard input, and output meant to be discarded appears on the parent's standard output. Every other failure of the spawn's setup is an `IO.Error` |
| Native repro | Under `ulimit -n 64`, with the parent's descriptors exhausted: `stdout := .null` writes to the parent's standard output; `stdin := .null` reads the parent's `line1`, and the parent's own `getLine` then gets `line2`. Without exhaustion: no input, and the parent reads `line1`. Case: `process/null_open_fails` (the program keeps its handles open to the end) |
| Our behaviour | `/dev/null` is opened in the parent before the spawn, and its failure is the spawn's `IO.Error` (`resource exhausted (error code: 24, too many open files)`; `process::Ends::new`). Every pipe is made first and `/dev/null` opened after them, so the parent's pipe ends get native's numbers. One consequence: a `null` stream after a piped one needs one free descriptor more than natively, where the forked child closes the pipe's other end before it opens `/dev/null`: with exactly two descriptors free, `stdout := .piped, stderr := .null` runs natively and fails with `EMFILE` here (case `process/pipe_null_two_free`: native's outcome, and the shared runtime's as its `alt1`, a deviation of its own that `check` accepts beside native's) |
| Translators | leanrs is already correct; lean2rr's leanrt `proc.rs` mimics native today (a fix and tests are coming on the lean2rr side) |
| Upstream | Not reported (owner: record only); still present on lean4 master |
| Verdict | lean2rr-side judge, 2026-10-04; leanrs-side judge confirms |

## Limits

Implementation caps where Lean's definition has a value but the runtime
stops. They are not bugs. The owner's decision (2026-10-03): "if we can lift
the restrictions, then we can lift them". So the shared crate's rules impose
no cap: each implementation computes the definition's result as far as it
can.

| Id | Summary | Where | Translators |
|---|---|---|---|
| LB-04 | `Nat.shiftRight` by 2^32 or more, of an operand with that many bits: `INTERNAL PANIC: Nat.shiftr exponent is too big` | object.cpp:1594-1612 (32-bit shift count) | lifted: both compute the definition's result (leanrs DV17 (a)); `semantics::nat::shiftr`; rows `nat/shiftr.2^4294967296+*.2^32` |
| LB-05 | A `Nat` beyond GMP's limb count ends the process: GMP 6.3.0, the version Lean 4.34.0 links, raises `SIGFPE` with no message (status 136), e.g. for `(2^62)^(2^32 - 1)`, whose power asks for about 2^32 limbs at once (about a second, natively, under a 4G cap). (Older GMPs printed `gmp: overflow in mpz type` and aborted.) | GMP 6.3.0 `_mpz_realloc` (more than `INT_MAX` limbs) calls `__gmp_overflow_in_mpz`, whose `__gmp_exception` raises `SIGFPE` (errno.c); reached through mpz.cpp | both translators end with native's `INTERNAL PANIC: out of memory`, exit 1, instead of `SIGFPE`: every rule whose result size is known before computing it (`Nat` add, succ, mul, pow, shiftLeft; `Int` add, sub, mul, negSucc) refuses a result above the backend's `BigNat::MAX_BITS` at once, before calling the backend (`semantics::nat::check_result_bits`; native's exponent message instead when the exponent or shift is 2^32 or more, LB-11 and LB-12); lean2rr's `MAX_BITS` is about GMP's cap, leanrs's 2^39 bits. Below `MAX_BITS`, an allocation that fails ends as native ends at that site, which depends on the site: a Lean object (`lean_alloc_object`/realloc: arrays, strings, ByteArray, push growth) ends with `INTERNAL PANIC: out of memory`, exit 1; big-number limbs are allocated by GMP's own allocator (Lean's mpz.cpp installs no memory functions), which prints `GNU MP: Cannot allocate memory (size=N)` and aborts (status 134, buffered stdout lost); `getLine`'s `std::string` growth (io.cpp:645-668) ends with `std::bad_alloc`, terminate, 134. leanrs aborts with 134 and no flush on a failed limb allocation (only the message differs: a deviation row); lean2rr keeps GMP's own end where its limbs come from GMP. So the `MAX_BITS` refusal is a lifted limit with its own end (`INTERNAL PANIC: out of memory`, exit 1, the same in both translators), and below it each allocation site ends as natively. Rows `nat/pow.2^62.2^32-1`, `pow.2^64+1.2^32-1`, `pow.2^200.2^32-1`, `powlog2.2^64.2^32-1` (native's `SIGFPE` in `native`) |
| LB-06 | `ByteArray.copySlice` with an offset or length of 2^64 or more: `INTERNAL PANIC: out of memory` | object.cpp:2556-2565 (`lean_nat_to_size_t`) | lifted: both return the definition's clamped copy (leanrs DV17 (c)); `semantics::array::copy_slice`; rows `array/copyslice.*` with an argument of 2^64 or more |
| LB-11 | `Nat.pow` with an exponent of 2^32 or more: `INTERNAL PANIC: Nat.pow exponent is too big`, whatever the base, so also `0 ^ (2^64)` (0) and `1 ^ (2^64)` (1) | object.cpp:1616-1619 (`lean_nat_pow`: `lean_unbox(a2) > UINT_MAX`, or an exponent that is not a scalar) | lifted (owner, 2026-10-03) where the result can be computed: `semantics::nat::pow` gives bases 0 and 1 for any exponent, and other bases while `bit_len(a) * e` is at most the backend's `MAX_BITS`; above it, native's own message (review RS2-03); rows `nat/pow.*` and `nat/powlog2.*` with an exponent of 2^32 or more |
| LB-12 | `Nat.shiftLeft` of a nonzero value by 2^32 or more: `INTERNAL PANIC: Nat.shiftl exponent is too big` | object.cpp:1578-1592 (`lean_nat_shiftl`: the same 32-bit test) | lifted (owner, 2026-10-03) where the result can be computed: `semantics::nat::shiftl` while `bit_len(a) + s` is at most `MAX_BITS`, native's message above it; rows `nat/shiftllog2.*`, `nat/shiftl.*` |

The LB-04, LB-06, LB-11 and LB-12 rows whose result can be computed
expect the definition's result and record native's panic in their `native`
field; the rows whose result cannot be computed end as natively. A result
below `MAX_BITS` that the machine has no memory for fails where the
translator's backend allocates it.

Related upstream: #15193, #15194, #15439, PRs #14286 and #14274.

## Not bugs (followed as native)

### LB-07: no flush of stdout on an aborting internal panic

| Field | Content |
|---|---|
| Summary | With `LEAN_ABORT_ON_PANIC` set, an `INTERNAL PANIC` aborts without flushing stdout; a `panic!` under the same variable keeps C stdout |
| Where | object.cpp: `lean_internal_panic` (92-96), `abort_on_panic` (84-88), `panic_eprintln` (131-138), `lean_panic_impl` (188); stack_overflow.cpp:71-75 |
| Why not | C11 7.22.4.1p2 makes flushing on `abort` implementation-defined, and glibc 2.27 deliberately stopped it ("resulting in deadlocks and further data corruption"). The variable exists to abort: it's used to make panics fatal in builds and CI, and nothing promises that output survives. `panic!` keeps stdout only as a side effect of `std::cerr`'s tie to `cout`; that path still loses file-handle buffers, and a stack overflow loses stdout without the variable. Not flushing is the runtime's uniform rule |
| Behaviour | Native, exactly |
| Verdict | leanrs-side judge: bug; lean2rr-side judge, cross-checking: not a bug (2026-10-03). The cross-check's wider evidence decided it. leanrs may keep DV7 as a leanrs-only deviation |

### LB-08: a thunk forced inside its own closure spins

| Field | Content |
|---|---|
| Where | object.cpp:540-565 (`lean_thunk_get_core` waits with `yield()` for "another thread") |
| Why not | `Thunk.get` of such a thunk diverges by definition, and building one needs `unsafe` |
| Behaviour | Native (spins). leanrs's diagnostic panic (DV18 (b)) is a leanrs-side choice |
| Verdict | leanrs-side judge, 2026-10-03 |

### LB-09: a second `Handle.rewind` serves glibc's buffered bytes

| Field | Content |
|---|---|
| Where | io.cpp:570-577 (`fseek`); glibc 2.39 libio/fileops.c:980-996 (in-buffer seek) |
| Why not | C11 7.21.9.2 and POSIX `fseek` don't require a buffer refresh; Lean's docs promise a cursor, not freshness; the same staleness occurs with no rewind at all |
| Behaviour | Native. The shared io/ follows it (leanrs's DV20 (b) goes away at adoption) |
| Verdict | leanrs-side judge, 2026-10-03 |

### LB-10: closed descriptors 0-2 at start

| Field | Content |
|---|---|
| Where | io.cpp's handle primitives on descriptors 0-2; libuv's loop takes the closed numbers |
| Why not | POSIX deems such an exec environment non-conforming; native fails with a catchable `invalid argument (22)`, without crashing or silently losing data |
| Behaviour | Native in lean2rr and the shared crate. leanrs keeps DV19 for a Rust reason (std opens `/dev/null` first) |
| Verdict | leanrs-side judge, 2026-10-03 |
