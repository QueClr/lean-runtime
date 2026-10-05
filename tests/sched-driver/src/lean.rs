//! A translator's values over `lean_runtime::sched`, as small as the cases
//! need: tasks (the value in a slot the task's job fills, the handle's last
//! reference releasing the task), promises (resolved after the free when
//! dropped inside one: `sched::defer`), `IO.Ref` (the crate's
//! `sched::Ref`), and `Array`, whose free is a drain (`sched::DrainScope`).

use lean_runtime::sched::{self, Deferred, DrainScope, Job, Outcome, TaskId, TaskState};
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

/// A counted object that a task may share with `main` or another task (a
/// `BaseMutex`, a promise held twice, a stream's buffer): `Rc` here, `Arc`
/// in threads mode (`tests/sched-driver-mt`), so that the shared ports
/// (`cases.rs`) compile in both drivers.
pub type Obj<T> = Rc<T>;

/// A mutable cell that a task may write (a stream's buffer): `RefCell`
/// here, a lock in threads mode, both with `borrow` and `borrow_mut`.
pub type Var<T> = RefCell<T>;

/// `Task.Priority.default`, `Task.Priority.dedicated`.
pub const PRIO_DEFAULT: u64 = 0;
pub const PRIO_DEDICATED: u64 = 9;
/// `Task.Priority.max`.
pub const PRIO_MAX: u64 = 8;

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
    /// holds it; otherwise `sched::await_task`: from a `sync` task, Lean's
    /// panic message first (`task_manager::wait_for`), then the wait.
    pub fn get(&self) -> T {
        if let Some(v) = self.0.slot.get() {
            return v.clone();
        }
        sched::await_task(self.0.id, crate::glue::lean_panic);
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

    /// `ptrAddrUnsafe` of the promise: its object's address.
    pub fn addr(&self) -> usize {
        Rc::as_ptr(&self.0) as usize
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
        // Lean's `deactivate_promise`: resolved with `none`. Inside a free
        // (a drain, `Arr`'s drop), lean2rr's way: the slot's store now, in
        // the free's order, and the resolution, which walks the `sync`
        // dependents (Lean code that may block), once the drain is over
        // (`sched::defer`; docs/sched.md, "The wait cores", R3).
        let t = &self.result.0;
        if DrainScope::active() {
            if t.slot.get().is_none() {
                let _ = t.slot.set(None);
                sched::defer(Deferred::Resolve(t.id));
            }
            return;
        }
        let slot = t.slot.clone();
        sched::resolve(t.live(), move || {
            let _ = slot.set(None);
        });
    }
}

/// `IO.Ref α`: the crate's `sched::Ref` (Lean 4.35's rule, LB-01, LB-18 in
/// docs/lean-bugs.md; docs/sched.md, "The wait cores", core 3.2) in a
/// counted handle, as `tests/sched-driver-mt` wraps threads mode's:
/// - `modify` is `take`, then `put` (`ST.Prim.Ref.modifyUnsafe`), so the
///   reference is empty while `modify`'s function runs, and that function
///   may block (`Task.get`), letting other contexts run;
/// - until `modify`'s store, `get`, `take`, `set` and `swap` wait (a
///   blocking yield point), the taker's own included; `set` is `swap` with
///   the result dropped;
/// - reads are polling points in programs with tasks (`sched::ref_read`),
///   writes writers points (`sched::before_publish`).
///
/// Native 4.34.0 differs where its reference is shared with a task
/// (multi-threaded): `get` and `take` spin while the slot is empty, as here
/// (io.cpp 1459-1500, case `refs/get_during_modify`), but `set` stores into
/// the empty slot, and `modify`'s store then overwrites it (LB-01,
/// `refs/set_during_modify`), and `swap` returns its own argument (LB-18,
/// `refs/swap_during_modify`).
pub struct Ref<T>(Rc<sched::Ref<T>>);

impl<T> Clone for Ref<T> {
    fn clone(&self) -> Self {
        Ref(self.0.clone())
    }
}

impl<T: Clone> Ref<T> {
    /// `IO.mkRef`.
    pub fn new(v: T) -> Ref<T> {
        Ref(Rc::new(sched::Ref::new(v)))
    }

    /// `ST.Ref.get`.
    pub fn get(&self) -> T {
        self.0.get()
    }

    /// `ST.Ref.set`.
    pub fn set(&self, v: T) {
        self.0.set(v)
    }

    /// `ST.Ref.swap`.
    pub fn swap(&self, v: T) -> T {
        self.0.swap(v)
    }

    /// `ST.Ref.modify`: `take`, then `put` (`Ref.modifyUnsafe`).
    pub fn modify(&self, f: impl FnOnce(T) -> T) {
        self.0.modify(f)
    }
}

/// `Array α` as a counted object: its free (the last reference's drop)
/// is a drain (`sched::DrainScope`, the no-suspend scope), and releases the
/// elements from the last one, as native's `lean_del_core` reaches them
/// (the judge's nested-free verdict). A promise dropped there is resolved
/// after the drain (`Promise`'s drop).
pub struct Arr<T>(Obj<ArrObj<T>>);

pub struct ArrObj<T>(Vec<T>);

impl<T> Clone for Arr<T> {
    fn clone(&self) -> Self {
        Arr(self.0.clone())
    }
}

impl<T> Arr<T> {
    /// `#[a, b, ...]`.
    pub fn new(v: Vec<T>) -> Arr<T> {
        Arr(Obj::new(ArrObj(v)))
    }
}

impl<T> Drop for ArrObj<T> {
    fn drop(&mut self) {
        let _drain = DrainScope::enter();
        while let Some(x) = self.0.pop() {
            drop(x);
        }
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
