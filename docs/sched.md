# The task scheduler (`sched`)

`sched` (feature `sched`) is Lean 4.34.0's task manager
(`src/runtime/object.cpp`, `task_manager`) on one thread, for both
translators. It is lean2rr's model (its `leanrt`: `sched.rs`, `task.rs`,
`sync.rs`; plan §5.14), with three changes:
- it no longer depends on lean2rr's representation of values;
- the stack switch is corosensei's instead of hand-written assembly;
- one rule for pure tasks is new ("The pure-task rule").

This file covers:
- the model;
- blocking IO and the event loop (sched-io);
- what a translator's glue writes;
- why the glue's one `unsafe` step is sound, and the checklist for glue
  authors;
- the pure-task rule and its native evidence;
- the differences from lean2rr's runtime;
- the checklist of decisions Q5;
- what already works per thread, toward real threads;
- the costs to measure (O12).

## Files

| File | What |
|---|---|
| `src/sched/mod.rs` | The public API, `start`, `ref_read`, the low-level waits |
| `src/sched/task.rs` | Tasks: queues, dependents, walks, queries, cancellation, promises, the final run, the yield points |
| `src/sched/ctx.rs` | Contexts: corosensei coroutines, the hub, the `Glue` trait, the running stack's bounds |
| `src/sched/reactor.rs` | The event loop (sched-io): descriptors and timers on epoll, the cooperative `poll_fds`, the loop context and its callbacks |
| `src/sched/uv.rs` | `Std.Internal.UV`'s loop, timers and signals on the event loop |
| `src/sched/env.rs` | `LEAN_NUM_THREADS`, the number of processors, `LEAN_STACK_SIZE_KB` |
| `src/sched/sync.rs` | `Std.Sync`'s mutexes and condition variable |
| `src/io/coop.rs` | sched-io in the io layer (features `io` and `sched`): the cooperative reads, writes, `flock` and `waitpid`, and the stream locks |
| `tests/sched-driver/` | Every case of `tests/cases/tasks`, `sync`, `refs`, `taskio` and `uvloop`, and the io cases with tasks, as a Rust program over `sched` and `io`, with the glue a translator writes (native's startup descriptors included) |

`sched` depends on corosensei 0.3.4, rustix 1.1 (the event loop's epoll
and poll) and signal-hook 0.3.18 (the signal watchers' delivery; its safe
API only), and is built with cargo, offline, from the committed
`Cargo.lock` (`cargo build --offline --locked --features sched`;
docs/development.md, "Builds").

## The model

Native Lean runs tasks on a pool of worker threads. Here one thread picks
one of the schedules the pool can produce. A worker may start a task at any
time after it is created, and must have finished it when its value is
needed. So a task is *deferred*: it runs at the first of these points.

- **It is needed.** `Task.get` or `IO.wait` (`wait`) runs it right there,
  on the stack of whoever needs it, as a worker would while the caller
  waits. A task whose sources are still pending first runs that chain, from
  its deepest end, one task after the other.
- **The running code blocks.** A sleep, a lock, a promise, a task running
  elsewhere, or a read, write or wait that would block in the kernel
  ("Blocking IO and the event loop") blocks it, and one of the task
  manager's workers is free (`LEAN_NUM_THREADS`, or the number of online
  processors). The task then starts on a *context* of its own.
- **An effect point.** At an output, a flush, a process spawn or an exit,
  a task queued 5 ms ago or more (`STALE`) goes first, as natively its
  worker would have run it by then.
- **Polling.** `IO.getTaskState`/`IO.hasFinished` report a pending task
  waiting until the program asks again after a sleep, or asks 1000 times
  without one; the task then runs and is reported finished.
- **`main` returns.** `finish` runs what is left (`lean_finalize_task_manager`;
  see "Exit" below for where it differs from native).

A pure task the program drops before it has started never runs: Lean
deletes it (`release`).

**Contexts.** `main`'s context runs on the thread's own stack. Every other
context is a corosensei coroutine, on a stack of a native worker thread's
size: 1 GiB, or `LEAN_STACK_SIZE_KB` plus 128 KiB. corosensei's coroutines
are asymmetric (a coroutine suspends to whoever resumed it). So every
switch goes through `main`'s stack. When `main`'s context blocks, it runs
the *hub*, which resumes the contexts that can go on, one at a time; a
context that blocks suspends back to the hub. The hub picks, in this order:
1. a context that can go on, in the order they became able to (the event
   loop's due timers and ready descriptors are looked at first, and wake
   theirs);
2. a queued task, on a new context, if a worker is free;
3. a pure task a worker has started (below), when nothing else will ever
   happen (no sleeper, timer or registered descriptor);
4. otherwise it waits in the event loop: in `epoll_wait` until a registered
   descriptor is ready or the earliest sleeper or timer is due, or forever
   (a deadlocked native program waits forever too).

**Dependents.** When a task finishes, its dependents are walked from the
newest, as Lean's `handle_finished` walks them. A `sync` dependent (or one
at priority `LEAN_SYNC_PRIO`, 2^32-1) runs there and then, on the finishing
thread. The others are queued at their priority. Example
(`tasks/sync_dependent_order`, one worker):
- three `mapTask`s of `b` are made: "async dep", then a sync one, then
  "newest async dep";
- the sync one creates a task "made by sync dep";
- they print "newest async dep", "made by sync dep", "async dep".

A dependent the walk has not reached yet is not queued. A newer `sync`
dependent can hold the walk up, sleeping on the context that finished the
source. Then `wait` (and `IO.waitAny`, and the polling) waits for the walk
to queue the dependent, as natively (review RS1S-16; cases
`tasks/wait_dep_mid_walk`, `wait_dep_mid_promise_walk`,
`poll_dep_mid_walk`). On the walk's own context, that never happens: a
deadlock, as natively (`tasks/sync_dep_waits_older`: a `sync` dependent
waits for an older async one). So is a task that waits for a dependent of
itself (`tasks/task_waits_own_dep`). `IO.waitAny` and the polling treat
such a dependent as waiting: `IO.waitAny` returns when another task of its
list finishes, as natively (review RS1S-17; `tasks/wait_any_own_dep`).

**Waiters wake after a walk** (reviews RS2-01, RS2-05 and RS2-06 of
sched-2). Natively the threads blocked in `Task.get`, `IO.wait` or
`IO.waitAny` sleep on one condition variable, which `resolve_core`
notifies (`notify_all`, `object.cpp` 927-935) after a task's value is set
and its dependents walked, the `sync` ones run inline. Here the same:
1. when a task finishes (a promise is resolved or dropped too), its value
   is set: from now on `Task.get`, `IO.wait`, `IO.waitAny` and
   `IO.getTaskState` see it finished at once;
2. its dependents are walked;
3. when that walk ends, and when any walk ends, its own or another
   referenced task's (a `sync` dependent finishing inside a walk, an
   unrelated task), on any context, every context blocked in `wait` on a
   task that has its value wakes, and every `IO.waitAny` looks again
   (`notify_all` in `src/sched/task.rs`, over the walks in progress,
   `open_walks`).

A task that finishes after the translator's last reference to it went (an
IO task whose handle the program dropped, a pure task deleted while it
ran) notifies nobody: natively its keep-alive reference is the last one,
and its finish deletes it (`m_deleted`) without `resolve_core` (review
RS2-08; `tasks/sync_walk_mutex_unref_finish`, `wait_any_unref_finish`).

So a waiter wakes at the end of the first walk of a referenced task that
ends after its task's value is set. A `sync` dependent that sleeps, blocks
or suspends and then returns holds the waiters until it returns, unless
another referenced task finishes meanwhile:
- `tasks/sync_dependent_before_waiter`: no other task; the waiter sees the
  dependent finished;
- `tasks/waiter_wakes_after_nested_finish`: a newer, quick `sync`
  dependent's finish wakes the waiter while an older one still sleeps;
- `tasks/sync_walk_mutex_unrelated_finish`, `sync_walk_stuck_unrelated_finish`:
  a `sync` dependent blocks on a mutex the waiter holds, or loops forever,
  and an unrelated task's finish wakes the waiter.

`IO.waitAny` counts a finished task of its list at its first look and
after each notification (`notify_seq`), as native's `wait_any` looks
again only when `resolve_core` notifies: a task whose value is set while
its walk still runs is not seen until then (`tasks/wait_any_wakes_on_finish`:
it wakes when a queued dependent finishes, before the walk ends; review
RS2-06). An enqueue or a context's end also wakes it (`Wait::Progress`),
but then it only runs a task of its list that has become runnable, as a
native worker would run it (`tasks/wait_any_pure_stalled`: a dependent
queued by a walk that then stalls; review RS2-09).

