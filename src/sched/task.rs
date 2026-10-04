//! Deferred tasks: Lean's task manager (`src/runtime/object.cpp`,
//! `task_manager`) on the scheduler's contexts.
//!
//! After the task manager has started, every new task is *deferred*: it
//! stays pending until the program needs it (`wait`), or the running code
//! blocks with a worker free, or an effect point finds it queued for a while,
//! or the program polls for it, or `main` returns (`finish`), which runs every
//! queued task as Lean's finalization does. Each such schedule is one a
//! native thread pool can produce (a worker may start a task at any time
//! after it is created, and must have finished it when its value is
//! needed). From lean2rr's leanrt (`task.rs`), with the task's computation a
//! boxed closure (`Job`) instead of a Reussir cell, and Lean's own deletion
//! of dropped pure tasks (`release`) instead of reference counts read off
//! cells.
//!
//! A task is an entry in a slab, named by a `TaskId` (entry and generation);
//! an id whose entry is gone names a finished task. The translator keeps the
//! task's value in its own object: the job fills it before it returns.
//!
//! Pure tasks (`Task.spawn`, `Task.map`, `Task.bind`: `keep_alive = false`)
//! differ from lean2rr's model in one rule (docs/sched.md, "Pure tasks"):
//! where a worker would start one that no IO task waits for, it is only
//! marked started (`pick`), and runs when it is needed, polled, at exit, or
//! when nothing else can go on. A pure task has no effects, so running it
//! later is the schedule of a slow worker; running it at once on a context
//! would let a runaway one starve `main`, which natively goes on in parallel
//! (case `tasks/runaway_pure_task_started`).

use super::ctx::{block, yield_now, CtxId, Status, Wait, MAIN, STALE};
use super::{glue_opt, with, Glue, Sched};
use std::collections::VecDeque;
use std::rc::Rc;
use std::time::{Duration, Instant};

/// A task's computation. It fills the translator's result slot, then returns
/// `Outcome::Done`; or, for a bind task whose function returned a task that
/// has not finished, `Outcome::Continue`.
pub type Job = Box<dyn FnOnce() -> Outcome>;

/// What a job did.
pub enum Outcome {
    /// It has stored the task's value: the task has finished.
    Done,
    /// A bind task's function returned `TaskId`, which has not finished: the
    /// task waits for it, keeping its priority and `sync` flag, and then
    /// runs `Job` (which copies that task's value), as Lean's
    /// `task_bind_fn1` sets the task's closure again.
    Continue(TaskId, Job),
}

/// A task: an entry of the scheduler and its generation. The id of a task
/// whose entry is gone (finished, or deleted) answers as finished.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct TaskId(u64);

impl TaskId {
    /// A finished task (`Task.pure`, or one that ran at once).
    pub const FINISHED: TaskId = TaskId(0);

    fn new(i: u32, g: u32) -> TaskId {
        TaskId(((g as u64) << 32) | i as u64)
    }
    fn idx(self) -> u32 {
        self.0 as u32
    }
    fn gen(self) -> u32 {
        (self.0 >> 32) as u32
    }
    /// The id as a word (for a translator that stores it in its own values).
    pub fn to_bits(self) -> u64 {
        self.0
    }
    pub fn from_bits(b: u64) -> TaskId {
        TaskId(b)
    }
}

/// `IO.TaskState`, its constructors in Lean's order.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TaskState {
    Waiting = 0,
    Running = 1,
    Finished = 2,
}

/// The message of the Lean panic that native `Task.get` prints when it waits
/// for an unfinished task inside a `sync := true` task
/// (`task_manager::wait_for`, `src/runtime/object.cpp`): see `in_sync_task`.
pub const GET_IN_SYNC_TASK: &str = "`Task.get` called from a `(sync := true)` task";

/// The message of Lean's internal panic for `IO.Promise.new` before the task
/// manager runs (`lean_promise_new`); the glue reports it as Lean's
/// `lean_internal_panic` does.
pub const PROMISE_BEFORE_MANAGER: &str = "`IO.Promise.new` called before the task manager is running; this typically happens when called (directly or transitively, e.g. via `IO.CancelToken.new`) from an `initialize` block. Construct lazily on first use instead.";

pub(crate) const NONE: u32 = u32::MAX;

/// Priorities: Lean's 0..=8 (`Task.Priority.max`), and 9 for dedicated
/// tasks (a thread of their own natively, so always started).
const PRIOS: usize = 10;
const DEDICATED: usize = PRIOS - 1;

// Entry flags.
/// `Task.spawn`/`map`/`bind` (`keep_alive = false`): deleted when dropped
/// before it starts.
const PURE: u32 = 1 << 0;
/// Runs on the thread that finishes the task it waits for (`sync := true`,
/// or priority `LEAN_SYNC_PRIO`).
const SYNC: u32 = 1 << 1;
/// In its priority's queue.
const QUEUED: u32 = 1 << 2;
/// Waits for `link` (a dependent, or a bind task waiting for the task it
/// continues as); in that task's list of dependents.
const WAITING: u32 = 1 << 3;
const RUNNING: u32 = 1 << 4;
const CANCELED: u32 = 1 << 5;
/// Natively it could have started before `main` returned (`check_canceled`).
const EARLY: u32 = 1 << 6;
/// An unresolved promise: no computation.
const PROMISE: u32 = 1 << 7;
/// Runs on the current thread when it begins (a `sync` dependent handed by a
/// walk, a task at priority `LEAN_SYNC_PRIO`): `thread` is its thread.
const INLINE: u32 = 1 << 8;
/// `IO.checkCanceled` was called in the current run.
const CHECKED: u32 = 1 << 9;
/// Priority `LEAN_SYNC_PRIO` (2^32-1): runs as soon as it is enqueued.
const SYNCPRIO: u32 = 1 << 10;
/// Handed by a walk of dependents: its own walk is continued by that walk's
/// loop.
const FROM_WALK: u32 = 1 << 11;
/// Running on the thread of whoever ran it (it began `INLINE`).
const ON_THREAD: u32 = 1 << 12;
/// A pure task a worker has started (natively), not run yet (`pick`).
const PICKED: u32 = 1 << 13;
/// Dropped by the program after it started: its value is discarded
/// (Lean's `m_deleted` on a running task).
const DELETED: u32 = 1 << 14;
/// Finished; the entry stays until its dependents are walked.
const FINISHED: u32 = 1 << 15;

struct Entry {
    /// 0 for a free slot.
    gen: u32,
    flags: u32,
    prio: u8,
    job: Option<Job>,
    /// Its dependents, newest first; its siblings in its source's list.
    head_dep: u32,
    next_dep: u32,
    prev_dep: u32,
    /// `QUEUED`: the sequence number of its queue item (stale items are
    /// skipped); `WAITING`: the task it waits for (its source).
    link: u32,
    /// Pending: `IO.getTaskState` reported it waiting (`query`): the sleep
    /// count + 1 at the first such answer (0: never), and the number of
    /// answers. Running: `aux[1]` is the sleep count when the run started.
    aux: [u32; 2],
    /// Running (or about to run `INLINE`): its thread number
    /// (`thread_number`).
    thread: u64,
    /// Its *IO need*: the number of its dependents that are IO tasks, or
    /// pure tasks with IO need themselves (`need_up`). A pure task with IO
    /// need is started as an IO task is (`eligible`).
    io_need: u32,
    /// When it was last queued.
    queued_at: Option<Instant>,
}

impl Entry {
    const fn free() -> Entry {
        Entry {
            gen: 0,
            flags: 0,
            prio: 0,
            job: None,
            head_dep: NONE,
            next_dep: NONE,
            prev_dep: NONE,
            link: NONE,
            aux: [0, 0],
            thread: 0,
            io_need: 0,
            queued_at: None,
        }
    }
}

/// The dependents of a finished task still to be walked.
struct Walk {
    /// The finished task's entry, kept until the walk is over: its list of
    /// dependents is the walk's, newest first, as Lean's `handle_finished`
    /// walks them.
    owner: u32,
    /// The thread that finished the task (its `sync` dependents run there).
    thread: u64,
    /// The task finished before Lean's shutdown flag was set (natively).
    early: bool,
    canceled: bool,
    /// The worker is free once the walk is over (`Tasks::worker`).
    worker: bool,
    /// The walk's own loop stops at its end (otherwise it is the walk of a
    /// `sync` dependent, continued by the loop of the enclosing walk).
    base: bool,
}

