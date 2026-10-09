//! Lean's task manager on real threads: a port of `task_manager`
//! (`src/runtime/object.cpp` 758-1098, Lean 4.34.0), with the translator's
//! task value in its own slot (the job fills it) and Lean's reference
//! counts replaced by the glue's `release`.
//!
//! **The lock.** One `Mutex<State>` (`Shared::st`) guards everything, as
//! native's `m_mutex`: the task table, the queues, the lists of
//! dependents, the worker counts. Three condition variables go with it:
//! - `queue_cv` (`m_queue_cv`): a task was queued, the pool's limit rose, or
//!   the shutdown began. Idle workers wait on it;
//! - `finished_cv` (`m_task_finished_cv`): a task finished and its
//!   dependents were walked (`resolve_core`'s `notify_all`, after
//!   `handle_finished`), LB-32's wake, a contended promise resolved. `wait`,
//!   `wait_any` and a resolver that lost the race wait on it;
//! - `quiet_cv` (`m_dedicated_finished_cv`): a worker or a dedicated thread
//!   ended. `finish` waits on it.
//!
//! Every wait checks its condition again in a loop under the lock, so a
//! spurious wake-up (which native's `std::condition_variable` also has)
//! changes nothing but when the check runs.
//!
//! **A hold ends with its hand-offs** (hunt HMT3-01). The lock's guard
//! (`Guard`) runs `hand_off` when it is dropped and before a wait on a
//! condition variable lets the lock go: a `notify_one` of the hold that
//! found no worker on `queue_cv` (`wake_one`: every idle worker is in its
//! `task_end` hook) gives a worker in its hook the queue's next task only
//! then, as natively a woken worker takes a task only once the notifying
//! hold has ended. So `State::wakes` is 0 whenever the lock is free.
//!
//! **No translator code runs under the lock** (docs/threads.md, 1.3): not a
//! job, not a resolver's `store`, not a glue hook, and no translator value
//! is dropped there (a deleted task's job is taken out and dropped after the
//! unlock; the glue `Arc` is replaced and dropped outside). Native does the
//! same (`run_task` unlocks around the closure, 898-904;
//! `deactivate_task_core` drops the closure unlocked, 811-829; `resolve`
//! drops `v` unlocked, 1002). So a translator destructor that calls
//! `release` or `resolve`, on any thread, cannot deadlock. The one other
//! lock is a `Ref`'s or a `Std.Sync` object's own, never held together
//! with this one. On the threads the manager makes, each of those runs
//! under `AbortOnUnwind` (`guarded`): a panic in a job, a hook or such a
//! destructor aborts the process, so a worker never ends while `live`
//! still counts it (review RT1-01).
//!
//! **Atomics.** The scheduler's state is plain data under the lock. The
//! atomics are: `Shared::shutting_down` and `Shared::started` (copies of
//! `State`'s, written under the lock, read without it by
//! `check_canceled`, `manager_running` and `Std.Sync`'s owners), and a
//! running task's cancellation flag (`Frame::canceled`, written by `cancel`
//! under the lock, read by `check_canceled` on the task's thread without
//! it), all `Relaxed`, as native's `m_canceled` loads are (a cancellation
//! is seen soon, not at once; the next lock or unlock of the task's thread
//! orders it).
//!
//! **Ids.** A task's id is a 64-bit serial, never reused within a run, so
//! an id the glue keeps after the task finished names a finished task
//! (`TaskId`): no generation, no reuse caveat.

use super::{Glue, Job, Outcome, TaskId};
use crate::sched::common::{priority, PRIOS};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::hash::{BuildHasherDefault, Hasher};
use std::ops::{Deref, DerefMut};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError};
use std::thread::JoinHandle;

/// The priority of dedicated tasks (above `Task.Priority.max`): a thread of
/// their own (`spawn_dedicated_worker`, 873-883). 0..=8 are the pool's.
const DEDICATED: usize = PRIOS - 1;

// Entry flags.
/// `Task.spawn`/`map`/`bind` (`keep_alive = false`): deleted when released
/// before it starts.
const PURE: u16 = 1 << 0;
/// In its priority's queue.
const QUEUED: u16 = 1 << 1;
/// In its source's list of dependents (`add_dep`).
const WAITING: u16 = 1 << 2;
/// Its job runs (`run_task` took its closure).
const RUNNING: u16 = 1 << 3;
/// An unresolved promise: no job (`lean_promise_new`).
const PROMISE: u16 = 1 << 4;
/// A resolver's `store` runs (`resolve`): the first resolution wins.
const RESOLVING: u16 = 1 << 5;
/// A pure task released while it ran (or while tasks still held it):
/// native `m_deleted`; its finish notifies nobody.
const DELETED: u16 = 1 << 6;
/// An IO task released before it finished: natively its keep-alive
/// reference is then the last one, so its finish deletes it (`run_task`,
/// 905-912) and notifies nobody.
const UNREFERENCED: u16 = 1 << 7;

/// A task or promise of the table (native `lean_task_imp`).
struct Entry {
    flags: u16,
    /// 0..=8 for the pool, `DEDICATED` (`common::priority`).
    prio: u8,
    /// Runs on the thread that enqueues it (`sync := true`: native's
    /// internal priority `LEAN_SYNC_PRIO`, which `enqueue_core` runs there,
    /// 793-796; no Lean priority gives it here, LB-39).
    sync: bool,
    /// `m_canceled`.
    canceled: bool,
    /// While it runs: the flag its thread's `check_canceled` reads without
    /// the lock; `cancel` sets it too.
    cancel_flag: Option<Arc<AtomicBool>>,
    /// The computation (`m_closure`): `None` while it runs, for a promise,
    /// and once released.
    job: Option<Job>,
    /// Its dependents, oldest first: the walk takes the newest first, as
    /// `handle_finished` follows `m_head_dep` (938-952). The ids of
    /// dependents released meanwhile stay here and are skipped.
    deps: Vec<u64>,
}

/// A hasher for the table's keys, serials: one multiplication (the golden
/// ratio's odd constant), a bijection that spreads consecutive serials.
#[derive(Default)]
pub(crate) struct IdHasher(u64);

impl Hasher for IdHasher {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 = (self.0.rotate_left(8) ^ u64::from(b)).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        }
    }
    fn write_u64(&mut self, n: u64) {
        self.0 = n.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    }
}

