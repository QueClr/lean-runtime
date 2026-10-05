//! Program cases in threads mode (features `io` and `threads`;
//! docs/threads.md, T2): each case's twin is the port of
//! `tests/sched-driver`'s (which runs them over the single-thread
//! scheduler) over `sched::mt` and threads mode's `sched::uv`, with a
//! translator's values made thread-safe (`Arc`, `OnceLock`, `sched::Ref`):
//! - every case of `tests/cases/uvloop` (the loop thread resolves the
//!   timers' and signals' promises and runs their `sync` dependents; each
//!   twin runs inside a task, on a worker thread, which `main` waits for,
//!   `tests/in_task/mod.rs`, so every extern comes from a worker), those of
//!   LB-33 and LB-34 included;
//! - `tasks/worker_keeps_streams` and `worker_keeps_errno` (review AR-24),
//!   and `worker_streams_closed_at_exit` and `worker_streams_at_process_exit`
//!   (review AR-33: a worker's streams dropped when `finish` joins it, by
//!   its thread-locals' destructors), on `main`, as their programs run.
//!
//! The expected outcomes are the cases' own: native Lean 4.34.0's, or the
//! correct one where native is wrong (LB-19, LB-20, LB-33, LB-34), or the
//! recorded alternative (`signal_sigio_default`'s status 157, RSIOB-06).
//!
//! The test runs `scripts/cases.py check --diff` on hard links of this
//! binary named after each case, so a failing twin is named, with its
//! differences (review RT2-L-02). As a twin the binary opens native Lean's
//! startup descriptors in an ELF constructor, as a translator's glue does,
//! so the signal pipe and the loop's eventfd are native's (`signal_fds`).
//!
//! The binary runs without libtest (`harness = false`).

use lean_runtime::io::process::{self, Child, SpawnArgs, Stdio, StdioConfig};
use lean_runtime::io::{exit, Handle, IoError};
use lean_runtime::sched::uv::{self, LoopPromise, Signal, Timer};
use lean_runtime::sched::{self, Job, Outcome, TaskId, TaskState};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

mod in_task;

type R<T> = Result<T, IoError>;

// ---------------------------------------------------------------------------
// The glue: a translator's values over `sched` in threads mode

/// `IO.println`: one `putStr` of the line and `\n` on stdout.
fn println(s: &str) {
    let _ = Handle::stdout().put_str(format!("{s}\n").as_bytes());
}

/// `IO.eprintln`.
fn eprintln(s: &str) {
    let _ = Handle::stderr().put_str(format!("{s}\n").as_bytes());
}

/// An uncaught error: `IO.Error.toString`, status 1.
fn ok<T>(r: R<T>) -> T {
    match r {
        Ok(v) => v,
        Err(e) => {
            exit::show_error(error_text(&e).as_bytes());
            exit::exit(1)
        }
    }
}

struct TaskObj<T> {
    id: TaskId,
    slot: Arc<OnceLock<T>>,
}

impl<T> TaskObj<T> {
    /// The id to pass to the scheduler: `FINISHED` once the slot holds it.
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
        // Lean's `deactivate_task`
        if self.slot.get().is_none() {
            sched::release(self.id);
        }
    }
}

/// `Task α`.
struct Task<T>(Arc<TaskObj<T>>);

impl<T> Clone for Task<T> {
    fn clone(&self) -> Self {
        Task(self.0.clone())
    }
}

const PRIO_DEFAULT: u64 = 0;
const PRIO_DEDICATED: u64 = 9;

/// A value that may cross threads.
trait Val: Clone + Send + Sync + 'static {}
impl<T: Clone + Send + Sync + 'static> Val for T {}

