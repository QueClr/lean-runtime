# The task scheduler (`sched`)

`sched` (feature `sched`) is Lean 4.34.0's task manager
(`src/runtime/object.cpp`, `task_manager`) on one thread, for both
translators. It is lean2rr's model (its `leanrt`: `sched.rs`, `task.rs`,
`sync.rs`; plan §5.14), with three changes:
- it no longer depends on lean2rr's representation of values;
- the stack switch is corosensei's instead of hand-written assembly;
- one rule for pure tasks is new ("The pure-task rule").

Threads mode, Lean's task manager on real threads, is the feature `threads`
instead (`sched::mt`, re-exported as `sched` with the same names; it
excludes `sched`): `docs/threads.md`.

This file covers:
- the model;
- blocking IO and the event loop (sched-io);
- what a translator's glue writes;
- the wait cores (wait-1): waiting for a computation another context runs,
  the single-thread `ST.Ref`, and the deferred resolution of promises
  dropped in a free;
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
| `src/sched/mod.rs` | The public API, `start`, the lazy start (`start_lazy`, `ensure_started`), `ref_read`, the low-level waits |
| `src/sched/task.rs` | Tasks: queues, dependents, walks, queries, cancellation, promises, the final run, the yield points |
| `src/sched/ctx.rs` | Contexts: corosensei coroutines, the hub, the `Glue` trait, the running stack's bounds |
| `src/sched/stack_overflow.rs` | Lean's stack-overflow report: the opt-in SIGSEGV handler that knows the contexts' guard pages (a native quirk with `unsafe`, AR-11; `docs/native-quirks.md`) |
| `src/sched/reactor.rs` | The event loop (sched-io): descriptors and timers on epoll, the cooperative `poll_fds`, the loop context and its callbacks |
| `src/sched/uv.rs` | `Std.Internal.UV`'s loop, timers and signals on the event loop |
| `src/sched/slots.rs` | With `io`: the current standard streams and modelled `errno` of each context and each emulated worker, swapped in while it runs (review AR-24) |
| `src/sched/uv_signals.rs` | The process-wide part of the signal watchers' delivery (signal-hook's handlers, the signal pipe, the counts), shared with threads mode's `sched::uv` (T2) |
| `src/sched/env.rs` | `LEAN_NUM_THREADS`, the number of processors, `LEAN_STACK_SIZE_KB` |
| `src/sched/common.rs` | The plain items both modes share: `TaskState`, the messages, the priorities, `await_task` (`Task.get`'s rule) and `thread_create_failed` (native's abort when a thread cannot be made) |
| `src/sched/threads.rs`, `src/sched/mt/` | Threads mode (feature `threads`, `docs/threads.md`): the module `sched` of a threads build, and `sched::mt`, with `sched::mt::uv`, `Std.Internal.UV` on a loop thread of its own (T2) |
| `src/sched/sync.rs` | `Std.Sync`'s mutexes and condition variable |
| `src/sched/wait.rs` | The wait cores, core 3.1: `WaitList`, `Gate`, the keyed claims ("The wait cores") |
| `src/sched/refs.rs` | Core 3.2: the single-thread `ST.Ref` under Lean 4.35's rule, `Ref<T>` and `ref_keyed` |
| `src/sched/drain.rs` | Core 3.3, in both modes: `DrainScope` and the deferred promise resolutions |
| `src/sched/wait_tests.rs` | The cores' unit tests (single-thread) |
| `src/io/coop.rs` | sched-io in the io layer (features `io` and `sched`): the cooperative reads, writes, `flock` and `waitpid`, and the stream locks |
| `tests/sched-driver/` | Every case of `tests/cases/tasks`, `sync`, `refs`, `taskio` and `uvloop`, and the io cases with tasks, as a Rust program over `sched` and `io`, with the glue a translator writes (native's startup descriptors included). The ports of the `tasks`, `sync`, `refs` and `taskio` cases (`src/cases.rs`) are shared with threads mode's driver |
| `tests/sched-driver-mt/` | Threads mode's driver (`docs/threads.md`, 0.6): the same ports of the `tasks`, `sync`, `refs` and `taskio` cases over `sched::mt`, 5 runs each, with `Arc` values and a glue whose hooks check their contract; built in a cargo invocation of its own |

`sched` depends on corosensei 0.3.4, rustix 1.1 (the event loop's epoll
and poll), signal-hook 0.3.18 (the signal watchers' delivery; its safe
API only) and nix 0.31 (`sigaction` and its re-exported libc, for the
stack-overflow report), and is built with cargo, offline, from the committed
`Cargo.lock` (`cargo build --offline --locked --features sched`;
docs/development.md, "Builds").

## The model

Native Lean runs tasks on a pool of worker threads. Here one thread picks
one of the schedules the pool can produce. A worker may start a task at any
time after it is created, and must have finished it when its value is
needed. So a task is *deferred*: it runs at the first of these points.

- **It is needed.** `Task.get` or `IO.wait` (`wait`) runs it right there,
  on the stack of whoever needs it, as a worker would while the caller
  waits, once it is the task a free worker would start (the queue's head);
  until then the caller waits while the heads start on contexts of their
  own. A task whose sources are still pending first runs that chain, from
  its deepest end, one task after the other, under the same rule.
  `IO.waitAny` runs a task of its list only when it is the only unfinished
  one ("What a waiter or a poller runs on its own stack", sched-3).
- **The running code blocks.** A sleep, a lock, a promise, a task running
  elsewhere, or a read, write or wait that would block in the kernel
  ("Blocking IO and the event loop") blocks it, and one of the task
  manager's workers is free (`LEAN_NUM_THREADS`, or the number of online
  processors). The task then starts on a *context* of its own.
- **An effect point.** At an output, a flush, a process spawn or an exit,
  a task queued 5 ms ago or more (`STALE`) goes first, as natively its
  worker would have run it by then; so do a context whose sleep has ended
  and a UV timer that has come due, as natively their threads ran them at
  their deadlines (review HU-01 for the timer).
- **Polling.** `IO.getTaskState`/`IO.hasFinished` report a pending task
  waiting until the program asks again after a sleep, or asks 1000 times
  without one; what a free worker would start then starts on a context of
  its own and runs before the answer, the polled task once it is the
  queue's head; nothing runs on the poller's stack.
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
3. the oldest pure task a worker has started (below), when a context waits
   for a queued pure task that only such started tasks keep from starting
   (review AR-25, "The pure-task rule");
4. a pure task a worker has started, when nothing else will ever happen (no
   sleeper, timer or registered descriptor);
5. otherwise it waits in the event loop: in `epoll_wait` until a registered
   descriptor is ready or the earliest sleeper or timer is due, or forever
   (a deadlocked native program waits forever too).

**Dependents.** When a task finishes, its dependents are walked from the
newest, as Lean's `handle_finished` walks them. A `sync` dependent runs
there and then, on the finishing thread. The others are queued at their
priority: 0 to 8 in the pool's queues, anything above 8 (2^32 - 1 and a
big priority included, LB-39) as a dedicated task. Example
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
itself (`tasks/task_waits_own_dep`). Such a `wait` never ends, and the
context waits forever (`Wait::OnItself`), but natively it is a `wait_for`,
which raises the worker limit by one for a pool task: the context does not
hold its worker, so a task queued meanwhile still starts (review AR-15;
`tasks/self_wait_frees_worker`: at one worker, a task that waits for its
own dependent, and one queued while it slept, which then runs). A wait
forever that is no `wait_for` keeps its worker (`hang`, `Wait::Forever`: a
thunk forced inside its own computation, `Promise.result!` on a dropped
promise), as natively that thread holds it. `IO.waitAny` and the polling treat
such a dependent as waiting: `IO.waitAny` returns when another task of its
list finishes, as natively (review RS1S-17; `tasks/wait_any_own_dep`).

**The workers a context holds** (`holds_worker`; reviews AR-15, AR-16,
RS4-01). Natively a worker that finishes a pool task stays busy until
`handle_finished` has walked its dependents, the `sync` ones run on it; and
`wait_for` raises the worker limit by one only for a pool task, not for a
`sync` task (its priority is `LEAN_SYNC_PRIO`, so `in_pool` is false). Here
a context's activities are its running tasks and its walks, interleaved
(`Walk::depth`), and the thread is the one of the innermost activity that
runs on a thread of its own:
- a running task not on the thread below it: a pool worker if it is a pool
  task; its own `wait` (`Wait::Cell`, `Wait::Progress`, `Wait::OnItself`)
  frees the worker, but only when it is the innermost activity;
- a walk of such a task (`Walk::own`): a pool worker for the whole walk;
- a `sync` dependent (`ON_THREAD`), and a walk
  of a promise or of such a task run on the thread below them: their waits,
  endless ones included, keep that thread's worker, and `wait` does not
  count the waiter's worker as free when it decides whether the awaited
  task is the head a free worker would start (`wait_raises_limit`).

Cases (one worker, recorded natively): `tasks/sync_walk_keeps_worker` (a
`sync` dependent sleeps in its source's walk: a task queued meanwhile runs
only after the walk, "D done" then "B ran"; with 20 workers at once, the
driver's `sync_walk_keeps_worker_w20`), `sync_self_wait_keeps_worker` and
`sync_wait_in_inline_walk` (a `sync` dependent's endless wait, in a pool
task's walk and in the walk of a task run on a pool waiter's stack: the
queued task never runs), `sync_dep_waits_queued_task` (a `sync` dependent
waits for a queued task while the only worker is busy in the walk: a
deadlock, as natively). Mutation checks: walks that hold no worker fail
`sync_walk_keeps_worker`, `sync_self_wait_keeps_worker` and
`sync_wait_in_inline_walk`; waits in a `sync` task that free the worker
fail the last two; counting the waiter's worker as free in a `sync` task
runs the queued task in `sync_dep_waits_queued_task`.

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
RS2-06). An enqueue, a start or a context's end also wakes it
(`Wait::Any`), but then it only looks whether a task of its list can run
now, as a native worker would run it (`tasks/wait_any_pure_stalled`: a
dependent queued by a walk that then stalls; review RS2-09; the rule is in
the next section).