/// What belongs to a context, as natively to a thread: the tasks running on
/// it, innermost last, and the walks of dependents in progress.
#[derive(Default)]
pub(crate) struct CtxState {
    walks: Vec<Walk>,
    running: Vec<u32>,
}

pub(crate) struct Tasks {
    /// The task manager runs (`main` has started, `LEAN_NUM_THREADS` is not
    /// 0). Before, Lean has no task manager and runs every task at once.
    pub(crate) started: bool,
    /// `main` has returned; the remaining tasks run as during Lean's
    /// task-manager shutdown.
    pub(crate) shutting_down: bool,
    /// Sleeps so far (`IO.sleep`, `dbgSleep`): time passing, for the
    /// heuristics below.
    pub(crate) epoch: u32,
    slab: Vec<Entry>,
    free: Vec<u32>,
    /// Pending tasks that do not wait for another task, one queue per
    /// priority as in Lean's task manager (the highest non-empty one is
    /// taken first), in the order they were enqueued: (entry, sequence
    /// number).
    queues: [VecDeque<(u32, u32)>; PRIOS],
    /// The number of valid items of each queue.
    queued: [u32; PRIOS],
    next_q: u32,
    /// The task the lone native worker has started (`LEAN_NUM_THREADS=1`),
    /// still pending here: it runs first in the final run. An idle worker is
    /// woken by an enqueue (`wake`: when) and picks the first task of the
    /// highest non-empty queue once it has woken (a thread start,
    /// `LATENCY_COLD`, the first time, `LATENCY_WARM` later): tasks queued
    /// back to back compete by priority. When a task it ran finishes, it
    /// picks the next one right away.
    worker: u32,
    wake: Option<Instant>,
    worker_exists: bool,
    /// The last generation handed out (never 0).
    serial: u32,
    /// Pure tasks a worker has started, not run yet (`pick`), in that order:
    /// (entry, generation); items whose entry no longer has `PICKED` are
    /// stale.
    picked: VecDeque<(u32, u32)>,
    /// Started pure tasks an IO task has come to wait for since, directly or
    /// through pure tasks (`need_up`): started on a context as soon as one
    /// can be (`startable`).
    picked_io: Vec<(u32, u32)>,
}

/// How long a native worker takes to pick up a task after the enqueue that
/// woke it: measured with `LEAN_NUM_THREADS=1` (a new thread: 80-100 µs; an
/// idle one: 15-20 µs).
const LATENCY_COLD: Duration = Duration::from_micros(90);
const LATENCY_WARM: Duration = Duration::from_micros(20);

/// How long a native worker takes to start a task it is woken for (a new
/// thread's start; `zero_sleep`).
pub(crate) const WORKER_LATENCY: Duration = LATENCY_COLD;

/// How many answers of `IO.getTaskState` without time passing make a polling
/// loop (`query`).
const POLL_QUERIES: u32 = 1000;

impl Tasks {
    pub(crate) fn new() -> Tasks {
        Tasks {
            started: false,
            shutting_down: false,
            epoch: 0,
            slab: Vec::new(),
            free: Vec::new(),
            queues: Default::default(),
            queued: [0; PRIOS],
            next_q: 0,
            worker: NONE,
            wake: None,
            worker_exists: false,
            serial: 0,
            picked: VecDeque::new(),
            picked_io: Vec::new(),
        }
    }
}

impl Drop for Tasks {
    fn drop(&mut self) {
        // At thread exit, pending jobs are not dropped: they hold Lean values
        // whose destructors would call back into the scheduler during its
        // destruction (see `Contexts`'s `Drop`).
        for e in &mut self.slab {
            if let Some(job) = e.job.take() {
                std::mem::forget(job);
            }
        }
    }
}

/// Lean passes `lean_unbox(prio)` as an `unsigned`: the priority modulo
/// 2^32, where 2^32-1 is `LEAN_SYNC_PRIO` and above 8 is dedicated.
pub(crate) fn priority(prio: u64) -> (u8, bool) {
    let p = prio as u32;
    if p == u32::MAX {
        (0, true)
    } else {
        ((p as u64).min(PRIOS as u64 - 1) as u8, false)
    }
}

/// Which queued task `startable` may give, by when it was queued.
#[derive(Clone, Copy)]
pub(crate) enum Gate {
    Any,
    /// Queued at this time or before (`effect`: a task a worker would have
    /// been running for a while).
    Before(Instant),
    /// Queued after this queue position (`effect`: released by what ran at
    /// an effect point).
    After(u32),
}

/// What `wait` does next.
enum WaitStep {
    Done,
    Run(u32),
    Block(Wait),
    Hang,
    Again,
}

/// What `query` answers (lean2rr's codes 0-4).
enum Query {
    Answer(TaskState),
    /// Run the task, then it has finished (3).
    Run,
    /// Let the others go on once, then answer its state (4, and a task that
    /// cannot finish without others).
    YieldThenStatus,
}

/// What the final run does next.
enum Final {
    Run(u32),
    Wait,
    Done,
}

impl Sched {
    fn ent(&self, i: u32) -> &Entry {
        &self.tk.slab[i as usize]
    }
    fn ent_mut(&mut self, i: u32) -> &mut Entry {
        &mut self.tk.slab[i as usize]
    }
    fn st(&mut self) -> &mut CtxState {
        let c = self.cx.cur;
        &mut self.cx.ctxs[c].st
    }
    fn st_ref(&self) -> &CtxState {
        &self.cx.ctxs[self.cx.cur].st
    }
    fn id_of(&self, i: u32) -> TaskId {
        TaskId::new(i, self.ent(i).gen)
    }

    /// The entry of an unfinished task.
    pub(crate) fn find(&self, id: TaskId) -> Option<u32> {
        let (i, g) = (id.idx(), id.gen());
        match self.tk.slab.get(i as usize) {
            Some(e) if g != 0 && e.gen == g && e.flags & FINISHED == 0 => Some(i),
            _ => None,
        }
    }

    fn alloc(&mut self, job: Option<Job>, flags: u32, prio: u8) -> u32 {
        let t = &mut self.tk;
        t.serial = t.serial.wrapping_add(1);
        if t.serial == 0 {
            t.serial = 1;
        }
        let e = Entry {
            gen: t.serial,
            flags,
            prio,
            job,
            ..Entry::free()
        };
        match t.free.pop() {
            Some(i) => {
                t.slab[i as usize] = e;
                i
            }
            None => {
                t.slab.push(e);
                (t.slab.len() - 1) as u32
            }
        }
    }

    /// Remove entry `i` (unqueued, not waiting, no dependents left, its job
    /// gone).
    fn free_entry(&mut self, i: u32) {
        let e = self.ent_mut(i);
        debug_assert!(e.job.is_none() && e.head_dep == NONE);
        *e = Entry::free();
        self.tk.free.push(i);
    }

    /// The thread of the innermost running task (0: `main`).
    pub(crate) fn cur_thread(&self) -> u64 {
        match self.st_ref().running.last() {
            Some(&r) => self.ent(r).thread,
            None => self.cx.ctxs[self.cx.cur].thread_base,
        }
    }

    /// Whether running task `i` could still be running before Lean's
    /// shutdown flag was set: it started early and no time has passed in it.
    fn early_now(&self, i: u32) -> bool {
        let e = self.ent(i);
        self.tk.shutting_down
            && e.flags & EARLY != 0
            && e.flags & CHECKED == 0
            && e.aux[1] == self.tk.epoch
    }

    /// A new deferred task at priority `prio` (Lean's `Task.Priority`, as
    /// passed). Whether the caller must run it now, on the current thread
    /// (priority `LEAN_SYNC_PRIO`, not a dependent: `enqueue_core` runs it at
    /// once).
    fn register(&mut self, job: Job, prio: u64, keep_alive: bool, dep: bool) -> (u32, bool) {
        self.settle_worker();
        let (p, sp) = priority(prio);
        let mut flags = 0;
        if !keep_alive {
            flags |= PURE;
        }
        if sp {
            flags |= SYNC | SYNCPRIO;
        }
        if let Some(&r) = self.st_ref().running.last() {
            if self.early_now(r) {
                flags |= EARLY;
            }
        }
        let i = self.alloc(Some(job), flags, p);
        if dep {
            return (i, false);
        }
        if sp {
            self.run_here(i);
            return (i, true);
        }
        self.enqueue(i);
        (i, false)
    }

