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
    /// Runs on the thread that enqueues it (`sync := true`, or priority
    /// `LEAN_SYNC_PRIO`: native `enqueue_core` runs it there, 793-796).
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
}

type Guard<'a> = MutexGuard<'a, State>;

impl Shared {
    fn new() -> Shared {
        Shared {
            st: Mutex::new(State {
                started: false,
                shutting_down: false,
                glue: None,
                stack_size: crate::sched::thread_stack_size(),
                limit: 0,
                raised: 0,
                live: 0,
                idle: 0,
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
        }
    }

    /// The lock. A panic never unwinds while it is held (jobs and hooks
    /// abort the process, `AbortOnUnwind`), so poisoning is ignored.
    fn lock(&self) -> Guard<'_> {
        self.st.lock().unwrap_or_else(PoisonError::into_inner)
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

fn wait_on<'a>(cv: &Condvar, g: Guard<'a>) -> Guard<'a> {
    cv.wait(g).unwrap_or_else(PoisonError::into_inner)
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
    loop {
        if g.queued == 0 {
            if g.shutting_down {
                break;
            }
            g = wait_on(&sh.queue_cv, g);
            continue;
        }
        // `live >= idle`: a worker counts itself idle only while it lives.
        if !g.shutting_down && g.live - g.idle >= g.max() {
            g = wait_on(&sh.queue_cv, g);
            continue;
        }
        let Some(id) = dequeue(&mut g) else {
            continue;
        };
        g.idle -= 1;
        g = run_task(&sh, g, id, true);
        g.idle += 1;
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
        sh.queue_cv.notify_one();
    }
}

/// `add_dep(src, d)` (1009-1023), under the lock: `d` waits for `src` if it
/// has not finished; otherwise it is enqueued now. `Some(d)` when the
/// caller must run it now, on its thread: a `sync` task, or any task once
/// `finish` is over, when there is no task manager and tasks run at once
/// (a bind task's `Continue` then; review RT1-02).
fn add_dep(sh: &Arc<Shared>, g: &mut State, src: TaskId, d: u64) -> Option<u64> {
    if src.0 != d {
        if let Some(s) = g.tasks.get_mut(&src.0) {
            s.deps.push(d);
            g.tasks
                .get_mut(&d)
                .expect("lean-runtime: a dependent not in the table")
                .flags |= WAITING;
            return None;
        }
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
    drive(sh, g, Some((id, own)), Vec::new())
}

/// The loop of `run_task` and `resolve`: run `next` if any, then go on with
/// the innermost walk.
fn drive<'a>(
    sh: &'a Arc<Shared>,
    mut g: Guard<'a>,
    mut next: Option<(u64, bool)>,
    mut walks: Vec<Walk>,
) -> Guard<'a> {
    loop {
        if let Some((id, own)) = next.take() {
            let (g2, again) = run_one(sh, g, id, own, &mut walks);
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
                if w.notify {
                    sh.finished_cv.notify_all();
                }
                if let Some(own) = w.end {
                    g = task_end(sh, g, own);
                }
            }
        }
    }
}

/// The glue's `task_end(own)`, outside the lock (the clone of the glue is
/// dropped under the hook's guard too: RT1-01).
fn task_end<'a>(sh: &'a Arc<Shared>, g: Guard<'a>, own: bool) -> Guard<'a> {
    let Some(gl) = g.glue.clone() else {
        return g;
    };
    drop(g);
    hook(move || {
        gl.task_end(own);
        drop(gl);
    });
    sh.lock()
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
    if framed {
        let frame = CURRENT.with(|r| r.borrow_mut().pop());
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
            // (`end_running_task`, AR-26): only the glue's `task_end` is left
            None => (task_end(sh, g, own), None),
        },
        Outcome::Continue(src, k) => {
            let Some(e) = g.tasks.get_mut(&id) else {
                // a job that ended its task (`end_running_task`) returned
                // `Continue`: the glue's error; an abort with the message,
                // as a panic on a thread the crate made (review RT2-16)
                drop(AbortOnUnwind(
                    "a job that ended its task (end_running_task) and returned Continue",
                ));
                unreachable!("AbortOnUnwind's drop aborts");
            };
            e.flags &= !RUNNING;
            e.cancel_flag = None;
            let mut again = None;
            if e.flags & DELETED != 0 {
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
            (task_end(sh, g, own), again)
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

/// `lean_task_spawn_core` with the task manager running (`Some`; `None`:
/// there is none, and the caller runs the job at once): a new task, enqueued
/// (`alloc_task`, `enqueue`); at priority `LEAN_SYNC_PRIO` it runs now, on
/// this thread.
pub(crate) fn spawn(
    sh: &Arc<Shared>,
    job: Job,
    prio: u64,
    keep_alive: bool,
) -> Result<TaskId, Job> {
    let mut g = sh.lock();
    if !g.started {
        return Err(job);
    }
    let (p, sp) = priority(prio);
    let flags = if keep_alive { 0 } else { PURE };
    let id = alloc(&mut g, Some(job), flags, p, sp);
    if sp {
        g = run_task(sh, g, id, false);
    } else {
        enqueue(sh, &mut g, id);
    }
    drop(g);
    Ok(TaskId(id))
}

/// `lean_task_map_core`/`lean_task_bind_core` with the task manager running:
/// a new task waiting for `src` (`add_dep`); with `sync`, its priority is
/// `LEAN_SYNC_PRIO`, so once `src` has finished it runs on the thread that
/// walks `src`'s dependents, or here if `src` has finished already.
pub(crate) fn depend(
    sh: &Arc<Shared>,
    src: TaskId,
    job: Job,
    prio: u64,
    sync: bool,
    keep_alive: bool,
) -> Result<TaskId, Job> {
    let mut g = sh.lock();
    if !g.started {
        return Err(job);
    }
    let (p, sp) = priority(prio);
    let flags = if keep_alive { 0 } else { PURE };
    let id = alloc(&mut g, Some(job), flags, p, sync || sp);
    if let Some(now) = add_dep(sh, &mut g, src, id) {
        g = run_task(sh, g, now, false);
    }
    drop(g);
    Ok(TaskId(id))
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
            sh.queue_cv.notify_one();
        }
    }
    while g.tasks.contains_key(&id.0) {
        g = wait_on(&sh.finished_cv, g);
    }
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
    drop(drive(sh, g, None, vec![w]));
    true
}

/// `end_running_task` (AR-26): task `id`, the innermost task running on
/// this thread, whose job has stored its value, ends now: it leaves the
/// table and its dependents are walked here (its `sync` ones run here), then
/// its waiters wake, as after its job returns `Outcome::Done`, but with the
/// job's own state still in place. The glue's `task_end` stays for
/// `run_one`. Nothing when `id` is not the innermost task this thread runs
/// (a job the glue runs itself; review RT2-14), or once it has ended (a
/// second call).
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
        let w = Walk {
            deps: e.deps,
            canceled: e.canceled,
            notify: e.flags & (DELETED | UNREFERENCED) == 0,
            end: None,
        };
        drop(drive(sh, g, None, vec![w]));
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

/// The number of entries in the table (tests).
#[cfg(test)]
pub(crate) fn table_len(sh: &Shared) -> usize {
    sh.lock().tasks.len()
}