**A bind task that continues as another task** (review HL2-01, fixes-13).
A bind task whose function returns an unfinished task stops running and
waits for that task (`bind_wait`). Natively `add_dep` puts it behind that
task, and the worker that runs that task runs the bind task afterwards, so
its waiters wake when it finishes. Here the bind task is pending again,
and a pure one without IO need runs only when a waiter needs it. So its
waiters look again at once: the contexts blocked on it while it ran
(`Wait::Cell`, also through a dependent of it) and every `IO.waitAny`.
`wait` then runs the new chain from its deepest end, under the rule of the
next section; `IO.waitAny` starts a started pure task at its root on a
context of its own. Before the fix nobody woke them. The context that ran
the bind task went on without it, and with a sleeper or a watched
descriptor the hub never ran the new task by itself (its last resort), so
the program hung where native ends. Case `tasks/wait_bind_continued_elsewhere`
(four workers; `IO.waitAny` starts the bind task `s` on a context of its
own, where `s` waits 300 ms for an IO task and then returns a
`Task.spawn`; `main` waits for `s` meanwhile, and a dedicated ticker sleeps
in a loop): native prints three lines; before the fix the scheduler hung in
`IO.wait s`. The driver's program `hl2_bind_continued_waiter` does the same
with a timer that ends the hang after 3 s (before the fix: "the timer came
first: true"). In the shapes found, an `IO.waitAny` waiter was woken anyway
(the bind task's context ended, or the new task was enqueued); the wake-up
at `bind_wait` covers a shape where neither happens. Threads mode has no
such gap: its workers run
every queued task, the new source included, and the source's walk queues
the bind task again.

### What a waiter or a poller runs on its own stack (sched-3)

Natively a waiter's thread sleeps, and a free worker takes the queue's
head: `dequeue` takes the first task of the highest non-empty queue, first
come, first served within a priority. `wait_for` (`Task.get`, `IO.wait`) in
a pool task raises the worker limit by one while it waits, so a new worker
takes the head; `wait_any` (`IO.waitAny`) does not, so its thread keeps its
worker. Running a task on the waiter's stack is one of native's schedules
only for a task that a free worker would run at that moment. Any other task
run there makes the waiter wait for that task: if it blocks on something
only the waiter provides (a promise the waiter resolves afterwards, a lock
it holds), the program hangs where native ends (leanrs's reviews AR-9 and
AR-10, corrected; `src/sched/task.rs`):
- **`wait`** runs the awaited task, or the deepest pending task of the
  chain it waits for, on the waiter's stack only once it may
  (`may_run_awaited`): a pure task a worker has started (`PICKED`), a
  dedicated task (a thread of its own at once), or, if a worker is free
  (the waiter's own worker counted free, as `wait_for` raises the limit),
  the lone worker's task or the first task of the highest non-empty queue.
  Otherwise the waiter blocks on that task (`Wait::Cell`; a pool waiter's
  worker is free meanwhile), and the hub starts the heads on contexts of
  their own; the awaited task starts when it becomes the head, there or
  on the waiter's stack when the waiter looks again. Before it decides,
  `may_run_awaited` lets the woken worker take what it would have taken
  by now (`settle_worker`). If that worker starts the awaited pure task
  (`pick`), the task runs on the waiter's stack, as any started task does
  (fixes-8, below). `may_run_awaited`
  also says yes for a pending task in no queue that waits for nothing
  (`QUEUED` and `WAITING` clear): the state of a task handed to a context
  that is about to begin it, between `hand` and `begin`. On one thread no
  other context observes that state, so this branch only keeps sched-2's
  behaviour (the task runs) should it ever be reached (review AR-15). It
  says no for a task that waits for its source (`WAITING`): that task runs
  once the walk of its source's dependents queues it, or runs it, a `sync`
  one (review HL2-03, fixes-13). The chain that `wait` keeps across its
  steps can go stale: when the source of its deepest task has started and
  continued as another task since (a bind task run on the waiter's stack,
  or on another context), that task waits again for a task the chain no
  longer holds, and `wait` builds the chain again from the awaited task.
  Before the fix `may_run_awaited` took such a task, waiting in no queue,
  for a handed one, and ran it before its source had finished, without
  the cancellation and the `EARLY` mark the walk passes on, and its unlink
  took the IO need from its source's chain. Case
  `tasks/wait_chain_bind_continued` (`main` waits for `u`, a dependent of
  `s2`, a `sync` dependent of the pure bind task `r`, whose function
  returns a `Task.spawn`): native prints `3`, nothing on stderr; before the
  fix `s2` ran right after `r`'s function, and its `Task.get` of `r` printed
  "`Task.get` called from a `(sync := true)` task" on stderr (unit test
  `a_kept_chain_runs_no_dependent_of_a_bind_task_that_continued`). A chain built
  in the same step whose deepest task waits for a pending task is a cycle
  (a bind task that waits for a task that depends on it): no task of it
  ever finishes, and `wait` waits forever, as natively (before, it ran a
  task of the cycle; the unit test `review2_need_cycle_counts` now expects
  the cycle's tasks never to run).
- **`IO.waitAny`** runs a task of its list on its own stack only when that
  task is the only unfinished one of the list (every listed task names it:
  none has finished, also not one whose walk has not notified yet, which
  `IO.waitAny` returns at the notification), only once it may (the
  waiter's worker not counted free), and only when the waiter holds no
  worker (`main`, a dedicated task): run there by a pool waiter, it would
  take one worker where natively it takes two. Otherwise it blocks with
  `Wait::Any`, which keeps a pool waiter's worker (AR-10 (ii); sched-2's
  `Wait::Progress` freed it), while the hub starts the heads on contexts of
  their own.
- **The polling threshold** (`IO.getTaskState`, `IO.hasFinished`: two
  sleeps for a pure task, one for another, or 1000 answers) starts what a
  free worker would start now on a context of its own (`start_polled`),
  lets the others go on once, and answers the task's state then (AR-9).
  The polled task starts when it becomes the queue's head; nothing runs on
  the poller's stack. The next answers start over (the task is reported
  waiting until the next threshold).
- **The pure-task rule** still holds: a worker that reaches a queued pure
  task no IO task waits for only marks it started (`pick`). A waiter needs
  it, so the mark wakes the waiters of that task, and `wait` then runs it on
  its stack (it is started, so it may). A mark made during the waiter's own
  look (`settle_worker` in `may_run_awaited`) comes before the waiter
  blocks, so its wake-up reaches nobody: the look checks the mark again
  and runs the task (fixes-8). Before that fix the waiter blocked on the
  started task with no wake-up to come. The hub starts a started pure task
  by itself only as its last resort, which a watched descriptor prevents
  for good, and a pending sleep or timer until it ends. With a descriptor
  watched, the hub waited in `epoll_wait` forever (lean2rr's `RtTcp`:
  about one run in 20 hung after the worker's 90 µs latency passed
  between the task's enqueue and the wait). With only sleeps or timers
  pending, the waiter waited until every one of them had ended (for good
  while a task slept in a loop): for example, a `Task.get` waited out an
  unrelated `IO.sleep 5000` (review RF8-01). A debug build checks that no context blocks on a started pure
  task (`register_block`, review RF8-03). `IO.waitAny` and the polling
  threshold start such a task, when a task they wait for needs it, on a
  context of its own (as the last resort does). A started pure task keeps
  its worker until it has run (review AR-25): an awaited pure task needs a
  worker free of started tasks too, and a waiter that waits behind them
  makes the oldest run on a context of its own ("The pure-task rule").

Example (`tasks/wait_queue_order`, one worker): `x` holds the worker for
100 ms; `b` and `c` are queued, then `a` at `Task.Priority.max`, which
waits for `c`. When `x` ends, the worker takes `a`; `c` is not the head,
so `a` blocks, and the worker it frees runs `b`, then `c`: `X B C A`, as
natively (sched-2 ran `a` on `main`'s stack at once, and `c` on `a`'s:
`C A B X`).

The cases, recorded natively (5 runs each, `LEAN_NUM_THREADS=1`), with
twins in the driver; "before" is sched-2's outcome:

| Case | What the program does | Native | Before |
|---|---|---|---|
| `tasks/wait_queue_order` | as above | `X B C A` | `C A B X` |
| `tasks/wait_head_blocks_on_main` | `main` waits for `c` while `b`, ahead of `c`, waits for a promise `main` resolves afterwards | ends | ends (a runtime that runs the head on the waiter hangs) |
| `tasks/wait_any_head_blocks_on_main` | `IO.waitAny [a, b]` while `a` waits for a promise `main` resolves afterwards | `waitAny` returns `b`'s value, then ends | hangs (`a` ran on `main`'s stack) |
| `tasks/wait_any_keeps_worker` | `t` blocks in `IO.waitAny` on a promise; `b` is queued; `main` resolves after 100 ms | `T done` before `B` | `B` first |
| `tasks/poll_queue_order` | `b` then `t` queued; `main` polls `t` without sleeps | `B T` | `T B` |
| `tasks/poll_threshold_promise` | `t` waits for a promise `main` resolves after 3000 polls of `t` | ends | hangs at the 1000th poll |
| `tasks/poll_threshold_mutex` | `t` locks a `BaseMutex` `main` holds across 3000 polls of `t` | ends | hangs at the 1000th poll |
| `tasks/wait_picked_pure` | `main` waits for a pure task queued behind an IO task, while a dedicated task ticks | ends | ends (without the wake at `pick`, the new rule hangs) |
| `tasks/wait_any_picked_pure` | `IO.waitAny` of two pure tasks while a dedicated task ticks | `t1`'s value | ends (without the start of started pure tasks, the new rule hangs) |
| `tasks/wait_any_finished_unnotified` | `IO.waitAny [t2, u]` while `t2` has finished and its `sync` dependent queues a task, then sleeps 200 ms; `u` waits for a promise `main` resolves afterwards (also run with 20 workers) | `t2`'s value, then `u`'s line | ends (the first version of the new rule ran `u` on `main`'s stack and hung: leanrs's review) |

Mutation checks (2026-10-04): with sched-2's `wait`, `wait_any` and
`state`, the six cases marked as differing above fail (three by a
60-second timeout); running the queue's head on the waiter's stack instead
of blocking hangs `wait_head_blocks_on_main` and `wait_picked_pure`; a
`pick` that wakes nobody hangs `wait_picked_pure` and
`wait_any_picked_pure`; an `IO.waitAny` that does not start the started
pure tasks it needs hangs `wait_any_picked_pure`; an `IO.waitAny` that
takes the one listed task left unfinished for the only one, while another
has finished without a notification yet, hangs
`wait_any_finished_unnotified` (with 1 and 20 workers); a polling threshold that
does not start them hangs the unit test
`polling_with_sleeps_runs_a_pure_task_after_two`.

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
standard streams and exits. In native's order (`~task_manager`,
`object.cpp` 972-988: the standard workers leave their loops once the
queue is empty and are joined, 981-982, and only then are the dedicated
threads waited for, 984-985):
1. it runs the queued and the started pool tasks and waits for the running
   ones, until no pool task is queued, started or running;
2. then the standard workers end (reviews AR-33, AR-34): with `io`, the
   emulated workers' current standard streams and `errno` (`slots`) are
   dropped, so a handle a task left set as its stdout is closed and
   flushed; then the glue's `workers_end` (item 1 of "The glue"). Natively
   each worker's thread finalizers (`lean_finalize_thread`, `thread.cpp`
   58-61) drop the current streams of `MK_THREAD_LOCAL_GET` (`io.cpp`
   115-117) when the worker ends;
3. then it waits until every dedicated task has run to completion, tasks
   enqueued meanwhile included (a pool task enqueued now, LB-13's corrected
   run below, starts with a fresh set, dropped at its end), and no context
   but `main`'s and the event loop's is left. The event loop's context is
   not waited for (reviews HL2-02 and RF13-01 to RF13-14, fixes-13):
   natively libuv's loop thread is detached, and `~task_manager` joins
   only the standard workers and waits only for the dedicated threads.
   - **Unless it carries a thread's task.** A pool or dedicated task run on
     its stack (`IO.waitAny` or `Task.get` in a callback runs the task a
     free worker would start), or the walk of such a task's dependents, is
     natively on a standard worker or a dedicated thread, which
     `~task_manager` joins or waits for. So the loop context is waited for
     while it carries one (`loop_carries_thread`), as a worker context is.
     A `sync` dependent and the walk of a promise run on the loop thread
     natively, and do not count. Case `uvloop/loop_task_at_exit` (a
     timer's `sync` dependent waits with `IO.waitAny` for an IO task that
     sleeps 1 s; `main` returns at 100 ms): native prints "main done", then
     "t done"; with the first version of this fix "t done" was lost
     (RF13-01).
   - **With no loop context alive, the final run looks at the loop** once
     no task is left to start (review AR-52, fixes-14): a timer that came
     due, or a descriptor event (a signal) that came, while the final run
     ran tasks on `main`'s stack starts the loop context, which then goes by
     the rules below, as natively the loop thread fires it alongside the
     workers. Only what is due by then: natively a timer due after the
     workers have ended never fires. With the loop context's budget used
     up (below), none starts (review RF14-05). Cases
     `uvloop/timer_due_in_final_run`
     (a one-shot timer of 300 ms with a `sync` dependent that prints, and a
     task that computes for about 1.5 s with no scheduling point, calibrated
     in `main`) and `timer_chain_in_final_run` (timer A's `sync` dependent
     starts timer B of 700 ms; A fires while `main` sleeps, and its loop
     context ends; B comes due while the final run runs the task): "main
     done", then "timer fired"; "A fired", "main done", "B fired", as
     natively; before, the last line was lost. Unit test
     `the_final_run_fires_a_timer_that_came_due_during_a_task`.
   - **It can go on: it runs alone, for the final run's task time plus
     1 s.** A sleep or a descriptor wait of the loop context that has
     ended by now ends first (the final run wakes the due sleepers and
     looks at the descriptors), as natively the loop thread wakes at its
     deadline while the final run runs a task. Case
     `uvloop/loop_sleep_expired` (a `sync` dependent sleeps 500 ms, then
     prints; `main` returns about 100 ms after the timer starts and leaves
     a task that computes for about 1.5 s without a scheduling point,
     calibrated in `main`): native prints "main done", then "late"; with
     the first version "late" was lost (RF13-02). Then `main` waits
     (`Wait::FinalLoop`) while the loop context runs alone, until it waits
     or ends, or until its budget ends: over the whole final run, the loop
     context's time may reach the time the final run has spent on tasks
     (running them, waiting while other contexts ran them, and the worker
     contexts it starts) plus 1 s (`LOOP_VALVE`). The loop context's time
     is its time alone. The time `main` waits for a task it carries
     (`Final::WaitLoop`) counts for nothing: that wait is never cut
     (RF13-01), earns the loop context no time (review RF13-13: before,
     those waits earned it more, and `uvloop/loop_cycle_long` below ran all
     its cycles) and costs it none (review RF13-14: the sixth version
     charged it, so a wait longer than 1 s used up the budget, and
     `uvloop/loop_carried_then_poll` below lost its line). Natively the loop thread runs alongside the
     workers, so it gets as much time as the final run's tasks take; the
     1 s is grace. The budget is never reset: a callback that waits now and
     then (its own task, say) would otherwise keep the exit for good
     (`uvloop/loop_cycle`, below). Meanwhile its polling
     points find nothing else able to run and start no queued task, so it
     runs at full speed, and a task it enqueues runs after it, as natively
     a worker takes the task while the loop thread goes on (an enqueue does
     not wake `main`; another context's wait or end does): when it waits,
     when its budget ends, or once the task has been queued for 5 ms
     (`STALE`, by then a native worker runs it). At a polling point `main`
     then wakes, runs the queued task (its time adds to the budget) and
     lets the loop context go on again, so a callback that spins on what
     such a task does goes on (review RF13-10: the fourth version made it
     wait for the whole budget). Only polling points (clock and reference
     reads, task-state queries) make that check, not effect points
     (`effect_slow`) or zero sleeps (`zero_sleep`). When the budget ends, `main`'s deadline
     makes it able to run at the loop context's next polling or effect
     point, and `finish` returns, leaving the loop context suspended. So a
     callback that ends within its budget prints what it prints natively,
     and one that polls forever keeps the exit about as long as the final
     run's tasks took, plus 1 s, after they end (the budget counts all
     their time, also time before it could run), where native exits when
     the workers end; unless it keeps enqueueing tasks that `main` runs,
     which natively keep the workers, and so the exit, going too (each
     one's run adds to the budget; a task the callback waits for, run on
     its stack, does not). A callback that keeps running past native's
     exit time can print extra lines, among them the "`Task.get` called
     from a `(sync := true)` task" warning of each `IO.wait` it makes (the
     driver's program `rf13f_loop_cycle_wait`: `uvloop/loop_cycle` with
     `IO.wait`, which natively prints the warning once). The limits of
     this rule:
     - a callback that waits back to back on tasks it starts, run on its
       stack, never reaches a budget decision: each wait counts for
       nothing, and it is never left able to run. Natively a program that
       loops so forever hangs in some runs and exits early in others;
       here, for an endless loop of that shape, the exit never comes on
       its own (the driver's program `rf13g_back_to_back_waits` ends only
       because its callback stops after 3 s);
     - a callback blocked on a wait with no deadline whose event no
       remaining task brings (a descriptor without a timeout, say) is left
       suspended, even if natively the event would come while the workers
       still run; a lock or promise a remaining task settles is not such a
       case (the final run runs that task first, and the callback goes on);
     - the exit can stretch to about twice the final run's task time plus
       1 s: the budget also counts the final run's waits while other
       contexts run, during which the loop context may run too;
     - only polling points make the check for a task queued 5 ms ago
       (above).

     The cases, each recorded
     natively and calibrated in `main` where a duration matters:
     - `uvloop/loop_sleep_poll_print` and `loop_sleep_compute_print` (as
       `loop_sleep_expired`, with one clock read, or about 10 ms of
       computation, before the print): "main done", then "late"; the second
       version stopped the callback at its first polling or effect point
       and lost the line (RF13-04);
     - `uvloop/loop_valve_counts_run` (after `main`'s 1 s task the
       callback starts a 1.5 s task, reads the clock and prints): "main
       done", then "late"; the third version counted the wall clock of the
       callback's task, run on `main`'s stack, against its 1 s (RF13-07);
     - `uvloop/loop_spawn_poll` (the callback starts a 2 s task that
       prints, reads the clock, then prints): "main done", "callback
       done", "task done"; before, the task ran at the clock read, so its
       line came first, or the callback was cut after it;
     - `uvloop/loop_poll_work` (a callback that updates a reference for
       about 1.5 s, its reads scheduling points here, while `main` leaves
       a 4 s task; the updates are calibrated on the same reference,
       shared with a task first, since natively a shared one is slower):
       "main done", then "late true"; a 1 s limit cut it, slowed further
       by switches to `main` at each scheduling point;
     - `uvloop/loop_spins_on_task` (after `main`'s 1 s task the callback
       starts a task that sets a flag and spins on the flag): "main done",
       then "late", written by hand (natively the flag's `set` is lost now
       and then, LB-01, and the loop thread spins until the exit); the
       fourth version lost the line (RF13-10; the driver's program
       `rf13d_loop_spins_on_task` exits at about 1.1 s, 3.1 s before);
     - `uvloop/loop_cycle` (a callback prints a line, then cycles forever:
       a task that sleeps 1 ms, which it waits for with `IO.waitAny`, then 300 ms of
       reference updates; `main` sleeps 100 ms and prints a line): both
       lines, as natively, here at about 1.1 s where native exits at
       0.1 s; the versions before the fifth reset the budget at each of
       its waits, and the exit never came (the other user's review);
     - `uvloop/loop_cycle_long` (5 cycles of a 400 ms task, waited for
       with `IO.waitAny`, then 300 ms of reference updates, then "dep
       done"): "cycling", "main done", as natively, here at about 2.6 s
       (during the fourth cycle's polling, which alone uses up the 1 s)
       where native exits at 0.4 s (after the first task's worker ends);
       the fifth version ran all 5 cycles (3.5 s, "dep done"), and an
       endless version never ended (RF13-13);
     - `uvloop/loop_carried_then_poll` (the callback waits with
       `IO.waitAny` for a 1.2 s task, reads the clock, then prints):
       "main done", then "after"; the sixth version charged the wait and
       lost the line (RF13-14). Its expected files are written by hand:
       native prints the same lines, but now and then crashes at the exit
       after them;
     - `uvloop/loop_polls_at_exit` (a `sync` dependent prints a line, then
       reads the clock forever; `main` sleeps 300 ms and prints a line):
       native prints both lines and exits at once; here the same comes
       about 1 s later (1.3 s); before fixes-13, the final run and the loop
       context let each other go first forever (RF13-03).

     The driver's programs: `rf13_loop_polls_at_exit` (a callback that
     polls for 2 s; the exit at about 1.1 s, without its line),
     `rf13_loop_polls_and_spawns` (the same with an IO task started in
     each round: the tasks' time extends the budget, and here the callback
     reaches its end, 3.3 s; natively the outcome varies, RF13-06),
     `rf13c_loop_spawns_and_sleeps` (a task and a 1 ms sleep in each round,
     for 3 s: the callback waits every round, so its budget starts afresh
     and it runs to its end, as natively, RF13-08), `rf13c_valve_counts_run`
     and `rf13_loop_sleep_compute_print`. The unit test
     `the_final_run_lets_a_loop_context_able_to_run_go_on` checks that a
     timer's callback whose loop context an effect point started runs in
     `finish`.
   - **It waits in a callback** (a `sync` dependent of a timer's promise that
     sleeps, or waits for a promise or a lock): `finish` returns and leaves
     it suspended, unless the wait ends by itself (a sleep, a descriptor
     wait with a timeout) within the time the final run's tasks took that
     the loop context has not used yet, without the 1 s of grace: then
     `main` waits until then (`Wait::FinalLoop`), the wait counts as the
     loop context's time, and the loop context goes on, as natively the
     loop thread wakes alongside the workers (review RF13-12). Case
     `uvloop/loop_sleeps_after_task` (a callback sleeps 300 ms, prints
     "a", sleeps 100 ms, prints "b", while `main` leaves a task of about
     1.5 s, calibrated): "main done", "a", "b"; the fifth version lost "b".
     Before fixes-13 the final run waited for it, for good
     when the callback waited forever. Case `uvloop/loop_blocked_at_exit` (a
     `sync` dependent of a one-shot timer's promise prints a line, then
     sleeps in a loop; `main` sleeps 1 s, prints a line on stderr and one
     on stdout, and returns 3): native prints the three lines and exits
     with status 3; before, the scheduler hung (HL2-02).

   The glue's flush and exit come after, as before: `main`'s buffered line
   is written, and the status is `main`'s. A loop context that waits
   while it holds a stream's lock (a write to a full pipe) is waited for
   by the exit's flush, as any context ("Stream locks").

It does not wait for a task whose dependency never finishes (an unresolved
promise, a cycle), as natively.

Dedicated tasks drop their streams at their end, as their threads end
then. What is never dropped, as natively: `main`'s streams (its thread is
not finalized), the event loop context's (native's loop thread never
ends), and every set at `IO.Process.exit`, which runs no thread finalizers
natively either; glibc's exit then flushes `stdout` first and the other
open streams after it, as `io::exit::exit_flush` does. Cases (recorded
natively, also the same with 2 and 4 workers natively), with twins in the
driver and in threads mode (`tests/threads_twins.rs`), where the workers'
thread-locals' destructors run when `finish` joins them before it waits
for the dedicated threads:
- `tasks/worker_streams_closed_at_exit` (`| cat`; a task makes a handle on
  `/dev/stdout` its stdout and prints `A0`; `main` prints `B`: `A0B`;
  before AR-33 `BA0`);
- `tasks/worker_streams_at_process_exit` (the same, then
  `IO.Process.exit 0`: `BA0`);
- `tasks/worker_streams_before_dedicated` (a task makes `cat`'s input its
  stdout and prints a line; a dedicated task waits for `cat`: `main done`,
  `via cat 0`, `cat exited 0`; before AR-34 the workers' streams were
  dropped only after the dedicated tasks, so `main done`, then a hang).

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
    (`query`): it starts on a context of its own (`start_polled`);
  - an IO task comes to wait for it, directly or through other pure tasks
    (`need_up` walks the chain of waiting tasks up and gives each its *IO
    need*; `startable` starts a started pure task that gains it);
  - a waiter needs its worker (`needed_picked`, below);
  - nothing else can go on, and no sleeper will wake (`last_resort`);
  - `main` returns (`finish`, before the queued tasks).

**A started pure task keeps its worker** (review AR-25, from lean2rr's
switch to the crate). Natively a worker runs the task it dequeues to its
end before it dequeues another (the worker loop, `object.cpp` 863-865:
`dequeue`, then `run_task`). So W workers have at most W tasks started at
a time, and a task queued behind them is not started yet: the program can
still delete it (`deactivate_task`, 1060-1072). Here a started pure task
counts as its worker's task until it runs (`picked_pool`):
- a worker starts a pure task only while a worker is free of the contexts'
  tasks and of started pure tasks (`pure_room`). Otherwise the pure task
  stays in its queue, and `release` still deletes it;
- an awaited pure task (`wait`) needs such a free worker too, so it waits
  behind the pure tasks queued in front of it;
- a waiter (`wait`, `IO.waitAny`, the polling threshold) that waits for a
  queued pure task, while every worker that no context holds is busy with
  a started pure task, makes the oldest of them run on a context of its
  own (`needed_picked`, `picked_hold_pool`), as natively its worker
  finishes it and takes the next task. It runs at once, also while a
  context that holds a worker only sleeps: natively that sleeper's wake
  could free its worker first (LSCHED-03, below). Waiting for the sleeper
  instead (reviews RF3-02, LF3-02, reverted after LF3-05) idled the only
  thread for as long as the sleeper slept, so a watchdog task fired where
  native ends (case `tasks/picked_task_watchdog`). The oldest started
  task also runs when the running context reaches a polling point, an
  effect point or a zero sleep (`IO.sleep 0`, `dbgSleep 0`) and nothing
  else can go on (reviews LF3-01, LF3-04): a started task run for a
  waiter that reads references (every 1000th read polls), prints or
  sleeps 0 ms lets the next one run, whose worker then takes the awaited
  task, and lets a due sleeper go on (cases
  `tasks/picked_task_reaches_yield_points`, `picked_task_sleep_zero`;
  driver test `effect_points_in_a_started_task_let_the_next_run`; each
  fails without it);
- an IO task, and a pure task with IO need, do not wait for started pure
  tasks: for them a started pure task takes no time, as before (LSCHED-01).
  Such a task starts once a worker is free of the contexts' tasks, and
  passes over the pure tasks queued in front of it that wait for a worker
  (`Tasks::elig` counts the queued tasks that are not pure tasks without IO
  need, so a queue without one is not scanned).

The exception for IO tasks is needed. If an IO task waited for the started
pure tasks, it would start only when one of them is needed. Example: one
worker; a pure task is started, an IO task that sets a flag is queued
behind it, and `main` polls the flag with sleeps. Nothing needs the pure
task, so the IO task would never start, and the program would hang where
native ends. It would also run the runaway task of
`tasks/runaway_pure_task_before_io` to free its worker, so `main` would
print nothing (neither native's outcome nor `alt1`).

The cases, recorded natively (5 runs each), with twins in the driver.
"Now" is the crate's outcome today; "Before the fix" is the crate's
outcome before the change the case checks, named in each cell (AR-25 at
31c7bfa, or a fix of fixes-3's reviews):

| Case | What the program does | Native | Now | Before the fix |
|---|---|---|---|---|
| `tasks/wait_pure_queue_order` | one worker; four pure tasks that print their number (`dbgTrace`); `main` waits for the last one first | `task 0` to `task 3` in order | native's | before AR-25: `task 3` first, then 2, 1, 0 (`wait` started the three in front at once and ran the awaited one first) |
| `tasks/drop_queued_behind_pure` | one worker, busy 50 ms with an IO task; `t0` (pure, about 100 ms) and `t1` (pure, never ends) queued behind it; `main` drops `t1` once the worker is free, then waits for `t0` | `t0 752938 false`, status 0 | native's | before AR-25: a hang (the free worker started `t0` and `t1` at once, so `t1` could not be deleted) |
| `tasks/runaway_pure_before_awaited` | two workers; `p` (pure, never ends, no yield point), `q` (pure, quick), then `t`; `main` waits for `t` | `t`'s line, then a hang | nothing, then a hang (`alt1`, LSCHED-02 below) | before AR-25: `t`'s line with `q` reported unfinished, then a hang (not native either) |
| `tasks/picked_task_sleeping_worker` | two workers; IO task `a` sleeps 200 ms; `p` (pure, about 1 s, no yield point) started; `main` waits for pure `t` | `t` while `p` still runs (`p finished then: false`) | `t` after `p` (`alt1`, LSCHED-03 below) | no fix: RF3-02's wait gave native's outcome, and was reverted after LF3-05 |
| `tasks/picked_task_reaches_yield_points` | two workers; `p` (pure, never ends) loops over an `ST.Ref`; `q` (pure, quick), then `t`; `main` waits for `t` | `t = 1001`, `p` unfinished, `q` finished, then a hang | native's | before LF3-01: nothing, then a hang (`p` ran for the waiter, and its reference reads never let `q` run) |
| `tasks/picked_task_ticking_worker` | a control: two workers; an IO task sleeps 100 ms at a time until the exit; `p` (pure, about 100 ms) started; `main` waits for pure `t` | `t = 2`, status 0 | native's | native's; a waiter that waited for every wake of the ticker (LF3-02's concern) would hang |
| `tasks/picked_task_short_sleeper_long` | a control: two workers; an IO task sleeps 1 s once; `p` (pure, a few ms) started; `main` waits for pure `t` | `t` before 500 ms, then `tick` | native's | with RF3-02's wait (reverted): `t` after 1 s (RF3-05) |
| `tasks/picked_task_watchdog` | two workers; a watchdog IO task sleeps 3 s, then exits with status 1 unless shutting down; `p` (pure, about 100 ms) started; `main` waits for pure `t` | `t = 2`, status 0 | native's | with RF3-02's wait (reverted after LF3-05): `timeout`, status 1 |
| `tasks/picked_task_sleep_zero` | two workers; `p` (pure, never ends) calls `dbgSleep 0` at every step; `q` (pure, quick), then `t`; `main` waits for `t` | `t = 1001`, `p` unfinished, `q` finished, then a hang | native's | before LF3-04: nothing, then a hang (`p`'s zero sleeps never let `q` run) |

Unit tests (`src/sched/tests.rs`): `an_awaited_pure_task_waits_for_the_pure_tasks_in_front`,
`a_pure_task_behind_a_started_one_can_still_be_deleted`,
`an_io_task_does_not_wait_for_started_pure_tasks`, and, with a pending
timer that keeps the hub from its last resort,
`a_waiter_runs_the_started_pure_tasks_it_waits_behind`,
`wait_any_runs_the_started_pure_tasks_its_list_waits_behind`,
`polling_runs_the_started_pure_tasks_the_polled_task_waits_behind`;
with a watched descriptor and a timer that ends the watch after 2 s,
`a_pure_task_the_worker_starts_in_the_waiters_look_runs_there` (fixes-8:
without the second check of the mark, `main` blocks until the timer);
for the chain's root, with a pending timer only,
`a_chain_root_the_worker_starts_in_the_waiters_look_runs_there` (without
the check, `main` waits until the timer has run); for `IO.waitAny`'s lone
task, `a_pure_task_the_worker_starts_in_wait_anys_look_runs_there`
(without the check, the task runs on a context of its own) (review
RF8-02).
Mutation checks (2026-10-04): the first two unit tests and the first two
cases fail on the code before AR-25; without the hub's `needed_picked`,
`IO.waitAny`'s or the polling threshold's start of the oldest started task,
the matching timer test fails (a 20-second wait, or a poll that never
ends); without the pass-over of pure tasks that wait for a worker,
`an_io_task_does_not_wait_for_started_pure_tasks` fails (the IO task does
not run while `main` sleeps).

**Its worker's id** (review RF3-01). With `io`, a pool task runs with its
emulated worker's standard streams and `errno` (`slots`, review AR-24),
and `running_worker` names that worker. A started pure task takes the id
of the worker that started it at the pick (the lowest free id then), and
keeps it until it runs, so no other task uses that worker's set meanwhile:
it runs with what its worker's previous task left, as natively. A pool
task's id is free again when its walk of dependents is over, as natively
the worker then takes its next task, so the lone worker's next pick gets
the same id. One corner, with LSCHED-01: an IO task that starts while
started pure tasks hold every worker takes an id beyond the number of
workers, with a fresh set (natively it would wait for one of those
workers, and get its leftovers). Case `tasks/picked_task_own_worker_streams`
(two workers: `p` is started, then IO task `x` on the other worker sets
its stderr to a buffer; native: `p`'s trace on the process's stderr, the
buffer `"x\n"`; before the fix `p` ran with `x`'s streams, and its trace
went into the buffer), with its twin, and the unit test
`a_started_pure_task_runs_with_its_workers_streams`; both fail before the
fix.

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
  third, after two sleeps, starts the task on a context of its own, which
  runs it before the answer, true.

`polling_with_sleeps_runs_a_pure_task_after_two` (`src/sched/tests.rs`)
records the three answers. No recorded case polls a pure task this way.

**Known differences: LSCHED-01, LSCHED-02 and LSCHED-03.** Deferring a
started pure task gives a schedule native's pool does not produce when
runaway pure tasks, queued first, would take every worker (LSCHED-01).
Running the oldest started pure task for a waiter gives one when that task
never ends (LSCHED-02), and delays the awaited task by its run time where
natively a sleeping worker's wake could take it first (LSCHED-03; "Known
differences from native" below).

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
- **The runtime's `IO.Process.output`** waits for both pipes, and for room
  in its input's pipe while it writes the input (LB-40), with `poll_fds`
  before its `poll(2)` and its reads (case
  `taskio/output_input_while_ticking`).

Regular files, block devices and directories never block (natively they
never give `EAGAIN` either), so their calls stay plain. So do descriptors
in non-blocking mode, whose `EAGAIN` is the result (the standard input of a
child that could not start, `fdopen_bounded_pipe`). Each stream finds out
its file type once, with `fstat`, the first time a cooperating call needs
it; its non-blocking mode is read at every cooperating call (`F_GETFL`, one
system call), since the flag belongs to the open file description, which
the processes sharing it (the parent, a child spawned with `inherit`) may
change at any time. Read once, a descriptor made blocking later got a plain
call that blocked the scheduler's only thread, so a task that would make
it ready never ran and the program waited for good; one made non-blocking
later waited where native's call returns `EAGAIN` (io bug hunt HIO-06,
io-fixes-1; unit test `the_nonblocking_mode_is_read_at_every_call`). The
modelled `errno` is not touched.

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
  looks without waiting: the descriptors at most once a millisecond, then
  the due timers always. Within one look a descriptor's event (a signal
  watcher's pipe among them) is queued before the timers due, as libuv
  runs the io callbacks before the timers in one iteration (`uv__io_poll`,
  then `uv__run_timers`), and as threads mode does (review HU-04,
  fixes-14). Case `uvloop/signal_before_timer_in_look` (a `sync` dependent
  of timer A computes for about 1 s on the loop; timer B comes due at
  100 ms, SIGUSR1 at 300 ms; one look finds both after A's dependent):
  "A done", "signal", "timer B", as natively; before, "timer B" came first.
  A context woken by the loop is an ordinary context able to run: at an
  effect point it goes first only once it has been able to run for 5 ms
  (`STALE`), as for any other. A timer that the effect point's look finds
  due goes first at once, as a due sleeper does, when the loop context can
  run its callback (review HU-01): natively the loop thread ran it at its
  deadline. Case `uvloop/timer_effect_order` (a sleeper, a one-shot timer of
  100 ms with a `sync` dependent that prints, and `Std.Async.sleep 100` in
  an async task, each while `main` computes for about 0.5 s with no
  scheduling point, then prints): each event's line before `main`'s, as
  natively; before, the timer's and the async task's came after (the loop
  context started at the effect point had been able to run for less than
  5 ms). Unit test `an_effect_point_lets_a_due_timer_go_first`.

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

Threads mode has its own `sched::uv` (`src/sched/mt/uv.rs`, T2), with the
same names: a loop thread, a recursive loop lock and one watcher list for
the process, as natively (`docs/threads.md`, 0.5). This section is the
single-thread one; the process-wide part of the signals' delivery is
shared (`src/sched/uv_signals.rs`).

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
  (RSIOB-14). It takes only the timers due a loop wake-up ago (1 ms,
  `LOOP_LATENCY`; review HU-03, fixes-14): natively the loop thread wakes
  from `epoll_wait` for a timer and takes its lock some tens of
  microseconds after the deadline (up to about a millisecond on a loaded
  host), so an extern made right after a timer came due takes the lock
  first. Case `uvloop/timer_fresh_next_twice`: two `next`s in a row on a
  fresh repeating timer of period 1 s give one promise, which the 0th tick
  resolves ("true, true"), and `next` then `reset` moves the 0th tick to
  1 s later ("false"), as natively; before, the second extern ran the tick
  that had come due microseconds before ("true, false", "true"). Both
  orders are races, natively too (review RF14-02): an extern that comes
  later than the loop's wake (here, later than `LOOP_LATENCY`; in threads
  mode and natively, after the loop thread took the lock) lets the tick go
  first, so the case accepts each line's other outcome too (`alt1` to
  `alt3`, LSCHED-04 below). The latency was 100 µs in the first version;
  1 ms keeps native's usual order on a loaded host, and every other uvloop
  case gives the same outcome with it. Unit test
  `a_catch_up_leaves_a_timer_due_within_the_loop_latency`. Cases
  `uvloop/timer_due_stop` (a one-shot timer due during a
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
    before the callback, from the loop's time (`uv_timer_again` adds the
    period to `loop->time` of the iteration). Here that is the time of the
    look that found the tick due, not the time the loop context starts the
    callback, which other contexts able to run may delay (review HU-06,
    fixes-14; `reactor::loop_time`); for a tick that a look found due while
    the loop context ran another callback, the time the loop context takes
    it, as natively the busy loop thread looks again only then. Case
    `uvloop/timer_period_from_look` (tick 1 of a 1 s timer comes due while
    `main` computes for about 1.5 s; `main`'s clock read then finds it due
    and a sleeping task's sleep over, and that task computes 2 s before the
    loop context runs tick 1; `main` looks at 3.2 s): "tick 2 by then:
    true", as natively; before, tick 2 came about 3 s after the look. Unit
    test `a_repeating_timers_next_period_starts_at_the_look`. A repeating
    timer with timeout 0 ticks once (libuv's repeat 0 means no repeat), as
    natively, and the loop keeps it until `stop`, as natively
    `lean_inc(obj)` at its start (review HU-02, fixes-14, both modes: the
    timer holds itself, `held`, from its tick until `stop`). Case
    `uvloop/timer_repeat_zero_held` (after the tick, `next` gives a promise
    that the timer holds; the program drops the timer and the promise and
    keeps the promise's task): "finished: false", as natively; before, the
    loop let go of the timer after its tick, and the task read `none`. With
    the timer kept, `stop` lets go of the promise, which reads `none`. Unit
    tests `a_repeating_timer_with_timeout_zero_is_held_until_stop` (both
    modes);
  - `reset` moves a running timer's next resolution to `timeout` ms from
    now; `cancel` drops the promise (a one-shot timer becomes initial
    again); `stop` drops it (a finished timer's too, as timer.cpp 243-246)
    and finishes the timer. After `stop`, `next` gives a new promise that
    nothing resolves (it reads `none` once the program drops it).
- **`stop` and `cancel` change the state before they release the
  promise** (LB-33, LB-34). The handle's promise is taken out and its new
  state set first (`stop`: the loop's timer or the listening stopped, then
  finished; `cancel` of a one-shot handle: stopped, then initial again), and
  only then is the promise released. If the handle held the last reference,
  the promise reads `none`, and its `sync` dependents run there, inside the
  extern, and see the handle after the operation, as every later dependent
  does:
  - after `stop`, `next` gives a new promise that the handle does not hold,
    which reads `none` once dropped, so a dependent that subscribes again
    on every value runs again each time (cases
    `uvloop/timer_oneshot_stop_resubscribe`, `timer_stop_rearm_async_dependent`,
    `signal_stop_drops_promise`, natively the same);
  - after `cancel` of a repeating handle, `next` gives a promise that the
    handle keeps and the next tick or signal resolves; after `cancel` of a
    one-shot handle, `next` starts it again.

  This is Lean master's `stop` (PR #14793, in no release yet, not in
  4.35.0-rc1). Lean 4.34.0 releases first, while the handle still runs and
  stores the promise (timer.cpp 243-252 and 271-284, signal.cpp 236-241 and
  263-273): a dependent's `next` frees the promise twice and stores a new
  one that is then lost, or, on a one-shot handle, gets the promise being
  freed. The order for `cancel` is lean-runtime's own correction: master's
  `cancel` is unchanged. A timer that does not run lets go of its promise
  on `stop`, as in 4.34.0 (timer.cpp 243-246; master returns early).
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
  For a signal whose default is to ignore it, the new watcher never gets
  the next signal: the loop drops it (review AR-50, below; cases
  `uvloop/signal_reset_urg_in_sync_dependent` and
  `signal_reset_winch_in_sync_dependent`, "B got: false"; the controls
  `signal_reset_usr1_in_sync_dependent`, status 138, and
  `signal_reset_urg_in_async_dependent`, "B got: true").

**Signal delivery** uses signal-hook's safe API only (review RSIOB-05):
- A signal's handlers are installed at its first watcher and never taken
  back (signal-hook cannot restore a disposition). They run in the order
  they were registered (`handlers` and `reset_pair` in
  `src/sched/uv_signals.rs`, the lists that the registration follows):
  1. the conditional default action (`flag::register_conditional_default`)
     on the signal's `default` flag, which runs the signal's default action
     while no watcher listens (natively libuv restores `SIG_DFL` when the
     last watcher stops): after `stop`, SIGUSR1 ends the program again
     (status 138), and SIGCHLD is ignored again;
  2. `flag::register` sets the signal's `arrived` flag;
  3. `low_level::pipe::register_raw` writes a byte into the loop's signal
     pipe (the flag first, so a reader woken by the byte finds it set);
  4. while every listener of the signal is one-shot, a reset pair on a flag
     of that registration's own: a second conditional default action on
     the flag, then a `flag::register` that sets it. That is libuv's
     `SA_RESETHAND` for one-shot watchers: the first signal passes the
     check and sets the flag, so a second signal takes the default action
     before the loop has delivered the first (RSIOB-02, = leanrs's R1),
     when that action ends or stops the process; for a signal that is
     ignored by default, the loop drops the second signal (below).
     Each registration gets a pair on a fresh flag, registered and
     unregistered (`low_level::unregister`, the check first) as
     `uv__signal_start` and `uv__signal_stop` re-register libuv's handler:
     when the first watcher starts, when a repeating one joins one-shot
     ones, when only one-shot ones remain, and when none is left.
- **The check of `default` comes first** (review AR-49). The check that
  the loop can change, `default`, runs before the handler's byte wakes the
  loop, as natively the kernel decides the disposition when it delivers
  the signal and never again. The reset pair's check runs after the byte,
  but reads only its registration's own flag, which only handlers write.
  Before the fix the check of `default` came after the byte: the handler's
  byte woke the loop, the loop delivered the signal to a one-shot watcher,
  which stopped (the last one, so `default` was set), and the handler then
  read `default` and killed the process. Threads mode's twin of
  `uvloop/signal_stop_in_sync_dependent` ended with status 138 now and
  then; single-thread mode had the same window, since the handler can run
  on another thread than the loop's. And `default` means only "no watcher
  listens": the reset used to set `default` itself. When a repeating
  watcher joined one-shot ones, `register` cleared `default`, then
  unregistered the reset; `low_level::unregister` waits for the handlers
  that are running, so a handler between its check and its reset set
  `default` during that wait, and the repeating watcher listened while the
  next signal took the default action. Now a late reset sets only its own
  registration's flag, which nothing reads any more, so the fix does not
  depend on that wait (an internal detail of signal-hook-registry 1.4.8).
  Only RSIOB-02 at the first one-shot registration does, in the narrow
  window between the pair's registration and the clearing of `default`
  (the module comment). The pipe's action keeps its place, third: it is
  never unregistered (below), so it cannot move to the end. Unit tests
  `the_default_is_checked_before_the_loop_wakes` (the lists' order),
  `each_oneshot_registration_has_a_fresh_reset_flag` and
  `a_late_reset_leaves_a_new_watcher_listening` (SIGURG's actions in the
  test process; the last two fail with the reset's flag on `default`).
- **The signal pipe** is native's own, from `io::startup` (made at startup,
  as libuv makes it in `uv__process_init`), when the glue opened native's
  startup descriptors: a watcher then opens no descriptor, and the pipe's
  type and numbers are native's (case `uvloop/signal_fds`). Without them,
  the first watcher makes a pipe of its own; if it cannot (`EMFILE`), that
  watcher's `next` fails, and the next watcher tries again (RSIOB-16).
  The watchers take it with `io::startup::claim_signal_pipe`, which hands
  it to the first caller only. A translator that keeps its own scheduler
  and signal watchers over `io` may claim it instead, with or without
  `sched` (AR-17); it then has the same duty, and `sched`'s watchers, if
  any, make a pipe of their own.
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
  tree orders them (RSIOB-08; case `uvloop/signal_order`). The watchers of
  every signal of the batch are taken before the first delivery (review
  HU-05, fixes-14, both modes): natively the handler writes one message
  per watcher listening when the signal comes, so a watcher that a `sync`
  dependent of an earlier delivery starts does not get a signal of the same
  batch. Case `uvloop/signal_batch_new_watcher` (a repeating watcher W0 of
  SIGUSR2; a one-shot watcher W1 of SIGUSR1 whose `sync` dependent starts a
  one-shot watcher W2 of SIGUSR2; both signals come while the loop computes
  in a timer's `sync` dependent): W1 and W0 get theirs and W2's promise
  stays pending, as natively; before, W2 got SIGUSR2 too. Occurrences of
  one signal between two calls are one delivery, where libuv makes one per
  occurrence (its pipe carries one message per occurrence and watcher); a
  repeating watcher's promise takes one value either way.
- **A one-shot registration delivers one signal** (review AR-50). Under a
  one-shot registration (every listener one-shot), the first signal that the
  loop takes spends the registration, and the loop drops the later ones
  until the next registration (`Hooked::take`): natively the kernel restored
  `SIG_DFL` when it delivered the first, and a later one takes the default
  action. For a signal that ends or stops the process, the reset pair's
  check takes that action in the handler, as before. For a signal whose
  default is to ignore it (SIGCHLD, SIGCONT, SIGURG, SIGWINCH), the pair's
  action does nothing, and the handler has already set the flag and written
  the byte, so the drop is that signal's default action. For a stop signal,
  the drop is what happens when the process continues: natively the stop was
  all the signal did. Before the fix, in RSIOB-11's state (above), the new
  one-shot watcher got the second SIGURG or SIGWINCH, which natively the
  kernel discards (leanrs's repro), and a second SIGTSTP, SIGTTIN or SIGTTOU
  when the process continued after the stop. The loop marks the registration
  when it takes a signal, not when the kernel delivers one, so the two can
  differ at a registration, both ways. A signal that came under the
  registration before, while watchers listened, and that the loop has not
  taken yet must not spend the new one: when a repeating watcher stops and
  one-shot ones remain, `register` moves the `arrived` flag into the new
  pair's `carried`, and the loop delivers that signal without spending the
  registration, as natively it was caught before the `sigaction` with
  `SA_RESETHAND` (review AR-50, part 2). The pair's registration waits for
  the handlers that are running (signal-hook's wait), so the flag then holds
  every signal of the registration before. Before the fix, in cases
  `uvloop/signal_reset_usr1_after_repeating_stop` and
  `signal_reset_urg_after_repeating_stop` (a `sync` dependent of a repeating
  watcher W's promise, on the loop, starts a one-shot watcher O, computes
  while a signal comes, then stops W; a `sync` dependent of O's promise
  starts a one-shot watcher B; then a third signal), the loop spent the new
  registration with the second signal and dropped the third ("B got:
  false", in both modes), which natively B gets ("B got: true"; strace: the
  second signal is caught under W's `SA_RESTART` registration, before the
  `SA_RESETHAND` one). The move reads the new pair's flag first: a handler
  sets `arrived` before the pair's flag, so when the flag is set by then,
  the move also takes a signal of the new registration, its first, and the
  registration is spent; the loop delivers the moved signals once, and
  drops a later one (review RF11-03). A flag read after the move could be
  a handler's whose `arrived` came after the move, which the loop must
  deliver as the registration's first. The order leaves a window: a
  handler that sets `arrived` before the swap and the pair's flag after
  the read (one on another thread still in its pipe write when the move
  runs, for example) has its signal moved without spending the
  registration, so the next ignored signal is delivered once more, a stop
  signal stops the process and is then delivered, and in the two-handler
  race (below) a terminating one is delivered (review RF11-07). At the
  first watcher's start a window
  remains, between `listen`'s clearing of `arrived` (RSIOB-03) and the end
  of `register`, while `default` is still set: an ignored signal's default
  action does nothing, so the handler sets the flag, which spends the new
  registration (natively the signal came before `sigaction` and was
  discarded; one delivery either way; review RF10-02). That matters only if
  a one-shot watcher then starts in a `sync` dependent of that delivery. The
  other way round, a signal that came under a spent registration and that
  the loop has not taken yet would be delivered by the next registration: a
  repeating watcher that joins before the loop's next look would get it, and
  the one-shot one with it, where natively the kernel discarded it (review
  RF10-01). So `register` clears `arrived` when it takes back a spent
  registration's pair (`unregister` too): the first signal's flag was taken
  when the loop spent it, and the take-back waits for the handlers that are
  running (signal-hook's wait), so the flag holds only later signals of the
  spent registration. Unit tests
  `a_spent_oneshot_registration_drops_a_later_signal` (it fails without the
  rule or without the clear) and `a_signal_before_a_reregistration_does_not_spend_it`
  (it fails without the move, when a take-back loses the carried signal,
  when the move does not spend a registration whose flag is set, or when a
  take under a spent registration loses the carried signal); the cases
  above in both drivers fail without them.
- **A later signal that ends the process ends it** (review AR-50, part 2).
  Two handlers of one signal can run at the same time on two threads (the
  kernel blocks a signal only on the thread whose handler runs it), and
  both can pass the reset pair's check before either sets its flag, where
  natively the kernel resets the disposition when it delivers the first
  signal, in the same step, and the second one takes the default action,
  whichever thread it goes to (a C probe: an `SA_RESETHAND` handler that
  spins 300 ms with the signal blocked on its own thread only, and a second
  SIGUSR1 50 ms after the first: status 138 every time, and no second
  handler). Within signal-hook's safe API and without descriptors of our
  own, the check and the set cannot be one step: the safe actions are
  stores, which leave the same state whether one handler ran or two, and
  an atomic swap needs an action of our own (`low_level::register`,
  unsafe); a pipe per registration, whose bytes would count the handlers,
  would cost two descriptors that native does not open (case
  `uvloop/signal_fds`; review RF11-05). The loop sees the second
  handler's flag when it comes after the loop spent the registration: at
  its next take,
  or at the registration's take-back (`unregister`, when the watcher it
  delivered to was the last one, or a repeating watcher's start). For a
  signal whose default action ends the process, the loop then takes it
  (`later_default`: signal-hook's `emulate_default_handler`, which restores
  `SIG_DFL` and raises the signal again, so the status is the signal's and
  the kernel dumps a core where the signal's default makes one; SIGIO's
  exit with status 157, RSIOB-06). Such a flag comes from a handler that
  passed the check, or from one that saw the flag set and ends the process
  itself: either way the process ends with the signal, as natively. The
  spending signal came after the registration (one from before is carried;
  at the first watcher's start a terminating one meets `default` and ends
  the process), so its handler ran the pair. For a signal whose default is
  to ignore it, the loop drops the flag, as before. Not covered, within
  signal-hook's safe API and without descriptors of our own:
  - the process ends when a thread next looks at the flag (the loop's
    take, or a take-back), not when the second signal comes, as natively.
    In both modes the loop runs the first delivery's `sync` dependents
    before it looks again (in single-thread mode, at a yield point), so
    one may run (and print) before the process ends, and if `main` ends
    first the process exits with `main`'s status (reviews RF11-01,
    RF11-08);
  - a handler of a one-shot re-registration that sets `arrived` before the
    move's swap of it and the pair's flag after the move's read of it has
    its signal moved without spending the registration (above; review
    RF11-07);
  - when both handlers set `arrived` before the loop takes it, the loop sees
    one signal: one delivery, and the second signal, natively the default
    action, is lost (a terminating one does not end the process);
  - a stop signal (SIGTSTP, SIGTTIN, SIGTTOU) that the loop finds after the
    registration is spent is dropped: its handler may have stopped the
    process already (RSIOB-11's state, after the process continued), and
    the loop cannot tell, so a stop there would stop the process twice; in
    the race the process does not stop, where natively it does. And a
    take-back takes the pair back before it clears `arrived`: a stop signal
    that comes between the two, when a repeating watcher starts under a
    spent registration, is dropped, where natively it stops the process
    (before the `sigaction`) or is delivered (after it) (review RF11-04).

  The race needs two signals that the kernel delivers to two threads
  within the few instructions between a handler's check and its set (or a
  handler preempted there); the take-back's window, a stop signal between
  two steps of a registration. No case can show the race on demand: which
  thread runs a handler, and when, is the kernel's choice. Unit test
  `a_later_signal_of_a_spent_registration_takes_its_default_action` sets
  the second handler's flag without the pair's check, at the take and at
  both take-backs, in a child process: SIGUSR2 ends it with its signal,
  SIGIO with status 157, and SIGTSTP is dropped and does not stop it.
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
  discards the three. No safe route restores their disposition. When the
  process continues, the handler goes on with the `arrived` flag and the
  byte (the check of `default` comes first, AR-49): a watcher that starts
  at that moment may also take it, after the stop, where natively the
  signal does one or the other (the old order had the mirror window, with
  neither). No case (a stopped process).
- the reset of a one-shot watcher's handler happens in the handler and in
  the loop, where the kernel's `SA_RESETHAND` resets the disposition
  before the handler runs: the same outcome for a second signal. The
  reset pair's check ends or stops the process (case
  `uvloop/signal_oneshot_twice`: two SIGUSR1 50 ms apart while `main`
  computes without a yield point, status 138), and the loop drops a
  second signal that is ignored by default (AR-50, above). Two handlers
  of one signal that run at the same time on two threads can both pass
  the pair's check, where natively the second takes the default action.
  A second signal that ends the process still ends it when the loop finds
  its flag after the first one's delivery (AR-50, part 2, above); not when
  both flags come before the loop's take (the loop sees one signal), and a
  second stop signal does not stop the process. One that is ignored by
  default is dropped by the loop either way. No case: a race of two
  deliveries.

**The loop holds a running handle**, as natively `lean_inc(obj)`: a
running timer or a listening watcher fires even if the program dropped it,
and a running repeating timer with timeout 0, which no tick resolves after
its first, keeps its promise until `stop` (HU-02, above).
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
- **LB-33, LB-34**: natively `stop` and `cancel` release the promise while
  the timer or watcher still runs and stores it, so a `sync` dependent's
  `next` frees it twice and its new promise is lost (the process can hang
  at exit), or, on a one-shot handle, gets the promise being freed. Here
  the state changes first (above). Cases `uvloop/timer_stop_rearm_in_sync_dependent`,
  `timer_cancel_rearm_in_sync_dependent`,
  `timer_oneshot_stop_keep_in_sync_dependent`,
  `timer_oneshot_cancel_keep_in_sync_dependent`,
  `timer_oneshot_cancel_resubscribe`, `signal_stop_rearm_in_sync_dependent`,
  `signal_cancel_rearm_in_sync_dependent`,
  `signal_oneshot_stop_keep_in_sync_dependent`,
  `signal_oneshot_cancel_keep_in_sync_dependent`; the controls
  `timer_oneshot_stop_resubscribe`, `timer_stop_rearm_async_dependent` and
  `signal_stop_drops_promise` follow native.

Cases `tests/cases/uvloop` (recorded natively, 5 runs, with twins in the
driver): `loop_configure`, `timer_oneshot`, `timer_repeating`,
`timer_cancel_reset`, `timer_due_stop`, `timer_catchup_bound` (its
dependent subscribes again only on `some`: LB-33),
`signal_rearm_in_sync_dependent`, `signal_rearm_in_async_dependent`,
`signal_usr1` (SIGUSR1 sent by
`kill`, a child, to the program: one-shot, repeating, `cancel`, an unknown
number, and status 138 after `stop`), `signal_stale`,
`signal_stale_deferred`, `signal_oneshot_twice`, `signal_cancel_restart`,
`signal_order`, `signal_fds`, `exit_listening`, `loop_blocked_at_exit`,
`loop_task_at_exit`, `loop_sleep_expired`, `loop_sleep_poll_print`,
`loop_sleep_compute_print`, `loop_valve_counts_run`, `loop_spawn_poll`,
`loop_poll_work`, `loop_spins_on_task`, `loop_cycle`, `loop_cycle_long`,
`loop_sleeps_after_task`, `loop_carried_then_poll`, `loop_polls_at_exit`
("Exit"), `timer_due_in_final_run`, `timer_chain_in_final_run` ("Exit",
AR-52), `timer_effect_order`, `timer_repeat_zero_held`,
`timer_fresh_next_twice`, `signal_before_timer_in_look`,
`signal_batch_new_watcher`, `timer_period_from_look` (HU-01 to HU-06),
`signal_sigio_default`,
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
Every extern, `Timer::new` and `Signal::new` included, first catches the
loop up, so it may run due callbacks and their `sync` dependents, and let
other contexts run. For a value to leave behind in a `mem::take`-style
move, use `Timer::placeholder()` or `Signal::placeholder()` (review AR-22):
they do not catch up and let nothing run. They are not `Timer.mk` or
`Signal.mk`: an initial one-shot timer with timeout 0, an initial one-shot
watcher of no signal. Never give one to the program or use it as a timer
or a watcher; that is the glue's error.

## The glue

A translator writes this glue around the crate. `tests/sched-driver/src/`
(`glue.rs` with `glue_common.rs`, and `lean.rs`) is a complete example. In
threads mode the glue is smaller (no `suspend`, no yield points; a thunk or
a reference blocks its own thread with a lock): `tests/sched-driver-mt/src/`
(`glue.rs`, `lean.rs`) is the example there, with the same case ports
(`docs/threads.md`, 2.4 and 0.6).

1. **`Glue`.** Implement `Glue::suspend`, which is the one `unsafe` step:
   ```rust
   fn suspend(&self, s: Suspend<'_>) {
       // SAFETY: lean-runtime docs/sched.md, "Why Glue::suspend is sound".
       unsafe { (*s.yielder()).suspend(()) }
   }
   ```
   The optional hooks, for the glue's own per-thread and per-task state:
   - `switched(from, to)`: the running context changes;
   - `task_begin` and `task_end`: a task begins and ends.

   The io layer's per-thread state, the current standard streams
   (`IO.setStdout` & co.) and the modelled `errno`, is the scheduler's (with
   `io`; review AR-24, `src/sched/slots.rs`): each context has its own set,
   swapped by the hub, and a task that natively has a thread of its own
   runs with its emulated thread's: a pool task with the lowest free
   worker's set, which keeps what the task leaves (natively a worker keeps
   its thread-locals from one task to the next; cases
   `tasks/worker_keeps_streams` and `worker_keeps_errno`), a dedicated task
   with a fresh one. A glue must not swap `io::streams` in its hooks too.

   A glue that keeps per-thread state of its own (lean2rr's stream cells,
   which only its generated code can build and drop) follows the same
   emulated workers with `sched::running_worker()` (review AR-32,
   lean2rr's AR-S3). From `task_begin` through the job to `task_end`, it is
   the id of the emulated worker the innermost running task occupies, the
   one whose `io::streams` set `slots` swapped in: a pool task takes the
   lowest id no task holds (a task waiting in `Task.get` still holds its
   own; a pure task a worker has started holds the id of that worker from
   the start, review RF3-01), and keeps it to the end of its run (the id
   is free for the next pick once its walk is over). A `sync` task runs
   on the thread below it and shares its answer (review RF3-03, as in
   threads mode): inside a pool task's walk, that task's id; on `main`'s
   thread, `None`. It is `None` for a dedicated task, `main` and the
   initializers. So the glue keeps one set per id: swapped in at a pool
   task's `task_begin`, kept at its `task_end`, and fresh for a dedicated
   task (a `sync` task's `task_begin` comes with `own_thread` false: it
   keeps the set of the thread below it). Unit tests
   `running_worker_names_a_pool_tasks_emulated_worker` (two pool tasks at
   one worker: one id, and the `sync` dependent in their walk the same; a
   dedicated task and `main`: `None`; the hooks see the same),
   `a_sync_dependent_shares_its_threads_worker` (on `main`'s thread:
   `None`) and
   `a_pool_task_run_by_a_pool_waiter_takes_the_next_id`. In threads mode
   the same function gives the standard worker thread's index
   (`docs/threads.md`).

   `Glue::workers_end` (review AR-34) is where the glue drops that
   per-worker state: `finish` calls it once, after the last pool task, right
   after it drops the io layer's per-worker sets, and before it waits for
   the dedicated tasks ("Exit" above), as natively each standard worker's
   thread finalizers run when `~task_manager` joins it. A pool task that
   begins later (a dedicated task's dependent, LB-13's corrected run) gets a
   fresh io set, dropped at its end; the glue does the same with its own
   set: fresh at that task's `task_begin`, dropped at its `task_end`. Unit
   tests `the_workers_end_once_before_the_dedicated_tasks_run` and
   `a_late_pool_task_drops_its_streams_at_its_end`; in threads mode,
   `the_workers_end_once_before_the_dedicated_threads_are_waited_for` (the
   hook is there for symmetry: a glue's per-thread state is each thread's,
   dropped at its `thread_end`).

   `switched` runs on `main`'s stack, inside the hub: it must not block or
   yield, and the scheduler panics if it tries. When nothing can run, the
   hub waits in the scheduler's own event loop; the glue has no hook there
   (sched-io removed sched-1's `Glue::idle`).
2. **Lifecycle.**
   - `sched::install_stack_overflow_handler()` on the thread that runs the
     initializers, and on `main`'s thread if it is another one (item 8).
   - Run the module initializers. Tasks run at once then, as natively.
   - `sched::start(glue)` on the thread that runs `main`
     (`lean_init_task_manager`). It takes Lean's numbers: the task
     manager's workers from `LEAN_NUM_THREADS` (else the online
     processors), and each context's stack from Lean's thread size
     (`lthread`: 1 GiB on 64-bit targets, or `LEAN_STACK_SIZE_KB` rounded
     down to 4 KiB plus 128 KiB). A translator with rules of its own calls
     `start_with(glue, workers, stack_size)` instead (leanrs: its
     `LEANRS_STACK_SIZE_KB`, or 4 GiB).
   - Or the lazy start (lean2rr; audit item 4.5):
     `sched::start_lazy(glue, workers, stack_size)` takes the numbers at
     `main`'s start (`lean_num_threads()`, `thread_stack_size()`), and
     `ensure_started()` builds the scheduler at the program's first task,
     promise, `Std.Sync` object or operation, timer, signal watcher or
     socket. The crate's own entry points for those call it (`spawn`,
     `depend`, `dependent_runs_now`, `promise_new`, every method of
     `sync`'s objects, `uv::loop_configure`, `uv::loop_alive`,
     `uv::Timer::new`, `uv::Signal::new`, `net`'s socket constructors and
     DNS lookups); the glue calls it before anything of its own that needs
     the scheduler. A
     program that makes none of them builds no scheduler state, context or
     event loop. Until then `deferring()` already says whether new tasks
     are deferred (the task manager runs natively from `main`'s start),
     `manager_running()` stays false (it means "built with workers"),
     `sched_started()` is false (so `current_context()` is not needed: it
     is `MAIN`), and `finish()` builds nothing. At the start, `ST.Ref`
     reads become polling points (`set_ref_read_yields(true)`). Every
     `Std.Sync` operation starts it first, since a wait needs the
     scheduler's contexts. A lock's owner does not depend on the start
     (item 6): `main` is the same owner before and after its first task
     (lean2rr's review RS4-05). Single-thread scheduler only (threads
     mode starts eagerly). `tests/sched-driver`'s `lazy_start_cases` runs
     cases through it, one per kind of entry point.
   - `set_ref_read_yields(true)` if the program creates tasks (the lazy
     start does it itself).
   - Run `main`, with a scheduler as Lean's `lean_run_main` does if the
     glue wants native's thread: `io::startup::run_main(stack_size, body)`
     runs `body` on a new thread with that stack (Lean's size is
     `thread_stack_size()`), or on the calling thread with
     `LEAN_MAIN_USE_THREAD=0`, and aborts with libc++'s report when the
     thread cannot be made (audit item 4.2). `body` installs Lean's
     stack-overflow report first (item 8). The scheduler's state is the
     thread's own (thread-locals), so with `run_main` the whole of this
     list from `sched::start` (or `start_with`, `start_lazy`) to
     `sched::finish` runs inside `body`, on `main`'s thread: a `start`
     before `run_main` starts a scheduler on the initializers' thread,
     which `main` never sees (review RSH2-03).
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
   the end of `run_task`, that is with the bytes still on their way.

   **A job with per-task state of the glue's own** (review AR-26, lean2rr's
   AR-S1). The task's `sync` dependents natively run on the finishing
   thread right after the task, with what the task left there (its current
   streams: a dependent's line lands in the buffer the task set). The
   scheduler walks them after the job returns, inside the emulated thread's
   `io::streams` set, so a glue whose streams live there needs nothing. A
   glue that opens its own per-task state in the job (lean2rr's stream
   context, values only its generated code can drop) and closes it before
   the job returns calls `end_running_task(id)` after it stores the value
   and before it closes that state, with the id `spawn` or `depend`
   returned for this job (kept in its task object): the task ends and its
   dependents are walked there, to the end of its walk, inside the state;
   the job then returns `Outcome::Done` (seen as already ended; `Continue`
   after it is a panic) and only finishes up, with no wait. The call does
   nothing for an id the scheduler is not running as the innermost task in
   this context (`TaskId::FINISHED` from a job the glue runs itself, a
   second call; review RT2-14). The job may start before `spawn` or
   `depend` returns: in threads mode a worker takes it at once; in both
   modes the source of a `sync` dependent may have finished by the time
   `depend` makes it, and the dependent then runs inside the call (review
   HR-02, fixes-14: in the single-thread scheduler `depend`'s writers point
   lets other contexts run, after the glue's `dependent_runs_now`). The
   glue stores the id where the job reads it before it gives the id to
   anyone, so that no dependent exists yet when such a job finds no id and
   the call does nothing. A
   chain of `sync` dependents whose jobs
   call it recurses once per link, as natively (review RT2-15): mind
   `main`'s stack. Threads mode has the same call and contract (a
   `Continue` after it aborts there). The calls:
   - `Task.spawn`/`IO.asTask`: `spawn(job, prio, keep_alive)`;
   - `Task.map`/`bind`, `IO.mapTask`/`bindTask`: when
     `dependent_runs_now(src, sync)` is true, apply `f` at once; otherwise
     `depend(src, job, prio, sync, keep_alive)`. If `src` has finished by
     the time `depend` makes the dependent (its writers point can let it
     finish), a `sync` dependent runs at once, inside the call, and an
     async one is queued (review HR-02, fixes-14; before, the single-thread
     scheduler queued the `sync` one, which then ran later, where a
     `Task.get` in it printed the "`Task.get` called from a `(sync :=
     true)` task" panic). The `sync` one runs as Lean's fast path, the
     function applied in the caller (`lean_task_map_core`,
     `lean_task_bind_core`: natively the source had finished before the
     drop that delayed it here returned, so the glue's check would have
     seen it): it is the caller's code, so `in_sync_task` answers for the
     caller, and a `Task.get` in it prints that panic only where the caller
     is a `sync` task (review RF14-03; the task is marked `FAST`). So do
     `IO.checkCanceled` (the caller's flag) and the worker it holds while
     it waits: a pool caller's wait in it frees the caller's worker, as
     native `wait_for` does for the pool task the function natively runs
     in (with one worker, a wait for a queued task runs that task; the
     driver's program `rf14_fast_pool_caller_waits`, which hung before; unit
     test `a_dependent_run_at_once_checks_its_callers_cancel`). The
     signature and the result are unchanged: the id of the task, whose job
     has run: for a map, it has finished, its value already in the glue's
     slot; for a bind whose function returned an unfinished task, it waits
     for that task. So `depend` runs translator code: the glue holds no
     borrow across it that the job could need. Likewise a `sync` bind task whose function returned a task that
     has finished by the time the bind task waits for it (the writers point
     at the job's end) runs on at once, on the thread of its first run
     (review RF14-04), instead of being queued. Unit tests
     `depend_runs_a_sync_dependent_of_a_finished_source_at_once`,
     `a_dependent_run_at_once_waits_as_its_caller`,
     `a_sync_bind_task_whose_task_has_finished_runs_on_at_once` and
     `a_sync_bind_task_runs_on_on_the_thread_of_its_first_run`; the
     driver's program `rf14_depend_fast_path` (`process/handoff_then_sync_map`
     without the drain-end hook: the function waits for an unfinished task
     with no panic on stderr);
   - `prio` is Lean's `Task.Priority`, the whole `Nat`. A priority of 2^64
     or more (a big `Nat`) is passed as `u64::MAX`, saturated, never its
     low bits. 0 to 8 are the pool's queues, and every priority above 8 is
     a dedicated task (`common::priority`): native cuts the priority to an
     `unsigned`, so there 2^32 - 1 is `LEAN_SYNC_PRIO` and runs at once on
     the spawning thread, and 2^32 to 2^32 + 8 are pool priorities (LB-39
     of `docs/lean-bugs.md`; case `tasks/big_priority_dedicated`). No
     priority makes a task `sync`: only `depend`'s `sync` argument does;
   - `Task.get`/`IO.wait`: if the slot holds the value, that; otherwise
     `await_task(id, report)`, then read the slot. `await_task` is the
     rule: in a `sync` task (`in_sync_task()`) it calls `report` with
     `GET_IN_SYNC_TASK` first, and the glue prints it as a Lean panic
     (native's "`Task.get` called from a `(sync := true)` task",
     `wait_for`; `tasks/get_in_sync_task`); then it calls `wait(id)`. For
     `TaskId::FINISHED` it does nothing. Threads mode has the same
     function;
   - `IO.getTaskState`: `state(id)`; `IO.waitAny`: `wait_any(ids)`;
   - `IO.cancel`: `cancel(id)`; `IO.checkCanceled`: `check_canceled()`;
   - `IO.getTID`: `io::env::get_tid()` (with `io`): `gettid` plus
     `tid_offset()`, so the code gets the id of the OS thread it natively
     runs on (review AR-37, lean2rr's RS5-04). Natively
     (`task_manager`, `object.cpp`) a pool worker stays alive and takes
     the next pool task once idle, and a new worker starts only when none
     is idle (`enqueue_core`, 805-806); a dedicated task always gets a new
     thread (`spawn_dedicated_worker`, 873-883); a `sync` dependent runs
     on the thread that finishes its source (`handle_finished`), or
     resolves its promise; the event loop is one thread for the whole
     program (`libuv.cpp` 26). So `tid_offset()` is:
     - 0 in `main`, and in a `sync` task run on `main`'s thread;
     - in a pool task, its emulated worker's number (`running_worker`),
       the same for every task of that worker: pool tasks one after the
       other share one id, as natively they share the one idle worker;
     - in a dedicated task, a new number, even after every earlier task
       has finished;
     - in a `sync` task, the number of the thread below it;
     - in the event loop's callbacks, the loop thread's number, the same
       for every loop context.

     New numbers count up from 1 in the order the scheduler first needs
     them, as Linux hands out the ids of new threads, so a program's ids
     usually come out as native's do (`main`'s id plus 1, 2, ...). Each
     context keeps the number of the code running on it (`Ctx::tid`),
     which a pool or dedicated task replaces from its begin to the end of
     its run (`enter_worker`, `WorkerGuard`). `thread_number()` is not
     this number: it is the depth of nested tasks on the context, with
     which the scheduler tells the owners of locks and taken references
     apart (`sched::sync`, lean2rr's `refs`), and two tasks run one after
     the other at the same depth share it. Before AR-37 `IO.getTID` used
     it, so a dedicated task that followed a finished pool task got the
     pool task's id, and each new loop context an id of its own. Cases
     `tasks/get_tid_threads` (with its twin in threads mode) and
     `uvloop/get_tid_loop_thread`; unit tests
     `tid_offset_tells_a_dedicated_task_from_the_idle_worker` and
     `tid_offset_of_the_event_loop_is_one_thread`. The numbers are unique
     on one scheduler: with schedulers on several OS threads (an
     embedder, the crate's tests), `gettid` plus one thread's number may
     equal another thread's id. In threads mode `get_tid` is `gettid`
     alone, and `tid_offset()` is `thread_number()`;
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
     resolution stores. The glue tests whether the promise has a value
     inside `store` (which `resolve` calls only for an unresolved promise),
     never before the call: `resolve` makes a writers point first, during
     which another context can resolve the promise (review HR-01: lean2rr's
     glue tested its slot first and stored over another resolution; case
     `process/handoff_then_resolve_again`, which the driver's glue passes).
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

   The owner of a lock is a thread: the OS thread, the context on that
   thread's scheduler, and the thread number of the innermost task running
   on the context (`sched::sync`'s module comment). The OS thread tells the
   module initializers from `main` as natively: a `BaseRecursiveMutex` an
   initializer keeps locked is `main`'s to lock again when `main` runs on
   the initializers' thread (`LEAN_MAIN_USE_THREAD=0`, or a glue without
   `run_main`), and `main` waits for it forever on `run_main`'s thread of
   its own (with no other context, `hang_thread`: no panic). Before AR-39
   (lean2rr's review RS7-02) the owner held whether the scheduler had
   started instead of the OS thread, which was wrong both ways: with
   workers, `main` on the initializers' thread waited for good; with
   `LEAN_NUM_THREADS=0`, `main` on its own thread took the lock. Threads
   mode had the same flag next to its OS thread, and dropped it too. Unit
   tests `a_recursive_lock_is_the_os_threads_across_the_start`,
   `a_recursive_lock_from_another_os_thread_waits` and
   `relocking_from_another_os_thread_hangs`; driver case
   `rs7_init_reclock`.
7. **Waits of the glue's own objects.** The crate's wait cores do them
   ("The wait cores", core 3.1): a thunk or a static with room for 4 bytes
   holds a `Gate` (`step`, then `finish`, which makes the writers point
   before the store); a cell with no room uses the keyed functions
   (`step_keyed` for a constant, `wait_running_keyed` for a cell that says
   "running" without its runner, then `done_keyed` after the store and its
   `before_publish()`). A thunk being forced on another context waits for
   its store; a thunk forced inside its own computation hangs (lean-bugs
   LB-08: natively it spins forever; the other contexts go on). A glue
   object of another kind waits with a `WaitList` (`wait`, `wake_all`), or
   with `current_context()`, `block_sync()` and `wake(c)` directly.

   **A reference taken by `modify`** waits the same way: the crate's
   `sched::Ref<T>` in the glue's counted handle, or `sched::ref_keyed`
   around the glue's record (core 3.2). The semantics are
   Lean 4.35's (LB-01 and LB-18 in `docs/lean-bugs.md`):
   - `ST.Ref.modify` is `take`, then a store into the emptied reference
     (`ST.Prim.Ref.modifyUnsafe`), so the reference is empty while its
     function runs.
   - That function can block (a `Task.get` in it), and other contexts then
     run.
   - Only `modify`'s own store fills the empty reference. Until then `get`,
     `take`, `set` and `swap` wait, the taker's own included (review
     RS4-01): each is a blocking yield point. `set` is `swap` with the
     result dropped.
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
   natively), `refs/own_get_during_modify` (the taker's own read waits
   forever, as natively), `refs/set_during_modify` (LB-01) and
   `refs/swap_during_modify` (LB-18). The driver's `Ref`
   (`tests/sched-driver/src/lean.rs`) wraps `sched::Ref` in an `Rc`.
8. **Stack overflow.** Lean's report (`src/runtime/stack_overflow.cpp`) is
   the crate's (AR-11), behind the feature `stack-overflow` (with `sched`,
   or with `threads`, where every thread the task manager makes installs it
   at its entry; lean2rr enables it, leanrs decides at its adoption of
   `sched`, its DV6 until then). The glue calls
   `sched::install_stack_overflow_handler()` once per OS thread that runs
   Lean code, at that thread's entry, before any Lean code runs on it: the
   process's main thread for the initializers, and `main`'s thread if it is
   another one. The scheduler's contexts need nothing more: the hub
   publishes each context's guard at every switch (`publish`), and
   `sched::start` registers its own thread once the handler is installed.
   The first call comes from `main` (or later), after Rust's runtime has
   started, never from an ELF constructor: std's runtime start installs
   Rust's handler, and alternate stacks for the threads std spawns, only
   where it finds the default disposition, so installed before it, the
   crate's handler leaves a Rust thread that does not register without
   Rust's report (review SO-2). With a C-style entry (lean2rr's `leanrt`),
   std's runtime start never runs, and the previous action is the default.
   From then on:
   - a fault in the guard page below the stack of a registered thread, or
     of the context running on it, writes
     `\nStack overflow detected. Aborting.\n` to descriptor 2 and aborts
     (status 134, buffered output lost), as natively on any Lean thread;
   - any other fault goes to the action that was there before, called as
     the kernel would call it (its mask, `SA_NODEFER`, and the default
     restored first under `SA_RESETHAND`; review SO-1): Rust's handler,
     where Rust's runtime installed it (it reports an overflow of a Rust
     thread's own stack, with Rust's message), or the default (status
     139), as Lean's handler restores it.

   The handler is a native quirk with `unsafe` (`src/sched/stack_overflow.rs`;
   its proof in `docs/native-quirks.md`, "Lean's stack-overflow report":
   the alternate signal stack of each registered thread, no thread-local,
   lock or allocation in the handler, the window between a publication of
   the running context and the switch, the forward). The glue writes no
   `unsafe` for it (`tests/sched-driver/src/glue.rs`, built with the
   feature). `running_stack()` (with or without the feature) gives the
   running context's bounds, but a glue's own handler must not read it: its
   thread-locals are not async-signal-safe in every link mode.

   **Without the feature, or without the call,** a task that overflows its
   context's stack ends with a plain SIGSEGV, status 139, without Lean's
   message (Rust's handler knows only the guards of threads), and an
   overflow of a Rust thread's own stack gets Rust's message. Without the
   feature the crate compiles no `unsafe` for it, the hub updates no
   record (no cost at a switch), and `install_stack_overflow_handler` does
   not exist.

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
      (`spawn`, `depend`), `Std.Sync` operations (a lock, a try, an unlock,
      a `Condvar` wait or notify; so `Std.Channel` and the rest of
      `Std.Sync` too; the try functions since review HR-03, fixes-14), the
      glue's reference writes (`before_publish`, item 7), the end of each
      drain (the drain-end hook `after_drain`, below), the end of a task's
      job (`before_task_value`, item 3, and `run_task`) and of
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

    **The drain-end hook: `sched::after_drain()`** (reviews HR-01 to
    HR-03, fixes-14; part of the glue's contract). Natively the drop's
    `fclose` returns before the free goes on, so code after the free reads
    state that every other thread changed while the drop waited. Here the
    hand-off returns at once, and the context's next writers point waits
    for the writer while other contexts run: glue code that reads state
    after the free, reaches a writers point, then acts on what it read,
    acts on stale state. lean2rr's glue tested a promise, then resolved it
    over another context's resolution (HR-01); it tested
    `sync && taskDone(src)`, then `depend` (whose writers point let `src`
    finish) queued a `sync` dependent (HR-02); a try function read a lock
    with no writers point at all (HR-03). So each translator calls
    `after_drain()` at the end of every drain, where native's `fclose`
    would have returned:
    - when: once the drain's no-suspend scope has been left and its
      deferred resolutions have run (`run_deferred`), where a switch is
      allowed (the call may let other contexts run, as `run_deferred`
      may). `DrainScope`'s outermost drop calls both, in that order
      (leanrs's `Deep` and `Dyn` drains); lean2rr calls `run_deferred`,
      then `after_drain`, from its drained hook (review RF14-07: the order
      was the other one in the first version of fixes-14);
    - what: the writer threads of the streams the calling context handed
      off end (`join_own_writers`), while the other contexts run, so the
      bytes are delivered and every effect of the other contexts meanwhile
      is visible, as natively after `fclose`;
    - cost: one relaxed load while no writer runs in the process, so it
      is safe and cheap to call after every drain. The count of running
      writers is process-wide: while a writer of another context (or of
      another thread) runs, the call takes the slow path (a lock and a scan
      of the writers) and waits for nothing (review RF14-06);
    - so a drain's end now lets other contexts run whenever its context
      has a writer that runs: `DrainScope`'s outermost drop, and lean2rr's
      drained hook, switch then (they could already switch in
      `run_deferred`);
    - it waits for nothing inside a no-suspend scope (a drain nested in an
      outer one: the outer drain's end waits), while the context holds a
      stream lock, or while a panic unwinds; the next writers point waits
      then;
    - in threads mode it does nothing (a drop's `fclose` blocks its own
      thread there).

    **Deferred resolutions and the drain's writers** (review RF14-07).
    Natively a free (`lean_del_core`) reaches its objects in its order, so
    a promise reached before a stream is resolved, its `sync` dependents
    run, before the stream's `fclose` blocks; one reached after waits for
    it. Here both are put off to the drain's end: each deferred entry
    records a writer mark (`io::coop::writer_mark`, the id the next
    hand-off gets), and `run_deferred` walks the entries in push order;
    before each one it waits for the context's writers but the ones the
    drain handed off after the entry (ids from its mark up to the walk's
    start), and runs it with those skipped by every writers point, its
    `sync` dependents' included (`io::coop::skip_writers`; an exit, which
    natively flushes the still open stream, waits for them). Then
    `after_drain` waits for the rest. The list is moved out before any
    wait, so no entry is queued at a switch (R6). Case
    `process/deferred_resolve_before_handoff` (one free drops `#[stdin,
    p]`, so the promise first, as `lean_del_core` frees from the last
    element; a reader task waits for `p`, then reads a child's standard
    output; the child writes 70 000 bytes before it reads its standard
    input, whose last byte waits in the dropped handle): "reader got
    70000", "child 0", as natively; before, the drain's end waited for the
    writer before it resolved `p`, and the program hung (debug builds of
    the first version of fixes-14 aborted on R6 first). The other order,
    `#[p, stdin]`, waits for the writer before it resolves `p`, as natively
    `fclose` blocks first: that program deadlocks, natively too (unit test
    `deferred_resolutions_wait_only_for_the_writers_handed_off_before_them`,
    both orders with a pipe a thread drains). The rule gives native's outcome only when the translator hands its drops
    and deferrals to the crate in native's free order (`lean_del_core`'s
    LIFO, an array from its last element): the marks follow the order of
    the translator's calls. A translator that frees in another order (Rust
    drop glue, which drops an array from its first element, a struct in
    declaration order) gets the outcome that order implies, which can be
    native's mirror: `#[stdin, p]` then behaves as natively `#[p, stdin]`
    does. That is the translator's own free order (leanrs: its DV11), not
    the crate's.

    Cases (recorded natively): `process/handoff_then_sync_map` (the
    hand-off, then `IO.mapTask (sync := true)` over a task that finishes
    at 300 ms, whose function waits for another task: "dep 5 7", "main
    after mapTask"; before, the "`Task.get` called from a `(sync := true)`
    task" panic and the lines reversed), `handoff_in_tree_then_sync_map`
    (leanrs's form: the handle is dropped with the tree that holds it, in
    the driver an `Arr`'s `DrainScope`), `handoff_then_resolve_again` (HR-01:
    "B sees (some 2)", "main sees (some 2)") and `handoff_then_try_lock`
    (each try function right after a hand-off, with no drain-end hook in
    the port, while a task takes the lock at 150 ms: "false" four times;
    before, the try took the lock first, and the port hung); unit test
    `the_drain_end_hook_and_the_try_locks_wait_for_the_writer`.

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
    code that may do IO and wait. A translator defers the resolution of a
    promise dropped in its free (`sched::defer`) and runs the deferred list
    after the free has left the scope (`sched::run_deferred`, or the drop
    of the outermost `sched::DrainScope`), so that this code runs with the
    cooperative IO it would have anywhere else ("The wait cores", core 3.3:
    R1-R6; `resolve` inside the scope is a debug assertion).

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

## The wait cores (wait-1)

Three protocols that both translators used to write themselves are cores
of the crate since batch wait-1 (the owner's rule of 2026-10-05: each
translator keeps its own memory layout, and runtime logic lives in the
crate once). Each core comes as an object, for a translator whose values
have room for it, and as keyed functions, for one whose values do not
(lean2rr's Reussir records).

| Core | Items | Used by | Modes |
|---|---|---|---|
| 3.1 Wait for a computation that another context runs (`src/sched/wait.rs`) | `WaitList`; `Gate` and `Step`; `step_keyed`, `wait_running_keyed`, `done_keyed` | `Gate`: a translator's thunk cell or static (leanrs's `ThunkCell` and `LocalLazy`). Keyed: lean2rr's constant slots (`step_keyed`, `done_keyed`) and its `busy` thunks (`wait_running_keyed`, `done_keyed`) | single-thread only |
| 3.2 `ST.Ref` under Lean 4.35's rule (`src/sched/refs.rs`) | `Ref<T>` (also `Ref::empty`); `ref_keyed::{read_point, write_point, swap_point, take, wait, put, store}` | `Ref<T>` in a counted handle (leanrs's `Rc`; the drivers); `ref_keyed` around a record (lean2rr) | `Ref<T>` in both (threads mode's is `mt::Ref`, same API); `ref_keyed` single-thread only |
| 3.3 Deferred promise resolution (`src/sched/drain.rs`) | `DrainScope`, `Deferred`, `defer`, `deferred_pending`, `run_deferred` | `DrainScope` and `Deferred::Call` (leanrs's drains); `defer(Deferred::Resolve(id))` in a Reussir drain and `run_deferred()` at each drain's end (lean2rr) | both |

### Rules for all three

- **W1. Order.** Waiters wake in the order they began to wait (FIFO). A
  wake never switches: the storing context goes on until its next switch,
  as `sync::Mutex` and `Condvar` work. Natively the order is the OS's
  (spinning threads, futex wake-ups); FIFO is the deterministic choice. A
  wake (`sched::wake`, which the cores use) wakes only a context blocked in
  `block_sync` (`Wait::Sync`): a stale entry naming a context that went on
  to another kind of wait (after a panic, in a test) cannot cut that wait
  short (review RW1-08). The io layer's contended `flock`, which naps in
  `block_until` (`Wait::Sleep`) and looks again, is woken at an unlock in
  this process by an internal wake that accepts the nap
  (`wake_napping`; review NEW-1). The driver program `w1_flock_handoff`
  checks that the unlock itself ends the waiter's wait (two contexts
  contend for one lock; `io::flock_last_wake`, a hidden test hook, records
  whether the unlock found the waiter able to run, so no clock is read).
- **W2. No borrow across a wait or translator code.** No borrow of the
  crate is held across a wait, a drop of a translator value, or translator
  code. The one exception is `T::clone` inside `Ref::get`'s borrow.
- **W3. No block inside a no-suspend scope.** A core that must block (a
  wait or a hang) inside the scope reports a Rust `panic!` with the
  reason, `sched::WAIT_IN_NO_SUSPEND` ("lean-runtime: a wait inside a
  no-suspend scope (a free)"), as the io layer does for a stream a
  suspended context holds (`io/coop.rs`). The `extern "C"` keyed
  functions cannot unwind, so there the panic aborts (status 134, with the
  reason and "panic in a function that cannot unwind"); both translators
  build with `panic = "abort"` anyway. The fast paths, the claims and the
  wakes never block, so they are allowed in the scope. It is unreachable
  from Lean code ("W3 is unreachable from Lean code" below).
- **W4. Fast checks inline, slow paths out of line.** A fast check is one
  `const` thread-local load (`done_keyed`, the `ref_keyed` points,
  `deferred_pending`). The slow functions lean2rr's prelude calls are
  `extern "C"`, so they cannot unwind and the inline points need no
  cleanup path.
- **W5. Keys are plain data.** A key is a `usize` the crate never
  dereferences. It names a live object while it has an entry, because the
  runner and every waiter hold a reference to the object. An object's key
  is its address, which is even, and debug builds check it (`Gate::step`,
  `wait_running_keyed`, and `ref_keyed`'s `take`, `wait`, `store` and
  `put`). A translator that keys an index
  (lean2rr's constant slot) passes `(index << 1) | 1`, so the two spaces
  never meet. One object uses either a `Gate` or the keyed functions,
  never both.
- **W6. Safe at thread teardown.** The cores use `try_with`. Once the
  thread's locals are gone (a translator's thread-local destructed at
  exit), a wake does nothing and a wait hangs the thread. W6 comes before
  W3: their test of the scope is `in_no_suspend_scope()` (false once the
  locals are gone), not `in_no_suspend()` (true then, for the io layer). At teardown the thread counts as
  `main`'s context: `Gate::step` and `step_keyed` claim a computation with
  no runner known (`step_keyed` also when its table is gone), and a
  recorded runner makes the thread hang (review RW1-09). Core 3.3's list
  has no destructor, so a drain at teardown still defers (below).

Every wait of the cores is `block_sync` (`Wait::Sync`) and every hang is
`hang()` (`Wait::Forever`): the context keeps its worker, as a native
thread that spins on a thunk, on an empty ref or on a constant's lock
keeps its own. Without a task manager, a wait or a hang with no other
context blocks the thread for good.

### 3.1 Wait for a computation that another context runs

Native: `lean_thunk_get_core` (`object.cpp` 540-565) lets the first forcer
run the closure; every other forcer spins until the value appears, the
forcer itself included if its own closure forces the thunk (LB-08: the
program hangs, the other threads go on). `lean_obj_once_cold` (2896-2904)
takes a lock around a constant's initializer: a second thread waits for
it, and re-entering on the same thread deadlocks.

A computation has a value, or it does not. Without one it may have a
runner, the context that claimed it and runs it (a task run on the stack
of the context that waits for it counts as that context):

| Situation | What happens |
|---|---|
| The value is stored | The caller reads it: its own fast path, no core function |
| No value, no runner | The running context becomes the runner (`Step::Run`, `step_keyed` true). It computes the value, then finishes: `Gate::finish(key, store)`, or its own writers point (`before_publish()`), store and `done_keyed(key)`. That clears the runner and wakes the waiters (W1) |
| No value; the runner is the running context | `hang()` (LB-08). Before the task manager runs, the thread hangs |
| No value; another context is the runner | The context registers under the key and waits (`block_sync`), then looks again (`Step::Again`, `step_keyed` false) |
| No value; the cell says "running" but records no runner (lean2rr's `busy`; `wait_running_keyed`) | If the keyed table records a runner, the two rows above apply. Otherwise, before the task manager runs or with no other live context, the running context must be the runner: `hang()`. Otherwise it waits; if it is in fact its own runner it waits forever, which keeps its worker as the hang does and cannot be told apart from it (the judge's verdict on audit divergence 2) |
| Any wait or hang above inside a no-suspend scope | A Rust panic (W3) |

**A Rust panic out of the computation** leaves the runner recorded:
nothing clears it while the panic unwinds, so a later step on the same
context hangs, and on another context waits forever (as leanrs's
`thunk.rs` did). Both translators end the process on a Rust panic, so this
is not a case (leanrs's review, text fix 1).

**The writers point.** `Gate::finish` calls `before_publish()` before the
store, so a waiter never sees the value before the streams the
computation's drops handed off are delivered (natively its `fclose`s
returned first). For a static (leanrs's `LocalLazy`, and its `Lazy`,
which stays its own) this point is new, observable and native-matching
(leanrs's proof review, F1). With the keyed functions the writers point is
the caller's, before its store (lean2rr's `l2r_lcell_set` makes it).

**`Gate`'s contract**, which leanrs's `unsafe impl Sync for LocalLazy`
relies on (proof review F5): `Gate::new` is a `const fn`; `Gate` is
`Send` (automatically), not `Sync`, and has no `Drop`; its methods touch
only the gate's own cell and the calling thread's thread-locals (the keyed
table, the scheduler's state), never dereference the key, and hold no
borrow while `finish`'s `store` runs or while the context waits. It is 4
bytes (`Cell<u32>`, `u32::MAX` for no runner): leanrs's thunk cell for a
`u64` stays 40 bytes.

**The keyed table** is per thread: `{ key, runner, waiters }` entries, and
a `const` count for `done_keyed`'s inline test. An entry lives from a
keyed claim or a first wait to the value's store, so the table usually
holds no entry, or a few during a constant's initialization (one per
constant whose initialization is in progress). Its first 8 entries are in
places of the thread-local itself, the others in a `Vec`, so a claim and
its store with no waiter allocate nothing (AR-40; `ref_keyed`'s table
keeps 4 the same way). An allocation there on lean2rr's `main` thread,
at its first claim of a constant, had shifted the layout of its heap (one
more 2 MiB huge page at the peak of a benchmark). `tests/keyed_alloc.rs`
checks both tables with a counting allocator. The table keeps its
destructor (the waiter lists), so at thread teardown it is gone, as W6
says.

**Before the task manager runs** the running context is `main`'s, found
without building the scheduler's state, as the frame of 3.2 is: there is
one context, since a task runs at once on the context that creates it
(also with `LEAN_NUM_THREADS=0`). A claim (`step_keyed`, `Gate::step`)
and a store that no context waits for (`done_keyed`, `Gate::finish`,
`put`, a keyed closing store) touch only the core's own table and cell. So
a program that creates no tasks builds no scheduler state at its
constants and thunks (lean2rr claims each constant through `step_keyed`,
before `main`); a wait or a hang builds it.

```rust
// leanrs's thunk (the cell holds `gate: Gate`; key: the cell's address)
loop {
    if let Some(v) = c.value.get() { return v }
    match c.gate.step(key) {
        Step::Again => continue,
        Step::Run => {
            let Some(f) = c.f.take() else { sched::hang() };
            let v = f();
            c.gate.finish(key, || { let _ = c.value.set(v); });
        }
    }
}
// lean2rr's constant accessor, its cold path (key: (slot << 1) | 1)
loop { if has(slot) { return true } if sched::step_keyed(key) { return false } }
// ... the caller computes, then: before_publish(), the store, done_keyed(key)
```

### 3.2 `ST.Ref` under Lean 4.35's rule

The rule is `docs/threads.md` 3.1 (LB-01, LB-18), in both modes:

| Operation | Full | Empty (taken) |
|---|---|---|
| `get` | a clone, made inside the borrow | wait for the closing store, then the same |
| `take` | the value moves out, the reference is empty | wait, then the same |
| `set` | `swap`, the old value dropped after the borrow | wait |
| `swap` | the old value out, the new one in | wait |
| `put` (`modify`'s closing store) | a glue error: a debug assertion, then a replace | fill, then wake every waiter (W1) |
| keyed `store` (a `set` or `swap` whose role is decided at run time) | not taken: the plain store follows | taken in the running frame: the closing store. Taken elsewhere: wait |

- The taker's own `get` and `take` during its `modify` wait too: forever,
  as natively the taker's own `get` spins forever on its multi-threaded
  ref (review RS4-01, accepted by lean2rr as L1; case
  `refs/own_get_during_modify`). Natively the program hangs there; a value
  read from the empty cell would be one the program never stored.
- A store from code nested inside `modify`'s function (a `sync` dependent
  of a promise the function drops, a task run on the taker's stack) waits,
  as in Lean 4.35, where natively 4.34 loses it (LB-01).
- `get`, `take` and `swap` call `ref_read()`; `set`, `swap`, `take` and
  `put` call `before_publish()`; both come before any borrow.
- `Ref<T>` is leanrs's `st.rs` cell moved as it is: every borrow begins and
  ends inside one method, a wait registers with no borrow of the content
  held (`WaitList::wait`), a replaced value is dropped or returned after
  the borrow, and `put` takes the waiters in a borrow of their own. Its API
  is `mt::Ref`'s: `new`, `empty`, `get` (`T: Clone`), `take`, `put`,
  `swap`, `set`, `modify`, `modify_get`.

**The keyed form** keeps the taken references in a thread-local table,
under the record's address. The translator keeps the value in its record,
and makes its own cell operation right after the call:

| Operation | Calls, then the cell's operation |
|---|---|
| `get` | `if read_point() { wait(key) }` |
| `set` | `if write_point() { store(key) }` |
| `swap` | `if swap_point() { store(key) }` |
| `take` | `take(key)` (both points included); the cell holds a placeholder until the closing store |
| `modify`'s closing store | `if write_point() { store(key) }` (the frame decides: lean2rr's option B, chosen in L2), or `write_point(); put(key)` where the translator knows the store statically (`put` makes no writers point of its own) |

The points are one `const` thread-local load past `ref_read` and
`before_publish` (W4); the slow functions are `extern "C"`.

**The frame** decides which store closes a take: the running context, the
number of tasks running on it, and the innermost one's entry and
generation. `modify`'s take and its closing store run in one frame.
Everything the scheduler runs nested inside `modify`'s function (a task run
on the waiting context's stack, a `sync` dependent of a promise resolved or
dropped there) passes through `run_task`, which begins it at a deeper
frame; thunks and constants forced there are pure code and reach a
reference only through such tasks. So `store` picks exactly `modify`'s
own closing store in safe code. In `unsafe` code a second store in the
taker's own frame closes the take early, where a static analysis would
mark the same store. Before the task manager runs the frame is `main`'s
outside any task, found without building the scheduler's state (review
RS4-02); it is the same frame after the start, so a `modify` across a lazy
start (lean2rr's) still closes.

### 3.3 Deferred promise resolution

Native: when the last reference to an unresolved promise goes inside a
free, `lean_del_core` resolves it with `none` right there
(`deactivate_promise`, `object.cpp` 1353-1357), and its `sync` dependents
run inside the free, on the freeing thread, before the free reaches the
next object. A free inside a dependent is a new `lean_dec_ref_cold`, so
the promises it frees are resolved inside that dependent.

A translator's free must not suspend (lean2rr's Reussir drain is the
thread's, review RS4-04; leanrs's drain must not switch, Lem-Deep (6)),
but a `sync` dependent is Lean code that may block. The rules:
- **R1.** Every step that runs inside a drain runs in the no-suspend scope
  (`DrainScope::enter` enters it at the outermost drain).
- **R2.** Nothing in the scope suspends: the writers points do not wait
  there, a dropped stream's close hands its bytes off (item 11), the wait
  cores panic instead of blocking (W3), and `resolve` asserts in debug
  builds that it is not called there (`sched::RESOLVE_IN_NO_SUSPEND`).
- **R3.** A promise resolution reached inside a drain is deferred
  (`defer`), never run there. The slot's store may happen in the drain,
  in the drain's order (lean2rr: `Deferred::Resolve(id)` after its store);
  or with the resolution (leanrs: `Deferred::Call`, its closure stores and
  calls `resolve`). The two differ once an earlier entry's dependent
  blocks: other contexts then run, and with the store made in the drain
  they see a later promise's `none` (in its slot; `IO.getTaskState` says
  `finished`) before its dependents have run, where natively that promise
  is not resolved yet; with the store made with the resolution they see it
  unresolved, as natively (review RW1-05). Each translator decides at its
  adoption: lean2rr keeps its shape with a note in its plan §10, or moves
  the store into the resolution.
- **R4.** The deferred list runs only after the drain has ended and the
  scope has been left, through `run_deferred()`: `DrainScope`'s outermost
  drop calls it; a translator with drains of its own calls it at each
  drain's end (lean2rr: its `drained` hook, Reussir patch 0040).
  `leave_no_suspend` never runs it (AR-8 stands). While a panic unwinds,
  nothing runs and the entries stay queued for the next `run_deferred()`.
  Each entry records a writer mark at its `defer`; before it runs, the
  walk waits for the context's writers but the ones its drain handed off
  after it, and it runs with those skipped, as natively the free resolved
  the promise before their `fclose`; the drain-end hook `after_drain`
  then waits for the rest (review RF14-07; item 11 of "The glue",
  "Deferred resolutions and the drain's writers"). The rule gives native's outcome only when the translator hands its drops
  and deferrals to the crate in native's free order (`lean_del_core`'s
  LIFO, an array from its last element): the marks follow the order of
  the translator's calls. A translator that frees in another order (Rust
  drop glue, which drops an array from its first element, a struct in
  declaration order) gets the outcome that order implies, which can be
  native's mirror: `#[stdin, p]` then behaves as natively `#[p, stdin]`
  does. That is the translator's own free order (leanrs: its DV11), not
  the crate's.