fn job_filling<T: Val>(slot: &Arc<OnceLock<T>>, f: impl FnOnce() -> T + Send + 'static) -> Job {
    let slot = slot.clone();
    Box::new(move || {
        let v = f();
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

    fn pure(v: T) -> Task<T> {
        Task(Arc::new(TaskObj {
            id: TaskId::FINISHED,
            slot: Arc::new(OnceLock::from(v)),
        }))
    }

    /// `Task.get` / `IO.wait`.
    fn get(&self) -> T {
        if let Some(v) = self.0.slot.get() {
            return v.clone();
        }
        if sched::in_sync_task() {
            eprintln(sched::GET_IN_SYNC_TASK);
        }
        sched::wait(self.0.id);
        self.0
            .slot
            .get()
            .expect("a finished task has its value")
            .clone()
    }

    fn state(&self) -> TaskState {
        match self.0.live() {
            TaskId::FINISHED => TaskState::Finished,
            id => sched::state(id),
        }
    }
}

/// `IO.hasFinished`.
fn has_finished<T: Val>(t: &Task<T>) -> bool {
    t.state() == TaskState::Finished
}

/// `BaseIO.asTask act prio`.
fn as_task<T: Val>(act: impl FnOnce() -> T + Send + 'static, prio: u64) -> Task<T> {
    Task::with_slot(|slot| sched::spawn(job_filling(slot, act), prio, true))
}

/// `BaseIO.mapTask f t prio sync` (`keep_alive`).
fn map_task<A: Val, B: Val>(
    f: impl FnOnce(A) -> B + Send + 'static,
    t: Task<A>,
    prio: u64,
    sync: bool,
) -> Task<B> {
    let src = t.0.live();
    if sched::dependent_runs_now(src, sync) {
        return Task::pure(f(t.get()));
    }
    Task::with_slot(|slot| {
        sched::depend(src, job_filling(slot, move || f(t.get())), prio, sync, true)
    })
}

/// `IO.Promise α`: its task's slot holds `Option α`.
struct Promise<T: Val> {
    result: Task<Option<T>>,
}

impl<T: Val> Promise<T> {
    fn new() -> Promise<T> {
        let id = match sched::promise_new() {
            Ok(id) => id,
            Err(msg) => {
                eprintln(&format!("INTERNAL PANIC: {msg}"));
                exit::exit(1)
            }
        };
        Promise {
            result: Task(Arc::new(TaskObj {
                id,
                slot: Arc::new(OnceLock::new()),
            })),
        }
    }

    fn resolve(&self, v: T) {
        let slot = self.result.0.slot.clone();
        sched::resolve(self.result.0.live(), move || {
            let _ = slot.set(Some(v));
        });
    }

    fn is_resolved(&self) -> bool {
        self.result.0.slot.get().is_some()
    }
}

impl<T: Val> Drop for Promise<T> {
    fn drop(&mut self) {
        // Lean's `deactivate_promise`: resolved with `none`
        let slot = self.result.0.slot.clone();
        sched::resolve(self.result.0.live(), move || {
            let _ = slot.set(None);
        });
    }
}

/// `IO.Promise α` as a counted object, as the loop's handles hold it.
struct UvPromise<T: Val>(Arc<Promise<T>>);

impl<T: Val> Clone for UvPromise<T> {
    fn clone(&self) -> Self {
        UvPromise(self.0.clone())
    }
}

impl<T: Val> UvPromise<T> {
    fn new() -> UvPromise<T> {
        UvPromise(Arc::new(Promise::new()))
    }
    /// `IO.Promise.result?`.
    fn result_opt(&self) -> Task<Option<T>> {
        self.0.result.clone()
    }
    fn resolve(&self, v: T) {
        self.0.resolve(v)
    }
    /// `ptrAddrUnsafe` of the promise: its object's address.
    fn addr(&self) -> usize {
        Arc::as_ptr(&self.0) as usize
    }
}

impl LoopPromise for UvPromise<()> {
    fn is_resolved(&self) -> bool {
        self.0.is_resolved()
    }
    fn resolve(&self, _: i64) {
        self.0.resolve(())
    }
}

impl LoopPromise for UvPromise<i64> {
    fn is_resolved(&self) -> bool {
        self.0.is_resolved()
    }
    fn resolve(&self, v: i64) {
        self.0.resolve(v)
    }
}

fn mono_ms_now() -> u64 {
    lean_runtime::io::env::mono_ms_now()
}

fn sleep(ms: u32) {
    sched::sleep_ms(ms)
}

fn to_nat(s: &str) -> u64 {
    s.parse().unwrap_or(0)
}

// ---- IO definitions, as their Lean code reads ----

/// `Handle.readToEnd`.
fn read_to_end(h: &Handle) -> R<String> {
    let mut acc = Vec::new();
    loop {
        let mut v = Vec::new();
        h.read_vec(1024, &mut v)?;
        if v.is_empty() {
            break;
        }
        acc.extend_from_slice(&v);
    }
    String::from_utf8(acc)
        .map_err(|_| IoError::user_error("Tried to read from handle containing non UTF-8 data."))
}

/// `IO.Process.spawn`.
fn spawn(cmd: &str, args: &[&str], stdio: StdioConfig) -> R<Child> {
    let args: Vec<&[u8]> = args.iter().map(|a| a.as_bytes()).collect();
    process::spawn(
        stdio,
        &SpawnArgs {
            cmd: cmd.as_bytes(),
            args: &args,
            cwd: None,
            env: &[],
            inherit_env: true,
            setsid: false,
        },
    )
}

const INHERIT: StdioConfig = StdioConfig {
    stdin: Stdio::Inherit,
    stdout: Stdio::Inherit,
    stderr: Stdio::Inherit,
};

/// `IO.Process.output` (its Lean definition: standard output read on a
/// dedicated task while the caller reads standard error).
fn output(cmd: &str, args: &[&str]) -> R<String> {
    let child = spawn(
        cmd,
        args,
        StdioConfig {
            stdin: Stdio::Null,
            stdout: Stdio::Piped,
            stderr: Stdio::Piped,
        },
    )?;
    let out = child.stdout.clone().expect("piped");
    let stdout = as_task(move || read_to_end(&out).ok(), PRIO_DEDICATED);
    let _ = read_to_end(child.stderr.as_ref().expect("piped"))?;
    let _ = child.process.wait()?;
    Ok(stdout.get().unwrap_or_default())
}

fn get_pid() -> u32 {
    lean_runtime::io::env::get_pid()
}

/// Lean's `IO.Error.toString`, for the errors these twins meet.
fn error_text(e: &IoError) -> String {
    use IoError as E;
    let down = |s: &str| {
        let mut c = s.chars();
        match c.next() {
            Some(f) => f.to_lowercase().chain(c).collect::<String>(),
            None => String::new(),
        }
    };
    match e {
        E::InvalidArgument(None, c, d) => {
            format!("invalid argument (error code: {c}, {})", down(d))
        }
        E::OtherError(c, d) => format!("{} (error code: {c})", down(d)),
        E::InappropriateType(None, c, d) => {
            format!("inappropriate type (error code: {c}, {})", down(d))
        }
        E::AlreadyExists(None, c, d) => {
            format!("already exists (error code: {c}, {})", down(d))
        }
        E::UserError(m) => m.clone(),
        e => format!("{e:?}"),
    }
}

fn repr_unit(o: &Option<()>) -> String {
    match o {
        Some(()) => "some ()".into(),
        None => "none".into(),
    }
}

fn repr_int(o: &Option<i64>) -> String {
    match o {
        Some(v) if *v < 0 => format!("some ({v})"),
        Some(v) => format!("some {v}"),
        None => "none".into(),
    }
}

// ---------------------------------------------------------------------------
// The twins (tests/sched-driver/src/cases.rs, "tests/cases/uvloop")

type UTimer = Timer<UvPromise<()>>;
type USignal = Signal<UvPromise<i64>>;

fn uv_ok<T>(r: Result<T, i32>) -> T {
    ok(r.map_err(|e| IoError::decode_uv_error(e, None)))
}

fn finished<T: Val>(p: &UvPromise<T>) -> bool {
    has_finished(&p.result_opt())
}

fn loop_configure(args: &[String]) -> u32 {
    println(&format!("alive: {}", uv::loop_alive()));
    uv_ok(uv::loop_configure(args[0] == "1", args[1] == "1"));
    println(&format!("configured, alive: {}", uv::loop_alive()));
    uv_ok(uv::loop_configure(false, false));
    println(&format!("alive: {}", uv::loop_alive()));
    0
}

fn timer_oneshot(args: &[String]) -> u32 {
    let ms = to_nat(&args[0]);
    let t: UTimer = Timer::new(ms, false);
    let t0 = mono_ms_now();
    let p = t.next(UvPromise::new);
    println(&format!("pending after next: {}", !finished(&p)));
    let r = p.result_opt().get();
    drop(p);
    let dt = mono_ms_now() - t0;
    println(&format!(
        "resolved {}, after at least {ms} ms: {}",
        repr_unit(&r),
        dt >= ms
    ));
    let p2 = t.next(UvPromise::new);
    println(&format!("second next resolved at once: {}", finished(&p2)));
    drop(p2);
    t.reset();
    t.cancel();
    let p3 = t.next(UvPromise::new);
    println(&format!(
        "after reset and cancel, next resolved: {}",
        finished(&p3)
    ));
    drop(p3);
    t.stop();
    let p4 = t.next(UvPromise::new);
    sleep((2 * ms) as u32);
    println(&format!("after stop, next resolved: {}", finished(&p4)));
    p4.resolve(());
    0
}

fn timer_repeating(args: &[String]) -> u32 {
    let ms = to_nat(&args[0]);
    let n = to_nat(&args[1]);
    let t: UTimer = Timer::new(ms, true);
    let t0 = mono_ms_now();
    let p = t.next(UvPromise::new);
    let r = p.result_opt().get();
    drop(p);
    println(&format!("first tick {}", repr_unit(&r)));
    for i in 0..n {
        let a = t.next(UvPromise::new);
        let b = t.next(UvPromise::new);
        let fa = finished(&a);
        let _ = b.result_opt().get();
        drop(b);
        println(&format!(
            "tick {}: pending before {}, same promise resolved: {}",
            i + 1,
            !fa,
            finished(&a)
        ));
    }
    let dt = mono_ms_now() - t0;
    println(&format!(
        "{n} periods took at least {} ms: {}",
        n * ms,
        dt >= n * ms
    ));
    t.stop();
    let q = t.next(UvPromise::new);
    sleep((2 * ms) as u32);
    println(&format!("after stop, next resolved: {}", finished(&q)));
    q.resolve(());
    drop(q);
    let z: UTimer = Timer::new(0, true);
    let z0 = z.next(UvPromise::new);
    let _ = z0.result_opt().get();
    drop(z0);
    let z1 = z.next(UvPromise::new);
    sleep((3 * ms) as u32);
    println(&format!(
        "timeout 0: first resolved, second resolved: {}",
        finished(&z1)
    ));
    drop(z1);
    z.stop();
    0
}

fn timer_cancel_reset(args: &[String]) -> u32 {
    let ms = to_nat(&args[0]);
    let t: UTimer = Timer::new(ms, false);
    let p = t.next(UvPromise::new);
    t.cancel();
    sleep((2 * ms) as u32);
    println(&format!(
        "one-shot: cancelled promise resolved: {}",
        finished(&p)
    ));
    p.resolve(());
    drop(p);
    let p2 = t.next(UvPromise::new);
    let r = p2.result_opt().get();
    drop(p2);
    println(&format!(
        "one-shot: next after cancel resolves {}",
        repr_unit(&r)
    ));
    let u: UTimer = Timer::new(ms, true);
    let u0 = u.next(UvPromise::new);
    let _ = u0.result_opt().get();
    drop(u0);
    let u1 = u.next(UvPromise::new);
    u.cancel();
    sleep((2 * ms) as u32);
    println(&format!(
        "repeating: cancelled promise resolved: {}",
        finished(&u1)
    ));
    u1.resolve(());
    drop(u1);
    let u2 = u.next(UvPromise::new);
    let r = u2.result_opt().get();
    drop(u2);
    println(&format!(
        "repeating: next after cancel resolves {}",
        repr_unit(&r)
    ));
    u.stop();
    let v: UTimer = Timer::new(2 * ms, false);
    let t0 = mono_ms_now();
    let q = v.next(UvPromise::new);
    sleep(ms as u32);
    v.reset();
    sleep(ms as u32);
    println(&format!(
        "reset: resolved before the moved deadline: {}",
        finished(&q)
    ));
    let r = q.result_opt().get();
    drop(q);
    let dt = mono_ms_now() - t0;
    println(&format!(
        "reset: resolves {} after at least {} ms: {}",
        repr_unit(&r),
        3 * ms,
        dt >= 3 * ms
    ));
    0
}

/// `kill (sig : String)`: `IO.Process.output` of `kill -SIG <pid>`.
fn kill_self(sig: &str) {
    let pid = get_pid().to_string();
    let _ = ok(output("kill", &[&format!("-{sig}"), &pid]));
}

fn signal_usr1(args: &[String]) -> u32 {
    let num: i64 = args[0].parse().expect("an Int");
    let s: USignal = Signal::new(num as i32, false);
    let p = uv_ok(s.next(UvPromise::new));
    println(&format!("one-shot pending: {}", !finished(&p)));
    kill_self("USR1");
    let r = p.result_opt().get();
    drop(p);
    println(&format!("one-shot got {}", repr_int(&r)));
    let p2 = uv_ok(s.next(UvPromise::new));
    println(&format!("one-shot second next resolved: {}", finished(&p2)));
    drop(p2);
    let m: USignal = Signal::new(num as i32, true);
    for i in 0..2 {
        let q = uv_ok(m.next(UvPromise::new));
        kill_self("USR1");
        let r = q.result_opt().get();
        drop(q);
        println(&format!("repeating {i}: got {}", repr_int(&r)));
    }
    let q = uv_ok(m.next(UvPromise::new));
    m.cancel();
    kill_self("USR1");
    sleep(100);
    println(&format!(
        "repeating: cancelled promise resolved: {}",
        finished(&q)
    ));
    q.resolve(0);
    drop(q);
    let q2 = uv_ok(m.next(UvPromise::new));
    kill_self("USR1");
    let r = q2.result_opt().get();
    drop(q2);
    println(&format!("repeating: after cancel got {}", repr_int(&r)));
    match USignal::new(99, false).next(UvPromise::new) {
        Ok(_) => println("signal 99: next succeeded"),
        Err(e) => println(&format!(
            "signal 99: {}",
            error_text(&IoError::decode_uv_error(e, None))
        )),
    }
    uv_ok(m.stop());
    uv_ok(s.stop());
    println("stopped; the next SIGUSR1 ends the program");
    let _ = Handle::stdout().flush();
    kill_self("USR1");
    sleep(1000);
    println("not reached");
    0
}

/// `run cmd`: `IO.Process.output { cmd }`.
fn run_cmd(cmd: &str) {
    let _ = ok(output(cmd, &[]));
}

fn signal_stale(args: &[String]) -> u32 {
    let num: i64 = args[0].parse().expect("an Int");
    let w: USignal = Signal::new(num as i32, true);
    let p = uv_ok(w.next(UvPromise::new));
    run_cmd("true");
    let r = p.result_opt().get();
    drop(p);
    println(&format!("first watcher got {}", repr_int(&r)));
    uv_ok(w.stop());
    run_cmd("true");
    println("a child exited with no watcher");
    let w2: USignal = Signal::new(num as i32, false);
    let q = uv_ok(w2.next(UvPromise::new));
    sleep(200);
    println(&format!(
        "the new watcher saw the earlier signal: {}",
        finished(&q)
    ));
    run_cmd("true");
    let r = q.result_opt().get();
    drop(q);
    println(&format!("the new watcher got {}", repr_int(&r)));
    0
}

fn signal_oneshot_twice(args: &[String]) -> u32 {
    let num: i64 = args[0].parse().expect("an Int");
    let s: USignal = Signal::new(num as i32, false);
    let p = uv_ok(s.next(UvPromise::new));
    let pid = get_pid();
    let _child = ok(spawn(
        "sh",
        &[
            "-c",
            &format!("kill -USR1 {pid}; sleep 0.05; kill -USR1 {pid}"),
        ],
        INHERIT,
    ));
    println("before");
    let _ = Handle::stdout().flush();
    let n = to_nat(&args[1]);
    let mut x = (pid as u64) | 1;
    for _ in 0..n {
        x = std::hint::black_box(
            x.wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407),
        );
    }
    println(&format!("not reached {x} {}", finished(&p)));
    0
}

fn signal_failed_next(args: &[String]) -> u32 {
    let num: i64 = args[0].parse().expect("an Int");
    let s: USignal = Signal::new(num as i32, false);
    let text = |e: i32| error_text(&IoError::decode_uv_error(e, None));
    match s.next(UvPromise::new) {
        Ok(_) => println("first next: ok"),
        Err(e) => println(&format!("first next: {}", text(e))),
    }
    s.cancel();
    println("cancelled");
    match s.next(UvPromise::new) {
        Ok(_) => println("second next: ok"),
        Err(e) => println(&format!("second next: {}", text(e))),
    }
    uv_ok(s.stop());
    println("done");
    0
}

/// A pure computation of `ms` milliseconds.
fn spin_ms(ms: u64) {
    let t = Instant::now();
    let mut acc = 0u64;
    while t.elapsed() < Duration::from_millis(ms) {
        acc = std::hint::black_box(acc.wrapping_mul(31).wrapping_add(7));
    }
    std::hint::black_box(acc);
}

fn spawn_sh(script: &str) {
    let c = ok(spawn("sh", &["-c", script], INHERIT));
    drop(c);
}

fn signal_stale_deferred(_: &[String]) -> u32 {
    let w: USignal = Signal::new(17, true);
    let p = uv_ok(w.next(UvPromise::new));
    run_cmd("true");
    let r = p.result_opt().get();
    drop(p);
    println(&format!("first watcher got {}", repr_int(&r)));
    uv_ok(w.stop());
    run_cmd("true");
    println("a child exited with no SIGCHLD watcher");
    let u: USignal = Signal::new(10, false);
    let pu = uv_ok(u.next(UvPromise::new));
    let w2: USignal = Signal::new(17, false);
    let q = uv_ok(w2.next(UvPromise::new));
    sleep(200);
    println(&format!(
        "the new SIGCHLD watcher saw the earlier signal: {}",
        finished(&q)
    ));
    q.resolve(0);
    pu.resolve(0);
    uv_ok(u.stop());
    uv_ok(w2.stop());
    0
}

fn timer_due_stop(args: &[String]) -> u32 {
    let ms = to_nat(&args[1]);
    let t: UTimer = Timer::new(10, false);
    let p = t.next(UvPromise::new);
    spin_ms(ms);
    t.stop();
    let task = p.result_opt();
    drop(p);
    let r = task.get();
    println(&format!("due timer, then stop: {}", repr_unit(&r)));
    0
}

fn signal_cancel_restart(args: &[String]) -> u32 {
    let ms = to_nat(&args[1]);
    let s: USignal = Signal::new(10, false);
    drop(uv_ok(s.next(UvPromise::new)));
    let pid = get_pid();
    spawn_sh(&format!("sleep 0.3; kill -USR1 {pid}"));
    spin_ms(ms);
    s.cancel();
    let p2 = uv_ok(s.next(UvPromise::new));
    sleep(100);
    println(&format!(
        "after cancel and next, resolved: {}",
        finished(&p2)
    ));
    p2.resolve(0);
    uv_ok(s.stop());
    0
}

fn signal_order(_: &[String]) -> u32 {
    let a: USignal = Signal::new(10, false);
    let b: USignal = Signal::new(10, true);
    let pa = uv_ok(a.next(UvPromise::new));
    let pb = uv_ok(b.next(UvPromise::new));
    let ta = map_task(
        |_: Option<i64>| println("one-shot (started first)"),
        pa.result_opt(),
        PRIO_DEFAULT,
        true,
    );
    let tb = map_task(
        |_: Option<i64>| println("repeating (started second)"),
        pb.result_opt(),
        PRIO_DEFAULT,
        true,
    );
    drop(pa);
    drop(pb);
    kill_self("USR1");
    ta.get();
    tb.get();
    uv_ok(b.stop());
    0
}

/// `(← System.FilePath.readDir "/proc/self/fd").size`.
fn fd_count() -> u64 {
    let mut n = 0;
    ok(lean_runtime::io::fs::read_dir(b"/proc/self/fd", |_| n += 1));
    n
}

fn signal_fds(_: &[String]) -> u32 {
    let before = fd_count();
    let s: USignal = Signal::new(10, true);
    let p = uv_ok(s.next(UvPromise::new));
    let after = fd_count();
    println(&format!(
        "descriptors added by the first watcher: {}",
        after - before
    ));
    p.resolve(0);
    drop(p);
    uv_ok(s.stop());
    println(&format!("after stop: {}", fd_count() - before));
    0
}

fn exit_listening(_: &[String]) -> u32 {
    let s: USignal = Signal::new(10, true);
    let p = uv_ok(s.next(UvPromise::new));
    let _a = map_task(
        |r: Option<i64>| eprintln(&format!("signal dependent ran: {}", repr_int(&r))),
        p.result_opt(),
        PRIO_DEFAULT,
        true,
    );
    drop(p);
    drop(s);
    let t: UTimer = Timer::new(100000, true);
    let q = t.next(UvPromise::new);
    let _ = q.result_opt().get();
    drop(q);
    let q2 = t.next(UvPromise::new);
    let _b = map_task(
        |r: Option<()>| eprintln(&format!("timer dependent ran: {}", repr_unit(&r))),
        q2.result_opt(),
        PRIO_DEFAULT,
        true,
    );
    drop(q2);
    drop(t);
    println("main returns");
    0
}

fn signal_sigio_default(_: &[String]) -> u32 {
    let s: USignal = Signal::new(29, true);
    drop(uv_ok(s.next(UvPromise::new)));
    uv_ok(s.stop());
    kill_self("IO");
    sleep(200);
    println("survived SIGIO");
    0
}

/// The judge's LB20_Probe.lean (args: KIND OP MODE AFTER).
fn lb20_probe(args: &[String]) -> u32 {
    let say = |s: &str| {
        println(s);
        let _ = Handle::stdout().flush();
    };
    let (kind, stop) = (args[0].as_str(), args[1] == "stop");
    let sync = args[2] == "sync";
    let after = args[3].as_str();
    if kind == "timer" {
        let t: UTimer = Timer::new(10, false);
        let p = t.next(UvPromise::new);
        let t2 = t.clone();
        let tk = map_task(
            move |_: Option<()>| {
                if stop {
                    t2.stop()
                } else {
                    t2.cancel()
                }
            },
            p.result_opt(),
            PRIO_DEFAULT,
            sync,
        );
        tk.get();
        say("dependent ran: ok");
        say(&format!("first promise resolved: {}", finished(&p)));
        drop(p);
        match after {
            "next" => {
                for _ in 0..3 {
                    let q = t.next(UvPromise::new);
                    let task = q.result_opt();
                    drop(q);
                    say(&format!("next: {}", has_finished(&task)));
                }
            }
            "reset" => {
                t.reset();
                say("reset: ok");
            }
            _ => {}
        }
    } else {
        let s: USignal = Signal::new(10, false);
        let p = uv_ok(s.next(UvPromise::new));
        let s2 = s.clone();
        let tk = map_task(
            move |_: Option<i64>| {
                if stop {
                    let _ = s2.stop();
                } else {
                    s2.cancel();
                }
            },
            p.result_opt(),
            PRIO_DEFAULT,
            sync,
        );
        kill_self("USR1");
        tk.get();
        say("dependent ran: ok");
        say(&format!("first promise resolved: {}", finished(&p)));
        drop(p);
        if after == "next" {
            for _ in 0..3 {
                let q = uv_ok(s.next(UvPromise::new));
                let task = q.result_opt();
                drop(q);
                say(&format!("next: {}", has_finished(&task)));
            }
        }
    }
    say("done");
    0
}

/// `arm` of `timer_catchup_bound`: re-subscribe from a `sync` dependent of
/// each tick.
fn catchup_arm(t: UTimer, n: Arc<sched::Ref<u64>>, work_ms: u64) {
    let p = t.next(UvPromise::new);
    let task = p.result_opt();
    drop(p);
    let _ = map_task(
        move |v: Option<()>| {
            spin_ms(work_ms);
            n.modify(|k| k + 1);
            // on `some` only, as the case (LB-33: at `t.stop` the dependent
            // reads `none` once and ends; re-subscribing on `none` would
            // never end, as on Lean master)
            if v.is_some() {
                catchup_arm(t, n, work_ms);
            }
        },
        task,
        PRIO_DEFAULT,
        true,
    );
}

fn timer_catchup_bound(args: &[String]) -> u32 {
    let work_ms = to_nat(&args[1]);
    let n = Arc::new(sched::Ref::new(0u64));
    let t: UTimer = Timer::new(1, true);
    catchup_arm(t.clone(), n.clone(), work_ms);
    sleep(100);
    let t0 = Instant::now();
    let u: UTimer = Timer::new(1000, false);
    let dt = t0.elapsed().as_millis();
    let k = n.get();
    println(&format!(
        "Timer.mk under 2 s: {}; under 500 ticks so far: {}",
        dt < 2000,
        k < 500
    ));
    t.stop();
    drop(u);
    0
}

fn signal_rearm_in_dependent(args: &[String]) -> u32 {
    let sync = args[0] == "sync";
    let a: USignal = Signal::new(10, false);
    let pa = uv_ok(a.next(UvPromise::new));
    let tb = map_task(
        move |_: Option<i64>| {
            let b: USignal = Signal::new(10, false);
            let pb = uv_ok(b.next(UvPromise::new));
            (b, pb)
        },
        pa.result_opt(),
        PRIO_DEFAULT,
        sync,
    );
    drop(pa);
    kill_self("USR1");
    let (_b, pb) = tb.get();
    println("B listening");
    let _ = Handle::stdout().flush();
    kill_self("USR1");
    let t = pb.result_opt();
    drop(pb);
    println(&format!("B got {}", repr_int(&t.get())));
    0
}

// ---------------------------------------------------------------------------
// The cases of LB-33 and LB-34 (tests/cases/uvloop/*_rearm_*, *_keep_*,
// *_resubscribe, signal_stop_drops_promise), as tests/sched-driver ports
// them: `say` is `IO.println` and a flush.

type SharedRef<T> = Arc<sched::Ref<T>>;

fn new_ref<T>(v: T) -> SharedRef<T> {
    Arc::new(sched::Ref::new(v))
}

fn say(s: &str) {
    println(s);
    let _ = Handle::stdout().flush();
}

fn some_none<T>(v: &Option<T>) -> &'static str {
    if v.is_some() {
        "some"
    } else {
        "none"
    }
}

