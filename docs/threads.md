# Real threads for `sched` (threads mode)

Status (2026-10-04): T1, T2 and T3 are done. T1 is `sched::mt`, behind
the cargo feature `threads` (`src/sched/threads.rs`, `src/sched/mt/`). T2 is
io and `Std.Internal.UV` in threads mode (0.5). T3 is the second driver,
`tests/sched-driver-mt`, which runs the task, sync, refs and taskio cases in
threads mode, 5 times each, with the single-thread driver's ports; the new
cases recorded natively; and the site's page for threads mode (0.6). Later
items (section 5): `net` in threads mode (N), the translators' threads modes
(leanrs's L2, lean2rr's E2), and the measurement step P, which needs the
owner's permission. The design below was written at main 1c36a58; section 0
says where main e95ca47, T1, T2 and T3 changed it. The owner's direction
(2026-10-03): "parallelism will be supported, not the focus now".

The owner's decisions (2026-10-04):
- lean2rr stays single-threaded for now: "reussir no change yet. lean2rr
  can just not support it if it cant for now";
- **the model is native's worker pool** (1.2), confirmed ("ok, then worker
  pool is fine. go ahead"): `LEAN_NUM_THREADS` OS workers; a thread per
  dedicated task; one more worker while a pool task waits (`wait_for`), and
  `IO.waitAny` keeps its worker, as natively; one lock over the task table;
  no coroutines and no `Glue::suspend` in this mode;
- **everything atomic in threads mode** (2.1), confirmed. Type-directed
  coloring comes later, only after measurement.

leanrs reviewed this design (2026-10-04). Its answers to the open questions
are folded in (section 6). Its seven constraints for T1 (2026-10-04, from
what its adoption of `sched` uses) are answered point by point in 0.3.

This file is the implementors' reference. It covers:
0. what changed since the design, T1's, T2's and T3's choices, and
   leanrs's constraints;
1. the model, and how each rule of `sched` carries over;
2. the contract a translator meets so that values can cross threads;
3. the crate's shared state that needs locks or atomics;
4. what stays deterministic, and how the tests run both modes;
5. the order of the work;
6. risks, and open questions for the owner.

**Sources.** Native: Lean 4.34.0 (tag `v4.34.0`), files under
`src/runtime/` unless named otherwise, with v4.34.0's line numbers.
lean2rr: `runtime/leanrt/src/` and `docs/translation-plan.md`, 2026-10-04.
Reussir: `docs/design/thread-safety.md` and
`crates/reussir-codegen/src/lower/mod.rs` of lean2rr's checkout. leanrs:
`rt/leanrs_rt/src/`, snapshot e3ff2ad2 (2026-10-03). This crate: main at
1c36a58 (sched-1, io-2, semantics-3, cleanup-1) for sections 1 to 6;
section 0 at e95ca47, T1 (main 2df0ab5) and T2. libuv: 1.48.0, the version
Lean 4.34.0 links (`src/unix/core.c` `uv_run`, `src/timer.c`
`uv__run_timers`).

## 0. Since the design: main e95ca47, T1, T2 and T3

### 0.1 What main gained, and what it means for the pool

Since 1c36a58 main gained sched-io (cooperative IO, the event loop, UV
timers and signals), sched-2 (native's wake order, LB-32), sched-3 (what a
waiter or a poller runs on its own stack, AR-9 and AR-10; the stack-overflow
report, AR-11), sched-4 (the workers a context holds, AR-15 and AR-16;
LSCHED-01), net-1, net-2, fixes-1 and the io batches (AR-1 to AR-20).

**Carried over to the pool.** Native's task manager does these itself, so
`sched::mt` ports native's code and needs no emulation:
- sched-2's wake order: one condition variable, notified at the end of
  each walk of a referenced task (`resolve_core`, `object.cpp` 927-935);
  `IO.waitAny` looks again at each notification; a task released before it
  finished notifies nobody (review RS2-08).
- LB-32 stays fixed: `option_get_or_block` wakes every waiter before its
  thread blocks for good. Waiters of other finished tasks wake too, which a
  spurious wake-up of native's condition variable also gives.
- AR-10 (ii): `IO.waitAny` keeps its worker. AR-15: `wait` in a pool task
  raises the pool's limit by one, also a wait that never ends. AR-16: a
  worker stays busy through its task's walk, whose `sync` dependents run on
  it, and a `sync` task's wait raises nothing (`wait_for`'s `in_pool`).
- LB-13 stays fixed (`finish`; `spawn_worker` also during the shutdown).
- AR-11: each thread the manager makes installs Lean's stack-overflow
  report at its entry. Its record holds the thread's own guard; there is no
  context guard.
- fixes-1's exit order: `finish`, then `io::exit::after_main` (AR-6), then
  the glue's flush and exit. `IO.Process.exit` and an internal panic join
  nothing (LB-29). `exit_lock`'s rule for a holder on another thread (a
  plain wait) applies.

**Gone in threads mode.** These emulate threads on one thread:
- sched-io's cooperative IO: the reactor, `poll_fds`, `wait_fd`, the
  stream locks parked at a switch, `io::coop`. io's cooperative code is
  `cfg(feature = "sched")`, so with `threads` io compiles its plain path: a
  blocking call blocks its own thread, as natively.
- fixes-1's writer hand-offs (AR-8): a dropped stream's close blocks the
  dropping thread, as natively, so no writer thread exists. "A context's
  own writers" become "a thread's own", and a thread has none.
  `before_task_value` and `before_publish` stay callable, as no-ops.
- sched-3's runs on a waiter's or poller's stack (AR-9, AR-10) and the
  polling threshold: a waiter blocks, a worker runs the task, and
  `IO.getTaskState` gives native's answer at once.
- sched-4's `holds_worker` bookkeeping, and LSCHED-01 with the pure-task
  rule: a runaway pure task takes its worker, as natively, so
  `tasks/runaway_pure_task_before_io` gives native's outcome, not its
  `alt1`. The same holds for fixes-3's started pure tasks that keep their
  workers (AR-25), LSCHED-02 (`tasks/runaway_pure_before_awaited`) and
  LSCHED-03 (`tasks/picked_task_sleeping_worker`, where real workers race
  as natively). T3's driver checks it: it accepts no alternative of an
  LSCHED case, and all four give native's outcome in 5 of 5 runs (0.6).
- The yield points (`effect`, `poll`, `ref_read`, `set_ref_read_yields`),
  `STALE`, `LATENCY_*`, `POLL_QUERIES` and `EARLY`: no-ops, or nothing.
- The no-suspend scope stays callable (leanrs's point 1): a depth per
  thread, which changes no behaviour, since nothing suspends.

**New in threads mode:**
- `release`, `resolve` and `cancel` work from any thread. Ids are 64-bit
  serials, never reused within a run, so a finished task's id stays
  harmless.
- Two threads that resolve one promise: the first claims it and stores;
  the second waits until the value is stored, then returns false (natively
  it waits for the lock, which the first holds until `m_value` is set,
  `resolve`, 995-1007).
- After `finish` there is no task manager, as natively:
  `lean_finalize_task_manager` sets `g_task_manager` to null (`object.cpp`
  1129-1133), so later tasks run at once there too (`lean_task_spawn_core`,
  `lean_task_map_core`). The deviation is only where native dereferences
  the null manager: `resolve` (`lean_promise_resolve`, 1333), `Task.get` or
  `IO.wait` of an unfinished task (`lean_task_get`, 1226), `IO.cancel`,
  `IO.getTaskState` and `IO.waitAny` of one (1293-1308), and the drop of an
  unfinished task (`deactivate_task`, 1152-1160). There `sched::mt` gives
  safe answers: a promise resolved then (a translator's value dropped at
  the exit) runs its dependents at once on the resolving thread, a bind
  task's `Continue` then runs at once, and no thread is made (review
  RT1-02).
