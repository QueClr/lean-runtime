# Real threads for `sched` (design)

Status: design only (2026-10-04). Nothing here is implemented. The owner's
direction (2026-10-03): "parallelism will be supported, not the focus now".
Implementation starts after sched-io (cooperative blocking IO).

The owner's decisions so far (2026-10-04):
- lean2rr stays single-threaded for now: "reussir no change yet. lean2rr
  can just not support it if it cant for now";
- the model (native's pool, 1.2) and the value sharing (everything atomic,
  2.1) await the owner's confirmation.

leanrs reviewed this design (2026-10-04). Its answers to the open questions
are folded in (section 6).

This file is the implementors' reference. It covers:
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
1c36a58 (sched-1, io-2, semantics-3, cleanup-1).

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
  `threads`. It has the same functions, with `Send` bounds.
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
  locks, condition variables and atomics, and no new dependency.
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
   queued or running and      dedicated threads: one task each, then exit
   no dedicated thread is
   left, then join workers    state: static Mutex<State> and Condvars
 glue: flush, exit              (queue, finished, quiescent), as m_mutex
```

Each OS thread keeps its running tasks (innermost last) in a thread-local,
as native's `g_current_task_object` (`object.cpp` 730). `check_canceled`,
`in_sync_task` and the glue's `task_begin` read it.

**The lock rule.** No translator code runs under the scheduler's lock:
jobs, the `store` of `resolve`, the drop of a job, glue hooks. Native does
the same (`run_task` unlocks around the closure, `deactivate_task_core`
drops the closure unlocked, `resolve` drops `v` unlocked, 1002). So a
translator destructor that calls `release` or `resolve` cannot deadlock.

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
| `Task.get`, `IO.wait` | A pending task runs inline on the waiter's stack; otherwise the context blocks | The thread blocks (`wait_for`). A pool task frees its worker place meanwhile (the pool grows by one) |
| `sync` dependents, `LEAN_SYNC_PRIO` | Run on the finishing context, newest first | Run on the finishing thread, newest first (`handle_finished`, `enqueue_core`, `run_task`) |
| Dedicated tasks | A priority-9 queue, always started | A thread each (`spawn_dedicated_worker`) |
| `effect`, `poll`, `ref_read` | Let other contexts go first | No-ops |
| `sleep_ms` | Blocks the context | `std::thread::sleep` |
| `IO.getTaskState` | The polling rules (`query`) | Native's answer: queued or waiting is `waiting`; running, or an unresolved promise, is `running` (`get_task_state`, 1085) |
| `IO.waitAny` | A finished task; else the first pending one runs inline; else wait | The first finished task in list order; else block until a task finishes (`wait_any`) |
| `IO.cancel` | A flag; passed on to the dependents when the task finishes | The same (`cancel`, 1074; `handle_finished`) |
| `IO.checkCanceled` | The flag, or shutdown with the `EARLY` emulation | The flag, or the shutdown flag (`lean_io_check_canceled_core`, 1284) |
| Promises | `promise_new`, `resolve`; dependents walked on the resolving context | The same, on the resolving thread. The first resolution wins under the lock (`resolve`, 995) |
| Exit | `finish` runs what is left on `main`; LB-13 is not copied | `finish` sets the shutdown flag. It waits until no task is queued or running and no dedicated thread is left, then joins the workers. An enqueue during shutdown still gets a worker, so LB-13 is not copied |
| `IO.Process.exit` | From any context | From any thread. The other threads run until the process ends, as natively |
| `Std.Sync` | Contexts block. The owner is a context plus a task's thread number | Threads block on condition variables. The owner is the OS thread |
| A thunk forced on two threads | The glue's waiter list (`block_sync`, `wake`). Forced inside itself: `hang` | A blocking once-cell in the glue. Forced inside itself: its thread hangs (LB-08) |
| Stack overflow | The guard of `main`'s stack or of the running context (`running_stack`) | The guard of each OS thread. `mt::Glue::thread_start` installs the thread's alternate signal stack and records its guard |
| Current streams | Swapped per context; a task starts with the process's streams | Per OS thread. `task_begin` still starts each pool task with the process's streams |
| `IO.getTID` | `main`'s id plus `thread_number()` | The thread's `gettid` (`lean_io_get_tid`, `process.cpp` 340) |
| `LEAN_NUM_THREADS=0` | Tasks run at once | The same |
| A Rust panic in a job | Goes on as `main`'s panic | Aborts the process after Rust's message, since no thread can take it over (leanrs agrees, 6) |

Starting each task with fresh streams is one of native's schedules. A
native worker keeps its slots from one task to the next (`io.cpp` 115-117,
`MK_THREAD_LOCAL_GET`); a fresh worker starts from the process's streams
(`src/io/streams.rs`, module comment). Both modes do the same, so their
outputs agree (leanrs agrees, 6).

The single-thread mode runs a waited-for pending task inline on the
waiter's stack. Threads mode does not: natively a waiter blocks and a worker
runs the task; inline, the waiter's OS thread would own the task's locks,
and more pool tasks could run than `LEAN_NUM_THREADS`. leanrs agrees (6).

### 1.5 What changes in the code

The single-thread files do not change. Threads mode is new code in
`src/sched/mt/`. It shares only plain items with `sched`:
- `TaskState`, `priority()`;
- the messages (`GET_IN_SYNC_TASK`, `PROMISE_BEFORE_MANAGER`);
- `env.rs` (`lean_num_threads`, `thread_stack_size`).

| Piece | Today (`src/sched/`) | Threads mode (`src/sched/mt/`) |
|---|---|---|
| State | `thread_local! SCHED: RefCell<Sched>` (`mod.rs`) | One `static Mutex<State>` and its condition variables |
| Task table | A slab per thread; `TaskId` is a 32-bit generation and an index (`task.rs`, `TaskId::new`) | One table. Ids are valid on every thread. A 64-bit serial is never reused within a run, so the 2^32 reuse caveat goes |
| Run queue | Ten queues per thread, with the lone worker emulated (`Tasks::queues`, `worker`, `wake`) | The same ten FIFO queues, shared; real workers take from them |
| Contexts | `Contexts`: coroutines, the hub, `cur`, `CtxId`, the stack pool (`ctx.rs`) | None. Each OS thread has a thread-local stack of its running tasks |
| Waiters | `cell_waiters`, `progress_waiters`, listed by `CtxId` | Condition variables: a task finished (every waiter checks again), the queue, quiescence |
| Jobs | `Box<dyn FnOnce() -> Outcome>` | `Box<dyn FnOnce() -> Outcome + Send>` |
| Glue | `Rc<dyn Glue>`: `suspend`, `switched`, `task_begin`, `task_end` (sched-io removed `idle`: the hub waits in the scheduler's event loop) | `Arc<dyn mt::Glue>`, `Send + Sync`: `thread_start`, `thread_end`, `task_begin`, `task_end` |
| `Std.Sync` | State in a `RefCell`, waiters by `CtxId` (`sync.rs`) | State in a `Mutex`, and a `Condvar` per object |
| Streams | io's thread-local slots, swapped per context (`streams.rs`, `swap_context`) | The same slots, now one set per real thread |
| Stack bounds | `running_stack()`, per context | Not needed; the glue records each thread's guard |
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
// lean_runtime::sched::mt, feature "threads" (std only, no unsafe)
pub type Job = Box<dyn FnOnce() -> Outcome + Send>;
pub enum Outcome { Done, Continue(TaskId, Job) }
pub trait Glue: Send + Sync {
    fn thread_start(&self) {}           // a new worker or dedicated thread:
    fn thread_end(&self) {}             //   signal stack, guard bounds
    fn task_begin(&self, _own_thread: bool) {}  // streams, as in sched
    fn task_end(&self, _own_thread: bool) {}
}
pub fn start(glue: Arc<dyn Glue>);
pub fn start_with(glue: Arc<dyn Glue>, workers: u32, stack_size: usize);
pub fn spawn(job: Job, prio: u64, keep_alive: bool) -> TaskId;
pub fn dependent_runs_now(src: TaskId, sync: bool) -> bool;
pub fn depend(src: TaskId, job: Job, prio: u64, sync: bool, keep_alive: bool) -> TaskId;
pub fn wait(id: TaskId);
pub fn state(id: TaskId) -> TaskState;
pub fn wait_any(ids: &[TaskId]) -> usize;
pub fn cancel(id: TaskId);
pub fn check_canceled() -> bool;
pub fn release(id: TaskId);                 // from any thread
pub fn in_sync_task() -> bool;
pub fn promise_new() -> Result<TaskId, &'static str>;
pub fn resolve(id: TaskId, store: impl FnOnce()) -> bool;  // store runs here
pub fn finish();
pub fn effect() {}  pub fn poll() {}  pub fn ref_read() {}
pub fn sleep_ms(ms: u32);
pub mod sync { /* Mutex, Condvar, RecursiveMutex, SharedMutex: Send + Sync */ }
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
  reports native's `failed to create thread` message and aborts, as
  `thread_create_failed` does today.

### 2.5 A translator that cannot provide these yet

- `sched` stays as it is: no `Send` bounds, `Rc<dyn Glue>`, coroutines.
  It stays the default.
- The feature `threads` selects at compile time. Without it the build is
  byte-identical to today's: no `sched::mt`, no `Arc`, no `Send` or `Sync`
  bound, and the IO layer's paths as sched-io leaves them.
- With it, the IO layer takes the blocking path by `cfg`, never by a
  per-call branch (3.2). So `threads` and the coroutine `sched` exclude each
  other (a `compile_error!` when both are on): a build has one scheduler.
  Each translator builds a program for one mode.
- A translator chooses threads mode per program, and only if it emits
  atomic values, behind a feature of its own. The glue's call sites keep
  their names (`spawn`, `wait`, `effect`, ...), so switching modes changes
  only the module path.

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
- a reference implementation in the drivers (`tests/sched-driver`).

**The rule holds in single-thread mode too.** `modify`'s function can block
(a `Task.get` in it), and another context then runs. A reader that found
the reference empty would see the placeholder at once. So `get`, `take`,
`set` and `swap` of an empty reference are blocking yield points until
`modify`'s store: a glue duty (`docs/sched.md`, The glue, item 7). The driver's `Ref`
(`tests/sched-driver/src/lean.rs`) follows the rule. Each translator checks
its own refs (6).

### 3.2 Table

| Item | Where | Today | Under threads |
|---|---|---|---|
| Scheduler state | `sched/mod.rs` `SCHED` | Thread-local `RefCell` | `sched::mt`: a global `Mutex` (1.5) |
| Ref-read polling | `sched/mod.rs` `REF_YIELDS`, `REF_READS_LEFT` | An atomic flag and a thread-local count | Not used: `ref_read` is a no-op |
| Thread numbers | `sched/ctx.rs` `NEXT_THREAD` | A process-wide atomic | Unchanged |
| Running stack bounds, hub flag | `sched/ctx.rs` `RUN_LO`/`HI`/`TOP`, `IN_HUB_HOOK` | Thread-locals | Not used. The glue records each thread's guard in `thread_start` |
| `Std.Sync` objects | `sched/sync.rs` | `RefCell` state, `CtxId` waiters | `Mutex` state and a `Condvar` per object |
| `IO.Ref` | The translator's | `modify`'s function can block with the reference empty (3.1). `get`, `take`, `set` and `swap` of an empty reference must block until `modify`'s store: a glue duty (`docs/sched.md`, The glue, item 7; LB-01, LB-18) | A lock and a condition variable per reference, with the rule of 3.1 |
| Standard streams' `FILE`s | `io/handle.rs` `STDIN`, `STDOUT`, `STDERR` | `static Mutex<CFile>`: glibc locks each `FILE` | Unchanged |
| Open files | `io/handle.rs` `FileStream::file` | `Mutex<CFile>`; `Handle` is an `Arc` | Unchanged; already `Send + Sync` |
| `Handle.lock` | `io/handle.rs` `Handle::flock` | Waits in `flock` without the stream's lock (review RIO1-01) | Unchanged |
| A sink under a stream's lock | `io/mod.rs` `ByteSink` | The sink must not call `io` or exit | Unchanged; the rule is per thread |
| Open-handle list | `io/handle.rs` `OPEN`, `release` | `Mutex<Vec<Arc<FileStream>>>`; every release under it | Unchanged. Opens racing the exit's walk behave as with glibc's list lock |
| Current streams | `io/streams.rs` `CURRENT` | Thread-local, swapped per context (`swap_context`) | One set per real thread, as natively. `task_begin` gives each pool task fresh slots |
| Route of the runtime's stderr lines | `io/streams.rs` `StderrPut` | An `Rc` in the thread-local | Unchanged: it never leaves its thread |
| errno model | `io/error.rs` `ERRNO` | Thread-local, shared by all contexts | Per thread, as C's `errno` |
| Working directory | `io/process.rs` `CWD_LOCK` (`RwLock`) | Held for writing by the fallback spawn (`fallback_spawn`) and by `setCurrentDir` and `uv_chdir` (`with_cwd_change`); held for reading by `getcwd`, `uv_cwd` (`with_cwd_read`) and spawns without a `cwd`. Relative path operations take nothing. Gap documented: "another thread's relative path operation during the spawn ... still sees `cwd`" (module comment, item 4) | The gap becomes reachable (below) |
| Spawner thread | `io/process.rs` `SPAWNER`, `NO_PRIVATE_CWD` | `Mutex<Option<Sender>>`; one long-lived thread | Unchanged. Spawns with a `cwd` queue on it, where native's forks run in parallel: a speed difference only |
| Modelled pids | `io/process.rs` `NEXT_MODELLED_PID` | `AtomicU32` | Unchanged |
| `environ` copy | `io/environ.rs` `ENVIRON`, `set`, `unset` | A `Mutex`; the C environment changes through `std::env::set_var` and `remove_var` | Unchanged. C code that reads the environment on another thread races with `setenv`, as natively (`lean_uv_os_setenv`, `uv/system.cpp` 320, calls libuv's `uv_os_setenv`). The crate is on edition 2021, where `set_var` is safe |
| Process title | `io/uvsys.rs` `TITLE` | `Mutex` | Unchanged |
| Startup descriptors | `io/startup.rs` `DESCRIPTORS` | `OnceLock` | Unchanged |
| `forceExit` flag | `io/exit.rs` `EXITING_WITHOUT_FLUSH` | `AtomicBool`, `SeqCst` | Unchanged |
| `semantics` | `src/semantics/` | No global state | Unchanged |

**The working directory under threads.** Today the gap needs a second
thread running Lean code. Threads mode decides the spawn path at
`mt::start`, by starting the spawner thread there, before any task runs. If
`unshare(CLONE_FS)` is refused, every relative-path operation takes
`CWD_LOCK` for reading for the rest of the run (one uncontended read lock
per call); if it works, the common case, nothing changes. The real fix stays
the one `process.rs` names, `posix_spawn_file_actions_addchdir_np`, which
nix does not wrap and the crate cannot call without `unsafe`.

**Signals under threads.** `sched::uv`'s signal delivery keeps each
signal's `arrived` flag process-wide, while the watcher lists are per
thread (one per scheduler): with schedulers on two threads, a signal would
reach only the thread whose loop takes the flag first (review RSIOB-15).
Threads mode routes signals instead: one delivery for the process, which
hands each signal to the watchers of every thread, as libuv's one loop
does.

**IO under threads.** sched-io's cooperative path (a wait for the
descriptor in the scheduler's event loop, `reactor::poll_fds`, then the same
system call; the stream locks parked at every switch; the no-suspend scope)
is for the single-thread scheduler only. In threads mode every call takes the plain
blocking path, which blocks its own thread, as natively. The choice is a
compile-time `cfg` on the feature `threads`, not a branch per call, so a
build without the feature is unchanged (2.5).

## 4. Determinism and testing

**Single-thread mode.** It stays deterministic for a program whose events
are farther apart than the emulation thresholds (`docs/sched.md`, "Schedules
that depend on the machine's speed"). Its tests do not change.

**Threads mode is not deterministic, and neither is native.** What it
promises:
- every outcome is one that native Lean 4.34.0 could produce, except the
  documented deviations (LB-01 and LB-13 fixed; fresh streams per task,
  which is one of native's schedules);
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
- **Unit tests of `sched::mt`** (small sizes): the task table, deletion,
  the walk of dependents, cancellation, `waitAny`, promises, exit with late
  tasks, the pool growing by one while a pool task waits. They run under
  Miri too, which runs std threads, locks and condition variables and
  reports data races (it cannot run corosensei's stack switch).
- **A second driver**, `tests/sched-driver-mt`, a package of its own:
  `threads` and the coroutine `sched` exclude each other in one build (2.5),
  so cargo builds it in a separate invocation. Its glue is over
  `sched::mt`, its values use `Arc`, a `OnceLock` slot and the `IO.Ref` of
  3.1, and it shares the case ports (`cases.rs`). It runs the cases of
  `tasks/`, `sync/` and `refs/` 5 times each, and accepts the recorded
  outcome or a recorded alternative; a case with a `native` field (LB-13's)
  must give the corrected outcome every time.
- **`scripts/check.sh`** runs both drivers, in debug and release builds.
  ThreadSanitizer is run by hand (nightly), as AddressSanitizer is today.
- **New cases, recorded natively,** that need contention: LB-01 (a task
  sets a ref while `main` reads it); more tasks than `LEAN_NUM_THREADS`
  waiting on each other; `IO.waitAny` returning the faster of two tasks; a
  stack overflow in a pool and in a dedicated task; an exit while tasks
  still enqueue (LB-13); a mutex handed between real threads.
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
| T1 | `sched::mt`: the task manager (spawn, depend, wait, waitAny, state, cancel, release, promises, exit without LB-13), `mt::sync`, `mt::Glue`, worker and dedicated threads with Lean's stack size | T0: the IO switch, and the order the owner set | About 1,200 lines and 500 of unit tests | Yes, with Miri |
| T2 | io in threads mode: the blocking path instead of sched-io's cooperative one, by `cfg` (3.2); the `CWD_LOCK` rule of 3.2; per-task streams | T1 | About 150 lines | Yes |
| T3 | The second driver (`tests/sched-driver-mt`), `check.sh`, the new cases recorded natively, the docs (`sched.md`, the site) | T1, T2 | About 700 lines, and the cases | Yes |
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
- **Streams:** fresh per task in both modes (leanrs; 1.4).
- **A Rust panic in a job on a worker** aborts the process (leanrs; 1.4).
- **Lean 4.35's refs:** read from tag `v4.35.0-rc1` (3.1).
- **Refs, judged (2026-10-04):** a `set` lost during `modify` extends LB-01,
  and a `swap` returning its own argument is LB-18. Both modes follow Lean
  4.35 (3.1). leanrs approved the design at 5c6b365.

### Open questions for the owner

1. **The model.** Native's pool, with no coroutines in threads mode
   (recommended, 1.2)? Awaiting the owner's confirmation.
2. **Atomic values.** Everything atomic in threads-mode programs, and
   type-directed coloring only after measurement (2.1)? Awaiting the
   owner's confirmation.
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
