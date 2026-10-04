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
| `src/sched/env.rs` | `LEAN_NUM_THREADS`, the number of processors, `LEAN_STACK_SIZE_KB` |
| `src/sched/sync.rs` | `Std.Sync`'s mutexes and condition variable |
| `tests/sched-driver/` | Every case of `tests/cases/tasks`, `tests/cases/sync` and `tests/cases/refs` as a Rust program over `sched`, with the glue a translator writes |

`sched` depends on corosensei 0.3.4 and is built with cargo, offline, from
the committed `Cargo.lock` (`cargo build --offline --locked --features
sched`; docs/development.md, "Builds").

## The model

Native Lean runs tasks on a pool of worker threads. Here one thread picks
one of the schedules the pool can produce. A worker may start a task at any
time after it is created, and must have finished it when its value is
needed. So a task is *deferred*: it runs at the first of these points.

- **It is needed.** `Task.get` or `IO.wait` (`wait`) runs it right there,
  on the stack of whoever needs it, as a worker would while the caller
  waits. A task whose sources are still pending first runs that chain, from
  its deepest end, one task after the other.
- **The running code blocks.** A sleep, a lock, a promise, or a task running
  elsewhere blocks it, and one of the task manager's workers is free
  (`LEAN_NUM_THREADS`, or the number of online processors). The task then
  starts on a *context* of its own.
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
1. a context that can go on, in the order they became able to;
2. a queued task, on a new context, if a worker is free;
3. a pure task a worker has started (below), when nothing else will ever
   happen;
4. otherwise it waits for the earliest sleeper, or forever (a deadlocked
   native program waits forever too).

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
     the process's streams;
   - `idle(deadline)`: an event loop, later; by default a sleep.

   `switched` and `idle` run on `main`'s stack, inside the hub: they must
   not block or yield, and the scheduler panics if they try.
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
   - `sched::finish()`.
   - Flush, then exit with `main`'s result.
   - `IO.Process.exit` from anywhere, a task's context included: `effect()`,
     flush, and C's `exit`, as `lean_io_exit`. The task manager is not
     finalized and no task is waited for (`tasks/exit_from_task`: status 3,
     `main`'s buffered line written, the other task never run).
3. **Tasks.** The task's value lives in the translator's own object. The
   `Job` fills it and returns `Outcome::Done`, or
   `Outcome::Continue(t2, job2)` when a bind function returned an unfinished
   task. The calls:
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
   - `IO.getTID` in a task: `main`'s id plus `thread_number()`.

   The job of a dependent holds its source's handle, as Lean's closures do.
   **When the last reference to an unfinished task goes, call
   `release(id)`**: Lean's `deactivate_task`, which deletes a pure task that
   has not started.

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
    each new worker context (`Sched::start_worker`, before its coroutine
    first runs);
  - the coroutine's function, built in `Sched::start_worker`, stores `y` as
    its first statement, before it calls `worker_main` (the only code that
    runs tasks on the context);
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
  `Glue::switched` and `Glue::idle` on `main`'s stack, inside `hub_hook`.
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

  A panic in a hub hook (`switched`, `idle`) cannot go on: it would unwind
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
- **`switched` and `idle` do not block or yield** (S4 enforces it with a
  panic).

### Checklist for glue authors

1. `suspend` is exactly `unsafe { (*s.yielder()).suspend(()) }`.
2. No other code reads `s.yielder()`, and the `Suspend` value is not kept.
3. No scheduler function is called from a signal handler, another thread,
   or a stack the translator switched to itself.
4. `switched` and `idle` only save and restore state, or wait without the
   scheduler.
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
  hardware: the 32 program cases of `tasks/` and `sync/`, a panic test,
  and leanrs's three adversarial checks (`adv_*`: blocking during
  unwinding, a second panic there, `process::exit` from a context), in
  debug and release builds, on both toolchains (`scripts/check.sh`). Every
  case with contention suspends: the mutex and condition-variable cases,
  the promise waits, the sleeping tasks of `sync_dependent_order`. The
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
  cases added in the fourth review have not run under it yet. Leak
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
| The event loop (`net`) | Not yet: `Glue::idle` is where it plugs in | Batch scope |
| `persist` (the walk of `lean_mark_persistent`) | Not yet | Batch scope |

Lean's panic for `Task.get` inside a `sync := true` task, which lean2rr does
not reproduce, is the glue's: `in_sync_task()` tells it when to report it
(`tasks/get_in_sync_task`).

The limits of one thread (lean2rr plan §10, "Tasks") hold here too:
- a context that computes without blocking or a yield point delays the
  others;
- `IO.waitAny` does not pick the fastest of several running tasks;
- **a blocking system call blocks the whole thread.** A read, a write or a
  wait for a child process that blocks in the kernel stops every context,
  not only the one that made it. Example: `IO.Process.output` of a child
  that writes more than 64 KiB to stdout reads its stderr on `main` while
  a task reads its stdout. `main` blocks in the stderr read, the stdout
  task never runs, the child blocks on its full stdout pipe, and the
  program hangs where native finishes. Until a later batch makes IO
  cooperative (nonblocking attempts, the descriptor registered on `EAGAIN`,
  and `Glue::idle` polling the registered descriptors and timers), a
  translator that admits processes or blocking reads along with tasks has
  this limit (leanrs's review of sched-1, item 1).

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
