//! A translator's values over `lean_runtime::sched`, as small as the cases
//! need: tasks (the value in a slot the task's job fills, the handle's last
//! reference releasing the task), promises, and `IO.Ref`.

use lean_runtime::sched::{self, CtxId, Job, Outcome, TaskId, TaskState};
use std::cell::{OnceCell, RefCell};
use std::rc::Rc;

struct TaskObj<T> {
    id: TaskId,
    slot: Rc<OnceCell<T>>,
}

impl<T> TaskObj<T> {
    /// The id to pass to the scheduler: `FINISHED` once the slot holds the
    /// value. A finished task's entry is reused by later tasks, and after
    /// 2^32 of them its generation too, so the glue never passes the id of a
    /// task its own slot says has finished (docs/sched.md, "The glue").
    fn live(&self) -> TaskId {
        if self.slot.get().is_some() {
            TaskId::FINISHED
        } else {
            self.id
        }
    }
}

impl<T> Drop for TaskObj<T> {
    fn drop(&mut self) {
        // Lean's `deactivate_task`: the last reference is gone. Nothing to do
        // for a finished task (its slot holds the value).
        if self.slot.get().is_none() {
            sched::release(self.id);
        }
    }
}

/// `Task α`: one counted object, as Lean's.
pub struct Task<T>(Rc<TaskObj<T>>);

impl<T> Clone for Task<T> {
    fn clone(&self) -> Self {
        Task(self.0.clone())
    }
}

/// `Task.Priority.default`, `Task.Priority.dedicated`.
pub const PRIO_DEFAULT: u64 = 0;
pub const PRIO_DEDICATED: u64 = 9;

fn job_filling<T: 'static>(slot: &Rc<OnceCell<T>>, f: impl FnOnce() -> T + 'static) -> Job {
    let slot = slot.clone();
    Box::new(move || {
        let v = f();
        // the job's hand-offs end before its value is seen (docs/sched.md,
        // item 3 of "The glue")
        sched::before_task_value();
        let _ = slot.set(v);
        Outcome::Done
    })
}

impl<T: Clone + 'static> Task<T> {
    fn with_slot(make: impl FnOnce(&Rc<OnceCell<T>>) -> TaskId) -> Task<T> {
        let slot = Rc::new(OnceCell::new());
        let id = make(&slot);
        Task(Rc::new(TaskObj { id, slot }))
    }

    /// `Task.pure`.
    pub fn pure(v: T) -> Task<T> {
        Task(Rc::new(TaskObj {
            id: TaskId::FINISHED,
            slot: Rc::new(OnceCell::from(v)),
        }))
    }

    /// `Task.spawn fn prio`.
    pub fn spawn(f: impl FnOnce() -> T + 'static, prio: u64) -> Task<T> {
        Task::with_slot(|slot| sched::spawn(job_filling(slot, f), prio, false))
    }

    /// `Task.get` / `IO.wait` (`lean_task_get`): the value if the slot
    /// holds it; otherwise, from a `sync` task, Lean's panic message first
    /// (`task_manager::wait_for`), then the wait.
    pub fn get(&self) -> T {
        if let Some(v) = self.0.slot.get() {
            return v.clone();
        }
        if sched::in_sync_task() {
            crate::glue::lean_panic(sched::GET_IN_SYNC_TASK);
        }
        sched::wait(self.0.id);
        self.0
            .slot
            .get()
            .expect("a finished task has its value")
            .clone()
    }

    /// `IO.getTaskState`.
    pub fn state(&self) -> TaskState {
        match self.0.live() {
            TaskId::FINISHED => TaskState::Finished,
            id => sched::state(id),
        }
    }
}

/// `IO.hasFinished`.
pub fn has_finished<T: Clone + 'static>(t: &Task<T>) -> bool {
    t.state() == TaskState::Finished
}