    /// Task `i` is to run on the current thread when it begins.
    fn run_here(&mut self, i: u32) {
        let th = self.cur_thread();
        let e = self.ent_mut(i);
        e.flags |= INLINE;
        e.thread = th;
    }

    /// Put pending task `i` at the end of its priority's queue.
    fn enqueue(&mut self, i: u32) {
        let now = Instant::now();
        let t = &mut self.tk;
        t.next_q = t.next_q.wrapping_add(1);
        let q = t.next_q;
        let e = &mut t.slab[i as usize];
        e.flags |= QUEUED;
        e.link = q;
        e.queued_at = Some(now);
        let p = e.prio as usize;
        t.queues[p].push_back((i, q));
        t.queued[p] += 1;
        // An enqueue by `main` wakes the idle worker (if one is free: tasks
        // the scheduler started on contexts of their own hold workers).
        if t.started
            && !t.shutting_down
            && t.worker == NONE
            && t.wake.is_none()
            && self.cx.cur == MAIN
            && self.st_ref().running.is_empty()
            && self.pool_in_use() < self.cx.pool_limit
        {
            self.tk.wake = Some(now);
        }
        // A task was queued: whoever waits for any progress looks again.
        if self.cx.blocked > 0 {
            self.wake_progress();
        }
    }

    /// Take task `i` off its queue: its item is removed if it is at an end
    /// of the queue (a task forced right after it was created), and becomes
    /// stale otherwise (skipped later; the queue is compacted when they pile
    /// up).
    fn unqueue(&mut self, i: u32) {
        let t = &mut self.tk;
        let e = &mut t.slab[i as usize];
        if e.flags & QUEUED == 0 {
            return;
        }
        e.flags &= !QUEUED;
        let (p, l) = (e.prio as usize, e.link);
        t.queued[p] -= 1;
        let q = &mut t.queues[p];
        if q.back() == Some(&(i, l)) {
            q.pop_back();
        } else if q.front() == Some(&(i, l)) {
            q.pop_front();
        } else if q.len() > 64 && q.len() > 4 * t.queued[p] as usize {
            let slab = &t.slab;
            q.retain(|&(j, l)| {
                let f = &slab[j as usize];
                f.gen != 0 && f.flags & QUEUED != 0 && f.link == l
            });
        }
    }

    /// Link task `d` at the head of `s`'s dependents.
    fn link(&mut self, s: u32, d: u32) {
        let h = self.ent(s).head_dep;
        let e = self.ent_mut(d);
        e.link = s;
        e.next_dep = h;
        e.prev_dep = NONE;
        e.flags |= WAITING;
        if h != NONE {
            self.ent_mut(h).prev_dep = d;
        }
        self.ent_mut(s).head_dep = d;
        if self.counts_for_need(d) {
            self.need_up(d);
        }
    }

    /// Whether dependent `d` gives its source IO need: it is an IO task, or a
    /// pure task with IO need itself.
    fn counts_for_need(&self, d: u32) -> bool {
        let e = self.ent(d);
        e.flags & PURE == 0 || e.io_need > 0
    }

    /// Dependent `d` (linked) has come to count for its source's IO need
    /// (review RS1S-01 of sched-1): the source gains one, and while a pure
    /// source goes from none to some, so does its own source, up the chain
    /// of waiting tasks (`t.map g` waited for by an IO task needs `t` too).
    /// A started pure task that gains IO need is due to run as soon as a
    /// worker can (`picked_io`).
    fn need_up(&mut self, mut d: u32) {
        loop {
            let e = self.ent(d);
            if e.flags & WAITING == 0 || e.link == NONE {
                return;
            }
            let s = e.link;
            let se = self.ent_mut(s);
            se.io_need += 1;
            if se.io_need != 1 {
                return;
            }
            let (flags, g) = (se.flags, se.gen);
            if flags & PICKED != 0 {
                self.tk.picked_io.push((s, g));
            }
            if flags & PURE == 0 {
                return;
            }
            d = s;
        }
    }

    #[cfg(test)]
    pub(crate) fn need_of(&self, i: u32) -> u32 {
        self.ent(i).io_need
    }

    /// The IO-need invariant (from the reviewer of sched-1, round 2): an
    /// entry's need counts its dependents that are IO tasks or have need,
    /// and a pending pure task has need exactly when an IO task waits for
    /// it, through pure tasks at any depth.
    #[cfg(test)]
    pub(crate) fn check_need(&self) {
        fn reach_io(s: &Sched, i: u32, depth: usize) -> bool {
            if depth > s.tk.slab.len() {
                return false;
            }
            let mut d = s.ent(i).head_dep;
            while d != NONE {
                let de = s.ent(d);
                if de.flags & PURE == 0 || reach_io(s, d, depth + 1) {
                    return true;
                }
                d = de.next_dep;
            }
            false
        }
        for (i, e) in self.tk.slab.iter().enumerate() {
            if e.gen == 0 {
                continue;
            }
            let mut n = 0;
            let mut d = e.head_dep;
            while d != NONE {
                let de = self.ent(d);
                assert_eq!(de.link, i as u32, "dependent {d} of {i} links elsewhere");
                assert!(de.flags & WAITING != 0, "dependent {d} of {i} not WAITING");
                if de.flags & PURE == 0 || de.io_need > 0 {
                    n += 1;
                }
                d = de.next_dep;
            }
            assert_eq!(e.io_need, n, "io_need of entry {i} (flags {:#x})", e.flags);
            if e.flags & PURE != 0 && e.flags & FINISHED == 0 {
                assert_eq!(
                    e.io_need > 0,
                    reach_io(self, i as u32, 0),
                    "stale need at entry {i}"
                );
            }
        }
    }

    /// The reverse of `need_up`: dependent `d` (still linked) no longer
    /// counts for its source's IO need. Bounded by the number of entries:
    /// in a cycle of pure tasks the need would never reach 0 (review
    /// RS1S-14; `need_up` stops there, at a need already counted).
    fn need_down(&mut self, mut d: u32) {
        for _ in 0..=self.tk.slab.len() {
            let e = self.ent(d);
            if e.flags & WAITING == 0 || e.link == NONE {
                return;
            }
            let s = e.link;
            let se = self.ent_mut(s);
            se.io_need = se.io_need.saturating_sub(1);
            if se.io_need != 0 || se.flags & PURE == 0 {
                return;
            }
            d = s;
        }
    }

    /// Unlink waiting task `d` from its source's dependents.
    fn unlink(&mut self, d: u32) {
        let e = self.ent(d);
        if e.flags & WAITING == 0 {
            return;
        }
        if self.counts_for_need(d) {
            self.need_down(d);
        }
        let e = self.ent(d);
        let (s, n, p) = (e.link, e.next_dep, e.prev_dep);
        if p != NONE {
            self.ent_mut(p).next_dep = n;
        } else if s != NONE {
            self.ent_mut(s).head_dep = n;
        }
        if n != NONE {
            self.ent_mut(n).prev_dep = p;
        }
        let e = self.ent_mut(d);
        e.link = NONE;
        e.next_dep = NONE;
        e.prev_dep = NONE;
        e.flags &= !WAITING;
    }

    /// `d` (just registered as a dependent) was created depending on `src`
    /// (`sync`: with `sync := true`): if `src` is unfinished, `d` waits for
    /// it and runs or is enqueued when it finishes, as Lean's `add_dep`;
    /// otherwise it is enqueued now. Whether the caller must run it now
    /// (priority `LEAN_SYNC_PRIO` and `src` finished).
    fn depend(&mut self, src: TaskId, d: u32, sync: bool) -> bool {
        if sync {
            self.ent_mut(d).flags |= SYNC;
        }
        if let Some(s) = self.find(src) {
            self.link(s, d);
            return false;
        }
        if self.ent(d).flags & SYNCPRIO != 0 {
            self.run_here(d);
            return true;
        }
        self.enqueue(d);
        false
    }

    /// Take pending task `i` off its queue, its source's dependents and the
    /// started pure tasks, to run it now.
    fn hand(&mut self, i: u32) {
        self.unqueue(i);
        self.unlink(i);
        self.ent_mut(i).flags &= !PICKED;
    }