- A Rust panic in a job, a glue hook, or the destructor of a value the
  crate drops on a thread it made (a task's continuation, the glue)
  aborts the process, on any thread (1.4; review RT1-01).
- A thread the manager cannot make: native's `failed to create thread:
  <strerror>`, then an abort (`thread.cpp` 135-140;
  `sched::thread_create_failed`, shared with the single-thread scheduler).
- `sched::Ref`: the 4.35 rule of 3.1 as a lock and a condition variable.

**A native data race the port avoids.** `task_bind_fn1` stores a bind
task's continuation without the task manager's lock (`imp->m_closure =
c`, `object.cpp` 1268), while `deactivate_task_core` reads and clears the
same field under the lock (813, 815), and `get_task_state` reads it
(1085-1097). Its effects are a leaked continuation (a queued inner pure
task then runs to completion) and a `waiting` or `running` answer that
can flip mid-transition; both are within native's schedules, so it is no
LB (no wrong output). `sched::mt` returns the continuation in the job's
result (`Outcome::Continue`) and stores it, or drops it for a released
task, under the lock (both reviews of T1, 2026-10-04).

**Since T2** (0.5): `sched::uv` (`Std.Internal.UV`'s loop, timers and
signals) runs on a loop thread of its own, as natively; the working
directory's lock rule and the routing of signals of 3.2 are in. `net`
stays on the single-thread event loop: `net` turns `sched` on, so `net`
with `threads` is a compile error. Networking in threads mode is a later
item (section 5; networking is not a target now, owner 2026-10-04).

### 0.2 Features (T1, T2)

| Features | Since T2 | Why |
|---|---|---|
| `threads` | Allowed. It depends on rustix and signal-hook, as `sched` does | `sched::uv`'s loop thread waits with `poll(2)` on an eventfd and the signal pipe (rustix), and the signal watchers use signal-hook's safe API (T2). Both crates are the crate's already, approved for `sched`; a plain `rustc` build of `threads` no longer works (cargo, offline, as `io` and `sched`) |
| `threads` with `sched` | Compile error | A build has one scheduler (2.5) |
| `threads` with `net` | Compile error | `net` turns `sched` on; the network needs the event loop. Networking in threads mode is a later item (section 5) |
| `threads` with `io` | Allowed | io takes its plain path by the existing `cfg(feature = "sched")`, as without `sched`. Where `unshare(CLONE_FS)` is refused (Docker's default seccomp profile), a spawn with a `cwd` moves the whole process's working directory meanwhile (`fallback_spawn`, under `CWD_LOCK`); since T2 every lookup of a path the program supplies then holds `CWD_LOCK` shared, never across a wait, so none resolves against the child's `cwd` (3.2; reviews RT1-04, RT2-03) |
| `threads` with `stack-overflow` | Allowed | `stack-overflow` no longer turns `sched` on; alone it is a compile error. Every worker, dedicated thread and the loop thread install the report at their entry |
| `threads` with `proc-title`, `unsafe-fast` | Allowed | Nothing shared with the scheduler |

A build without `threads` is unchanged: the same items and bounds, no
`Arc`, no new lock (`src/sched/mod.rs` and its files compile as before;
only plain items moved: `TaskState`, the messages and the priorities to
`src/sched/common.rs` in T1, and the process-wide part of the signal
watchers' delivery to `src/sched/uv_signals.rs` in T2, unchanged). `check.sh`
builds and tests `threads` and every feature that may go with it; with
`io,threads,proc-title,stack-overflow,unsafe-fast` the io twins run inside
tasks and the uvloop twins over threads mode's `sched::uv` (0.5).

### 0.3 leanrs's constraints for T1

1. **The same glue entry points.** `sched` re-exports `sched::mt` under
   the single-thread scheduler's names: `start`, `start_with`, `finish`,
   `spawn`, `depend`, `dependent_runs_now`, `wait`, `wait_any`, `state`,
   `is_finished`, `cancel`, `check_canceled`, `release` (for every task, IO
   tasks included), `in_sync_task`, `promise_new`, `resolve`,
   `option_get_or_block`, `hang`, `before_task_value`, `before_publish`,
   `effect`, `poll`, `ref_read`, `set_ref_read_yields`, `sleep_ms`,
   `enter_no_suspend`, `leave_no_suspend`, `no_suspend`, `in_no_suspend`,
   `io_cooperative`, `Job`, `Outcome`, `TaskId`, `TaskState`, `sync::*`.
   `io::exit::after_main` is io's, unchanged. leanrs's DrainScope stays
   correct on a worker: the crate drops no translator value under its lock,
   and `resolve` runs a promise's walk on whatever thread calls it, after
   the drop walk. Met in T2: `sched::uv` with `LoopPromise` (`is_resolved`,
   `resolve`), `Timer`, `Signal`, `loop_configure` and `loop_alive`, the
   single-thread module's names and shapes, with `LoopPromise: Clone + Send
   + 'static` (0.5).
2. **`Send` in one place.** `sched::Job` is `Box<dyn FnOnce() -> Outcome +
   Send>` in threads mode (`'static` is implied), and has no `Send` in the
   single-thread build.
3. **The 4.35 rule** for refs: `sched::Ref` (`src/sched/mt/refs.rs`), a
   lock and a condition variable, with `get`, `take`, `put`, `set`, `swap`,
   `modify` and `modify_get`.
4. **Stack overflow.** Every worker and dedicated thread calls
   `install_stack_overflow_handler()` at its entry, after std's start.
5. **Thunks and lazy constants** block the OS thread with std's
   `OnceLock`: no crate API.
6. **The exit order** of D34 (c): `finish` joins the IO tasks and the
   started or referenced pure tasks, then `io::exit::after_main`, then the
   glue flushes (stdout first) and exits. There are no writer hand-offs to
   join. `IO.Process.exit` and panics join nothing (LB-29).
7. **Process-wide and per-thread state:** the table in 0.4.

### 0.4 State in threads mode

| State | Where | In threads mode |
|---|---|---|
| Task table, queues, worker counts | `sched::mt`, `State` | Process-wide, under one lock |
| The tasks running on a thread (`check_canceled`, `in_sync_task`, `wait`'s pool rule) | `sched::mt`, `CURRENT` | Per thread |
| Thread number, no-suspend depth | `sched::mt` | Per thread |
| Current standard streams | `io/streams.rs`, `CURRENT` | Per thread, as natively: a pool worker keeps them from one task to the next; a new worker, a dedicated task's thread and `main` start with the process's (0.5 item 2) |
| errno model | `io/error.rs`, `ERRNO` | Per thread, as C's `errno`: a pool worker keeps it from one task to the next (0.5 item 2) |
| Standard streams' `FILE`s, open-handle registry | `io/handle.rs`, `STDIN` & co., `OPEN` | Process-wide, a lock each |
| Writer hand-offs of dropped streams | `io/coop.rs` | None: `io::coop` is `sched`'s |
| `IO.Process.output`'s drains | `io/process.rs`, `DRAINS` | Process-wide; `finish` joins them (`after_main`) |
| Working directory | The process's; `io/process.rs`, `CWD_LOCK`, `NO_FALLBACK` | Process-wide, as natively. Changes (`setCurrentDir`, `uv_chdir`) and reads (`currentDir`, `uv_cwd`) take `CWD_LOCK`; since T2 the lookups of the paths the program supplies take it shared while a fallback spawn may happen, that is where `unshare(CLONE_FS)` is refused (3.2, RT1-04), absolute ones too, never across a wait (RT2-03) |
| `environ` copy | `io/environ.rs`, `ENVIRON` | Process-wide, under a lock; the C environment changes through `std::env::set_var` (3.2) |
| Spawner thread | `io/process.rs`, `SPAWNER`, `NO_PRIVATE_CWD` | Process-wide: one long-lived thread for the spawns with a `cwd`, which queue on it. Since T2 `sched::start` starts it, so the spawn path is decided before any task runs (3.2) |
| `Std.Internal.UV`'s loop | `sched/mt/uv.rs`, `LOOP` | Process-wide, as native's `global_ev`: one loop thread (made at the first use), the loop lock, the armed timers and the listening signal watchers (T2) |
| Signal handlers, the signal pipe, the watchers' counts | `sched/uv_signals.rs`, `HOOKED`, `PIPE` | Process-wide, in both modes (T2 moved them out of `sched/uv.rs`) |
| Stack-overflow records | `sched/stack_overflow.rs` | One per registered thread |
| Alternate signal stacks the crate makes | `sched/stack_overflow.rs`, `FREE_ALTSTACKS` | One per live registered thread that std gave none; given back to a process-wide free list when the thread ends, and reused (RT1-03) |
| `Std.Sync` objects, `Ref`s | The translator's handles | Shared, a lock each |

### 0.5 T2: io and `Std.Internal.UV` in threads mode

T2 (branch threads-2, 2026-10-04; its review round RT2, both reviews, the
same day) is four items. A build without `threads` takes no new lock and
makes the same system calls; the single-thread scheduler changed in one
rule only, the threads' state of item 2, which both modes follow.

**1. The working directory** (review RT1-04; 3.2). `src/io/process.rs`,
module comment item 4, "Threads mode".
- `sched::start` (`start_with`, with `io`) decides the spawn path before
  any task runs: it starts the spawner thread (`decide_spawn_path`). So the
  spawner's file-system attributes, a copy taken at its `unshare` (its
  `umask` and root directory; module comment, item 4), are the process's
  at `sched::start`, not at the first spawn with a `cwd`. Lean has no
  `umask`, so only foreign code could tell.
- If the spawner unshares its file-system attributes (the common case), it
  sets `NO_FALLBACK`, under `CWD_LOCK` held exclusively, so no fallback
  spawn is in progress then. From then on no spawn takes the fallback, and
  lookups take no lock.
- Otherwise (`unshare(CLONE_FS)` refused, as under Docker's default seccomp
  profile) every lookup of a path the program supplies holds `CWD_LOCK`
  shared for the rest of the run: `io::process::with_path_lookup` and
  `open_looked_up`, one uncontended read lock per call. A fallback spawn
  holds `CWD_LOCK` exclusively while the process is in the child's `cwd`,
  so it excludes them, as it already excluded `IO.currentDir` and
  `uv_cwd`. The paths the crate names itself (`/proc/self/exe`,
  `/dev/urandom`, `/dev/null`, ...) take no lock: none goes through the
  working directory (review RT2-L-03).
- **No wait under the lock** (review RT2-03). A fallback spawn waits for
  every reader, and while it waits std's lock lets no new reader in. An
  `open(2)` of a FIFO waits for its other end, which may be that spawn's
  child: a deadlock, and every later lookup blocked behind it, where
  natively all go on. So `Handle.mk`'s open (`io::process::open_looked_up`)
  never waits under the lock:
  - a creating open (`write`, `writeNew`, `append`) is the one open under
    the lock with `O_NONBLOCK` added, which is then cleared (`O_APPEND`
    kept). So every check of a creating open runs, the kernel's
    `may_create_in_sticky` included (`fs.protected_regular`,
    `fs.protected_fifos`: another user's file or FIFO in a sticky directory
    such as `/tmp` is `EACCES`, as natively; review RT2-11, which found the
    first fix's reopen without `O_CREAT` skipping it), and a missing name
    is created as the one open creates it. Only when it would wait
    (`ENXIO`: a FIFO with no reader; `EWOULDBLOCK`: a lease) is the file
    opened as below: those errors come in `vfs_open`, after the lookup and
    the sticky check and before any truncation;
  - any other open resolves the path under the lock, `open(path, O_PATH |
    O_CLOEXEC)`, which never opens the file itself, and opens the file
    after the unlock through the descriptor, `open("/proc/self/fd/N",
    flags)`: the same file and the same checks of its type and
    permissions. The `O_PATH` descriptor then goes and the file's takes the
    lowest free number (`F_DUPFD_CLOEXEC` from 0), as the one open's would
    (review RT2-12);
  - without `/proc`, or when the second descriptor is not to be had
    (`EMFILE`, `ENFILE`, where the one open needs only one; RT2-12), it is
    the one open under the lock, as before T2's fix.

  Every other holder of the lock waits for nothing:
  the other lookups (`stat`, `opendir`, `mkdir`, `rename`, ...), `getcwd`,
  and `posix_spawn`, which returns once the child has called `execve`; so
  the spawner's wait for the write lock (`NO_FALLBACK`) ends too.
- The operations: `Handle.mk` (`open`), `createDir`, `removeDir`,
  `removeFile`, `rename`, `hardLink`, `setAccessRights`, `realPath` (the
  resolution, the working directory's length and the walk under one lock),
  `readDir` (the `opendir`; `readdir` reads the descriptor), `metadata`,
  `symlinkMetadata`, the temporary file's and directory's creation (a
  relative `TMPDIR`), the `open(".")` of a spawn with a relative `cwd`, and
  the stand-in of a child that cannot start (a spawn without a `cwd`).
  Spawns without a `cwd` already held the lock shared (RIO2-13).
- **Absolute paths take the lock too.** Skipping it is not safe. The kernel
  resolves an absolute path from the root directory, which `chdir` does not
  change, but the path can still reach the working directory: through
  `/proc/self/cwd`, a magic link to the process's working directory (also
  `/proc/thread-self/cwd` and `/proc/<pid>/cwd`), directly or through any
  symbolic link whose target goes there. The test below sees the child's
  `cwd` through `/proc/self/cwd/marker` without the lock.
- **The lock order.** `SPAWNER`, then `CWD_LOCK` (`spawner` sets
  `NO_FALLBACK` under both). Under `CWD_LOCK` the crate makes system calls
  only, never a stream lock, the scheduler's lock or the loop lock, and a
  thread never takes it twice (a second read lock can wait for good behind
  a waiting writer). The caller's sink (`realPath`'s result) runs after the
  unlock. `CWD_LOCK` may be taken while a stream lock or the loop lock is
  held, never the other way.
- **Deviation** (no Lean bug): once a spawner has unshared, a later refusal
  of `unshare` (a helper thread's, `on_helper`; or a new spawner's after the
  first one ended, which takes a Rust panic) does not take the fallback:
  that spawn fails with `EAGAIN`, as a failed `fork` does. Natively the
  forked child enters `cwd` itself. Taking the fallback there would move
  the process while other threads' path lookups take no lock.
- Tests: `io::process::tests::threads_path_lookups_never_see_a_fallback_spawns_cwd`
  (with the test hook `FORCE_FALLBACK`: a thread loops over `metadata`,
  `Handle.mk` and a read, `readDir`, `realPath`, `createDir` and
  `removeDir` of relative paths, and `metadata` of
  `/proc/self/cwd/marker`, while `main` makes 100 spawns in another
  directory; without the lock it fails at its first look, and with the
  lock for relative paths only it fails at the absolute path);
  `threads_no_fallback_once_the_spawner_unshared` (the refusal, the working
  directory unchanged); `rt2_03_a_fifo_open_does_not_block_a_fallback_spawn`
  (a reader and a creating writer wait in their opens of FIFOs while
  fallback spawns' children open the other ends: a deadlock before RT2-03,
  killed at 10 s; both go on after); and
  `threads_fallback_opens_keep_their_outcomes` (every mode on a new, an
  existing, a directory, an unreadable, a dangling-link and a missing path:
  the same errors, files and open-file status flags as the one `open(2)`;
  with the added `O_NONBLOCK` left set it fails); and
  `rt2_12_open_at_the_descriptor_limit` (the file's number is the one
  open's, 4, where the first fix gave 5; with one descriptor left
  `Handle.mk` succeeds, where it was `EMFILE`). The sticky check itself
  needs a second user id, so no test runs it.
- A translator's own system calls that resolve paths go through
  `io::process::with_path_lookup` too, and none that can wait (a glue
  duty, in threads mode).

**2. A worker's streams and `errno`** (1.4; reviews RT2-L-01, AR-24). A
task's current standard streams (`IO.setStdout` & co.) and its `errno` are
its thread's: Lean's docs say `IO.setStdout` replaces "the stdout of the
current thread", and both are thread-locals natively (`io.cpp` 115-117,
`MK_THREAD_LOCAL_GET`; C's `errno`). So a pool worker keeps them from one
task to the next: a task that sets a stream, or leaves `errno` set, and
does not restore it, leaves it to the next task of that worker; a new
worker, a dedicated task's thread and `main` start with the process's
streams and `errno` 0; a `sync` task shares the thread it runs on. leanrs's
probe (task A sets its stdout to a buffer and does not restore it; `main`
waits for A; task B prints) put B's line into A's buffer in 10 runs of 10
at `LEAN_NUM_THREADS` 1, 2 and 4. T2 first gave each task fresh streams,
which was not one of native's outcomes; it now follows native, in both
modes, and the glue does nothing for it:
- **threads mode**: the real threads give it. The io layer's slots
  (`io::streams`) and its modelled `errno` (`io::error`) are thread-locals,
  so a worker keeps them; `Glue::task_begin` and `task_end` have nothing to
  do for them (T2's `io::streams::task_begin` and `task_end` are gone).
  A worker's thread-locals' destructors drop them when it ends: `finish`
  joins the standard workers once the queue is empty, then calls
  `Glue::workers_end`, and only then waits for the dedicated threads, as
  `~task_manager` does (`object.cpp` 981-985; reviews AR-33, AR-34;
  `tests/threads_twins.rs` runs `tasks/worker_streams_closed_at_exit`,
  `worker_streams_at_process_exit` and `worker_streams_before_dedicated`);
- **the single-thread scheduler** (with `io`): `src/sched/slots.rs` keeps a
  `ThreadSlots` (the slots and the modelled `errno`) per context, swapped
  by the hub around each resume, and per emulated thread, swapped by
  `run_task`: a pool task runs with the set of the lowest free worker id
  (free: no task of it runs; one waiting in `Task.get` holds it), which
  keeps what the task leaves; a dedicated task with a fresh set; the event
  loop's context keeps one set across loop contexts (native's one loop
  thread). The glue's `switched` must not swap `io::streams` too now.
  `finish` drops the emulated workers' sets once no pool task is queued or
  running, before it waits for the dedicated tasks, as native's task-manager
  finalization ends the workers (reviews AR-33, AR-34).
  Which idle worker natively takes a task is the schedule's choice; the
  lowest free id is one of its outcomes, and the one the cases record.

`sched::running_worker() -> Option<u32>` (review AR-32, lean2rr's AR-S3),
in both modes, names the worker for a glue's own per-thread state: in
threads mode the standard worker's index (the order the task manager made
it in), on that thread whatever runs there (a `sync` dependent included),
and `None` on a dedicated task's thread, `main`'s and the loop thread; in
the single-thread scheduler the emulated worker id of the innermost
running pool task (a `sync` task shares the thread below it, and that
thread's answer, in both modes; review RF3-03; `docs/sched.md`, item 1 of
"The glue"). Unit test in threads mode:
`running_worker_names_the_pool_worker_thread`.

**A glue with per-task state of its own** (review AR-26, lean2rr's
AR-S1; lean2rr's case `RtTaskSyncStream`: a task sets stdout to a buffer,
its `sync := true` dependent's line must land there). The crate's side
holds in both modes: the task's `io::streams` set stays installed while its
dependents are walked (threads mode: the real thread's; the single-thread
scheduler: `TaskSlots` lives until the end of `run_task`, after the walk).
A translator whose stream values are its own objects, which only its code
can drop, cannot put them in `io::streams` (the crate would drop them);
lean2rr opens a stream context in its job and closes it before the job
returns, which was before the walk. So both schedulers have
`sched::end_running_task(id)`: the job calls it after it stores the value,
with its own task's id (the one `spawn` or `depend` returned for it, which
the glue keeps in its task object); the task ends and its dependents are
walked there, inside the job's context, then the job closes the context
and returns `Outcome::Done` (the scheduler sees the task ended; only the
glue's `task_end` is left).
- The call ends task `id` only while the scheduler runs its job as the
  innermost task of the calling thread (or context). A job the glue runs
  itself (the `dependent_runs_now` path, a `spawn` or `depend` without a
  task manager, which return `TaskId::FINISHED`) and a second call do
  nothing (review RT2-14; before, the call ended the enclosing task).
- The job may start before `spawn` or `depend` returns (a worker can take
  it at once; a `LEAN_SYNC_PRIO` spawn or a `sync` dependent of a finished
  task runs inside the call). The glue stores the id where the job reads
  it before it gives the id to anyone: then a job that finds no id has no
  dependent yet, and its call doing nothing is harmless.
- The walk is the task's own, to its end, also for a `sync` dependent that
  a walk handed (review RT2-15, leanrs's AR26-01; before, the single-thread
  scheduler walked nothing there, and the dependent's own `sync`
  dependents ran after its context closed).
- A chain of `sync` dependents whose jobs call it runs one level deeper on
  the stack per link, as natively (`handle_finished` runs `run_task`):
  the scheduler's own walk is a loop, but this one is the job's. A glue
  that calls it on `main`'s stack (8 MiB) should know.
- A job that calls it and then returns `Continue` is the glue's error: a
  panic in the single-thread scheduler, an abort with the crate's message
  in threads mode (review RT2-16; before, a panic with the lock held, after
  which `finish` waited for good).

Unit tests in both schedulers: `a_job_ends_its_task_before_it_closes_its_context`
(with a control job that does not call it: its dependent runs after the
context closed), `rt2_14_end_running_task_ends_only_its_own_task` (an
inline job's call and a second call end nothing),
`rt2_15_end_running_task_in_a_walked_sync_dependent` (A -> B -> C: C sees
B's context); and `rt2_16_continue_after_end_running_task_aborts` in
threads mode. Each fails with its fix undone (checked by mutation; RT2-15's
threads-mode twin passed before too).

Cases (recorded natively with `cases.py expect`, 5 runs each, identical):
`tasks/worker_keeps_streams` (B's line lands in A's buffer; while A sleeps
with its redirection, `main` prints to its own stdout; a dedicated task
prints to the real one) and `tasks/worker_keeps_errno`
(`LEAN_NUM_THREADS=1`: B's `getLine` on a handle with its sticky error flag
reports A's leftover `ENOTDIR`, `main`'s reports its own `EEXIST`; A's
`ENOENT` would crash natively, LB-03). Both pass on the single-thread
scheduler (`tests/sched-driver`) and in threads mode
(`tests/threads_twins.rs`); with the single-thread swaps disabled (the old
shared state) both fail. Unit tests: `a_pool_worker_keeps_its_streams_and_errno`
in `sched::tests` and in `sched::mt::tests` (under Miri too, with
`io,threads`).

**3. `sched::uv` on threads** (`src/sched/mt/uv.rs`, re-exported as
`sched::uv`; module comment).
- **Which thread runs the loop.** A thread of its own, the loop thread, as
  natively (`initialize_libuv`, `libuv.cpp` 19-27, starts an `lthread` that
  runs `event_loop_run_loop` for the life of the process). Here it is made
  at the first use of the loop, with the task manager's stack size and
  Lean's stack-overflow report; it calls the glue's `thread_start` once, at
  its start, or, made before `sched::start` (a Lean `initialize` that makes
  a `Timer`), once a glue appears, before it runs translator code (review
  RT2-05); it never ends.
- **The loop lock** (`LoopLock`): native's recursive `event_loop_t` mutex
  with `n_waiters` (`event_loop.cpp` 66-102). The loop thread holds it
  during each iteration (`uv_run(UV_RUN_ONCE)`, libuv `core.c` 415-471):
  it waits in `poll(2)` for the earliest timer, the signal pipe (while a
  watcher listens) and the loop's async eventfd (chosen once, when the
  loop is made: native's, from `io::startup`, when the glue has opened
  native's startup descriptors by then, else its own; review RT2-04); then
  it delivers the signals that came, and runs the timers due
  (`uv__run_timers`, `timer.c` 165-195: the due timers collected first, in
  deadline order, then start order). Every extern takes the lock first.
  When another thread holds it, the extern counts itself a waiter and
  writes the eventfd (`uv_async_send`), the loop thread ends its iteration,
  and it waits while a waiter is left before its next iteration (85-91).
  So an extern acts after the loop has handled what was due, as natively.
  The lock is recursive per thread: a callback's `sync` dependent that
  calls an extern on the loop thread goes on at once.
- **`next` reads the promise's state once** per decision (review RT2-13):
  the program may resolve the promise from another thread at any time;
  RT2-07's first fix read it twice, and a resolution in between made `next`
  panic (tests `rt2_13_{timer,signal}_next_with_a_promise_resolved_between_its_reads`:
  a panic before, the same promise after).
- **Who resolves what.** The loop thread resolves a timer's promise when
  the timer fires (`handle_timer_event`) and a watcher's when its signal
  came (`handle_signal_event`), through `LoopPromise::resolve`, with the
  loop lock held: the promise's `sync` dependents run on the loop thread,
  its other dependents go to the pool, its waiters wake. An extern resolves
  nothing itself; it makes a handle's promise (`new_promise`, with no
  handle state lock held, review RT2-07) and drops a handle's reference
  (`stop`, `cancel`, a repeating handle's `next`) on the calling thread with
  the loop lock held, as natively `lean_dec(m_promise)` runs under
  `event_loop_lock`; the glue resolves a promise whose last reference goes
  with `none` there.
- **`stop` and `cancel`** change the handle's state before they release its
  promise, as the single-thread module does (Lean master's `stop`, PR
  #14793; `cancel` as lean-runtime's correction; LB-33, LB-34): the
  release's `sync` dependents see a finished (or initial) handle. A `stop`
  of a timer that does not run still releases its promise, as 4.34.0.
- **Placeholders** (AR-22's shapes, review RT2-08): `Timer::placeholder()`
  and `Signal::placeholder()` make an initial handle without the loop lock,
  so they never wait for the loop, for a glue's `mem::take`-style moves.
- **`LoopPromise`** has the single-thread module's methods (`is_resolved`,
  `resolve`), with the bound `Clone + Send + 'static`: the loop thread
  resolves and drops it. `Timer<P>` and `Signal<P>` are `Send + Sync`. The
  names and shapes are the single-thread module's, so a glue switches with
  a `cfg` (leanrs's point 1).
- **Signals: one delivery for the process** (3.2). The handlers, the pipe
  and the watchers' counts are process-wide (`src/sched/uv_signals.rs`,
  shared with the single-thread scheduler, moved there unchanged); the
  listening watchers are one list, the loop's. So a signal reaches the
  watchers started on any thread, repeating ones first, then in creation
  order, as libuv's one loop delivers to all its handles.
- **The lock order.** The loop lock first. Under it: a handle's state lock
  and the loop's `data` lock, each held only for plain data (a handle's
  before `data`, never the other way); and, through translator code (a
  promise's `new`, `resolve` and drop, and the `sync` dependents they run),
  the scheduler's lock, the stream locks, `CWD_LOCK` and `uv_signals`'
  locks. Nothing that holds one of those takes the loop lock, except
  translator code that the loop lock's holder runs itself (the same
  thread). So, as natively, a `sync` dependent on the loop thread that
  waits for a task which calls an extern on another thread waits for good,
  and a glue must not call an extern while it holds a stream's guard.
- **LB-19 and LB-20 are not copied**: a one-shot timer or watcher is
  finished before its promise resolves, and a failed `next` holds no extra
  reference, as in the single-thread `sched::uv` (`docs/sched.md`).
- **Deviations** (no Lean bug): the loop thread is made at the first use,
  not at startup (a program that does not use `Std.Internal.UV` has no such
  thread); `Loop.configure`'s `blockSigProfSignal` does not block `SIGPROF`
  in the loop thread while it polls (natively that only decides which
  thread runs a `SIGPROF` handler, which no Lean program can see), and
  `accumulateIdleTime` turns on nothing (nothing in Lean reads the
  metrics); occurrences of one signal between two iterations are one
  delivery (libuv makes one per occurrence), as in the single-thread
  module; an extern's timer counts from its own clock read, not from the
  loop's cached time (`uv_timer_start` adds the timeout to `loop->time`,
  which can be a little old). A repeating timer's next period counts from
  the iteration's time, as libuv's `uv_timer_again` (review RT2-09).
- Tests: the unit tests of `sched::mt::uv` (`src/sched/mt/uv_tests.rs`):
  the loop lock (recursive; a waiter goes before the loop's next
  iteration; under Miri too), one-shot and repeating timers resolved on
  the loop thread, `cancel` and `stop`, an extern that interrupts a loop
  thread waiting for a timer a minute away, externs in a resolution on the
  loop thread (recursive lock, LB-20), an unknown signal, one SIGUSR2
  that reaches a repeating watcher started on the test's thread and a
  one-shot watcher started in a task (in a child process); and the
  review's: `rt2_04_*` (native's descriptors opened after the loop's first
  use: an extern waited 7.8 s before, at once after), `rt2_05_*`
  (`thread_start` once, before the first resolution, on a loop made before
  `start`), `rt2_07_*` (a promise maker that calls an extern on its handle:
  a deadlock before, killed at 10 s), `rt2_08_*` (placeholders made while
  another thread holds the loop lock return at once), `rt2_09_*` (the tick
  after a late one came 301 ms later before, at once after). Each failed
  before its fix and passes after.

**4. The twins inside tasks.** In a threads build (`check.sh`'s
`io,threads,proc-title,stack-overflow,unsafe-fast`):
- every twin of `tests/io_cases.rs` and `tests/io2_cases.rs` runs inside a
  task, on a worker, which `main` waits for (`tests/in_task/mod.rs`): the
  program `IO.asTask (twin args)` then `IO.wait`. The same expected
  outcomes pass, the title's twins included (they read the arguments'
  memory and `/proc/self/cmdline`, not the thread's name);
- `tests/threads_twins.rs` runs every case of `tests/cases/uvloop` (34,
  LB-33's and LB-34's included) over threads mode's `sched::uv`, each twin
  inside a task, and the two task cases of item 2 on `main`, with a
  translator's values made thread-safe (`Arc`, `OnceLock`, `sched::Ref`),
  and with native's startup descriptors (`signal_fds`: no descriptor
  added). All give the cases' expected outcomes, `signal_sigio_default`'s
  recorded alternative included. It calls `cases.py check --diff`, which
  names a failing twin and prints its differences (review RT2-L-02).
  Three windows were widened and re-recorded natively, since a loaded host
  can stall a worker past them (review RT2-10): `timer_repeating`'s period
  40 to 100 ms, `timer_oneshot`'s 60 to 150 ms, `timer_cancel_reset`'s 200
  to 300 ms.

**Left for later** (section 5; T3 is done, 0.6):
- `net` in threads mode (networking is not a target now, owner); the real
  fix of the working directory's fallback,
  `posix_spawn_file_actions_addchdir_np`, which needs `unsafe`;
- `sched::uv`'s loop thread made at startup, as natively, only if a case
  ever needs it (none does).

### 0.6 T3: the task cases in both modes

T3 (branch threads-3, 2026-10-04) runs the program cases with tasks in
threads mode through a second driver, recorded the cases that need real
contention, and added the site's page for threads mode (`site/threads.html`).

**The second driver**, `tests/sched-driver-mt` (binary `sched-cases-mt`), is
a workspace package of its own. `threads` and the coroutine `sched` exclude
each other in one build (2.5), so cargo builds it in an invocation of its
own: `cargo test -p sched-driver-mt` (`docs/development.md`, "Tests").
- **Shared ports, no fork.** The ports of the `tasks/`, `sync/`, `refs/`
  and `taskio/` cases are one file, `tests/sched-driver/src/cases.rs`, which
  both drivers compile (this one by `#[path]`), with Lean's IO definitions
  (`lio.rs`) and the glue's scheduler-independent part (`glue_common.rs`:
  output, Lean's panics, `IO.Process.exit`, native's startup descriptors).
  A port names only what both drivers' `lean.rs` define: `Task`, `Promise`,
  `Ref`, the task functions, `Obj` (a counted object a task shares: `Rc`,
  or `Arc` here) and `Var` (a cell a task writes: `RefCell`, or a lock). The
  single-thread driver's other ports (`uvloop/`, the io and process cases
  with tasks, its own programs) moved to `cases_st.rs`.