/// `showV` of the signal cases.
fn show_v(v: &Option<i64>) -> String {
    match v {
        Some(s) => format!("some {s}"),
        None => "none".into(),
    }
}

/// Lean's `toString` of an `Array String`.
fn show_array(a: &[String]) -> String {
    format!("#[{}]", a.join(", "))
}

/// `arm` of the timer cases: a `sync` dependent that prints its value and
/// subscribes again on any value, at most `cap` times.
fn timer_arm_capped(t: UTimer, n: SharedRef<u64>, cap: u64) {
    let p = t.next(UvPromise::new);
    let task = p.result_opt();
    drop(p);
    let _ = map_task(
        move |v: Option<()>| {
            n.modify(|k| k + 1);
            let k = n.get();
            say(&format!("dependent {k}: value {}", some_none(&v)));
            if k < cap {
                timer_arm_capped(t, n, cap);
            }
        },
        task,
        PRIO_DEFAULT,
        true,
    );
}

/// `arm` of the timer cases that record values: a dependent (`sync` or not)
/// that records its value and subscribes again while it has fewer than
/// `cap` values, then resolves `done`.
fn timer_arm_record(
    t: UTimer,
    values: SharedRef<Vec<String>>,
    cap: usize,
    done: UvPromise<()>,
    sync: bool,
) {
    let p = t.next(UvPromise::new);
    let task = p.result_opt();
    drop(p);
    let _ = map_task(
        move |v: Option<()>| {
            values.modify(|mut a| {
                a.push(some_none(&v).into());
                a
            });
            if values.get().len() < cap {
                timer_arm_record(t, values, cap, done, sync);
            } else {
                done.resolve(());
            }
        },
        task,
        PRIO_DEFAULT,
        sync,
    );
}