    /// Task `i`'s job panicked (`Unwound`): it stays unfinished, and the
    /// running context drops it from its running tasks, with whatever a
    /// panic left above it. If it was the lone worker's task, the worker is
    /// free.
    fn abandon_running(&mut self, i: u32) {
        let st = self.st();
        if let Some(pos) = st.running.iter().rposition(|&r| r == i) {
            st.running.truncate(pos);
        }
        self.refresh_holds(self.cx.cur);
        if self.tk.worker == i {
            self.worker_idle();
        }
    }

    /// A panic unwound the loop of the walks above `keep` (`WalkUnwound`):
    /// they end. Their dependents not walked yet are queued, `sync` ones
    /// included, so they run later as other tasks do; their sources' entries
    /// are freed.
    fn abandon_walks(&mut self, keep: usize) {
        while self.st_ref().walks.len() > keep {
            let w = self.st().walks.pop().unwrap();
            loop {
                let d = self.ent(w.owner).head_dep;
                if d == NONE {
                    break;
                }
                self.unlink(d);
                let e = self.ent_mut(d);
                if w.canceled {
                    e.flags |= CANCELED;
                }
                if w.early {
                    e.flags |= EARLY;
                }
                self.enqueue(d);
            }
            self.free_entry(w.owner);
            if w.worker {
                self.worker_idle();
            }
        }
    }

    /// Task `i` (handed) starts running: its job, and whether it runs as on
    /// a thread of its own (a worker's) rather than on the current thread.
    fn begin(&mut self, i: u32) -> (Job, bool) {
        let th = self.cur_thread();
        let epoch = self.tk.epoch;
        let e = self.ent_mut(i);
        let own = e.flags & INLINE == 0;
        if own {
            e.thread = th + 1;
        }
        let on = if own { 0 } else { ON_THREAD };
        e.flags =
            (e.flags & !(INLINE | CHECKED | ON_THREAD | PICKED | QUEUED | WAITING)) | RUNNING | on;
        e.aux[1] = epoch;
        let job = e.job.take().expect("lean-runtime: a task ran twice");
        self.st().running.push(i);
        self.refresh_holds(self.cx.cur);
        (job, own)
    }

    /// The running task `i` has finished: its dependents are to be walked
    /// (`walk_next`). Whether the caller walks them now (0 in lean2rr's
    /// `end`: the loop of the walk that handed this task continues with
    /// them).
    fn end(&mut self, i: u32) -> bool {
        let r = self.st().running.pop();
        debug_assert_eq!(r, Some(i));
        self.refresh_holds(self.cx.cur);
        let early = self.early_now(i);
        let gen = self.ent(i).gen;
        let flags = self.ent(i).flags;
        let thread = self.ent(i).thread;
        let e = self.ent_mut(i);
        e.flags = FINISHED;
        let base = flags & FROM_WALK == 0;
        // The worker that ran it picks the next task once the walk is over.
        let t = &self.tk;
        let worker = t.worker == i
            || (t.worker == NONE
                && self.st_ref().running.is_empty()
                && t.started
                && !t.shutting_down
                && flags & ON_THREAD == 0);
        if worker {
            self.tk.worker = NONE;
            self.tk.wake = None;
        }
        let w = Walk {
            owner: i,
            thread,
            early,
            canceled: flags & CANCELED != 0,
            worker,
            base,
        };
        self.st().walks.push(w);
        self.on_finish((i, gen));
        base
    }

    /// The running bind task `i` has run its function, which returned the
    /// unfinished task `src`: it stops running and waits for `src` (keeping
    /// its priority and flags), then runs `job`. A job to drop if the task
    /// was deleted meanwhile (natively it is freed, `run_task`).
    fn bind_wait(&mut self, i: u32, src: TaskId, job: Job) -> Option<Job> {
        if self.st_ref().running.last() == Some(&i) {
            self.st().running.pop();
            self.refresh_holds(self.cx.cur);
        }
        if self.ent(i).flags & DELETED != 0 {
            let w = self.tk.worker == i;
            self.ent_mut(i).flags = 0;
            self.free_entry(i);
            if w {
                self.worker_idle();
            }
            return Some(job);
        }
        let e = self.ent_mut(i);
        e.flags &= !(RUNNING | INLINE | FROM_WALK | ON_THREAD);
        e.aux = [0, 0];
        e.job = Some(job);
        match self.find(src) {
            Some(s) => self.link(s, i),
            None => self.enqueue(i),
        }
        if self.tk.worker == i {
            self.worker_idle();
        }
        None
    }

    /// Promise `i` has been resolved (the translator stored its value): its
    /// dependents are to be walked on the resolving thread.
    fn resolve_promise(&mut self, i: u32) {
        let early = match self.st_ref().running.last() {
            Some(&r) => self.early_now(r),
            None => false,
        };
        let gen = self.ent(i).gen;
        let canceled = self.ent(i).flags & CANCELED != 0;
        self.ent_mut(i).flags = FINISHED;
        let thread = self.cur_thread();
        self.st().walks.push(Walk {
            owner: i,
            thread,
            early,
            canceled,
            worker: false,
            base: true,
        });
        self.on_finish((i, gen));
    }

    /// The next step of the walk of the dependents of the task that finished
    /// last (`end`): a `sync` dependent is handed to the caller, which runs
    /// it on the finishing thread; the others are enqueued at their
    /// priority. `None` when the walk is over.
    fn walk_next(&mut self) -> Option<u32> {
        loop {
            let w = self.st_ref().walks.last()?;
            let (owner, thread, early, canceled) = (w.owner, w.thread, w.early, w.canceled);
            let d = self.ent(owner).head_dep;
            if d == NONE {
                let w = self.st().walks.pop().unwrap();
                self.free_entry(owner);
                if w.worker {
                    self.worker_idle();
                }
                if w.base {
                    return None;
                }
                continue;
            }
            self.unlink(d);
            let e = self.ent_mut(d);
            if canceled {
                e.flags |= CANCELED;
            }
            if early {
                e.flags |= EARLY;
            }
            if e.flags & SYNC != 0 {
                e.flags |= INLINE | FROM_WALK;
                e.thread = thread;
                return Some(d);
            }
            self.enqueue(d);
        }
    }

    /// Whether a queued pure task is to be started as an IO task is: an IO
    /// task waits for it, directly or through pure tasks (`io_need`), or the
    /// program is shutting down. Otherwise a worker that would start it only
    /// marks it started (`pick`).
    fn eligible(&self, i: u32) -> bool {
        let e = self.ent(i);
        e.flags & PURE == 0 || e.io_need > 0 || self.tk.shutting_down
    }

    /// A worker starts pure task `i` (natively): it can no longer be
    /// deleted, `IO.getTaskState` reports it running, and it runs when it is
    /// needed, polled, at exit, or when nothing else can go on. It does not
    /// keep a worker: here it takes no time.
    fn pick(&mut self, i: u32) {
        self.unqueue(i);
        let g = self.ent(i).gen;
        self.ent_mut(i).flags |= PICKED;
        self.tk.picked.push_back((i, g));
    }

    /// The first valid item of the highest non-empty queue (stale items in
    /// front are discarded).
    fn first_queued(&mut self) -> Option<u32> {
        let t = &mut self.tk;
        for p in (0..PRIOS).rev() {
            if t.queued[p] == 0 {
                continue;
            }
            while let Some(&(i, q)) = t.queues[p].front() {
                let e = &t.slab[i as usize];
                if e.gen != 0 && e.flags & QUEUED != 0 && e.link == q {
                    return Some(i);
                }
                t.queues[p].pop_front();
            }
        }
        None
    }

    pub(crate) fn has_queued(&mut self) -> bool {
        self.first_queued().is_some()
    }