/// The task manager's state, under `Shared::st` (native's `task_manager`
/// fields).
pub(crate) struct State {
    /// The task manager runs: `start` with workers, until `finish` is over
    /// (`g_task_manager` non-null). Before and after, tasks run at once.
    started: bool,
    /// `m_shutting_down`: `finish` has begun.
    shutting_down: bool,
    /// A task manager has run (`start` with workers): from then on, tasks
    /// made without one go through the table (`without_manager`, review
    /// RF15-C06).
    ever_started: bool,
    glue: Option<Arc<dyn Glue>>,
    /// The stack size of the threads made from now on (`lthread`'s).
    stack_size: usize,
    /// `LEAN_NUM_THREADS` (`start_with`'s `workers`).
    limit: u32,
    /// Pool tasks blocked in `wait`: each raises the limit by one
    /// (`wait_for`, 1036-1043). `limit + raised` is `m_max_std_workers`.
    raised: u32,
    /// Standard workers made and not ended: native `m_std_workers.size()`,
    /// which only shrinks during the shutdown here (a worker ends when the
    /// queue is empty and the shutdown has begun).
    live: u32,
    /// `m_idle_std_workers`: live workers between tasks.
    idle: u32,
    /// The standard workers (their indices) in their task's `task_end` hook,
    /// counted idle, that took no task: such a worker cannot wait on
    /// `queue_cv` meanwhile, so a task it would take when woken is handed to
    /// it at the end of the hold that woke it (`wake_one`, `hand_off`; hunts
    /// HMT2-01, HMT3-01).
    hook_idle: Vec<u32>,
    /// The tasks handed to such workers (`take`n: counted busy, started):
    /// each runs its task once its hook has returned.
    handed: Vec<(u32, u64)>,
    /// The `notify_one`s of the current hold that found no worker waiting on
    /// `queue_cv` (`wake_one`): `hand_off` serves them when the hold ends;
    /// 0 whenever the lock is free.
    wakes: u32,
    /// Standard workers made so far: the next one's index (its position in
    /// native's `m_std_workers`; `running_worker`, review AR-32).
    made: u32,
    /// `m_num_dedicated_workers`: dedicated threads made and not ended.
    dedicated: u32,
    /// One FIFO queue per pool priority (`m_queues`); the ids of tasks
    /// released while queued stay and are skipped.
    queues: [VecDeque<u64>; DEDICATED],
    /// The queued tasks not released (`m_queues_size` without them).
    queued: u32,
    tasks: HashMap<u64, Entry, BuildHasherDefault<IdHasher>>,
    /// The last id handed out.
    serial: u64,
    /// The standard workers' handles, joined by `finish`.
    worker_handles: Vec<JoinHandle<()>>,
    /// The dedicated threads' handles, joined by `finish`; those of ended
    /// threads are let go once many pile up (`spawn_dedicated`). Kept apart
    /// from the workers' so that no worker's handle is let go before
    /// `finish` joins it (review RT1-05).
    dedicated_handles: Vec<JoinHandle<()>>,
    /// Resolvers waiting for another one's `store` (`resolve`).
    resolvers_waiting: u32,
}

impl State {
    /// `m_max_std_workers`.
    fn max(&self) -> u32 {
        self.limit.saturating_add(self.raised)
    }
}

impl Drop for State {
    fn drop(&mut self) {
        // The jobs of tasks that never ran (a dependent of a promise never
        // resolved) are not dropped: they hold the translator's values, whose
        // destructors would call back into this manager while it is dropped
        // (natively nothing is freed at exit either).
        for e in self.tasks.values_mut() {
            if let Some(job) = e.job.take() {
                std::mem::forget(job);
            }
        }
    }
}

/// One task manager: its state, its lock and its condition variables (see
/// the module comment). Production has one, the process's (`GLOBAL`); the
/// unit tests make one each (`bind_local`).
pub(crate) struct Shared {
    st: Mutex<State>,
    queue_cv: Condvar,
    finished_cv: Condvar,
    quiet_cv: Condvar,
    /// `State::shutting_down`, for `check_canceled`.
    shutting_down: AtomicBool,
    /// `State::started`, for `manager_running` and `Std.Sync`'s owners.
    started: AtomicBool,
    /// The threads blocked in `wait` (tests: a test learns that its waiter
    /// sleeps on `finished_cv` before it lets the task finish).
    #[cfg(test)]
    blocked_waits: std::sync::atomic::AtomicU32,
}

/// The lock, held (`Shared::lock`). Every hold ends with `hand_off`: when
/// the guard is dropped, and in `wait_on` before the wait lets the lock go.
/// `None` only inside `into_inner`.
pub(crate) struct Guard<'a>(Option<MutexGuard<'a, State>>);

impl<'a> Guard<'a> {
    /// The end of the hold, for a wait that lets the lock go: `hand_off`,
    /// then the bare guard.
    fn into_inner(mut self) -> MutexGuard<'a, State> {
        let mut g = self
            .0
            .take()
            .expect("lean-runtime: a guard without its lock");
        hand_off(&mut g);
        g
    }
}

impl Deref for Guard<'_> {
    type Target = State;
    fn deref(&self) -> &State {
        self.0
            .as_deref()
            .expect("lean-runtime: a guard without its lock")
    }
}

impl DerefMut for Guard<'_> {
    fn deref_mut(&mut self) -> &mut State {
        self.0
            .as_deref_mut()
            .expect("lean-runtime: a guard without its lock")
    }
}

impl Drop for Guard<'_> {
    fn drop(&mut self) {
        if let Some(g) = &mut self.0 {
            hand_off(g);
        }
    }
}

impl Shared {
    fn new() -> Shared {
        Shared {
            st: Mutex::new(State {
                started: false,
                ever_started: false,
                shutting_down: false,
                glue: None,
                stack_size: crate::sched::thread_stack_size(),
                limit: 0,
                raised: 0,
                live: 0,
                idle: 0,
                hook_idle: Vec::new(),
                handed: Vec::new(),
                wakes: 0,
                made: 0,
                dedicated: 0,
                queues: Default::default(),
                queued: 0,
                tasks: HashMap::default(),
                serial: 0,
                worker_handles: Vec::new(),
                dedicated_handles: Vec::new(),
                resolvers_waiting: 0,
            }),
            queue_cv: Condvar::new(),
            finished_cv: Condvar::new(),
            quiet_cv: Condvar::new(),
            shutting_down: AtomicBool::new(false),
            started: AtomicBool::new(false),
            #[cfg(test)]
            blocked_waits: std::sync::atomic::AtomicU32::new(0),
        }
    }

    /// The lock. A panic never unwinds while it is held (jobs and hooks
    /// abort the process, `AbortOnUnwind`), so poisoning is ignored.
    fn lock(&self) -> Guard<'_> {
        Guard(Some(self.st.lock().unwrap_or_else(PoisonError::into_inner)))
    }

    pub(crate) fn started(&self) -> bool {
        self.started.load(Ordering::Relaxed)
    }

    /// `finish` has returned: the shutdown began and the manager stopped
    /// (`started` is cleared last, once every thread has been joined).
    pub(crate) fn finished(&self) -> bool {
        self.shutting_down.load(Ordering::Relaxed) && !self.started.load(Ordering::Relaxed)
    }
}

/// Wait on `cv`: the hold ends (`hand_off`), and a new one begins when the
/// wait returns.
fn wait_on<'a>(cv: &Condvar, g: Guard<'a>) -> Guard<'a> {
    Guard(Some(
        cv.wait(g.into_inner())
            .unwrap_or_else(PoisonError::into_inner),
    ))
}

// ---------------------------------------------------------------------------
// The calling thread

/// The process's task manager (`g_task_manager`): made by `start`.
static GLOBAL: OnceLock<Arc<Shared>> = OnceLock::new();

/// Thread numbers (`thread_number`) for threads other than `start`'s.
static NEXT_NUMBER: AtomicU64 = AtomicU64::new(1);

/// A task running on this thread (native `g_current_task_object`).
struct Frame {
    /// The task (`end_running_task`).
    id: u64,
    /// A pool task (priority 0..=8, not `sync`): its `wait` raises the
    /// pool's limit (`wait_for`'s `in_pool`, 1031).
    pool: bool,
    sync: bool,
    canceled: Arc<AtomicBool>,
    shared: Arc<Shared>,
    /// The job ended its task itself (`end_running_task`), a referenced
    /// one: its waiters wake once `run_one` has run its `task_end`, when the
    /// worker counts itself idle (hunt HMT2-03).
    notify_at_end: Cell<bool>,
}