fn timer_stop_rearm_in_sync_dependent(args: &[String]) -> u32 {
    let (period, cap) = (to_nat(&args[0]), to_nat(&args[1]));
    let n = new_ref(0u64);
    let t: UTimer = Timer::new(period, true);
    let p0 = t.next(UvPromise::new);
    let r = p0.result_opt().get();
    drop(p0);
    say(&format!("0th tick: {}", some_none(&r)));
    timer_arm_capped(t.clone(), n.clone(), cap);
    say("stop: begin");
    t.stop();
    say("stop: end");
    sleep(100);
    say(&format!("dependent runs: {}", n.get()));
    say("main: end");
    0
}

fn timer_cancel_rearm_in_sync_dependent(args: &[String]) -> u32 {
    let period = to_nat(&args[0]);
    let values = new_ref(Vec::<String>::new());
    let done: UvPromise<()> = UvPromise::new();
    let t: UTimer = Timer::new(period, true);
    let p0 = t.next(UvPromise::new);
    let r = p0.result_opt().get();
    drop(p0);
    say(&format!("0th tick: {}", some_none(&r)));
    timer_arm_record(t.clone(), values.clone(), 2, done.clone(), true);
    say("cancel: begin");
    t.cancel();
    say("cancel: end");
    let _ = done.result_opt().get();
    say(&format!("dependent values: {}", show_array(&values.get())));
    t.stop();
    say("stop: end");
    0
}