    /// The queued task the scheduler can start now on a new context, if a
    /// worker is free for it (and `gate` lets it): the lone worker's started
    /// one, else the first of the highest non-empty queue. Natively a task
    /// at a priority up to `Task.Priority.max` waits for one of the task
    /// manager's workers (`LEAN_NUM_THREADS`, or one per processor); a
    /// worker waiting for a task (`IO.wait`, `Task.get`) frees its place
    /// meanwhile (`wait_for`); a dedicated task has a thread of its own.
    /// Pure tasks no IO task waits for are picked on the way (`pick`); a
    /// picked one that an IO task has come to wait for comes first.
    pub(crate) fn startable(&mut self, gate: Gate) -> Option<(u32, u32)> {
        if !self.tk.started {
            return None;
        }
        self.settle_worker();
        // A started pure task an IO task waits for: natively it runs on its
        // worker already, and releases the IO task when it finishes.
        while let Some(&(i, g)) = self.tk.picked_io.last() {
            let e = self.ent(i);
            if e.gen == g && e.flags & PICKED != 0 && e.io_need > 0 {
                return Some((i, g));
            }
            self.tk.picked_io.pop();
        }
        loop {
            let w = self.tk.worker;
            let cand = if w != NONE && self.ent(w).flags & QUEUED != 0 {
                w
            } else {
                self.first_queued()?
            };
            let e = self.ent(cand);
            let ok = match gate {
                Gate::Any => true,
                Gate::Before(t) => e.queued_at.is_some_and(|q| q <= t),
                Gate::After(mark) => (e.link.wrapping_sub(mark) as i32) > 0,
            };
            if !ok {
                return None;
            }
            if e.prio as usize != DEDICATED && self.pool_in_use() >= self.cx.pool_limit {
                return None;
            }
            if !self.eligible(cand) {
                self.pick(cand);
                if cand == w {
                    self.tk.worker = NONE;
                }
                continue;
            }
            return Some((cand, self.ent(cand).gen));
        }
    }

    /// When nothing else can ever go on: a pure task a worker has started
    /// (`pick`), to run on a context of its own (natively it runs on its
    /// worker meanwhile).
    pub(crate) fn last_resort(&mut self) -> Option<(u32, u32)> {
        if !self.tk.started {
            return None;
        }
        while let Some(&(i, g)) = self.tk.picked.front() {
            let e = self.ent(i);
            if e.gen == g && e.flags & PICKED != 0 {
                return Some((i, g));
            }
            self.tk.picked.pop_front();
        }
        None
    }

    /// A worker context's first task, handed, if it is still pending and
    /// not running elsewhere.
    pub(crate) fn take_preselect(&mut self) -> Option<u32> {
        let (i, g) = self.cx.cur_ctx().preselect.take()?;
        let e = self.ent(i);
        if e.gen != g || e.flags & (QUEUED | PICKED) == 0 {
            return None;
        }
        self.hand(i);
        Some(i)
    }

    /// Whether a context with this bookkeeping holds one of the task
    /// manager's workers: the innermost task running on it (not on the
    /// thread of whoever ran it) is at a pool priority, and it is not
    /// waiting for a task.
    fn holds_worker(&self, st: &CtxState, w: Wait) -> bool {
        if matches!(w, Wait::Cell(..) | Wait::Progress) {
            return false;
        }
        for &i in st.running.iter().rev() {
            let e = self.ent(i);
            if e.flags & ON_THREAD != 0 {
                continue;
            }
            return (e.prio as usize) < DEDICATED;
        }
        false
    }

    /// The number of the task manager's workers in use: a counter kept by
    /// `refresh_holds` (review RS1S-11), checked against a full count in
    /// debug builds.
    fn pool_in_use(&self) -> u32 {
        debug_assert_eq!(
            self.cx.in_use,
            self.cx
                .ctxs
                .iter()
                .filter(|c| c.status != Status::Dead && self.holds_worker(&c.st, c.wait))
                .count() as u32
        );
        self.cx.in_use
    }

    /// Context `c`'s running tasks, wait or status changed: count again
    /// whether it holds a worker (`holds_worker`).
    pub(crate) fn refresh_holds(&mut self, c: CtxId) {
        let x = &self.cx.ctxs[c];
        let now = x.status != Status::Dead && self.holds_worker(&x.st, x.wait);
        let x = &mut self.cx.ctxs[c];
        if now != x.holds {
            x.holds = now;
            if now {
                self.cx.in_use += 1;
            } else {
                self.cx.in_use -= 1;
            }
        }
    }

    /// What the idle worker picks: the first task of the highest non-empty
    /// queue; pure tasks no IO task waits for are picked on the way.
    fn pick_worker(&mut self) -> u32 {
        loop {
            let Some(i) = self.first_queued() else {
                return NONE;
            };
            if self.eligible(i) {
                return i;
            }
            self.pick(i);
        }
    }

    /// The worker is free: it starts the next queued task, if any.
    fn worker_idle(&mut self) {
        self.tk.wake = None;
        self.tk.worker =
            if self.tk.started && !self.tk.shutting_down && self.pool_in_use() < self.cx.pool_limit
            {
                self.pick_worker()
            } else {
                NONE
            };
    }

    /// The woken worker has picked its task if enough time has passed.
    fn settle_worker(&mut self) {
        if let Some(w) = self.tk.wake {
            let lat = if self.tk.worker_exists {
                LATENCY_WARM
            } else {
                LATENCY_COLD
            };
            if w.elapsed() >= lat {
                self.tk.worker_exists = true;
                self.worker_idle();
            }
        }
    }

    /// Whether tasks are queued, or the worker is waking up, or a started
    /// pure task waits to run for an IO task (`effect`, `poll`).
    pub(crate) fn worker_busy(&self) -> bool {
        let t = &self.tk;
        t.wake.is_some()
            || t.worker != NONE
            || t.queued.iter().any(|&n| n > 0)
            || !t.picked_io.is_empty()
    }

    /// The state of task `id` (lean2rr's `status`): unresolved promises and
    /// started pure tasks are running, as natively.
    fn status_of(&self, id: TaskId) -> TaskState {
        match self.find(id) {
            None => TaskState::Finished,
            Some(i) if self.ent(i).flags & (RUNNING | PROMISE | PICKED) != 0 => TaskState::Running,
            Some(_) => TaskState::Waiting,
        }
    }

    /// For `IO.waitAny`: 2 finished, 1 running (or an unresolved promise), 0
    /// pending and able to run, 3 pending but waiting, directly or through
    /// other pending tasks, for an unresolved promise, a running task (on
    /// another context, or on this one, where waiting hangs; review
    /// RS1S-17), or the walk of a finished task's dependents (running it
    /// would wait; review RS1S-16).
    fn wait_status(&self, i: u32) -> u8 {
        let e = self.ent(i);
        if e.flags & (RUNNING | PROMISE) != 0 {
            return 1;
        }
        let mut c = i;
        for _ in 0..self.tk.slab.len() {
            if self.ent(c).flags & WAITING == 0 {
                break;
            }
            c = self.ent(c).link;
            // A source running on this context counts too: `wait` on the
            // dependent would hang there (review RS1S-17).
            if self.ent(c).flags & (PROMISE | RUNNING | FINISHED) != 0 {
                return 3;
            }
        }
        0
    }

    /// `IO.getTaskState` (lean2rr's `query`). A pending task is reported
    /// waiting until the program asks again after some time has passed (a
    /// sleep since the first answer; two for a pure task, `Pure tasks` in
    /// docs/sched.md) or keeps asking (`POLL_QUERIES` answers): it is then
    /// polling for the task, which a worker would have run meanwhile, so it
    /// runs and is reported finished. A task that cannot finish without
    /// others (an unresolved promise, a task waiting for one or for a
    /// running task) does not run: the others go on once
    /// (`poll`), and its state is answered then; so is a task running on
    /// another context, at every question.
    fn query(&mut self, id: TaskId) -> Query {
        let Some(i) = self.find(id) else {
            return Query::Answer(TaskState::Finished);
        };
        let flags = self.ent(i).flags;
        if flags & RUNNING != 0 {
            if self.st_ref().running.contains(&i) {
                return Query::Answer(TaskState::Running);
            }
            return Query::YieldThenStatus;
        }
        let idle = if flags & (PROMISE | PICKED) != 0 {
            TaskState::Running
        } else {
            TaskState::Waiting
        };
        let epoch = self.tk.epoch;
        let need = if flags & PURE != 0 && !self.tk.shutting_down {
            2
        } else {
            1
        };
        let [observed, queries] = self.ent(i).aux;
        if observed == 0 {
            self.ent_mut(i).aux = [epoch.wrapping_add(1), 1];
            return Query::Answer(idle);
        }
        let sleeps = epoch.wrapping_add(1).wrapping_sub(observed);
        if sleeps >= need || queries >= POLL_QUERIES {
            // The next question starts over (a promise may stay unresolved).
            self.ent_mut(i).aux = [0, 0];
            if flags & PROMISE != 0 || self.wait_status(i) == 3 {
                return Query::YieldThenStatus;
            }
            return Query::Run;
        }
        self.ent_mut(i).aux[1] += 1;
        Query::Answer(idle)
    }