/// A standard worker's state across the run of its task, which `drive`
/// passes down to the task's `task_end` (hunt HMT2-01).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Turn {
    /// Not a standard worker's own task: a `sync` task, a dedicated task, a
    /// resolver's walk, a task run at once after `finish`; or a worker's
    /// task that runs again at once (`add_dep`'s `again`).
    Other,
    /// A standard worker runs its task: the task's `task_end` counts it
    /// idle and takes its next task.
    Worker,
    /// Counted idle; no task was queued (or the limit held it back).
    Idle,
    /// Took this task, which it runs next.
    Took(u64),
}

thread_local! {
    /// The task manager this thread belongs to: `start`'s thread, and every
    /// thread the manager makes. Other threads use the process's (`GLOBAL`).
    static MGR: RefCell<Option<Arc<Shared>>> = const { RefCell::new(None) };
    /// The tasks running on this thread, innermost last: a `sync` task runs
    /// inside the task or walk that enqueued it (native's
    /// `scoped_current_task_object` saves and restores the current task).
    static CURRENT: RefCell<Vec<Frame>> = const { RefCell::new(Vec::new()) };
    /// This thread's number (`thread_number`).
    static NUMBER: Cell<Option<u64>> = const { Cell::new(None) };
    /// A standard worker's index (`running_worker`); `None` on every other
    /// thread.
    static WORKER: Cell<Option<u32>> = const { Cell::new(None) };
}

/// The index of the standard worker this thread is (review AR-32): `None`
/// on a dedicated task's thread, `main`'s, the loop thread and any other.
pub(crate) fn running_worker() -> Option<u32> {
    WORKER.try_with(Cell::get).ok().flatten()
}

/// `f` on the calling thread's task manager, `None` if there is none (no
/// `start` yet: tasks run at once). The thread's own is borrowed, not
/// cloned, for the whole call (nested calls borrow it again); while the
/// thread's locals are being destroyed, the process's is used.
pub(crate) fn with_shared<R>(f: impl FnOnce(Option<&Arc<Shared>>) -> R) -> R {
    let mut f = Some(f);
    let local = MGR.try_with(|m| {
        let m = m.borrow();
        m.as_ref()
            .map(|sh| (f.take().expect("called once"))(Some(sh)))
    });
    if let Ok(Some(r)) = local {
        return r;
    }
    (f.take().expect("called once"))(GLOBAL.get())
}

/// Make `sh` the calling thread's task manager.
fn bind(sh: Arc<Shared>) {
    let _ = MGR.try_with(|m| {
        if let Ok(mut m) = m.try_borrow_mut() {
            *m = Some(sh);
        }
    });
}

fn innermost<R>(f: impl FnOnce(&Frame) -> R) -> Option<R> {
    CURRENT
        .try_with(|r| r.borrow().last().map(f))
        .ok()
        .flatten()
}

/// `IO.checkCanceled` (`lean_io_check_canceled_core`): the innermost task
/// running on this thread was canceled, or the shutdown has begun; false
/// outside tasks. No lock.
pub(crate) fn check_canceled() -> bool {
    innermost(|f| {
        f.canceled.load(Ordering::Relaxed) || f.shared.shutting_down.load(Ordering::Relaxed)
    })
    .unwrap_or(false)
}

/// Whether the innermost task running on this thread is a `sync` one.
pub(crate) fn in_sync_task() -> bool {
    innermost(|f| f.sync).unwrap_or(false)
}

