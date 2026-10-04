//! A translator's values over `lean_runtime::sched`, as small as the cases
//! need: tasks (the value in a slot the task's job fills, the handle's last
//! reference releasing the task), promises, and `IO.Ref`.

use lean_runtime::sched::{self, Job, Outcome, TaskId, TaskState};
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
        let _ = slot.set(f());
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
                let _ = slot.set(t2.get());
                return Outcome::Done;
            }
            let id2 = t2.0.id;
            Outcome::Continue(
                id2,
                Box::new(move || {
                    let _ = slot.set(t2.get());
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

    /// `IO.Promise.result?`.
    pub fn result_opt(&self) -> Task<Option<T>> {
        self.result.clone()
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
pub struct Ref<T>(Rc<RefCell<T>>);

impl<T> Clone for Ref<T> {
    fn clone(&self) -> Self {
        Ref(self.0.clone())
    }
}

impl<T: Clone> Ref<T> {
    /// `IO.mkRef`.
    pub fn new(v: T) -> Ref<T> {
        Ref(Rc::new(RefCell::new(v)))
    }
    /// `ST.Ref.get`.
    pub fn get(&self) -> T {
        sched::ref_read();
        self.0.borrow().clone()
    }
    /// `ST.Ref.set`.
    pub fn set(&self, v: T) {
        *self.0.borrow_mut() = v;
    }
    /// `ST.Ref.modify`.
    pub fn modify(&self, f: impl FnOnce(T) -> T) {
        sched::ref_read();
        let v = self.0.borrow().clone();
        *self.0.borrow_mut() = f(v);
    }
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