/// `toString` of an `IO.TaskState`.
pub fn task_state_str(s: TaskState) -> &'static str {
    match s {
        TaskState::Waiting => "waiting",
        TaskState::Running => "running",
        TaskState::Finished => "finished",
    }
}

/// `BaseIO.asTask act prio`.
pub fn as_task<T: Clone + 'static>(act: impl FnOnce() -> T + 'static, prio: u64) -> Task<T> {
    Task::with_slot(|slot| sched::spawn(job_filling(slot, act), prio, true))
}

/// `BaseIO.mapTask f t prio sync` (`keep_alive`), or `Task.map` (not).
pub fn map_task<A: Clone + 'static, B: Clone + 'static>(
    f: impl FnOnce(A) -> B + 'static,
    t: Task<A>,
    prio: u64,
    sync: bool,
    keep_alive: bool,
) -> Task<B> {
    let src = t.0.live();
    if sched::dependent_runs_now(src, sync) {
        return Task::pure(f(t.get()));
    }
    // The job holds its source, as Lean's `task_map_fn` closure does.
    Task::with_slot(|slot| {
        sched::depend(
            src,
            job_filling(slot, move || f(t.get())),
            prio,
            sync,
            keep_alive,
        )
    })
}

/// `BaseIO.bindTask t f prio sync` (`keep_alive`), or `Task.bind` (not):
/// the bind task finishes as the task `f` returns (`task_bind_fn1`).
pub fn bind_task<A: Clone + 'static, B: Clone + 'static>(
    t: Task<A>,
    f: impl FnOnce(A) -> Task<B> + 'static,
    prio: u64,
    sync: bool,
    keep_alive: bool,
) -> Task<B> {
    let src = t.0.live();
    if sched::dependent_runs_now(src, sync) {
        return f(t.get());
    }
    Task::with_slot(|slot| {
        let slot = slot.clone();
        let job: Job = Box::new(move || {
            let t2 = f(t.get());
            if t2.0.live() == TaskId::FINISHED || sched::is_finished(t2.0.id) {
                sched::before_task_value();
                let _ = slot.set(t2.get());
                return Outcome::Done;
            }
            let id2 = t2.0.id;
            Outcome::Continue(
                id2,
                Box::new(move || {
                    let v = t2.get();
                    sched::before_task_value();
                    let _ = slot.set(v);
                    Outcome::Done
                }),
            )
        });
        sched::depend(src, job, prio, sync, keep_alive)
    })
}

/// `IO.waitAny`.
pub fn wait_any<T: Clone + 'static>(ts: &[Task<T>]) -> T {
    let ids: Vec<TaskId> = ts.iter().map(|t| t.0.live()).collect();
    let k = sched::wait_any(&ids);
    ts[k].get()
}

/// `IO.cancel`.
#[allow(dead_code)]
pub fn cancel<T>(t: &Task<T>) {
    sched::cancel(t.0.live())
}

/// `IO.Promise α`: its task's slot holds `Option α` (`none` when the promise
/// is dropped unresolved).
pub struct Promise<T: Clone + 'static> {
    result: Task<Option<T>>,
}

impl<T: Clone + 'static> Promise<T> {
    /// `IO.Promise.new`.
    pub fn new() -> Promise<T> {
        let id = match sched::promise_new() {
            Ok(id) => id,
            Err(msg) => {
                eprintln!("INTERNAL PANIC: {msg}");
                std::process::exit(1)
            }
        };
        Promise {
            result: Task(Rc::new(TaskObj {
                id,
                slot: Rc::new(OnceCell::new()),
            })),
        }
    }

    /// `IO.Promise.resolve`.
    pub fn resolve(&self, v: T) {
        let slot = self.result.0.slot.clone();
        sched::resolve(self.result.0.live(), move || {
            let _ = slot.set(Some(v));
        });
    }

    /// `IO.Promise.result?` (`lean_io_promise_result_opt`): another
    /// reference to the promise's own task, the same at every call.
    pub fn result_opt(&self) -> Task<Option<T>> {
        self.result.clone()
    }

    /// `IO.Promise.result!`: `result?.map (sync := true)
    /// Option.getOrBlock!`, whose function is `sched::option_get_or_block`
    /// with the glue's forced panic report. On a resolved promise it runs at
    /// once (`Task.pure` of the value); otherwise when the promise is
    /// resolved or dropped, on that thread.
    pub fn result_bang(&self) -> Task<T> {
        map_task(
            |o: Option<T>| sched::option_get_or_block(o, crate::glue::lean_panic_forced),
            self.result_opt(),
            PRIO_DEFAULT,
            true,
            false,
        )
    }

    /// `promise_is_resolved`: the slot holds a value.
    pub fn is_resolved(&self) -> bool {
        self.result.0.slot.get().is_some()
    }
}