/// This thread's number: 0 on the thread that called `start`, otherwise a
/// number of its own, from a process-wide counter.
pub(crate) fn thread_number() -> u64 {
    NUMBER
        .try_with(|n| match n.get() {
            Some(k) => k,
            None => {
                let k = NEXT_NUMBER.fetch_add(1, Ordering::Relaxed);
                n.set(Some(k));
                k
            }
        })
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Threads

/// A Rust panic unwinding a job or a glue hook that the crate runs aborts
/// the process, after Rust's message (docs/threads.md, 1.4): on a worker no
/// thread could take it over, and the state would keep a task running for
/// good. The guard is forgotten on the normal path.
struct AbortOnUnwind(&'static str);

impl Drop for AbortOnUnwind {
    fn drop(&mut self) {
        use std::io::Write;
        let _ = writeln!(
            std::io::stderr(),
            "lean-runtime: a Rust panic in {} (threads mode); aborting",
            self.0
        );
        std::process::abort();
    }
}

/// A job and its continuations, run at once where no task manager ever ran
/// (`mt::run_at_once`): a Rust panic in them aborts, as in a job the crate
/// runs as a task (docs/threads.md, 1.4; review RF15-C06).
pub(crate) fn run_job_at_once(job: Job) {
    let guard = AbortOnUnwind("a task's job");
    let mut out = job();
    while let Outcome::Continue(_, k) = out {
        out = k();
    }
    std::mem::forget(guard);
}

/// Run a glue hook outside the lock (a panic in it aborts).
fn hook(f: impl FnOnce()) {
    guarded("a glue hook", f);
}

/// Run `f` outside the lock: a glue hook, or the drop of a translator's
/// value or of the glue (whose destructors may panic). A panic in it aborts
/// (review RT1-01: a destructor's panic on a worker must not end that
/// worker while `live` still counts it).
pub(crate) fn guarded(what: &'static str, f: impl FnOnce()) {
    let guard = AbortOnUnwind(what);
    f();
    std::mem::forget(guard);
}

/// A thread with Lean's stack size (`lthread`: `pthread_attr_setstacksize`).
pub(crate) fn spawn_thread(stack_size: usize, f: impl FnOnce() + Send + 'static) -> JoinHandle<()> {
    match std::thread::Builder::new().stack_size(stack_size).spawn(f) {
        Ok(h) => h,
        Err(e) => crate::sched::thread_create_failed(&e),
    }
}

/// The entry of every thread the manager makes, before it runs a task: it
/// belongs to `sh`, gets its number, installs Lean's stack-overflow report
/// (feature `stack-overflow`: the handler, once per process, and this
/// thread's alternate signal stack and record; leanrs's point 4), and calls
/// the glue's `thread_start`.
fn thread_entry(sh: &Arc<Shared>) {
    bind(sh.clone());
    let _ = thread_number();
    #[cfg(feature = "stack-overflow")]
    crate::sched::install_stack_overflow_handler();
    let glue = sh.lock().glue.clone();
    if let Some(gl) = glue {
        // the clone is dropped inside the guard too: it may be the glue's
        // last reference, if `start_with` replaced it meanwhile
        hook(move || {
            gl.thread_start();
            drop(gl);
        });
    }
}

/// What a thread of the crate's own that runs translator code (`sched::uv`'s
/// loop thread) takes from the calling thread's task manager: its glue (for
/// `thread_start`) and the stack size of the threads it makes (`lthread`'s);
/// without a task manager, no glue and Lean's stack size.
pub(crate) fn glue_and_stack_size() -> (Option<Arc<dyn Glue>>, usize) {
    with_shared(|sh| match sh {
        Some(sh) => {
            let g = sh.lock();
            (g.glue.clone(), g.stack_size)
        }
        None => (None, crate::sched::thread_stack_size()),
    })
}

/// The end of such a thread: the glue's `thread_end`.
fn thread_exit(glue: Option<Arc<dyn Glue>>) {
    if let Some(gl) = glue {
        hook(move || {
            gl.thread_end();
            drop(gl);
        });
    }
}

/// `spawn_worker` (831-871): a new standard worker. Unlike native, it also
/// makes one during the shutdown (native returns at once there, 832-833),
/// so a pool task enqueued once no worker is left still runs (LB-13).
fn spawn_worker(sh: &Arc<Shared>, g: &mut State) {
    // After `finish` no thread is made (review RT1-02): nothing enqueues
    // then, and no task then is a pool task.
    debug_assert!(g.started, "lean-runtime: a worker made after finish");
    g.live += 1;
    let index = g.made;
    g.made += 1;
    let sh2 = sh.clone();
    let h = spawn_thread(g.stack_size, move || worker_main(sh2, index));
    g.worker_handles.push(h);
}

/// A standard worker's loop (`spawn_worker`'s lambda): take the first task
/// of the highest non-empty queue and run it, unless the busy workers
/// already reach the limit (outside the shutdown); wait while the queue is
/// empty; end once it is empty and the shutdown has begun.
fn worker_main(sh: Arc<Shared>, index: u32) {
    let _ = WORKER.try_with(|w| w.set(Some(index)));
    thread_entry(&sh);
    let mut g = sh.lock();
    g.idle += 1;
    // The task the last one's `task_end` took (counted busy already).
    let mut next = None;
    loop {
        let id = match next.take() {
            Some(id) => id,
            None => {
                if g.queued == 0 {
                    if g.shutting_down {
                        break;
                    }
                    g = wait_on(&sh.queue_cv, g);
                    continue;
                }
                let Some(id) = take(&mut g) else {
                    g = wait_on(&sh.queue_cv, g);
                    continue;
                };
                id
            }
        };
        // The worker counts itself idle again once its task's walk is done,
        // and takes its next task, before the glue's `task_end` unlocks
        // (`task_end`); or here if the run reached no `task_end` (a task
        // gone from the table: none since `take` marks it running, kept
        // for safety).
        let mut turn = Turn::Worker;
        g = drive(&sh, g, Some((id, true)), Vec::new(), &mut turn);
        match turn {
            Turn::Worker => g.idle += 1,
            Turn::Took(id) => next = Some(id),
            Turn::Idle | Turn::Other => {}
        }
    }
    g.idle -= 1;
    g.live -= 1;
    // `finish` waits for the last worker to end.
    sh.quiet_cv.notify_all();
    let glue = g.glue.clone();
    drop(g);
    thread_exit(glue);
}

/// `spawn_dedicated_worker` (873-883): a thread of its own for task `id`.
/// Natively the thread is detached; here its handle is kept for `finish` to
/// join, and the handles of ended dedicated threads are let go once many
/// pile up (detaching an ended thread frees it).
fn spawn_dedicated(sh: &Arc<Shared>, g: &mut State, id: u64) {
    debug_assert!(
        g.started,
        "lean-runtime: a dedicated thread made after finish"
    );
    g.dedicated += 1;
    if g.dedicated_handles.len() >= 64 {
        g.dedicated_handles.retain(|h| !h.is_finished());
    }
    let sh2 = sh.clone();
    let h = spawn_thread(g.stack_size, move || {
        thread_entry(&sh2);
        let g = sh2.lock();
        let mut g = run_task(&sh2, g, id, true);
        g.dedicated -= 1;
        sh2.quiet_cv.notify_all();
        let glue = g.glue.clone();
        drop(g);
        thread_exit(glue);
    });
    g.dedicated_handles.push(h);
}

/// `dequeue` (772-787): the first task of the highest non-empty queue.
/// Released tasks' ids are dropped on the way (natively freed when dequeued,
/// `run_task` 887-890).
fn dequeue(g: &mut State) -> Option<u64> {
    for p in (0..DEDICATED).rev() {
        while let Some(id) = g.queues[p].pop_front() {
            if let Some(e) = g.tasks.get_mut(&id) {
                if e.flags & QUEUED != 0 {
                    e.flags &= !QUEUED;
                    g.queued -= 1;
                    return Some(id);
                }
            }
        }
    }
    None
}

/// The worker loop's take step (`spawn_worker`'s lambda, 846-866), for a
/// standard worker counted idle, with a task queued: the first task of the
/// highest non-empty queue, unless the busy workers reach the limit
/// (outside the shutdown). The worker then counts as busy, and the task as
/// started (`RUNNING`), as natively `run_task` takes its closure in the
/// hold of the dequeue (887-898): so a `release` before its job begins
/// (during the glue's `task_end`, which comes between, `task_end`) marks a
/// pure task deleted and canceled, and `state` answers running.
fn take(g: &mut State) -> Option<u64> {
    debug_assert!(g.queued > 0);
    // `live >= idle`: a worker counts itself idle only while it lives.
    if !g.shutting_down && g.live - g.idle >= g.max() {
        return None;
    }
    let id = dequeue(g)?;
    g.idle -= 1;
    if let Some(e) = g.tasks.get_mut(&id) {
        e.flags |= RUNNING;
    }
    Some(id)
}

/// `enqueue_core` (789-809) for task `id`, pending and not `sync` (a `sync`
/// one runs on the enqueuing thread: the callers run it themselves, with
/// `run_task`): a dedicated task gets its thread; a pool task goes to the
/// end of its priority's queue, and a new worker is made if none is idle
/// and fewer than the limit live, else an idle one is woken.
fn enqueue(sh: &Arc<Shared>, g: &mut State, id: u64) {
    let e = g
        .tasks
        .get_mut(&id)
        .expect("lean-runtime: enqueue of a task not in the table");
    debug_assert!(!e.sync && e.flags & (QUEUED | WAITING | RUNNING) == 0);
    if e.prio as usize == DEDICATED {
        spawn_dedicated(sh, g, id);
        return;
    }
    e.flags |= QUEUED;
    let p = e.prio as usize;
    g.queues[p].push_back(id);
    g.queued += 1;
    if g.idle == 0 && g.live < g.max() {
        spawn_worker(sh, g);
    } else {
        wake_one(sh, g);
    }
}

/// `m_queue_cv.notify_one()` after an enqueue or a raised limit: an idle
/// worker takes a queued task. A worker waiting on `queue_cv` is woken, if
/// one is (it takes a task once this hold ends). Otherwise every idle
/// worker is in its `task_end` hook (`hook_idle`) and cannot wait on
/// `queue_cv`: the wake is counted (`wakes`), and when this hold ends
/// `hand_off` gives such a worker the queue's next task (hunt HMT2-01: a
/// raise during the hook found the worker idle and made no worker, though
/// it was about to take a queued task). Until then the worker stays idle,
/// as natively a signalled worker is until it holds the lock again (hunt
/// HMT3-01: handed a task at once, it counted busy in the middle of a walk,
/// so the walk's next enqueue made a worker, and a finishing worker's walk
/// gave its first dependent away instead of taking it).
fn wake_one(sh: &Shared, g: &mut State) {
    let hooked = g.hook_idle.len() as u32;
    if hooked > 0 && g.idle == hooked {
        g.wakes += 1;
    } else {
        sh.queue_cv.notify_one();
    }
}

/// The end of a hold (`Guard`): the wakes `wake_one` counted in it reach the
/// workers in their `task_end` hooks. For each wake, while a task is queued,
/// a worker in its hook (never the calling thread's own: a hook that queued
/// a task would wait for itself) takes the queue's next task (`take`, which
/// keeps the limit) into `handed`: from now on it counts busy, as natively
/// the woken worker is once it has taken the task, and it runs the task
/// once its hook has returned. Within one hold no worker joins the waiters
/// on `queue_cv` (that needs the lock), so every wake of the hold found
/// none. A wake left over (fewer such workers or queued tasks, or the
/// limit) is lost, as natively a `notify_one` that reaches no waiter is,
/// or a woken worker that finds nothing it may take waits again. A
/// finishing worker's take step comes before (`task_end`), so it takes the
/// queue's next task first (the first its walk queued, if no task was
/// queued before), as natively.
fn hand_off(g: &mut State) {
    if g.wakes == 0 {
        return;
    }
    let mut n = std::mem::take(&mut g.wakes);
    let me = running_worker();
    while n > 0 && g.queued > 0 {
        let Some(k) = g.hook_idle.iter().position(|&w| Some(w) != me) else {
            break;
        };
        let Some(id) = take(g) else {
            break;
        };
        let w = g.hook_idle.swap_remove(k);
        g.handed.push((w, id));
        n -= 1;
    }
}

/// `add_dep(src, d)` (1009-1023), under the lock: `d` waits for `src` if it
/// has not finished; otherwise it is enqueued now. `Some(d)` when the
/// caller must run it now, on its thread: a `sync` task, or any task once
/// `finish` is over, when there is no task manager and tasks run at once
/// (a bind task's `Continue` then; review RT1-02). A bind task whose
/// function returned the task itself (`src` is `d`) waits for itself, so
/// for good, as natively, where `add_dep(t, t)` puts `t` in its own list
/// of dependents (hunt HMT-01: it was queued again, and its continuation
/// ran with no value to read).
fn add_dep(sh: &Arc<Shared>, g: &mut State, src: TaskId, d: u64) -> Option<u64> {
    if let Some(s) = g.tasks.get_mut(&src.0) {
        s.deps.push(d);
        g.tasks
            .get_mut(&d)
            .expect("lean-runtime: a dependent not in the table")
            .flags |= WAITING;
        return None;
    }
    if !g.started || g.tasks.get(&d).is_some_and(|e| e.sync) {
        return Some(d);
    }
    enqueue(sh, g, d);
    None
}

// ---------------------------------------------------------------------------
// Running tasks and walking dependents

/// A walk of the dependents of a finished task or resolved promise
/// (`handle_finished`, 938-952), followed by `resolve_core`'s
/// `notify_all` (935).
struct Walk {
    /// The dependents not walked yet, the newest last.
    deps: Vec<u64>,
    /// Passed on to every dependent (`handle_finished`, 943-944).
    canceled: bool,
    /// Its end notifies the waiters: not for a task released before it
    /// finished (`DELETED`, `UNREFERENCED`), whose finish natively skips
    /// `resolve_core` (`run_task`, 905-912).
    notify: bool,
    /// The glue's `task_end(own)` at its end (the task's `task_begin`'s
    /// pair, as in the single-thread scheduler: after its `sync` dependents
    /// ran); `None` for a promise.
    end: Option<bool>,
}

/// `run_task` (885-925) for task `id`, then the walks of dependents it
/// leads to, on the calling thread; called with the lock held, and returns
/// with it held. `own`: the task runs as on a thread of its own (a worker's
/// pool task, a dedicated task), so the glue gives it fresh streams;
/// otherwise it is a `sync` task on the current thread.
///
/// Native recursion (a `sync` dependent's `run_task` inside the walk's
/// `enqueue_core`, its own walk inside that) becomes a loop over a stack of
/// walks, in the same order: a long chain of `sync` dependents uses no
/// stack.
pub(crate) fn run_task<'a>(sh: &'a Arc<Shared>, g: Guard<'a>, id: u64, own: bool) -> Guard<'a> {
    drive(sh, g, Some((id, own)), Vec::new(), &mut Turn::Other)
}

/// The loop of `run_task` and `resolve`: run `next` if any, then go on with
/// the innermost walk. `turn`: `Turn::Worker` when a standard worker runs
/// `next`, its task; the task's `task_end` counts it idle and takes its
/// next task (`Turn::Idle`, `Turn::Took`).
fn drive<'a>(
    sh: &'a Arc<Shared>,
    mut g: Guard<'a>,
    mut next: Option<(u64, bool)>,
    mut walks: Vec<Walk>,
    turn: &mut Turn,
) -> Guard<'a> {
    loop {
        if let Some((id, own)) = next.take() {
            let (g2, again) = run_one(sh, g, id, own, &mut walks, turn);
            g = g2;
            next = again.map(|j| (j, false));
            continue;
        }
        let Some(w) = walks.last_mut() else {
            return g;
        };
        match w.deps.pop() {
            Some(d) => {
                let canceled = w.canceled;
                let Some(e) = g.tasks.get_mut(&d) else {
                    // released while it waited (natively freed here)
                    continue;
                };
                e.flags &= !WAITING;
                if canceled {
                    e.canceled = true;
                }
                // After `finish` there is no task manager: a promise resolved
                // then (a translator's value dropped at exit) runs its
                // dependents here, as tasks run at once then.
                if e.sync || !g.started {
                    next = Some((d, false));
                } else {
                    enqueue(sh, &mut g, d);
                }
            }
            None => {
                let w = walks.pop().expect("a walk");
                // The glue's `task_end` first, then the waiters wake: natively
                // the worker keeps the lock from `resolve_core`'s
                // notification until it is idle, unless a later `sync`
                // dependent of the walk runs. The hook's unlock is part of the
                // task's run, as its job's is, so no waiter goes on in it
                // (fixes-19: the hook of a `sync` dependent, the last task
                // of a walk, let a waiter it woke see the worker busy).
                if let Some(own) = w.end {
                    g = task_end(sh, g, own, turn);
                }
                if w.notify {
                    sh.finished_cv.notify_all();
                }
            }
        }
    }
}