fn timer_oneshot_keep_in_sync_dependent(args: &[String]) -> u32 {
    let op = args[0].clone();
    let (ms, count) = (to_nat(&args[1]), to_nat(&args[2]));
    let kept: SharedRef<Option<UvPromise<()>>> = new_ref(None);
    let t: UTimer = Timer::new(ms, false);
    let p = t.next(UvPromise::new);
    let (t2, kept2) = (t.clone(), kept.clone());
    let _ = map_task(
        move |v: Option<()>| {
            let q = t2.next(UvPromise::new);
            say(&format!("dependent: value {}", some_none(&v)));
            kept2.set(Some(q));
        },
        p.result_opt(),
        PRIO_DEFAULT,
        true,
    );
    drop(p);
    say(&format!("{op}: begin"));
    if op == "cancel" {
        t.cancel()
    } else {
        t.stop()
    }
    say(&format!("{op}: end"));
    let fresh: Vec<UvPromise<()>> = (0..count).map(|_| UvPromise::new()).collect();
    let Some(q) = kept.get() else {
        say("nothing kept");
        exit::exit(0)
    };
    let a = q.addr();
    say(&format!("aliased: {}", fresh.iter().any(|f| f.addr() == a)));
    say(&format!("kept resolved: {}", finished(&q)));
    for f in &fresh {
        f.resolve(());
    }
    say(&format!(
        "kept resolved after resolving the fresh promises: {}",
        finished(&q)
    ));
    exit::exit(0)
}