- **R5.** The list is per thread; each entry is tagged with the context
  that deferred it, and `run_deferred()` moves out the running context's
  entries only before it walks them. Entries run in push order (the order
  of the last drops), on the dropping context. After each entry, the
  running context's entries queued meanwhile run before the walk's next
  entry (lean2rr's step-4 `run_later` shape, review RW1-01). So a drain
  inside a deferred resolution resolves its own promises before the outer
  walk's next entry, at its own end or, if the translator did not report
  that end, right after the dependent: native's order. The guarantee,
  whatever order a translator's free drops values in: `pC` (dropped by
  `pA`'s dependent) is resolved before `pB` if and only if `pA` is (the
  judge's nested-free verdict; case `tasks/promise_nested_free_order`,
  recorded natively: `C, A` and `C, A, B`). If a dependent never returns
  (`Promise.result!` of a dropped promise), the rest of the moved entries
  never runs, as natively the free never resumes; the thread's list is
  clean for the other contexts.
- **R6.** The list is empty at every context switch and when a context
  ends; debug builds check both (`switch_away`, and the end of a context's
  function). It holds for drains that never switch (leanrs) and for a
  translator that runs the list at every drain's end before its context
  can block or end.

**A drain whose end is not reported** breaks R6: lean2rr's Reussir drains
without its patch 0040, where its fallback runs the list at the next
effect point. Debug builds report it at the next switch, or when the
context ends. In release builds the entries stay queued, tagged with their
context: they run at that context's next `run_deferred()` (a drain's end,
a settle point), never on another context; if the context ends first,
they run there, at its end (and a later context that reuses its number
never sees them). Until they run, their promises look unresolved and their
dependents have not run, where natively they ran inside the free; `main`'s
run at `finish` at the latest (its end as a context, which debug builds
also check), and never if the program exits first (`IO.Process.exit`). **lean2rr must make patch
0040 required (its L5) before it adopts this core** (review RW1-01).