/// The glue's `task_end(own)`, outside the lock (the clone of the glue is
/// dropped under the hook's guard too: RT1-01). A standard worker's task
/// (`own` and `Turn::Worker`) first counts the worker idle and runs the
/// worker loop's take step, under the lock: natively
/// `m_idle_std_workers++` and the loop's `dequeue` follow `resolve_core`
/// with no unlock between (fixes-19, hunt HMT2-01). So a thread that sees
/// the task finished and queues a task finds the worker idle, and the task
/// goes to it (`enqueue_core`), not to a new worker
/// (`tasks/worker_keeps_streams`, 1 run in 5 under load before fixes-19);
/// and a task queued before goes to it at once, so a raise of the limit
/// meanwhile finds it busy and makes a worker. `turn` becomes `Took(id)`,
/// the task to run next, or `Idle`. The take step comes before the hold
/// ends, and so before its hand-offs (`hand_off`): the worker takes the
/// queue's next task first (the first its walk queued, if no task was
/// queued before), and the walk's other wakes go to workers in their hooks
/// (hunt HMT3-01).
///
/// A worker counted idle in its hook cannot wait on `queue_cv`: it is in
/// `hook_idle` meanwhile, and a task queued then (or a raise with a task
/// queued) is handed to it when that hold ends (`wake_one`, `hand_off`),
/// which it runs next. So it counts as idle in its hook only while no task
/// is there for it, as natively. A hook that never returns keeps a task
/// handed to it from running (`Glue::task_end`: a hook must not block).
fn task_end<'a>(sh: &'a Arc<Shared>, mut g: Guard<'a>, own: bool, turn: &mut Turn) -> Guard<'a> {
    let glue = g.glue.clone();
    let mut hooked = None;
    if own && *turn == Turn::Worker {
        g.idle += 1;
        let next = if g.queued > 0 { take(&mut g) } else { None };
        *turn = next.map_or(Turn::Idle, Turn::Took);
        if next.is_none() && glue.is_some() {
            hooked = running_worker();
            if let Some(me) = hooked {
                g.hook_idle.push(me);
            }
        }
    }
    let Some(gl) = glue else {
        return g;
    };
    drop(g);
    hook(move || {
        gl.task_end(own);
        drop(gl);
    });
    let mut g = sh.lock();
    if let Some(me) = hooked {
        if let Some(k) = g.handed.iter().position(|&(w, _)| w == me) {
            *turn = Turn::Took(g.handed.swap_remove(k).1);
        } else {
            let k = g
                .hook_idle
                .iter()
                .position(|&w| w == me)
                .expect("lean-runtime: a worker left hook_idle with no task");
            g.hook_idle.swap_remove(k);
        }
    }
    g
}