fn timer_oneshot_cancel_resubscribe(args: &[String]) -> u32 {
    let (ms, cap) = (to_nat(&args[0]), to_nat(&args[1]));
    let n = new_ref(0u64);
    let t: UTimer = Timer::new(ms, false);
    timer_arm_capped(t.clone(), n.clone(), cap);
    say("cancel: begin");
    t.cancel();
    say("cancel: end");
    sleep(100);
    say(&format!("dependent runs: {}", n.get()));
    say("stop: begin");
    t.stop();
    say("stop: end");
    0
}

fn timer_oneshot_stop_resubscribe(args: &[String]) -> u32 {
    let (ms, cap) = (to_nat(&args[0]), to_nat(&args[1]));
    let n = new_ref(0u64);
    let t: UTimer = Timer::new(ms, false);
    timer_arm_capped(t.clone(), n.clone(), cap);
    say("stop: begin");
    t.stop();
    say("stop: end");
    sleep(100);
    say(&format!("dependent runs: {}", n.get()));
    0
}

fn timer_stop_rearm_async_dependent(args: &[String]) -> u32 {
    let (period, cap) = (to_nat(&args[0]), to_nat(&args[1]));
    let values = new_ref(Vec::<String>::new());
    let done: UvPromise<()> = UvPromise::new();
    let t: UTimer = Timer::new(period, true);
    let p0 = t.next(UvPromise::new);
    let r = p0.result_opt().get();
    drop(p0);
    say(&format!("0th tick: {}", some_none(&r)));
    timer_arm_record(t.clone(), values.clone(), cap as usize, done.clone(), false);
    t.stop();
    say("stop: end");
    let _ = done.result_opt().get();
    say(&format!("dependent values: {}", show_array(&values.get())));
    0
}

/// `arm` of the signal cases: as `timer_arm_capped`.
fn signal_arm_capped(s: USignal, n: SharedRef<u64>, cap: u64) {
    let p = uv_ok(s.next(UvPromise::new));
    let task = p.result_opt();
    drop(p);
    let _ = map_task(
        move |v: Option<i64>| {
            n.modify(|k| k + 1);
            let k = n.get();
            say(&format!("dependent {k}: value {}", some_none(&v)));
            if k < cap {
                signal_arm_capped(s, n, cap);
            }
        },
        task,
        PRIO_DEFAULT,
        true,
    );
}

fn signal_stop_rearm_in_sync_dependent(args: &[String]) -> u32 {
    let num: i64 = args[0].parse().expect("an Int");
    let cap = to_nat(&args[1]);
    let n = new_ref(0u64);
    let s: USignal = Signal::new(num as i32, true);
    signal_arm_capped(s.clone(), n.clone(), cap);
    say("stop: begin");
    uv_ok(s.stop());
    say("stop: end");
    say(&format!("dependent runs: {}", n.get()));
    0
}

/// `arm` of `signal_cancel_rearm_in_sync_dependent`: records each value,
/// subscribes again in the first run only.
fn signal_arm_record(s: USignal, values: SharedRef<Vec<String>>) {
    let p = uv_ok(s.next(UvPromise::new));
    let task = p.result_opt();
    drop(p);
    let _ = map_task(
        move |v: Option<i64>| {
            values.modify(|mut a| {
                a.push(show_v(&v));
                a
            });
            if values.get().len() == 1 {
                signal_arm_record(s, values);
            }
        },
        task,
        PRIO_DEFAULT,
        true,
    );
}

fn signal_cancel_rearm_in_sync_dependent(args: &[String]) -> u32 {
    let num: i64 = args[0].parse().expect("an Int");
    let values = new_ref(Vec::<String>::new());
    let s: USignal = Signal::new(num as i32, true);
    signal_arm_record(s.clone(), values.clone());
    say("cancel: begin");
    s.cancel();
    say("cancel: end");
    let q = uv_ok(s.next(UvPromise::new));
    kill_self("USR1");
    let r = q.result_opt().get();
    drop(q);
    say(&format!("main's next after cancel: {}", show_v(&r)));
    say(&format!("dependent values: {}", show_array(&values.get())));
    uv_ok(s.stop());
    say("stop: end");
    0
}