/// `IO.Promise α` as a counted Lean object, as the event loop's timers and
/// signals hold it (`lean_runtime::sched::uv::LoopPromise`): a clone is
/// `lean_inc`, a drop `lean_dec`, and the last one resolves it with `none`.
pub struct UvPromise<T: Clone + 'static>(Rc<Promise<T>>);

impl<T: Clone + 'static> Clone for UvPromise<T> {
    fn clone(&self) -> Self {
        UvPromise(self.0.clone())
    }
}

impl<T: Clone + 'static> UvPromise<T> {
    /// `lean_io_promise_new`.
    pub fn new() -> UvPromise<T> {
        UvPromise(Rc::new(Promise::new()))
    }

    /// `IO.Promise.result?`.
    pub fn result_opt(&self) -> Task<Option<T>> {
        self.0.result_opt()
    }

    /// `IO.Promise.result!`.
    pub fn result_bang(&self) -> Task<T> {
        self.0.result_bang()
    }

    /// `IO.Promise.resolve`.
    pub fn resolve(&self, v: T) {
        self.0.resolve(v)
    }
}

impl lean_runtime::sched::uv::LoopPromise for UvPromise<()> {
    fn is_resolved(&self) -> bool {
        self.0.is_resolved()
    }
    fn resolve(&self, _: i64) {
        self.0.resolve(())
    }
}

impl lean_runtime::sched::uv::LoopPromise for UvPromise<i64> {
    fn is_resolved(&self) -> bool {
        self.0.is_resolved()
    }
    fn resolve(&self, v: i64) {
        self.0.resolve(v)
    }
}

impl<T: Clone + 'static> Drop for Promise<T> {
    fn drop(&mut self) {
        // Lean's `deactivate_promise`: resolved with `none`.
        let slot = self.result.0.slot.clone();
        sched::resolve(self.result.0.live(), move || {
            let _ = slot.set(None);
        });
    }
}

/// `IO.Ref α`: reads are polling points in programs with tasks
/// (`sched::ref_read`).
///
/// The semantics are Lean 4.35's (LB-01, LB-18 in docs/lean-bugs.md):
/// - `modify` is `take`, then a store into the emptied cell (`put`,
///   `ST.Prim.Ref.modifyUnsafe`), so the cell is empty while `modify`'s
///   function runs. That function may block (`Task.get`), and other contexts
///   then run.
/// - Only `modify`'s own store fills the empty cell. Until then `get`,
///   `take`, `set` and `swap` wait: each blocks the context (a yield point)
///   until the store wakes it. `set` is `swap` with the result dropped.
/// - So `modify` and `swap` are atomic. The cost, as in 4.35: a `modify`
///   whose function waits for a task that uses the same reference deadlocks.
///
/// Native 4.34.0 differs where its reference is shared with a task
/// (multi-threaded): `get` and `take` spin while the slot is empty, as here
/// (io.cpp 1459-1500, case `refs/get_during_modify`), but `set` stores into
/// the empty slot, and `modify`'s store then overwrites it (LB-01,
/// `refs/set_during_modify`), and `swap` returns its own argument (LB-18,
/// `refs/swap_during_modify`) (docs/sched.md, The glue, item 7;
/// docs/threads.md, 3.1).
pub struct Ref<T>(Rc<RefObj<T>>);