/// One run of task `id`'s job (`run_task`'s body). When it finishes, its
/// entry leaves the table (from now on it is finished: native sets
/// `m_value` before `handle_finished`, 929) and its walk is pushed on
/// `walks`. A bind task that continues as an unfinished task waits for it
/// (`add_dep`, 919-923); `Some(id)` if it must run again here at once (a
/// `sync` one whose new source has finished).
fn run_one<'a>(
    sh: &'a Arc<Shared>,
    mut g: Guard<'a>,
    id: u64,
    own: bool,
    walks: &mut Vec<Walk>,
    turn: &mut Turn,
) -> (Guard<'a>, Option<u64>) {
    let Some(e) = g.tasks.get_mut(&id) else {
        // released before it began: a dedicated or queued pure task
        // (natively freed by `run_task`, 887-890)
        return (g, None);
    };
    let job = e.job.take().expect("lean-runtime: a task ran twice");
    e.flags = (e.flags & !(QUEUED | WAITING)) | RUNNING;
    let flag = Arc::new(AtomicBool::new(e.canceled));
    e.cancel_flag = Some(flag.clone());
    let frame = Frame {
        id,
        // on a thread of its own at a pool priority: a worker's task. A
        // `sync` task, or a task run at once after `finish` (`own` false),
        // runs on the thread below it and holds no worker (RT1-02).
        pool: own && (e.prio as usize) < DEDICATED,
        sync: e.sync,
        canceled: flag,
        shared: sh.clone(),
        notify_at_end: Cell::new(false),
    };
    let glue = g.glue.clone();
    drop(g);

    // The job and its hooks, outside the lock; a panic aborts. (While the
    // thread's locals are destroyed, a task run by a translator's
    // destructor, a `sync` dependent of a promise it resolves, has no frame:
    // it is then neither canceled nor `sync` for `check_canceled` and
    // `in_sync_task`.)
    let guard = AbortOnUnwind("a task's job");
    let framed = CURRENT.try_with(|r| r.borrow_mut().push(frame)).is_ok();
    if let Some(gl) = &glue {
        gl.task_begin(own);
    }
    drop(glue);
    let out = job();
    // whether the job ended its task itself and its waiters wait for the
    // `task_end` below (`end_running_task`)
    let mut notify_at_end = false;
    if framed {
        let frame = CURRENT.with(|r| r.borrow_mut().pop());
        notify_at_end = frame.as_ref().is_some_and(|f| f.notify_at_end.get());
        drop(frame);
    }
    std::mem::forget(guard);

    let mut g = sh.lock();
    match out {
        Outcome::Done => match g.tasks.remove(&id) {
            Some(e) => {
                walks.push(Walk {
                    deps: e.deps,
                    canceled: e.canceled,
                    notify: e.flags & (DELETED | UNREFERENCED) == 0,
                    end: Some(own),
                });
                (g, None)
            }
            // the job ended its task and walked its dependents itself
            // (`end_running_task`, AR-26): the glue's `task_end` is left,
            // then the waiters wake, as after a walk here (hunt HMT2-03)
            None => {
                let g = task_end(sh, g, own, turn);
                if notify_at_end {
                    sh.finished_cv.notify_all();
                }
                (g, None)
            }
        },
        Outcome::Continue(src, k) => {
            let Some(e) = g.tasks.get(&id) else {
                // a job that ended its task (`end_running_task`) returned
                // `Continue`: the glue's error; an abort with the message,
                // as a panic on a thread the crate made (review RT2-16)
                drop(AbortOnUnwind(
                    "a job that ended its task (end_running_task) and returned Continue",
                ));
                unreachable!("AbortOnUnwind's drop aborts");
            };
            // a released task that a dependent still holds runs on as a
            // started one (`release`): it continues as an unreleased one,
            // so its dependents get its value (hunt HMT-04)
            let held = e.deps.iter().any(|d| g.tasks.contains_key(d));
            let e = g.tasks.get_mut(&id).expect("found above");
            e.flags &= !RUNNING;
            e.cancel_flag = None;
            let mut again = None;
            if e.flags & DELETED != 0 && !held {
                // released meanwhile: freed, its continuation dropped
                // outside the lock (`run_task`, 905-912), under a guard: a
                // panic in a translator's destructor there aborts (RT1-01)
                g.tasks.remove(&id);
                drop(g);
                guarded("a destructor of a task's continuation", move || drop(k));
                g = sh.lock();
            } else {
                e.job = Some(k);
                again = add_dep(sh, &mut g, src, id);
            }
            // a task that runs again here at once (`again`) keeps the worker
            // busy: it counts itself idle after that run
            let g = if again.is_some() {
                task_end(sh, g, own, &mut Turn::Other)
            } else {
                task_end(sh, g, own, turn)
            };
            (g, again)
        }
    }
}

// ---------------------------------------------------------------------------
// The operations (`mt`'s public functions call these with the manager)

/// Configure the manager (`lean_init_task_manager_using`): the glue, the
/// number of workers (0: no task manager) and the stack size of threads made
/// from now on. Called again, it replaces them.
pub(crate) fn configure(sh: &Arc<Shared>, glue: Arc<dyn Glue>, workers: u32, stack_size: usize) {
    let mut g = sh.lock();
    let old = g.glue.replace(glue);
    g.limit = workers;
    g.stack_size = stack_size;
    g.started = workers > 0;
    g.ever_started |= g.started;
    sh.started.store(g.started, Ordering::Relaxed);
    // the limit may have risen
    sh.queue_cv.notify_all();
    drop(g);
    guarded("the destructor of the replaced glue", move || drop(old));
}

/// `start`'s manager: the process's, made once, bound to the calling thread
/// (its number is 0).
pub(crate) fn bind_global() -> Arc<Shared> {
    let sh = GLOBAL.get_or_init(|| Arc::new(Shared::new())).clone();
    bind(sh.clone());
    let _ = NUMBER.try_with(|n| n.set(Some(0)));
    sh
}

/// A manager of the calling thread's own, not the process's (unit tests: one
/// per test, so that tests run in parallel).
#[cfg(test)]
pub(crate) fn bind_local() -> Arc<Shared> {
    let sh = Arc::new(Shared::new());
    bind(sh.clone());
    sh
}

fn alloc(g: &mut State, job: Option<Job>, flags: u16, prio: u8, sync: bool) -> u64 {
    g.serial += 1;
    let id = g.serial;
    g.tasks.insert(
        id,
        Entry {
            flags,
            prio,
            sync,
            canceled: false,
            cancel_flag: None,
            job,
            deps: Vec::new(),
        },
    );
    id
}