    /// The next step of `wait` (lean2rr's `l2r_task_get_S`,
    /// `source_next` and `wait_running`): a pending task runs here; one
    /// that waits for pending tasks first runs that chain from its deepest
    /// end, one task after the other (`chain`, kept across steps, so that a
    /// long chain neither recurses nor is searched once per task); a task
    /// running on another context, or a promise, is waited for.
    fn wait_step(&mut self, id: TaskId, chain: &mut Option<Vec<(u32, u32)>>) -> WaitStep {
        let Some(i) = self.find(id) else {
            return WaitStep::Done;
        };
        let flags = self.ent(i).flags;
        if flags & RUNNING != 0 {
            // On the running context it needs itself: natively a wait
            // forever.
            if self.st_ref().running.contains(&i) {
                return WaitStep::Hang;
            }
            return WaitStep::Block(Wait::Cell(i, id.gen()));
        }
        if flags & PROMISE != 0 {
            return WaitStep::Block(Wait::Cell(i, id.gen()));
        }
        if flags & WAITING == 0 {
            *chain = None;
            self.hand(i);
            return WaitStep::Run(i);
        }
        if chain.is_none() {
            // The pending tasks `i` waits for, transitively (bounded: a bind
            // task waiting for a task that depends on it is a cycle), up to
            // one that is running, or has finished while its walk has not
            // reached the next yet (review RS1S-16).
            let mut v = Vec::new();
            let mut c = i;
            for _ in 0..=self.tk.slab.len() {
                let s = self.ent(c).link;
                if self.ent(c).flags & WAITING == 0
                    || s == NONE
                    || s == i
                    || self.ent(s).flags & (RUNNING | FINISHED) != 0
                {
                    break;
                }
                v.push((s, self.ent(s).gen));
                if self.ent(s).flags & PROMISE != 0 {
                    break;
                }
                c = s;
            }
            if v.is_empty() {
                // It waits for a running task, or for the walk of a finished
                // one (`source_wait`), or for itself.
                return self.source_wait(i).unwrap_or(WaitStep::Hang);
            }
            *chain = Some(v);
        }
        let v = chain.as_mut().unwrap();
        while let Some(&(d, g)) = v.last() {
            let e = self.ent(d);
            if e.gen != g || e.flags & (RUNNING | FINISHED) != 0 {
                // Finished or started meanwhile.
                v.pop();
                continue;
            }
            if e.flags & PROMISE != 0 {
                return WaitStep::Block(Wait::Cell(d, g));
            }
            if let Some(w) = self.source_wait(d) {
                return w;
            }
            v.pop();
            self.hand(d);
            return WaitStep::Run(d);
        }
        *chain = None;
        WaitStep::Again
    }

    /// What pending task `x` waits for, if its source has started: natively
    /// a dependent is run or queued by the walk of its source's dependents,
    /// once the source has finished (`handle_finished`).
    /// - The source runs on another context: wait until it has finished,
    ///   then look again.
    /// - Its walk has not reached `x` yet, on another context (it runs a
    ///   newer `sync` dependent there): wait until something is queued or
    ///   finishes, then look again (review RS1S-16).
    /// - Either of them on this context: `x` can never run before this
    ///   context goes on, a deadlock natively.
    fn source_wait(&self, x: u32) -> Option<WaitStep> {
        let e = self.ent(x);
        if e.flags & WAITING == 0 || e.link == NONE {
            return None;
        }
        let s = e.link;
        let se = self.ent(s);
        let st = self.st_ref();
        if se.flags & RUNNING != 0 {
            if st.running.contains(&s) {
                return Some(WaitStep::Hang);
            }
            return Some(WaitStep::Block(Wait::Cell(s, se.gen)));
        }
        if se.flags & FINISHED != 0 {
            if st.walks.iter().any(|w| w.owner == s) {
                return Some(WaitStep::Hang);
            }
            return Some(WaitStep::Block(Wait::Progress));
        }
        None
    }

    /// Lean's `deactivate_task`: the program has dropped its last reference
    /// to task `id`. A pure task that has not started is deleted and never
    /// runs (its job is returned, to be dropped by the caller outside the
    /// scheduler's state: dropping it releases what it holds, its source
    /// task included, which may be deleted in turn). An IO task is kept
    /// alive until it has run (`keep_alive`); a started one runs to
    /// completion and its value is discarded.
    fn deactivate(&mut self, id: TaskId) -> Option<Job> {
        let i = self.find(id)?;
        let flags = self.ent(i).flags;
        if flags & PROMISE != 0 || flags & PURE == 0 {
            return None;
        }
        if flags & (RUNNING | PICKED) != 0 || self.tk.worker == i || self.ent(i).head_dep != NONE {
            // Natively it has started (or tasks still hold it, which the
            // translator's references should rule out).
            self.ent_mut(i).flags |= DELETED | CANCELED;
            return None;
        }
        self.unqueue(i);
        self.unlink(i);
        let job = self.ent_mut(i).job.take();
        self.free_entry(i);
        job
    }

    /// `main` has returned; the remaining tasks are about to run. The tasks
    /// queued or started now could have been started by native workers
    /// before Lean's shutdown flag was set (`EARLY`).
    fn shutdown(&mut self) {
        self.settle_worker();
        self.tk.shutting_down = true;
        let t = &mut self.tk;
        for p in 0..PRIOS {
            for k in 0..t.queues[p].len() {
                let (i, q) = t.queues[p][k];
                let e = &mut t.slab[i as usize];
                if e.gen != 0 && e.flags & QUEUED != 0 && e.link == q {
                    e.flags |= EARLY;
                }
            }
        }
        for k in 0..t.picked.len() {
            let (i, g) = t.picked[k];
            let e = &mut t.slab[i as usize];
            if e.gen == g && e.flags & PICKED != 0 {
                e.flags |= EARLY;
            }
        }
    }

    /// The final run's next step, on `main`'s context (lean2rr's `next_tag`
    /// at shutdown): the started pure tasks first, then queued tasks as a
    /// free worker would start them; `main` waits while tasks run on other
    /// contexts, and runs whatever they queue meanwhile. A task enqueued
    /// after `main` returned always runs, unlike natively, where one queued
    /// once no standard worker is left never does (LB-13). When only tasks
    /// waiting for others remain (an unresolved promise, a cycle), it is
    /// done: Lean's workers stop when the queue is empty and leave such tasks
    /// behind.
    fn final_next(&mut self) -> Final {
        if let Some((i, _)) = self.last_resort() {
            self.tk.picked.pop_front();
            self.hand(i);
            return Final::Run(i);
        }
        if let Some((i, _)) = self.startable(Gate::Any) {
            self.hand(i);
            return Final::Run(i);
        }
        if !self.has_queued() && self.cx.workers == 0 {
            return Final::Done;
        }
        Final::Wait
    }

    fn check_canceled_now(&mut self) -> bool {
        let Some(&i) = self.st_ref().running.last() else {
            return false;
        };
        let late = if self.tk.shutting_down {
            let late = !self.early_now(i);
            self.ent_mut(i).flags |= CHECKED;
            late
        } else {
            false
        };
        late || self.ent(i).flags & CANCELED != 0
    }
}

// ---------------------------------------------------------------------------
// Running tasks

/// Run handed task `i` on the running context: begin, its job, then its end
/// and the walk of its dependents (or, for a bind task, its wait for the task
/// it continues as).
pub(crate) fn run_task(i: u32) {
    let (job, own) = with(|s| s.begin(i));
    let g = glue_opt();
    if let Some(g) = &g {
        g.task_begin(own);
    }
    let mut guard = Unwound {
        i,
        own,
        g: &g,
        ran: false,
    };
    let out = job();
    guard.ran = true;
    let leftover = match out {
        Outcome::Done => {
            if with(|s| s.end(i)) {
                walk_loop();
            }
            None
        }
        Outcome::Continue(t2, k) => with(|s| s.bind_wait(i, t2, k)),
    };
    std::mem::forget(guard);
    if let Some(g) = &g {
        g.task_end(own);
    }
    drop(leftover);
}

