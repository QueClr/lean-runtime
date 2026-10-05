# The panic and exit executor (`io::panic`)

`semantics::panic` gives a panic's plan as data. `io::panic` (feature `io`)
carries it out, and does the same for the other ends of a program: the
internal panic, the uncaught error and `IO.Process.exit`. Before this
module, each translator and the crate's test drivers had a copy of this
code (redundancy audit item 3.4). This file records how the copies
differed, what native Lean 4.34.0 does, and what the crate does now.

## What native does

Sources: `src/runtime/object.cpp` 76-191 (`should_abort_on_panic`,
`lean_internal_panic`, `panic_eprintln`, `lean_panic_impl`),
`src/runtime/io.cpp` 62-68 (`lean_io_result_show_error`) and 1606-1608
(`lean_io_exit`), the generated `main` (`LCNF/EmitC.lean` 1119-1150).

| Path | Lines and stream | Stdout | End |
|---|---|---|---|
| Panic, goes on (`lean_panic_fn`) | the message, then `backtrace:` and the frames unless `LEAN_BACKTRACE=0`; one `io_eprintln` (one `putStr` of Lean's current stderr) per line | not flushed | the call returns |
| Panic, `LEAN_ABORT_ON_PANIC` set (any value) or exit-on-panic, or `force_stderr` | the same lines on `std::cerr`, each as `write(line)` then `"\n"` | flushed first (`std::cerr` is tied to `std::cout`) | `abort()` (134), `exit(1)`, or returns (`force_stderr` only) |
| Internal panic | `INTERNAL PANIC: <msg>\n` with `fprintf` on C's `stderr` (one `write` under the `FILE` lock, recursive per thread: the line waits for another thread's write in progress; `%s` stops at a NUL byte) | not flushed | `abort()` (134, stdout lost: LB-07) under `LEAN_ABORT_ON_PANIC`, else `exit(1)` (stdout written after the line) |
| Uncaught error | after `lean_finalize_task_manager`: `uncaught exception: <msg up to NUL>\n` on `std::cerr` (three writes) | flushed first | status 1 |
| `IO.Process.exit c` | none | written by `exit` | status `c` |

Both variables are read with `getenv` at every panic, never cached. The
generated `main` of 4.34.0 leaves panic messages on during module
initialization (only the LLVM backend turned them off).

