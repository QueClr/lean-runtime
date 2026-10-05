//! A translator's values in threads mode, as small as the cases need, with
//! the single-thread driver's names (`tests/sched-driver/src/lean.rs`), so
//! that the shared ports compile against either: tasks (the value in a slot
//! the task's job fills, the handle's last reference releasing the task),
//! promises, and `IO.Ref`.
//!
//! What threads mode asks of them (docs/threads.md, 2.4): a job is `Send`
//! (a worker thread runs and drops it), so every value a task captures
//! crosses threads. Here that is `Arc` for a counted object, `OnceLock` for
//! a task's slot (any thread reads it), a lock for a mutable cell (`Var`),
//! and `sched::Ref` for `IO.Ref`: Lean 4.35's rule (3.1) as a lock and a
//! condition variable.

use lean_runtime::sched::{self, Job, Outcome, TaskId, TaskState};
use std::sync::{Arc, MutexGuard, OnceLock, PoisonError};

/// A value that may cross threads: what a task returns, a promise holds or
/// a reference stores.
pub trait Val: Clone + Send + Sync + 'static {}
impl<T: Clone + Send + Sync + 'static> Val for T {}

/// A counted object that a task may share with `main` or another task (a
/// `BaseMutex`, a promise held twice, a stream's buffer): `Arc` here, `Rc`
/// in the single-thread driver.
pub type Obj<T> = Arc<T>;

/// A mutable cell that a task may write (a stream's buffer): a lock here,
/// a `RefCell` in the single-thread driver, both with `borrow` and
/// `borrow_mut`.
#[derive(Default)]
pub struct Var<T>(std::sync::Mutex<T>);

impl<T> Var<T> {
    pub fn new(v: T) -> Var<T> {
        Var(std::sync::Mutex::new(v))
    }

    pub fn borrow(&self) -> MutexGuard<'_, T> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn borrow_mut(&self) -> MutexGuard<'_, T> {
        self.borrow()
    }
}

struct TaskObj<T> {
    id: TaskId,
    slot: Arc<OnceLock<T>>,
}

impl<T> TaskObj<T> {
    /// The id to pass to the scheduler: `FINISHED` once the slot holds the
    /// value (docs/sched.md, "The glue", item 3).
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
        // Lean's `deactivate_task`: the last reference is gone, on whichever
        // thread drops it. Nothing to do for a finished task.
        if self.slot.get().is_none() {
            sched::release(self.id);
        }
    }
}

/// `Task α`: one counted object, as Lean's.
pub struct Task<T>(Arc<TaskObj<T>>);

impl<T> Clone for Task<T> {
    fn clone(&self) -> Self {
        Task(self.0.clone())
    }
}

/// `Task.Priority.default`, `Task.Priority.dedicated`.
pub const PRIO_DEFAULT: u64 = 0;
pub const PRIO_DEDICATED: u64 = 9;
/// `Task.Priority.max`.
pub const PRIO_MAX: u64 = 8;

fn job_filling<T: Val>(slot: &Arc<OnceLock<T>>, f: impl FnOnce() -> T + Send + 'static) -> Job {
    let slot = slot.clone();
    Box::new(move || {
        let v = f();
        // a no-op in threads mode (no stream hand-offs), called as a glue
        // for both modes would
        sched::before_task_value();
        let _ = slot.set(v);
        Outcome::Done
    })
}

impl<T: Val> Task<T> {
    fn with_slot(make: impl FnOnce(&Arc<OnceLock<T>>) -> TaskId) -> Task<T> {
        let slot = Arc::new(OnceLock::new());
        let id = make(&slot);
        Task(Arc::new(TaskObj { id, slot }))
    }

    /// `Task.pure`.
    pub fn pure(v: T) -> Task<T> {
        Task(Arc::new(TaskObj {
            id: TaskId::FINISHED,
            slot: Arc::new(OnceLock::from(v)),
        }))
    }

    /// `Task.spawn fn prio`.
    pub fn spawn(f: impl FnOnce() -> T + Send + 'static, prio: u64) -> Task<T> {
        Task::with_slot(|slot| sched::spawn(job_filling(slot, f), prio, false))
    }

    /// `Task.get` / `IO.wait` (`lean_task_get`): the value if the slot
    /// holds it; otherwise, from a `sync` task, Lean's panic message first
    /// (`task_manager::wait_for`), then the wait, which blocks this thread.
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
pub fn has_finished<T: Val>(t: &Task<T>) -> bool {
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
pub fn as_task<T: Val>(act: impl FnOnce() -> T + Send + 'static, prio: u64) -> Task<T> {
    Task::with_slot(|slot| sched::spawn(job_filling(slot, act), prio, true))
}

/// `BaseIO.mapTask f t prio sync` (`keep_alive`), or `Task.map` (not).
pub fn map_task<A: Val, B: Val>(
    f: impl FnOnce(A) -> B + Send + 'static,
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
pub fn bind_task<A: Val, B: Val>(
    t: Task<A>,
    f: impl FnOnce(A) -> Task<B> + Send + 'static,
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
pub fn wait_any<T: Val>(ts: &[Task<T>]) -> T {
    let ids: Vec<TaskId> = ts.iter().map(|t| t.0.live()).collect();
    let k = sched::wait_any(&ids);
    ts[k].get()
}

/// `IO.cancel`.
pub fn cancel<T>(t: &Task<T>) {
    sched::cancel(t.0.live())
}

/// `IO.Promise α`: its task's slot holds `Option α` (`none` when the promise
/// is dropped unresolved).
pub struct Promise<T: Val> {
    result: Task<Option<T>>,
}

impl<T: Val> Promise<T> {
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
            result: Task(Arc::new(TaskObj {
                id,
                slot: Arc::new(OnceLock::new()),
            })),
        }
    }

    /// `IO.Promise.resolve`: the first resolution stores, from any thread.
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
    /// with the glue's forced panic report.
    pub fn result_bang(&self) -> Task<T> {
        map_task(
            |o: Option<T>| sched::option_get_or_block(o, crate::glue::lean_panic_forced),
            self.result_opt(),
            PRIO_DEFAULT,
            true,
            false,
        )
    }
}

impl<T: Val> Drop for Promise<T> {
    fn drop(&mut self) {
        // Lean's `deactivate_promise`: resolved with `none`.
        let slot = self.result.0.slot.clone();
        sched::resolve(self.result.0.live(), move || {
            let _ = slot.set(None);
        });
    }
}

/// `IO.Ref α`: the crate's `sched::Ref`, Lean 4.35's rule as a lock and a
/// condition variable (docs/threads.md, 3.1; LB-01, LB-18): `modify` is
/// `take`, then `put`, and while its function runs `get`, `take`, `set` and
/// `swap` block their thread until its store; `set` is `swap` with the
/// result dropped, so it is never lost; `swap` returns the value the
/// reference held.
pub struct Ref<T>(Arc<sched::Ref<T>>);

impl<T> Clone for Ref<T> {
    fn clone(&self) -> Self {
        Ref(self.0.clone())
    }
}

impl<T: Clone> Ref<T> {
    /// `IO.mkRef`.
    pub fn new(v: T) -> Ref<T> {
        Ref(Arc::new(sched::Ref::new(v)))
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

    /// `ST.Ref.modify`.
    pub fn modify(&self, f: impl FnOnce(T) -> T) {
        self.0.modify(f)
    }
}

/// `IO.monoMsNow`.
pub fn mono_ms_now() -> u64 {
    lean_runtime::io::env::mono_ms_now()
}

/// `IO.sleep`: the calling thread sleeps.
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