**The drain depth at a switch.** A `DrainScope` held across a switch (a
glue bug: the scheduler's own waits, a sleep or a promise, still suspend
in the scope; B1) is set aside with the no-suspend depth (review RSIO-10),
so the other contexts are neither in the drain nor in the scope, and the
context is in both again when it goes on (review RW1-02).

**At thread teardown** the list stays: it has no destructor
(`ManuallyDrop`), so a drain in a thread-local's destructor still defers,
and its scope's end runs the entry after the drain, outside the scope
(review RW1-03). Entries still queued when the thread ends are neither run
nor dropped, as native Lean frees nothing at exit.

**The in-flight count.** `deferred_pending()` counts the entries queued
and the ones a walk has moved out but not finished (one whose dependent
blocked, and the rest of that walk behind it; one that never returns stays
counted). So a "has every task settled?" test (lean2rr's `settled`) sees a
promise moved out but not yet resolved (the judge's caveat on the
nested-free fix); after a walk whose dependent never returns, it stays
true for good (those promises are never resolved: only the cost of a
skipped shortcut). `DrainScope`'s drop tests a count of the queued entries
instead, so a walk that hung does not make every later drain call
`run_deferred()`, which is out of line (review RW1-06).

**A panic.** A Rust panic unwinding through a `DrainScope` runs nothing
and leaves the entries queued; a panic out of an entry of a walk puts the
entries not yet run back on the thread's list, after the ones queued
meanwhile (those came from inside the entry). The next `run_deferred()`
runs them. leanrs's drain used to run them while unwinding; its binaries
abort on a panic, so only tests see the difference, and a test that
catches such a panic and then switches makes R6's check fire (accepted:
leanrs's proof review, F2).

```rust
// leanrs: a promise's last handle, inside a drain
if DrainScope::active() { sched::defer(Deferred::Call(Box::new(resolve))) } else { resolve() }
// lean2rr: inside a Reussir drain, after the slot's store
sched::defer(Deferred::Resolve(id));
// ... and at each drain's end (its `drained` hook), outside the drain
sched::run_deferred();
```

In threads mode the depth and the list are the thread's; the resolutions
run on the dropping thread after its drain, through threads mode's
`resolve`. `Deferred::Call` needs no `Send`: it runs on the thread that
pushed it.

### W3 is unreachable from Lean code

lean2rr accepted W3's panic only if no correct program reaches it (L6).
The argument:
1. Both translators enter the no-suspend scope only for frees (lean2rr:
   its stream close and the deferred step of a promise's drop inside a
   Reussir drain; leanrs: its drains, through `DrainScope`).
2. A free runs no Lean code: the drop glue releases values. The one way
   Lean code runs inside a free natively is a promise's deactivation,
   whose `sync` dependents run there. R3 defers that resolution past the
   drain, and R2's debug check in `resolve` reports any resolution left
   inside the scope.
3. Every wait core is reached only from Lean code: a thunk's force, a
   constant's read, a reference operation. So none is reached inside the
   scope.

The checks: the debug assertion in `resolve` (R2); the case
`tasks/promise_nested_free_order`, whose driver ports (both modes) free
containers of promises with `sync` dependents (a reference write, an
output) and assert in each dependent that it runs outside every drain and
scope; the driver program `w1_dependent_waits`, where such a dependent
reaches a wait core (a reference another task's `modify` holds) and waits
after the drain, not in it; and W3's own tests (unit tests through
`WaitList`, `Gate` and `Ref`; `w1_w3_keyed` through the `extern "C"`
functions).

### How the cores are tested

- Unit tests (`src/sched/wait_tests.rs`, single-thread; Miri runs the ones
  that start no context, but for the model test below, which leaks by
  design and runs under Miri with `-Zmiri-ignore-leaks`): a gate's claim
  and finish; W3 through `Gate`,
  `WaitList` and `Ref`; keyed claims with odd keys; `main` waiting for
  another context's run, and woken by `finish` or `done_keyed`; `Ref`'s
  operations, every value dropped once, `modify` in place, the debug check
  of a put into a full reference; the keyed take and its closing store; a
  task run inside `modify` at a deeper frame; leanrs's model test of its
  drain (`beh_drain_defers_promise.rs`, 2000 random value graphs) against
  the real `DrainScope`, `defer`, `run_deferred` and `resolve`. The drain
  tests' bodies (`src/sched/drain.rs`) run in both modes: drop order, a
  nested drain first, the in-flight count, a panic through a drain, a
  panic out of an entry, `run_deferred` and `resolve` inside a scope,
  nested scopes; and, single-thread, R6's check. `common.rs` checks that
  `sched::Ref` has one API in both modes. The review's repros, as
  regression tests (`rw1_*`): a drain depth held across a switch is not
  seen by the other contexts (RW1-02); a drain at thread teardown defers
  and runs its entry outside the scope (RW1-03); an entry an unreported
  drain left runs on its own context, at its end, and debug builds report
  it there (RW1-01); an unreported inner drain's entries run right after
  their walk entry (RW1-01).
- Driver programs (`tests/sched-driver/src/wait1.rs`, where several
  contexts wait): a thunk on three contexts with FIFO wakes and D8's probe;
  a self-forced thunk (and the exit that waits for it); a static read on
  two contexts; lean2rr's constants and `busy` thunks (alone, and before
  the task manager); W3 through the keyed functions; the keyed reference's
  frame rule (its own store closes; a dependent's store, a stack task's
  store, a nested take and the taker's own `get` wait); a dependent that
  waits after the drain; a deferred resolution that hangs; `main` waiting
  inside its own no-suspend scope while a reader reads cooperatively
  (`rsio_ns_leak scope_wait`, review RW1-04); a contended `flock` handed
  off at the unlock, not at the waiter's nap's end (`w1_flock_handoff`,
  review NEW-1); and leanrs's
  evidence `beh_ref_empty_cell.rs` on the real types: 3000 random schedules
  of reference operations on contexts of their own, against Lean 4.35's
  store model, for `Ref<T>` and for `ref_keyed`.
- Cases, in both drivers: `refs/*` through the crate's `Ref` (the
  single-thread driver's own copy is gone), `refs/own_get_during_modify`,
  and `tasks/promise_nested_free_order` (the drivers' `Arr` frees in a
  drain, and their promises defer).

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
  If `main` was in a hook's cold path, which cannot unwind (review AR-28;
  "Rust panics" below), the process aborts there instead.
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
   block until `modify`'s own store (item 7 of "The glue"; LB-01, LB-18;
   `sched::Ref` or `sched::ref_keyed` do it). This is not about soundness
   either, but about Lean's semantics.