**Native probe** (`Order.lean`, run with `LEAN_BACKTRACE=0` and stdout and
stderr in one pipe; the program first prints `pending;` without a newline,
so it stays in stdout's buffer):

| Case | Output | Status |
|---|---|---|
| `panic!`-style panic (`a[k]!` out of bounds) | `Error: index out of bounds\npending;after 0\n` | 0 |
| the same, `LEAN_ABORT_ON_PANIC=1` | `pending;Error: index out of bounds\n` | 134 |
| internal panic (`2 ^ (2^33 + k)`) | `INTERNAL PANIC: Nat.pow exponent is too big\npending;` | 1 |
| the same, `LEAN_ABORT_ON_PANIC=1` | `INTERNAL PANIC: Nat.pow exponent is too big\n` | 134 |
| `throw (IO.userError "bad")` from `main`, with or without `LEAN_ABORT_ON_PANIC` | `pending;uncaught exception: bad\n` | 1 |
| `IO.Process.exit 3`, with or without `LEAN_ABORT_ON_PANIC` | `pending;` | 3 |
| a panic with Lean's stderr set to a stream that brackets each `putStr`, backtraces on | `[Error: index out of bounds\n][backtrace:\n][<frame>\n]`... (one `putStr` per line) | 0 |
| a panic during initialization (a closed term), `LEAN_BACKTRACE=0` | `Error: index out of bounds\n` (printed during initialization) | (see below) |

`tests/io_panic.rs` checks the same orders on the crate, and the internal
panic's wait for another thread's stderr write in progress (review RSH3-01:
natively the line comes after the other write's 200000 bytes, also under
`LEAN_ABORT_ON_PANIC`). The cases `tasks/promise_in_initialize` and
`_abort` record native's internal panic of `IO.Promise.new` in an
`initialize` declaration, with stdout pending.

## The API

- `io::panic::report(msg, force_stderr, glue)`: `lean_panic_impl`, by
  `lean_panic_plan(glue.settings(), force_stderr)`: an effect point, the
  lines, then the abort, the exit or the return.
- `io::panic::internal_panic(msg, glue) -> !`: the line built on the stack
  (an `InternalLine`, handed to the glue whole), no effect point, no
  allocation; then the abort or `exit(1)`.
- `io::panic::uncaught(msg, glue) -> !`: `io::exit::after_main`, the line,
  status 1.
- `io::panic::process_exit(code, glue) -> !`: an effect point, then the
  exit.
- `io::panic::settings()` and `io::panic::abort_on_panic()`: the
  environment, read now.
- `io::panic::write_internal_line_locked(line)`: the line's pieces to
  descriptor 2 under the lock of the crate's `stderr` model (see row 10).
- `io::panic::write_stderr_fd(bytes)`: a write to descriptor 2 with no lock
  and no allocation.

A glue implements `io::panic::PanicGlue`. Every method has native's
behaviour on the crate's own streams as its default, so `io::panic::Native`
implements none:

| Method | Default | What a translator may keep |
|---|---|---|
| `lean_eprintln(line)` | `io::debug::runtime_eprintln` (the crate's current streams) | its own Lean stderr stream (lean2rr's stream cells) |
| `settings()` | `settings()` | its own flags (leanrs's cached abort flag, no backtrace) |
| `panic_effect(plan)` | `sched::effect` | its own effect points (lean2rr's panic of `panicCore` makes one only on the process's stderr) |
| `abort_on_panic()` | `abort_on_panic()` | the same, for the internal panic |
| `flush_stdout()` | the crate's `stdout` model | a test capture's release (leanrs) |
| `process_stderr(bytes)` | the crate's `stderr` model | another writer to descriptor 2 (leanrs's `stderr()`, its test capture) |
| `internal_stderr(line)` | `write_internal_line_locked` | another writer (leanrs's `stderr()`, its test capture; lean2rr's old path, the crate's `stderr` model per piece); it must not allocate |
| `abort()` | `std::process::abort` | a test capture's release first |
| `exit(code)` | `io::exit::exit` | the same |

## How the copies differed, and the resolution

"l2r" is lean2rr's leanrt (`lib.rs` 119-270, `prelude.rr` 90-200), "rs" is
leanrs_rt (`panic.rs` 43-178, `io.rs` 153-230, `io/env.rs` 77-105), "drv"
is the crate's own copies (the test drivers' `glue_common.rs`, `lnet.rs`
and `Promise::new`, `tests/io2_cases.rs`, `tests/threads_twins.rs`,
`io::process`'s output drain, `io::startup`'s line). The knob is the
`PanicGlue` method that keeps a translator's behaviour where it differs from
the default.

| # | What | Native | l2r | rs | drv | Crate now | Knob |
|---|---|---|---|---|---|---|---|
| 1 | When the variables are read | `getenv` at every panic | every panic | `LEAN_ABORT_ON_PANIC` cached at the first panic (`OnceLock`), plus `set_abort_on_panic` | every panic | every panic (`settings`, `abort_on_panic`) | `settings`, `abort_on_panic` |
| 2 | Backtrace | `backtrace:` and the frames unless `LEAN_BACKTRACE=0` (never `(stack trace unavailable)`: that `#else` branch of `print_backtrace` is dead, since `backtrace:` and the call are both under `#if LEAN_SUPPORTS_BACKTRACE`, object.cpp 179-185) | `backtrace:` and `(stack trace unavailable)`, its stand-in for the frames | never | as l2r | as l2r (native's frames change from run to run) | `settings` (`backtrace: false`) |
| 3 | `putStr` calls on Lean's stream | one per line | one for all lines | one (a single line) | one per line | one per line | `lean_eprintln` (a glue may collect the lines and write them once) |
| 4 | Effect point before a panic's lines | none (threads run meanwhile) | `panicCore`: on the process's stderr only (on Lean's stream, the default stream's `putStr` makes one, a stream of `IO.setStderr` none); the runtime's panics: when printing | when printing | when printing | when printing or ending | `panic_effect` |
| 5 | Stdout flush before the process's stderr | before each write (the tie) | once | once (`before_end`) | once | once (stdout is empty after it) | `flush_stdout` |
| 6 | Writes to the process's stderr | `line`, then `"\n"`, per line | one write for all lines | `line\n` in one write | `line`, then `"\n"` | `line`, then `"\n"` | `process_stderr` |
| 7 | The writer of the process's stderr | C's `stderr` | the crate's model | std's `stderr` (or the test capture) | the crate's model | the crate's model | `process_stderr` |
| 8 | Flush with messages off and an abort | none | flushed (unreachable: messages are always on) | none | none | none | none |
| 9 | Internal panic: building the line | `fprintf` (no allocation), `%s` stops at a NUL byte | a `Vec`; whole message | 256-byte stack line, a `String` beyond; whole message | `String`s; `io::startup` cut at 256 bytes | stack pieces of at most 256 bytes, each ending on a character boundary (LS3-01), never cut, no allocation; stops at a NUL byte | none |
| 10 | Internal panic: the writer | C's `stderr`, one `write` under its `FILE` lock (waits for another thread's write in progress; recursive, so the same thread gets in) | the crate's model (its lock; its first use allocates its 1-byte buffer) | std's `stderr` (its own lock; or the test capture) | the crate's model; `eprintln!` in `Promise::new` | descriptor 2 directly under the crate's `stderr` lock (RSH3-01, LS3-03); without it when this thread holds it, and with `sched` once the program has a task, a promise, a timer or a watch (the cooperative lock allocates and may switch): there the line may land inside another context's or thread's stderr write in progress, a deviation | `internal_stderr` |
| 11 | Internal panic: the abort | `LEAN_ABORT_ON_PANIC`, read after the line | read after the line (both variables) | cached flag; `before_abort` then `abort` | `lnet.rs`, `Promise::new`: never; the others as l2r | read after the line (that variable only) | `abort_on_panic`, `abort` |
| 12 | Internal panic: the exit | `exit(1)`, stdout written (the recursive lock lets the flush take a `FILE` this thread holds) | the crate's `exit` | the crate's `exit` | the drivers' `Promise::new`: `std::process::exit(1)`, stdout lost | the crate's `exit`; its flush skips `stderr` when this thread holds its lock, where a relock would wait for good (as the cooperative locks' rule, RFX1-17 (c)) | `exit` |
| 13 | Internal panic: effect point | none | none | none | `lnet.rs`: one | none (no other code runs; out-of-memory end) | none |
| 14 | Uncaught error | task manager finalized, flush, three writes, status 1 | `after_main`, `show_error`, `exit(1)` | the same (test capture released first) | the same; `threads_twins` without `after_main`; the net ports before `sched::finish` (RSH3-04) | the same, through the glue (the drivers report a port's error after `sched::finish`) | `flush_stdout`, `process_stderr`, `exit` |
| 15 | `IO.Process.exit` | `exit(code)` | effect point, the crate's `exit` | the same (test capture released) | the same | the same | `exit` |
| 16 | During initialization | messages on | on | on | on | on (`settings`) | `settings` |
| 17 | Statuses | 134, 1, `code` | the same | the same | the same | the same | none |

leanrs's deferred message differences (the shared-runtime coordinators'
list; the judge's verdict on the audit's divergence 3): the
cached `LEAN_ABORT_ON_PANIC` is rows 1 and 11 (knobs `settings` and
`abort_on_panic`), the missing `backtrace:` is row 2 (knob `settings`).
With its own glue for these and for rows 5, 7, 10 and 11's test-capture
release, leanrs adopts the executor without a change of output; its
`stderr()` (std's) has a lock of its own, so its internal panic's line
waits for its own writes in progress, but not for the crate's.

## Open points

- **Row 3 (lean2rr).** With backtraces on, native makes one `putStr` per
  line, lean2rr one for all of them. Only a stream of `IO.setStderr` whose
  `putStr` is not plain concatenation sees it. lean2rr can keep its single
  write with its glue (its adoption note), or take native's.
- **Row 4 (lean2rr).** With a stream of `IO.setStderr`, lean2rr's
  `panicCore` makes no effect point before its lines; the executor's
  default makes one, as leanrs and the drivers do. This changes only the
  order of contexts, not the text. lean2rr keeps its points with
  `panic_effect` until it decides.
- **`abort_on_panic` allocates** std's copy of the variable's value when it
  is set to a non-empty value (`std::env::var_os`; safe Rust has no other
  read of the environment). It runs after the line, when the process is
  about to abort. If that allocation fails, Rust's allocation-error abort
  prints its own line, with the same status 134.
- **`exit(1)` after an internal panic** is the crate's `exit_flush`, which
  collects the open files into a `Vec` (an allocation when files are open);
  glibc's `_IO_flush_all` allocates nothing. This is so in every copy, and
  it is not changed here.
- **A panic during initialization** prints as during `main`. In the native
  probe (`Init.lean`: a closed term of type `Nat` that panics, then `main`
  prints a `Nat`), the program then ends with SIGSEGV (139) in
  `_init_l_Nat_reprFast___closed__0`; with a `String` closed term it does
  not. This is a suspected native bug, for a judge. It is not part of the
  executor.
- `IO.Process.forceExit` is not part of this module: lean2rr writes the
  handed-off streams and calls `_exit`, leanrs makes an effect point and
  calls `io::exit::force_exit`.