/// A Rust panic unwinds `run_task` (review RS1S-12 of sched-1). Out of the
/// job (`ran` false), the task stays unfinished, as the tasks of a dead
/// context do: its context drops it from its running tasks
/// (`abandon_running`). Either way the glue hears of the task's end.
struct Unwound<'a> {
    i: u32,
    own: bool,
    g: &'a Option<Rc<dyn Glue>>,
    ran: bool,
}

impl Drop for Unwound<'_> {
    fn drop(&mut self) {
        if !self.ran && super::alive() {
            with(|s| s.abandon_running(self.i));
        }
        if let Some(g) = self.g {
            g.task_end(self.own);
        }
    }
}

/// The loop of a base walk: its `sync` dependents run here, one after the
/// other, and their own walks are continued by this loop, so a long chain of
/// `sync` dependents does not recurse.
fn walk_loop() {
    // The base walk (`end`, `resolve_promise`) is the top one: on a Rust
    // panic, it and the walks above it end (`WalkUnwound`).
    let keep = with(|s| s.st_ref().walks.len()).saturating_sub(1);
    let guard = WalkUnwound(keep);
    while let Some(d) = with(|s| s.walk_next()) {
        run_task(d);
    }
    std::mem::forget(guard);
}

/// A Rust panic unwinds `walk_loop` (review RS1S-12 of sched-1): the walks
/// above `.0` end (`abandon_walks`).
struct WalkUnwound(usize);

impl Drop for WalkUnwound {
    fn drop(&mut self) {
        if super::alive() {
            with(|s| s.abandon_walks(self.0));
        }
    }
}

// ---------------------------------------------------------------------------
// The translators' API

/// `lean_task_spawn_core(c, prio, keep_alive)`: `Task.spawn` (`keep_alive`
/// false) and `IO.asTask` (true). Without a task manager (during module
/// initialization, or `LEAN_NUM_THREADS=0`) the job runs at once and the
/// task is finished; at priority `LEAN_SYNC_PRIO` it runs at once as a task
/// on the current thread; otherwise it is deferred.
pub fn spawn(job: Job, prio: u64, keep_alive: bool) -> TaskId {
    if !with(|s| s.tk.started) {
        let _ = job();
        return TaskId::FINISHED;
    }
    let (i, now, id) = with(|s| {
        let (i, now) = s.register(job, prio, keep_alive, false);
        (i, now, s.id_of(i))
    });
    if now {
        run_task(i);
    }
    id
}

/// Whether a dependent of `src` is not a task at all: without a task
/// manager, or with `sync := true` on a finished task, Lean applies the
/// function at once in the calling thread (`lean_task_map_core`,
/// `lean_task_bind_core`). The translator then does that itself instead of
/// calling `depend`.
pub fn dependent_runs_now(src: TaskId, sync: bool) -> bool {
    with(|s| !s.tk.started || (sync && s.find(src).is_none()))
}

/// `Task.map`/`Task.bind` (`keep_alive` false), `IO.mapTask`/`IO.bindTask`
/// (true): a new task running `job` once `src` has finished (it reads
/// `src`'s value), as Lean's `add_dep`; when `src` finishes, a `sync`
/// dependent runs there and then on the finishing thread, the others are
/// queued. Requires `!dependent_runs_now(src, sync)`.
pub fn depend(src: TaskId, job: Job, prio: u64, sync: bool, keep_alive: bool) -> TaskId {
    let (i, now, id) = with(|s| {
        let (i, _) = s.register(job, prio, keep_alive, true);
        let now = s.depend(src, i, sync);
        (i, now, s.id_of(i))
    });
    if now {
        run_task(i);
    }
    id
}

/// `Task.get`/`IO.wait` (`lean_task_get`): returns once task `id` has
/// finished. A pending task runs here, on the stack of whoever needs it (as
/// natively a worker runs it while the caller waits); one running on another
/// context, or an unresolved promise, is waited for while other contexts
/// run; a task needed by its own computation waits forever, as natively.
pub fn wait(id: TaskId) {
    let mut chain = None;
    loop {
        match with(|s| s.wait_step(id, &mut chain)) {
            WaitStep::Done => return,
            WaitStep::Run(i) => run_task(i),
            WaitStep::Block(w) => block(w),
            WaitStep::Hang => super::hang(),
            WaitStep::Again => {}
        }
    }
}

/// Whether task `id` has finished (no polling semantics: see `state`).
pub fn is_finished(id: TaskId) -> bool {
    with(|s| s.find(id).is_none())
}

/// `IO.getTaskState` (`lean_io_get_task_state_core`), as a polling program
/// sees it (`query`); `IO.hasFinished` is `state(id) == Finished`. A polling
/// point: other contexts may run first.
pub fn state(id: TaskId) -> TaskState {
    match with(|s| s.query(id)) {
        Query::Answer(a) => a,
        Query::Run => {
            wait(id);
            TaskState::Finished
        }
        Query::YieldThenStatus => {
            poll();
            with(|s| s.status_of(id))
        }
    }
}

/// `IO.waitAny` (`lean_io_wait_any_core`): the index of the first finished
/// task of `ids`. If none has finished, the first pending one that can run
/// (not waiting for an unresolved promise or a task on another context)
/// runs: it finished first. If none can, wait until some task finishes, and
/// look again.
pub fn wait_any(ids: &[TaskId]) -> usize {
    assert!(!ids.is_empty(), "lean-runtime: IO.waitAny of an empty list");
    loop {
        let pick = with(|s| {
            if let Some(k) = ids.iter().position(|&id| s.find(id).is_none()) {
                return Ok(k);
            }
            for (k, &id) in ids.iter().enumerate() {
                if let Some(i) = s.find(id) {
                    if s.wait_status(i) == 0 {
                        return Err(Some(k));
                    }
                }
            }
            Err(None)
        });
        match pick {
            Ok(k) => return k,
            Err(Some(k)) => {
                wait(ids[k]);
                return k;
            }
            Err(None) => block(Wait::Progress),
        }
    }
}

/// `IO.cancel` (`lean_io_cancel_core`): set the cancellation flag of an
/// unfinished task. When a canceled task finishes, its dependents created
/// while it was unfinished are canceled too (`handle_finished`).
pub fn cancel(id: TaskId) {
    with(|s| {
        if let Some(i) = s.find(id) {
            s.ent_mut(i).flags |= CANCELED;
        }
    })
}

/// `IO.checkCanceled` (`lean_io_check_canceled_core`): inside a task,
/// whether it was canceled or the program is shutting down; false in `main`.
/// At shutdown, a task that could have started before Lean's flag was set
/// (queued or started when `main` returned, or created or released by such a
/// task still in its first moments) sees it once time has passed in it (a
/// sleep) or from its second check on; any other task sees it at once. A
/// polling point.
pub fn check_canceled() -> bool {
    poll();
    with(|s| s.check_canceled_now())
}

/// Lean's `deactivate_task`: the translator's last reference to task `id`
/// is gone. A pure task that has not started is deleted and never runs; any
/// other task runs to completion (`deactivate`). Call it with no borrow of
/// the translator's own state that the task's job may need: the job is
/// dropped here.
pub fn release(id: TaskId) {
    if !super::alive() {
        return;
    }
    let job = with(|s| s.deactivate(id));
    drop(job);
}

/// Whether the innermost task running on this thread is a `sync` one: a
/// `sync := true` dependent, or a task at priority `LEAN_SYNC_PRIO` (native
/// Lean gives both that priority). Native `Task.get` (and `IO.wait`) of an
/// unfinished task from such a task prints `GET_IN_SYNC_TASK` as a Lean
/// panic (which goes on, unless `LEAN_ABORT_ON_PANIC`) before it waits
/// (`task_manager::wait_for`). The glue reproduces it: when its own slot for
/// the task is still empty and this is true, it reports that Lean panic,
/// then calls `wait`.
pub fn in_sync_task() -> bool {
    with(|s| {
        s.st_ref()
            .running
            .last()
            .is_some_and(|&i| s.ent(i).flags & SYNC != 0)
    })
}