/// Whether `spawn` and `depend` leave the job to their caller, which runs
/// it at once (`mt::run_at_once`): no task manager ever ran
/// (`LEAN_NUM_THREADS=0`), so every task has finished (no promise can
/// exist: `promise_new` needs the manager). Once one has run
/// (`ever_started`: after `finish`, and after a later `start_with` with no
/// workers, review RF15-C06) the table can still hold unfinished tasks
/// (an unresolved promise, a task waiting for one): a task made then runs
/// at once here too, but through the table, so that a bind task's
/// `Continue` to such a task waits for it, as RT1-02's `add_dep` makes it
/// (review RF15-A02: the continuation ran at once and read an empty slot).
fn without_manager(g: &State) -> bool {
    !g.started && !g.ever_started
}

/// The id `spawn` and `depend` return for task `id`, made after `finish`
/// and run at once as far as it could: `TaskId::FINISHED` once it has
/// finished, as a task run at once without a task manager is (the glue's
/// fast path); its own id while it waits (a bind task whose `Continue`
/// names a task that has not finished, a dependent of such a task).
fn after_finish_id(g: &State, id: u64) -> TaskId {
    if g.tasks.contains_key(&id) {
        TaskId(id)
    } else {
        TaskId::FINISHED
    }
}

/// `lean_task_spawn_core` with a task manager's state (`Err(job)`: no task
/// manager ever ran, `without_manager`, and the caller runs the job at
/// once): a new task, enqueued (`alloc_task`, `enqueue`) at the queue of
/// `prio`, the whole value (`common::priority`: above 8 dedicated, LB-39).
/// After `finish` it runs at once here, on the calling thread (`run_task`,
/// RT1-02); a bind task's `Continue` then waits for a task that has not
/// finished (`add_dep`), and its id names an unfinished task until then
/// (`after_finish_id`).
pub(crate) fn spawn(
    sh: &Arc<Shared>,
    job: Job,
    prio: u64,
    keep_alive: bool,
) -> Result<TaskId, Job> {
    let mut g = sh.lock();
    if without_manager(&g) {
        return Err(job);
    }
    let flags = if keep_alive { 0 } else { PURE };
    let id = alloc(&mut g, Some(job), flags, priority(prio), false);
    if g.started {
        enqueue(sh, &mut g, id);
        return Ok(TaskId(id));
    }
    let g = run_task(sh, g, id, false);
    Ok(after_finish_id(&g, id))
}

/// `lean_task_map_core`/`lean_task_bind_core` with a task manager's state
/// (`Err(job)` as for `spawn`): a new task waiting for `src` (`add_dep`);
/// with `sync` (natively at priority `LEAN_SYNC_PRIO`), once `src` has
/// finished it runs on the thread that walks `src`'s dependents, or here if
/// `src` has finished already. Only `sync` makes it so: `prio` (the whole
/// value) picks its queue. After `finish` every task runs at once once its
/// source has finished (`add_dep`, RT1-02): here, or on the thread that
/// resolves its source (review RF15-A02).
pub(crate) fn depend(
    sh: &Arc<Shared>,
    src: TaskId,
    job: Job,
    prio: u64,
    sync: bool,
    keep_alive: bool,
) -> Result<TaskId, Job> {
    let mut g = sh.lock();
    if without_manager(&g) {
        return Err(job);
    }
    let started = g.started;
    let flags = if keep_alive { 0 } else { PURE };
    let id = alloc(&mut g, Some(job), flags, priority(prio), sync);
    if let Some(now) = add_dep(sh, &mut g, src, id) {
        g = run_task(sh, g, now, false);
    }
    if started {
        return Ok(TaskId(id));
    }
    Ok(after_finish_id(&g, id))
}

pub(crate) fn dependent_runs_now(sh: &Arc<Shared>, src: TaskId, sync: bool) -> bool {
    let g = sh.lock();
    !g.started || (sync && !g.tasks.contains_key(&src.0))
}

pub(crate) fn is_finished(sh: &Arc<Shared>, id: TaskId) -> bool {
    !sh.lock().tasks.contains_key(&id.0)
}

/// `task_manager::wait_for` (1025-1047): block until task `id` has finished.
/// A pool task raises the pool's limit by one meanwhile and makes a worker
/// if none is idle (else wakes one), so the pool cannot starve; `wait_any`
/// does not. A task that waits for itself, or for a dependent of itself,
/// waits forever, as natively.
pub(crate) fn wait(sh: &Arc<Shared>, id: TaskId) {
    let mut g = sh.lock();
    if !g.tasks.contains_key(&id.0) {
        return;
    }
    // (a frame is `pool` only on a thread of the manager, where `started`
    // holds; the check keeps a `start_with(.., 0, ..)` made meanwhile from
    // making a worker)
    let in_pool = innermost(|f| f.pool).unwrap_or(false) && g.started;
    if in_pool {
        g.raised += 1;
        if g.idle == 0 {
            spawn_worker(sh, &mut g);
        } else {
            wake_one(sh, &mut g);
        }
    }
    #[cfg(test)]
    sh.blocked_waits.fetch_add(1, Ordering::Relaxed);
    while g.tasks.contains_key(&id.0) {
        g = wait_on(&sh.finished_cv, g);
    }
    #[cfg(test)]
    sh.blocked_waits.fetch_sub(1, Ordering::Relaxed);
    if in_pool {
        g.raised -= 1;
    }
}

/// `task_manager::wait_any` (1049-1058): the index of the first finished
/// task of `ids`; else block until a task's finish notifies, and look again.
/// The thread keeps its worker (no raise of the limit).
pub(crate) fn wait_any(sh: &Arc<Shared>, ids: &[TaskId]) -> usize {
    let mut g = sh.lock();
    loop {
        if let Some(k) = ids
            .iter()
            .position(|id| id.0 == 0 || !g.tasks.contains_key(&id.0))
        {
            return k;
        }
        g = wait_on(&sh.finished_cv, g);
    }
}

/// `get_task_state` (1085-1097): finished once out of the table; running
/// while its job runs, or an unresolved promise (no closure); otherwise
/// waiting (queued, waiting for its source, or a dedicated task its thread
/// has not begun).
pub(crate) fn state(sh: &Arc<Shared>, id: TaskId) -> super::TaskState {
    use super::TaskState;
    match sh.lock().tasks.get(&id.0) {
        None => TaskState::Finished,
        Some(e) if e.flags & (RUNNING | PROMISE) != 0 => TaskState::Running,
        Some(_) => TaskState::Waiting,
    }
}

/// `task_manager::cancel` (1074-1079).
pub(crate) fn cancel(sh: &Arc<Shared>, id: TaskId) {
    let mut g = sh.lock();
    if let Some(e) = g.tasks.get_mut(&id.0) {
        e.canceled = true;
        if let Some(f) = &e.cancel_flag {
            f.store(true, Ordering::Relaxed);
        }
    }
}