A `sync` dependent that blocks for good keeps its source's waiters asleep
until another referenced task finishes, or forever (`tasks/sync_walk_mutex_alone`,
`sync_walk_stuck_alone`: native's deadlocks, followed). That is the
program's misuse: `Task.map`'s `sync := true` "should only be done when
executing `f` is cheap and non-blocking", and `Task.get` warns that
"deadlocks may otherwise occur". The one exception is a Lean bug (LB-32 in
`docs/lean-bugs.md`): when `Promise.result!` reaches its permanent block on
a dropped promise (`option_get_or_block`), Lean's own code blocks the walk,
and the waiters of the walks in progress on its context wake there, where
natively they wake only when another referenced task finishes, or never.

**Exit (decisions Q5 refinement A).** `finish` sets Lean's shutdown flag
(`IO.checkCanceled` is true in tasks from then on). Then it runs the
remaining tasks and waits, and only then returns; the glue then flushes the
standard streams and exits. It waits until:
- no task is queued or running, tasks enqueued meanwhile included;
- every dedicated task has run to completion;
- no context but `main`'s is left.

It does not wait for a task whose dependency never finishes (an unresolved
promise, a cycle), as natively.

Native Lean differs in one point, a Lean bug (LB-13 in
`docs/lean-bugs.md`): once no standard worker is left after `main`, a pool
task enqueued then never runs, so its effects are lost and a wait on it
hangs. `finish` runs it. The cases `tasks/late_*` expect the correct outcome
and record native's in their `native` field; `tasks/late_pool_child_runs`,
`late_dedicated_child_runs` and `main_waits_dedicated_child` are controls
where native already runs the task.

So:
- a runaway task keeps the process alive;
- the buffered stdout of `main` never appears (`runaway_io_task_unawaited`);
- a task's stderr line comes before `main`'s earlier stdout line on a
  merged pipe (`exit_joins_before_flush`).

**Schedules that depend on the machine's speed.** A few rules emulate, by
elapsed time, what native threads do in parallel, so on a slower or busier
machine the same program can take another of native's schedules:
- an output (any effect point) lets a task queued 5 ms ago or more go first
  (`STALE`), and a context able to run for 5 ms;
- the lone idle worker picks its task 90 µs after the enqueue that woke it
  the first time, 20 µs later on (`LATENCY_COLD`, `LATENCY_WARM`): tasks
  queued within that window compete by priority;
- a pending task polled with `IO.getTaskState` runs after 1000 answers
  without a sleep (`POLL_QUERIES`), however long those take.

The recorded cases do not depend on these thresholds: their sleeps are tens
of milliseconds or longer.

## The pure-task rule

**The rule.** Where a worker would start a pure task (`Task.spawn`,
`Task.map`, `Task.bind`) that no IO task waits for, directly or through
other pure tasks, the task is only marked *started* (`pick`,
`src/sched/task.rs`). From then on:
- it can no longer be deleted (`release` keeps it, as natively a started
  task runs to completion);
- `IO.getTaskState` reports it running;
- it runs at the first of these points:
  - it is needed (`wait`);
  - it is polled: two sleeps since the first answer, or 1000 answers
    (`query`);
  - an IO task comes to wait for it, directly or through other pure tasks
    (`need_up` walks the chain of waiting tasks up and gives each its *IO
    need*; `startable` starts a started pure task that gains it);
  - nothing else can go on, and no sleeper will wake (`last_resort`);
  - `main` returns (`finish`, before the queued tasks).

An IO task starts as before (lean2rr's rules), and so does a pure task with
IO need: in `t := Task.spawn f; u := t.map g; IO.mapTask h u`, `t` and `u`
run as soon as a worker is free, as natively. Without the chain, `main`
polling a flag that `h` sets would wait forever (review RS1S-01 of sched-1;
`tasks/pure_chain_io_dep`, and `tasks/pure_bind_io_dep` through
`Task.bind`).

**Why.** A pure task has no effects, so running it later is the schedule
of a slow worker. Running it at once on a context lets a runaway one take
the only thread while `main`, which natively goes on in parallel, waits for
its sleep to end. lean2rr's runtime does that.

**Native evidence.** Three cases exercise pure tasks. Each was run 20 times
on a native Lean 4.34.0 build (the toolchain in
`~/.elan/toolchains/leanprover--lean4---v4.34.0`), and each run gave the
recorded outcome.

| Case | What the program does | Native, 20 runs | lean2rr | Here |
|---|---|---|---|---|
| `tasks/runaway_pure_task_started` | `Task.spawn spin`; `hasFinished` (false); sleep 50 ms; `hasFinished` (false); prints `main done false false`; drops the task; returns | 20 of 20: prints the line, then hangs (the started task runs at exit) | prints nothing, then hangs: the task starts on a context during the sleep and never gives the thread back | prints the line, then hangs |
| `tasks/runaway_pure_task_referenced` | `Task.spawn spin` kept in a global; returns | 20 of 20: hangs after `main done` | same | same |
| `tasks/dropped_pure_task` | `Task.spawn spin`; `hasFinished`; drops it; returns | 20 of 20: exits 0 (deleted while queued) | same | same |

lean2rr's builds of all 15 cases of `tasks/` and `sync/` pass 14; the one
that fails is `runaway_pure_task_started`. The scheduler here passes all 15
(`tests/sched-driver`).

**The cost.** A program that polls a pure task with sleeps sees it finish
one question later than under lean2rr's rules:

```lean
let t := Task.spawn fun _ => quick 1
while !(← IO.hasFinished t) do IO.sleep 5
```

- Natively the loop typically ends at the first question, since the worker
  has finished `quick` by then.
- Under lean2rr's rules the first question answers false (a deferred task
  is reported waiting at the first question), and the second, after one
  sleep, runs the task and answers true.
- Here the first two questions answer false (waiting, then running), and the
  third, after two sleeps, runs the task and answers true.

`polling_with_sleeps_runs_a_pure_task_after_two` (`src/sched/tests.rs`)
records the three answers. No recorded case polls a pure task this way.

## Blocking IO and the event loop (sched-io)

Natively a read of an empty pipe, a write into a full one, `flock` and
`waitpid` block only the thread that makes them; the program's other
threads go on. On one thread such a call would stop every context. Example:
`IO.Process.output` reads the child's standard output on a dedicated task
while `main` reads its standard error. If the child writes more than a pipe
holds (64 KiB) to its standard output first, `main` blocks in its read, the
task never runs, the child blocks on its full pipe, and the program hangs
where native finishes (leanrs's review of sched-1, item 1; case
`taskio/output_big_stdout`).

**When a call cooperates.** A blocking call cooperates when
`io_cooperative()` is true: the task manager runs, and another context
exists, a task is queued, a started pure task waits for an IO task, or the
event loop has a descriptor, a timer or a due callback, and the thread is
not in a no-suspend scope (item 11 of "The glue"). Otherwise it is the
plain system call, as before, and costs nothing more:
- `coop_possible()`, one relaxed load of a process-wide flag, is false
  until the first task, promise, timer or watch: a program without any
  pays one load per stream lock and per system call (the speed floor O12);
- after that, the check is a borrow of the thread's scheduler and a few
  comparisons, once per system call that may block.

**What a cooperating call does** (`src/io/coop.rs`). It first waits for its
descriptor with `poll_fds`, so the other contexts run meanwhile, and then
makes the same system call, which no longer blocks. So its bytes and its
errors are the blocking call's; only the interleaving changes, to one that
native's threads allow.
- **Reads** (`read(2)` of a pipe, a FIFO, a socket, a terminal, a child's
  standard output) wait until the descriptor is readable. A read of the
  controlling terminal while the process's group is not its foreground
  group (`tcgetpgrp` against `getpgrp`) stays plain, so it gets `SIGTTIN`
  (or `EIO`) at once, as natively (review RSIO-08).
- **Writes** depend on the descriptor:
  - pipes and sockets: `pwritev2(RWF_NOWAIT)`, which writes what fits; on
    `EAGAIN` they wait until the descriptor is writable;
  - FIFOs (the kernel has no `RWF_NOWAIT` for them: `EOPNOTSUPP`, probed on
    Linux 7.0): they wait until writable, then write at most `PIPE_BUF`
    bytes, which a writable FIFO always takes. The byte stream is the
    blocking path's; the sizes of the `write(2)` calls may differ (a write
    of more than `PIPE_BUF` bytes to a pipe is not atomic natively either);
  - terminals and other character devices: they wait until writable, then
    make one `write(2)` of the whole rest, atomic against other writers as
    natively (the tty layer's write lock; review RSIO-05). A terminal is
    writable once fewer than 256 bytes wait in its output (`WAKEUP_CHARS`),
    so the write can still block the thread until the terminal drains, as
    natively the writing thread blocks.
- **`Handle.lock`** retries `flock` with `LOCK_NB`. An unlock or a close in
  this process wakes the waiters at once; another process's unlock is seen
  within 1 to 16 ms (the retry doubles from 1 ms).
- **`Child.wait`** waits until the child's pidfd (`pidfd_open`) is readable,
  which it is once the child has exited; where `pidfd_open` fails, it looks
  again every 1 to 16 ms. `waitid(WNOWAIT)` looks without reaping, so the
  `waitpid` that follows gives the status, or `ECHILD` for a pid that is no
  child, as without the wait.
- **The runtime's `IO.Process.output`** waits for both pipes with
  `poll_fds` before its `poll(2)` and its reads.

Regular files, block devices and directories never block (natively they
never give `EAGAIN` either), so their calls stay plain. So do descriptors
already in non-blocking mode, whose `EAGAIN` is the result (the standard
input of a child that could not start, `fdopen_bounded_pipe`). Each stream
finds out which it is once, with `fstat` and `F_GETFL`, the first time a
cooperating call needs it; the modelled `errno` is not touched.

**Stream locks.** A stream's `FILE` lock (a `std::sync::Mutex`) stays held
across such a wait, as glibc's lock stays held while its thread blocks in
`read(2)`. Another context that wants the same stream must then wait for it
cooperatively: a plain `Mutex::lock` from the same thread would deadlock.
So, once `coop_possible()`:
- every stream lock is taken through `io::coop::lock`, which records it in
  the running context's list (`HELD`, a thread-local);
- every context switch, whatever its reason (an IO wait, a promise, a
  sleep, a yield point; `switch_away`), moves the suspending context's
  locks to `OWNED`, under its id, and gives them back when it goes on
  (review RSIO-01: a lock held across any wait is covered, not only the io
  layer's own waits);
- a context that finds a stream locked by a suspended context of this
  thread waits for it (`block_sync`), and the guard's drop wakes it;
- a stream held by another OS thread is waited for with the plain
  `Mutex::lock`, as before.

A lock taken before the program's first task, promise, timer or watch is
not recorded (no cost before then), so a guard (`Handle::file()`) taken
then and held across the first task's creation and a later switch is the
one case another context cannot wait for.

So a task blocked writing to a full standard output (a slow reader at the
other end of the pipe) keeps standard output, and `main`'s next `println`
waits for it, as natively. So does the exit's flush (`_IO_flush_all` waits
for each stream's lock): a stream whose holder is writing is waited for,
cooperatively, so the writer and the tasks that drain its reader run during
the exit, and every byte is delivered, as natively; but a stream whose
holder is blocked reading is skipped (LB-29: natively the exit waits for
that read, forever when the input never comes). Each stream records what
its holder is blocked in, under its lock (`cfile`'s idle, input and output
states).

**The event loop** (`src/sched/reactor.rs`). It is the scheduler's own, one
per scheduler (per thread):
- **Descriptors.** `poll_fds(items, timeout)` is `poll(2)` for the calling
  context: it registers each descriptor with the loop's epoll instance, the
  context blocks (`Wait::Io`), and the loop wakes it when epoll reports one
  ready (level-triggered; an error or a hang-up counts). The epoll instance
  is native's own libuv loop descriptor when the glue has opened native's
  startup descriptors (`io::startup`), so the process has the same
  descriptors as natively; otherwise the loop makes one.
- **Timers and watches.** `timer_start(deadline, callback)` and
  `watch(fd, interest, callback)` run their callback on the *loop context*:
  a context of its own, as libuv's loop thread is a thread of its own,
  started when a callback is due and ended when none is left. A callback
  may block (a promise's `sync` dependent runs there); what becomes due
  meanwhile runs after it, in order, as on libuv's one thread. Due timers
  run in deadline order, then in start order.
- **The watch API** (review RSIO-02, and net-1's three guarantees):
  - `watch(fd, interest, callback) -> WatchId`, where `fd: impl AsFd +
    'static` (a clone of the glue's `Rc<OwnedFd>`): the reactor keeps it
    until `unwatch`, so the descriptor stays open and its registration can
    always be changed or deleted. One watch per descriptor (`EEXIST`).
  - Level-triggered, and out of epoll's set from the moment a call is
    queued until it returns: a callback that blocks while its descriptor
    stays ready does not make the hub spin, and the next call is queued
    only once the descriptor is seen ready again after the return.
  - Right before a queued call runs, `poll(2)` checks the descriptor for
    the watch's current interest; `POLLERR` and `POLLHUP` count as ready
    (a failed connect shows as `POLLERR|POLLHUP`, an end of file as
    `POLLIN|POLLHUP`). A call whose descriptor is no longer ready is
    skipped, and the watch is armed again.
  - `watch_modify(id, interest)` and `unwatch(id)` may be called from
    anywhere, the watch's own callback included. A modify there sets the
    interest it is armed with when the call returns. An unwatch there ends
    it: it is not armed again, and a queued call does not run.
  - `unwatch` lets go of the descriptor and the callback before it returns
    (a running callback keeps only its own reference to itself), so a
    descriptor the program no longer holds closes right there, as native's
    finalizer closes a socket, and its port can be bound again at once.
- **When it looks.** The hub, when no context can run, waits in
  `epoll_wait` until a descriptor is ready or the earliest sleeper or timer
  is due. Each step of the hub, each polling point and each effect point
  looks without waiting: due timers always, the descriptors at most once a
  millisecond. A context woken by the loop is an ordinary context able to
  run: at an effect point it goes first only once it has been able to run
  for 5 ms (`STALE`), as for any other.

Example (`taskio/task_reads_main_writes`): `main` writes 200 000 bytes to
`cat`'s input while a task reads `cat`'s output.
1. `main`'s write fills `cat`'s input pipe: `pwritev2` gives `EAGAIN`, and
   `main` blocks on the descriptor.
2. The hub starts the queued task on a context; it reads `cat`'s output
   until the pipe is empty, then blocks on it in turn.
3. `cat` drains its input; epoll reports `main`'s descriptor writable and
   `main` goes on, and so on, until `main` closes the pipe.

The cases of `tests/cases/taskio` check it (each recorded natively, 5 runs,
with a twin in `tests/sched-driver`):

| Case | What blocks | Without sched-io |
|---|---|---|
| `output_big_stdout` | `IO.Process.output` (Lean's definition), 300 000 bytes on the child's stdout before its stderr | hangs |
| `output_both_overflow` | the same, 100 000 bytes on each pipe in turn, 4 times | hangs |
| `output_while_ticking` | the runtime's `output` in `main` while a task prints every 50 ms | the task's lines come after the output |
| `task_reads_main_writes` | a task reading `cat`'s output while `main` writes to its input | hangs |
| `wait_in_task` | `Child.wait` in a task while another prints every 50 ms | the wait's line comes first |

The io cases with tasks run through the driver too: `io/lock_blocked` and
`io/lock_exit` (a task waiting in `flock`; without sched-io both hang) and
`io/lock_during_read` (a task blocked reading a pipe while `main` takes a
`flock`; without sched-io the thread waits 2 s in the read, and the effect
rule still prints `main`'s line first).

### `Std.Internal.UV`: the loop, timers and signals

`src/sched/uv.rs` has the externs of `Std.Internal.UV.Loop`, `Timer` and
`Signal` (Lean's `src/runtime/uv/event_loop.cpp`, `timer.cpp`, `signal.cpp`,
over libuv 1.48). Natively a dedicated thread runs libuv's loop, and a
timer or a signal watcher resolves its Lean promise from there. Here its
callback runs on the loop context, so the promise's waiters wake and its
`sync` dependents run on a context of its own, as natively on a thread of
its own.

- **Every extern catches the loop up first** (review RSIOB-04). Natively
  each extern takes the loop's lock (`event_loop_lock`), which makes the
  loop thread finish its current iteration: the timers due and the signals
  that arrived are handled before the extern acts. Here each extern
  (`reactor::catch_up`) looks at the loop at once (the descriptors too,
  without the millisecond's throttle) and lets the loop context run one
  iteration, however much keeps becoming due (review RSIOB-13), as long as
  it can go on (not when it waits in a callback, not in a no-suspend
  scope). From the loop context itself it does nothing: natively the loop
  thread already holds its lock, which is recursive, so a callback's `sync`
  dependent calling an extern runs no iteration. The yield lets every
  context able to run go first, not only the loop's, so each extern is a
  scheduling point for all of them, an order native's threads allow too
  (RSIOB-14). Cases `uvloop/timer_due_stop` (a one-shot timer due during a
  computation, then `stop`: its promise holds `()`),
  `uvloop/signal_cancel_restart` (a signal during a computation, then
  `cancel` and `next`, as `Std.Async`'s signal selector does: the resolved
  promise) and `uvloop/timer_catchup_bound` (a 1 ms repeating timer whose
  ticks' `sync` dependents compute and re-subscribe: `Timer.mk` from `main`
  returns at once, with generous bounds for a loaded host).
- **`Loop.configure`** succeeds and changes nothing (natively it turns on
  libuv's idle-time metrics, which nothing in Lean reads, and blocks
  `SIGPROF` in the loop thread while it polls; here the loop polls on the
  program's own thread, whose `SIGPROF` must stay deliverable).
  **`Loop.alive`** is true: native's loop always has its async handle.
- **Timers** follow `timer.cpp`'s state machine (initial, running,
  finished), each with its promise (`m_promise`):
  - one-shot: `next` starts the timer, and its promise resolves `timeout` ms
    later; later `next`s give that promise;
  - repeating: the first `next` gives a promise that resolves at once (the
    0th multiple), then each tick resolves the current promise, and `next`
    gives a new one once it has resolved. libuv starts the next period
    before the callback, from the time it fires. A repeating timer with
    timeout 0 ticks once (libuv's repeat 0 means no repeat), as natively;
  - `reset` moves a running timer's next resolution to `timeout` ms from
    now; `cancel` drops the promise (a one-shot timer becomes initial
    again); `stop` drops it (a finished timer's too, as timer.cpp 243-246)
    and finishes the timer. After `stop`, `next` gives a new promise that
    nothing resolves (it reads `none` once the program drops it).
- **Signals** follow `signal.cpp`'s: Lean's signal numbers (its table;
  others become 0, which `next` refuses with `UV_EINVAL`, leaving the
  watcher running with a promise that never resolves, as natively), one-shot
  and repeating watchers, the promise resolved with the signal's number.
- **A one-shot timer or watcher finishes before its promise resolves**
  (LB-20): its `sync` dependents see a finished handle, as every later
  dependent does, so their `stop` or `cancel` acts as on a finished handle.
  libuv still calls a watcher back before it stops listening (RSIOB-11): a
  one-shot watcher started by a `sync` dependent of another's promise finds
  that one still listening, so the handler is not registered again after
  `SA_RESETHAND`, and the next signal takes the default action (case
  `uvloop/signal_rearm_in_sync_dependent`, status 138; with an async
  dependent, `signal_rearm_in_async_dependent`, the new watcher gets it).

**Signal delivery** uses signal-hook's safe API only (review RSIOB-05):
- A signal's handlers are installed at its first watcher and never taken
  back (signal-hook cannot restore a disposition). They run in the order
  they were registered:
  1. `flag::register` sets the signal's `arrived` flag;
  2. `low_level::pipe::register_raw` writes a byte into the loop's signal
     pipe (the flag first, so a reader woken by the byte finds it set);
  3. the conditional default action (`flag::register_conditional_default`),
     which runs the signal's default action while no watcher listens
     (natively libuv restores `SIG_DFL` when the last watcher stops): after
     `stop`, SIGUSR1 ends the program again (status 138), and SIGCHLD is
     ignored again;
  4. while every listener of the signal is one-shot, a second
     `flag::register` that sets the default's flag at each signal: libuv's
     `SA_RESETHAND` for one-shot watchers, so a second signal takes the
     default action before the loop has delivered the first (RSIOB-02, =
     leanrs's R1). It is registered and unregistered
     (`low_level::unregister`) as `uv__signal_start` and `uv__signal_stop`
     re-register libuv's handler: when the first watcher starts, when a
     repeating one joins one-shot ones, when only one-shot ones remain, and
     when none is left.
- **The signal pipe** is native's own, from `io::startup` (made at startup,
  as libuv makes it in `uv__process_init`), when the glue opened native's
  startup descriptors: a watcher then opens no descriptor, and the pipe's
  type and numbers are native's (case `uvloop/signal_fds`). Without them,
  the first watcher makes a pipe of its own; if it cannot (`EMFILE`), that
  watcher's `next` fails, and the next watcher tries again (RSIOB-16).
  **The pipe's duty:** its
  descriptors live in a static the crate owns, for the life of the
  process: never closed, `dup2`'d over or reused, and its write end is never
  unregistered. signal-hook's handlers write to it by number, so a wrong
  descriptor is no undefined behaviour, but would corrupt whatever the
  number names. `register_raw` sets `O_NONBLOCK` on the write end, which
  native's pipe has too.
- **The loop** watches the pipe's read end. Its call drains the pipe until
  `EAGAIN`, then takes each signal's `arrived` flag and delivers the
  signals that came, in signal-number order, to this thread's watchers of
  each: repeating ones first, then in creation order, as libuv's signal
  tree orders them (RSIOB-08; case `uvloop/signal_order`). Occurrences of
  one signal between two calls are one delivery, where libuv makes one per
  occurrence (its pipe carries one message per occurrence and watcher); a
  repeating watcher's promise takes one value either way.
- **A signal that came while no watcher of it listened is no watcher's**:
  its flag is cleared when the signal's first watcher starts (RSIOB-03;
  cases `uvloop/signal_stale`, `signal_stale_deferred`).
- **At exit** the thread's watcher list is forgotten, not dropped: a
  watcher still listening holds a promise whose release would run its
  `sync` dependents inside the thread's destruction (RSIOB-01; case
  `uvloop/exit_listening`, status 0, nothing runs, as natively).

Deviations, by the safe API's limits (recorded as lean-runtime's own; no
Lean bug):
- **SIGIO** (RSIOB-06): signal-hook's table lacks its default action
  (terminate). After its last watcher stops, SIGIO makes the program exit
  with status 157 (`flag::register_conditional_shutdown`), where natively
  the signal kills it: `$?` and `IO.Process.wait` give 157 both ways; only
  `WIFSIGNALED` (and a core dump, which SIGIO's default does not make)
  tell them apart (case `uvloop/signal_sigio_default`: native's
  `returncode -29`, and lean-runtime's `returncode 157` as `alt1`).
- **SIGTSTP, SIGTTIN, SIGTTOU** (RSIOB-07): after their last watcher stops,
  signal-hook's default action stops the process with SIGSTOP, where
  natively the signal itself stops it (`WSTOPSIG` 19, not 20, 21 or 22),
  and SIGSTOP stops it even in an orphaned process group, where the kernel
  discards the three. No safe route restores their disposition. No case
  (a stopped process).
- the reset of a one-shot watcher's handler happens in the handler, where
  the kernel's `SA_RESETHAND` resets the disposition before the handler
  runs: the same outcome for a second signal (case
  `uvloop/signal_oneshot_twice`: two SIGUSR1 50 ms apart while `main`
  computes without a yield point, status 138).

**The loop holds a running handle**, as natively `lean_inc(obj)`: a
running timer or a listening watcher fires even if the program dropped it.
The promises are the translator's (`LoopPromise`: `is_resolved`,
`resolve`); a handle keeps a clone, and dropping the last clone resolves
the promise with `none`, as `deactivate_promise`. So the cases keep a
promise alive while they check that it has not resolved: compiled Lean
releases a promise right after its last use (the C of
`uvloop/timer_cancel_reset` releases it before `IO.hasFinished`), and a
promise only the program holds then resolves with `none`.

**Lean bugs** (`docs/lean-bugs.md`):
- **LB-19**: native's failed `next` releases the watcher once too often,
  and a later `cancel` or `stop` frees it while the program holds it; the
  next `next` crashes (SIGSEGV). Here a failed `next` leaves the watcher
  running with its promise, as natively, but with no extra reference:
  `cancel` makes it initial, and the next `next` fails with `EINVAL` again
  (case `uvloop/signal_failed_next`, native's crash in its `native` field).
- **LB-20**: natively a one-shot timer or watcher is still running while
  its promise's `sync` dependents run (handle_timer_event and
  handle_signal_event resolve before they finish the handle), so a
  dependent's `stop` or `cancel` releases the handle, and the callback
  releases it again: the next use crashes. Here the handle is finished
  first (above). Cases `uvloop/timer_stop_in_sync_dependent`,
  `timer_cancel_in_sync_dependent`, `signal_stop_in_sync_dependent`,
  `signal_cancel_in_sync_dependent` (the correct outcome is native's own
  for the same program with `sync := false`).

Cases `tests/cases/uvloop` (recorded natively, 5 runs, with twins in the
driver): `loop_configure`, `timer_oneshot`, `timer_repeating`,
`timer_cancel_reset`, `timer_due_stop`, `timer_catchup_bound`,
`signal_rearm_in_sync_dependent`, `signal_rearm_in_async_dependent`,
`signal_usr1` (SIGUSR1 sent by
`kill`, a child, to the program: one-shot, repeating, `cancel`, an unknown
number, and status 138 after `stop`), `signal_stale`,
`signal_stale_deferred`, `signal_oneshot_twice`, `signal_cancel_restart`,
`signal_order`, `signal_fds`, `exit_listening`, `signal_sigio_default`,
and the Lean-bug cases above. A case whose native program computes while a
signal or a timer comes (`timer_due_stop`, `signal_cancel_restart`) takes
two arguments: the native busy loop's length (about 1 or 2 s natively), and
the twin's spin, in milliseconds.

**For glue authors.** The translator's external objects hold a
`uv::Timer<P>` or `uv::Signal<P>`, where `P` is its counted promise
reference implementing `uv::LoopPromise`; the externs are one call each:
`lean_uv_timer_mk` is `Timer::new(timeout, repeating)`, `lean_uv_timer_next`
is `t.next(|| new_promise())`, then `reset`, `stop`, `cancel`;
`lean_uv_signal_mk` is `Signal::new(signum, repeating)`, then `next`,
`stop` and `cancel`; `lean_uv_event_loop_configure` and
`lean_uv_event_loop_alive` are `uv::loop_configure` and `uv::loop_alive`. A
failure is a libuv error code, which the glue turns into Lean's error with
`io::IoError::decode_uv_error(code, None)` (`lean_decode_uv_error`).

## The glue

A translator writes this glue around the crate. `tests/sched-driver/src/`
(`glue.rs`, `lean.rs`) is a complete example.

1. **`Glue`.** Implement `Glue::suspend`, which is the one `unsafe` step:
   ```rust
   fn suspend(&self, s: Suspend<'_>) {
       // SAFETY: lean-runtime docs/sched.md, "Why Glue::suspend is sound".
       unsafe { (*s.yielder()).suspend(()) }
   }
   ```
   The optional hooks:
   - `switched(from, to)`: each thread's current standard streams
     (`IO.setStdout` & co.);
   - `task_begin` and `task_end`: a task on a thread of its own starts with
     the process's streams.

   `switched` runs on `main`'s stack, inside the hub: it must not block or
   yield, and the scheduler panics if it tries. When nothing can run, the
   hub waits in the scheduler's own event loop; the glue has no hook there
   (sched-io removed sched-1's `Glue::idle`).
2. **Lifecycle.**
   - Run the module initializers. Tasks run at once then, as natively.
   - `sched::start(glue)` on the thread that runs `main`
     (`lean_init_task_manager`). It takes Lean's numbers: the task
     manager's workers from `LEAN_NUM_THREADS` (else the online
     processors), and each context's stack from Lean's thread size
     (`lthread`: 1 GiB on 64-bit targets, or `LEAN_STACK_SIZE_KB` rounded
     down to 4 KiB plus 128 KiB). A translator with rules of its own calls
     `start_with(glue, workers, stack_size)` instead (leanrs: its
     `LEANRS_STACK_SIZE_KB`, or 4 GiB).
   - `set_ref_read_yields(true)` if the program creates tasks.
   - Run `main`.
   - `sched::finish()`. With `io`, it also waits for the io layer's
     dedicated tasks (`io::exit::after_main`: the standard-output readers
     `IO.Process.output` leaves running when it fails; AR-6). A glue without
     `sched` calls `io::exit::after_main()` itself once `main` has returned.
   - Flush, then exit with `main`'s result.
   - `IO.Process.exit` from anywhere, a task's context included: `effect()`,
     flush, and C's `exit`, as `lean_io_exit`. The task manager is not
     finalized and no task is waited for (`tasks/exit_from_task`: status 3,
     `main`'s buffered line written, the other task never run).
3. **Tasks.** The task's value lives in the translator's own object. The
   `Job` fills it and returns `Outcome::Done`, or
   `Outcome::Continue(t2, job2)` when a bind function returned an unfinished
   task. Right before it stores the value, the job calls
   `before_task_value()`, which waits (letting the others run) for the
   writer threads of the streams the context's drops handed off (item 11):
   natively the task's thread was in their `fclose` until then, so a waiter
   of the task sees their bytes delivered (one relaxed load when there is
   none; the scheduler's own points wait the same way, item 11). A glue
   that does not call it lets a waiter see the value before the wait at
   the end of `run_task`, that is with the bytes still on their way. The
   calls:
   - `Task.spawn`/`IO.asTask`: `spawn(job, prio, keep_alive)`;
   - `Task.map`/`bind`, `IO.mapTask`/`bindTask`: when
     `dependent_runs_now(src, sync)` is true, apply `f` at once; otherwise
     `depend(src, job, prio, sync, keep_alive)`;
   - `Task.get`/`IO.wait`: if the slot holds the value, that; otherwise, if
     `in_sync_task()`, report the Lean panic `GET_IN_SYNC_TASK` (native's
     "`Task.get` called from a `(sync := true)` task", `wait_for`;
     `tasks/get_in_sync_task`), then `wait(id)` and read the slot;
   - `IO.getTaskState`: `state(id)`; `IO.waitAny`: `wait_any(ids)`;
   - `IO.cancel`: `cancel(id)`; `IO.checkCanceled`: `check_canceled()`;
   - `IO.getTID` in a task: `main`'s id plus `thread_number()`;
   - `Task.pure a` (`lean_task_pure`): no call, glue only. Natively it is
     a task object that holds `a` and has no task-manager state
     (`alloc_task(v)`, `object.cpp` 1180-1201: `m_value` set, `m_imp`
     null). The glue makes its task object with `a` in the slot and the
     id `TaskId::FINISHED`. It then behaves as every finished task:
     - `IO.getTaskState` is `finished`, with no polling point;
     - `Task.get` and `IO.wait` give `a` at once, with no
       `GET_IN_SYNC_TASK`, even in a `sync` task;
     - `IO.cancel` does nothing, and `IO.waitAny` takes it when it is
       the first finished task of the list;
     - a `sync := true` dependent runs at once (`dependent_runs_now` is
       true), and its result is again such a task; another dependent is
       queued at once (`depend` with `TaskId::FINISHED`), as Lean's
       `add_dep` enqueues it;
     - a bind function may return it: the bind task then finishes with
       its value (`task_bind_fn1`);
     - its drop calls nothing.

     A task the glue finished at once is the same: a `spawn` without the
     task manager, a dependent that `dependent_runs_now` applied. Cases
     `tasks/task_pure_graph`, `pure_get_in_sync_task` (a `Task.get` in a
     `sync` task, with no panic) and `cancel_promise_and_pure`.

   The job of a dependent holds its source's handle, as Lean's closures do.
   **When the last reference to an unfinished task goes, call
   `release(id)`, for every task, IO tasks included**: Lean's
   `deactivate_task`. It deletes a pure task that has not started; any other
   task runs to completion, but is marked unreferenced, so that its finish
   notifies nobody, as natively such a finish deletes the task
   (`m_deleted`) without `resolve_core`'s `notify_all` (review RS2-08 of
   sched-2; "Waiters wake after a walk"). A glue that skips `release` for
   IO tasks wakes waiters where native does not
   (`tasks/sync_walk_mutex_unref_finish`, `wait_any_unref_finish`).

   **The glue's slot comes first.** Once the slot holds the value, the task
   has finished, and its `TaskId` must not be passed to the scheduler again:
   the entry is reused by later tasks, and after 2^32 of them the generation
   too, so an old id could name a new task. Pass `TaskId::FINISHED` instead
   (as the source of `depend` or `dependent_runs_now`, in `wait_any`'s
   list), answer `state` and `Task.get` from the slot, and skip `release`
   and `cancel`. A finished task's id is also the fast path: no call at all
   (review RS1S-10).
4. **Promises.**
   - `IO.Promise.new`: `promise_new()`. Before the task manager runs it
     returns Lean's internal-panic message, which the glue reports.
   - `IO.Promise.resolve v`: `resolve(id, || store some v)`. Only the first
     resolution stores.
   - Dropping the last reference to an unresolved promise:
     `resolve(id, || store none)` (Lean's `deactivate_promise`).
   - `IO.Promise.result?` (`lean_io_promise_result_opt`): no call, glue
     only. Natively the promise owns one task object (`m_result`), and
     `result?` returns another reference to it (`object.cpp` 1347-1351),
     the same object at every call. The glue's promise holds its task
     object (the slot, of `Option α`, and `promise_new`'s id), and
     `result?` returns another reference to that object. Its states:
     - before the resolution, `IO.getTaskState` is `running`, since a
       promise's task has no closure (`get_task_state`, `object.cpp`
       1085); `wait` blocks, and dependents wait;
     - after `resolve v`, it holds `some v`, and a second `resolve`
       changes nothing;
     - after the drop of the unresolved promise, it holds `none`;
     - `IO.cancel` before the resolution cancels the dependents made
       meanwhile when it is resolved (`handle_finished`), `sync` ones too;
       `IO.waitAny` takes a finished task of its list first.

     Cases `tasks/promise_result_opt` and `cancel_promise_and_pure`.
   - `Promise.result!` is Lean code: `result?.map (sync := true)
     Option.getOrBlock!`. Its function, the private `Option.getOrBlock!`
     (`lean_option_get_or_block`), is `option_get_or_block(opt, report)`
     ("`Promise.result!` on a dropped promise" below).
5. **Yield points.**
   - `effect()` before output, flush, process spawn and `IO.Process.exit`.
     A Lean panic's message is output too (`lean_panic` prints it through
     Lean's stderr stream), so `effect()` comes before it.
   - `poll()` at clock reads.
   - `ref_read()` at `ST.Ref` reads.
   - `sleep_ms(ms)` for `IO.sleep` and `dbgSleep`.

   `IO.getTaskState` and `IO.checkCanceled` poll by themselves.
6. **`Std.Sync`.** Keep a `sync::Mutex`, `Condvar`, `RecursiveMutex` or
   `SharedMutex` in the translator's handle for `BaseMutex` & co.
   `Std.Channel`, `Barrier`, `Semaphore` and the rest of `Std.Sync` are Lean
   code over these and promises. They need nothing more.
7. **Waits of the glue's own objects.** A thunk being forced on another
   context: put `current_context()` in the thunk's waiter list, call
   `block_sync()`, and `wake(c)` each waiter when the value is stored. A
   thunk forced inside its own computation: `hang()` (lean-bugs LB-08:
   natively it spins forever; the other contexts go on).

   **A reference taken by `modify`** waits the same way. The semantics are
   Lean 4.35's (LB-01 and LB-18 in `docs/lean-bugs.md`):
   - `ST.Ref.modify` is `take`, then a store into the emptied reference
     (`ST.Prim.Ref.modifyUnsafe`), so the reference is empty while its
     function runs.
   - That function can block (a `Task.get` in it), and other contexts then
     run.
   - Only `modify`'s own store fills the empty reference. Until then `get`,
     `take`, `set` and `swap` wait: each is a blocking yield point. Register
     the context, `block_sync()`, and look again when woken. `set` is
     `swap` with the result dropped.
   - `modify`'s store fills the reference and wakes the waiters.
   - Without this, a reader sees the empty cell, a placeholder, at once.
   - A write (`set`, `swap`, `take`, `modify`) first calls
     `before_publish()`: the context's handed-off streams end before
     another context can see the write (item 11; one relaxed load when
     there is none).
   - The cost, as in 4.35: a `modify` whose function waits for a task that
     uses the same reference deadlocks.
   - A Lean panic in modify's function returns its default, so the store
     still runs. A Rust panic ends the process, and the glue must not catch
     it and leave the cell empty.

   Native 4.34.0 differs, where its reference is shared with a task
   (multi-threaded). `get` and `take` spin while it is empty, as here
   (`io.cpp` 1459-1500). But `set` stores into the empty slot and is then
   overwritten by `modify`'s store (LB-01), and `swap` returns its own
   argument, one object with two owners (LB-18).

   Cases: `refs/get_during_modify` (the read waits for `modify`'s store, as
   natively), `refs/set_during_modify` (LB-01) and `refs/swap_during_modify`
   (LB-18). The driver's `Ref` (`tests/sched-driver/src/lean.rs`) is an
   example.
8. **Stack overflow.** Lean's report (`src/runtime/stack_overflow.cpp`) is
   a SIGSEGV handler on an alternate signal stack. Installing one
   (`sigaction`) is `unsafe`, so it stays in the glue, as in lean2rr's
   `rt.rs`. The handler reports `\nStack overflow detected. Aborting.\n` and
   aborts when the fault lies in the guard page of the thread's own stack,
   or of the running context's stack (`running_stack()`: the faulting
   thread's own thread-local atomics, async-signal-safe).
   `tests/sched-driver/src/glue.rs` has one.
   - Without such a handler, a task that overflows its context's stack ends
     with a plain SIGSEGV, status 139, without Lean's message. Rust's own
     handler knows only the guards of threads, not of coroutine stacks.
   - The driver's handler looks up `main`'s guard on the process's main
     thread, where its `main` runs. A glue whose `main` runs on a spawned
     std thread (leanrs's does) must look up that thread's guard instead,
     and pass every other fault on to the handler it replaced (Rust's). Rust
     then reports an overflow of a std thread's stack as it does without
     the glue.

9. **Blocking IO** needs nothing from the glue when it goes through the
   crate's `io` (handles, processes): it cooperates by itself. A stream's
   `StreamGuard` (`Handle::file()`) may be held across any wait: every
   switch records it as held by the suspended context, and another context
   that wants the stream waits for it (review RSIO-01; the one exception is
   a guard taken before the program's first task, promise, timer or watch,
   "Stream locks" above). A blocking call the glue makes on its own (user C
   code over a descriptor, a translator's own IO) waits first with
   `sched::wait_fd(fd, interest)` or `sched::poll_fds(items, timeout)`,
   which are plain `poll(2)` when `io_cooperative()` is false; any state of
   its own that another context may need, it releases first. At exit, a
   stream whose guard a suspended context holds across such a wait is
   treated as held by a writer (the crate cannot tell what the glue waits
   for): the exit's flush waits for the guard, cooperatively, and waits for
   good if the context never lets it go (LB-29 skips only a holder blocked
   in the crate's own read). A glue drops its guards before a wait that may
   last.
10. **The event loop's callbacks** (the UV externs, the network, `net`):
    `timer_start(deadline, callback)` and `timer_stop`, and `watch(fd,
    interest, callback)`, `watch_modify(id, interest)` and `unwatch(id)`
    ("The watch API" above). The callbacks run on the loop context and may
    resolve promises (`resolve`), whose `sync` dependents run there. The
    glue gives `watch` its own reference to the descriptor (a clone of its
    `Rc<OwnedFd>`), which the reactor holds until `unwatch`.
11. **Free and drop paths: the no-suspend scope.** Dropping a handle's
    last reference closes its stream, and the flush of its pending output
    may wait for a full pipe: a cooperative write would suspend the
    context right there, inside the translator's free (review RSIO-03), and
    lean2rr's runtime must never suspend inside a free. Both translators
    mark their free and drop paths (leanrs: every walk of its drop
    worklist) with `sched::enter_no_suspend()` and `leave_no_suspend()`, or
    the guard `sched::no_suspend()`: a thread-local counter, nestable, with
    no lock and no allocation, read only where a cooperative call would
    wait. In the scope:
    - **a dropped stream's flush never waits** (review RSIO-09): it writes
      what the descriptor takes without blocking (`pwritev2(RWF_NOWAIT)`
      for pipes and sockets; `PIPE_BUF` bytes to a FIFO, or the whole rest
      to a terminal, once `poll(2)` says it is writable). If the descriptor
      would block, the rest of the bytes and the descriptor are handed to
      an internal writer thread (below), and the drop returns at once. So a
      pipe whose reader is a task of the same program gets every byte, as
      natively. Streams of regular files flush as usual;
    - the io layer's other waits are plain system calls that block the
      thread;
    - a stream that a suspended context holds, needed in the scope, is a
      panic with the reason, not a silent deadlock;
    - the scheduler's own waits (a promise, a sleep, a `Std.Sync` lock) are
      not affected, and a context that waits inside its scope does not put
      the others in it: every switch sets the depth aside and gives it back
      when the context goes on (review RSIO-10).

    **Leaving the scope never suspends** (AR-8, which replaces RSIO-14's
    close at the outermost leave): `leave_no_suspend` (or the guard's drop)
    only decrements the counter, so a drop walk may end anywhere, in any
    `Drop` and during a panic's unwinding.

    **The hand-off of a dropped stream** (`io::coop::hand_off`, AR-8;
    reviews RFX1-01 to RFX1-03). Natively the drop's `fclose` blocks its
    thread until the pipe takes the last bytes. Here the bytes (a
    `Vec<u8>`) and the descriptor go to a writer thread of their own,
    `lean-runtime-close`, which makes the blocking writes (as glibc's
    `fclose` makes them: until every byte is written or a write fails,
    `EPIPE` once the reader has gone) and closes the descriptor. The drop
    never suspends, never waits for a stream lock, and needs no later
    scheduling point, so the bytes reach the child whatever the dropping
    context does next (reads a handle a task drains, polls
    `Child.tryWait`, waits in the glue's `block_sync`). It is an internal
    helper, not parallelism a Lean program can see: the thread holds plain
    data only, never a Lean value, and runs no Lean code. One thread per
    hand-off, started then, so a pipe that never drains blocks no other
    stream's close. Where no thread can start (`EAGAIN`), the dropping
    thread writes and closes itself, blocking as the plain `fclose` does:
    no panic, no byte lost.

    **Who waits for a writer** (reviews RFX1-07, RFX1-09, RFX1-12; leanrs's
    re-check of a771e57). Each hand-off records the context that dropped
    the stream, whose thread natively would still be in `fclose` until the
    writes end, unable to do anything another context could see:
    - that context waits for its writers at every point where it publishes
      or may suspend (`sched::writers_point`): its effect points (output,
      flush, process spawn, `IO.Process.exit`), polls, sleeps, waits
      (`wait`, `wait_any`, `hang`), promise resolutions, task creations
      (`spawn`, `depend`), `Std.Sync` operations (a lock, an unlock, a
      `Condvar` wait or notify; so `Std.Channel` and the rest of `Std.Sync`
      too), the glue's reference writes (`before_publish`, item 7), the end
      of a task's job (`before_task_value`, item 3, and `run_task`) and of
      `main` (`finish`), `IO.cancel`, the entry of every stream lock (so a
      write through another descriptor of the same pipe comes after the
      handed-off bytes), of `flock` and of `Child.wait`, and the entry of
      every io call with an effect outside the process (`io::effect_point`:
      a child spawned or killed, `IO.Process.output`, a file or directory
      created, removed, renamed or changed, `Handle.mk`, a temporary file
      or directory, the working directory, the environment, the process
      title or priority; `net`'s connections, binds, listens, sends,
      shutdowns and multicast memberships). So a context that
      learns of the drop through any of these (a promise, a lock, a
      reference, the task's value, the pipe itself, the child's fate) sees
      every byte delivered (`rfx2_causal_handoff`, `rfx3_promise_handoff`,
      `process/handoff_then_resolve`, `process/handoff_then_write`,
      `process/handoff_then_kill`, `rfx4_fs_signal`). Not at
      the glue's `block_sync`: the glue has registered the context as a
      waiter by then, and a wait there could lose its wake-up.
      Not while the context holds a stream lock (the wait could need that
      stream, review RFX1-02's shape), in a no-suspend scope, or while a
      panic unwinds: the next point waits instead. The writer is a thread,
      so its pipe drains without the context, and a context that reaches no
      point (one that only polls `Child.tryWait`) never stops it;
    - its exit (`IO.Process.exit`, an internal panic, `forceExit`) waits for
      its writers first, before standard output's flush;
    - other contexts' writers still running at an exit are not waited for:
      natively glibc unlinks a stream from its list before `fclose` flushes
      it, so `exit` never waits for another thread's `fclose` in progress
      (`rfx2_exit_unrelated_handoff`: exit at once, as natively in 0.31 s).

    The wait is the scheduler's: the waiting context looks again every 1
    to 16 ms, woken by the event loop's timers (so it sees a writer's end
    up to 16 ms late; review RFX1-20), and the other contexts run
    meanwhile; with no other context, or off the scheduler's thread, the
    thread waits on a condition variable the writers notify. It takes no
    descriptor: each writer thread removes its own entry when it has
    written and closed, so a hand-off leaves nothing open after its writer
    (review RFX1-19).
    So a task of this program that drains the
    writer's pipe goes on, as natively the program's other threads ran
    while the drop's `fclose` waited: other contexts run briefly at a job's
    end and during the exit, also during `forceExit`'s wait
    (`rsio_exit_join`, `rfx2_exit_handoff`). An exit off the scheduler's
    thread (the drain's out-of-memory end) or in a no-suspend scope waits
    plainly; a job's end in a no-suspend scope does not wait (a later end,
    or the exit, does). An abort (`LEAN_ABORT_ON_PANIC`) waits for nothing,
    as native's `abort` flushes nothing. Only running writers cost
    anything: a count of them, which each writer lowers when it ends, is the
    one relaxed load at every point (review RFX1-16). An exit must not come
    from a context that holds a stream's guard (`Handle::file()`): that
    stream is skipped, its pending output unwritten (review RFX1-17).
    Limits:
    - a later write to the same pipe through another descriptor may still
      overtake the handed-off bytes where the dropping context does not
      wait first: a write by another context, a write made in a no-suspend
      scope or while the context holds another stream's lock (where its
      wait is put off), a child's write; natively the drop's `fclose` had
      finished first;
    - an effect made inside the same no-suspend drop walk as a hand-off
      cannot wait for its writer, and may overtake the handed-off bytes
      (review RFX1-21): a later drop in the walk whose flush succeeds and
      closes its own pipe, a socket closed by its `Drop`, a pure thunk that
      drops a handle and whose value the glue then stores. Only the order
      between channels changes; no byte is lost. A glue calls
      `before_publish()` where it stores a thunk's value, outside the walk;
    - the `errno` of a failing write or close in the writer is the
      writer's own: natively a failing `fclose` at the drop sets the
      program's `errno` (review RFX1-06), which a later `getLine` on a
      stream with its error indicator set could report;
    - the handed-off descriptor itself stays open until its writer has
      finished, where natively the drop closed it (reviews RFX1-13,
      RFX1-19; inherent to the hand-off, and the only descriptor it
      holds): its number is not free for reuse at once (a program near its
      descriptor limit can get `EMFILE` sooner than natively), and a
      `flock` lock on it is released only then (a context waiting in
      `flock` looks again within 16 ms); once the writer has ended,
      nothing is left open (`rfx4_fd_after`);
    - where no writer thread can start, the dropping thread writes in
      place and blocks; if the pipe's reader is a task of this program,
      that task cannot run, and the program waits for good (review
      RFX1-11; natively the reader's own thread would drain it). Writer
      threads take a 64 KiB stack (the system's minimum where larger), so
      this needs the thread limit or the memory exhausted.

    Cases: `rsio_drop_no_suspend`, `rsio_ns_*`, `rsio_ns_unwind`,
    `rsio_ns_force_exit`, `rsio_exit_join`, `rfx1_shared_read`,
    `rfx1_trywait`, `rfx2_*` in `tests/sched-driver`; the unit tests of
    `io::coop` (the hand-off into a full pipe; no thread to start).

    **Promise walks and `sync` dependents run outside the scope.** Dropping
    the last reference to an unresolved promise resolves it
    (`deactivate_promise`), and the walk runs its `sync` dependents, user
    code that may do IO and wait. A translator resolves dropped promises
    (`sched::resolve`) after its drop walk has left the scope, so that this
    code runs with the cooperative IO it would have anywhere else.

### `Promise.result!` on a dropped promise

`Option.getOrBlock!` (`lean_option_get_or_block`, `io.cpp` 1639-1651)
returns the value of `some`. On `none` it calls
`lean_panic("PANIC: Promise.result!: promise has been dropped without ever
being resolved", force_stderr = true)`, then sleeps forever
(`sleep_for(seconds::max())`), "only reachable when using non-fatal
panics". `sched::option_get_or_block(opt, report)` does the same:
- `report(PROMISE_DROPPED)` is the glue's report, by the plan
  `semantics::panic::lean_panic_plan(settings, true)`:
  1. `effect()`, as for any output;
  2. the lines on the process's stderr (`PanicStream::ProcessStderr`):
     C's `stdout` is flushed first, as `std::cerr` is tied to it, and
     Lean's current stderr is not used, so `IO.setStderr` does not catch
     them (`force_stderr`, `panic_eprintln`, `object.cpp` 131-138);
  3. the abort (`LEAN_ABORT_ON_PANIC`) or the exit (exit-on-panic) the
     plan says. `report` returns only for `PanicEnd::Return`.
- then the waiters of the tasks whose walks are in progress on the context
  wake, `result?` among them (LB-32, "Waiters wake after a walk" above);
- then the context waits forever (`hang`), while the others go on.

The value is `none` only after the promise was dropped unresolved. So the
function runs in the walk of `deactivate_promise`, on the context that
dropped the promise, and that context hangs: on `main`, the program hangs;
on a task, the program goes on, but the final run waits for that context,
so the process never exits. A `result!` task dropped before its promise is
deleted (`release`) and never runs. On a promise resolved before `result!`,
the map runs at once and the task is `Task.pure` of the value
(`lean_task_map_core`).

The cases, recorded natively (5 runs each; the two LB-32 cases expect the
correct outcome and keep native's in their `native` field) with twins in
the driver:

| Case | What the program does | Native |
|---|---|---|
| `tasks/result_bang_some` | `result!` of a promise resolved before, then by `main`, then by a dedicated task | the three values; the first task is finished at once |
| `tasks/result_bang_dropped` | prints `before` to `stdout`, then `main` drops the promise | `before` (flushed by the panic), the panic line, then a hang |
| `tasks/result_bang_dropped_in_task` | a dedicated task drops the promise; `main` waits, runs another task, prints, returns | the panic line, then `main`'s and the other task's lines on stderr; the exit hangs, so `main done` on `stdout` is never written |
| `tasks/result_bang_dropped_abort` | `LEAN_ABORT_ON_PANIC=1`, Lean's stderr set to a buffer, then the drop | `before`, the panic line on the process's stderr, status 134 |
| `tasks/result_bang_dropped_first` | the `result!` task is dropped before its promise | no panic |
| `tasks/result_bang_dropped_redirected` | as `result_bang_dropped`, Lean's stderr set to a buffer first, no abort | the panic line on the process's stderr (with `LEAN_ABORT_ON_PANIC`, every panic goes there, so only this case tells the streams apart) |
| `tasks/get_in_sync_task_redirected` | the contrast: `Task.get` in a `sync` task, a `lean_panic` without `force_stderr`, Lean's stderr set to a buffer | the line in the buffer |
| `tasks/result_bang_dep_order` | dependents of `result?` made before and after `result!`, then the drop | the walk, newest first, queues the later one, then hangs in `result!`: the earlier one never runs |
| `tasks/dropped_promise_waiter_wakes` | a task blocked in `IO.wait p.result?` when the drop's walk hangs in `result!` | natively the waiter never wakes; here it wakes with `none` (LB-32) |
| `tasks/dropped_promise_waiter_unrelated_finish` | the same, with an unrelated task that finishes 1 s in | natively that finish wakes the waiter, 800 ms late; here it wakes at the drop (LB-32) |

## Why `Glue::suspend` is sound

This section is self-contained: with it and the crate's source
(`src/sched/ctx.rs`, plus corosensei 0.3.4's source), a reader can check
the glue's dereference.

### What the glue does

The glue's only `unsafe` operation is in the body of `Glue::suspend`:

```rust
unsafe { (*s.yielder()).suspend(()) }
```

`s.yielder()` is a `*const corosensei::Yielder<(), ()>`. Dereferencing it is
sound if, for the whole call:
- (P1) it points to the `Yielder` of a live corosensei coroutine;
- (P2) that coroutine is the one running on the current stack, and the
  call happens from its own stack.

`Yielder::suspend` then switches back to whoever resumed the coroutine, as
corosensei's safe API does, and returns when the coroutine is resumed.

### Where the pointer comes from, and why it does not move

- corosensei calls a coroutine's function with `y: &Yielder` (its
  `coroutine_func`, `src/coroutine.rs`). A `Yielder` is
  `#[repr(transparent)]` over the coroutine's *parent link*.
- The parent link is a word at a fixed place: the second word below the
  base of the coroutine's stack, reserved by `init_stack`
  (`src/arch/aarch64.rs`, and the same layout in `src/arch/x86_64.rs`).
  corosensei writes the parent's stack pointer there at each resume.
- The stack is a fixed `mmap`ping (`DefaultStack`), unmapped only when the
  `DefaultStack` is dropped. Moving the `Coroutine` value moves no stack
  memory.

So `y` has one address for the coroutine's whole life.

### The invariants the crate maintains

Each invariant names the code that establishes it.

- **S1. Where the pointer is stored.** A context's yielder pointer is the
  field `Ctx::yielder` (`src/sched/ctx.rs`), private to that file. It is
  written in exactly three places:
  - `Ctx::new` makes it null, for `main`'s context (`Contexts::new`) and for
    each new context (`Sched::start_context`, called by `start_worker` and
    by the event loop's `ev_start_loop`, before its coroutine first runs);
  - the coroutine's function, built in `Sched::start_context`, stores `y`
    as its first statement, before it calls its entry, `worker_main` or the
    event loop's `loop_main` (the only code that runs on the context);
  - `Sched::after_resume`: when the coroutine has returned, the field is set
    back to null, the stack goes back to the pool, and the slot is freed.

  It is read in two places, `block` and `yield_now` (S3).

  `main`'s context (index `MAIN`, built in `Contexts::new`) never has a
  coroutine, and its field stays null. A slot is reused only after
  `after_resume` freed it, and its new coroutine stores its own pointer
  first. So the field of a context with a live coroutine is that
  coroutine's own `y`, and the code running on it never sees a null or a
  stale pointer.
- **S2. Which context is running.** `cur` is the running context. It is set
  to `n ≠ MAIN` in one place, `Sched::enter`, which `hub` calls right
  before `co.resume(())`. It is set back to `MAIN` right after `resume`
  returns:
  - normally, by `Sched::after_resume`;
  - on a panic, by `PanicGuard::drop`.

  Between `enter` and the actual switch, and between the switch back and
  `after_resume`, only `publish` and corosensei's own code run. No
  scheduler function runs there. So code that runs while `cur == n` runs on
  `n`'s stack, and code on `main`'s stack sees `cur == MAIN`.
- **S3. The only call site.** `Glue::suspend` is called in one place,
  `switch_away` (`src/sched/ctx.rs`):
  - it calls it only when `cur != MAIN`, so never on `main`'s context;
    with `cur == MAIN` it runs the hub instead;
  - its pointer comes from `block` or `yield_now`, which read
    `ctxs[cur].yielder` in the same borrow that records the context as
    blocked or able to run;
  - it asserts that the pointer is not null.

  `Suspend` has private fields and only `switch_away` makes one, so the
  glue cannot call its own `suspend` with another pointer. The lifetime
  parameter of `Suspend<'_>` is documentary: the types do not tie it to
  anything, and `yielder()` returns a raw pointer that can be copied out.
  Not keeping the `Suspend` or the pointer is the glue's duty ("What the
  glue must guarantee"), not something the types enforce. By S1 and S2 the
  pointer is the running coroutine's own, and the call is made from that
  coroutine's stack: P1 and P2 hold.
- **S4. Hooks run on `main`'s stack and cannot switch.** The hub runs
  `Glue::switched` on `main`'s stack, inside `hub_hook`. Its wait when
  nothing can run (`reactor::idle`, the event loop's `epoll_wait` or a
  sleep) calls no glue code and never blocks a context.
  `block` and `yield_now`, the only callers of `switch_away`, first assert
  that no hub hook is running (`not_in_hub_hook`). So no hook can reach
  `switch_away`, whatever it calls. The refusal is a panic in a hook, so
  the process aborts (S6). The test `a_hub_hook_cannot_block` checks it, in
  a child process. `task_begin` and `task_end` run on the task's own
  context, where blocking is fine.
- **S5. No forced unwinding.** corosensei unwinds a suspended coroutine
  only when it is dropped while suspended (or on `force_unwind`, which the
  crate never calls). A forced unwind would make `Yielder::suspend` return
  by unwinding. The crate never drops a suspended coroutine:
  - a coroutine leaves the context table only in `Sched::enter`, right
    before `resume`;
  - when `resume` returns, `after_resume` either puts it back (it
    suspended) or turns it into its stack (`into_stack`: it returned);
  - a panic out of `resume` means the coroutine completed;
  - at thread exit, `Contexts::drop` forgets the suspended coroutines
    rather than dropping them;
  - if a hook panics before `enter`, the coroutine is still in the table,
    which is never dropped with live coroutines.

  **The one stretch that must not panic** is in `hub`, from `Sched::enter`
  to the end of `Sched::after_resume`, where the coroutine is held outside
  the table. In it run only `publish`, `resume`, and `after_resume`'s
  bookkeeping (index arithmetic on the table, a push on the stack pool,
  debug assertions). `after_resume` puts a suspended coroutine back into the
  table as its last step. As a second line of defence, the coroutine is
  held there by `Parked` (`src/sched/ctx.rs`), whose `Drop` forgets a
  coroutine that is still suspended instead of dropping it, so even a panic
  in that stretch would not force-unwind it.

  `PanicGuard` does not cover `after_resume`: `hub` forgets the guard as soon
  as `resume` returns. S2 and S5 still hold there. `after_resume` sets `cur`
  back to `MAIN` as its first statement, and `Parked` keeps a suspended
  coroutine from being dropped until the last one. The only code there that
  can panic is a debug assertion, and an `expect` on a coroutine that
  `Parked` holds by then, which cannot fire.

  `IO.Process.exit` called from a task on a context runs glibc's `exit` on
  that context's stack, and with it the thread's TLS destructors.
  - The scheduler's own, `Contexts::drop` and `Tasks::drop`, only forget
    suspended coroutines and pending jobs.
  - The running coroutine is not in the table: `hub`'s frame on `main`'s
    stack holds it, and `exit` never unwinds that frame.

  So nothing is unwound or freed under the running stack
  (`adv_exit_from_task`: status 3, no AddressSanitizer report).

  So `Yielder::suspend` always returns normally, when the hub resumes the
  context.
- **S6. Panics.** A Rust panic on context `n` unwinds `n`'s frames up to
  corosensei's catch at the base of the coroutine (`catch_unwind_at_root`;
  feature `unwind`, on by default). A frame of `Glue::suspend` is on `n`'s
  stack only while `n` is suspended, and a suspended context runs no code.
  So a panic never unwinds through an active `Yielder::suspend` call, S5
  ruling out forced unwinds. If the glue's `suspend` body panics before
  calling `Yielder::suspend`, that is an ordinary panic on `n`.

  A suspension can happen *during* unwinding, though. While a panic unwinds
  `n`, a destructor can reach the scheduler and block, for example:
  - dropping the last reference to a promise resolves it (`resolve`);
  - `walk_loop` then runs its `sync` dependent there and then;
  - the dependent waits for a task still running (`wait`), so `n`
    suspends through `Glue::suspend`.

  This is an ordinary call from `n`'s own stack while `n` runs, so P1 and P2
  hold as for any other suspension. The landing pad is suspended and
  resumed like any frame; once `n` resumes, the unwinding goes on to the
  coroutine's base. The test `adv_block_in_drop_during_unwind` checks it
  (status 101, the dependent's lines printed).

  A second panic raised there is a panic in a destructor during unwinding,
  and Rust aborts the process, as in plain Rust
  (`adv_panic_in_sync_dep_of_drop`: status 134). With `panic = "abort"`
  nothing unwinds, so neither case arises.

  `resume` resumes the caught panic on `main`'s stack. `PanicGuard::drop`
  then sets `cur` back to `MAIN` and marks `n` dead; `main`, which was
  blocked or letting others go first in the hub, runs again and waits for
  nothing (review RS1S-04; test `a_caught_context_panic_leaves_main_usable`).
  A dead context is never resumed (`Sched::wake` and `hub_step` select only
  blocked or able contexts), and its slot is never freed or reused, so its
  stale pointer is never read.

  A glue that catches the panic can go on, without what the panic unwound
  (review RS1S-12):
  - A task whose job the panic unwound stays unfinished, as the tasks of a
    dead context do, and its waiters wait forever. That holds on `n`, and on
    `main` for a task `main` ran inline. The guard `Unwound` in `run_task`
    drops the task from its context's running tasks, frees the lone worker
    if it was its task, and calls `Glue::task_end`. Test:
    `review2_caught_panic_through_an_inline_task`.
  - A walk of dependents the panic unwound ends there (the guard
    `WalkUnwound` in `walk_loop`). Its dependents not walked yet are
    queued, `sync` ones included, and run later as other tasks do. Test:
    `a_caught_panic_in_a_sync_dependent_ends_its_walk`.

  A panic in a hub hook (`switched`) cannot go on: it would unwind
  `hub` with a context half switched. `hub_hook` aborts the process
  instead, after Rust's message.
- **S7. The pointer is not used after the coroutine's function returns.**
  After the function returns, no code runs on `n` any more. By S1 its field
  is null from `after_resume` on, and nothing reads it before then.

### What the glue must guarantee

- **The only dereference is in the body of `Glue::suspend`,** once per
  call, as `(*s.yielder()).suspend(())`, and nothing else happens in that
  body.
- **The pointer is not stored, copied out or used anywhere else**: not in a
  field, not in a thread-local, not in a closure that outlives the call.
- **Scheduler functions are called only from stacks the scheduler knows**:
  `main`'s thread stack and its contexts. Not from:
  - a signal handler;
  - another coroutine or stack switch of the translator's own;
  - another thread (the scheduler is thread-local, so another thread has a
    scheduler of its own, which never holds this thread's pointers).

  A blocking call from a foreign stack would run while `cur` names a
  context whose stack is not the current one, which breaks P2.
- **`switched` does not block or yield** (S4 enforces it with a panic).

### Checklist for glue authors

1. `suspend` is exactly `unsafe { (*s.yielder()).suspend(()) }`.
2. No other code reads `s.yielder()`, and the `Suspend` value is not kept.
3. No scheduler function is called from a signal handler, another thread,
   or a stack the translator switched to itself.
4. `switched` only saves and restores state.
5. `start` is called on the thread that runs `main`, and every later call
   is made on that thread.
6. A `TaskId` is passed to the scheduler only while the glue's own slot for
   that task is empty (item 3 of "The glue"); a finished task's id becomes
   `TaskId::FINISHED`. This is not about soundness, but about naming the
   right task after 2^32 tasks.
7. `get`, `take`, `set` and `swap` of a reference that `modify` has taken
   block until `modify`'s own store (item 7 of "The glue"; LB-01, LB-18).
   This is not about soundness either, but about Lean's semantics.

### How it is checked

Miri cannot run corosensei's stack switch (inline assembly). So the unit
tests that create a context are `#[cfg_attr(miri, ignore)]`, and the
argument is checked by:
- **Review** of S1-S7 against the code they name. The field has three
  writers and two readers, all in `src/sched/ctx.rs`, and `Glue::suspend`
  has one caller.
- **Runtime assertions in the default build:**
  - `switch_away` refuses a null pointer;
  - `not_in_hub_hook` refuses a switch from a hub hook (S4);
  - corosensei refuses to resume a completed coroutine.
- **The driver's integration tests** (`tests/sched-driver`), on real
  hardware: the program cases of `tasks/`, `sync/`, `refs/`, `taskio/`
  and `uvloop/` and the io cases with tasks, the review regression
  programs (`rsio_*`), a panic test,
  and leanrs's three adversarial checks (`adv_*`: blocking during
  unwinding, a second panic there, `process::exit` from a context), in
  debug and release builds, on both toolchains (`scripts/check.sh`). Every
  case with contention suspends: the mutex and condition-variable cases,
  the promise waits, the sleeping tasks of `sync_dependent_order`, and
  every IO wait of the sched-io cases (from a context and from `main`). The
  panic test unwinds from a context into `main` (S6). A wrong pointer there
  would crash or corrupt the output, which is compared byte for byte with
  native Lean's.
- **AddressSanitizer**, run by hand (not in `check.sh`). The driver's
  feature `asan` turns on corosensei's `sanitizer` feature, whose fiber
  annotations tell AddressSanitizer about every stack switch:
  ```
  RUSTFLAGS=-Zsanitizer=address ASAN_OPTIONS=detect_leaks=0 \
    cargo test --offline --target aarch64-unknown-linux-gnu -p sched-driver --features asan
  ```
  All 34 of the driver's tests passed at d0d6a0d (2026-10-04, after the
  third review of sched-1; 31 of 31 at 089fbc4 by its reviewer); the three
  cases added in the fourth review, sched-io's fourteen and sched-2's
  twenty-four have not run under it yet. Leak
  detection is off because the driver's glue leaks its 64 KiB alternate
  signal stack on purpose; that leak was the only report.

  What AddressSanitizer covers is limited. It checks the stack bounds at
  each switch and accesses to freed heap memory. A stale yielder pointer
  into a pooled stack, which stays mapped, or into a reused slot is
  ordinary mapped memory, which it does not flag. So S1 and S7 rest on
  review, and on the null assertion in `switch_away`.

### Alternatives ruled out

The dereference sits in the glue because nothing safe gives deep code the
running coroutine's yielder. These were considered:
- **Threading the yielder through the translated program.** Pure code
  blocks too (`Task.get` of a task running on another context), and
  lean2rr's Reussir code cannot carry a Rust reference.
- **A scoped thread-local holding the yielder** (`scoped-tls`). Its safe API
  assumes scopes that nest; coroutines interleave them, and a restored
  pointer dangles once its coroutine has ended.
- **OS threads passing a baton** instead of coroutines. Jobs hold
  non-`Send` values (`Rc`), so moving them to another thread needs
  `unsafe impl Send`.
- **Other crates** (leanrs's survey, 2026-10-03):
  - corosensei has no API to suspend from nested code (its issue #71,
    closed as not planned);
  - generator 0.8.10 has an unsound safe API: memory corruption was
    reproduced from safe code, and a drop while panicking frees the stack
    without running destructors. It also has RUSTSEC-2019-0020 and
    RUSTSEC-2020-0151 and open Miri reports of undefined behaviour, and it
    installs a process-wide SIGSEGV handler and panic hook;
  - may and context have `unsafe` constructors;
  - stackful uses 2 MiB stacks, where Lean's threads have 1 GiB.

The owner approved this design (2026-10-03): "rare justified unsafe is
fine, but need to be documented well".

## Differences from lean2rr's runtime

| lean2rr's leanrt | Here | Why |
|---|---|---|
| A task is a Reussir cell; the runtime hands tasks to generated code by tag | A task is a boxed `FnOnce` and the translator's slot | Neither translator's representation |
| Dropped pure tasks are found by reading cell counts when a task would start (`droppable_in`, pins) | `release` at the last reference, as `deactivate_task` | The same moment as Lean's, and no search |
| Pure tasks start on contexts like IO tasks | Started pure tasks run later ("The pure-task rule") | `runaway_pure_task_started` |
| Hand-written stack switch, any context to any other | corosensei; every switch goes through `main`'s stack | O2: no hand-written assembly |
| Stacks reserved with `MAP_NORESERVE`, released with `madvise` when pooled | corosensei's `mmap(PROT_NONE)` and `mprotect`; up to 8 pooled stacks keep their touched pages | No `unsafe` in the crate (see the checklist) |
| `checkCanceled`, clock and ref reads do not yield | They are polling points; ref reads only with `set_ref_read_yields` | Decisions Q5 refinement B |
| Walks of promises dropped inside a Reussir free (`later`) | None: a translator resolves promises where its values are dropped | Reussir's free stack |
| The event loop (`net`) | The scheduler's own (`src/sched/reactor.rs`, sched-io): epoll, timers and watches; blocking IO cooperates | The glue has no hook; the network builds on it |
| `persist` (the walk of `lean_mark_persistent`) | Not yet | Batch scope |

Lean's panic for `Task.get` inside a `sync := true` task, which lean2rr does
not reproduce, is the glue's: `in_sync_task()` tells it when to report it
(`tasks/get_in_sync_task`).

The limits of one thread (lean2rr plan §10, "Tasks") hold here too:
- a context that computes without blocking or a yield point delays the
  others;
- `IO.waitAny` does not pick the fastest of several running tasks;
- reads, writes, `flock` and `waitpid` cooperate (sched-io, "Blocking IO
  and the event loop"), but a few blocking calls still block the whole
  thread:
  - `open(2)` of a FIFO waits for its other end (natively the opening
    thread waits): a FIFO whose two ends are opened by two tasks of one
    program hangs here;
  - a translator's own blocking calls outside the crate, unless its glue
    waits with `poll_fds` first;
  - a read of a descriptor that another process reads too can find its
    data gone between the readiness and the `read(2)`, and then blocks, as
    every reader natively would;
- where cooperation cannot see an event, it looks again: another process's
  `flock` release within 16 ms, a child without a pidfd within 16 ms;
- a write to a terminal is one `write(2)` once it is writable (256 bytes
  free), so it can block the thread until the terminal drains, as natively
  the writing thread blocks;
- in a no-suspend scope (item 11 of "The glue") every io wait blocks the
  thread, but for a dropped stream's flush, which is handed to a writer
  thread when it would block.

## The checklist of decisions Q5

- **Non-`Send` closures.** `Coroutine::with_stack` needs no `Send`; jobs
  hold `Rc` values in every test.
- **Stack size.** 1 GiB, or `LEAN_STACK_SIZE_KB` rounded down to 4 KiB plus
  128 KiB, as Lean's `lthread`. `start` reads it, and the size is rounded up
  to 64 KiB. corosensei's `DefaultStack` maps it `PROT_NONE` and makes all
  but one guard page writable with `mprotect`. That charges the commit
  accounting as an ordinary writable mapping does, where lean2rr passes
  `MAP_NORESERVE`:
  - with overcommit mode 0 (this host) each 1 GiB stack passes the
    heuristic check, and 200 of them were mapped in a probe;
  - in mode 2 both are refused alike (the kernel ignores `MAP_NORESERVE`
    there);
  - pages are touched only as used.
- **Stack overflow.** corosensei's `Stack` trait exposes `base()` and
  `limit()`, the guard page included, and `DefaultStack` maps exactly one
  guard page below the usable part. The scheduler publishes the running
  context's guard and top in thread-local atomics (`running_stack()`),
  updated at every switch, and none on `main`'s context.
  - The handler and the abort stay in the glue (item 8 of "The glue").
  - The driver's handler (`glue.rs`) reports Lean's message for a fault in
    `main`'s guard or the running context's.
  - `tasks/stack_overflow_in_task` matches native: the task overflows on a
    context, `\nStack overflow detected. Aborting.\n`, status 134, stdout
    not flushed.
  - corosensei's own trap API (`CoroutineTrapHandler`) is `unsafe` and not
    needed.
- **Rust panics.** With unwinding (`panic = "unwind"`, Rust's default),
  S6: a panic in a context goes on as a panic of `main`: Rust's message,
  then whatever the glue does with a panic in `main` (status 101 in the
  driver). The driver test `a_rust_panic_in_a_context_goes_on_in_main`
  checks it. A glue that catches it goes on without the tasks and walks the
  panic unwound (S6). A panic in `switched` or `idle` aborts. With
  `panic = "abort"` (leanrs's builds), a panic anywhere, a context
  included, prints Rust's message and aborts at the panic site: status 134,
  nothing unwound, no buffered stdout written. Suspended contexts are never
  unwound (S5): at thread exit their coroutines are forgotten, since their
  frames hold Lean values whose destructors would call back into the
  scheduler.
- **Polling yield points.** Task-state queries, `checkCanceled`, clock reads
  (`poll`), and `ST.Ref` reads in programs with tasks (`ref_read`): every
  `REF_READS_PER_POLL`-th read (1000) on a thread is a polling point, the
  first one included. A loop that polls a reference set by another context
  still reaches a polling point every 1000 reads, so it ends; the reads in
  between cost a thread-local countdown (review RS1S-03). A polling point
  costs a thread-local access and a few comparisons when nothing else is
  pending (O12, below).

## Toward real threads

The owner's direction (2026-10-03): parallelism will be supported later.
The model stays single-threaded for now, and nothing here should block
real threads.

**The one helper thread pair today: DNS lookups (`net`).** `net::dns` runs
glibc's `getaddrinfo` and `getnameinfo` on two threads of its own, started
by the first lookup (never at startup), as libuv runs them on its thread
pool. They are an internal helper, not Lean-visible parallelism: they get
plain data (a host, a service, a family, an address), wait in the C
library, send plain data back (addresses, names, libuv's error code), and
wake the loop through the loop's eventfd. No Lean value, `Rc` or scheduler
state crosses to them; the promise is resolved on the loop context, on the
scheduler's thread, like every other callback. At exit the process waits
for the lookups in progress and drops their answers, as native's libuv
joins its thread pool (`docs/net.md`).

**Already per thread.**
- The scheduler's state: contexts, the task table, the yielder pointers.
  It is a `thread_local!` (`src/sched/mod.rs`), set up by `start` on the
  calling thread.
- The published stack bounds (`running_stack`) and the hub-hook flag. A
  SIGSEGV handler runs on the faulting thread and reads that thread's
  bounds.
- The soundness argument (S1-S7): a scheduler resumes only its own
  coroutines, on its own thread.
- Thread numbers (`thread_number`) are 64-bit: a worker context's number
  from a process-wide counter, times 2^32, plus the depth of nested tasks
  on it. They stay unique across threads, and neither wrap nor run into
  each other (review RS1S-07).
- The event loop (sched-io): each scheduler has its own registrations,
  timers and loop context; the first takes native's libuv epoll descriptor,
  later ones make their own. The io layer's stream-lock lists (`HELD`,
  `OWNED`) are thread-locals, and a stream that another thread holds is
  waited for with the plain lock. `coop_possible` is process-wide, as a
  property of the program.

`set_ref_read_yields` is process-wide on purpose: it is a property of the
program.

**Assumes one thread, and what would change on a worker pool.**
- **The task table** is the thread's. A pool needs one table shared by the
  workers, with ids valid across threads, behind a lock or with atomic task
  state. Today `Sched` keeps the task table and the thread's contexts under
  one `RefCell`, so this is a split of `src/sched/task.rs`'s state.
- **Queues.** A pool needs a shared queue, or one per worker with stealing.
  Queued tasks can move between workers. A suspended context stays on the
  worker that started it, since its frames may hold references to that
  thread's thread-locals.
- **Jobs** are `Box<dyn FnOnce() -> Outcome>` without `Send`. A pool needs
  `Send` jobs, so the translators' values need atomic reference counts
  (leanrs's `Rc`, Reussir's counts). That is a change of the public API.
- **`Std.Sync` objects** keep their state in a `RefCell` and wait on the
  thread's contexts. A pool needs real locks, and waiters that may be on
  any worker.
- **The glue's own waiter lists** (a thunk being forced elsewhere) hold
  `CtxId`s, which name contexts of one thread's scheduler only (an opaque
  type, review RS1S-08). On a pool they need a worker as well, or a waker
  that knows the worker.
- **Promises, `release` and `resolve`** go through the shared table.
- **The stack-overflow report** already works per thread. Each worker
  thread installs the glue's alternate signal stack.
- **`ST.Ref`** operations are atomic today only because one thread runs
  them. With real threads each `get`, `set`, `swap` and `modify` must be
  atomic, or LB-01 (a concurrent `set` lost by a `get`) comes back.
- **The glue's own state:** `Rc<dyn Glue>` and the per-thread standard
  streams and IO buffers the glue keeps would need `Arc` and locks, or
  stay per worker.
- **The translators' own values.** A task's job and value cross threads:
  - leanrs's `LocalLazy` aborts when used from another thread, and its
    `Thunk` and shared cells are not atomic;
  - lean2rr needs atomic reference counts at the task boundary (its
    Reussir values count without atomics).
- **Joining at exit.** `finish` would join the worker threads, after the
  last task has run, before the glue flushes (as `~task_manager` does).

**Semantics on a worker pool.**
- Lean's task-manager rules hold unchanged:
  - exit joins the tasks before the streams are flushed;
  - a dropped pure task that has not started is deleted;
  - dependents are walked newest first, with `sync` ones on the finishing
    thread;
  - priorities, dedicated tasks and cancellation work as now.
- Deferred start stays a schedule a pool can produce. A pool may also start
  tasks as soon as a worker is free, as native Lean does.
- The pure-task rule is needed only while one thread must not be
  monopolized. With real workers, a started pure task can simply run.
- The yield points, the effect points' 5 ms rule and the lone worker's
  latency model emulate parallelism on one thread. With real workers they
  are unnecessary, and harmless if kept.

## Costs to measure (O12, owner-approved timing session)

None of these has been timed.
- **Polling points.** `ref_read` is an atomic load when off, and a
  thread-local countdown when on; every 1000th read, and every `poll`, costs
  a thread-local `RefCell` borrow and checks on the sleepers, runnable
  contexts and queues, and a clock read and a queue scan only when something
  is pending. The number of the task manager's workers in use is a counter
  (review RS1S-11), not a scan of the contexts.
- **Effect points.** The same check per output, and a clock read when
  something is pending.
- **Context switches.**
  - Two corosensei switches per handover (through `main`'s stack), where
    lean2rr makes one.
  - The hub's `RefCell` borrows, and a thread-local flag per switch (S4).
  - A new context maps a stack (`mmap` and `mprotect`) unless one of the 8
    pooled stacks is free.
- **Per-task cost.** One boxed `Job` per task, a slab entry, and the
  translator's slot. The lone-worker model reads the clock once per spawn
  while a worker is waking.
- **Memory.** A pooled stack keeps every page it touched (no `madvise`
  without `unsafe`); up to 8 are kept. A deep recursion in a task leaves
  that much RSS behind, as a native worker thread's stack would.
- **sched-io.** Before the first task, promise, timer or watch: one relaxed
  load per stream lock and per system call that may block. After it: per
  stream lock, a push and a pop of a thread-local list; per such system
  call, the `io_cooperative` check (a borrow and a few comparisons, and a
  thread-local read of the no-suspend depth), and,
  when it cooperates, a `poll(2)` before each read, `pwritev2` instead of
  `write(2)`, and two `epoll_ctl` per wait that blocks. While the loop has
  a registration or a timer, every polling and effect point takes its slow
  path, and looks at the descriptors at most once a millisecond. Every
  context switch moves the running context's stream locks aside and back
  (two thread-local accesses; nothing to move when it holds none). A
  watch's call costs two `epoll_ctl` (out of the set, then back) and one
  `poll(2)` (the check before the call).
