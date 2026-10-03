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
- a case in `tests/cases/` whose `expected` is native's output (or, when
  native is nondeterministic, the correct output), with `deviations` naming
  the entry.

Each translator also lists it among its intended differences. Reporting a bug
to Lean upstream is the owner's decision; the entry records the status.

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
| Upstream | Not reported |
| Verdict | lean2rr-side judge, 2026-10-03 |

### LB-03: an error without a file name can crash

| Field | Content |
|---|---|
| Summary | `IO.Process.getCurrentDir` after the working directory was removed crashes with SIGSEGV instead of raising an `IO.Error`: the error decoder dereferences a null file name |
| Where | `src/runtime/process.cpp:318-325` (`decode_io_error(errno, nullptr)` after `getcwd` fails); `src/runtime/io.cpp:276-279` (`UV_ENOENT`: `lean_assert(fname != nullptr); inc_ref(fname)`), and `io.cpp:259-262` (`UV_EINTR`, same shape). The same applies to every caller passing no name: `waitpid`, `kill`, `flock`, `fflush`, `fseek`, `ftruncate`, `fread`, `fwrite`, getline, `fputs` |
| Why it is a bug | A crash (status 139, buffered stdout lost) where the type promises an `IO.Error`, and Lean's own `lean_assert(fname != nullptr)` states the intent |
| Native repro | `createDirAll d; setCurrentDir d; removeDir d; getCurrentDir` inside a `try … catch`: segmentation fault, the `catch` never runs. Case: `io/error_without_file_name` |
| Our behaviour | Raise the error class's `IO.Error` without a file name (`noFileOrDirectory "" 2 "no such file or directory"` here), for every error class |
| Translators | lean2rr: plan §10, "Runtime"; leanrs: DV18 (a) |
| Upstream | Not reported |
| Verdict | leanrs-side judge, 2026-10-03 |

## Limits

Implementation caps where Lean's definition has a value but the runtime
stops. They are not bugs. Whether a translator may lift them (leanrs
does; lean2rr follows native) is an open question for the owner.

| Id | Summary | Where | Translators |
|---|---|---|---|
| LB-04 | `Nat.shiftRight` by 2^32 or more, of an operand with that many bits: `INTERNAL PANIC: Nat.shiftr exponent is too big` | object.cpp:1594-1612 (32-bit shift count) | leanrs DV17 (a): computes; lean2rr: native |
| LB-05 | A `Nat` beyond GMP's limb count aborts (`gmp: overflow in mpz type`) | GMP `_mpz_realloc` (INT_MAX limbs), via mpz.cpp | leanrs DV17 (b); lean2rr: native |
| LB-06 | `ByteArray.copySlice` with an offset or length of 2^64 or more: `INTERNAL PANIC: out of memory` | object.cpp:2556-2565 (`lean_nat_to_size_t`) | leanrs DV17 (c): returns the definition's clamped copy; lean2rr: native |

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