fn signal_oneshot_keep_in_sync_dependent(args: &[String]) -> u32 {
    let op = args[0].clone();
    let num: i64 = args[1].parse().expect("an Int");
    let count = to_nat(&args[2]);
    let kept: SharedRef<Option<UvPromise<i64>>> = new_ref(None);
    let s: USignal = Signal::new(num as i32, false);
    let p = uv_ok(s.next(UvPromise::new));
    let (s2, kept2) = (s.clone(), kept.clone());
    let _ = map_task(
        move |v: Option<i64>| {
            let q = uv_ok(s2.next(UvPromise::new));
            say(&format!("dependent: value {}", some_none(&v)));
            kept2.set(Some(q));
        },
        p.result_opt(),
        PRIO_DEFAULT,
        true,
    );
    drop(p);
    say(&format!("{op}: begin"));
    if op == "cancel" {
        s.cancel()
    } else {
        uv_ok(s.stop())
    }
    say(&format!("{op}: end"));
    let fresh: Vec<UvPromise<i64>> = (0..count).map(|_| UvPromise::new()).collect();
    let Some(q) = kept.get() else {
        say("nothing kept");
        exit::exit(0)
    };
    let a = q.addr();
    say(&format!("aliased: {}", fresh.iter().any(|f| f.addr() == a)));
    say(&format!("kept resolved: {}", finished(&q)));
    for f in &fresh {
        f.resolve(7);
    }
    say(&format!(
        "kept resolved after resolving the fresh promises: {}",
        finished(&q)
    ));
    exit::exit(0)
}

fn signal_stop_drops_promise(args: &[String]) -> u32 {
    let num: i64 = args[0].parse().expect("an Int");
    let n = new_ref(0u64);
    let s: USignal = Signal::new(num as i32, true);
    let p = uv_ok(s.next(UvPromise::new));
    let n2 = n.clone();
    let _ = map_task(
        move |v: Option<i64>| {
            n2.modify(|k| k + 1);
            say(&format!("dependent {}: value {}", n2.get(), some_none(&v)));
        },
        p.result_opt(),
        PRIO_DEFAULT,
        true,
    );
    drop(p);
    say("stop: begin");
    uv_ok(s.stop());
    say("stop: end");
    say(&format!("dependent runs: {}", n.get()));
    0
}

// ---------------------------------------------------------------------------
// tests/cases/tasks/worker_keeps_{streams,errno}.lean (review RT2-L-01,
// AR-24): a pool worker keeps its thread's streams and `errno` from one task
// to the next, which threads mode gets from its real threads. These twins
// run on `main`, as the programs do.

/// The current stdout as these twins set it: a buffer
/// (`IO.FS.Stream.ofBuffer`), or `None` for the process's stream. `Arc`:
/// `IO.FS.Stream` crosses threads in threads mode.
type OutStream = Option<Arc<std::sync::Mutex<Vec<u8>>>>;

/// `IO.println` on the current stdout.
fn out_println(s: &str) {
    use lean_runtime::io::streams::{current, StdStream};
    let cur: OutStream = current(StdStream::Stdout, || None);
    match cur {
        Some(b) => b
            .lock()
            .unwrap()
            .extend_from_slice(format!("{s}\n").as_bytes()),
        None => println(s),
    }
}