/// `deactivate_task` (1060-1072, `deactivate_task_core` 811-829): the
/// translator's last reference to task `id` is gone. A pure task that has
/// not started leaves the table (natively it stays queued, marked deleted,
/// and is freed when dequeued or walked) and its job is returned, to be
/// dropped by the caller outside the lock. A running pure task is marked
/// deleted and canceled (`m_deleted`, `m_canceled`): it runs to its end, and
/// its finish notifies nobody. An IO task runs to completion (its
/// keep-alive reference, `alloc_task` 1173-1174), and its finish notifies
/// nobody either. A promise is not released this way (its drop resolves
/// it).
pub(crate) fn release(sh: &Arc<Shared>, id: TaskId) -> Option<Job> {
    let mut g = sh.lock();
    let g = &mut *g;
    let e = g.tasks.get(&id.0)?;
    if e.flags & PROMISE != 0 {
        return None;
    }
    // A dependent still in the table holds this task through its job, which
    // a translator's references rule out: then it runs as a started one.
    let held = e.deps.iter().any(|d| g.tasks.contains_key(d));
    let e = g.tasks.get_mut(&id.0)?;
    if e.flags & PURE == 0 {
        e.flags |= UNREFERENCED;
        return None;
    }
    if e.flags & RUNNING != 0 || held {
        e.flags |= DELETED;
        e.canceled = true;
        if let Some(f) = &e.cancel_flag {
            f.store(true, Ordering::Relaxed);
        }
        return None;
    }
    let mut e = g.tasks.remove(&id.0)?;
    if e.flags & QUEUED != 0 {
        g.queued -= 1;
    }
    e.job.take()
}

/// `lean_promise_new` with the task manager running.
pub(crate) fn promise_new(sh: &Arc<Shared>) -> Option<TaskId> {
    let mut g = sh.lock();
    if !g.started {
        return None;
    }
    Some(TaskId(alloc(&mut g, None, PROMISE, 0, false)))
}

/// Gives the claim of a resolver whose `store` panicked back (the promise is
/// unresolved again), and wakes the resolvers that wait for it.
struct Unclaim<'a> {
    sh: &'a Shared,
    id: u64,
}

impl Drop for Unclaim<'_> {
    fn drop(&mut self) {
        let mut g = self.sh.lock();
        if let Some(e) = g.tasks.get_mut(&self.id) {
            e.flags &= !RESOLVING;
        }
        drop(g);
        self.sh.finished_cv.notify_all();
    }
}

/// `task_manager::resolve` (995-1007): if promise `id` is unresolved, claim
/// it, run `store` outside the lock, then finish it and walk its dependents
/// on this thread (its `sync` ones run here), and notify. The first
/// resolution wins. A resolver that finds another one's `store` running
/// waits until that one has finished the promise, as natively it waits for
/// the lock, which the first holds until it has set `m_value`; then it
/// returns false, `store` not called.
pub(crate) fn resolve(sh: &Arc<Shared>, id: TaskId, store: impl FnOnce()) -> bool {
    let mut g = sh.lock();
    loop {
        match g.tasks.get_mut(&id.0) {
            None => return false,
            Some(e) if e.flags & PROMISE == 0 => return false,
            Some(e) if e.flags & RESOLVING != 0 => {
                g.resolvers_waiting += 1;
                g = wait_on(&sh.finished_cv, g);
                g.resolvers_waiting -= 1;
            }
            Some(e) => {
                e.flags |= RESOLVING;
                break;
            }
        }
    }
    drop(g);
    let unclaim = Unclaim { sh, id: id.0 };
    store();
    std::mem::forget(unclaim);
    let mut g = sh.lock();
    let e = g
        .tasks
        .remove(&id.0)
        .expect("lean-runtime: a claimed promise left the table");
    if g.resolvers_waiting > 0 {
        sh.finished_cv.notify_all();
    }
    let w = Walk {
        deps: e.deps,
        canceled: e.canceled,
        notify: true,
        end: None,
    };
    drop(drive(sh, g, None, vec![w], &mut Turn::Other));
    true
}

/// `end_running_task` (AR-26): task `id`, the innermost task running on
/// this thread, whose job has stored its value, ends now: it leaves the
/// table and its dependents are walked here (its `sync` ones run here), as
/// after its job returns `Outcome::Done`, but with the job's own state
/// still in place. The glue's `task_end` stays for `run_one`, and so does
/// the wake of its waiters (the frame's `notify_at_end`): `run_one` wakes
/// them after that `task_end`, once a standard worker counts itself idle,
/// as after a walk of its own (hunt HMT2-03: they woke here, and a waiter
/// that queued a task while the job finished up found the worker busy and
/// made a new one). Nothing when `id` is not the innermost task this thread
/// runs (a job the glue runs itself; review RT2-14), or once it has ended
/// (a second call).
pub(crate) fn end_running_task(id: TaskId) {
    if innermost(|f| f.id) != Some(id.0) {
        return;
    }
    let id = id.0;
    with_shared(|sh| {
        let Some(sh) = sh else {
            return;
        };
        let mut g = sh.lock();
        let Some(e) = g.tasks.remove(&id) else {
            return;
        };
        let notify = e.flags & (DELETED | UNREFERENCED) == 0;
        innermost(|f| f.notify_at_end.set(notify));
        let w = Walk {
            deps: e.deps,
            canceled: e.canceled,
            notify: false,
            end: None,
        };
        drop(drive(sh, g, None, vec![w], &mut Turn::Other));
    })
}

/// LB-32's wake (`option_get_or_block`): every waiter looks again, so the
/// waiters of the tasks whose walks are in progress on this thread (they
/// have finished) return.
pub(crate) fn wake_waiters(sh: &Shared) {
    sh.finished_cv.notify_all();
}

/// `lean_finalize_task_manager` (`~task_manager`, 972-988): set the shutdown
/// flag, wake the workers, and wait until no task is queued, no worker is
/// left (each ends once the queue is empty) and no dedicated thread is
/// left; then join them. A task enqueued meanwhile gets a worker and runs
/// (`spawn_worker`, LB-13). Afterwards there is no task manager: tasks run
/// at once, as with `g_task_manager` null.
pub(crate) fn finish(sh: &Arc<Shared>) {
    let mut g = sh.lock();
    if !g.started {
        return;
    }
    g.shutting_down = true;
    sh.shutting_down.store(true, Ordering::Relaxed);
    sh.queue_cv.notify_all();
    // `~task_manager` (object.cpp 972-988): the standard workers leave
    // their loops once the queue is empty, and are joined (their thread
    // finalizers, here their thread-locals' destructors, drop their current
    // streams), and only then are the dedicated threads waited for
    // (review AR-34)
    while !(g.queued == 0 && g.live == 0) {
        g = wait_on(&sh.quiet_cv, g);
    }
    let workers = std::mem::take(&mut g.worker_handles);
    let glue = g.glue.clone();
    drop(g);
    for h in workers {
        let _ = h.join();
    }
    if let Some(gl) = glue {
        hook(move || {
            gl.workers_end();
            drop(gl);
        });
    }
    let mut g = sh.lock();
    while !(g.queued == 0 && g.live == 0 && g.dedicated == 0) {
        g = wait_on(&sh.quiet_cv, g);
    }
    let mut handles = std::mem::take(&mut g.worker_handles);
    handles.append(&mut g.dedicated_handles);
    g.started = false;
    sh.started.store(false, Ordering::Relaxed);
    drop(g);
    for h in handles {
        let _ = h.join();
    }
}

/// The number of live workers (tests).
#[cfg(test)]
pub(crate) fn live_workers(sh: &Shared) -> u32 {
    sh.lock().live
}

/// The number of threads blocked in `wait` (tests).
#[cfg(test)]
pub(crate) fn blocked_waits(sh: &Shared) -> u32 {
    sh.blocked_waits.load(Ordering::Relaxed)
}

/// The number of entries in the table (tests).
#[cfg(test)]
pub(crate) fn table_len(sh: &Shared) -> usize {
    sh.lock().tasks.len()
}