- **The values** (`tests/sched-driver-mt/src/lean.rs`): a task is an `Arc`
  with a `OnceLock` slot, its last drop calls `release` from any thread;
  `Promise` the same; `IO.Ref` is `sched::Ref`, the rule of 3.1; every job
  and closure is `Send`, every value `Clone + Send + Sync`.
- **The glue** (`glue.rs`): `install_stack_overflow_handler` on `main`'s
  thread (the manager's threads install it themselves), the initializers,
  `sched::start(Arc<dyn Glue>)`, `main`, `finish`, the flush. No `unsafe`.
  The five hooks check the crate's side of their contract in every case
  (2.4): `thread_start` and `thread_end` pair up on each thread the manager
  makes, and all have ended once `finish` returns; `task_begin` and
  `task_end` nest on each thread, with the same `own_thread`, and none is
  open at `thread_end` or after `finish`; a task with a thread of its own
  begins only on a thread the manager made that runs no other task (review
  RT3-03); `workers_end` (fixes-4, AR-34) comes once, from `finish` on
  `main`'s thread with no task open, after every standard worker made
  before `finish` has ended. A broken rule aborts the run, so the case
  fails.
- **What it accepts.** Every case of the four areas runs 5 times, one run
  after the other, 6 cases at a time (`SCHED_MT_JOBS`; `SCHED_MT_CASES`
  picks cases). Each run must give the expected outcome or a recorded
  alternative, as `scripts/cases.py check` compares it (the single-thread
  driver's runner, `tests/sched-driver/tests/runner/`), with one exception:
  a case whose `deviations` give `lean_runtime` a known difference
  `LSCHED-xx` accepts no alternative, since its alternative is the deferred
  model's outcome and threads mode is native's model. A Lean-bug case's
  expected files are the correct outcome, so a run with native's fails.
  Alternatives come from native runs only (section 4).
- **Single-thread-only cases.** `cases::SINGLE_THREAD_ONLY` lists, with the
  reason, a case of the four areas whose port depends on what only the
  single-thread scheduler has (its yield points, the deferred model of an
  LSCHED difference); the driver refuses to run it, and its test checks
  that every case of the four areas either runs or is listed, and that each
  case's TOML is in a form its LSCHED check reads (review RT3-02). At T3 the
  list is empty: the yield points are no-ops in threads mode and no port
  needs them, and the LSCHED cases give native's outcome.
- **`uvloop/`** is not this driver's: `tests/threads_twins.rs` runs all 34
  of its cases in threads mode, each inside a task (0.5, item 4).
- **Results at T3**: 101 cases of 101 (with fixes-4's
  `tasks/worker_streams_before_dedicated`), 5 runs each, in a debug and a
  release build; no case needed a new alternative. The single-thread
  driver passes the same 101.

**The new cases** (`tests/cases/tasks/`, recorded natively with
`scripts/cases.py expect`, 5 runs each, identical; both drivers run them):
- `wait_chain_beyond_pool` (`LEAN_NUM_THREADS=2`): six pool tasks, each
  waiting for a promise the next one resolves; each wait raises the pool by
  one (`wait_for`), so the chain unwinds from the last task to the first;
- `wait_any_faster` (`LEAN_NUM_THREADS=2`): `IO.waitAny [slow, fast]`
  returns the fast task's value while the slow task still sleeps;
- `stack_overflow_in_dedicated` (`LEAN_STACK_SIZE_KB=1024`): Lean's
  message and status 134 from a dedicated task's own thread;
- `late_tasks_while_enqueuing` (LB-13, `LEAN_NUM_THREADS=1`): after `main`
  returned, a dedicated task enqueues a pool task every 200 ms while the
  only worker runs a busy task for 500 ms. Natively the worker runs the
  two tasks queued meanwhile, then exits, and the three enqueued later
  never run (`native`); the expected outcome runs all five.

Already present, and now run in threads mode too: LB-01's `refs/lost_update`
(a task sets a reference while `main` reads it), a stack overflow in a pool
task (`tasks/stack_overflow_in_task`), LB-13's `late_*` cases (one late
enqueue each), and a mutex handed between real threads
(`sync/mutex_handoff`: `main` to a dedicated task; `sync/condvar_turns`:
two dedicated tasks).

**`scripts/check.sh`** runs both drivers, in a debug and a release build,
on both toolchains, and clippy on both packages.

## Summary

- **The model is native's.** Threads mode is Lean's task manager ported to
  Rust:
  - a pool of `LEAN_NUM_THREADS` OS worker threads;
  - a thread per dedicated task;
  - one more worker while a pool task waits;
  - one lock over the task table.

  A blocked task blocks its own thread. There are no coroutines, so there
  is no `Glue::suspend`.
- **Two modes, chosen per program when it is built.** The single-thread
  scheduler (`sched`, today's) stays the default and does not change.
  Threads mode is a separate module, `sched::mt`, behind the cargo feature
  `threads`. It has the same functions, with `Send` bounds, and a threads
  build re-exports it as `sched` (0.3).
  - The feature selects at compile time, never per call: a build without it
    is byte-identical to today's (no `Arc`, no `Send` or `Sync` bounds).
  - A build has one scheduler: `threads` and the coroutine `sched` exclude
    each other.
- **In threads mode, every counted object is atomic.** Native marks objects
  multi-threaded lazily (`lean_mark_mt`). Neither translator can do that:
  leanrs's types are fixed at compile time, and Reussir colors a box
  atomic only when it creates it.
- **The crate's API checks `Send`.** Jobs are `Send`, and the glue is
  `Send + Sync`. leanrs would meet that with `Arc` and its relatives,
  behind a feature of its own. lean2rr does not support threads mode for
  now (owner); later it would need atomic Reussir boxes.
- **Native bugs are not copied.** LB-13 (a late pool task never runs),
  LB-01 (a lost `set`, through `get` or `modify`) and LB-18 (a `swap`
  returning its own argument) stay fixed. Refs follow Lean 4.35 in both
  modes (3.1).
- **The crate needs no `unsafe` for threads mode.** It uses std's threads,
  locks, condition variables and atomics; its `sched::uv` (T2) also uses
  rustix (`poll`, the eventfd) and signal-hook (the signal watchers), the
  crates `sched` uses already, through their safe APIs only. The leanrs
  coordinator approved them for `threads` (the delegated approver,
  2026-10-04).
- **The promise.** Every threads-mode outcome is one that native Lean could
  produce, except the documented deviations. A racy program's output varies
  from run to run, as it does natively. The recorded cases space their
  events tens of milliseconds apart, and they give the recorded outcome.

## 1. The model

### 1.1 What native Lean does

- **One lock.** The `task_manager` class (`object.cpp` 758-1098, about 340
  lines) guards everything with one `mutex`: one queue per priority 0..8
  (`m_queues`; `LEAN_MAX_PRIO`, line 71), the lists of dependents, the
  worker counts. It has three condition variables: `m_queue_cv`,
  `m_task_finished_cv` and `m_dedicated_finished_cv`.
- **Workers** are `lthread`s made on demand. An enqueue spawns one when none
  is idle and fewer than `m_max_std_workers` exist, else wakes an idle one
  (`enqueue_core`, 789-809). A worker takes the first task of the highest
  non-empty queue (`dequeue`) and runs it with the lock released
  (`spawn_worker`, 831; `run_task`, 885). The maximum is
  `LEAN_NUM_THREADS`, else `hardware_concurrency` (`get_lean_num_threads`,
  1111); 0 means no task manager, and tasks run at once
  (`lean_init_task_manager_using`, 1102; `lean_task_spawn_core`, 1189).
- **Special priorities** (`enqueue_core`): above 8, a thread of its own
  (`spawn_dedicated_worker`, 873); `LEAN_SYNC_PRIO` (2^32-1, line 72), at
  once on the enqueuing thread.
- **Waiting.** `wait_for` (1025) blocks on `m_task_finished_cv`. A pool task
  that waits raises `m_max_std_workers` by one meanwhile, and spawns or
  wakes a worker, so the pool cannot starve. A `sync` task that waits prints
  the `Task.get` panic first. `wait_any` (1049) blocks the same way, but
  does not raise the pool.
- **Finishing.** `resolve_core` (927) stores the value; `handle_finished`
  (938) walks the dependents, newest first, through `enqueue_core`, so a
  `sync` one (priority `LEAN_SYNC_PRIO`, `lean_task_map_core`, 1211) runs
  there, on the finishing thread. Cancellation passes on to them.
- **Deletion.** The last reference to an unfinished task gone,
  `deactivate_task` (1060) and `deactivate_task_core` (811) mark it deleted
  and drop its closure unlocked. A queued deleted task is freed when
  dequeued; a running one's value is dropped when it finishes (`run_task`).
  An IO task holds a reference to itself until it has run (`alloc_task`,
  1169: `keep_alive`).
- **Exit.** `~task_manager` (972) sets `m_shutting_down`, joins the standard
  workers, then waits for the dedicated ones. During shutdown
  `spawn_worker` returns at once (831-833): LB-13.
- **Values that cross threads.** An object starts single-threaded (`m_rc >
  0`, a plain count). `lean_mark_mt` (663) walks an object graph and negates
  each count; a negative count is updated atomically (`lean.h`:
  `lean_is_mt`, `lean_inc_ref_n`, `lean_dec_ref`). It is called on a task's
  closure (`alloc_task`) and value (`resolve_core`), a bind continuation
  (`task_bind_fn1`), a thunk's value (`lean_thunk_get_core`, 540), a value
  set into a multi-threaded or persistent ref (`lean_st_ref_set`, `io.cpp`
  1504), and by `Runtime.markMultiThreaded` (`io.cpp` 1614). A task object
  is born multi-threaded (`lean_set_task_header`, 1162). A multi-threaded
  object is never updated in place: `lean_is_exclusive` is false for it.
- **Threads.** Every `lthread` has Lean's stack size: 1 GiB on 64-bit
  targets (`thread.cpp` 28), or `LEAN_STACK_SIZE_KB` plus 128 KiB. Each
  installs its own stack guard and alternate signal stack when it starts
  (`lthread::imp::_main` builds a `stack_guard`, `thread.cpp` 128; its
  constructor calls `sigaltstack`, `stack_overflow.cpp` 85-90).

```
                    m_mutex
  main ──┐   ┌───────────────────────┐ ──► standard workers: at most
  task ──┼──►│ queues 0..8           │       LEAN_NUM_THREADS, plus one for
  task ──┘   │ lists of dependents   │       each pool task in wait_for
             │ worker counts         │ ──► dedicated threads, one per task
             └───────────────────────┘
  resolve_core ─► handle_finished ─► enqueue_core (sync dependents run here)
```

### 1.2 Three candidate models

| | Native's pool | M:N: coroutines on N workers | One thread per task |
|---|---|---|---|
| What | Native's workers and dedicated threads; a blocked task blocks its thread | N workers, each a hub of corosensei contexts; queued tasks stolen between workers; a started context stays on its worker | Every task gets an OS thread |
| Schedules | Native's | A context that computes delays the others on its worker, so every yield point and cooperative IO must stay | Not native's: no `LEAN_NUM_THREADS` cap, and priorities never matter |
| OS threads | As natively | N | One per live task |
| `unsafe` | None in the crate; no `Glue::suspend` | `Glue::suspend` on every worker, and wake-ups across workers | None |
| A blocking system call | Blocks one thread, as natively | Blocks a whole worker, unless that call was made cooperative | Blocks one thread |
| Code | New and small: a port of 340 lines of C++ | sched-1 per worker, plus stealing and wake-up channels | Small |

**Choice: native's pool.**
- It gives native's schedules by construction. The single-thread mode's
  emulation rules are then not needed: the pure-task rule, the yield
  points, the lone worker's latency model, the polling counts and the
  `EARLY` flags.
- It has no coroutine, so the only `unsafe` step of `sched` does not
  exist in this mode.
- Every blocking call blocks only its own thread, as natively. sched-io
  makes only some calls cooperative.
- M:N's one gain is fewer OS threads for blocked tasks, and native does not
  have that either: it uses one thread per blocked pool task.

A suspended coroutine never moves to another thread, in any model. Its
frames may hold thread-local addresses, and LLVM may keep such an address in
a register across the switch. Examples are Rust's `thread_local!`, this
crate's errno model, and Reussir's per-thread drop worklist
(`reussir_rt::drop`, lean2rr `drop.rs`). That is why M:N would pin a started
context to its worker (see `docs/sched.md`, "Toward real threads").

### 1.3 Threads mode at a glance

```
 main thread                  worker threads (lthread's stack size)
 ───────────                  ─────────────────────────────────────
 mt::start(glue)              loop: lock; wait for a queued task;
 main ── spawn/depend ──►        take the highest priority, oldest first;
         wait/wait_any ◄──       unlock; run its job (it fills the glue's
 mt::finish(): set shutdown,     slot); lock; mark it finished; walk its
   wait until no task is         dependents; notify the waiters
   queued, no worker and      dedicated threads: one task each, then exit
   no dedicated thread is
   left, then join them       state: one Mutex<State> and Condvars
 glue: flush, exit              (queue, finished, quiet), as m_mutex
```

Each OS thread keeps its running tasks (innermost last) in a thread-local,
as native's `g_current_task_object` (`object.cpp` 730). `check_canceled`,
`in_sync_task` and `wait` (whether it raises the pool's limit) read it.

**The lock rule.** No translator code runs under the scheduler's lock:
jobs, the `store` of `resolve`, the drop of a job, glue hooks. Native does
the same (`run_task` unlocks around the closure, `deactivate_task_core`
drops the closure unlocked, `resolve` drops `v` unlocked, 1002). So a
translator destructor that calls `release` or `resolve` cannot deadlock.

**In T1** (`src/sched/mt/task.rs`, module comment): one `Mutex<State>`
and three condition variables. `queue_cv` wakes idle workers (an enqueue,
a raised limit, the shutdown); `finished_cv` wakes `wait`, `wait_any` and a
resolver that lost the race (the end of a walk of a referenced task, LB-32's
wake, a contended resolution); `quiet_cv` wakes `finish` (a worker or a
dedicated thread ended). The other locks (a `Ref`'s, a `Std.Sync` object's)
are never held together with it. The only atomics: the shutdown and
started flags, and a running task's cancellation flag, read without the
lock by `check_canceled`.

**The slot.** The job writes the glue's slot before it returns. The
scheduler then marks the task finished under the lock. So whoever learns
under the lock that the task has finished also sees the slot (and a
`OnceLock` slot synchronizes by itself). The rule "the glue's slot comes
first" (`docs/sched.md`, The glue, item 3) stays.

### 1.4 How each rule carries over

| Rule | Single-thread mode (today) | Threads mode |
|---|---|---|
| A task starts | Deferred. It runs when needed, when the running code blocks with a worker free, at an effect point after 5 ms, when polled, or at exit | When a worker is free, as natively (`enqueue_core`) |
| Pure-task rule | A started pure task runs late (`pick`) | Not needed. A started pure task runs on its worker, and `main` goes on in parallel |
| Dropped pure task | Deleted if not started (`release`) | The same. A queued one is deleted; a running one finishes and its value is dropped (`deactivate_task`, `run_task`) |
| `Task.get`, `IO.wait` | A pending task runs inline on the waiter's stack once it is the head a free worker would take; otherwise the context blocks (sched-3) | The thread blocks (`wait_for`). A pool task frees its worker place meanwhile (the pool grows by one) |
| `sync` dependents, `LEAN_SYNC_PRIO` | Run on the finishing context, newest first | Run on the finishing thread, newest first (`handle_finished`, `enqueue_core`, `run_task`) |
| Dedicated tasks | A priority-9 queue, always started | A thread each (`spawn_dedicated_worker`) |
| `effect`, `poll`, `ref_read` | Let other contexts go first | No-ops |
| `sleep_ms` | Blocks the context | `std::thread::sleep` |
| `IO.getTaskState` | The polling rules (`query`) | Native's answer: queued or waiting is `waiting`; running, or an unresolved promise, is `running` (`get_task_state`, 1085) |
| `IO.waitAny` | A finished task; else the only unfinished one runs inline once it is the head (a waiter without a worker only); else wait, keeping the worker (sched-3) | The first finished task in list order; else block until a task finishes (`wait_any`) |
| `IO.cancel` | A flag; passed on to the dependents when the task finishes | The same (`cancel`, 1074; `handle_finished`) |
| `IO.checkCanceled` | The flag, or shutdown with the `EARLY` emulation | The flag, or the shutdown flag (`lean_io_check_canceled_core`, 1284) |
| Promises | `promise_new`, `resolve`; dependents walked on the resolving context | The same, on the resolving thread. The first resolution wins under the lock (`resolve`, 995) |
| Exit | `finish` runs what is left on `main`; LB-13 is not copied | `finish` sets the shutdown flag. It waits until no task is queued, no standard worker is left (each ends once the queue is empty) and no dedicated thread is left, then joins them. An enqueue during shutdown still gets a worker, so LB-13 is not copied. Afterwards there is no task manager: tasks run at once |
| `IO.Process.exit` | From any context | From any thread. The other threads run until the process ends, as natively |
| `Std.Sync` | Contexts block. The owner is a context plus a task's thread number | Threads block on condition variables. The owner is the OS thread |
| A thunk forced on two threads | The glue's waiter list (`block_sync`, `wake`). Forced inside itself: `hang` | A blocking once-cell in the glue. Forced inside itself: its thread hangs (LB-08) |
| Stack overflow | The crate's report (`install_stack_overflow_handler`, feature `stack-overflow`, AR-11): the guard of the registered thread's stack or of the context running on it | The guard of each OS thread. Each worker and each dedicated task's thread calls `install_stack_overflow_handler` at its entry, before the glue's `thread_start` (leanrs's point 4): its alternate signal stack and its record; the table grows with the live threads (review RS3-01) |
| Current streams, `errno` | Per context and per emulated worker (`slots`): a pool task gets the lowest free worker's set, which keeps what it leaves; a dedicated task a fresh one | Per OS thread: a pool worker keeps them from one task to the next, a new worker and a dedicated task's thread start fresh (0.5 item 2) |
| `IO.getTID` | `io::env::get_tid()`: `main`'s id plus `tid_offset()`, the emulated OS thread's number: a pool task's worker's, a dedicated task's a new one (review AR-37) | `io::env::get_tid()`: the thread's own `gettid`, native's answer (`lean_io_get_tid`, `process.cpp` 340). `thread_number()` and `tid_offset()` are still there, both the thread's number: 0 on `start`'s thread, a number of its own on any other |
| `LEAN_NUM_THREADS=0` | Tasks run at once | The same |
| A Rust panic in a job | Goes on as `main`'s panic | Aborts the process after Rust's message, since no thread can take it over (leanrs agrees, 6). So does a panic in a glue hook, on any thread, `main`'s included, and in the destructor of a value the crate drops (a released task's continuation, the glue; review RT1-01) |

A native worker keeps its streams and `errno` from one task to the next
(`io.cpp` 115-117, `MK_THREAD_LOCAL_GET`); a fresh worker starts from the
process's streams. Both modes do the same (0.5 item 2; reviews RT2-L-01,
AR-24, which corrected T2's first choice, fresh streams for every task), so
their outputs agree.

The single-thread mode runs a waited-for pending task inline on the
waiter's stack. Threads mode does not: natively a waiter blocks and a worker
runs the task; inline, the waiter's OS thread would own the task's locks,
and more pool tasks could run than `LEAN_NUM_THREADS`. leanrs agrees (6).

### 1.5 What changes in the code

The single-thread files do not change. Threads mode is new code in
`src/sched/mt/`, and `src/sched/threads.rs` is the module `sched` of a
threads build (`src/lib.rs` picks it with `#[path]`). It shares only plain
items with `sched`:
- `src/sched/common.rs`: `TaskState`, `priority()` and the messages
  (`GET_IN_SYNC_TASK`, `PROMISE_BEFORE_MANAGER`, `PROMISE_DROPPED`), moved
  out of `task.rs`, which re-exports them;
- `env.rs` (`lean_num_threads`, `thread_stack_size`);
- `stack_overflow.rs` (feature `stack-overflow`), whose context guard stays
  0 in threads mode.

| Piece | Today (`src/sched/`) | Threads mode (`src/sched/mt/`) |
|---|---|---|
| State | `thread_local! SCHED: RefCell<Sched>` (`mod.rs`) | One `Shared` per task manager, a `Mutex<State>` and three condition variables: the process's (`start`), or a unit test's own |
| Task table | A slab per thread; `TaskId` is a 32-bit generation and an index (`task.rs`, `TaskId::new`) | One table. Ids are valid on every thread. A 64-bit serial is never reused within a run, so the 2^32 reuse caveat goes |
| Run queue | Ten queues per thread, with the lone worker emulated (`Tasks::queues`, `worker`, `wake`) | The same ten FIFO queues, shared; real workers take from them |
| Contexts | `Contexts`: coroutines, the hub, `cur`, `CtxId`, the stack pool (`ctx.rs`) | None. Each OS thread has a thread-local stack of its running tasks |
| Waiters | `cell_waiters`, `progress_waiters`, listed by `CtxId` | Condition variables: a task finished (every waiter checks again), the queue, quiescence |
| Jobs | `Box<dyn FnOnce() -> Outcome>` | `Box<dyn FnOnce() -> Outcome + Send>` |
| Glue | `Rc<dyn Glue>`: `suspend`, `switched`, `task_begin`, `task_end`, `workers_end` (sched-io removed `idle`: the hub waits in the scheduler's event loop) | `Arc<dyn mt::Glue>`, `Send + Sync`: `thread_start`, `thread_end`, `task_begin`, `task_end`, `workers_end` |
| `Std.Sync` | State in a `RefCell`, waiters by `CtxId` (`sync.rs`) | State in a `Mutex`, and a `Condvar` per object |
| Streams, `errno` | io's thread-local slots and modelled `errno`, swapped per context and per emulated worker (`slots.rs`) | The same thread-locals, one set per real thread |
| Stack bounds | `running_stack()` and the report's record, per context | Not needed; each thread's record holds its own guard |
| Emulation | `STALE`, `LATENCY_*`, `POLL_QUERIES`, `EARLY`, `PICKED`, `io_need` | None |

The `Std.Sync` objects keep sched-1's handover rule: a released mutex goes to
the thread that has waited longest. That is one of native's outcomes (glibc's
mutex promises no order). Locking a `BaseMutex` that the same thread holds
waits forever, as glibc's does (`src/sched/sync.rs`, module comment). A
`Condvar` wait returns only once notified, as in sched-1. Native's may also
wake spuriously (`std::condition_variable::wait` without a predicate), so
both are native outcomes.

### 1.6 `Glue::suspend` on several workers

**In threads mode, `Glue::suspend` is never called:** that mode has no
coroutine. S1-S7 (`docs/sched.md`, "Why `Glue::suspend` is sound") hold for
the single-thread mode only, and nothing changes there.

**If an M:N mode were built later** (rejected above):
- S1 and S2 hold per worker only if the yielder fields and `cur` stay in
  per-worker (thread-local) state, never in a shared table;
- S3 and S7 are unchanged;
- S4: each hub runs on its worker's stack (`IN_HUB_HOOK` is already
  thread-local);
- S5: a worker must not exit while it owns suspended contexts;
- S6: a panic caught at a context's base resumes in a hub with no `main` to
  go on with, so it must abort;
- checklist items 3 and 5 become "every call is made on the thread whose
  scheduler made the context": a wake-up from another worker goes through
  that worker's queue, never resumes the context itself;
- moving a suspended context to another thread stays excluded (1.2).

## 2. The translator contract

### 2.1 Three ways to let values cross threads

| | What | Cost | leanrs | lean2rr |
|---|---|---|---|---|
| Native's marking | A count turns atomic when its object becomes reachable from another thread (`lean_mark_mt`) | Atomic only for shared objects, but a branch on every count update; a marked object is copied on every update | A custom pointer with a sign bit: `unsafe`, in every type | Ruled out by Reussir's design: a box is colored when it is created, and never later (`thread-safety.md` §1) |
| Everything atomic | Every counted object of a threads-mode program is atomic | An atomic read-modify-write on every count update. In-place updates stay: a count of 1 is still exclusive | `Arc` and its relatives, at the type mapping | Every Reussir box atomic, opaque types' counts included |
| Copies at the boundary | Values stay thread-local. A deep copy turns them `Send` at a spawn, and back at a `get` | O(size) per crossing, and sharing is lost. Refs, tasks, promises and mutexes cannot be copied (they are aliased), so they must be thread-safe anyway. Closures are opaque | A `DeepClone` per type, none for closures | A rebuild ("bare→arc conversion is an explicit rebuild", `thread-safety.md` §5) |

**Recommendation: everything atomic in threads mode.** The crate's API
checks `Send` (2.4). A program translated for the single-thread mode pays
nothing.

**Type-directed coloring** makes atomic only the types whose values can
reach another thread. It is a later optimization, after measurement. Values
reach another thread through:
- a task's closure or value;
- a ref, a promise or a thunk;
- `Std.Sync` state.

Through lean2rr's uniform `Box` and through closures, most types of a
program with tasks end up reachable.

### 2.2 What leanrs needs

leanrs's tasks run when they are created today (`task.rs`, module comment:
"the runtime starts no thread"). It adopts the single-thread `sched` first.
For threads mode:
- `Rc` becomes `Arc` for every shared value. `Task<T>(Rc<OnceCell<T>>)`
  (`task.rs`) becomes an `Arc<OnceLock<T>>` beside the crate's `TaskId`.
- `Thunk`: today an `Rc<ThunkCell>` with a `OnceCell` and a
  `Cell<Option<Box<dyn FnOnce>>>` ("`Thunk` is not `Sync`", `thunk.rs`).
  It becomes an `Arc` over a locked state:
  - a second thread that forces it waits;
  - a thunk forced inside itself hangs. leanrs's `get` already parks
    forever there (`None => loop { park() }`, per leanrs's review), a hang
    like native's spin (LB-08).
- `Nat` (and `Int` over it): a big `Nat` is an `Rc<Natural>` kept as a
  word through `Rc::into_raw` and `increment_strong_count` (leanrs's D3,
  four `unsafe` blocks proved by Lem-NT; `nat.rs`, module comment). A
  `PhantomData<Rc<()>>` field makes `Nat` neither `Send` nor `Sync`. Threads
  mode needs an `Arc` variant, with Lem-NT redone.
- `Shared<T>`: its `SharedCell` count is a `Cell<usize>` ("neither `Send`
  nor `Sync`, as `Rc<T>` is", `ptr.rs`). It becomes an atomic-count twin, or
  `Arc`. A twin is leanrs's own `unsafe`, and its proof (Lem-SC) is redone.
- `LocalLazy`: on any thread but its owner, `get` aborts with `INTERNAL
  PANIC: constant accessed from a second thread` (`lazy.rs`). Constants
  move to `Lazy` (`LazyLock`, `T: Send + Sync`).
- `IO.Ref` is `Rc<RefCell<T>>` (`ptr.rs`, `impl Placeholder for
  Rc<RefCell<T>>`). It becomes a lock with the semantics of 3.1.
- Function values `Rc<dyn Fn>` and `Box<dyn Fn>` gain `+ Send + Sync`.

Rust's compiler then checks every crossing. leanrs's `unsafe` in threads
mode is the `Nat` variant (Lem-NT) and the `Shared` twin (Lem-SC), both
proofs redone. It stays feasible as a runtime feature (a type alias and
twins), since every one of its about 470 `Rc::` call sites exists on `Arc`
too. The twins are behind that feature, so leanrs's default build is
byte-identical: no `Arc`, no `Send` or `Sync` bounds.

### 2.3 What lean2rr would need, and what it would need from Reussir

**The owner's decision (2026-10-04):** "reussir no change yet. lean2rr can
just not support it if it cant for now". lean2rr stays on the single-thread
`sched`. For a later threads mode:
- **Its runtime is single-threaded by design.** `leanrt` keeps its state in
  `Global<UnsafeCell<T>>` statics with `unsafe impl Sync` (`task.rs`,
  `sched.rs`); its plan says (§6) "Real threads, using Reussir's atomic
  reference counting, come later."
- **Reussir colors a box atomic when it creates it** (`Arc<X>`), and never
  re-colors an existing value (`thread-safety.md` §1), so native's
  `lean_mark_mt` has no counterpart. Reussir lowers `Arc` for `[shared]`
  records (test `lowers_arc_to_an_atomic_rc_box`) and has sync cells
  (`Atomic`, `Mutex`, `FlatLock`, `RwLock`). It lacks the `Sync` auto trait
  and `Arc` for arrays and closures, "an explicit error" today (test
  `arc_of_array_reports_unimplemented_lowering`; §7, items 1, 3 and 4).
- **lean2rr's containers are opaque to Reussir.** Its arrays, strings, refs
  and thunk or task cells are `leanrt` types whose block starts with the
  `u32` count that Reussir's code updates (`drop.rs`, module comment: "the
  FFI contract").
- **So it would need from Reussir** either (a) a whole-program atomic mode
  (every box created atomic, every count update atomic, opaque types'
  included), the simplest for lean2rr; or (b) `Arc` for closures and arrays
  and an atomic flag on opaque types, with lean2rr coloring the types.
- **And in `leanrt`:** atomic twins of `RVec`, `LRef`, `LCell` and
  `LPromise`; tasks on `sched::mt`; locks or once-cells for its constants
  (`once.rs`), `persist.rs` and `net.rs`; an `unsafe impl Send` in the glue
  to put a Reussir closure (a raw pointer to Rust) in a `Job`, sound only
  when every box the closure reaches is atomic. Its drop worklist
  (`reussir_rt::drop`) is already per thread.

### 2.4 The crate's API in threads mode

```rust
// lean_runtime::sched::mt, feature "threads" (std only, no unsafe),
// re-exported as lean_runtime::sched (src/sched/mt/mod.rs, T1)
pub type Job = Box<dyn FnOnce() -> Outcome + Send>;
pub enum Outcome { Done, Continue(TaskId, Job) }
pub trait Glue: Send + Sync {
    fn thread_start(&self) {}           // a new worker or dedicated thread
    fn thread_end(&self) {}
    fn task_begin(&self, _own_thread: bool) {}  // the glue's own state
    fn task_end(&self, _own_thread: bool) {}
    fn workers_end(&self) {}            // AR-34: the standard workers joined
}
pub fn start(glue: Arc<dyn Glue>);
pub fn start_with(glue: Arc<dyn Glue>, workers: u32, stack_size: usize);
pub fn spawn(job: Job, prio: u64, keep_alive: bool) -> TaskId;
pub fn dependent_runs_now(src: TaskId, sync: bool) -> bool;
pub fn depend(src: TaskId, job: Job, prio: u64, sync: bool, keep_alive: bool) -> TaskId;
pub fn wait(id: TaskId);
pub fn is_finished(id: TaskId) -> bool;
pub fn state(id: TaskId) -> TaskState;
pub fn wait_any(ids: &[TaskId]) -> usize;
pub fn cancel(id: TaskId);
pub fn check_canceled() -> bool;            // no lock
pub fn release(id: TaskId);                 // from any thread
pub fn end_running_task(id: TaskId);        // AR-26: a job ends its own task early
pub fn in_sync_task() -> bool;
pub fn await_task(id: TaskId, report: impl FnOnce(&str));  // Task.get's rule
pub fn thread_number() -> u64;  pub fn tid_offset() -> u64;  // AR-37: the same here
pub fn thread_create_failed(err: &std::io::Error) -> !;
pub fn running_worker() -> Option<u32>;     // AR-32: the standard worker's index
pub fn manager_running() -> bool;
pub fn promise_new() -> Result<TaskId, &'static str>;
pub fn resolve(id: TaskId, store: impl FnOnce()) -> bool;  // store runs here
pub fn option_get_or_block<T>(opt: Option<T>, report: impl FnOnce(&'static str)) -> T;
pub fn hang() -> !;  pub fn hang_thread() -> !;
pub fn finish();
pub fn effect() {}  pub fn poll() {}  pub fn ref_read() {}  // no-ops
pub fn set_ref_read_yields(_on: bool) {}
pub fn before_task_value() {}  pub fn before_publish() {}  // no-ops
pub fn sleep_ms(ms: u32);
pub fn enter_no_suspend();  pub fn leave_no_suspend();     // a depth per thread
pub fn no_suspend() -> NoSuspendGuard;  pub fn in_no_suspend() -> bool;
pub fn io_cooperative() -> bool;  pub fn coop_possible() -> bool;  // false
pub struct Ref<T>;  // the 4.35 rule (3.1); Ref::empty(), a placeholder (wait-1)
// wait-1, core 3.3 (docs/sched.md, "The wait cores"), as in the single-thread
// scheduler: per thread, resolutions run on the dropping thread
pub struct DrainScope;  pub enum Deferred { Resolve(TaskId), Call(Box<dyn FnOnce()>) }
pub fn defer(d: Deferred);  pub fn deferred_pending() -> bool;  pub fn run_deferred();
pub mod sync { /* Mutex, Condvar, RecursiveMutex, SharedMutex: Send + Sync */ }
pub mod uv {        // T2: Std.Internal.UV on the loop thread (0.5)
    pub trait LoopPromise: Clone + Send + 'static {
        fn is_resolved(&self) -> bool;
        fn resolve(&self, value: i64);   // on the loop thread
    }
    pub struct Timer<P: LoopPromise>;   // new, next, reset, stop, cancel
    pub struct Signal<P: LoopPromise>;  // new, next, stop, cancel
    pub fn loop_configure(accumulate_idle_time: bool, block_sigprof: bool) -> Result<(), i32>;
    pub fn loop_alive() -> bool;
}
    impl<P> Timer<P> { pub fn placeholder() -> Timer<P>; }   // RT2-08
    impl<P> Signal<P> { pub fn placeholder() -> Signal<P>; }
// with `io` (T2): io::process::with_path_lookup(f) for the glue's own path
// lookups (none that can wait)
```

The bounds, and why each is needed:
- **`Job: Send`.** A worker thread runs the job and drops it.
- **`Glue: Send + Sync`.** Every thread calls the glue through one `Arc`.
- **`store` has no bound.** It runs on the calling thread, before
  `resolve` returns.
- **The translator's slot** (its own type) must be `Send + Sync`, since
  any thread reads it. Example: `Arc<OnceLock<T>>` with `T: Send + Sync`.
- **`TaskId`** is a plain `Copy` word.
- **A worker or dedicated thread** is made with
  `std::thread::Builder::stack_size(stack_size)`. If that fails, the crate
  reports native's `failed to create thread: <strerror>` message (the
  error's text) and aborts.

### 2.5 A translator that cannot provide these yet

- `sched` stays as it is: no `Send` bounds, `Rc<dyn Glue>`, coroutines.
  It stays the default.
- The feature `threads` selects at compile time. Without it the build is
  byte-identical to today's: no `sched::mt`, no `Arc`, no `Send` or `Sync`
  bound, and the IO layer's paths as sched-io leaves them.
- With it, the IO layer takes the blocking path by `cfg`, never by a
  per-call branch (3.2): io's cooperative code is `cfg(feature = "sched")`.
  So `threads` and the coroutine `sched` exclude each other (a
  `compile_error!` when both are on): a build has one scheduler. Each
  translator builds a program for one mode.
- A translator chooses threads mode per program, and only if it emits
  atomic values, behind a feature of its own. The glue's call sites keep
  their names and paths (`sched::spawn`, `sched::wait`, `sched::effect`,
  ...): a threads build re-exports `sched::mt` as `sched`. Only the glue's
  `Glue` implementation and its `start` call differ.

## 3. Shared state in the crate

### 3.1 `ST.Ref` and LB-01

**What native 4.34.0 does** (`io.cpp` 1435-1540). A reference takes the
multi-threaded path when it is multi-threaded or persistent (`ref_maybe_mt`,
1457). A reference captured by a task is multi-threaded (1.1). Its slot is
then an atomic pointer, empty (null) while taken:
- `get` (1459-1484) exchanges the value out, increments it, and exchanges
  it back unconditionally. It spins while the slot is empty. Putting the
  value back undoes a `set` that landed in between: LB-01;
- `take` (1486-1500) spins until the slot holds a value, and leaves it
  empty;
- `set` (1504-1514) marks the value multi-threaded and exchanges it in,
  whether the slot is empty or not;
- `swap` (1523-1540) exchanges in a loop until it gets a value back. On an
  empty slot the value it gets back is its own argument: it returns the
  value it stored, one reference owned twice.

`Ref.modify` is `take`, then `set` (`ST.lean`, `Ref.modifyUnsafe`). So
while `modify`'s function runs:
- a `get` waits for `modify`'s set (case `refs/get_during_modify`);
- a `set` lands at once and is then overwritten by `modify`'s set: the set
  is lost (LB-01, `refs/set_during_modify`);
- a `swap` returns its own argument at once, and the reference keeps that
  object with one reference count for two owners (LB-18,
  `refs/swap_during_modify`; upstream issue #14584).

**What Lean 4.35.0-rc1 does** (PRs #14585 and #14775; read from tag
`v4.35.0-rc1`):
- `Ref.set` is `discard <| Ref.swap r a` (`ST.lean`);
- `swap` is a compare-and-swap loop that never stores into an empty slot,
  so it waits while the slot is empty (`lean_st_ref_swap`, `io.cpp` 1521);
- `modify` is `take`, then the new `put` (`lean_st_ref_put`, 1502), which
  asserts that the slot is empty;
- `get` puts the value back into the slot it emptied, asserting that the
  slot is still empty.

So in 4.35 every operation but `put` waits on an empty slot, and `modify`
and `swap` are atomic.

**The rule, in both modes: Lean 4.35's** (LB-01 and LB-18, judged
2026-10-04):
- a completed `set` is seen by every later `get`, and `get` never puts an
  old value back over a newer one (`refs/lost_update`);
- only `modify`'s own store fills the empty reference. While a `modify`
  holds it, `get`, `take`, `set` and `swap` wait for that store. `set` is
  `swap` with the result dropped;
- so `modify` and `swap` are atomic;
- waiting blocks instead of spinning, with the same outcomes;
- the cost, as in 4.35: a `modify` whose function waits for a task that
  uses the same reference deadlocks. No case records it: whether a deadlock
  is the required outcome is the owner's call.

**The shape in threads mode.** A `Mutex<Option<T>>` and a `Condvar`.
- `get`, `take`, `set` and `swap` wait on the condition variable while the
  value is `None`; `modify`'s store puts into `None` and notifies.
- A value is cloned under the lock.
- A replaced value is dropped after the lock is released. Dropping it may
  release a task, which takes the scheduler's lock.

The ref is the translator's object, not the crate's: the crate holds no Lean
value (`docs/development.md`, "Signatures on views and plain data"). leanrs
agrees (6). The crate gives:
- the semantics (this section);
- the program cases of `refs/`: `lost_update` and `set_during_modify`
  (LB-01), `swap_during_modify` (LB-18), `get_during_modify`;
- a reference implementation in the drivers (`tests/sched-driver`'s `Ref`
  for the single-thread scheduler; `tests/sched-driver-mt`'s wraps
  `sched::Ref`);
- in threads mode, the rule as a generic type, `sched::Ref<T>` (T1,
  `src/sched/mt/refs.rs`; leanrs's point 3): a `Mutex<Option<T>>` and a
  `Condvar`, which a translator's ref may wrap or copy. It holds the
  translator's value as `Std.Sync`'s objects sit in its handles; it clones
  under its own lock and drops a replaced value after the unlock. Its unit
  tests are the `refs/` cases on real threads. `Ref::empty()` (wait-1) is
  a reference with no value, which well-typed code never reads (a
  placeholder), as in the single-thread scheduler; threads mode's users
  are the crate's own driver and tests (leanrs has no threads build).

**The rule holds in single-thread mode too.** `modify`'s function can block
(a `Task.get` in it), and another context then runs. A reader that found
the reference empty would see the placeholder at once. So `get`, `take`,
`set` and `swap` of an empty reference are blocking yield points until
`modify`'s store, the taker's own included (review RS4-01). Since wait-1
the single-thread scheduler has the rule as a type too, `sched::Ref<T>`
(`src/sched/refs.rs`, leanrs's cell moved into the crate), with threads
mode's API, and as keyed functions, `sched::ref_keyed`, for a translator
whose reference is a record (lean2rr; `docs/sched.md`, "The wait cores",
core 3.2). Both drivers wrap the crate's type (`tests/sched-driver`'s
`Ref` holds an `Rc<sched::Ref<T>>`), so the `refs/` cases test it in both
modes.

### 3.2 Table

| Item | Where | Today | Under threads |
|---|---|---|---|
| Scheduler state | `sched/mod.rs` `SCHED` | Thread-local `RefCell` | `sched::mt`: a global `Mutex` (1.5) |
| Ref-read polling | `sched/mod.rs` `REF_YIELDS`, `REF_READS_LEFT` | An atomic flag and a thread-local count | Not used: `ref_read` is a no-op |
| Thread numbers | `sched/ctx.rs` `NEXT_THREAD` | A process-wide atomic | Unchanged |
| Running stack bounds, hub flag | `sched/ctx.rs` `RUN_LO`/`HI`/`TOP`, `IN_HUB_HOOK`; the stack-overflow report's records (`sched/stack_overflow.rs`) | Thread-locals; a record per registered thread | Not used. Each thread the manager makes registers its own guard at its entry (T1) |
| `Std.Sync` objects | `sched/sync.rs` | `RefCell` state, `CtxId` waiters | `Mutex` state and a `Condvar` per object |
| `IO.Ref` | The translator's | `modify`'s function can block with the reference empty (3.1). `get`, `take`, `set` and `swap` of an empty reference must block until `modify`'s store: a glue duty (`docs/sched.md`, The glue, item 7; LB-01, LB-18) | A lock and a condition variable per reference, with the rule of 3.1 |
| Standard streams' `FILE`s | `io/handle.rs` `STDIN`, `STDOUT`, `STDERR` | `static Mutex<CFile>`: glibc locks each `FILE` | Unchanged |
| Open files | `io/handle.rs` `FileStream::file` | `Mutex<CFile>`; `Handle` is an `Arc` | Unchanged; already `Send + Sync` |
| `Handle.lock` | `io/handle.rs` `Handle::flock` | Waits in `flock` without the stream's lock (review RIO1-01) | Unchanged |
| A sink under a stream's lock | `io/mod.rs` `ByteSink` | The sink must not call `io` or exit | Unchanged; the rule is per thread |
| Open-handle list | `io/handle.rs` `OPEN`, `release` | `Mutex<BTreeMap<serial, Arc<FileStream>>>` (AR-7); every release under it | Unchanged. Opens racing the exit's walk behave as with glibc's list lock |
| Current streams | `io/streams.rs` `CURRENT` | Thread-local, swapped per context and per emulated worker (`slots.rs`; review AR-24) | One set per real thread, as natively: a pool worker keeps it from one task to the next (0.5 item 2) |
| Route of the runtime's stderr lines | `io/streams.rs` `StderrPut` | An `Rc` in the thread-local | Unchanged: it never leaves its thread |
| errno model | `io/error.rs` `ERRNO` | Thread-local, shared by all contexts | Per thread, as C's `errno` |
| Working directory | `io/process.rs` `CWD_LOCK` (`RwLock`) | Held for writing by the fallback spawn (`fallback_spawn`) and by `setCurrentDir` and `uv_chdir` (`with_cwd_change`); held for reading by `getcwd`, `uv_cwd` (`with_cwd_read`) and spawns without a `cwd`. Relative path operations take nothing. Gap documented: "another thread's relative path operation during the spawn ... still sees `cwd`" (module comment, item 4) | The gap was reachable; since T2 path operations hold it for reading while a fallback spawn may happen (below) |
| `Std.Internal.UV`'s loop, timers, signal watchers | `sched/uv.rs` over `sched/reactor.rs`; `sched/uv_signals.rs` | Per scheduler (thread): the loop context, its timers, its watcher list; the signal handlers and pipe process-wide | One loop thread for the process, as natively (`sched/mt/uv.rs`, T2): one loop lock, one timer list, one watcher list (below) |
| Spawner thread | `io/process.rs` `SPAWNER`, `NO_PRIVATE_CWD` | `Mutex<Option<Sender>>`; one long-lived thread | Unchanged. Spawns with a `cwd` queue on it, where native's forks run in parallel: a speed difference only |
| Modelled pids | `io/process.rs` `NEXT_MODELLED_PID` | `AtomicU32` | Unchanged |
| `output`'s drains | `io/process.rs` `DRAINS` | A `Mutex<Vec<JoinHandle>>`; one thread per failed `output`, joined after `main` (AR-6); it keeps its bytes and may end the process with the out-of-memory panic (RFX1-04) | Unchanged |
| Dropped streams' writers | `io/coop.rs` `WRITERS`, `hand_off` | A `Mutex<Vec<JoinHandle>>`; one thread per hand-off, holding bytes and a descriptor only, joined by the exit (AR-8) | None: `io::coop` is `sched`'s, so a dropped stream's close blocks the dropping thread, as natively (T1) |
| `environ` copy | `io/environ.rs` `ENVIRON`, `set`, `unset` | A `Mutex`; the C environment changes through `std::env::set_var` and `remove_var` | Unchanged. C code that reads the environment on another thread races with `setenv`, as natively (`lean_uv_os_setenv`, `uv/system.cpp` 320, calls libuv's `uv_os_setenv`). The crate is on edition 2021, where `set_var` is safe |
| Process title | `io/uvsys.rs` `TITLE` | `Mutex` | Unchanged |
| Startup descriptors | `io/startup.rs` `DESCRIPTORS` | `OnceLock` | Unchanged |
| `forceExit` flag | `io/exit.rs` `EXITING_WITHOUT_FLUSH` | `AtomicBool`, `SeqCst` | Unchanged |
| `semantics` | `src/semantics/` | No global state | Unchanged |

**The working directory under threads** (done in T2, 0.5 item 1). Without
threads the gap needs a second thread running Lean code. Threads mode
decides the spawn path at `mt::start`, by starting the spawner thread
there, before any task runs. If `unshare(CLONE_FS)` is refused, every path
operation takes `CWD_LOCK` for reading for the rest of the run (one
uncontended read lock per call), absolute ones too (`/proc/self/cwd` leads
to the working directory); if it works, the common case, nothing changes,
and no spawn takes the fallback from then on. The real fix stays the one `process.rs` names,
`posix_spawn_file_actions_addchdir_np`, which nix does not wrap and the
crate cannot call without `unsafe` (review RT1-04).

**Signals under threads** (done in T2, 0.5 item 3). The single-thread
`sched::uv` keeps each signal's `arrived` flag process-wide, while the
watcher lists are per thread (one per scheduler): with schedulers on two
threads, a signal would reach only the thread whose loop takes the flag
first (review RSIOB-15). Threads mode routes signals instead: one delivery
for the process, the loop thread's, which hands each signal to the
watchers started on every thread, as libuv's one loop does.

**IO under threads.** sched-io's cooperative path (a wait for the
descriptor in the scheduler's event loop, `reactor::poll_fds`, then the same
system call; the stream locks parked at every switch; the no-suspend scope)
is for the single-thread scheduler only. In threads mode every call takes the plain
blocking path, which blocks its own thread, as natively. The choice is a
compile-time `cfg`, not a branch per call, so a build without the feature
is unchanged (2.5). Since T1 it is io's existing `cfg(feature = "sched")`:
a threads build has no `sched` feature, so io compiles the path it has
without `sched`.

## 4. Determinism and testing

**Single-thread mode.** It stays deterministic for a program whose events
are farther apart than the emulation thresholds (`docs/sched.md`, "Schedules
that depend on the machine's speed"). Its tests do not change.

**Threads mode is not deterministic, and neither is native.** What it
promises:
- every outcome is one that native Lean 4.34.0 could produce, except the
  documented deviations (LB-01 and LB-13 fixed);
- a recorded case gives its recorded outcome. Its events are tens of
  milliseconds apart, and native gave that outcome in every recorded run
  (5 runs for `cases.py expect`);
- `IO.waitAny` returns as soon as a task of its list has finished;
- a computing task delays no other task, and a blocking call blocks only its
  thread.

It does not promise native's interleaving of racy output lines, native's
thread ids, or native's resident memory.

**Schedule-dependent cases.** A `schedule_dependent` case records every
outcome native showed (`<id>.out`, `<id>.altK.*`), and threads mode passes
with any of them. An outcome nobody recorded: run native 20 more times; if
native shows it too, record it as an `altK`, else it is a bug. The known
example is `tasks/dropped_pure_task`: natively the process exits only if the
task is dropped before a worker picks it up, a window of one thread wake-up
that every native run so far took. Threads mode has the same window (its
workers are made on demand, as native's). If it shows the other schedule
(the hang the case's comment describes), a reviewer confirms it, and it
becomes a hand-written `alt1`.

**How the tests run both modes:**
- **Unit tests of `sched::mt`** (small sizes; T1, `src/sched/mt/tests.rs`):
  the task table, deletion, the walk of dependents, cancellation,
  `waitAny`, promises (a contended resolution too), exit with late tasks
  (LB-13), the pool growing by one while a pool task waits and `waitAny`
  keeping its worker, LB-32's wake, `Std.Sync` under real contention, the
  `refs/` cases on `sched::Ref`, the stack-overflow registration of the
  manager's threads, and the regression tests of T1's review (`rt1_*`: a
  panicking destructor aborts, nothing is enqueued and no thread is made
  after `finish`; the reuse of the crate's alternate stacks, with the
  example `threads_altstack_reuse`). They run under Miri too, which runs std threads,
  locks and condition variables and reports data races (it cannot run
  corosensei's stack switch): `cargo miri test --features threads --lib --
  sched::mt`, in `check.sh` with `LEAN_RUNTIME_MIRI=1`.
- **T2's unit tests** (0.5): `sched::mt::uv`'s (the loop lock, timers,
  signals; only the loop lock's runs under Miri, the others need `poll(2)`,
  an eventfd and signal handlers), `sched::mt::tests::each_task_sees_its_own_redirections`
  (under Miri too, with `io`), and the working directory's
  (`io::process::tests::threads_*`, child processes that spawn).
- **The io and uvloop twins in threads mode** (T2, 0.5 item 4): with
  `io,threads,...`, `tests/io_cases.rs` and `tests/io2_cases.rs` run every
  twin inside a task, and `tests/threads_twins.rs` runs the uvloop cases
  over threads mode's `sched::uv`, and the task cases of review AR-24, once
  each.
- **Two drivers, one set of ports** (T3, 0.6). `tests/sched-driver` runs
  the program cases with tasks over the single-thread scheduler, once each;
  `tests/sched-driver-mt`, a package of its own (`threads` and the
  coroutine `sched` exclude each other in one build, 2.5, so cargo builds
  it in a separate invocation), runs the ports of the `tasks/`, `sync/`,
  `refs/` and `taskio/` cases, the same file, over `sched::mt`, with `Arc`
  values, a `OnceLock` slot, `sched::Ref` for `IO.Ref` (3.1) and a glue
  whose hooks check their contract. It runs every case 5 times and accepts
  the recorded outcome or a recorded alternative, but no alternative of an
  `LSCHED-xx` case (the deferred model's); a Lean-bug case (`native`) must
  give the corrected outcome every time. A case whose port needs the
  single-thread scheduler would be listed in `cases::SINGLE_THREAD_ONLY`,
  with the reason; none is (T3). The `uvloop/` cases run in threads mode
  in `tests/threads_twins.rs`.
- **`scripts/check.sh`** runs both drivers, in debug and release builds.
  ThreadSanitizer is run by hand (nightly), as AddressSanitizer is today.
- **Cases that need contention, recorded natively** (T3, 0.6): LB-01, a
  task setting a ref while `main` reads it (`refs/lost_update`); more tasks
  than `LEAN_NUM_THREADS` waiting on each other
  (`tasks/wait_chain_beyond_pool`); `IO.waitAny` returning the faster of
  two tasks (`tasks/wait_any_faster`); a stack overflow in a pool and in a
  dedicated task (`tasks/stack_overflow_in_task`,
  `stack_overflow_in_dedicated`); an exit while tasks still enqueue, LB-13
  (`tasks/late_tasks_while_enqueuing`, and the `late_*` cases); a mutex
  handed between real threads (`sync/mutex_handoff`, `condvar_turns`).
- **The translators** run their program cases (`cases.py check --exe-dir`)
  against a single-thread build and a threads build of each case.

The machine is shared. Repeated runs stay small (5 per case), and no case
needs brute force.

## 5. Staging

```
 sched-io ─┬─► T1 sched::mt ─► T2 io, threads mode ─► T3 driver, both modes ─┐
           │                                                                  │
           ├─► L1 leanrs adopts sched ─────────────────► L2 leanrs threads ◄──┤
           │                                                                  │
           └─► E1 lean2rr adopts sched ─┐                                     │
               R1 Reussir atomic counts ┴──────────────► E2 lean2rr threads ◄─┘
               (R1 and E2: not now, owner 2026-10-04)

 L2 ─► P: measurement (with the owner's permission), then defaults
```

| Step | What | Depends on | Size | In the crate alone? |
|---|---|---|---|---|
| T0 | sched-io lands: cooperative IO in the single-thread mode | — | (its own batch) | — |
| T1 | Done (2026-10-04, branch threads-1). `sched::mt`: the task manager (spawn, depend, wait, waitAny, state, cancel, release, promises, exit without LB-13), `mt::sync`, `mt::Glue`, `sched::Ref`, worker and dedicated threads with Lean's stack size, the stack-overflow report on them | T0: the IO switch, and the order the owner set | About 1,300 lines and 900 of unit tests | Yes, with Miri |
| T2 | Done (2026-10-04, branch threads-2; 0.5). io in threads mode: the blocking path (since T1, io's plain path by `cfg`); the `CWD_LOCK` rule of 3.2, `with_path_lookup` around every system call that looks up a path, in threads builds where `unshare(CLONE_FS)` is refused (review RT1-04); a worker's streams and `errno` kept from task to task, in both modes (reviews RT2-L-01, AR-24); `sched::uv` on threads (a loop thread for timers and signals, `LoopPromise` with `Send` bounds, the routing of signals of 3.2, the placeholders); the io and uvloop twins inside tasks, and the task cases of AR-24; the review round RT2 | T1 | About 2,000 lines, comments included (250 of them moved out of `sched/uv.rs`), and 2,500 of tests | Yes |
| N | Later: `net` in threads mode (the network on the loop thread; `threads` with `net` stays a compile error until then). Networking is not a target now (owner, 2026-10-04), and leanrs's threads mode comes later | T2 | Medium | Yes |
| T3 | Done (2026-10-04, branch threads-3; 0.6). The second driver (`tests/sched-driver-mt`: the `tasks/`, `sync/`, `refs/` and `taskio/` cases, 5 runs each, with the single-thread driver's ports, shared), `check.sh`, the new cases recorded natively, the site's page for threads mode | T1, T2 | About 700 lines of driver and test, the shared ports moved, and 4 cases | Yes |
| L1 | leanrs adopts the single-thread `sched` (already planned) | sched-io | leanrs's | No |
| L2 | leanrs threads mode, behind a feature of leanrs's own: an `Arc` alias; twins of `Nat` (Lem-NT), `Shared` (Lem-SC), `Task`, `Thunk` and `IO.Ref`; `Lazy` for constants; the glue over `sched::mt` | T3, L1 | Medium to large | No |
| R1 | Not now (owner, 2026-10-04: "reussir no change yet"). Later: a whole-program atomic mode, or `Arc` for arrays and closures plus an atomic flag on opaque types (2.3) | — | Large (Reussir's §7 items 1, 3, 4) | No |
| E1 | lean2rr adopts the single-thread `sched` (already planned) | sched-io | lean2rr's | No |
| E2 | Not now (owner): lean2rr stays single-threaded. Later: atomic `leanrt` containers, its statics made thread-safe, the glue's `unsafe impl Send` | T3, R1, E1 | Large | No |
| P | Timing and RSS of both modes, then the choice of defaults | L2 | Small, but needs the owner's permission | — |

**What T1-T3 check before any translator has threads:** everything the
crate owns, through Rust ports of every case. That is the task manager's
rules and exit (LB-13), `Std.Sync` under real contention, the rules of 3.1
for refs (LB-01 and the `refs/` cases, in the drivers' refs), the
stack-overflow report on workers, the IO layer's locks under concurrent
tasks (the io cases' Rust twins can run inside tasks), and data races under
Miri.

## 6. Risks and open questions

### Risks

- **The cost of atomic counts. Not measured.** An uncontended atomic
  read-modify-write costs several times a plain increment; a contended one
  far more. Native pays only for objects that cross threads, plus a sign
  test on every count update (`lean.h`, `lean_inc_ref_n`). Everything
  atomic pays on every object of a threads-mode program. Mitigations:
  threads mode only for programs with tasks, type-directed coloring later.
  Benchmarks need the owner's permission.
- **Contended constants.** Native's constants are persistent: a count of 0,
  never updated (`lean_mark_persistent`, `object.cpp`; `lean_is_persistent`,
  `lean.h`). A translator's constant that every thread clones bounces one
  cache line between cores. Mitigation: uncounted (immortal) constants in
  each translator.
- **In-place updates.** Everything atomic keeps them: a count of 1 is
  exclusive under atomics too. Native's multi-threaded objects lose them
  (`lean_is_exclusive`). Per program, threads mode may be faster or slower
  than native.
- **One global lock, and one OS thread per blocked pool task** (1 GiB
  reserved): both as natively.
- **The loop lock** (T2). As natively, the loop thread holds it while a
  `sync` dependent of a timer's or a signal's promise runs there, and an
  extern's thread holds it while a promise it drops runs its `sync`
  dependents. Such a dependent that blocks stalls every extern and the
  loop's timers and signals meanwhile, and one that waits for a task which
  calls an extern on another thread waits for good (0.5 item 3).
- **`unsafe`.** The crate needs none. The glue keeps its existing `unsafe`,
  now on every thread (the alternate signal stack and the guard lookup in
  `thread_start`). leanrs redoes two proofs: Lem-NT for an `Arc` `Nat` and
  Lem-SC for an atomic `Shared` (2.2). lean2rr, later, would add an
  `unsafe impl Send`, sound only for a program built in Reussir's atomic
  mode. Still not possible safely: a spawn that enters its `cwd` without
  changing the process's directory (3.2).
- **Interleavings.** Native's interleavings cannot be matched
  deterministically, by any runtime with real threads. What is promised
  instead is in section 4.
- **A Rust panic on a worker** aborts the process (decided below).

### Decided, and leanrs's answers

- **lean2rr** stays single-threaded for now, and Reussir does not change
  yet (owner, 2026-10-04).
- **`IO.waitAny` in a pool task** keeps its worker, as natively (`wait_any`
  does not raise the pool; `wait_for` does). T1 follows native. A candidate
  Lean bug goes to a judge, and only a judged verdict changes it (leanrs).
- **Running a waited-for task inline** stays out of threads mode (leanrs;
  1.4).
- **The ref's code** is per translator. The crate gives the semantics, the
  cases and a reference implementation in the drivers, not a `Ref` type
  (leanrs; 3.1).
- **Streams:** per thread in both modes, a worker keeping its own from one
  task to the next, as natively (reviews RT2-L-01, AR-24, 2026-10-04;
  this replaces the first choice, fresh per task).
- **A Rust panic in a job on a worker** aborts the process (leanrs; 1.4).
- **Lean 4.35's refs:** read from tag `v4.35.0-rc1` (3.1).
- **Refs, judged (2026-10-04):** a `set` lost during `modify` extends LB-01,
  and a `swap` returning its own argument is LB-18. Both modes follow Lean
  4.35 (3.1). leanrs approved the design at 5c6b365.

### Open questions for the owner

1. **The model.** Answered (2026-10-04): native's pool, no coroutines in
   threads mode.
2. **Atomic values.** Answered (2026-10-04): everything atomic in
   threads-mode programs; type-directed coloring only after measurement.
3. **Choosing the mode.** A translator flag per program, or automatic for
   programs that create tasks once measurement says so (2.5)?
4. **The translators' refs.** Does each translator's `get`, `take`, `set`
   and `swap` of a reference that `modify` has taken block until
   `modify`'s store (`docs/sched.md`, The glue, item 7)? Each translator
   checks its own against the `refs/` cases.
5. **The deadlock of 3.1.** A `modify` whose function waits for a task that
   uses the same reference deadlocks, as in Lean 4.35. Is that the required
   outcome (a case `refs/set_inside_modify` expecting a hang), or may a
   runtime report it?