/// Lean's `repr` of a string.
fn quote(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn worker_keeps_streams(_: &[String]) -> u32 {
    let buf: Arc<std::sync::Mutex<Vec<u8>>> = Arc::default();
    let b2 = buf.clone();
    let a = as_task(
        move || {
            let _ = lean_runtime::io::streams::set_stdout(Some(b2) as OutStream, || None);
            out_println("A: before its sleep");
            sleep(200);
            out_println("A: after its sleep");
        },
        PRIO_DEFAULT,
    );
    sleep(100);
    out_println("main: while A sleeps");
    a.get();
    let b = as_task(|| out_println("B: after A, on A's worker"), PRIO_DEFAULT);
    b.get();
    let c = as_task(|| out_println("C: a thread of its own"), PRIO_DEDICATED);
    c.get();
    let text = String::from_utf8(buf.lock().unwrap().clone()).expect("UTF-8");
    out_println(&format!("A's buffer: {}", quote(&text)));
    0
}

// tests/cases/tasks/worker_streams_{closed_at_exit,at_process_exit}.lean
// (review AR-33): a worker's stdout, a handle a task left set, is dropped
// when `finish` joins the worker (its thread-locals' destructors), before
// `main`'s flush; `IO.Process.exit` drops nothing, and the exit's flush
// writes `stdout` first.

/// A stdout set to a handle (`IO.FS.Stream.ofHandle h`), or `None` for the
/// process's.
type HandleOut = Option<Handle>;

/// `IO.print s` on the current stdout.
fn print_current(s: &str) {
    use lean_runtime::io::streams::{current, StdStream};
    match current(StdStream::Stdout, || None as HandleOut) {
        Some(h) => {
            let _ = h.put_str(s.as_bytes());
        }
        None => {
            let _ = Handle::stdout().put_str(s.as_bytes());
        }
    }
}

fn worker_streams_closed_at_exit(args: &[String]) -> u32 {
    let n = args.len();
    let t = as_task(
        move || {
            let h = ok(Handle::open(
                b"/dev/stdout",
                lean_runtime::io::FsMode::Write,
            ));
            let _ = lean_runtime::io::streams::set_stdout(Some(h) as HandleOut, || None);
            print_current(&format!("A{n}"));
        },
        PRIO_DEFAULT,
    );
    t.get();
    print_current("B");
    0
}

fn worker_streams_at_process_exit(args: &[String]) -> u32 {
    worker_streams_closed_at_exit(args);
    exit::exit(0)
}

/// `Handle.getLine`.
fn get_line(h: &Handle) -> R<String> {
    let mut v = Vec::new();
    h.get_line(&mut v)?;
    Ok(String::from_utf8_lossy(&v).into_owned())
}

fn worker_keeps_errno(_: &[String]) -> u32 {
    use lean_runtime::io::{fs, FsMode};
    let h = {
        let w = ok(Handle::open(b"e.txt", FsMode::Write));
        ok(w.put_str(b"line1\n"));
        drop(w);
        ok(Handle::open(b"e.txt", FsMode::Read))
    };
    let _ = h.put_str(b"x").and_then(|()| h.flush());
    let a = as_task(
        || {
            let _ = fs::remove_dir(b"e.txt");
        },
        PRIO_DEFAULT,
    );
    a.get();
    let _ = fs::create_dir(b"e.txt");
    let h2 = h.clone();
    let b = as_task(
        move || match get_line(&h2) {
            Ok(l) => format!("ok {}", quote(&l)),
            Err(e) => error_text(&e),
        },
        PRIO_DEFAULT,
    );
    println(&format!("B's getLine: {}", b.get()));
    match get_line(&h) {
        Ok(l) => println(&format!("main's getLine: ok {}", quote(&l))),
        Err(e) => println(&format!("main's getLine: {}", error_text(&e))),
    }
    0
}

// ---------------------------------------------------------------------------
// The program

type Twin = fn(&[String]) -> u32;

const TWINS: &[(&str, Twin)] = &[
    ("loop_configure", loop_configure),
    ("timer_oneshot", timer_oneshot),
    ("timer_repeating", timer_repeating),
    ("timer_cancel_reset", timer_cancel_reset),
    ("signal_usr1", signal_usr1),
    ("signal_stale", signal_stale),
    ("signal_oneshot_twice", signal_oneshot_twice),
    ("signal_failed_next", signal_failed_next),
    ("signal_stale_deferred", signal_stale_deferred),
    ("timer_due_stop", timer_due_stop),
    ("signal_cancel_restart", signal_cancel_restart),
    ("signal_order", signal_order),
    ("signal_fds", signal_fds),
    ("exit_listening", exit_listening),
    ("signal_sigio_default", signal_sigio_default),
    ("timer_stop_in_sync_dependent", lb20_probe),
    ("timer_cancel_in_sync_dependent", lb20_probe),
    ("signal_stop_in_sync_dependent", lb20_probe),
    ("signal_cancel_in_sync_dependent", lb20_probe),
    ("timer_catchup_bound", timer_catchup_bound),
    ("signal_rearm_in_sync_dependent", signal_rearm_in_dependent),
    ("signal_rearm_in_async_dependent", signal_rearm_in_dependent),
    (
        "timer_oneshot_stop_resubscribe",
        timer_oneshot_stop_resubscribe,
    ),
    (
        "timer_stop_rearm_in_sync_dependent",
        timer_stop_rearm_in_sync_dependent,
    ),
    (
        "timer_cancel_rearm_in_sync_dependent",
        timer_cancel_rearm_in_sync_dependent,
    ),
    (
        "timer_oneshot_stop_keep_in_sync_dependent",
        timer_oneshot_keep_in_sync_dependent,
    ),
    (
        "timer_oneshot_cancel_keep_in_sync_dependent",
        timer_oneshot_keep_in_sync_dependent,
    ),
    (
        "timer_oneshot_cancel_resubscribe",
        timer_oneshot_cancel_resubscribe,
    ),
    (
        "timer_stop_rearm_async_dependent",
        timer_stop_rearm_async_dependent,
    ),
    (
        "signal_stop_rearm_in_sync_dependent",
        signal_stop_rearm_in_sync_dependent,
    ),
    (
        "signal_cancel_rearm_in_sync_dependent",
        signal_cancel_rearm_in_sync_dependent,
    ),
    (
        "signal_oneshot_stop_keep_in_sync_dependent",
        signal_oneshot_keep_in_sync_dependent,
    ),
    (
        "signal_oneshot_cancel_keep_in_sync_dependent",
        signal_oneshot_keep_in_sync_dependent,
    ),
    ("signal_stop_drops_promise", signal_stop_drops_promise),
    ("worker_keeps_streams", worker_keeps_streams),
    ("worker_keeps_errno", worker_keeps_errno),
    (
        "worker_streams_closed_at_exit",
        worker_streams_closed_at_exit,
    ),
    (
        "worker_streams_at_process_exit",
        worker_streams_at_process_exit,
    ),
];

/// The twins that run on `main`, as their programs do, not inside a task:
/// the task cases, whose outcome is about the threads their tasks run on.
const ON_MAIN: &[&str] = &[
    "worker_keeps_streams",
    "worker_keeps_errno",
    "worker_streams_closed_at_exit",
    "worker_streams_at_process_exit",
];

/// The glue: nothing to do in threads mode.
struct MainGlue;

impl sched::Glue for MainGlue {}

/// The twin named by `argv[0]`'s file name, if any.
fn twin_name(argv0: &[u8]) -> Option<&'static str> {
    let base = argv0.rsplit(|&b| b == b'/').next().unwrap_or(argv0);
    TWINS.iter().map(|(n, _)| *n).find(|n| n.as_bytes() == base)
}

/// The translator's ELF constructor: native Lean's startup descriptors
/// (libuv's loop: its signal pipe and eventfd), when this binary runs as a
/// twin.
extern "C" fn startup() {
    let Ok(cmdline) = std::fs::read("/proc/self/cmdline") else {
        return;
    };
    let argv0 = cmdline.split(|&b| b == 0).next().unwrap_or(&[]);
    if twin_name(argv0).is_some() {
        if let Err(f) = lean_runtime::io::startup::open_native_descriptors() {
            lean_runtime::io::startup::fail_as_native(f);
        }
    }
}

#[used]
#[link_section = ".init_array"]
static STARTUP: extern "C" fn() = startup;

fn main() {
    use std::os::unix::ffi::OsStrExt;
    if cfg!(miri) {
        return;
    }
    let argv0 = std::env::args_os().next().unwrap_or_default();
    if let Some(id) = twin_name(argv0.as_bytes()) {
        #[cfg(feature = "stack-overflow")]
        sched::install_stack_overflow_handler();
        let twin = TWINS.iter().find(|(n, _)| *n == id).unwrap().1;
        let args: Vec<String> = std::env::args().skip(1).collect();
        let code = if ON_MAIN.contains(&id) {
            // as the program: `main` with the task manager
            sched::start(std::sync::Arc::new(MainGlue));
            let code = twin(&args);
            sched::finish();
            code
        } else {
            // the twin inside a task (`in_task` starts and finishes the task
            // manager)
            in_task::run(twin, args)
        };
        exit::exit(code as i32)
    }
    let root = env!("CARGO_MANIFEST_DIR");
    let ids: Vec<&str> = TWINS.iter().map(|(id, _)| *id).collect();
    let exe = std::env::current_exe().expect("test binary path");
    let dir = exe
        .parent()
        .unwrap()
        .join(format!("threads-twins-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for id in &ids {
        let w = dir.join(id);
        if std::fs::hard_link(&exe, &w).is_err() {
            std::fs::copy(&exe, &w).unwrap();
        }
    }
    let status = std::process::Command::new("python3")
        .arg(format!("{root}/scripts/cases.py"))
        // `--diff`: a failing twin is named, with its outputs' differences
        // (review RT2-L-02)
        .args(["check", "--diff", "--exe-dir"])
        .arg(&dir)
        .args(&ids)
        .env("LEAN_RUNTIME_NO_CAP", "1")
        .status()
        .expect("python3 scripts/cases.py");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        status.success(),
        "a threads-mode twin differs from native Lean (see above)"
    );
}