8. A promise dropped in a free is resolved after it (`sched::defer`, then
   `run_deferred` or `DrainScope`'s drop): nothing waits inside a
   no-suspend scope ("The wait cores", W3 and R1-R6).

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
  and `uvloop/` and the io and process cases with tasks, the review regression
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
  detection was off because the driver's glue leaked its 64 KiB alternate
  signal stack on purpose, the only report then; the crate's report
  (AR-11) makes one only for a thread that has none, which Rust's threads
  never are.

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
| Walks of promises dropped inside a Reussir free (`later`) | The deferred resolutions (`defer`, `run_deferred`, core 3.3): the list moved out before its walk, so a free inside a dependent resolves its own promises first | Native's order (the judge's nested-free verdict) |
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

## Known differences from native

lean-runtime's own deviations of the deferred model, where native Lean is
right and the crate's outcome is another one (not a Lean bug: those are in
`docs/lean-bugs.md`). Each has a case whose expected files are native's,
with the crate's outcome as a hand-written alternative (`alt1`) and
`deviations = { lean_runtime = "LSCHED-nn" }` in its `.toml`, so `check`
accepts both (`tests/cases/README.md`).

| Id | What | Native | Here | Why | Case |
|---|---|---|---|---|---|
| LSCHED-01 | Runaway pure tasks (`Task.spawn` of a computation that never ends), queued before an IO task, when they would take every worker (for example one, at `LEAN_NUM_THREADS=1`; leanrs's DV26 (b), reviews AR-15, RS4-02, LF3-03) | The workers take the pure tasks first (first come, first served) and never finish them: the IO task never runs, and the exit waits forever (or `main` does, if it waits for the IO task) | An IO task does not wait for pure tasks no IO task waits for: a worker only marks such a task started ("The pure-task rule"), and since AR-25 an IO task also passes over the pure tasks that wait in the queue for a worker that started pure tasks keep. So the IO task runs during `main`'s next wait. A started runaway task runs at the exit, which then waits forever; a passed-over one is still queued, so if the program drops it, it is deleted and never runs, and the program can end where native hangs | The pure-task rule: a pure task no IO task waits for is deferred, so that a runaway one cannot take the only thread from `main`, which natively goes on in parallel (`tasks/runaway_pure_task_started`); a pure task has no effects, so the deferral shows only in what other tasks a stalled worker would have kept from running | `tasks/runaway_pure_task_before_io` (native: `main done false false`, then a hang; here: `io task ran` first, then the hang, `alt1`); `tasks/runaway_pure_passed_over` (one worker; `p0` finite, then runaway `p1`, kept, then an IO task; `main` drops `p1` after 300 ms and waits for the IO task; native: nothing, then a hang; here: `io`, `done`, status 0, `alt1`) |
| LSCHED-02 | A waiter needs a worker that started pure tasks keep, and the oldest of them never ends and reaches no polling point, effect point or zero sleep (two workers or more; reviews AR-25, LF3-01, LF3-04) | Another worker finishes its task and takes the awaited one: the waiter goes on, and the exit waits forever for the runaway task | The oldest started pure task runs on the one thread to free its worker (`needed_picked`) and never ends: nothing after the wait happens | One thread runs one task at a time and cannot tell which started task would end first; the oldest has run the longest. The program hangs either way (a started task runs to completion before the exit); only what it does before the hang differs | `tasks/runaway_pure_before_awaited` (native: `t = 1001`, `p finished: false, q finished: true`, then a hang; here: nothing, then a hang, `alt1`) |
| LSCHED-03 | A waiter needs a worker that started pure tasks keep, while a context that holds a worker sleeps, and the oldest started task reaches no yield point (two workers or more; reviews RF3-02, LF3-05) | A race: the sleeper's worker takes the awaited task when the sleeper wakes and ends, a started task's worker when that task ends; whichever comes first | The oldest started task runs at once for the waiter (AR-25), so the awaited task comes after that task's run, whatever the sleeper does. With yield points in the started task (reference reads, clock reads, outputs, zero sleeps) the hub resumes the due sleeper there, and its worker takes the awaited task, as natively (LF3-01, LF3-04) | One thread cannot know how long a started pure task takes, and cannot preempt it. Waiting for the sleeper's wake instead (RF3-02's fix) idled the only thread for as long as an unrelated sleeper slept, so a watchdog fired where native ends (LF3-05, `tasks/picked_task_watchdog`), and it was reverted: a delay by the started task's own run time is the admitted cost | `tasks/picked_task_sleeping_worker` (native: `t` while `p` still runs, `p finished then: false`; here: `t` after `p`'s run, `true`, `alt1`) |
| LSCHED-04 | A UV extern made within a loop wake-up after a timer came due (review RF14-02) | A race: the extern takes the loop's lock first, before the loop thread has woken for the timer, almost always; a slow extern lets the timer's callback run first | The same race, decided by elapsed time: the catch-up leaves a timer due less than `LOOP_LATENCY` (1 ms) ago, and runs one due earlier; threads mode has native's race | One thread decides by the clock what native threads decide by who gets the lock first; both orders are native's | `uvloop/timer_fresh_next_twice` (native: "true, true" and "false"; the other outcome of each line, alone or together, as `alt1` to `alt3`, accepted in every mode) |

The other places where the crate's schedule is one of native's but may
differ from the most frequent one are "Schedules that depend on the
machine's speed" (above) and "The limits of one thread" (below).

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
  context's guard at every switch, none on `main`'s context: in
  thread-local atomics (`running_stack()`), and in the record of Lean's
  stack-overflow report (`src/sched/stack_overflow.rs`, AR-11).
  - The handler and the abort are the crate's, opted into by the glue
    (item 8 of "The glue").
  - `tasks/stack_overflow_in_task` matches native: the task overflows on a
    context, `\nStack overflow detected. Aborting.\n`, status 134, stdout
    not flushed; the driver's `so_*` tests check `main`'s own stack, a
    second scheduler thread, a fault that is no overflow and the forward
    to Rust's handler.
  - corosensei's own trap API (`CoroutineTrapHandler`) is `unsafe` and not
    needed.
- **Rust panics.** With unwinding (`panic = "unwind"`, Rust's default),
  S6: a panic in a context goes on as a panic of `main`: Rust's message,
  then whatever the glue does with a panic in `main` (status 101 in the
  driver). The driver test `a_rust_panic_in_a_context_goes_on_in_main`
  checks it. A glue that catches it goes on without the tasks and walks the
  panic unwound (S6). A panic in `switched` or `idle` aborts. So does a
  panic in the cold path of an inlined hook (review AR-28): those cannot
  unwind (`extern "C"`: `effect_check`, `poll_check`, `ref_read_poll`,
  `release_live`, and io's `join_own_writers_slow`). A panic raised in
  one, or a context's panic resumed in `main` while `main` waits in one
  (the yield of an effect or polling point, a release whose dropped job
  blocks, the wait for a writer), aborts there with Rust's "panic in a
  function that cannot unwind", status 134 (driver test
  `a_rust_panic_resumed_in_a_hooks_cold_path_aborts`). Both translators
  call the hooks from FFI code (lean2rr's textures) or build with
  `panic = "abort"` (leanrs), where such a panic aborts anyway. With
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
real threads. Since T1, threads mode exists beside it, as a separate
scheduler (feature `threads`, `docs/threads.md`); this section is what the
single-thread scheduler does per thread, and what threads mode had to
change.

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
- The published stack bounds (`running_stack`) and the hub-hook flag. The
  stack-overflow report keeps one record per registered thread, found by
  the faulting thread's `errno` address, so a fault on any thread reads
  that thread's bounds.
- The soundness argument (S1-S7): a scheduler resumes only its own
  coroutines, on its own thread.
- Thread numbers (`thread_number`) are 64-bit: a worker context's number
  from a process-wide counter, times 2^32, plus the depth of nested tasks
  on it. They stay unique across threads, and neither wrap nor run into
  each other (review RS1S-07). `IO.getTID`'s numbers (`tid_offset`, review
  AR-37) are each scheduler's own, and are added to its thread's `gettid`.
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
  thread registers (`install_stack_overflow_handler`, or `start`): its
  alternate signal stack and its record.
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
- **The hooks inline into the translator's code** (review AR-23), so a
  crate built without LTO calls nothing on its hot path: `before_publish`
  and `before_task_value` are one relaxed load (the count of running
  writers); `poll` and `effect` are that load and `coop_possible()`'s until
  the program has a task, a promise, a timer or a watch; `ref_read` is the
  flag's load, then the countdown; `release` of a finished task's id is a
  comparison; the no-suspend scope is a thread-local counter;
  `manager_running`, which a translator asks at every task creation, is a
  thread-local flag's load (review AR-29). What follows
  is out of line (`#[inline(never)]`, and `#[cold]` for the writers' wait
  and every 1000th read), and cannot unwind (`extern "C"`, review AR-28):
  a caller that holds values with destructors across a hook then needs no
  cleanup path for the call. With one, LLVM priced lean2rr's
  `l2r_lcell_set` at 285 against its threshold of 225 and stopped inlining
  it into the generated task code. A Rust panic in a cold path aborts, as
  at an FFI boundary ("Rust panics" above). Checked on a probe crate's
  LLVM IR (each hook called with a `Box` held across it): every call of a
  cold path is a `call`, where before it was an `invoke` with a cleanup
  landing pad.
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
- **Per-task cost** (review AR-27). Per live task:
  - its slab entry, 56 bytes (80 before AR-27: the queue time is a 32-bit
    count of microseconds instead of an `Option<Instant>`, the thread
    number keeps only its low half, which shares a word with the polling
    counts, and the priority is the top byte of the flags; the unit test
    `the_slab_entry_is_56_bytes`). The entries are in chunks of 1024 that
    never move, so the slab never doubles: no unused half, and no copy of
    every entry when it grows, which mimalloc's `realloc` makes with both
    blocks resident;
  - its boxed `Job`: the closure's size, rounded up by the allocator (no
    allocation for a closure that captures nothing);
  - while queued, an 8-byte item in its priority's queue (a `VecDeque`,
    which grows by doubling); while started and not run, an 8-byte item in
    the started list, compacted when its stale items outnumber the others;
  - the translator's slot.

  A thin job (a function pointer and a word, without a `Box`) was
  considered and left out (fixes-3; again for AR-35 in perf-1). It would
  save one 16-byte allocation per task for a translator whose job
  captures one word (lean2rr's captures its slot and serial). No safe form
  keeps the entry at 56 bytes:
  - as a second form of `Job`, it makes the optional job 24 bytes: the
    compiler uses one spare value of the boxed form's fat pointer (a null
    pointer), which `Option` takes, and the thin form's two words leave
    none for the tag. Every entry would grow to 64 bytes, 8 more per task,
    boxed jobs (leanrs's) included;
  - a thin form of one word (a function that finds its data through the
    task's id) makes a 16-byte `Job`, but its `Option` is 24 bytes again:
    the one spare value goes to the form's tag;
  - 16 bytes with both forms needs `unsafe` (the tag in a pointer's spare
    bits).

  A thin form would also need a second function for a job the scheduler
  drops without running it (lean2rr's job is a guard whose drop marks its
  task unrun), which a closure's destructor does now.

  Probe (2026-10-04, a scratch program, not committed): TaskHeavy's first
  two phases (100 000 live pure tasks, awaited in order, then a
  100 000-long chain of `Task.map`) through the crate with 20 workers.
  Peak RSS, 3 runs each, equal within 0.1 MB: with mimalloc 40.9 MB at
  31c7bfa, 43.0 MB with AR-25 alone (its tasks stay queued, so the queue
  grows, and the started list kept its stale items), 28.6 MB with AR-27;
  with glibc's `malloc`, 20.3 MB, 21.0 MB and 17.9 MB. The lone-worker
  model reads the clock once per spawn while a worker is waking.
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