struct RefObj<T> {
    /// `None` while `modify` holds it.
    val: RefCell<Option<T>>,
    /// The contexts waiting for `modify`'s store.
    waiters: RefCell<Vec<CtxId>>,
}

impl<T> Clone for Ref<T> {
    fn clone(&self) -> Self {
        Ref(self.0.clone())
    }
}

impl<T: Clone> Ref<T> {
    /// `IO.mkRef`.
    pub fn new(v: T) -> Ref<T> {
        Ref(Rc::new(RefObj {
            val: RefCell::new(Some(v)),
            waiters: RefCell::new(Vec::new()),
        }))
    }

    /// `f` on the cell once it holds a value: while it is empty, the context
    /// blocks until `modify`'s store wakes it.
    fn when_full<R>(&self, mut f: impl FnMut(&mut Option<T>) -> Option<R>) -> R {
        loop {
            if let Some(r) = f(&mut self.0.val.borrow_mut()) {
                return r;
            }
            self.0.waiters.borrow_mut().push(sched::current_context());
            sched::block_sync();
        }
    }

    /// `ST.Ref.get`.
    pub fn get(&self) -> T {
        sched::ref_read();
        self.when_full(|c| c.clone())
    }

    /// `ST.Ref.take`: the value, leaving the cell empty until `put`.
    fn take(&self) -> T {
        sched::ref_read();
        self.when_full(Option::take)
    }

    /// `ST.Ref.put`: fills the cell `take` emptied, and wakes the contexts
    /// waiting for it.
    fn put(&self, v: T) {
        let old = self.0.val.borrow_mut().replace(v);
        debug_assert!(old.is_none(), "put into a full reference");
        let ws = std::mem::take(&mut *self.0.waiters.borrow_mut());
        for c in ws {
            sched::wake(c);
        }
    }

    /// The exchange of `swap` and `set`, once the cell holds a value.
    fn exchange(&self, v: T) -> T {
        let mut new = Some(v);
        self.when_full(|c| {
            if c.is_some() {
                std::mem::replace(c, new.take())
            } else {
                None
            }
        })
    }

    /// `ST.Ref.set`: `swap` with the result dropped (Lean 4.35), so it waits
    /// while `modify` holds the cell (LB-01). The old value is dropped after
    /// the cell's borrow.
    pub fn set(&self, v: T) {
        // a write another context can see: the context's handed-off streams
        // end first (docs/sched.md, item 7 of "The glue")
        sched::before_publish();
        drop(self.exchange(v));
    }

    /// `ST.Ref.swap`: waits while `modify` holds the cell (LB-18).
    pub fn swap(&self, v: T) -> T {
        sched::before_publish();
        sched::ref_read();
        self.exchange(v)
    }

    /// `ST.Ref.modify`: `take`, then `put` (`Ref.modifyUnsafe`).
    pub fn modify(&self, f: impl FnOnce(T) -> T) {
        sched::before_publish();
        let v = self.take();
        self.put(f(v));
    }
}

/// `IO.monoMsNow`: a clock read, so a polling point.
pub fn mono_ms_now() -> u64 {
    static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    sched::poll();
    START
        .get_or_init(std::time::Instant::now)
        .elapsed()
        .as_millis() as u64
}

/// `IO.sleep`.
pub fn sleep(ms: u32) {
    sched::sleep_ms(ms)
}

/// `IO.checkCanceled`.
pub fn check_canceled() -> bool {
    sched::check_canceled()
}

/// `String.toNat!` (the cases pass small decimal numbers).
pub fn to_nat(s: &str) -> u64 {
    s.parse().unwrap_or_else(|_| panic!("toNat! of {s:?}"))
}