/// `IO.getTID` inside tasks: natively a task runs on a worker thread, a task
/// needed by a running task on another one, a `sync` dependent on the thread
/// that finished its source. The number to add to `main`'s thread id (0 in
/// `main`).
pub fn thread_number() -> u64 {
    with(|s| s.cur_thread())
}

// ---------------------------------------------------------------------------
// Promises

/// `IO.Promise.new` (`lean_promise_new`): a new unresolved promise's task.
/// Before the task manager runs, Lean's internal panic
/// (`PROMISE_BEFORE_MANAGER`), which the glue reports.
pub fn promise_new() -> Result<TaskId, &'static str> {
    with(|s| {
        if !s.tk.started {
            return Err(PROMISE_BEFORE_MANAGER);
        }
        let i = s.alloc(None, PROMISE, 0);
        Ok(s.id_of(i))
    })
}

/// `IO.Promise.resolve` (`task_manager::resolve`), and the resolution with
/// `none` when the last reference to an unresolved promise goes
/// (`deactivate_promise`): if the promise is unresolved, `store` stores its
/// value in the translator's slot, then its dependents are walked on the
/// resolving thread (its `sync` dependents run here) and its waiters wake.
/// Only the first resolution counts: false (and `store` not called) if it
/// was resolved already.
pub fn resolve(id: TaskId, store: impl FnOnce()) -> bool {
    if !super::alive() {
        return false;
    }
    let unresolved = with(|s| s.find(id).is_some_and(|i| s.ent(i).flags & PROMISE != 0));
    if !unresolved {
        return false;
    }
    store();
    let ok = with(|s| match s.find(id) {
        Some(i) => {
            s.resolve_promise(i);
            true
        }
        None => false,
    });
    if ok {
        walk_loop();
    }
    ok
}

// ---------------------------------------------------------------------------
// Lifecycle

/// The final run (`lean_finalize_task_manager`, after `main` returns,
/// whatever it returned): Lean sets its shutdown flag, its workers finish the
/// queued tasks, and it joins them. Here the started pure tasks, the pending
/// IO tasks and the pure tasks still referenced run, in the order Lean's
/// task manager would start them, and the call returns once no task is
/// queued or running and no context but `main`'s is left: tasks enqueued
/// meanwhile run too, also a pool task enqueued once no native standard
/// worker would be left, which native Lean never runs (LB-13 in
/// docs/lean-bugs.md). Dedicated tasks run to completion. A task whose
/// dependency never finishes (an unresolved promise) is not waited for.
/// Dropped pure tasks were deleted (`release`) and never run. Only then does the glue flush the standard streams and exit
/// (decisions Q5 refinement A): a runaway task keeps the process alive, and
/// its buffered output is never flushed, as natively.
pub fn finish() {
    if !with(|s| s.tk.started) {
        return;
    }
    with(|s| s.shutdown());
    loop {
        match with(|s| s.final_next()) {
            Final::Run(i) => run_task(i),
            Final::Wait => block(Wait::FinalRun),
            Final::Done => return,
        }
    }
}

// ---------------------------------------------------------------------------
// Yield points

/// A polling point (lean2rr's `poll_yield`): task-state queries,
/// `IO.checkCanceled`, clock reads, and `ST.Ref` reads in programs with
/// tasks (`ref_read`). Natively other threads go on while the program polls,
/// so they do now: due sleepers, the contexts able to run, a queued task if a
/// worker is free (on a context of its own).
pub fn poll() {
    let go = with(|s| {
        if !s.tk.started
            || (s.cx.sleepers.is_empty() && s.cx.runnable.is_empty() && !s.worker_busy())
        {
            return false;
        }
        s.promote_sleepers(Instant::now());
        if s.cx.runnable.is_empty() {
            if let Some((e, g)) = s.startable(Gate::Any) {
                s.start_worker(e, g);
            }
        }
        !s.cx.runnable.is_empty()
    });
    if go {
        yield_now();
    }
}

/// An observable effect (output, flushing a handle, spawning a process,
/// `IO.Process.exit`) of the running context (lean2rr's `effect`): what
/// natively would have run by now on other threads goes first: a context
/// whose sleep has ended, a context able to run for a while (a lock handed
/// over, a promise resolved: `STALE`), a task a worker would have started a
/// while ago; then, round after round, what those release. What runs in
/// those rounds happened before natively: its own effect points start no
/// tasks, and let go first only what is due or able to run for a while.
pub fn effect() {
    let slow = with(|s| {
        s.tk.started && (!s.cx.sleepers.is_empty() || !s.cx.runnable.is_empty() || s.worker_busy())
    });
    if slow {
        effect_slow();
    }
}

fn effect_slow() {
    let (nested, mark) = with(|s| {
        let nested = s.cx.in_effect;
        s.cx.in_effect = true;
        (nested, s.tk.next_q)
    });
    for round in 0..64 {
        let go = with(|s| {
            let now = Instant::now();
            let mut go = false;
            if s.sleeper_due(now) {
                s.promote_sleepers(now);
                go = true;
            }
            if s.cx.runnable.iter().any(|&c| {
                let x = &s.cx.ctxs[c];
                (round > 0 && !x.at_effect) || now.saturating_duration_since(x.ready) >= STALE
            }) {
                go = true;
            }
            if !nested && !s.tk.shutting_down {
                let mut w = now
                    .checked_sub(STALE)
                    .and_then(|t| s.startable(Gate::Before(t)));
                if w.is_none() && round > 0 {
                    w = s.startable(Gate::After(mark));
                }
                if let Some((e, g)) = w {
                    s.start_worker(e, g);
                    go = true;
                }
            }
            if !go || s.cx.runnable.is_empty() {
                return false;
            }
            s.cx.cur_ctx().at_effect = true;
            true
        });
        if !go {
            break;
        }
        yield_now();
        with(|s| s.cx.cur_ctx().at_effect = false);
    }
    if !nested {
        with(|s| s.cx.in_effect = false);
    }
}

/// `IO.sleep ms` and `dbgSleep` (`ms` as Lean passes it). Other contexts and
/// queued tasks run meanwhile, as other threads would.
pub fn sleep_ms(ms: u32) {
    with(|s| {
        s.settle_worker();
        s.tk.epoch = s.tk.epoch.wrapping_add(1);
    });
    if ms == 0 {
        zero_sleep();
    } else {
        sleep_for(Duration::from_millis(ms as u64));
    }
    with(|s| s.settle_worker());
}

/// Sleep for `d`: the running context blocks until then, and others run
/// meanwhile; without anything else to do, a plain sleep.
fn sleep_for(d: Duration) {
    let alone = with(|s| {
        !s.tk.started || (s.cx.live() == 1 && !s.has_queued() && s.tk.picked_io.is_empty())
    });
    if alone {
        std::thread::sleep(d);
        return;
    }
    block(Wait::Sleep(Instant::now() + d));
}

/// `IO.sleep 0`: no time passes, but what natively runs meanwhile does: the
/// contexts whose sleep has ended, the contexts able to run, a queued task
/// a worker would have started by now.
fn zero_sleep() {
    let go = with(|s| {
        if !s.tk.started {
            return false;
        }
        let now = Instant::now();
        s.promote_sleepers(now);
        if !s.tk.shutting_down {
            if let Some((e, g)) = now
                .checked_sub(WORKER_LATENCY)
                .and_then(|t| s.startable(Gate::Before(t)))
            {
                s.start_worker(e, g);
            }
        }
        !s.cx.runnable.is_empty()
    });
    if go {
        yield_now();
    }
}

/// The context's number (for `Std.Sync`'s lock owners and the glue's
/// per-context state).
pub fn current_context() -> CtxId {
    with(|s| s.cx.cur)
}

/// Whether the task manager runs (`g_task_manager`).
pub fn manager_running() -> bool {
    with(|s| s.tk.started)
}

#[cfg(test)]
mod tests {
    use super::priority;

    #[test]
    fn priorities() {
        assert_eq!(priority(0), (0, false));
        assert_eq!(priority(8), (8, false));
        assert_eq!(priority(9), (9, false));
        assert_eq!(priority(1000), (9, false));
        assert_eq!(priority(4294967295), (0, true));
        assert_eq!(priority(4294967297), (1, false));
        assert_eq!(priority(8589934596), (4, false));
    }
}
